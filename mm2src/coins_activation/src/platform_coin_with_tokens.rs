use crate::context::lock_activation;
use crate::prelude::*;
use async_trait::async_trait;
use coins::activation_locks::ActivationGuard;
use coins::coin_work::PendingCoinWork;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
#[cfg(not(target_arch = "wasm32"))]
use coins::sql_tx_history_storage::SqliteTxHistoryStorage;
use coins::swap_evidence::SwapEvidenceNodeNotInList;
use coins::{lp_coinfind_registered, CoinProtocol, CoinsContext, MmCoinEnum, RegisteredTokenPolicy};
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
    /// Read by the generic initializer's implementors; the Solana initializer resolves its tokens itself (it accepts
    /// an inline custom-token `protocol`, CRD ch.40 R40.2.1), so no implementor remains in this crate.
    #[allow(dead_code)]
    pub(crate) ticker: String,
    /// The per-token request fields. No generic initializer reads them at present: the BCH initializer,
    /// which did, resolves its tokens itself (it accepts an inline custom-token `protocol`, CRD ch.55
    /// R55.9.2), the same way the EVM initializer does.
    #[allow(dead_code)]
    pub(crate) activation_request: Req,
    #[allow(dead_code)]
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
    /// A token entry of the request holds a member its platform cannot serve: the `gasless` block of a TRC20 token
    /// (CRD ch.63 §63.3.2).
    InvalidPayload(String),
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

        // The platform's token record is written at registration, not here (CRD ch.43 D43.1F.2 (b)).
        let tokens = self.enable_tokens(token_params).await.mm_err(Into::into)?;
        Ok(tokens.into_iter().map(Into::into).collect())
    }
}

