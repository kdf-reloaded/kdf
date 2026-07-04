//! Tendermint token activation (CRD §36.2 -- `enable_tendermint_token`).
//!
//! Implements the generic single-token activation traits for the Tendermint
//! token (`TendermintToken`), so the framework's
//! `enable_token::<TendermintToken>` entrypoint can activate one Tendermint
//! (IBC / native bank) token against an already-active Tendermint platform
//! coin. Unlike the EVM case the token is a distinct type from the platform
//! coin, so `TryPlatformCoinFromMmCoinEnum` is implemented for the platform
//! `TendermintCoin` here.

use crate::platform_coin_with_tokens::RegisterTokenInfo;
use crate::prelude::*;
use crate::token::{EnableTokenError, TokenActivationOps, TokenProtocolParams};
use async_trait::async_trait;
use coins::tendermint::{TendermintCoin, TendermintToken, TendermintTokenActivationParams};
use coins::{CoinBalance, CoinProtocol, MarketCoinOps, MmCoinEnum};
use common::Future01CompatExt;
use mm2_err_handle::prelude::*;
use serde_derive::Serialize;
use std::collections::HashMap;

impl TryPlatformCoinFromMmCoinEnum for TendermintCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized,
    {
        match coin {
            MmCoinEnum::TendermintCoin(coin) => Some(coin),
            _ => None,
        }
    }
}

/// Token protocol info resolved from coin configuration (R36.2.2 / R36.3.1):
/// the parent platform ticker, the token decimals, and the Cosmos / IBC
/// on-chain denomination.
pub struct TendermintTokenProtocol {
    pub platform_coin_ticker: String,
    pub decimals: u8,
    pub denom: String,
}

impl TryFromCoinProtocol for TendermintTokenProtocol {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::TENDERMINTTOKEN {
                platform,
                denom,
                decimals,
            } => Ok(TendermintTokenProtocol {
                platform_coin_ticker: platform,
                decimals,
                denom,
            }),
            proto => MmError::err(proto),
        }
    }
}

impl TokenProtocolParams for TendermintTokenProtocol {
    fn platform_coin_ticker(&self) -> &str { &self.platform_coin_ticker }
}

/// Single-token activation result (R36.2.3).
#[derive(Debug, Serialize)]
pub struct TendermintTokenInitResult {
    balances: HashMap<String, CoinBalance>,
    platform_coin: String,
}

#[async_trait]
impl TokenActivationOps for TendermintToken {
    type PlatformCoin = TendermintCoin;
    type ActivationParams = TendermintTokenActivationParams;
    type ProtocolInfo = TendermintTokenProtocol;
    type ActivationResult = TendermintTokenInitResult;
    type ActivationError = EnableTokenError;

    async fn enable_token(
        ticker: String,
        platform_coin: Self::PlatformCoin,
        _activation_params: Self::ActivationParams,
        protocol_conf: Self::ProtocolInfo,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>> {
        let token = TendermintToken::from_protocol(
            ticker,
            platform_coin.clone(),
            protocol_conf.decimals,
            &protocol_conf.denom,
        )
        .mm_err(|e| EnableTokenError::Internal(e.to_string()))?;

        // Register the token on the platform coin so the platform's balance
        // enumeration includes it.
        platform_coin.register_token_info(&token);

        let balance = token.my_balance().compat().await.mm_err(EnableTokenError::from)?;
        let my_address = token.my_address().map_to_mm(EnableTokenError::Internal)?;
        let mut balances = HashMap::new();
        balances.insert(my_address, balance);

        let init_result = TendermintTokenInitResult {
            balances,
            platform_coin: platform_coin.ticker().to_owned(),
        };
        Ok((token, init_result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::mm_number::BigDecimal;

    #[test]
    fn protocol_from_tendermint_token_coin_protocol() {
        let proto = CoinProtocol::TENDERMINTTOKEN {
            platform: "IRIS".to_owned(),
            denom: "uosmo".to_owned(),
            decimals: 6,
        };
        let parsed = TendermintTokenProtocol::try_from_coin_protocol(proto).unwrap();
        assert_eq!(parsed.platform_coin_ticker(), "IRIS");
        assert_eq!(parsed.decimals, 6);
        assert_eq!(parsed.denom, "uosmo");
    }

    #[test]
    fn protocol_accepts_ibc_denom() {
        let proto = CoinProtocol::TENDERMINTTOKEN {
            platform: "IRIS".to_owned(),
            denom: "ibc/27394FB092D2ECCD56123C74F36E4C1F926001CEADA9CA97EA622B25F41E5EB2".to_owned(),
            decimals: 6,
        };
        let parsed = TendermintTokenProtocol::try_from_coin_protocol(proto).unwrap();
        assert!(parsed.denom.starts_with("ibc/"));
    }

    #[test]
    fn protocol_rejects_non_tendermint_token() {
        let proto = CoinProtocol::TENDERMINT {
            account_prefix: "cosmos".to_owned(),
            chain_id: "cosmoshub-4".to_owned(),
        };
        assert!(TendermintTokenProtocol::try_from_coin_protocol(proto).is_err());
    }

    #[test]
    fn activation_params_deserializes_empty_object() {
        let params: TendermintTokenActivationParams = serde_json::from_str("{}").unwrap();
        let _ = params;
    }

    #[test]
    fn init_result_serializes_expected_fields() {
        let mut balances = HashMap::new();
        balances.insert("cosmos1abc".to_owned(), CoinBalance {
            spendable: BigDecimal::from(11),
            unspendable: BigDecimal::from(0),
        });
        let result = TendermintTokenInitResult {
            balances,
            platform_coin: "IRIS".to_owned(),
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["platform_coin"], "IRIS");
        assert_eq!(v["balances"]["cosmos1abc"]["spendable"], "11");
    }
}
