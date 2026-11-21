//! TRON balance reads shared by the withdraw pipeline and the generic wallet path.
//!
//! Native TRX comes from `/wallet/getaccount`, a TRC20 token from `balanceOf(address)` through
//! `/wallet/triggerconstantcontract` (CRD ch.21 §21.9). The reads return the balance in the smallest unit
//! (SUN, or token units) and a neutral [`TronBalanceError`] that the callers map into their own error type
//! ([`WithdrawError`], [`BalanceError`]). Only the documented absent-account answer (`{}`, see
//! [`TronApiClient::get_account`]) reads as a zero balance: a transport failure, a non-2xx status, an
//! undecodable body or a malformed `balanceOf` result is always an error, never a balance of zero.

use super::api::{TronApiClient, TronApiError};
use super::TronAddress;

use crate::eth::{EthCoin, EthCoinType};
use crate::{BalanceError, WithdrawError};

use derive_more::Display;
use ethereum_types::{Address, U256};
use mm2_err_handle::prelude::*;

/// Why a TRON balance could not be read.
#[derive(Debug, Display)]
pub(crate) enum TronBalanceError {
    /// The node request failed (transport, timeout, non-2xx status, undecodable body, node-side rejection).
    #[display(fmt = "{}", _0)]
    Node(TronApiError),
    /// The account reported a negative TRX balance.
    #[display(fmt = "TRON account reported negative balance")]
    NegativeBalance,
    /// `balanceOf` carried no `constant_result` entry.
    #[display(fmt = "balanceOf returned no result")]
    NoResult,
    /// The `balanceOf` result is not valid hex.
    #[display(fmt = "bad hex: {}", _0)]
    BadHex(String),
    /// The `balanceOf` result is not exactly one 32-byte word (a reverted call returns revert data here).
    #[display(fmt = "balanceOf result is {} bytes, expected 32", _0)]
    BadLength(usize),
    /// The `decimals()` result of a token is above 255 (CRD ch.21 R21.10B.2).
    #[display(fmt = "decimals() result is above 255")]
    DecimalsOutOfRange,
    /// The coin carries no TRON node client.
    #[display(fmt = "TRON API client missing on this coin")]
    ClientMissing,
    /// The coin is not a TRX or TRC20 coin.
    #[display(fmt = "TRON balance requested for a non-TRON coin")]
    NotTronCoin,
}

impl From<TronBalanceError> for WithdrawError {
    /// Keeps the error classes the withdraw pipeline always reported: a negative balance (and the coin-shape
    /// errors) are internal, every failed read is a transport-class error.
    fn from(e: TronBalanceError) -> Self {
        match e {
            TronBalanceError::NegativeBalance | TronBalanceError::ClientMissing | TronBalanceError::NotTronCoin => {
                WithdrawError::InternalError(e.to_string())
            },
            other => WithdrawError::Transport(other.to_string()),
        }
    }
}

impl From<TronBalanceError> for BalanceError {
    /// A failed request is a transport error; a node answer that is not a usable balance (including the node's
    /// own error answer) is an invalid response;
    /// a coin without a node client (or of the wrong family) is an internal error.
    fn from(e: TronBalanceError) -> Self {
        match e {
            TronBalanceError::Node(TronApiError::InvalidResponse(_) | TronApiError::NodeError(_))
            | TronBalanceError::NegativeBalance
            | TronBalanceError::NoResult
            | TronBalanceError::BadHex(_)
            | TronBalanceError::BadLength(_)
            | TronBalanceError::DecimalsOutOfRange => BalanceError::InvalidResponse(e.to_string()),
            TronBalanceError::Node(_) => BalanceError::Transport(e.to_string()),
            TronBalanceError::ClientMissing | TronBalanceError::NotTronCoin => BalanceError::Internal(e.to_string()),
        }
    }
}

/// The TRX balance of `address` in SUN, read from `/wallet/getaccount`.
///
/// An account that does not exist on chain (the documented empty answer) has a balance of zero.
///
/// # Errors
///
/// [`TronBalanceError::Node`] when the request fails or its answer is not usable;
/// [`TronBalanceError::NegativeBalance`] when the node reports a negative balance.
pub(crate) async fn trx_balance_sun(api: &TronApiClient, address: &TronAddress) -> Result<U256, TronBalanceError> {
    let account = api.get_account(address).await.map_err(TronBalanceError::Node)?;
    let sun = account.map_or(0, |account| account.balance);
    let sun = u64::try_from(sun).map_err(|_| TronBalanceError::NegativeBalance)?;
    Ok(U256::from(sun))
}

