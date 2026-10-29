use crate::context::lock_activation;
use crate::prelude::*;
use async_trait::async_trait;
use coins::activation_locks::ActivationGuard;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
#[cfg(not(target_arch = "wasm32"))]
use coins::sql_tx_history_storage::SqliteTxHistoryStorage;
use coins::{lp_coinfind, CoinProtocol, CoinsContext, MmCoinEnum};
use common::mm_number::BigDecimal;
use common::{log, HttpStatusCode, StatusCode};
use derive_more::Display;
use futures::future::AbortHandle;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use rpc_task::{RpcTaskError, TaskId};
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::convert::Infallible;
use std::time::Duration;

#[derive(Clone, Debug, Deserialize)]
pub struct TokenActivationRequest<Req> {
    pub(crate) ticker: String,
    #[serde(flatten)]
    pub(crate) request: Req,
}

pub trait TokenOf: Into<MmCoinEnum> {
    type PlatformCoin: PlatformWithTokensActivationOps + RegisterTokenInfo<Self>;
}

pub struct TokenActivationParams<Req, Protocol> {
    pub(crate) ticker: String,
    /// The per-token request fields. No generic initializer reads them at present: the BCH initializer,
    /// which did, resolves its tokens itself (it accepts an inline custom-token `protocol`, CRD ch.55
    /// R55.9.2), the same way the EVM initializer does.
    #[allow(dead_code)]
    pub(crate) activation_request: Req,
    pub(crate) protocol: Protocol,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait TokenInitializer {
    type Token: TokenOf;
    type TokenActivationRequest: Send;
    type TokenProtocol: TryFromCoinProtocol + Send;
    type InitTokensError: NotMmError;

    fn tokens_requests_from_platform_request(
        platform_request: &<<Self::Token as TokenOf>::PlatformCoin as PlatformWithTokensActivationOps>::ActivationRequest,
    ) -> Vec<TokenActivationRequest<Self::TokenActivationRequest>>;

    async fn enable_tokens(
        &self,
        params: Vec<TokenActivationParams<Self::TokenActivationRequest, Self::TokenProtocol>>,
    ) -> Result<Vec<Self::Token>, MmError<Self::InitTokensError>>;

