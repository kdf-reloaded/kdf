//! Tendermint token activation (CRD §36.2 -- `enable_tendermint_token`).
//!
//! Implements the single-token activation of the Tendermint token
//! (`TendermintToken`): [`enable_tendermint_token`] activates one Tendermint
//! (IBC / native bank) token against an already-active Tendermint platform
//! coin, from the coins configuration or from an inline custom-token
//! `protocol` (R36.2.1). Unlike the EVM case the token is a distinct type from
//! the platform coin, so `TryPlatformCoinFromMmCoinEnum` is implemented for the
//! platform `TendermintCoin` here.
//!
//! The protocol of a token, from the coins configuration or inline, is resolved
//! by [`resolve_tendermint_token_protocol`], which the platform-with-tokens
//! activation shares.

use crate::context::lock_activation;
use crate::erc20_token_activation::{configured_protocol, ProtocolResolveError};
use crate::platform_coin_with_tokens::{CustomTokenError, RegisterTokenInfo};
use crate::prelude::*;
use crate::token::{EnableTokenError, TokenActivationOps, TokenProtocolParams};
use async_trait::async_trait;
use coins::tendermint::{TendermintCoin, TendermintToken, TendermintTokenActivationParams};
use coins::{coin_conf, lp_coinfind, lp_coinfind_or_err, CoinBalance, CoinProtocol, CoinsContext, MarketCoinOps,
            MmCoinEnum};
use common::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};
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

/// One entry of `tokens_params` of `enable_tendermint_with_assets` (CRD R36.1.2); the ticker sits beside the
/// members below. Any other member of the entry is accepted and ignored.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TendermintTokenRequest {
    /// The protocol of a custom token that is absent from the coins configuration (CRD R36.1.2, R36.1.4 step 4).
    #[serde(default)]
    pub protocol: Option<CoinProtocol>,
}

/// `enable_tendermint_token` request parameters (CRD R36.2.1).
#[derive(Debug, Deserialize)]
pub struct EnableTendermintTokenRequest {
    ticker: String,
    /// Required, and may be empty; its members are accepted and ignored.
    activation_params: TendermintTokenActivationParams,
    /// The protocol of a custom token that is absent from the coins configuration.
    #[serde(default)]
    protocol: Option<CoinProtocol>,
}

/// Token protocol info resolved from coin configuration or from an inline
/// `protocol` (R36.2.2 / R36.3.1): the parent platform ticker, the token
/// decimals, and the Cosmos / IBC on-chain denomination.
pub struct TendermintTokenProtocol {
    pub platform_coin_ticker: String,
    pub decimals: u8,
    pub denom: String,
    /// The token comes from an inline `protocol` rather than from the coins configuration.
    pub custom: bool,
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
                custom: false,
            }),
            proto => MmError::err(proto),
        }
    }
}

