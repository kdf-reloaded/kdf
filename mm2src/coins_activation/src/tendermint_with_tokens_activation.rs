//! Tendermint platform-coin-with-tokens activation (CRD §36.1 --
//! `enable_tendermint_with_assets`).
//!
//! Implements the generic platform-coin-with-tokens activation traits for the
//! Tendermint platform coin (`TendermintCoin`), so the framework's
//! `enable_platform_coin_with_tokens::<TendermintCoin>` entrypoint can activate
//! a Tendermint platform coin plus an inline batch of Tendermint (IBC / native
//! bank) tokens in one call.
//!
//! This module covers the software-key activation paths: the single-address
//! ("Iguana") path, where the generic activator passes a secp256k1 private
//! key, and the HD path, where the key is derived from the seed at the coin's
//! `derivation_path` and `path_to_address` (R36.1.2), and the public-key form of `activation_params` (R36.1.3
//! form 1), which activates a watch-only coin (R36.14.16 -- R36.14.18). The WalletConnect forms (R36.1.3 forms 2
//! and 3) are parsed and refused until their activation is built.

use crate::platform_coin_with_tokens::*;
use crate::prelude::*;
use crate::tendermint_token_activation::{resolve_tendermint_token_protocol, TendermintTokenProtocol,
                                         TendermintTokenRequest};
use async_trait::async_trait;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::rpc_command::get_my_address::PathToAddress;
use coins::tendermint::{tendermint_coin_from_conf_and_request, tendermint_coin_from_conf_and_request_with_global_hd,
                        tendermint_coin_from_conf_and_request_with_public_key, RpcNode, TendermintCoin,
                        TendermintInitError, TendermintInitErrorKind, TendermintPublicKey, TendermintToken,
                        TendermintTokenInitError};
use coins::{CoinBalance, MarketCoinOps, MmCoinEnum, PrivKeyActivationPolicy};
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
use crypto::{CryptoCtx, KeyPairPolicy};
use futures::future::{abortable, AbortHandle};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use serde::{Deserialize as _, Deserializer};
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

impl TokenOf for TendermintToken {
    type PlatformCoin = TendermintCoin;
}

fn default_get_balances() -> bool { true }

/// `enable_tendermint_with_assets` request parameters (R36.1.2). `path_to_address` selects the activated
/// address under an HD login and has no effect under an iguana login. `activation_params` (R36.1.3) is parsed at
/// request level, so a malformed one is a request-parse failure; the public-key form activates a watch-only coin
/// and the WalletConnect forms are refused, see [`reject_unsupported_activation_params`].
///
/// The public docs list `required_confirmations` (default 3), `requires_notarization` and `priv_key_policy` for
/// this method. The reference ignores all three for Tendermint, since the docs do not state an observable effect.
/// Reloaded does the same for the first two: they are not declared here and so are ignored (CRD ch.36 R36.1.2).
/// `priv_key_policy` is declared: an absent value or `ContextPrivKey` activates as before, a hardware value is
/// refused, see [`reject_hardware_policy`] (CRD ch.36 §36.7, status note of 2026-11-15). `IguanaPrivKey` under an
/// HD login activates with the single internal key of the context and ignores `path_to_address` (Reloaded-original,
/// CRD ch.36 §36.7, status note of 2026-11-17).
#[derive(Clone, Deserialize)]
pub struct TendermintActivationParams {
    nodes: Vec<RpcNode>,
    // CRD R36.1.2: required, the array itself may be empty.
    tokens_params: Vec<TokenActivationRequest<TendermintTokenRequest>>,
    #[serde(default)]
    tx_history: bool,
    #[serde(default = "default_get_balances")]
    get_balances: bool,
    // Kept raw: it matters under an HD login without `IguanaPrivKey` only, where a value that does not parse is a
    // platform-coin creation error (CRD ch.36 R36.1.4 step 3); under an iguana login it is ignored whatever it holds.
    #[serde(default)]
    path_to_address: Option<Json>,
    #[serde(default, deserialize_with = "deserialize_activation_params")]
    activation_params: Option<ActivationParamsForm>,
    #[serde(default)]
    priv_key_policy: Option<PrivKeyActivationPolicy>,
}

