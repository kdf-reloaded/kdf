//! Token task-activation substrate (CRD ch. 48 §48.11).
//!
//! The sibling of the platform-coin task substrate for a **token**: activating one token on an already active
//! platform coin as a long-running task. The substrate owns the `init`/`status`/`user_action`/`cancel`
//! lifecycle, the synchronous `init` checks (R48.11.3), the error set (R48.11.4), the progress values
//! (R48.11.5) and the registration and cancel rules (R48.11.7). A token family supplies only what is specific
//! to it through [`InitTokenActivationOps`].

use crate::context::{lock_activation, CoinsActivationContext};
use crate::platform_coin_with_tokens::CustomTokenError;
use crate::prelude::*;
use crate::token::TokenProtocolParams;
use async_trait::async_trait;
use coins::{disable_coin, lp_coinfind, lp_coinfind_or_err, CoinProtocol, CoinsContext, MmCoinEnum};
use common::{log, HttpStatusCode, StatusCode, SuccessResponse};
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{CancelRpcTaskError, InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest,
                           RpcTaskUserActionError, RpcTaskUserActionRequest};
use rpc_task::{CancelCleanup, RpcTask, RpcTaskError, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared,
               RpcTaskStatusAlias, RpcTaskTypes, TaskId};
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub type InitTokenTaskManagerShared<Token> = RpcTaskManagerShared<InitTokenTask<Token>>;
pub type InitTokenTaskHandle<Token> = RpcTaskHandle<InitTokenTask<Token>>;

/// The in-progress values of a token task (CRD R48.11.5).
///
/// The hardware-wallet values of the vocabulary occur only when a token activation talks to a device. The
/// ERC-20 family reads the addresses of the already activated platform wallet and never does.
#[derive(Clone, Serialize)]
pub enum InitTokenInProgressStatus {
    ActivatingCoin,
    RequestingWalletBalance,
    Finishing,
}

