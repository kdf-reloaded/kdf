use crate::{lp_coinfind_or_err, CoinsContext, MmCoinEnum, WithdrawError};
use crate::{TransactionDetails, WithdrawRequest};
use async_trait::async_trait;
use common::SuccessResponse;
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction, HwRpcTaskUserActionRequest};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest, RpcTaskUserActionError};
use rpc_task::RpcTaskError;
use rpc_task::{RpcTask, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatusAlias, RpcTaskTypes};

pub type WithdrawAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type WithdrawUserAction = HwRpcTaskUserAction;
pub type WithdrawStatusError = RpcTaskStatusError;
pub type WithdrawUserActionError = RpcTaskUserActionError;
pub type InitWithdrawResponse = InitRpcTaskResponse;
pub type WithdrawStatusRequest = RpcTaskStatusRequest;
pub type WithdrawUserActionRequest = HwRpcTaskUserActionRequest;
pub type WithdrawTaskManager = RpcTaskManager<WithdrawTask>;
pub type WithdrawTaskManagerShared = RpcTaskManagerShared<WithdrawTask>;
pub type WithdrawTaskHandle = RpcTaskHandle<WithdrawTask>;
pub type WithdrawRpcStatus = RpcTaskStatusAlias<WithdrawTask>;
pub type WithdrawInitResult<T> = Result<T, MmError<WithdrawError>>;

/// Compatibility wire format for `task::withdraw::status` expected by legacy clients.
///
/// Legacy clients expect a terminal status value of `"Ok"` with `details` being a flat
/// `TransactionDetails` object. Terminal failures keep the RPC call successful and expose the
/// structured withdrawal error in `details`.
#[derive(Serialize)]
#[serde(tag = "status", content = "details")]
pub enum WithdrawCompatRpcStatus {
    /// Task completed successfully; `details` is the flat `TransactionDetails` JSON object.
    Ok(TransactionDetails),
    /// Task failed; `details` is the structured `WithdrawError` JSON object.
    Error(WithdrawError),
    InProgress(WithdrawInProgressStatus),
    UserActionRequired(WithdrawAwaitingStatus),
}

#[async_trait]
pub trait CoinWithdrawInit {
    fn init_withdraw(
        ctx: MmArc,
        req: WithdrawRequest,
        rpc_task_handle: &WithdrawTaskHandle,
    ) -> WithdrawInitResult<TransactionDetails>;
}

pub async fn init_withdraw(ctx: MmArc, request: WithdrawRequest) -> WithdrawInitResult<InitWithdrawResponse> {
    let coin = lp_coinfind_or_err(&ctx, &request.coin).await.mm_err(Into::into)?;
    let task = WithdrawTask {
        ctx: ctx.clone(),
        coin,
        request,
    };
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(WithdrawError::InternalError)?;
    let task_id = WithdrawTaskManager::spawn_rpc_task(&coins_ctx.withdraw_task_manager, task).mm_err(Into::into)?;
    Ok(InitWithdrawResponse { task_id })
}

pub async fn withdraw_status(
    ctx: MmArc,
    req: WithdrawStatusRequest,
) -> Result<WithdrawCompatRpcStatus, MmError<WithdrawStatusError>> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(WithdrawStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .withdraw_task_manager
        .lock()
        .map_to_mm(|e| WithdrawStatusError::Internal(e.to_string()))?;
    let status = task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| WithdrawStatusError::NoSuchTask(req.task_id))?;

    let compat_status = match status {
        rpc_task::RpcTaskStatus::Ok(tx_details) => WithdrawCompatRpcStatus::Ok(tx_details),
        rpc_task::RpcTaskStatus::Error(e) => WithdrawCompatRpcStatus::Error(e.into_inner()),
        rpc_task::RpcTaskStatus::InProgress(in_progress) => WithdrawCompatRpcStatus::InProgress(in_progress),
        rpc_task::RpcTaskStatus::UserActionRequired(awaiting_status) => {
            WithdrawCompatRpcStatus::UserActionRequired(awaiting_status)
        },
    };

    Ok(compat_status)
}