/// The parsed `activation_params` of a request (CRD ch.36 R36.1.3).
#[derive(Clone, Debug, PartialEq)]
enum ActivationParamsForm {
    /// Form 1: a public key and the Ledger flag; the coin activates watch-only.
    PublicKey {
        public_key: TendermintPublicKey,
        is_ledger_connection: bool,
    },
    /// Forms 2 and 3: a WalletConnect session. Parsed, and refused until that activation is built.
    WalletConnect,
}

/// The `pubkey` object of form 1: `{"type": "secp256k1" | "ed25519", "value": [<integer 0..255>, ...]}`.
#[derive(Deserialize)]
#[serde(tag = "type", content = "value")]
enum PubkeyJson {
    #[serde(rename = "secp256k1")]
    Secp256k1(Vec<u8>),
    #[serde(rename = "ed25519")]
    Ed25519(Vec<u8>),
}

#[derive(Deserialize)]
struct WithPubkeyParams {
    pubkey: PubkeyJson,
    is_ledger_connection: bool,
}

#[derive(Deserialize)]
struct WalletConnectParams {
    #[allow(dead_code)]
    session_topic: String,
}

/// Parses `activation_params` (CRD ch.36 R36.1.3). Any shape other than the three forms, a key type other than the
/// two listed, a `value` that is not an array of byte integers and a key of invalid length are a parse failure.
fn parse_activation_params(value: &Json) -> Result<ActivationParamsForm, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "`activation_params` must be an object".to_owned())?;
    // Form 3, the shape of the public docs' example.
    if let Some(policy) = object.get("priv_key_policy") {
        let is_wallet_connect = policy["type"] == "WalletConnect" && policy["data"].is_string();
        return if is_wallet_connect {
            Ok(ActivationParamsForm::WalletConnect)
        } else {
            Err(
                "`activation_params.priv_key_policy` must be {\"type\": \"WalletConnect\", \"data\": <topic>}"
                    .to_owned(),
            )
        };
    }
    let params = object
        .get("params")
        .ok_or_else(|| "`activation_params.params` is missing".to_owned())?;
    match object.get("type").and_then(Json::as_str) {
        Some("WithPubkey") => {
            let params: WithPubkeyParams =
                serde_json::from_value(params.clone()).map_err(|e| format!("Invalid `WithPubkey` params: {e}"))?;
            let public_key = match params.pubkey {
                PubkeyJson::Secp256k1(bytes) => TendermintPublicKey::from_raw_secp256k1(&bytes),
                PubkeyJson::Ed25519(bytes) => TendermintPublicKey::from_raw_ed25519(&bytes),
            }
            .ok_or_else(|| "The public key is not a valid key of its type".to_owned())?;
            Ok(ActivationParamsForm::PublicKey {
                public_key,
                is_ledger_connection: params.is_ledger_connection,
            })
        },
        Some("WalletConnect") => {
            serde_json::from_value::<WalletConnectParams>(params.clone())
                .map_err(|e| format!("Invalid `WalletConnect` params: {e}"))?;
            Ok(ActivationParamsForm::WalletConnect)
        },
        _ => Err("`activation_params.type` must be `WithPubkey` or `WalletConnect`".to_owned()),
    }
}

/// `null` and an absent member are no `activation_params`; anything else is parsed by [`parse_activation_params`].
fn deserialize_activation_params<'de, D>(deserializer: D) -> Result<Option<ActivationParamsForm>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Json>::deserialize(deserializer)? {
        None | Some(Json::Null) => Ok(None),
        Some(value) => parse_activation_params(&value)
            .map(Some)
            .map_err(<D::Error as serde::de::Error>::custom),
    }
}