/// The balance of `owner` in the TRC20 token at `contract`, in token units, read with `balanceOf(address)`
/// through `/wallet/triggerconstantcontract`. The result must be exactly one 32-byte big-endian word.
///
/// # Errors
///
/// [`TronBalanceError::Node`] when the request fails; [`TronBalanceError::NoResult`],
/// [`TronBalanceError::BadHex`] or [`TronBalanceError::BadLength`] when the answer carries no usable word.
pub(crate) async fn trc20_balance_units(
    api: &TronApiClient,
    contract: &TronAddress,
    owner: &TronAddress,
) -> Result<U256, TronBalanceError> {
    // ABI-encode the balanceOf(address) parameter: one 32-byte slot with the address left-padded with
    // 12 zero bytes.
    let mut parameter = [0u8; 32];
    parameter[12..].copy_from_slice(owner.to_evm_address().as_ref());
    let param_hex = hex::encode(parameter);

    let resp = api
        .trigger_constant_read(owner, contract, "balanceOf(address)", &param_hex)
        .await
        .map_err(TronBalanceError::Node)?;

    let raw = resp
        .constant_result
        .and_then(|results| results.into_iter().next())
        .ok_or(TronBalanceError::NoResult)?;
    let bytes = hex::decode(raw).map_err(|e| TronBalanceError::BadHex(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(TronBalanceError::BadLength(bytes.len()));
    }
    Ok(U256::from_big_endian(&bytes))
}

/// The `decimals()` of the TRC20 token at `contract`, read through `/wallet/triggerconstantcontract` with `caller`
/// as the calling address (CRD ch.21 R21.10B.2, R-A1 step 4). The result must be exactly one 32-byte big-endian
/// word, as for `balanceOf`, and at most 255.
///
/// # Errors
///
/// [`TronBalanceError::Node`] when the request fails; [`TronBalanceError::NoResult`],
/// [`TronBalanceError::BadHex`] or [`TronBalanceError::BadLength`] when the answer carries no usable word;
/// [`TronBalanceError::DecimalsOutOfRange`] when the value is above 255.
pub(crate) async fn trc20_decimals(
    api: &TronApiClient,
    contract: &TronAddress,
    caller: &TronAddress,
) -> Result<u8, TronBalanceError> {
    let resp = api
        .trigger_constant_read(caller, contract, "decimals()", "")
        .await
        .map_err(TronBalanceError::Node)?;
    let raw = resp
        .constant_result
        .and_then(|results| results.into_iter().next())
        .ok_or(TronBalanceError::NoResult)?;
    let bytes = hex::decode(raw).map_err(|e| TronBalanceError::BadHex(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(TronBalanceError::BadLength(bytes.len()));
    }
    let value = U256::from_big_endian(&bytes);
    if value > U256::from(u8::MAX) {
        return Err(TronBalanceError::DecimalsOutOfRange);
    }
    Ok(value.low_u32() as u8)
}

/// The balance of `coin`'s own address in the smallest unit of the coin: SUN for TRX, token units for a
/// TRC20 token. This is the TRON arm of the generic wallet balance (`MarketCoinOps::my_balance`).
///
/// # Errors
///
/// As [`coin_balance_units_of`].
pub(crate) async fn coin_balance_units(coin: &EthCoin) -> MmResult<U256, BalanceError> {
    coin_balance_units_of(coin, coin.my_address).await
}

/// The balance of `owner` in the smallest unit of `coin`: SUN for TRX, token units for a TRC20 token. The HD
/// wallet reads the balance of each of its addresses with it (CRD ch.21 R21.10B.4).
///
/// # Errors
///
/// [`BalanceError::Transport`] when the node cannot be asked, [`BalanceError::InvalidResponse`] when its answer
/// is not a usable balance, [`BalanceError::Internal`] when the coin has no node client or is not a TRON coin.
pub(crate) async fn coin_balance_units_of(coin: &EthCoin, owner: Address) -> MmResult<U256, BalanceError> {
    let api = coin
        .tron_api
        .as_ref()
        .ok_or_else(|| MmError::new(BalanceError::from(TronBalanceError::ClientMissing)))?;
    let owner = TronAddress::from_evm_address(owner);
    let balance = match &coin.coin_type {
        EthCoinType::Tron => trx_balance_sun(api, &owner).await,
        EthCoinType::Trc20 { token_addr, .. } => {
            trc20_balance_units(api, &TronAddress::from_evm_address(*token_addr), &owner).await
        },
        _ => Err(TronBalanceError::NotTronCoin),
    };
    balance.map_to_mm(BalanceError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdraw_keeps_its_error_classes() {
        let negative = WithdrawError::from(TronBalanceError::NegativeBalance);
        assert!(
            matches!(&negative, WithdrawError::InternalError(text) if text == "TRON account reported negative balance")
        );

        let no_result = WithdrawError::from(TronBalanceError::NoResult);
        assert!(matches!(&no_result, WithdrawError::Transport(text) if text == "balanceOf returned no result"));

        let node = WithdrawError::from(TronBalanceError::Node(TronApiError::Transport("down".to_owned())));
        assert!(matches!(&node, WithdrawError::Transport(text) if text == "Transport error: down"));
    }

    #[test]
    fn wallet_balance_errors_are_typed() {
        let transport = BalanceError::from(TronBalanceError::Node(TronApiError::AllNodesFailed("down".to_owned())));
        assert!(matches!(transport, BalanceError::Transport(_)));

        let undecodable = BalanceError::from(TronBalanceError::Node(TronApiError::InvalidResponse("junk".to_owned())));
        assert!(matches!(undecodable, BalanceError::InvalidResponse(_)));

        let node_error = BalanceError::from(TronBalanceError::Node(TronApiError::NodeError(
            "bad address".to_owned(),
        )));
        assert!(matches!(node_error, BalanceError::InvalidResponse(_)));

        for malformed in [
            TronBalanceError::NegativeBalance,
            TronBalanceError::NoResult,
            TronBalanceError::BadHex("zz".to_owned()),
            TronBalanceError::BadLength(31),
            TronBalanceError::DecimalsOutOfRange,
        ] {
            assert!(matches!(
                BalanceError::from(malformed),
                BalanceError::InvalidResponse(_)
            ));
        }

        for internal in [TronBalanceError::ClientMissing, TronBalanceError::NotTronCoin] {
            assert!(matches!(BalanceError::from(internal), BalanceError::Internal(_)));
        }
    }
}