pub async fn withdraw_cancel(
    ctx: MmArc,
    req: WithdrawStatusRequest,
) -> Result<SuccessResponse, MmError<WithdrawStatusError>> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(WithdrawStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .withdraw_task_manager
        .lock()
        .map_to_mm(|e| WithdrawStatusError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(withdraw_cancel_error)?;
    Ok(SuccessResponse::new())
}

fn withdraw_cancel_error(e: RpcTaskError) -> WithdrawStatusError { e.into() }

#[derive(Clone, Serialize)]
pub enum WithdrawInProgressStatus {
    Preparing,
    GeneratingTransaction,
    SigningTransaction,
    Finishing,
    /// The following statuses don't require the user to send `UserAction`,
    /// but they tell the user that he should confirm/decline the operation on his device.
    WaitingForTrezorToConnect,
    WaitingForUserToConfirmPubkey,
    WaitingForUserToConfirmSigning,
}

pub async fn withdraw_user_action(
    ctx: MmArc,
    req: WithdrawUserActionRequest,
) -> Result<SuccessResponse, MmError<WithdrawUserActionError>> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(WithdrawUserActionError::Internal)?;
    let mut task_manager = coins_ctx
        .withdraw_task_manager
        .lock()
        .map_to_mm(|e| WithdrawUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

#[async_trait]
pub trait InitWithdrawCoin {
    async fn init_withdraw(
        &self,
        ctx: MmArc,
        req: WithdrawRequest,
        task_handle: &WithdrawTaskHandle,
    ) -> Result<TransactionDetails, MmError<WithdrawError>>;
}

pub struct WithdrawTask {
    ctx: MmArc,
    coin: MmCoinEnum,
    request: WithdrawRequest,
}

impl RpcTaskTypes for WithdrawTask {
    type Item = TransactionDetails;
    type Error = WithdrawError;
    type InProgressStatus = WithdrawInProgressStatus;
    type AwaitingStatus = WithdrawAwaitingStatus;
    type UserAction = WithdrawUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RpcTask for WithdrawTask {
    fn initial_status(&self) -> Self::InProgressStatus { WithdrawInProgressStatus::Preparing }

    async fn run(self, task_handle: &WithdrawTaskHandle) -> Result<Self::Item, MmError<Self::Error>> {
        match self.coin {
            MmCoinEnum::UtxoCoin(ref standard_utxo) => {
                standard_utxo.init_withdraw(self.ctx, self.request, task_handle).await
            },
            MmCoinEnum::QtumCoin(ref qtum) => qtum.init_withdraw(self.ctx, self.request, task_handle).await,
            MmCoinEnum::EthCoin(ref eth) => eth.init_withdraw(self.ctx, self.request, task_handle).await,
            _ => MmError::err(WithdrawError::CoinDoesntSupportInitWithdraw {
                coin: self.coin.ticker().to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    #[test]
    fn withdraw_status_request_defaults_forget_if_finished_to_true() {
        let req: WithdrawStatusRequest = serde_json::from_value(json!({ "task_id": 7 })).unwrap();

        assert_eq!(req.task_id, 7);
        assert!(req.forget_if_finished);
    }

    #[test]
    fn terminal_withdraw_error_status_serializes_structured_details() {
        let status = WithdrawCompatRpcStatus::Error(WithdrawError::FromAddressNotFound);
        let actual = serde_json::to_value(status).unwrap();

        assert_eq!(
            actual,
            json!({
                "status": "Error",
                "details": {
                    "error_type": "FromAddressNotFound"
                }
            })
        );
    }

    #[test]
    fn withdraw_cancel_unknown_task_returns_no_such_task_error() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let req = WithdrawStatusRequest {
            task_id: 42,
            forget_if_finished: true,
        };

        let err = match block_on(withdraw_cancel(ctx, req)) {
            Ok(_) => panic!("withdraw_cancel unexpectedly succeeded"),
            Err(e) => e.into_inner(),
        };

        assert!(matches!(err, WithdrawStatusError::NoSuchTask(42)));
    }

    #[test]
    fn withdraw_cancel_terminal_task_error_is_not_collapsed_to_internal() {
        let err = withdraw_cancel_error(RpcTaskError::UnexpectedTaskStatus {
            task_id: 42,
            actual: rpc_task::TaskStatusError::Finished,
            expected: rpc_task::TaskStatusError::InProgress,
        });

        assert!(matches!(err, WithdrawStatusError::UnexpectedTaskStatus {
            task_id: 42,
            actual: rpc_task::TaskStatusError::Finished,
            expected: rpc_task::TaskStatusError::InProgress,
        }));
    }
}