/// Refuses a hardware `priv_key_policy` before any network access: no Tendermint signing path serves a hardware
/// key yet, and activating with the local key instead would hand a caller a hot-key coin they did not ask for.
/// An absent value, `ContextPrivKey` and `IguanaPrivKey` are not refused (CRD ch.36 §36.7, status note of
/// 2026-11-15).
fn reject_hardware_policy(
    ticker: &str,
    priv_key_policy: &Option<PrivKeyActivationPolicy>,
) -> Result<(), MmError<TendermintInitError>> {
    match priv_key_policy {
        Some(policy @ PrivKeyActivationPolicy::Trezor) => MmError::err(TendermintInitError {
            ticker: ticker.to_owned(),
            kind: TendermintInitErrorKind::HardwarePolicyNotSupported {
                policy: format!("{policy:?}"),
            },
        }),
        _ => Ok(()),
    }
}

/// Rejects a request that asks for a signing policy this build does not provide (CRD ch.36 §36.7, interim
/// divergence, narrowed 2026-11-16).
///
/// The public-key form (R36.1.3 form 1) is built and passes. The WalletConnect forms (R36.1.3 forms 2 and 3) are
/// not built yet and are refused instead of activating with the local context key, so that a caller who asked for
/// a device-backed activation never gets a hot-key one. The discriminant is the one the reference uses for an
/// unsupported signing context. Remove when the WalletConnect activation is built.
fn reject_unsupported_activation_params(
    ticker: &str,
    activation_params: &Option<ActivationParamsForm>,
) -> Result<(), MmError<TendermintActivationError>> {
    match activation_params {
        Some(ActivationParamsForm::WalletConnect) => {
            MmError::err(TendermintActivationError::PlatformCoinCreationError {
                ticker: ticker.to_owned(),
                error: "The `activation_params` signing policies are not supported yet".to_owned(),
            })
        },
        Some(ActivationParamsForm::PublicKey { .. }) | None => Ok(()),
    }
}