pub trait GetPlatformBalance {
    fn get_platform_balance(&self) -> BigDecimal;
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait PlatformWithTokensActivationOps: Into<MmCoinEnum> + TryPlatformCoinFromMmCoinEnum {
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

    /// Records `token`, a coin built by one of this platform's token initializers, on the token record the
    /// platform instance keeps for its balance reports (CRD ch.43 R43.1F.3 item 6, D43.1F.2 (b)). A coin of
    /// another kind is ignored.
    fn record_token(&self, token: &MmCoinEnum);

    /// Forgets the token recorded for `ticker` by [`Self::record_token`]; a ticker that is not recorded is
    /// ignored.
    fn forget_token_record(&self, ticker: &str);

    /// The ticker of the NFT chain that the request activates beside the platform coin, if any (CRD ch.43
    /// R43.1F.8). Only an EVM platform has one.
    fn requested_nft_ticker(&self, _activation_request: &Self::ActivationRequest) -> Option<String> { None }

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

    /// Whether the platform has a transaction-history process to start for a request that asks for one. A
    /// platform without one ignores `tx_history` (CRD ch.21 R21.10B.2 for TRON).
    fn history_supported(&self) -> bool { true }

    /// Completes the registration of what the activation set up beside the coins, once the platform and its
    /// tokens are registered. `reactivation` is true for the re-activation of a passive platform, which keeps
    /// the processes the platform already runs (CRD ch.43 D43.1F.5 (a)).
    fn on_platform_registered(
        _ctx: &MmArc,
        _ticker: &str,
        _activation_request: &Self::ActivationRequest,
        _reactivation: bool,
    ) {
    }

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
    pub(crate) client_id: u64,
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
    /// A requested token is registered and is not a dependent of the platform being activated (CRD ch.43
    /// R43.1F.4).
    TokenIsAlreadyActivated(String),
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
    /// A `swap_evidence_node` outside the platform's node list (CRD ch.51 R100 item 2).
    #[display(fmt = "{}", _0)]
    SwapEvidenceNodeNotInList(SwapEvidenceNodeNotInList),
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

impl From<SwapEvidenceNodeNotInList> for EnablePlatformCoinWithTokensError {
    fn from(error: SwapEvidenceNodeNotInList) -> Self {
        EnablePlatformCoinWithTokensError::SwapEvidenceNodeNotInList(error)
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
            InitTokensAsMmCoinsError::InvalidPayload(e) => EnablePlatformCoinWithTokensError::InvalidPayload(e),
        }
    }
}

impl HttpStatusCode for EnablePlatformCoinWithTokensError {
    fn status_code(&self) -> StatusCode {
        match self {
            EnablePlatformCoinWithTokensError::CoinProtocolParseError { .. }
            | EnablePlatformCoinWithTokensError::TokenProtocolParseError { .. }
            | EnablePlatformCoinWithTokensError::CustomTokenError(_)
            | EnablePlatformCoinWithTokensError::SwapEvidenceNodeNotInList(_)
            | EnablePlatformCoinWithTokensError::PlatformCoinCreationError { .. }
            | EnablePlatformCoinWithTokensError::PrivKeyPolicyNotAllowed(_)
            | EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(_)
            | EnablePlatformCoinWithTokensError::TaskTimedOut { .. }
            | EnablePlatformCoinWithTokensError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            EnablePlatformCoinWithTokensError::Transport(_) => StatusCode::BAD_GATEWAY,
            EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(_)
            | EnablePlatformCoinWithTokensError::TokenIsAlreadyActivated(_)
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

/// What a platform-with-tokens request names (CRD ch.43 R43.1F.2).
pub(crate) enum RequestedPlatform<Platform> {
    /// The ticker is not registered: a first activation builds the platform.
    New,
    /// The ticker is a passive platform of the method's family: the request re-activates this instance.
    Passive(Platform),
}

/// Decides what the platform ticker of a request names, before the platform's configuration is read (CRD ch.43
/// R43.1F.2): an active ticker, and a passive ticker of another family, answer `PlatformIsAlreadyActivated`; a
/// passive ticker of the method's family is re-activated. The activation locks of the ticker make the answer
/// stable for the rest of the activation; `init` of a task asks without them.
pub(crate) async fn requested_platform<Platform>(
    ctx: &MmArc,
    ticker: &str,
) -> Result<RequestedPlatform<Platform>, MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: PlatformWithTokensActivationOps,
{
    let registered = match lp_coinfind_registered(ctx, ticker).await {
        Ok(Some(coin)) => coin,
        _ => return Ok(RequestedPlatform::New),
    };
    let coins_ctx = CoinsContext::from_ctx(ctx).map_to_mm(EnablePlatformCoinWithTokensError::Internal)?;
    if coins_ctx.is_passive(ticker).await {
        if let Some(platform) = Platform::try_from_mm_coin(registered) {
            return Ok(RequestedPlatform::Passive(platform));
        }
    }
    MmError::err(EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(
        ticker.to_owned(),
    ))
}

/// The policy for a requested token that is already registered (CRD ch.43 R43.1F.4, D43.1F.1): the
/// `reactivation_registered_token` member of the startup configuration, which the node start has validated.
fn registered_token_policy(ctx: &MmArc) -> RegisteredTokenPolicy {
    RegisteredTokenPolicy::from_conf(&ctx.conf).unwrap_or(RegisteredTokenPolicy::DEFAULT)
}

/// Refuses a requested token that is registered for another platform, before anything is built (CRD ch.43
/// R43.1F.4); a registered token of this platform is kept and joins it again. Runs with the activation locks held,
/// in a first activation and in a re-activation.
async fn check_requested_tokens<Platform>(
    ctx: &MmArc,
    ticker: &str,
    request: &Platform::ActivationRequest,
) -> Result<(), MmError<EnablePlatformCoinWithTokensError>>
where
    Platform: PlatformWithTokensActivationOps,
{
    let coins_ctx = CoinsContext::from_ctx(ctx).map_to_mm(EnablePlatformCoinWithTokensError::Internal)?;
    coins_ctx
        .check_requested_tokens(
            ticker,
            &Platform::requested_token_tickers(request),
            registered_token_policy(ctx),
        )
        .await
        .mm_err(|e| EnablePlatformCoinWithTokensError::TokenIsAlreadyActivated(e.ticker))
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
    // The checks below are the ones that follow the wait.
    let _activation_guard = lock_platform_activation::<Platform>(&ctx, &req.ticker, &req.request).await?;
    let requested = requested_platform::<Platform>(&ctx, &req.ticker).await?;
    check_requested_tokens::<Platform>(&ctx, &req.ticker, &req.request).await?;

    let (platform_coin, reactivation, work) = match requested {
        RequestedPlatform::Passive(platform_coin) => (platform_coin, true, None),
        RequestedPlatform::New => {
            let (platform_conf, platform_protocol) = coin_conf_with_protocol(&ctx, &req.ticker).mm_err(Into::into)?;

            let priv_key = &*ctx.legacy_passphrase_key_pair().private().secret;

            // The work the build spawns belongs to the instance, and ends if the instance is not registered (CRD
            // ch.43 R43.1E.5 item 4, §43.1E not-established item (1)).
            let work = PendingCoinWork::new();
            let platform_coin = work
                .scope(Platform::enable_platform_coin(
                    ctx.clone(),
                    req.ticker.clone(),
                    platform_conf,
                    req.request.clone(),
                    platform_protocol,
                    priv_key,
                ))
                .await
                .mm_err(Into::into)?;
            (platform_coin, false, Some(work))
        },
    };

    finalize_platform_coin_activation::<Platform, NoXPubExtractor>(
        ctx,
        &req.ticker,
        platform_coin,
        reactivation,
        work,
        &req.request,
        None,
    )
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

    fn hardware_context(&self) -> Option<crypto::HardwareWalletArc> { None }
}

/// Shared post-build activation tail: enable the inline tokens, enumerate
/// balances, start any background history fetch, and register the platform coin
/// with its tokens. Used by both the one-shot activation routine and the
/// long-running task path (CRD ch. 48) so the two produce identical results.
///
/// `reactivation` is true when `platform_coin` is the passive instance of a re-activation (CRD ch.43 R43.1F.3):
/// no history collector is started for it (D43.1F.5), and its registration makes the registered entry active
/// instead of inserting one.
///
/// `work` is the background work that the build of a new platform instance spawned; it is `None` for a re-activation.
/// The registry keeps it when the platform is registered, and it ends when this call fails first (CRD ch.43 R43.1E.5
/// item 4, §43.1E not-established item (1)).
///
/// The token record of the platform instance is written once the tokens are built and before the answer is
/// assembled, which reads it. Every token this call added to the record is removed again when the call fails, so
/// a refused request leaves no token of its own on the record (D43.1F.2 (b)). The activation locks are held
/// throughout.
pub(crate) async fn finalize_platform_coin_activation<Platform, XPubExtractor>(
    ctx: MmArc,
    ticker: &str,
    platform_coin: Platform,
    reactivation: bool,
    work: Option<PendingCoinWork>,
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

    // A token that is already a dependent keeps its instance and its record (CRD ch.43 R43.1F.4). A registered token
    // of this platform that is not a dependent keeps its instance too and is recorded from that instance; the others
    // are recorded from the built tokens. What is recorded now is forgotten again if the call fails.
    let dependents = CoinsContext::from_ctx(&ctx)
        .map_to_mm(EnablePlatformCoinWithTokensError::Internal)?
        .dependents_of(ticker)
        .await;
    let mut recorded: Vec<String> = Vec::new();
    for token in &mm_tokens {
        let token_ticker = token.ticker();
        if dependents.contains(token_ticker) || recorded.iter().any(|t| t == token_ticker) {
            continue;
        }
        match lp_coinfind_registered(&ctx, token_ticker).await {
            Ok(Some(registered)) => platform_coin.record_token(&registered),
            _ => platform_coin.record_token(token),
        }
        recorded.push(token_ticker.to_owned());
    }

    // The answer lists every token on the record, so it is assembled after the record is written.
    let answer = async {
        let activation_result = platform_coin
            .get_activation_result(&ctx, request, xpub_extractor)
            .await
            .mm_err(Into::into)?;
        log::info!("{} current block {}", ticker, activation_result.current_block());

        Ok::<_, MmError<EnablePlatformCoinWithTokensError>>(activation_result)
    }
    .await;
    let activation_result = match answer {
        Ok(activation_result) => activation_result,
        Err(error) => {
            forget_recorded_tokens(&platform_coin, &recorded);
            return Err(error);
        },
    };

    let nft_ticker = platform_coin.requested_nft_ticker(request);
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    if reactivation {
        // The activation locks keep `disable_coin` from removing the passive entry meanwhile, so this refusal is
        // not reached; it changes nothing (CRD ch.43 R43.1F.3 item 5, D43.1F.2 (b)).
        if let Err(e) = coins_ctx.reactivate_platform_with_tokens(ticker, mm_tokens).await {
            forget_recorded_tokens(&platform_coin, &recorded);
            return MmError::err(EnablePlatformCoinWithTokensError::Internal(format!(
                "The passive platform {} is not registered any more",
                e.get_inner().ticker
            )));
        }
    } else {
        // The platform instance is new and dropped with the refusal, together with its token record.
        coins_ctx
            .add_platform_with_tokens_and_work(platform_coin.into(), work.unwrap_or_default(), mm_tokens)
            .await
            .mm_err(|e| EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated(e.ticker))?;
    }
    // The NFT chain of an inline `nft_req` is a dependent of the platform, not a token on its record (CRD ch.43
    // R43.1F.8 item 1).
    if let Some(nft_ticker) = nft_ticker {
        coins_ctx.add_nft_dependent(ticker, &nft_ticker).await;
    }
    // The hook runs in the scope of the registered instance, so a process it starts (the TRON history) is work of that
    // instance: removing the platform aborts it (CRD ch.43 R43.1E.5 item 4, ch.55 R55.4.9). A re-activation runs it in
    // the scope of the passive platform's existing record; no process starts then, and the NFT marking does not spawn.
    match coins_ctx.coin_work(ticker).await {
        Some(work) => work.run_in_scope(|| Platform::on_platform_registered(&ctx, ticker, request, reactivation)),
        None => Platform::on_platform_registered(&ctx, ticker, request, reactivation),
    }

    // The history collector starts after the registry accepted the platform, so a failure before the registration
    // leaves no loop behind and the loop's first registration check finds the platform (CRD ch.55 D55.8). A
    // re-activation starts none; the passive platform keeps the one it had (CRD ch.43 D43.1F.5 (a)).
    #[cfg(not(target_arch = "wasm32"))]
    if !reactivation && request.tx_history() {
        start_history_collector::<Platform>(&ctx, ticker, &activation_result).await;
    }

    Ok(activation_result)
}

/// Starts the transaction-history collector of the platform that has just been registered (CRD ch.55 D55.8).
#[cfg(not(target_arch = "wasm32"))]
async fn start_history_collector<Platform>(ctx: &MmArc, ticker: &str, activation_result: &Platform::ActivationResult)
where
    Platform: PlatformWithTokensActivationOps,
{
    let registered = match lp_coinfind_registered(ctx, ticker).await {
        Ok(Some(coin)) => Platform::try_from_mm_coin(coin),
        _ => None,
    };
    let Some(platform_coin) = registered else {
        log::warn!("{ticker}: the registered platform was not found, no history collector was started");
        return;
    };
    if !platform_coin.history_supported() {
        return;
    }
    // The collector is work of the registered instance: removing the platform aborts it (CRD ch.43 R43.1E.5 item 4),
    // and it runs in the instance's scope, where it reads its instance id (CRD ch.55 R55.4.9).
    let work = match CoinsContext::from_ctx(ctx) {
        Ok(coins_ctx) => coins_ctx.coin_work(ticker).await,
        Err(_) => None,
    };
    let Some(work) = work else {
        log::warn!("{ticker}: the registered platform has no background work record, no history collector was started");
        return;
    };
    let abort_handler = work.run_in_scope(|| {
        platform_coin.start_history_background_fetching(
            ctx.clone(),
            ctx.metrics.clone(),
            SqliteTxHistoryStorage(ctx.sqlite_connection.as_option().unwrap().clone()),
            activation_result.get_platform_balance(),
        )
    });
    work.add(abort_handler);
}

/// Removes the tokens a failed activation call added to the token record of the platform instance.
fn forget_recorded_tokens<Platform: PlatformWithTokensActivationOps>(platform_coin: &Platform, recorded: &[String]) {
    for token_ticker in recorded {
        platform_coin.forget_token_record(token_ticker);
    }
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
    // The checks below are the ones that follow the wait.
    let _activation_guard = lock_platform_activation::<Platform>(&ctx, &req.ticker, &req.request).await?;
    let requested = requested_platform::<Platform>(&ctx, &req.ticker).await?;
    check_requested_tokens::<Platform>(&ctx, &req.ticker, &req.request).await?;

    let (platform_coin, reactivation, work) = match requested {
        RequestedPlatform::Passive(platform_coin) => (platform_coin, true, None),
        RequestedPlatform::New => {
            let (platform_conf, platform_protocol) = coin_conf_with_protocol(&ctx, &req.ticker).mm_err(Into::into)?;

            let priv_key = &*ctx.legacy_passphrase_key_pair().private().secret;

            task_handle
                .update_in_progress_status(InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin)
                .mm_err(rpc_task_error)?;

            // The work the build spawns belongs to the instance, and ends if the instance is not registered (CRD
            // ch.43 R43.1E.5 item 4, §43.1E not-established item (1)).
            let work = PendingCoinWork::new();
            let platform_coin = work
                .scope(Platform::enable_platform_coin_with_task(
                    ctx.clone(),
                    req.ticker.clone(),
                    platform_conf,
                    req.request.clone(),
                    platform_protocol,
                    priv_key,
                    task_handle,
                ))
                .await
                .mm_err(Into::into)?;
            (platform_coin, false, Some(work))
        },
    };

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

    finalize_platform_coin_activation::<Platform, _>(
        ctx,
        &req.ticker,
        platform_coin,
        reactivation,
        work,
        &req.request,
        xpub_extractor,
    )
    .await
}
