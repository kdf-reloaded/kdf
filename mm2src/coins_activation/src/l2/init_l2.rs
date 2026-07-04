/// Task-based L2 activation framework. Long-running L2 activations
/// run as background RPC tasks with progress and cancellation support.
use crate::context::CoinsActivationContext;
use crate::l2::enable_l2::L2ProtocolParams;
use crate::l2::l2_activation_errors::{CancelL2ActivationError, L2ActivationError, L2ActivationStatusError,
                                      L2ActivationUserActionError};
use crate::prelude::*;
use async_trait::async_trait;
use coins::{lp_coinfind, lp_coinfind_or_err, CoinsContext, MmCoinEnum, RegisterCoinError};
use common::SuccessResponse;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{InitRpcTaskResponse, RpcTaskStatusRequest, RpcTaskUserActionRequest};
use rpc_task::{RpcTask, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatus, RpcTaskTypes};
use serde_derive::Deserialize;
use serde_json::Value as Json;

pub type L2ActivationResponse = InitRpcTaskResponse;
pub type L2StatusRequest = RpcTaskStatusRequest;
pub type L2UserActionRequest<UA> = RpcTaskUserActionRequest<UA>;
pub type L2TaskManagerShared<L2> = RpcTaskManagerShared<L2ActivationTask<L2>>;

#[derive(Debug, Deserialize)]
pub struct L2ActivationRequest<T> {
    ticker: String,
    activation_params: T,
}

#[async_trait]
pub trait InitL2ActivationOps: Into<MmCoinEnum> + Send + Sync + 'static {
    type PlatformCoin: TryPlatformCoinFromMmCoinEnum + Clone + Send + Sync;
    type ActivationParams: Clone + Send + Sync + 'static;
    type ProtocolInfo: L2ProtocolParams + TryFromCoinProtocol + Clone + Send + Sync;
    type ValidatedParams: Clone + Send + Sync;
    type CoinConf: Clone + Send + Sync;
    type ActivationResult: serde::Serialize + Clone + Send + Sync + 'static;
    type ActivationError: From<RegisterCoinError> + SerMmErrorType + Clone + Send + Sync + 'static;
    type InProgressStatus: L2InitialStatus + serde::Serialize + Clone + Send + Sync + 'static;
    type AwaitingStatus: serde::Serialize + Clone + Send + Sync + 'static;
    type UserAction: NotMmError + Send + Sync + 'static;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &L2TaskManagerShared<Self>;

    fn coin_conf_from_json(json: Json) -> Result<Self::CoinConf, MmError<Self::ActivationError>>;

    fn validate_platform_configuration(
        platform_coin: &Self::PlatformCoin,
    ) -> Result<(), MmError<Self::ActivationError>>;

    fn validate_activation_params(
        activation_params: Self::ActivationParams,
    ) -> Result<Self::ValidatedParams, MmError<Self::ActivationError>>;

    async fn init_l2(
        ctx: &MmArc,
        platform_coin: Self::PlatformCoin,
        validated_params: Self::ValidatedParams,
        protocol_conf: Self::ProtocolInfo,
        coin_conf: Self::CoinConf,
        task_handle: &RpcTaskHandle<L2ActivationTask<Self>>,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>>;
}

pub trait L2InitialStatus {
    fn initial_status() -> Self;
}

pub struct L2ActivationTask<L2: InitL2ActivationOps> {
    ctx: MmArc,
    platform_coin: L2::PlatformCoin,
    validated_params: L2::ValidatedParams,
    protocol_conf: L2::ProtocolInfo,
    coin_conf: L2::CoinConf,
}

