//! Tendermint platform-coin-with-tokens activation (CRD §36.1 --
//! `enable_tendermint_with_assets`).
//!
//! Implements the generic platform-coin-with-tokens activation traits for the
//! Tendermint platform coin (`TendermintCoin`), so the framework's
//! `enable_platform_coin_with_tokens::<TendermintCoin>` entrypoint can activate
//! a Tendermint platform coin plus an inline batch of Tendermint (IBC / native
//! bank) tokens in one call.
//!
//! This module covers the single-address ("Iguana") activation path: the
//! generic activator passes a secp256k1 private key, so HD / Ledger / external
//! signer policies (R36.1.3) -- which require the task variant of §36.6 -- are
//! out of scope here.

use crate::platform_coin_with_tokens::*;
use crate::prelude::*;
use crate::tendermint_token_activation::TendermintTokenProtocol;
use async_trait::async_trait;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::tendermint::{tendermint_coin_from_conf_and_request, RpcNode, TendermintCoin, TendermintInitError,
                        TendermintInitErrorKind, TendermintToken, TendermintTokenActivationParams,
                        TendermintTokenInitError};
use coins::{CoinBalance, MarketCoinOps};
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
use futures::future::{abortable, AbortHandle};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

impl TokenOf for TendermintToken {
    type PlatformCoin = TendermintCoin;
}

fn default_get_balances() -> bool { true }

/// `enable_tendermint_with_assets` request parameters (R36.1.2). The
/// single-address activation subset; `path_to_address` and the alternative
/// `activation_params` signing policy (R36.1.3) are accepted for
/// wire-compatibility but are not consumed on the private-key activation path.
#[derive(Clone, Deserialize)]
pub struct TendermintActivationParams {
    nodes: Vec<RpcNode>,
    #[serde(default)]
    tokens_params: Vec<TokenActivationRequest<TendermintTokenActivationParams>>,
    #[serde(default)]
    tx_history: bool,
    #[serde(default = "default_get_balances")]
    get_balances: bool,
    #[serde(default)]
    #[allow(dead_code)]
    path_to_address: Option<Json>,
    #[serde(default)]
    #[allow(dead_code)]
    activation_params: Option<Json>,
}

impl TxHistory for TendermintActivationParams {
    fn tx_history(&self) -> bool { self.tx_history }
}

/// Tendermint platform protocol info resolved from coin configuration
/// (R36.3.1). The authoritative chain identity / denom / decimals / IBC channels
/// are sourced from the coin config's `protocol_data`; the coin builder reads the
/// same `protocol_data` block when assembling the platform coin. This holder both
/// validates that the configured protocol is `TENDERMINT` and carries the parsed
/// fields for callers that need them.
#[allow(dead_code)]
pub struct TendermintPlatformProtocolInfo {
    account_prefix: String,
    chain_id: String,
    denom: String,
    decimals: u8,
    ibc_channels: HashMap<String, u64>,
}

impl TryFromCoinProtocol for TendermintPlatformProtocolInfo {
    fn try_from_coin_protocol(proto: coins::CoinProtocol) -> Result<Self, MmError<coins::CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            coins::CoinProtocol::TENDERMINT {
                account_prefix,
                chain_id,
                denom,
                decimals,
                ibc_channels,
            } => Ok(TendermintPlatformProtocolInfo {
                account_prefix,
                chain_id,
                denom,
                decimals,
                ibc_channels,
            }),
            proto => MmError::err(proto),
        }
    }
}

