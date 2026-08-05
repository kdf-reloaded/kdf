//! ERC-20 token activation (CRD §35.2 -- `enable_erc20`).
//!
//! Implements the generic single-token activation traits for the EVM token
//! (an `EthCoin` whose coin-type is `Erc20`), so the framework's
//! `enable_token::<EthCoin>` entrypoint can activate one ERC-20 token against an
//! already-active EVM platform coin. The same `EthCoin` type acts as both the
//! platform coin and the token (distinguished by its internal coin-type), which
//! is why `TokenOf::PlatformCoin` and `TryPlatformCoinFromMmCoinEnum` are both
//! implemented for `EthCoin`.

use crate::platform_coin_with_tokens::RegisterTokenInfo;
use crate::prelude::*;
use crate::token::{EnableTokenError, TokenActivationOps, TokenProtocolParams};
use async_trait::async_trait;
use coins::eth::{addr_from_str, EthCoin};
use coins::{CoinBalance, CoinProtocol, MarketCoinOps, MmCoin, MmCoinEnum};
use common::Future01CompatExt;
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;

impl TryPlatformCoinFromMmCoinEnum for EthCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized,
    {
        match coin {
            MmCoinEnum::EthCoin(coin) => Some(coin),
            _ => None,
        }
    }
}

/// Per-token activation parameters (R35.2.1 `activation_params`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Erc20ActivationRequest {
    #[serde(default)]
    pub required_confirmations: Option<u64>,
}

/// Token protocol info resolved from coin configuration (R35.2.2): the parent
/// platform ticker and the on-chain contract address.
pub struct Erc20Protocol {
    pub platform_coin_ticker: String,
    pub contract_address: String,
}

impl TryFromCoinProtocol for Erc20Protocol {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::ERC20 {
                platform,
                contract_address,
            } => Ok(Erc20Protocol {
                platform_coin_ticker: platform,
                contract_address,
            }),
            proto => MmError::err(proto),
        }
    }
}

impl TokenProtocolParams for Erc20Protocol {
    fn platform_coin_ticker(&self) -> &str { &self.platform_coin_ticker }
}

/// Single-token activation result (R35.2.3).
#[derive(Debug, Serialize)]
pub struct Erc20InitResult {
    balances: HashMap<String, CoinBalance>,
    platform_coin: String,
    token_contract_address: String,
    required_confirmations: u64,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenActivationOps for EthCoin {
    type PlatformCoin = EthCoin;
    type ActivationParams = Erc20ActivationRequest;
    type ProtocolInfo = Erc20Protocol;
    type ActivationResult = Erc20InitResult;
    type ActivationError = EnableTokenError;

    async fn enable_token(
        ticker: String,
        platform_coin: Self::PlatformCoin,
        activation_params: Self::ActivationParams,
        protocol_conf: Self::ProtocolInfo,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>> {
        let token_addr = addr_from_str(&protocol_conf.contract_address)
            .map_to_mm(|e| EnableTokenError::Internal(format!("Invalid ERC20 contract address: {}", e)))?;

        // Confirmation settings from the activation request take priority over
        // the platform coin's default.
        let required_confirmations = activation_params
            .required_confirmations
            .unwrap_or_else(|| platform_coin.required_confirmations());

        let token = platform_coin
            .erc20_token_from_conf_or_contract(ticker, token_addr, required_confirmations)
            .await
            .map_to_mm(EnableTokenError::Internal)?;

        // Register the token on the platform coin so the platform's balance
        // enumeration includes it.
        platform_coin.register_token_info(&token);

        let balance = token.my_balance().compat().await.mm_err(EnableTokenError::from)?;
        let my_address = token.my_address().map_to_mm(EnableTokenError::Internal)?;
        let mut balances = HashMap::new();
        balances.insert(my_address, balance);

        let init_result = Erc20InitResult {
            balances,
            platform_coin: platform_coin.ticker().to_owned(),
            token_contract_address: protocol_conf.contract_address,
            required_confirmations: token.required_confirmations(),
        };
        Ok((token, init_result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::mm_number::BigDecimal;

    #[test]
    fn activation_request_deserializes_and_defaults() {
        let req: Erc20ActivationRequest = serde_json::from_str(r#"{"required_confirmations":5}"#).unwrap();
        assert_eq!(req.required_confirmations, Some(5));

        let empty: Erc20ActivationRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(empty.required_confirmations, None);
    }

    #[test]
    fn protocol_from_erc20_coin_protocol() {
        let proto = CoinProtocol::ERC20 {
            platform: "ETH".to_owned(),
            contract_address: "0xdAC17F958D2ee523a2206206994597C13D831ec7".to_owned(),
        };
        let parsed = Erc20Protocol::try_from_coin_protocol(proto).unwrap();
        assert_eq!(parsed.platform_coin_ticker(), "ETH");
        assert_eq!(parsed.contract_address, "0xdAC17F958D2ee523a2206206994597C13D831ec7");
    }

    #[test]
    fn protocol_rejects_non_erc20() {
        let proto = CoinProtocol::ETH { chain_id: Some(1) };
        assert!(Erc20Protocol::try_from_coin_protocol(proto).is_err());
    }

    #[test]
    fn init_result_serializes_expected_fields() {
        let mut balances = HashMap::new();
        balances.insert("0xabc".to_owned(), CoinBalance {
            spendable: BigDecimal::from(7),
            unspendable: BigDecimal::from(0),
        });
        let result = Erc20InitResult {
            balances,
            platform_coin: "ETH".to_owned(),
            token_contract_address: "0xdAC17F958D2ee523a2206206994597C13D831ec7".to_owned(),
            required_confirmations: 3,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["platform_coin"], "ETH");
        assert_eq!(
            v["token_contract_address"],
            "0xdAC17F958D2ee523a2206206994597C13D831ec7"
        );
        assert_eq!(v["required_confirmations"], 3);
        assert_eq!(v["balances"]["0xabc"]["spendable"], "7");
    }
}