/// The error set of every token task: the mmrpc-level errors of `init` and the status-level errors of the
/// task (CRD R48.11.4, public catalogue set `InitTokenError`).
#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
// `UnexpectedTokenProtocol` carries a `CoinProtocol` by value for diagnostics on
// a cold error path. Boxing it (clippy's suggestion) would force `Box::new` at
// every construction site in this compatibility-sensitive activation code for a
// negligible size win on a rarely built RPC error enum. The per-item allow is
// the established precedent for this lint across coins_activation.
#[allow(clippy::large_enum_variant)]
pub enum InitTokenError {
    #[display(fmt = "No such task '{}'", _0)]
    NoSuchTask(TaskId),
    #[display(fmt = "The task timed out after {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    #[display(fmt = "Token {} is already activated", ticker)]
    TokenIsAlreadyActivated {
        ticker: String,
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
    /// Reserved by the chapter: the ERC-20 family reports a token it cannot build as `Transport` or `Internal`.
    #[allow(dead_code)]
    #[display(fmt = "Token {} cannot be created: {}", ticker, error)]
    TokenCreationError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Could not fetch the balance: {}", _0)]
    CouldNotFetchBalance(String),
    #[display(fmt = "Platform coin {} is not activated", _0)]
    PlatformCoinIsNotActivated(String),
    #[display(fmt = "{} is not a platform coin for token {}", platform_coin_ticker, token_ticker)]
    UnsupportedPlatformCoin {
        platform_coin_ticker: String,
        token_ticker: String,
    },
    /// Reserved by the chapter.
    #[allow(dead_code)]
    #[display(fmt = "The token protocol names a platform other than the activated one")]
    PlatformCoinMismatch,
    #[display(fmt = "Custom token rejected: {}", _0)]
    CustomTokenError(CustomTokenError),
    #[display(fmt = "Hardware wallet error: {}", _0)]
    HwError(String),
    Transport(String),
    Internal(String),
}

impl HttpStatusCode for InitTokenError {
    fn status_code(&self) -> StatusCode {
        match self {
            InitTokenError::NoSuchTask(_)
            | InitTokenError::TokenIsAlreadyActivated { .. }
            | InitTokenError::TokenConfigIsNotFound(_)
            | InitTokenError::TokenProtocolParseError { .. }
            | InitTokenError::UnexpectedTokenProtocol { .. }
            | InitTokenError::TokenCreationError { .. }
            | InitTokenError::PlatformCoinIsNotActivated(_)
            | InitTokenError::PlatformCoinMismatch
            | InitTokenError::CustomTokenError(_) => StatusCode::BAD_REQUEST,
            InitTokenError::TaskTimedOut { .. } => StatusCode::REQUEST_TIMEOUT,
            InitTokenError::HwError(_) => StatusCode::GONE,
            InitTokenError::CouldNotFetchBalance(_)
            | InitTokenError::UnsupportedPlatformCoin { .. }
            | InitTokenError::Transport(_)
            | InitTokenError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<RpcTaskError> for InitTokenError {
    fn from(err: RpcTaskError) -> Self {
        match err {
            RpcTaskError::NoSuchTask(task_id) => InitTokenError::NoSuchTask(task_id),
            RpcTaskError::Timeout(duration) => InitTokenError::TaskTimedOut { duration },
            other => InitTokenError::Internal(other.to_string()),
        }
    }
}

/// What a token family supplies to run on the token task substrate.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait InitTokenActivationOps: Into<MmCoinEnum> + Clone + Send + Sync + 'static {
    type PlatformCoin: TryPlatformCoinFromMmCoinEnum + Clone + Send + Sync + 'static;
    /// The `activation_params` of the request.
    type ActivationRequest: Clone + Send + Sync + 'static;
    type ProtocolInfo: TokenProtocolParams + Send + Sync + 'static;
    type ActivationResult: Serialize + Clone + Send + Sync + 'static;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitTokenTaskManagerShared<Self>;

    /// Resolves the token configuration, or the inline `protocol` of a custom token, into the protocol
    /// information of the family (the second synchronous check of `init`).
    fn resolve_protocol(
        ctx: &MmArc,
        ticker: &str,
        inline_protocol: Option<&CoinProtocol>,
    ) -> Result<Self::ProtocolInfo, InitTokenError>;

    /// Builds the token and produces the success object. Runs inside the task and must not register anything.
    async fn init_token(
        ctx: &MmArc,
        ticker: &str,
        platform_coin: &Self::PlatformCoin,
        activation_request: &Self::ActivationRequest,
        protocol_info: &Self::ProtocolInfo,
        task_handle: &InitTokenTaskHandle<Self>,
    ) -> Result<(Self, Self::ActivationResult), MmError<InitTokenError>>;

    /// Records the registered token on its platform coin.
    fn register_token_info(platform_coin: &Self::PlatformCoin, token: &Self);

    /// Forgets a token recorded by [`Self::register_token_info`].
    fn unregister_token_info(platform_coin: &Self::PlatformCoin, ticker: &str);
}

#[derive(Debug, Deserialize)]
pub struct InitTokenReq<Params> {
    ticker: String,
    /// The protocol of a custom token that is not in the coins configuration.
    #[serde(default)]
    protocol: Option<CoinProtocol>,
    activation_params: Params,
    /// The event-stream client the task status changes would be pushed to (CRD ch. 55 R55.2.2).
    /// It is accepted and ignored: streaming task events is deferred (D55.1).
    #[serde(default)]
    #[allow(dead_code)]
    client_id: u64,
}

/// The long-running token activation task.
pub struct InitTokenTask<Token: InitTokenActivationOps> {
    ctx: MmArc,
    ticker: String,
    request: Token::ActivationRequest,
    protocol_info: Token::ProtocolInfo,
    platform_coin: Token::PlatformCoin,
    /// Set once this task has registered its token, so a cancellation knows the token is the task's to remove.
    registered_by_task: Arc<AtomicBool>,
}

impl<Token: InitTokenActivationOps> RpcTaskTypes for InitTokenTask<Token> {
    type Item = Token::ActivationResult;
    type Error = InitTokenError;
    type InProgressStatus = InitTokenInProgressStatus;
    type AwaitingStatus = HwRpcTaskAwaitingStatus;
    type UserAction = HwRpcTaskUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<Token: InitTokenActivationOps> RpcTask for InitTokenTask<Token> {
    fn initial_status(&self) -> Self::InProgressStatus { InitTokenInProgressStatus::ActivatingCoin }

    fn cancel_cleanup(&self) -> Option<CancelCleanup> {
        let ctx = self.ctx.clone();
        let ticker = self.ticker.clone();
        let platform_coin = self.platform_coin.clone();
        let registered_by_task = self.registered_by_task.clone();
        Some(Box::new(move || {
            Box::pin(async move {
                if registered_by_task.load(Ordering::SeqCst) {
                    if let Err(error) = disable_coin(&ctx, &ticker).await {
                        log::warn!("Cannot deregister '{}' of the cancelled activation: {}", ticker, error);
                    }
                    Token::unregister_token_info(&platform_coin, &ticker);
                }
            })
        }))
    }

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>> {
        // R55.1.12: activations of one ticker run one at a time. The lock is held through registration, and the
        // ticker is checked again once it is ours, because the activation that held it may have registered the token.
        let _activation_guard = lock_activation(&self.ctx, &[&self.ticker])
            .await
            .map_to_mm(InitTokenError::Internal)?;
        if let Ok(Some(_)) = lp_coinfind(&self.ctx, &self.ticker).await {
            return MmError::err(InitTokenError::TokenIsAlreadyActivated { ticker: self.ticker });
        }

        let (token, result) = Token::init_token(
            &self.ctx,
            &self.ticker,
            &self.platform_coin,
            &self.request,
            &self.protocol_info,
            task_handle,
        )
        .await?;

        task_handle
            .update_in_progress_status(InitTokenInProgressStatus::Finishing)
            .mm_err(InitTokenError::from)?;

        // Registering is the last step (CRD R48.11.7): a concurrent registration of the same ticker ends the task.
        let coins_ctx = CoinsContext::from_ctx(&self.ctx).map_to_mm(InitTokenError::Internal)?;
        coins_ctx
            .add_coin(token.clone().into())
            .await
            .mm_err(|e| InitTokenError::TokenIsAlreadyActivated { ticker: e.ticker })?;
        self.registered_by_task.store(true, Ordering::SeqCst);
        Token::register_token_info(&self.platform_coin, &token);

        Ok(result)
    }
}

/// `task::<family>::init`: runs the synchronous checks of CRD R48.11.3, in order, and spawns the task.
pub async fn init_token<Token>(
    ctx: MmArc,
    request: InitTokenReq<Token::ActivationRequest>,
) -> MmResult<InitRpcTaskResponse, InitTokenError>
where
    Token: InitTokenActivationOps,
{
    // 1. The token is already active.
    if let Ok(Some(_)) = lp_coinfind(&ctx, &request.ticker).await {
        return MmError::err(InitTokenError::TokenIsAlreadyActivated { ticker: request.ticker });
    }

    // 2. The token configuration or protocol cannot be resolved.
    let protocol_info = Token::resolve_protocol(&ctx, &request.ticker, request.protocol.as_ref()).map_to_mm(|e| e)?;

    // 3. The platform coin is not active.
    let platform_ticker = protocol_info.platform_coin_ticker().to_owned();
    let platform_coin = lp_coinfind_or_err(&ctx, &platform_ticker)
        .await
        .mm_err(|_| InitTokenError::PlatformCoinIsNotActivated(platform_ticker.clone()))?;

    // 4. The active platform coin is not of the token's platform type.
    let platform_coin =
        Token::PlatformCoin::try_from_mm_coin(platform_coin).or_mm_err(|| InitTokenError::UnsupportedPlatformCoin {
            platform_coin_ticker: platform_ticker,
            token_ticker: request.ticker.clone(),
        })?;

    // 5. The task cannot be created.
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(InitTokenError::Internal)?;
    let task = InitTokenTask::<Token> {
        ctx,
        ticker: request.ticker,
        request: request.activation_params,
        protocol_info,
        platform_coin,
        registered_by_task: Arc::new(AtomicBool::new(false)),
    };
    let task_manager = Token::rpc_task_manager(&coins_act_ctx);
    let task_id =
        RpcTaskManager::spawn_rpc_task(task_manager, task).mm_err(|e| InitTokenError::Internal(e.to_string()))?;
    Ok(InitRpcTaskResponse { task_id })
}

/// `task::<family>::status`: the progress or the terminal answer of the task.
pub async fn init_token_status<Token>(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<RpcTaskStatusAlias<InitTokenTask<Token>>, RpcTaskStatusError>
where
    Token: InitTokenActivationOps,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = Token::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

/// `task::<family>::user_action`: no token task of the ERC-20 family ever awaits a user action, so this
/// validates the task id and answers with the task framework's response for a task that is not awaiting.
pub async fn init_token_user_action<Token>(
    ctx: MmArc,
    req: RpcTaskUserActionRequest<HwRpcTaskUserAction>,
) -> MmResult<SuccessResponse, RpcTaskUserActionError>
where
    Token: InitTokenActivationOps,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(RpcTaskUserActionError::Internal)?;
    let mut task_manager = Token::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| RpcTaskUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

/// `task::<family>::cancel`: aborts the task; a token it had already registered is deregistered by the task
/// framework once the cancelled task has stopped (CRD R48.11.7).
pub async fn cancel_init_token<Token>(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, CancelRpcTaskError>
where
    Token: InitTokenActivationOps,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(CancelRpcTaskError::Internal)?;
    let mut task_manager = Token::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| CancelRpcTaskError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}
