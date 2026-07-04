//! `task::connect_metamask::{init,status,cancel}` — the WASM-only RPC family that
//! drives the existing MetaMask login handshake (CRD ch. 47).
//!
//! This is a thin task wrapper around [`CryptoCtx::init_metamask_ctx`], which
//! already performs provider detection, account request, the EIP-712 login
//! challenge, public-key recovery and account verification. The task layer adds
//! the long-running `init`/`status`/`cancel` lifecycle; it deliberately has **no**
//! `user_action` method (§47.3) — the user's in-wallet confirmation is collected
//! by the MetaMask extension popup, not relayed through an RPC call.

use common::{HttpStatusCode, SuccessResponse};
use crypto::metamask::{from_metamask_error, MetamaskRpcError, WithInternal, WithMetamaskRpcError};
use crypto::{CryptoCtx, MetamaskCtxInitError};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::{from_ctx, MmArc};
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest};
use rpc_task::{RpcTask, RpcTaskError, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatus,
               RpcTaskStatusAlias, RpcTaskTypes, TaskId};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type ConnectMetamaskTaskManagerShared = RpcTaskManagerShared<ConnectMetamaskTask>;

/// Per-`MmCtx` context holding the MetaMask connection task manager.
///
/// `in_flight` tracks the most recently spawned task so a second `init` can be
/// rejected while a connection is still establishing (R47.1.4 / A1).
pub struct ConnectMetamaskCtx {
    connect_task_manager: ConnectMetamaskTaskManagerShared,
    in_flight: Mutex<Option<TaskId>>,
}

impl ConnectMetamaskCtx {
    /// Obtains a reference to this context, creating it if necessary.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<ConnectMetamaskCtx>, String> {
        from_ctx(&ctx.metamask_connect_ctx, move || {
            Ok(ConnectMetamaskCtx {
                connect_task_manager: RpcTaskManager::new_shared(),
                in_flight: Mutex::new(None),
            })
        })
    }
}

/// `task::connect_metamask::init` request (§47.1).
#[derive(Deserialize)]
pub struct ConnectMetamaskRequest {
    /// The calling application's name, presented to the user inside the EIP-712
    /// login challenge MetaMask asks them to sign.
    pub project: String,
}

/// `task::connect_metamask::status` success payload (§47.2.3).
#[derive(Clone, Serialize)]
pub struct ConnectMetamaskResult {
    /// The connected MetaMask account address (`0x`-prefixed, as returned by the wallet).
    pub eth_address: String,
}

/// In-progress phases of the connection task (§47.2.2).
#[derive(Clone, Serialize)]
pub enum ConnectMetamaskInProgressStatus {
    /// Detecting the provider and requesting the active account.
    Initializing,
    /// Awaiting the user's EIP-712 login-challenge signature in the wallet popup.
    SigningLoginMetadata,
}

