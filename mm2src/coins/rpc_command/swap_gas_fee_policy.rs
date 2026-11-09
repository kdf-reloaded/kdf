//! Top-level (unnamespaced) swap gas-fee policy RPC handlers (CRD §35.6).
//!
//! `get_swap_gas_fee_policy` reads the per-coin swap gas-fee policy currently
//! in effect; `set_swap_gas_fee_policy` updates it. The policy is one of
//! `{Legacy, Low, Medium, High}` (default `Legacy`) and governs how an EVM coin
//! prices gas for subsequent swap transactions (legacy gas price vs. an
//! EIP-1559 max-fee / max-priority-fee tier), and which of its
//! `priority_gas_prices` a Tendermint platform coin prices its swap
//! transactions and trade-fee simulations with (CRD R35.6.8). These RPCs apply
//! only to EVM-family coins and Tendermint platform coins; a Tendermint token
//! has no policy of its own (CRD R35.6.9).

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

use crate::eth::{EthCoin, SwapGasFeePolicy};
use crate::tendermint::TendermintCoin;
use crate::{lp_coinfind_or_err, CoinFindError, MmCoinEnum};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSwapGasFeePolicyRequest {
    /// EVM coin or Tendermint platform coin ticker.
    pub coin: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSwapGasFeePolicyRequest {
    /// EVM coin or Tendermint platform coin ticker.
    pub coin: String,
    /// The policy to put into effect (defaults to `Legacy` when omitted).
    #[serde(default)]
    pub swap_gas_fee_policy: SwapGasFeePolicy,
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SwapGasFeePolicyError {
    #[display(fmt = "No such coin {}", _0)]
    NoSuchCoin(String),
    #[display(fmt = "Coin {} does not support a swap gas-fee policy", _0)]
    NotSupported(String),
}

impl HttpStatusCode for SwapGasFeePolicyError {
    fn status_code(&self) -> StatusCode {
        match self {
            SwapGasFeePolicyError::NoSuchCoin(_) | SwapGasFeePolicyError::NotSupported(_) => StatusCode::BAD_REQUEST,
        }
    }
}

impl From<CoinFindError> for SwapGasFeePolicyError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => SwapGasFeePolicyError::NoSuchCoin(coin),
        }
    }
}

/// A coin that holds a swap gas-fee policy.
enum PolicyCoin {
    Eth(EthCoin),
    Tendermint(TendermintCoin),
}

/// Resolve a ticker to an active EVM coin or Tendermint platform coin, rejecting every other coin, a Tendermint
/// token included, as `NotSupported` (CRD R35.6.4, R35.6.9 item 1).
async fn policy_coin_from_ticker(ctx: &MmArc, ticker: &str) -> MmResult<PolicyCoin, SwapGasFeePolicyError> {
    match lp_coinfind_or_err(ctx, ticker).await.map_mm_err()? {
        MmCoinEnum::EthCoin(eth) => Ok(PolicyCoin::Eth(eth)),
        MmCoinEnum::TendermintCoin(tendermint) => Ok(PolicyCoin::Tendermint(tendermint)),
        _ => MmError::err(SwapGasFeePolicyError::NotSupported(ticker.to_owned())),
    }
}

pub async fn get_swap_gas_fee_policy(
    ctx: MmArc,
    req: GetSwapGasFeePolicyRequest,
) -> MmResult<SwapGasFeePolicy, SwapGasFeePolicyError> {
    Ok(match policy_coin_from_ticker(&ctx, &req.coin).await? {
        PolicyCoin::Eth(coin) => coin.swap_gas_fee_policy(),
        PolicyCoin::Tendermint(coin) => coin.swap_gas_fee_policy(),
    })
}

pub async fn set_swap_gas_fee_policy(
    ctx: MmArc,
    req: SetSwapGasFeePolicyRequest,
) -> MmResult<SwapGasFeePolicy, SwapGasFeePolicyError> {
    Ok(match policy_coin_from_ticker(&ctx, &req.coin).await? {
        PolicyCoin::Eth(coin) => {
            coin.set_swap_gas_fee_policy(req.swap_gas_fee_policy);
            coin.swap_gas_fee_policy()
        },
        // A coin without `priority_gas_prices` accepts and stores any value, with one warning (CRD R35.6.9 item 2).
        PolicyCoin::Tendermint(coin) => {
            coin.set_swap_gas_fee_policy(req.swap_gas_fee_policy);
            coin.swap_gas_fee_policy()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_request_deserializes() {
        let req: GetSwapGasFeePolicyRequest = serde_json::from_str(r#"{"coin":"ETH"}"#).unwrap();
        assert_eq!(req.coin, "ETH");
    }

    #[test]
    fn get_request_rejects_unknown_field() {
        let res: Result<GetSwapGasFeePolicyRequest, _> = serde_json::from_str(r#"{"coin":"ETH","extra":1}"#);
        assert!(res.is_err());
    }

    #[test]
    fn set_request_deserializes_policy() {
        let req: SetSwapGasFeePolicyRequest =
            serde_json::from_str(r#"{"coin":"ETH","swap_gas_fee_policy":"Medium"}"#).unwrap();
        assert_eq!(req.coin, "ETH");
        assert_eq!(req.swap_gas_fee_policy, SwapGasFeePolicy::Medium);
    }

    #[test]
    fn set_request_defaults_to_legacy() {
        let req: SetSwapGasFeePolicyRequest = serde_json::from_str(r#"{"coin":"ETH"}"#).unwrap();
        assert_eq!(req.swap_gas_fee_policy, SwapGasFeePolicy::Legacy);
    }

    #[test]
    fn policy_serializes_as_published_strings() {
        assert_eq!(serde_json::to_string(&SwapGasFeePolicy::Legacy).unwrap(), r#""Legacy""#);
        assert_eq!(serde_json::to_string(&SwapGasFeePolicy::Low).unwrap(), r#""Low""#);
        assert_eq!(serde_json::to_string(&SwapGasFeePolicy::Medium).unwrap(), r#""Medium""#);
        assert_eq!(serde_json::to_string(&SwapGasFeePolicy::High).unwrap(), r#""High""#);
    }
}
