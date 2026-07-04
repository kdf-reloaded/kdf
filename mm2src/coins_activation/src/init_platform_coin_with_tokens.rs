//! Shared platform-coin-with-tokens task-activation framework (CRD ch. 48).
//!
//! This is the third sibling of the long-running task substrates reloaded
//! already ships (the standalone-coin family backing `task::enable_utxo` /
//! `task::enable_qtum` / the Z-coin trio, and the l2 family backing
//! `task::enable_lightning`). It exposes the
//! `init`/`status`/`user_action`/`cancel` lifecycle over the **existing**
//! one-shot platform-coin-with-tokens activation routine
//! ([`enable_platform_coin_with_tokens`]), so a platform activation (EVM today,
//! Tendermint next) can be polled to completion and cancelled in flight without
//! forking the activation logic (R48.2.3).
//!
//! The task's unit of work is the one-shot routine verbatim, run inside the
//! task. Its `init` params and success result are therefore byte-identical to
//! the one-shot call (R48.2.1 / R48.2.2); the wrapper adds only progress
//! reporting, cancellation, and the (forward-compat) awaiting-user-action hook.
//!
//! ## Target gating
//!
//! [`enable_platform_coin_with_tokens`] is `async_trait(?Send)` on `wasm32`
//! (its trait methods return non-`Send` boxed futures so EVM/Tendermint
//! activation can hold browser-bound, non-`Send` state across `await`). To wrap
//! that `?Send` one-shot routine inside an [`RpcTask`], the `rpc_task` framework
//! relaxes [`RpcTask::run`]'s async-trait boxing to `?Send` on `wasm32` (it is
//! `Send` on native); the `RpcTaskManager` then spawns the task onto
//! `common::executor::spawn`, whose `wasm32` variant accepts a non-`Send`
//! future. This module is therefore cross-platform: `task::enable_eth::*` and
//! `task::enable_tendermint::*` are routed on both native and wasm, with
//! byte-identical `init` params and success results.

use crate::context::CoinsActivationContext;
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens_for_task, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq, PlatformWithTokensActivationOps};
use crate::prelude::CurrentBlock;
use async_trait::async_trait;
use common::{log, SuccessResponse};
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest, RpcTaskUserActionError,
                           RpcTaskUserActionRequest};
use rpc_task::{RpcTask, RpcTaskError, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatusAlias,
               RpcTaskTypes};
use serde_derive::Serialize;
use serde_json::Value as Json;

pub type InitPlatformCoinWithTokensTaskManagerShared<Platform> =
    RpcTaskManagerShared<InitPlatformCoinWithTokensTask<Platform>>;
pub type InitPlatformCoinWithTokensResponse = InitRpcTaskResponse;

/// Coarse in-progress phases reported while a platform activation runs (R48.3.1).
///
/// The wrapped one-shot routine is opaque (it creates the platform coin,
/// activates the inline tokens, enumerates balances, and starts any background
/// history fetch as a single call), so these phases bracket that call rather
/// than interleaving with its internal steps. Their exact serialized spelling is
/// informative (R48.3.3).
#[derive(Clone, Serialize)]
pub enum InitPlatformCoinWithTokensInProgressStatus {
    ActivatingCoin,
    RequestingWalletBalance,
    Finishing,
    /// A hardware-wallet signing policy is waiting for the Trezor device to be
    /// connected (R48.6.2). Only reached by an interactive (Trezor) policy.
    WaitingForTrezorToConnect,
    /// The device is prompting the user to confirm the account public key /
    /// address read during activation. Only reached by an interactive policy.
    WaitingForUserToConfirmPubkey,
}

/// Per-platform-coin task registration (R48.4.1).
///
/// Implemented once per platform coin (EthCoin here; TendermintCoin next),
/// supplying only the per-coin task-manager accessor. Everything else — the
/// task struct, the lifecycle handlers, and the `RpcTask` impl — is generic over
/// the platform coin.
///
/// The `enable_platform_coin_with_task` hook is the "policy addition, not
/// framework change" seam (R48.6.2): its default builds the platform coin
/// exactly as the one-shot routine does (ignoring the task handle), so the
/// shipped non-interactive policies behave identically; an interactive
/// (hardware-wallet) policy overrides it to drive the device via the threaded
/// task handle.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait InitPlatformCoinWithTokensActivationOps: PlatformWithTokensActivationOps + Send + Sync + 'static
where
    <Self as PlatformWithTokensActivationOps>::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<<Self as PlatformWithTokensActivationOps>::ActivationError>,
{
    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitPlatformCoinWithTokensTaskManagerShared<Self>;

    /// Build the platform coin during a task-driven activation, threading the
    /// task handle so an interactive signing policy can drive its device
    /// (surfacing connect / PIN / passphrase through the task's status /
    /// user-action vocabulary). A non-interactive policy ignores the handle and
    /// builds the coin exactly as [`PlatformWithTokensActivationOps::enable_platform_coin`]
    /// does, so it never enters the awaiting state (R48.6.1 / R48.6.2).
    async fn enable_platform_coin_with_task(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: <Self as PlatformWithTokensActivationOps>::ActivationRequest,
        protocol_conf: <Self as PlatformWithTokensActivationOps>::PlatformProtocolInfo,
        priv_key: &[u8],
        task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<Self>>,
    ) -> Result<Self, MmError<<Self as PlatformWithTokensActivationOps>::ActivationError>>;
}