/// `task::connect_metamask::init` error variants (§47.6.1).
#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ConnectMetamaskError {
    #[display(fmt = "MetaMask connection is already initializing")]
    MetamaskInitializingAlready,
    #[display(fmt = "MetaMask error: {}", _0)]
    MetamaskError(MetamaskRpcError),
    #[display(fmt = "Connection task timed out: {:?}", _0)]
    Timeout(Duration),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for ConnectMetamaskError {
    fn status_code(&self) -> StatusCode {
        match self {
            ConnectMetamaskError::MetamaskInitializingAlready => StatusCode::BAD_REQUEST,
            ConnectMetamaskError::MetamaskError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ConnectMetamaskError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
            ConnectMetamaskError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl WithMetamaskRpcError for ConnectMetamaskError {
    fn metamask_rpc_error(err: MetamaskRpcError) -> Self { ConnectMetamaskError::MetamaskError(err) }
}

impl WithInternal for ConnectMetamaskError {
    fn internal(err: String) -> Self { ConnectMetamaskError::Internal(err) }
}

impl From<MetamaskCtxInitError> for ConnectMetamaskError {
    fn from(e: MetamaskCtxInitError) -> Self {
        match e {
            MetamaskCtxInitError::InitializingAlready => ConnectMetamaskError::MetamaskInitializingAlready,
            MetamaskCtxInitError::MetamaskError(err) => from_metamask_error(err),
        }
    }
}

/// The connection task. `run` is a single step driven entirely by the browser
/// popup, so the task has no awaiting/user-action state (§47.3).
pub struct ConnectMetamaskTask {
    ctx: MmArc,
    project: String,
}

impl RpcTaskTypes for ConnectMetamaskTask {
    type Item = ConnectMetamaskResult;
    type Error = ConnectMetamaskError;
    type InProgressStatus = ConnectMetamaskInProgressStatus;
    type AwaitingStatus = ();
    type UserAction = ();
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl RpcTask for ConnectMetamaskTask {
    fn initial_status(&self) -> Self::InProgressStatus { ConnectMetamaskInProgressStatus::Initializing }

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>> {
        let crypto_ctx = CryptoCtx::from_ctx(&self.ctx).mm_err(|e| ConnectMetamaskError::Internal(e.to_string()))?;

        // `init_metamask_ctx` runs the whole handshake (detect -> request account
        // -> EIP-712 login sign -> recover -> verify) as one opaque call, so the
        // task cannot interleave a status update between the account request and
        // the signature. The dominant, user-visible wait is the wallet signature
        // popup, so we surface `SigningLoginMetadata` before invoking it; the
        // `Initializing` phase (registered as the initial status at spawn time)
        // is observable in the window before `run` executes.
        task_handle
            .update_in_progress_status(ConnectMetamaskInProgressStatus::SigningLoginMetadata)
            .mm_err(|e| ConnectMetamaskError::Internal(e.to_string()))?;

        let metamask_arc = crypto_ctx
            .init_metamask_ctx(self.project)
            .await
            .mm_err(ConnectMetamaskError::from)?;
        Ok(ConnectMetamaskResult {
            eth_address: metamask_arc.eth_account_str().to_owned(),
        })
    }
}

/// `task::connect_metamask::init` — start a connection task (§47.1).
pub async fn connect_metamask_init(
    ctx: MmArc,
    req: ConnectMetamaskRequest,
) -> MmResult<InitRpcTaskResponse, ConnectMetamaskError> {
    let metamask_ctx = ConnectMetamaskCtx::from_ctx(&ctx).map_to_mm(ConnectMetamaskError::Internal)?;

    let mut in_flight = metamask_ctx
        .in_flight
        .lock()
        .map_to_mm(|e| ConnectMetamaskError::Internal(e.to_string()))?;

    // Reject a second `init` while a connection is still establishing (R47.1.4).
    if let Some(prev_id) = *in_flight {
        let mut task_manager = metamask_ctx
            .connect_task_manager
            .lock()
            .map_to_mm(|e| ConnectMetamaskError::Internal(e.to_string()))?;
        if let Some(RpcTaskStatus::InProgress(_)) = task_manager.task_status(prev_id, false) {
            return MmError::err(ConnectMetamaskError::MetamaskInitializingAlready);
        }
    }

    let task = ConnectMetamaskTask {
        ctx: ctx.clone(),
        project: req.project,
    };
    let task_id = RpcTaskManager::spawn_rpc_task(&metamask_ctx.connect_task_manager, task)
        .mm_err(|e| ConnectMetamaskError::Internal(e.to_string()))?;
    *in_flight = Some(task_id);

    Ok(InitRpcTaskResponse { task_id })
}

/// `task::connect_metamask::status` — poll progress / final result (§47.2).
pub async fn connect_metamask_status(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<RpcTaskStatusAlias<ConnectMetamaskTask>, RpcTaskStatusError> {
    let metamask_ctx = ConnectMetamaskCtx::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = metamask_ctx
        .connect_task_manager
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

/// `task::connect_metamask::cancel` — abort a pending connection (§47.4).
pub async fn connect_metamask_cancel(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, RpcTaskStatusError> {
    let metamask_ctx = ConnectMetamaskCtx::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    {
        let mut task_manager = metamask_ctx
            .connect_task_manager
            .lock()
            .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
        task_manager.cancel_task(req.task_id).mm_err(|e| match e {
            RpcTaskError::NoSuchTask(task_id) => RpcTaskStatusError::NoSuchTask(task_id),
            other => RpcTaskStatusError::Internal(other.to_string()),
        })?;
    }

    // Reset any partially established session so a subsequent `init` starts cleanly (R47.4.3).
    if let Ok(crypto_ctx) = CryptoCtx::from_ctx(&ctx) {
        crypto_ctx.reset_metamask_ctx();
    }

    Ok(SuccessResponse::new())
}