    fn platform_coin(&self) -> &<Self::Token as TokenOf>::PlatformCoin;
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait TokenAsMmCoinInitializer: Send + Sync {
    type PlatformCoin;
    type ActivationRequest;

    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<MmCoinEnum>, MmError<InitTokensAsMmCoinsError>>;
}

// `UnexpectedTokenProtocol` carries a `CoinProtocol` by value for diagnostics on
// a cold error path. Boxing it (clippy's suggestion) would force `Box::new` at
// every construction site in this compatibility-sensitive activation code for a
// negligible size win on a rarely built error enum. The per-item allow is the
// established precedent for this lint across coins_activation.
#[allow(clippy::large_enum_variant)]
pub enum InitTokensAsMmCoinsError {
    TokenConfigIsNotFound(String),
    InvalidPubkey(String),
    TokenProtocolParseError {
        ticker: String,
        error: String,
    },
    UnexpectedTokenProtocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    /// A token's protocol names a platform other than the one being activated (CRD R35.1.5).
    PlatformCoinMismatch,
    /// An inline token `protocol` was rejected (CRD R35.2.7).
    CustomTokenError(CustomTokenError),
    /// The nodes gave no usable answer while a token was being built (CRD R35.1.6).
    Transport(String),
}

impl From<CoinConfWithProtocolError> for InitTokensAsMmCoinsError {
    fn from(err: CoinConfWithProtocolError) -> Self {
        match err {
            CoinConfWithProtocolError::ConfigIsNotFound(e) => InitTokensAsMmCoinsError::TokenConfigIsNotFound(e),
            CoinConfWithProtocolError::CoinProtocolParseError { ticker, err } => {
                InitTokensAsMmCoinsError::TokenProtocolParseError {
                    ticker,
                    error: err.to_string(),
                }
            },
            CoinConfWithProtocolError::UnexpectedProtocol { ticker, protocol } => {
                InitTokensAsMmCoinsError::UnexpectedTokenProtocol { ticker, protocol }
            },
        }
    }
}

pub trait RegisterTokenInfo<T: TokenOf<PlatformCoin = Self>> {
    fn register_token_info(&self, token: &T);
}

impl From<std::convert::Infallible> for InitTokensAsMmCoinsError {
    fn from(e: Infallible) -> Self { match e {} }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl<T> TokenAsMmCoinInitializer for T
where
    T: TokenInitializer + Send + Sync,
    InitTokensAsMmCoinsError: From<T::InitTokensError>,
{
    type PlatformCoin = <T::Token as TokenOf>::PlatformCoin;
    type ActivationRequest = <Self::PlatformCoin as PlatformWithTokensActivationOps>::ActivationRequest;

    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<MmCoinEnum>, MmError<InitTokensAsMmCoinsError>> {
        let tokens_requests = T::tokens_requests_from_platform_request(request);
        let token_params = tokens_requests
            .into_iter()
            .map(|req| -> Result<_, MmError<CoinConfWithProtocolError>> {
                let (_, protocol): (_, T::TokenProtocol) = coin_conf_with_protocol(&ctx, &req.ticker)?;
                Ok(TokenActivationParams {
                    ticker: req.ticker,
                    activation_request: req.request,
                    protocol,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .mm_err(Into::into)?;

        let tokens = self.enable_tokens(token_params).await.mm_err(Into::into)?;
        for token in tokens.iter() {
            self.platform_coin().register_token_info(token);
        }
        Ok(tokens.into_iter().map(Into::into).collect())
    }
}

pub trait GetPlatformBalance {
    fn get_platform_balance(&self) -> BigDecimal;
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait PlatformWithTokensActivationOps: Into<MmCoinEnum> {
    type ActivationRequest: Clone + Send + Sync + TxHistory;
    type PlatformProtocolInfo: TryFromCoinProtocol;
    type ActivationResult: GetPlatformBalance + CurrentBlock;
    type ActivationError: NotMmError;

    /// Initializes the platform coin itself
    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: Self::ActivationRequest,
        protocol_conf: Self::PlatformProtocolInfo,
        priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>>;

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>>;

    /// The tickers of the tokens that the request registers beside the platform coin. The activation locks them
    /// together with the platform ticker, so no other activation builds or registers one of them meanwhile
    /// (CRD ch.55 R55.1.12).
    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String>;

    /// Reports the activated platform and its tokens once the tokens are enabled.
    ///
    /// `xpub_extractor` is the source of the extended public keys of a hardware-wallet account when the
    /// activation runs inside a task; a platform with no hardware wallet policy ignores it.
    async fn get_activation_result<XPubExtractor>(
        &self,
        ctx: &MmArc,
        activation_request: &Self::ActivationRequest,
        xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<Self::ActivationResult, MmError<Self::ActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync;

    /// Rejects a request the one-shot call cannot serve before anything else is checked. The task variants
    /// do not call it: the hardware-wallet policy is usable through them only (CRD R35.1.2 phase 1).
    fn check_one_shot_request(
        _activation_request: &Self::ActivationRequest,
    ) -> Result<(), MmError<Self::ActivationError>> {
        Ok(())
    }

    /// Completes the registration of what the activation set up beside the coins, once the platform and its
    /// tokens are registered.
    fn on_platform_registered(_ctx: &MmArc, _ticker: &str, _activation_request: &Self::ActivationRequest) {}

    fn start_history_background_fetching(
        &self,
        ctx: MmArc,
        metrics: MetricsArc,
        storage: impl TxHistoryStorage + 'static,
        initial_balance: BigDecimal,
    ) -> AbortHandle;
}

#[derive(Debug, Deserialize)]
pub struct EnablePlatformCoinWithTokensReq<T: Clone> {
    pub(crate) ticker: String,
    /// The event-stream client the task status changes would be pushed to (CRD ch. 55 R55.2.2).
    /// It is accepted and ignored: streaming task events is deferred (D55.1).
    #[serde(default)]
    #[allow(dead_code)]
    client_id: u64,
    #[serde(flatten)]
    pub(crate) request: T,
}

/// The reason an inline custom token was rejected (CRD R35.2.7). Externally tagged on the wire.
#[derive(Clone, Debug, Display, Serialize)]
pub enum CustomTokenError {
    #[display(fmt = "Token {} is already in the coins config", ticker_in_config)]
    DuplicateTickerInConfig { ticker_in_config: String },
    #[display(fmt = "The contract is already configured as {}", ticker_in_config)]
    DuplicateContractInConfig { ticker_in_config: String },
    #[display(
        fmt = "Token {} with the same contract {} is already activated",
        ticker,
        contract_address
    )]
    TokenWithSameContractAlreadyActivated { ticker: String, contract_address: String },
    #[display(fmt = "The token contract address is invalid")]
    InvalidTokenAddress,
}

#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum EnablePlatformCoinWithTokensError {
    PlatformIsAlreadyActivated(String),
    #[display(fmt = "Platform {} config is not found", _0)]
    PlatformConfigIsNotFound(String),
    #[display(fmt = "{}", _0)]
    AtLeastOneNodeRequired(String),
    #[display(fmt = "Platform coin {} protocol parsing failed: {}", ticker, error)]
    CoinProtocolParseError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Unexpected platform protocol {:?} for {}", protocol, ticker)]
    UnexpectedPlatformProtocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    #[display(fmt = "Token {} config is not found", _0)]
    TokenConfigIsNotFound(String),
    #[display(fmt = "Token {} protocol parsing failed: {}", ticker, error)]
    TokenProtocolParseError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Unexpected token protocol {:?} for {}", protocol, ticker)]
    UnexpectedTokenProtocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    #[display(fmt = "The token protocol names a platform other than the activated one")]
    PlatformCoinMismatch,
    #[display(fmt = "Custom token rejected: {}", _0)]
    CustomTokenError(CustomTokenError),
    #[display(fmt = "Invalid payload: {}", _0)]
    InvalidPayload(String),
    #[display(fmt = "The hardware wallet policy is available only through the task variant")]
    UnexpectedDeviceActivationPolicy,
    #[display(fmt = "WalletConnect error: {}", _0)]
    WalletConnectError(String),
    #[display(fmt = "Error on platform coin {} creation: {}", ticker, error)]
    PlatformCoinCreationError {
        ticker: String,
        error: String,
    },
    // The method tables of the public docs spell this `PrivKeyNotAllowed` and the internal failure `InternalError`;
    // the error catalogue and the reference use `PrivKeyPolicyNotAllowed` and `Internal`, which are the wire names
    // (CRD ch.36 R36.1.7, ch.35 erratum (f)).
    #[display(fmt = "Private key policy is not allowed: {}", _0)]
    PrivKeyPolicyNotAllowed(String),
    #[display(fmt = "Unexpected derivation method: {}", _0)]
    UnexpectedDerivationMethod(String),
    #[display(fmt = "Failed spawning balance events: {}", _0)]
    FailedSpawningBalanceEvents(String),
    Transport(String),
    Internal(String),
    #[display(fmt = "No such task '{}'", _0)]
    NoSuchTask(TaskId),
    #[display(fmt = "The task timed out after {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
}

/// The platform error of a task-framework failure.
///
/// A free function rather than a `From` impl: inside the generic activation routines the `From` bound on the
/// platform's own error type makes `EnablePlatformCoinWithTokensError::from` ambiguous.
pub(crate) fn rpc_task_error(err: RpcTaskError) -> EnablePlatformCoinWithTokensError {
    match err {
        RpcTaskError::NoSuchTask(task_id) => EnablePlatformCoinWithTokensError::NoSuchTask(task_id),
        RpcTaskError::Timeout(duration) => EnablePlatformCoinWithTokensError::TaskTimedOut { duration },
        other => EnablePlatformCoinWithTokensError::Internal(other.to_string()),
    }
}

impl From<CoinConfWithProtocolError> for EnablePlatformCoinWithTokensError {
    fn from(err: CoinConfWithProtocolError) -> Self {
        match err {
            CoinConfWithProtocolError::ConfigIsNotFound(ticker) => {
                EnablePlatformCoinWithTokensError::PlatformConfigIsNotFound(ticker)
            },
            CoinConfWithProtocolError::UnexpectedProtocol { ticker, protocol } => {
                EnablePlatformCoinWithTokensError::UnexpectedPlatformProtocol { ticker, protocol }
            },
            CoinConfWithProtocolError::CoinProtocolParseError { ticker, err } => {
                EnablePlatformCoinWithTokensError::CoinProtocolParseError {
                    ticker,
                    error: err.to_string(),
                }
            },
        }
    }
}

impl From<InitTokensAsMmCoinsError> for EnablePlatformCoinWithTokensError {
    fn from(err: InitTokensAsMmCoinsError) -> Self {
        match err {
            InitTokensAsMmCoinsError::TokenConfigIsNotFound(ticker) => {
                EnablePlatformCoinWithTokensError::TokenConfigIsNotFound(ticker)
            },
            InitTokensAsMmCoinsError::TokenProtocolParseError { ticker, error } => {
                EnablePlatformCoinWithTokensError::TokenProtocolParseError { ticker, error }
            },
            InitTokensAsMmCoinsError::UnexpectedTokenProtocol { ticker, protocol } => {
                EnablePlatformCoinWithTokensError::UnexpectedTokenProtocol { ticker, protocol }
            },
            InitTokensAsMmCoinsError::InvalidPubkey(e) => EnablePlatformCoinWithTokensError::Internal(e),
            InitTokensAsMmCoinsError::PlatformCoinMismatch => EnablePlatformCoinWithTokensError::PlatformCoinMismatch,
            InitTokensAsMmCoinsError::CustomTokenError(e) => EnablePlatformCoinWithTokensError::CustomTokenError(e),
            InitTokensAsMmCoinsError::Transport(e) => EnablePlatformCoinWithTokensError::Transport(e),
        }
    }
}

impl HttpStatusCode for EnablePlatformCoinWithTokensError {
    fn status_code(&self) -> StatusCode {
        match self {
            EnablePlatformCoinWithTokensError::CoinProtocolParseError { .. }
            | EnablePlatformCoinWithTokensError::TokenProtocolParseError { .. }
            | EnablePlatformCoinWithTokensError::CustomTokenError(_)
            | EnablePlatformCoinWithTokensError::PlatformCoinCreationError { .. }
            | EnablePlatformCoinWithTokensError::PrivKeyPolicyNotAllowed(_)
            | EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(_)
            | EnablePlatformCoinWithTokensError::TaskTimedOut { .. }
            | EnablePlatformCoinWithTokensError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            EnablePlatformCoinWithTokensError::Transport(_) => StatusCode::BAD_GATEWAY,
            EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(_)
            | EnablePlatformCoinWithTokensError::PlatformConfigIsNotFound(_)
            | EnablePlatformCoinWithTokensError::AtLeastOneNodeRequired(_)
            | EnablePlatformCoinWithTokensError::TokenConfigIsNotFound(_)
            | EnablePlatformCoinWithTokensError::UnexpectedPlatformProtocol { .. }
            | EnablePlatformCoinWithTokensError::UnexpectedTokenProtocol { .. }
            | EnablePlatformCoinWithTokensError::PlatformCoinMismatch
            | EnablePlatformCoinWithTokensError::InvalidPayload(_)
            | EnablePlatformCoinWithTokensError::UnexpectedDeviceActivationPolicy
            | EnablePlatformCoinWithTokensError::WalletConnectError(_)
            | EnablePlatformCoinWithTokensError::FailedSpawningBalanceEvents(_)
            | EnablePlatformCoinWithTokensError::NoSuchTask(_) => StatusCode::BAD_REQUEST,
        }
    }
}

/// Waits for the activation locks of the platform ticker and of the tickers of the tokens the request registers
/// (CRD ch.55 R55.1.12). They are acquired in one sorted order, so overlapping requests cannot deadlock.
async fn lock_platform_activation<Platform>(
    ctx: &MmArc,
    ticker: &str,
    request: &Platform::ActivationRequest,
) -> Result<ActivationGuard, MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: PlatformWithTokensActivationOps,
{
    let token_tickers = Platform::requested_token_tickers(request);
    let mut tickers: Vec<&str> = token_tickers.iter().map(String::as_str).collect();
    tickers.push(ticker);
    lock_activation(ctx, &tickers)
        .await
        .map_to_mm(EnablePlatformCoinWithTokensError::Internal)
}

pub async fn enable_platform_coin_with_tokens<Platform>(
    ctx: MmArc,
    req: EnablePlatformCoinWithTokensReq<Platform::ActivationRequest>,
) -> Result<Platform::ActivationResult, MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: PlatformWithTokensActivationOps,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    Platform::check_one_shot_request(&req.request).mm_err(Into::into)?;

    // R55.1.12: the platform and every token the request registers are locked until the registration is done.
    // The check below is the one that follows the wait.
    let _activation_guard = lock_platform_activation::<Platform>(&ctx, &req.ticker, &req.request).await?;
    if let Ok(Some(_)) = lp_coinfind(&ctx, &req.ticker).await {
        return MmError::err(EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(
            req.ticker,
        ));
    }

    let (platform_conf, platform_protocol) = coin_conf_with_protocol(&ctx, &req.ticker).mm_err(Into::into)?;

    let priv_key = &*ctx.secp256k1_key_pair().private().secret;

    let platform_coin = Platform::enable_platform_coin(
        ctx.clone(),
        req.ticker.clone(),
        platform_conf,
        req.request.clone(),
        platform_protocol,
        priv_key,
    )
    .await
    .mm_err(Into::into)?;

    finalize_platform_coin_activation::<Platform, NoXPubExtractor>(ctx, &req.ticker, platform_coin, &req.request, None)
        .await
}

/// The extended public keys of a hardware wallet are unavailable: the one-shot call has no device to ask.
pub(crate) struct NoXPubExtractor;

#[async_trait]
impl HDXPubExtractor for NoXPubExtractor {
    async fn extract_utxo_xpub(
        &self,
        _trezor_utxo_coin: crypto::trezor::utxo::TrezorUtxoCoin,
        _derivation_path: crypto::DerivationPath,
    ) -> MmResult<crypto::XPub, coins::hd_pubkey::HDExtractPubkeyError> {
        MmError::err(coins::hd_pubkey::HDExtractPubkeyError::HwContextNotInitialized)
    }

    async fn extract_eth_xpub(
        &self,
        _derivation_path: crypto::DerivationPath,
    ) -> MmResult<crypto::XPub, coins::hd_pubkey::HDExtractPubkeyError> {
        MmError::err(coins::hd_pubkey::HDExtractPubkeyError::HwContextNotInitialized)
    }
}

/// Shared post-build activation tail: enable the inline tokens, enumerate
/// balances, start any background history fetch, and register the platform coin
/// with its tokens. Used by both the one-shot activation routine and the
/// long-running task path (CRD ch. 48) so the two produce identical results.
pub(crate) async fn finalize_platform_coin_activation<Platform, XPubExtractor>(
    ctx: MmArc,
    ticker: &str,
    platform_coin: Platform,
    request: &Platform::ActivationRequest,
    xpub_extractor: Option<&XPubExtractor>,
) -> Result<Platform::ActivationResult, MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: PlatformWithTokensActivationOps,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
    XPubExtractor: HDXPubExtractor + Sync,
{
    let mut mm_tokens = Vec::new();
    for initializer in platform_coin.token_initializers() {
        let tokens = initializer
            .enable_tokens_as_mm_coins(ctx.clone(), request)
            .await
            .mm_err(Into::into)?;
        mm_tokens.extend(tokens);
    }

    let activation_result = platform_coin
        .get_activation_result(&ctx, request, xpub_extractor)
        .await
        .mm_err(Into::into)?;
    log::info!("{} current block {}", ticker, activation_result.current_block());

    #[cfg(not(target_arch = "wasm32"))]
    if request.tx_history() {
        let abort_handler = platform_coin.start_history_background_fetching(
            ctx.clone(),
            ctx.metrics.clone(),
            SqliteTxHistoryStorage(ctx.sqlite_connection.as_option().unwrap().clone()),
            activation_result.get_platform_balance(),
        );
        ctx.abort_handlers.lock().unwrap().push(abort_handler);
    }

    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    coins_ctx
        .add_platform_with_tokens(platform_coin.into(), mm_tokens)
        .await
        .mm_err(|e| EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(e.ticker))?;
    Platform::on_platform_registered(&ctx, ticker, request);

    Ok(activation_result)
}

/// Task-driven platform activation (CRD ch. 48): identical to
/// [`enable_platform_coin_with_tokens`] except the platform coin is built via
/// [`InitPlatformCoinWithTokensActivationOps::enable_platform_coin_with_task`],
/// which threads the task handle so an interactive (hardware-wallet) signing
/// policy can drive the device. The non-interactive policies build the coin
/// exactly as the one-shot routine does (R48.6.1 / R48.6.2).
pub(crate) async fn enable_platform_coin_with_tokens_for_task<Platform>(
    ctx: MmArc,
    task_handle: &rpc_task::RpcTaskHandle<
        crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensTask<Platform>,
    >,
    req: EnablePlatformCoinWithTokensReq<Platform::ActivationRequest>,
) -> Result<Platform::ActivationResult, MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    use crate::init_platform_coin_with_tokens::{xpub_extractor_rpc_statuses,
                                                InitPlatformCoinWithTokensInProgressStatus};
    use coins::hd_pubkey::RpcTaskXPubExtractor;
    use crypto::CryptoCtx;

    // R55.1.12: the platform and every token the request registers are locked until the registration is done.
    // The check below is the one that follows the wait.
    let _activation_guard = lock_platform_activation::<Platform>(&ctx, &req.ticker, &req.request).await?;
    if let Ok(Some(_)) = lp_coinfind(&ctx, &req.ticker).await {
        return MmError::err(EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(
            req.ticker,
        ));
    }

    let (platform_conf, platform_protocol) = coin_conf_with_protocol(&ctx, &req.ticker).mm_err(Into::into)?;

    let priv_key = &*ctx.secp256k1_key_pair().private().secret;

    task_handle
        .update_in_progress_status(InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin)
        .mm_err(rpc_task_error)?;

    let platform_coin = Platform::enable_platform_coin_with_task(
        ctx.clone(),
        req.ticker.clone(),
        platform_conf,
        req.request.clone(),
        platform_protocol,
        priv_key,
        task_handle,
    )
    .await
    .mm_err(Into::into)?;

    if Platform::REPORTS_BALANCE_AND_FINISHING_PROGRESS {
        task_handle
            .update_in_progress_status(InitPlatformCoinWithTokensInProgressStatus::RequestingWalletBalance)
            .mm_err(rpc_task_error)?;
    }

    // The reader of the extended public keys of a hardware wallet account exists only when a device is
    // connected; a software wallet derives its keys from the seed.
    let crypto_ctx =
        CryptoCtx::from_ctx(&ctx).mm_err(|e| EnablePlatformCoinWithTokensError::Internal(e.to_string()))?;
    let xpub_extractor = RpcTaskXPubExtractor::new_unchecked(&ctx, task_handle, xpub_extractor_rpc_statuses());
    let xpub_extractor = crypto_ctx.hw_ctx().map(|_| &xpub_extractor);

    finalize_platform_coin_activation::<Platform, _>(ctx, &req.ticker, platform_coin, &req.request, xpub_extractor)
        .await
}