/// Token initializer enabling the inline Tendermint tokens of an
/// `enable_tendermint_with_assets` request.
pub struct TendermintTokenInitializer {
    platform_coin: TendermintCoin,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenInitializer for TendermintTokenInitializer {
    type Token = TendermintToken;
    type TokenActivationRequest = TendermintTokenActivationParams;
    type TokenProtocol = TendermintTokenProtocol;
    type InitTokensError = TendermintTokenInitError;

    fn tokens_requests_from_platform_request(
        platform_params: &TendermintActivationParams,
    ) -> Vec<TokenActivationRequest<Self::TokenActivationRequest>> {
        platform_params.tokens_params.clone()
    }

    async fn enable_tokens(
        &self,
        activation_params: Vec<TokenActivationParams<TendermintTokenActivationParams, TendermintTokenProtocol>>,
    ) -> Result<Vec<TendermintToken>, MmError<TendermintTokenInitError>> {
        let mut tokens = Vec::with_capacity(activation_params.len());
        for params in activation_params {
            let token = TendermintToken::from_protocol(
                params.ticker,
                self.platform_coin.clone(),
                params.protocol.decimals,
                &params.protocol.denom,
            )?;
            tokens.push(token);
        }
        Ok(tokens)
    }

    fn platform_coin(&self) -> &TendermintCoin { &self.platform_coin }
}

impl From<TendermintTokenInitError> for InitTokensAsMmCoinsError {
    fn from(err: TendermintTokenInitError) -> Self {
        match err {
            TendermintTokenInitError::InternalError(e) => InitTokensAsMmCoinsError::InvalidPubkey(e),
        }
    }
}

impl RegisterTokenInfo<TendermintToken> for TendermintCoin {
    fn register_token_info(&self, token: &TendermintToken) {
        self.add_activated_token_info(token.ticker.clone(), token.decimals, token.denom.clone());
    }
}

/// `enable_tendermint_with_assets` success result (R36.1.5). The `balance` /
/// `tokens_balances` view and the `tokens_tickers` view are mutually exclusive,
/// selected by the request's `get_balances` flag; absent fields are omitted.
#[derive(Clone, Debug, Serialize)]
pub struct TendermintActivationResult {
    ticker: String,
    address: String,
    current_block: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    balance: Option<CoinBalance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_balances: Option<HashMap<String, CoinBalance>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_tickers: Option<HashSet<String>>,
}

impl GetPlatformBalance for TendermintActivationResult {
    fn get_platform_balance(&self) -> BigDecimal {
        self.balance
            .as_ref()
            .map(|b| &b.spendable + &b.unspendable)
            .unwrap_or_default()
    }
}

impl CurrentBlock for TendermintActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

#[derive(Debug)]
pub enum TendermintActivationError {
    PlatformCoinCreationError { ticker: String, error: String },
    AtLeastOneNodeRequired,
    UnexpectedDerivationMethod(String),
    Transport(String),
    Internal(String),
}

impl From<TendermintActivationError> for EnablePlatformCoinWithTokensError {
    fn from(err: TendermintActivationError) -> Self {
        match err {
            TendermintActivationError::PlatformCoinCreationError { ticker, error } => {
                EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error }
            },
            TendermintActivationError::AtLeastOneNodeRequired => {
                EnablePlatformCoinWithTokensError::AtLeastOneNodeRequired
            },
            TendermintActivationError::UnexpectedDerivationMethod(e) => {
                EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(e)
            },
            TendermintActivationError::Transport(e) => EnablePlatformCoinWithTokensError::Transport(e),
            TendermintActivationError::Internal(e) => EnablePlatformCoinWithTokensError::Internal(e),
        }
    }
}

