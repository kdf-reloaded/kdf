use coins::utxo::UtxoActivationParams;
use coins::{coin_conf, CoinBalance, CoinProtocol, MmCoinEnum};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_derive::Serialize;
use serde_json::{self as json, Value as Json};
use std::collections::HashMap;

pub trait CurrentBlock {
    fn current_block(&self) -> u64;
}

pub trait TxHistory {
    fn tx_history(&self) -> bool;
}

impl TxHistory for UtxoActivationParams {
    fn tx_history(&self) -> bool { self.tx_history }
}

impl TxHistory for coins::utxo::bch::BchActivationRequest {
    fn tx_history(&self) -> bool { self.utxo_params.tx_history }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum DerivationMethod {
    /// Legacy iguana's privkey derivation, used by default
    Iguana,
    /// HD wallet derivation path, String is temporary here. Only the Solana result uses it.
    HDWallet(String),
}

// Only the Solana result uses it.
#[derive(Clone, Debug, Serialize)]
pub struct CoinAddressInfo<Balance> {
    pub(crate) derivation_method: DerivationMethod,
    pub(crate) pubkey: String,
    pub(crate) balances: Balance,
}

pub type TokenBalances = HashMap<String, CoinBalance>;

pub trait TryPlatformCoinFromMmCoinEnum {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized;
}

pub trait TryFromCoinProtocol {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized;
}

// Cold error path: created at most once per failed activation request and
// converted immediately, so the variant's size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CoinConfWithProtocolError {
    ConfigIsNotFound(String),
    CoinProtocolParseError { ticker: String, err: json::Error },
    UnexpectedProtocol { ticker: String, protocol: CoinProtocol },
}

pub fn coin_conf_with_protocol<T: TryFromCoinProtocol>(
    ctx: &MmArc,
    coin: &str,
) -> Result<(Json, T), MmError<CoinConfWithProtocolError>> {
    let conf = coin_conf(ctx, coin);
    if conf.is_null() {
        return MmError::err(CoinConfWithProtocolError::ConfigIsNotFound(coin.into()));
    }

    let coin_protocol: CoinProtocol = CoinProtocol::from_conf_json(conf["protocol"].clone()).map_to_mm(|err| {
        CoinConfWithProtocolError::CoinProtocolParseError {
            ticker: coin.into(),
            err,
        }
    })?;

    // CRD ch.40 R40.6D.2: the `swap_program_id` of a Solana entry, kept in the raw `protocol_data`, must be a base58
    // public key; an invalid member makes the entry unparsable.
    if matches!(coin_protocol, CoinProtocol::SOLANA) {
        if let Err(text) = coins::solana::swap_program_id_from_conf(&conf) {
            return MmError::err(CoinConfWithProtocolError::CoinProtocolParseError {
                ticker: coin.into(),
                err: <json::Error as serde::de::Error>::custom(text),
            });
        }
    }

    let coin_protocol =
        T::try_from_coin_protocol(coin_protocol).mm_err(|protocol| CoinConfWithProtocolError::UnexpectedProtocol {
            ticker: coin.into(),
            protocol,
        })?;
    Ok((conf, coin_protocol))
}