impl<L2: InitL2ActivationOps> RpcTaskTypes for L2ActivationTask<L2> {
    type Item = L2::ActivationResult;
    type Error = L2::ActivationError;
    type InProgressStatus = L2::InProgressStatus;
    type AwaitingStatus = L2::AwaitingStatus;
    type UserAction = L2::UserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<L2> RpcTask for L2ActivationTask<L2>
where
    L2: InitL2ActivationOps,
{
    fn initial_status(&self) -> Self::InProgressStatus { <L2::InProgressStatus as L2InitialStatus>::initial_status() }

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>> {
        let (coin, result) = L2::init_l2(
            &self.ctx,
            self.platform_coin,
            self.validated_params,
            self.protocol_conf,
            self.coin_conf,
            task_handle,
        )
        .await?;

        let c_ctx = CoinsContext::from_ctx(&self.ctx)
            .map_to_mm(RegisterCoinError::Internal)
            .map_mm_err()?;
        c_ctx.add_coin(coin.into()).await.map_err(|e| {
            let reg_err = RegisterCoinError::CoinIsInitializedAlready {
                coin: e.into_inner().ticker,
            };
            MmError::new(L2::ActivationError::from(reg_err))
        })?;

        Ok(result)
    }
}

/// Spawn a new L2 activation task; returns a task ID for polling.
pub async fn init_l2<L2>(
    ctx: MmArc,
    req: L2ActivationRequest<L2::ActivationParams>,
) -> Result<L2ActivationResponse, MmError<L2ActivationError>>
where
    L2: InitL2ActivationOps,
    L2ActivationError: From<L2::ActivationError>,
{
    let ticker = req.ticker.clone();
    if let Ok(Some(_)) = lp_coinfind(&ctx, &ticker).await {
        return MmError::err(L2ActivationError::AlreadyActivated(ticker));
    }

    let (coin_conf_json, protocol_conf): (Json, L2::ProtocolInfo) =
        coin_conf_with_protocol(&ctx, &ticker).mm_err(Into::into)?;
    let coin_conf = L2::coin_conf_from_json(coin_conf_json).mm_err(Into::into)?;

    let platform_coin = lp_coinfind_or_err(&ctx, protocol_conf.platform_coin_ticker())
        .await
        .mm_err(|_| L2ActivationError::PlatformNotActivated(ticker.clone()))?;

    let platform_coin =
        L2::PlatformCoin::try_from_mm_coin(platform_coin).or_mm_err(|| L2ActivationError::IncompatiblePlatform {
            platform_coin_ticker: protocol_conf.platform_coin_ticker().into(),
            l2_ticker: ticker.clone(),
        })?;

    L2::validate_platform_configuration(&platform_coin).mm_err(Into::into)?;
    let validated_params = L2::validate_activation_params(req.activation_params).mm_err(Into::into)?;

    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx)
        .map_to_mm(L2ActivationError::Internal)
        .map_mm_err()?;
    let task = L2ActivationTask::<L2> {
        ctx,
        platform_coin,
        validated_params,
        protocol_conf,
        coin_conf,
    };
    let task_manager = L2::rpc_task_manager(&coins_act_ctx);

    let task_id =
        RpcTaskManager::spawn_rpc_task(task_manager, task).mm_err(|e| L2ActivationError::Internal(e.to_string()))?;

    Ok(L2ActivationResponse { task_id })
}

/// Poll activation progress.
pub async fn init_l2_status<L2: InitL2ActivationOps>(
    ctx: MmArc,
    req: L2StatusRequest,
) -> MmResult<
    RpcTaskStatus<L2::ActivationResult, L2ActivationError, L2::InProgressStatus, L2::AwaitingStatus>,
    L2ActivationStatusError,
>
where
    L2ActivationError: From<L2::ActivationError>,
{
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx)
        .map_to_mm(L2ActivationStatusError::Internal)
        .map_mm_err()?;
    let mut task_manager = L2::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|poison| L2ActivationStatusError::Internal(poison.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .map(|s| s.map_err(L2ActivationError::from))
        .or_mm_err(|| L2ActivationStatusError::NoSuchTask(req.task_id))
}

/// Forward a user action (e.g. hardware wallet confirmation) to a waiting task.
pub async fn init_l2_user_action<L2: InitL2ActivationOps>(
    ctx: MmArc,
    req: L2UserActionRequest<L2::UserAction>,
) -> MmResult<SuccessResponse, L2ActivationUserActionError> {
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx)
        .map_to_mm(L2ActivationUserActionError::Internal)
        .map_mm_err()?;
    let mut task_manager = L2::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|poison| L2ActivationUserActionError::Internal(poison.to_string()))?;
    task_manager.on_user_action(req.task_id, req.user_action).map_mm_err()?;
    Ok(SuccessResponse::new())
}

/// Cancel an in-progress L2 activation, de-registering the coin if it was partly activated.
pub async fn cancel_l2_activation<L2: InitL2ActivationOps>(
    ctx: MmArc,
    req: L2StatusRequest,
) -> MmResult<SuccessResponse, CancelL2ActivationError> {
    let coins_act_ctx = CoinsActivationContext::from_ctx(&ctx)
        .map_to_mm(CancelL2ActivationError::Internal)
        .map_mm_err()?;
    let mut task_manager = L2::rpc_task_manager(&coins_act_ctx)
        .lock()
        .map_to_mm(|poison| CancelL2ActivationError::Internal(poison.to_string()))
        .map_mm_err()?;
    task_manager
        .cancel_task(req.task_id)
        .map_err(|e| MmError::new(CancelL2ActivationError::Internal(e.to_string())))?;
    Ok(SuccessResponse::new())
}