impl From<TendermintInitError> for TendermintActivationError {
    fn from(err: TendermintInitError) -> Self {
        let TendermintInitError { ticker, kind } = err;
        match kind {
            TendermintInitErrorKind::EmptyRpcUrls => TendermintActivationError::AtLeastOneNodeRequired,
            other => TendermintActivationError::PlatformCoinCreationError {
                ticker,
                error: other.to_string(),
            },
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PlatformWithTokensActivationOps for TendermintCoin {
    type ActivationRequest = TendermintActivationParams;
    type PlatformProtocolInfo = TendermintPlatformProtocolInfo;
    type ActivationResult = TendermintActivationResult;
    type ActivationError = TendermintActivationError;

    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        platform_conf: Json,
        activation_request: Self::ActivationRequest,
        _protocol_conf: Self::PlatformProtocolInfo,
        priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        if activation_request.nodes.is_empty() {
            return MmError::err(TendermintActivationError::AtLeastOneNodeRequired);
        }

        let coin = tendermint_coin_from_conf_and_request(
            &ctx,
            ticker,
            &platform_conf,
            activation_request.nodes,
            priv_key,
            activation_request.get_balances,
        )
        .await
        .mm_err(TendermintActivationError::from)?;
        Ok(coin)
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(TendermintTokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result(&self) -> Result<TendermintActivationResult, MmError<TendermintActivationError>> {
        let ticker = self.ticker().to_owned();
        let address = self.my_address().map_to_mm(TendermintActivationError::Internal)?;
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(TendermintActivationError::Transport)?;

        let (balance, tokens_balances, tokens_tickers) = if self.activation_get_balances() {
            let balance = self
                .my_balance()
                .compat()
                .await
                .mm_err(|e| TendermintActivationError::Transport(e.to_string()))?;
            let tokens_balances = self
                .get_activated_tokens_balances()
                .await
                .mm_err(|e| TendermintActivationError::Transport(e.to_string()))?;
            (Some(balance), Some(tokens_balances), None)
        } else {
            (None, None, Some(self.activated_token_tickers()))
        };

        Ok(TendermintActivationResult {
            ticker,
            address,
            current_block,
            balance,
            tokens_balances,
            tokens_tickers,
        })
    }

    fn start_history_background_fetching(
        &self,
        _ctx: mm2_core::mm_ctx::MmArc,
        _metrics: MetricsArc,
        _storage: impl TxHistoryStorage + 'static,
        _initial_balance: BigDecimal,
    ) -> AbortHandle {
        // Tendermint transaction-history background fetching is not yet wired in
        // reloaded; return a detached no-op abort handle so the platform-with-
        // tokens framework's `tx_history` path stays consistent with the other
        // coins without spawning a fetch loop.
        let (_fut, abort_handle) = abortable(futures::future::ready(()));
        abort_handle
    }
}

/// Per-coin task registry for the Tendermint `task::enable_tendermint::*` family
/// (CRD ch. 48).
pub type TendermintTaskManagerShared =
    crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensTaskManagerShared<TendermintCoin>;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensActivationOps for TendermintCoin {
    fn rpc_task_manager(activation_ctx: &crate::context::CoinsActivationContext) -> &TendermintTaskManagerShared {
        &activation_ctx.init_tendermint_task_manager
    }

    /// Tendermint activation is non-interactive: it builds the coin with the
    /// centrally-threaded local secret exactly as the one-shot activator and
    /// never enters the awaiting state (R48.6.1). The task handle is unused.
    async fn enable_platform_coin_with_task(
        ctx: MmArc,
        ticker: String,
        coin_conf: serde_json::Value,
        activation_request: <Self as PlatformWithTokensActivationOps>::ActivationRequest,
        protocol_conf: <Self as PlatformWithTokensActivationOps>::PlatformProtocolInfo,
        priv_key: &[u8],
        _task_handle: &rpc_task::RpcTaskHandle<
            crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensTask<Self>,
        >,
    ) -> Result<Self, MmError<<Self as PlatformWithTokensActivationOps>::ActivationError>> {
        <Self as PlatformWithTokensActivationOps>::enable_platform_coin(
            ctx,
            ticker,
            coin_conf,
            activation_request,
            protocol_conf,
            priv_key,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_params_deserializes_with_defaults() {
        let req: TendermintActivationParams = serde_json::from_str(
            r#"{
                "nodes": [
                    {"url": "https://rpc1.example"},
                    {"url": "https://rpc2.example", "komodo_proxy": true}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(req.nodes.len(), 2);
        assert!(req.tokens_params.is_empty());
        assert!(!req.tx_history);
        // get_balances defaults to true (R36.1.2).
        assert!(req.get_balances);
        assert!(!req.tx_history());
    }

    #[test]
    fn activation_params_with_tokens_and_flags() {
        let req: TendermintActivationParams = serde_json::from_str(
            r#"{
                "nodes": [{"url": "https://rpc.example"}],
                "tx_history": true,
                "get_balances": false,
                "tokens_params": [
                    {"ticker": "IRIS-IBC", "required_confirmations": 3}
                ]
            }"#,
        )
        .unwrap();
        assert!(req.tx_history());
        assert!(!req.get_balances);
        assert_eq!(req.tokens_params.len(), 1);
    }

    #[test]
    fn protocol_info_from_tendermint_coin_protocol() {
        let mut ibc_channels = HashMap::new();
        ibc_channels.insert("osmo".to_owned(), 141u64);
        let parsed = TendermintPlatformProtocolInfo::try_from_coin_protocol(coins::CoinProtocol::TENDERMINT {
            account_prefix: "cosmos".to_owned(),
            chain_id: "cosmoshub-4".to_owned(),
            denom: "uatom".to_owned(),
            decimals: 6,
            ibc_channels,
        })
        .unwrap();
        assert_eq!(parsed.account_prefix, "cosmos");
        assert_eq!(parsed.chain_id, "cosmoshub-4");
        assert_eq!(parsed.denom, "uatom");
        assert_eq!(parsed.decimals, 6);
        assert_eq!(parsed.ibc_channels.get("osmo"), Some(&141u64));
    }

    #[test]
    fn protocol_info_rejects_non_tendermint() {
        let proto = coins::CoinProtocol::TENDERMINTTOKEN {
            platform: "IRIS".to_owned(),
            denom: "uosmo".to_owned(),
            decimals: 6,
        };
        assert!(TendermintPlatformProtocolInfo::try_from_coin_protocol(proto).is_err());
    }

    #[test]
    fn activation_result_serializes_balances_view() {
        let mut tokens_balances = HashMap::new();
        tokens_balances.insert("IRIS-IBC".to_owned(), CoinBalance {
            spendable: BigDecimal::from(5),
            unspendable: BigDecimal::from(0),
        });
        let result = TendermintActivationResult {
            ticker: "IRIS".to_owned(),
            address: "cosmos1abc".to_owned(),
            current_block: 99,
            balance: Some(CoinBalance {
                spendable: BigDecimal::from(7),
                unspendable: BigDecimal::from(1),
            }),
            tokens_balances: Some(tokens_balances),
            tokens_tickers: None,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["ticker"], "IRIS");
        assert_eq!(v["address"], "cosmos1abc");
        assert_eq!(v["current_block"], 99);
        assert_eq!(v["balance"]["spendable"], "7");
        assert_eq!(v["tokens_balances"]["IRIS-IBC"]["spendable"], "5");
        // tokens_tickers omitted when balances are present.
        assert!(v.get("tokens_tickers").is_none());
        assert_eq!(result.get_platform_balance(), BigDecimal::from(8));
        assert_eq!(result.current_block(), 99);
    }

    #[test]
    fn activation_result_serializes_tickers_view() {
        let mut tickers = HashSet::new();
        tickers.insert("IRIS-IBC".to_owned());
        let result = TendermintActivationResult {
            ticker: "IRIS".to_owned(),
            address: "cosmos1abc".to_owned(),
            current_block: 1,
            balance: None,
            tokens_balances: None,
            tokens_tickers: Some(tickers),
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["tokens_tickers"][0], "IRIS-IBC");
        // balance / tokens_balances omitted when get_balances is false.
        assert!(v.get("balance").is_none());
        assert!(v.get("tokens_balances").is_none());
        assert_eq!(result.get_platform_balance(), BigDecimal::from(0));
    }
}