/// The long-running platform activation task. Its `run` calls the one-shot
/// activation routine unchanged (R48.2.3).
pub struct InitPlatformCoinWithTokensTask<Platform: PlatformWithTokensActivationOps> {
    ctx: MmArc,
    request: EnablePlatformCoinWithTokensReq<Platform::ActivationRequest>,
}

impl<Platform> RpcTaskTypes for InitPlatformCoinWithTokensTask<Platform>
where
    Platform: PlatformWithTokensActivationOps + Send + Sync + 'static,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    type Item = Platform::ActivationResult;
    type Error = EnablePlatformCoinWithTokensError;
    type InProgressStatus = InitPlatformCoinWithTokensInProgressStatus;
    // The shipped non-interactive policies (local/context, MetaMask, Tendermint)
    // complete activation without an interactive confirmation (R48.6.1), so they
    // never enter the awaiting state. The awaiting/user-action types default to
    // the hardware-wallet vocabulary (R48.6.2) so an interactive Trezor policy
    // can surface PIN / passphrase / connect states and receive the matching
    // user actions through `task::enable_<platform>::user_action`.
    type AwaitingStatus = HwRpcTaskAwaitingStatus;
    type UserAction = HwRpcTaskUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<Platform> RpcTask for InitPlatformCoinWithTokensTask<Platform>
where
    Platform: InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    fn initial_status(&self) -> Self::InProgressStatus { InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin }

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>> {
        // The interactive-capable activation routine builds the platform coin
        // through the per-platform `enable_platform_coin_with_task` hook (so a
        // hardware policy can drive the threaded device) and then runs the
        // shared token/balance/history/register tail (R48.2.3 / R48.6.2).
        let result = enable_platform_coin_with_tokens_for_task::<Platform>(self.ctx, task_handle, self.request).await?;

        task_handle
            .update_in_progress_status(InitPlatformCoinWithTokensInProgressStatus::Finishing)
            .mm_err(|e| EnablePlatformCoinWithTokensError::Internal(e.to_string()))?;
        log::debug!(
            "Platform coin activation task finished at block {}",
            result.current_block()
        );

        Ok(result)
    }
}

/// `task::enable_<platform>::init` — spawn a platform activation task and return
/// its `task_id` before activation completes (R48.1.2).
pub async fn init_platform_coin_with_tokens<Platform>(
    ctx: MmArc,
    request: EnablePlatformCoinWithTokensReq<Platform::ActivationRequest>,
) -> MmResult<InitPlatformCoinWithTokensResponse, EnablePlatformCoinWithTokensError>
where
    Platform: InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    let coins_act_ctx =
        CoinsActivationContext::from_ctx(&ctx).map_to_mm(EnablePlatformCoinWithTokensError::Internal)?;
    let task = InitPlatformCoinWithTokensTask::<Platform> { ctx, request };
    let task_manager = Platform::rpc_task_manager(&coins_act_ctx);
    let task_id = RpcTaskManager::spawn_rpc_task(task_manager, task)
        .mm_err(|e| EnablePlatformCoinWithTokensError::Internal(e.to_string()))?;
    Ok(InitPlatformCoinWithTokensResponse { task_id })
}

/// `task::enable_<platform>::status` — poll progress / terminal result (R48.1.3).
///
/// The terminal error preserves the one-shot platform-activation discriminants
/// (R48.5.1); `status` itself fails only with the task-framework discriminants
/// (`no_such_task`, internal) (R48.5.2).
pub async fn init_platform_coin_with_tokens_status<Platform>(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<RpcTaskStatusAlias<InitPlatformCoinWithTokensTask<Platform>>, RpcTaskStatusError>
where
    Platform: InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = Platform::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

/// `task::enable_<platform>::user_action` — deliver an interactive confirmation
/// to an awaiting task (R48.1.4).
///
/// Under the shipped EVM/Tendermint policies no task ever enters the
/// awaiting-user-action state (R48.6.1), so this routes for wire parity and for
/// the future hardware policy: it validates the `task_id` and returns the
/// standard task-framework response. It never panics and never fabricates a
/// confirmation (R48.6.3) — an unknown `task_id` yields `no_such_task`, and a
/// task that is not awaiting yields the standard unexpected-status error.
pub async fn init_platform_coin_with_tokens_user_action<Platform>(
    ctx: MmArc,
    req: RpcTaskUserActionRequest<<InitPlatformCoinWithTokensTask<Platform> as RpcTaskTypes>::UserAction>,
) -> MmResult<SuccessResponse, RpcTaskUserActionError>
where
    Platform: InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(RpcTaskUserActionError::Internal)?;
    let mut task_manager = Platform::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| RpcTaskUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

/// `task::enable_<platform>::cancel` — abort an in-flight activation (R48.1.5).
///
/// Cancellation removes the task from its registry before the coin is
/// registered (a cancelled `run` never reaches `lp_register_coin`/
/// `add_platform_with_tokens`), so the platform coin is not left half-registered
/// (R48.7.3).
pub async fn cancel_init_platform_coin_with_tokens<Platform>(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, RpcTaskStatusError>
where
    Platform: InitPlatformCoinWithTokensActivationOps,
    Platform::ActivationResult: serde::Serialize + Clone + Send + Sync + 'static,
    EnablePlatformCoinWithTokensError: From<Platform::ActivationError>,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = Platform::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(|e| match e {
        RpcTaskError::NoSuchTask(task_id) => RpcTaskStatusError::NoSuchTask(task_id),
        other => RpcTaskStatusError::Internal(other.to_string()),
    })?;
    Ok(SuccessResponse::new())
}