/// Resolves the protocol of a requested Tendermint token: the inline `protocol` of a custom token, else the coins
/// configuration (CRD R36.1.4 step 4, R36.2.2).
///
/// A custom token must not be in the coins configuration (`CustomTokenError::DuplicateTickerInConfig`) and must
/// carry the `TENDERMINTTOKEN` protocol. Whether the token names the platform it is activated on is checked by
/// the caller.
pub(crate) fn resolve_tendermint_token_protocol(
    ctx: &MmArc,
    ticker: &str,
    inline_protocol: Option<&CoinProtocol>,
) -> Result<TendermintTokenProtocol, ProtocolResolveError> {
    let (protocol, custom) = match inline_protocol {
        Some(protocol) => {
            if !coin_conf(ctx, ticker).is_null() {
                return Err(ProtocolResolveError::Custom(
                    CustomTokenError::DuplicateTickerInConfig {
                        ticker_in_config: ticker.to_owned(),
                    },
                ));
            }
            (protocol.clone(), true)
        },
        None => (configured_protocol(ctx, ticker)?, false),
    };
    match TendermintTokenProtocol::try_from_coin_protocol(protocol) {
        Ok(protocol) => Ok(TendermintTokenProtocol { custom, ..protocol }),
        Err(other) => Err(ProtocolResolveError::UnexpectedProtocol {
            ticker: ticker.to_owned(),
            protocol: other.into_inner(),
        }),
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

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
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
        let token = TendermintToken::from_protocol_with_custom_flag(
            ticker,
            platform_coin.clone(),
            protocol_conf.decimals,
            &protocol_conf.denom,
            protocol_conf.custom,
        )
        .mm_err(|e| EnableTokenError::Internal(e.to_string()))?;

        // CRD R36.2.4: a failed balance read, all nodes unreachable included, is `CouldNotFetchBalance`
        // (string, 500) and never `Transport`.
        let balance = token
            .my_balance()
            .compat()
            .await
            .mm_err(|e| EnableTokenError::CouldNotFetchBalance(e.to_string()))?;
        let my_address = token.my_address().map_to_mm(EnableTokenError::Internal)?;
        let mut balances = HashMap::new();
        balances.insert(my_address, balance);

        let init_result = TendermintTokenInitResult {
            balances,
            platform_coin: platform_coin.ticker().to_owned(),
        };
        Ok((token, init_result))
    }

    // The platform's balance enumeration includes the token only once it is registered (CRD R36.2.2).
    fn on_token_registered(platform_coin: &Self::PlatformCoin, token: &Self) {
        platform_coin.register_token_info(token);
    }
}

/// `enable_tendermint_token` (CRD §36.2): activates one Tendermint token on an active platform coin, from the
/// coins configuration or from the inline `protocol` of a custom token.
///
/// The steps follow the generic single-token activation (`token::enable_token`) with one difference: the
/// protocol is resolved by [`resolve_tendermint_token_protocol`], so a custom token is accepted and a duplicate
/// of a configured ticker is a `CustomTokenError`.
pub async fn enable_tendermint_token(
    ctx: MmArc,
    req: EnableTendermintTokenRequest,
) -> Result<TendermintTokenInitResult, MmError<EnableTokenError>> {
    // R55.1.12: the lock is held through registration; the check below is the one that follows the wait.
    let _activation_guard = lock_activation(&ctx, &[&req.ticker])
        .await
        .map_to_mm(EnableTokenError::Internal)?;
    if let Ok(Some(_)) = lp_coinfind(&ctx, &req.ticker).await {
        return MmError::err(EnableTokenError::TokenIsAlreadyActivated(req.ticker));
    }

    let token_protocol = resolve_tendermint_token_protocol(&ctx, &req.ticker, req.protocol.as_ref())
        .map_to_mm(EnableTokenError::from)?;

    let platform_coin = lp_coinfind_or_err(&ctx, token_protocol.platform_coin_ticker())
        .await
        .mm_err(|_| EnableTokenError::PlatformCoinIsNotActivated(token_protocol.platform_coin_ticker().to_owned()))?;

    let platform_coin =
        TendermintCoin::try_from_mm_coin(platform_coin).or_mm_err(|| EnableTokenError::UnsupportedPlatformCoin {
            platform_coin_ticker: token_protocol.platform_coin_ticker().into(),
            token_ticker: req.ticker.clone(),
        })?;

    let (token, activation_result) =
        TendermintToken::enable_token(req.ticker, platform_coin.clone(), req.activation_params, token_protocol).await?;

    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(EnableTokenError::Internal)?;
    coins_ctx
        .add_coin(token.clone().into())
        .await
        .mm_err(|e| EnableTokenError::TokenIsAlreadyActivated(e.ticker))?;
    TendermintToken::on_token_registered(&platform_coin, &token);

    Ok(activation_result)
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
            denom: "uatom".to_owned(),
            decimals: 6,
            ibc_channels: std::collections::HashMap::new(),
            priority_gas_prices: None,
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