/// The `path_to_address` of a request under an HD login: the default (account 0, `External`, address 0) when the
/// request has none or has `null`, else the parsed object. A value that does not parse is a platform-coin creation
/// error (CRD ch.36 R36.1.4 step 3).
fn path_to_address_of(
    ticker: &str,
    path_to_address: &Option<Json>,
) -> Result<PathToAddress, MmError<TendermintActivationError>> {
    match path_to_address {
        Some(path) if !path.is_null() => {
            serde_json::from_value(path.clone()).map_to_mm(|e| TendermintActivationError::PlatformCoinCreationError {
                ticker: ticker.to_owned(),
                error: format!("Invalid `path_to_address`: {e}"),
            })
        },
        _ => Ok(PathToAddress::default()),
    }
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
                // Read by the coin builder from the same `protocol_data` block, like the gas price.
                priority_gas_prices: _,
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

/// Whether every token's protocol names the platform being activated (CRD R36.1.4 step 4).
fn all_on_platform(protocols: &[TendermintTokenProtocol], platform_ticker: &str) -> bool {
    protocols
        .iter()
        .all(|protocol| protocol.platform_coin_ticker == platform_ticker)
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenAsMmCoinInitializer for TendermintTokenInitializer {
    type PlatformCoin = TendermintCoin;
    type ActivationRequest = TendermintActivationParams;

    /// Enables the requested tokens in passes, each covering every token in request order before the next
    /// starts, so the error reported is that of the earliest failing pass: the resolution of every token's
    /// protocol (from the coins configuration, or the inline `protocol` of a custom token), then the platform
    /// consistency of every token (`PlatformCoinMismatch`), then the construction. All tokens are checked before
    /// any is built, so a failure leaves nothing behind (CRD R36.1.4 step 4, R36.1.6).
    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<MmCoinEnum>, MmError<InitTokensAsMmCoinsError>> {
        let requests = &request.tokens_params;

        let mut protocols = Vec::with_capacity(requests.len());
        for token_request in requests {
            let protocol =
                resolve_tendermint_token_protocol(&ctx, &token_request.ticker, token_request.request.protocol.as_ref())
                    .map_to_mm(InitTokensAsMmCoinsError::from)?;
            protocols.push(protocol);
        }

        if !all_on_platform(&protocols, self.platform_coin.ticker()) {
            return MmError::err(InitTokensAsMmCoinsError::PlatformCoinMismatch);
        }

        let mut tokens = Vec::with_capacity(requests.len());
        for (token_request, protocol) in requests.iter().zip(protocols) {
            let token = TendermintToken::from_protocol_with_custom_flag(
                token_request.ticker.clone(),
                self.platform_coin.clone(),
                protocol.decimals,
                &protocol.denom,
                protocol.custom,
            )
            .mm_err(InitTokensAsMmCoinsError::from)?;
            tokens.push(token);
        }

        for token in tokens.iter() {
            self.platform_coin.register_token_info(token);
        }
        Ok(tokens.into_iter().map(Into::into).collect())
    }
}

impl From<TendermintTokenInitError> for InitTokensAsMmCoinsError {
    fn from(err: TendermintTokenInitError) -> Self {
        match err {
            TendermintTokenInitError::InternalError(e) => InitTokensAsMmCoinsError::InvalidPubkey(e),
            TendermintTokenInitError::PlatformCoinMismatch => InitTokensAsMmCoinsError::PlatformCoinMismatch,
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
    // The docs' response table types `tokens_balances` as an array; every docs example and the reference return a
    // ticker-keyed map, which is what Reloaded returns (CRD ch.36 R36.1.5).
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
    UnexpectedDerivationMethod(String),
    Internal(String),
}

impl From<TendermintActivationError> for EnablePlatformCoinWithTokensError {
    fn from(err: TendermintActivationError) -> Self {
        match err {
            TendermintActivationError::PlatformCoinCreationError { ticker, error } => {
                EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error }
            },
            TendermintActivationError::UnexpectedDerivationMethod(e) => {
                EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(e)
            },
            TendermintActivationError::Internal(e) => EnablePlatformCoinWithTokensError::Internal(e),
        }
    }
}

impl From<TendermintInitError> for TendermintActivationError {
    fn from(err: TendermintInitError) -> Self {
        // CRD ch.36 R36.1.6 (public-docs erratum): the public docs list `AtLeastOneNodeRequired` and `Transport` for
        // Tendermint, the reference never returns them (every step-3 failure, an empty `nodes` array included, is
        // `PlatformCoinCreationError`). Reloaded follows the reference.
        let TendermintInitError { ticker, kind } = err;
        TendermintActivationError::PlatformCoinCreationError {
            ticker,
            error: kind.to_string(),
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
        reject_hardware_policy(&ticker, &activation_request.priv_key_policy).mm_err(TendermintActivationError::from)?;
        reject_unsupported_activation_params(&ticker, &activation_request.activation_params)?;

        // A public key activates a watch-only coin: nothing is derived from the login, and no node is contacted
        // here (CRD ch.36 R36.14.16).
        if let Some(ActivationParamsForm::PublicKey {
            public_key,
            is_ledger_connection,
        }) = &activation_request.activation_params
        {
            return tendermint_coin_from_conf_and_request_with_public_key(
                &ctx,
                ticker,
                &platform_conf,
                activation_request.nodes,
                *public_key,
                *is_ledger_connection,
                activation_request.get_balances,
            )
            .await
            .mm_err(TendermintActivationError::from);
        }

        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|e| TendermintActivationError::Internal(e.to_string()))?;
        let coin = match crypto_ctx.key_pair_policy() {
            // An iguana login signs with the single key the generic activator passes.
            KeyPairPolicy::Iguana => {
                tendermint_coin_from_conf_and_request(
                    &ctx,
                    ticker,
                    &platform_conf,
                    activation_request.nodes,
                    priv_key,
                    activation_request.get_balances,
                )
                .await
            },
            // An HD login whose request asks for `IguanaPrivKey` signs with the single internal key of the context,
            // the key the UTXO family takes for this case, and `path_to_address` plays no part (Reloaded-original,
            // CRD ch.36 §36.7, status note of 2026-11-17). The `priv_key` the generic activator passes is the
            // passphrase key of the legacy context under either login; it is not used for the HD login.
            KeyPairPolicy::GlobalHDAccount(_)
                if matches!(
                    activation_request.priv_key_policy,
                    Some(PrivKeyActivationPolicy::IguanaPrivKey)
                ) =>
            {
                let internal_key = crypto_ctx.mm2_internal_privkey_secret();
                tendermint_coin_from_conf_and_request(
                    &ctx,
                    ticker,
                    &platform_conf,
                    activation_request.nodes,
                    internal_key.as_slice(),
                    activation_request.get_balances,
                )
                .await
            },
            // An HD login signs with the key of the address at `path_to_address` under the coin's
            // `derivation_path` (CRD ch.36 R36.1.2, R36.1.4 step 3).
            KeyPairPolicy::GlobalHDAccount(global_hd) => {
                let path_to_address = path_to_address_of(&ticker, &activation_request.path_to_address)?;
                tendermint_coin_from_conf_and_request_with_global_hd(
                    &ctx,
                    ticker,
                    &platform_conf,
                    activation_request.nodes,
                    global_hd,
                    &path_to_address,
                    activation_request.get_balances,
                )
                .await
            },
        }
        .mm_err(TendermintActivationError::from)?;
        Ok(coin)
    }

    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String> {
        activation_request
            .tokens_params
            .iter()
            .map(|token| token.ticker.clone())
            .collect()
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(TendermintTokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result<XPubExtractor>(
        &self,
        _ctx: &MmArc,
        _activation_request: &Self::ActivationRequest,
        _xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<TendermintActivationResult, MmError<TendermintActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        let ticker = self.ticker().to_owned();
        let address = self.my_address().map_to_mm(TendermintActivationError::Internal)?;
        // The first step that contacts a node: unreachable or failing nodes are a platform-coin creation error and
        // never `Transport` (CRD R36.1.6).
        let creation_error = |error: String| TendermintActivationError::PlatformCoinCreationError {
            ticker: ticker.clone(),
            error,
        };
        let current_block = self.current_block().compat().await.map_to_mm(creation_error)?;

        let (balance, tokens_balances, tokens_tickers) = if self.activation_get_balances() {
            let balance = self
                .my_balance()
                .compat()
                .await
                .mm_err(|e| creation_error(e.to_string()))?;
            let tokens_balances = self
                .get_activated_tokens_balances()
                .await
                .mm_err(|e| creation_error(e.to_string()))?;
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
        storage: impl TxHistoryStorage + 'static,
        _initial_balance: BigDecimal,
    ) -> AbortHandle {
        // CRD ch.36 R36.14.3: the platform coin's fetcher is the only writer of the history store. The
        // browser build has no history store in this tree, so there the call starts nothing.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ticker = self.ticker().to_owned();
            let (fut, abort_handle) = abortable(coins::tendermint::start_tendermint_history(self.clone(), storage));
            common::executor::spawn(async move {
                if let Ok(Err(e)) = fut.await {
                    common::log::info!("the Tendermint history fetcher stopped for {}, reason {}", ticker, e);
                }
            });
            abort_handle
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = storage;
            let (_fut, abort_handle) = abortable(futures::future::ready(()));
            abort_handle
        }
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

    // CRD R36.6.4: the Tendermint task reports `ActivatingCoin` and no other in-progress value.
    const REPORTS_BALANCE_AND_FINISHING_PROGRESS: bool = false;

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

#[cfg(all(test, not(target_arch = "wasm32")))] mod wire_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_params_deserializes_with_defaults() {
        let req: TendermintActivationParams = serde_json::from_str(
            r#"{
                "tokens_params": [],
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

    /// CRD ch.55 R55.1.12: the activation locks the tickers of the tokens the request registers beside the platform.
    #[test]
    fn the_requested_token_tickers_are_the_token_tickers_of_the_request() {
        let req: TendermintActivationParams = serde_json::from_str(
            r#"{
                "nodes": [{"url": "https://rpc.example"}],
                "tokens_params": [{"ticker": "IRIS-IBC"}, {"ticker": "IRIS-NYAN"}]
            }"#,
        )
        .unwrap();
        assert_eq!(
            <TendermintCoin as PlatformWithTokensActivationOps>::requested_token_tickers(&req),
            ["IRIS-IBC", "IRIS-NYAN"]
        );
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
            priority_gas_prices: None,
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
