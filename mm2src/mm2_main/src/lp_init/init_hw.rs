use crate::mm2::lp_native_dex::init_context::MmInitContext;
use async_trait::async_trait;
use common::{HttpStatusCode, SuccessResponse};
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, HwRpcTaskUserAction, HwRpcTaskUserActionRequest,
                          TrezorRpcTaskConnectProcessor};
use crypto::{CryptoCtx, CryptoCtxError, CryptoInitError, HwCtxInitError, HwError, HwRpcError, HwWalletType};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use primitives::hash::H160;
use rpc_task::rpc_common::{CancelRpcTaskError, InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest,
                           RpcTaskUserActionError};
use rpc_task::{CancelCleanup, RpcTask, RpcTaskError, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared,
               RpcTaskStatus, RpcTaskTypes};
use serde::{Deserialize, Deserializer};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TREZOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
const TREZOR_PIN_TIMEOUT: Duration = Duration::from_secs(600);

pub type InitHwAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type InitHwUserAction = HwRpcTaskUserAction;

pub type InitHwTaskManagerShared = RpcTaskManagerShared<InitHwTask>;
pub type InitHwStatus = RpcTaskStatus<InitHwSuccess, InitHwError, InitHwInProgressStatus, InitHwAwaitingStatus>;
type InitHwTaskHandle = RpcTaskHandle<InitHwTask>;

/// The description of the initialised hardware wallet (CRD ch. 55 R55.11.5).
///
/// The status page of the public docs shows these fields wrapped in a `result` object inside `details`. Its field
/// table and the reference release place them directly in `details`, and so does Reloaded.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InitHwSuccess {
    #[serde(rename = "type")]
    pub hw_wallet_type: &'static str,
    pub model: Option<String>,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    /// The public-key hash of the device: 40 hexadecimal characters.
    pub device_pubkey: String,
}

/// The error set of `task::init_trezor` (CRD ch. 55 R55.11.6).
#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum InitHwError {
    #[display(fmt = "Hardware Wallet context is initializing already")]
    HwContextInitializingAlready,
    #[display(fmt = "Hardware wallet error: {}", _0)]
    HwError(HwRpcError),
    #[display(fmt = "Unexpected user action: expected '{}'", expected)]
    UnexpectedUserAction { expected: String },
    #[display(fmt = "RPC timed out {:?}", _0)]
    Timeout(Duration),
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

impl From<CryptoInitError> for InitHwError {
    fn from(e: CryptoInitError) -> Self { InitHwError::Internal(e.to_string()) }
}

impl From<CryptoCtxError> for InitHwError {
    fn from(e: CryptoCtxError) -> Self { InitHwError::Internal(e.to_string()) }
}

impl From<HwCtxInitError<RpcTaskError>> for InitHwError {
    fn from(e: HwCtxInitError<RpcTaskError>) -> Self {
        match e {
            HwCtxInitError::InitializingAlready => InitHwError::HwContextInitializingAlready,
            HwCtxInitError::HwError(hw_error) => InitHwError::from(hw_error),
            HwCtxInitError::ProcessorError(rpc) => InitHwError::from(rpc),
        }
    }
}

impl From<HwError> for InitHwError {
    fn from(e: HwError) -> Self {
        if let HwError::ConnectionTimedOut { timeout } = e {
            return InitHwError::Timeout(timeout);
        }
        match e.hw_rpc_error() {
            Some(hw_error) => InitHwError::HwError(hw_error),
            None => InitHwError::Internal(e.to_string()),
        }
    }
}

impl From<RpcTaskError> for InitHwError {
    fn from(e: RpcTaskError) -> Self {
        let error = e.to_string();
        match e {
            RpcTaskError::Canceled => InitHwError::Internal("Canceled".to_owned()),
            RpcTaskError::Timeout(timeout) => InitHwError::Timeout(timeout),
            RpcTaskError::UnexpectedUserAction { expected } => InitHwError::UnexpectedUserAction {
                expected: expected.to_owned(),
            },
            RpcTaskError::NoSuchTask(_) | RpcTaskError::UnexpectedTaskStatus { .. } => InitHwError::Internal(error),
            RpcTaskError::Internal(internal) => InitHwError::Internal(internal),
        }
    }
}

impl HttpStatusCode for InitHwError {
    fn status_code(&self) -> StatusCode {
        match self {
            InitHwError::HwContextInitializingAlready | InitHwError::UnexpectedUserAction { .. } => {
                StatusCode::BAD_REQUEST
            },
            InitHwError::HwError(_) => StatusCode::GONE,
            InitHwError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
            InitHwError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The in-progress vocabulary of `task::init_trezor` (CRD ch. 55 R55.11.4).
#[derive(Clone, Deserialize, Serialize)]
pub enum InitHwInProgressStatus {
    Initializing,
    WaitingForTrezorToConnect,
    FollowHwDeviceInstructions,
}

pub struct InitHwTask {
    ctx: MmArc,
    hw_wallet_type: HwWalletType,
    /// The device the caller requires, if any.
    expected_device: Option<H160>,
    /// Set when the task is about to take over the context initialisation.
    started: Arc<AtomicBool>,
    /// Set when another task owned the context initialisation, so a cancellation must not touch the context.
    lost_to_another_task: Arc<AtomicBool>,
}

impl RpcTaskTypes for InitHwTask {
    type Item = InitHwSuccess;
    type Error = InitHwError;
    type InProgressStatus = InitHwInProgressStatus;
    type AwaitingStatus = InitHwAwaitingStatus;
    type UserAction = InitHwUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RpcTask for InitHwTask {
    fn initial_status(&self) -> Self::InProgressStatus { InitHwInProgressStatus::Initializing }

    /// A cancelled initialisation returns the hardware-wallet context to the not-initialised state
    /// (CRD ch. 55 R55.2.14).
    fn cancel_cleanup(&self) -> Option<CancelCleanup> {
        let ctx = self.ctx.clone();
        let started = self.started.clone();
        let lost_to_another_task = self.lost_to_another_task.clone();
        Some(Box::new(move || {
            Box::pin(async move {
                if started.load(Ordering::SeqCst) && !lost_to_another_task.load(Ordering::SeqCst) {
                    if let Ok(crypto_ctx) = CryptoCtx::from_ctx(&ctx) {
                        crypto_ctx.reset_hw_ctx();
                    }
                }
            })
        }))
    }

    async fn run(self, task_handle: &InitHwTaskHandle) -> Result<Self::Item, MmError<Self::Error>> {
        let crypto_ctx = CryptoCtx::from_ctx(&self.ctx).mm_err(Into::into)?;

        match self.hw_wallet_type {
            HwWalletType::Trezor => {
                let trezor_connect_processor = TrezorRpcTaskConnectProcessor::new(task_handle, HwConnectStatuses {
                    on_connect: InitHwInProgressStatus::WaitingForTrezorToConnect,
                    on_connected: InitHwInProgressStatus::Initializing,
                    on_connection_failed: InitHwInProgressStatus::Initializing,
                    on_button_request: InitHwInProgressStatus::FollowHwDeviceInstructions,
                    on_pin_request: InitHwAwaitingStatus::EnterTrezorPin,
                    on_passphrase_request: InitHwAwaitingStatus::EnterTrezorPassphrase,
                    on_ready: InitHwInProgressStatus::Initializing,
                })
                .with_connect_timeout(TREZOR_CONNECT_TIMEOUT)
                .with_pin_timeout(TREZOR_PIN_TIMEOUT);

                // The context initialisation starts synchronously within the first poll of the call below,
                // so a cancellation observes either "not started" or "started".
                self.started.store(true, Ordering::SeqCst);
                let hw_ctx = match crypto_ctx.init_hw_ctx_with_trezor(&trezor_connect_processor).await {
                    Ok(hw_ctx) => hw_ctx,
                    Err(e) => {
                        if matches!(e.get_inner(), HwCtxInitError::InitializingAlready) {
                            self.lost_to_another_task.store(true, Ordering::SeqCst);
                        }
                        return Err(e.map(Into::into));
                    },
                };

                if let Some(expected) = self.expected_device {
                    if hw_ctx.rmd160() != expected {
                        crypto_ctx.reset_hw_ctx();
                        return MmError::err(InitHwError::HwError(HwRpcError::FoundUnexpectedDevice));
                    }
                }
                let device_info = hw_ctx.device_info();
                Ok(InitHwSuccess {
                    hw_wallet_type: "Trezor",
                    model: device_info.and_then(|info| info.model.clone()),
                    device_name: device_info.and_then(|info| info.device_name.clone()),
                    device_id: device_info.and_then(|info| info.device_id.clone()),
                    device_pubkey: hw_ctx.rmd160().to_string(),
                })
            },
        }
    }
}

/// A 20-byte public-key hash written as 40 hexadecimal characters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevicePubkey(H160);

impl<'de> Deserialize<'de> for DevicePubkey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        H160::from_str(&hex)
            .map(DevicePubkey)
            .map_err(|e| serde::de::Error::custom(format!("'device_pubkey' must be 40 hexadecimal characters: {}", e)))
    }
}

/// The optional parameters of `task::init_trezor::init` (CRD ch. 55 R55.11.2).
#[derive(Default, Deserialize)]
pub struct InitTrezorParams {
    #[serde(default)]
    pub device_pubkey: Option<DevicePubkey>,
    /// The event-stream client the task status changes would be pushed to (CRD ch. 55 R55.2.2).
    /// It is accepted and ignored: streaming task events is deferred (D55.1).
    #[serde(default)]
    #[allow(dead_code)]
    pub client_id: u64,
}

/// `params` may be absent, `null` or an object.
pub type InitTrezorRequest = Option<InitTrezorParams>;

pub async fn init_trezor(ctx: MmArc, req: InitTrezorRequest) -> MmResult<InitRpcTaskResponse, InitHwError> {
    let init_ctx = MmInitContext::from_ctx(&ctx).map_to_mm(InitHwError::Internal)?;
    let task = InitHwTask {
        ctx,
        hw_wallet_type: HwWalletType::Trezor,
        expected_device: req.and_then(|params| params.device_pubkey).map(|pubkey| pubkey.0),
        started: Arc::new(AtomicBool::new(false)),
        lost_to_another_task: Arc::new(AtomicBool::new(false)),
    };
    let task_id = RpcTaskManager::spawn_rpc_task(&init_ctx.init_hw_task_manager, task)
        .mm_err(|e| InitHwError::Internal(e.to_string()))?;
    Ok(InitRpcTaskResponse { task_id })
}

pub async fn init_trezor_status(ctx: MmArc, req: RpcTaskStatusRequest) -> MmResult<InitHwStatus, RpcTaskStatusError> {
    let coins_ctx = MmInitContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .init_hw_task_manager
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

pub async fn init_trezor_user_action(
    ctx: MmArc,
    req: HwRpcTaskUserActionRequest,
) -> MmResult<SuccessResponse, RpcTaskUserActionError> {
    let coins_ctx = MmInitContext::from_ctx(&ctx).map_to_mm(RpcTaskUserActionError::Internal)?;
    let mut task_manager = coins_ctx
        .init_hw_task_manager
        .lock()
        .map_to_mm(|e| RpcTaskUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

/// Cancels the hardware-wallet initialisation. The context returns to the not-initialised state once the
/// cancelled task has stopped (CRD ch. 55 R55.11.7).
pub async fn init_trezor_cancel(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, CancelRpcTaskError> {
    let init_ctx = MmInitContext::from_ctx(&ctx).map_to_mm(CancelRpcTaskError::Internal)?;
    let mut task_manager = init_ctx
        .init_hw_task_manager
        .lock()
        .map_to_mm(|e| CancelRpcTaskError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use primitives::hash::H264;
    use serde_json::json;

    const DEVICE_PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn wire(error: InitHwError) -> (serde_json::Value, StatusCode) {
        (serde_json::to_value(&error).expect("serializable"), error.status_code())
    }

    // T55.8: each discriminant of R55.11.6 serialises with its bound `error_data` shape and HTTP status.
    #[test]
    fn error_set_serializes_with_the_bound_shapes_and_statuses() {
        let (wire_value, status) = wire(InitHwError::HwContextInitializingAlready);
        assert_eq!(wire_value, json!({ "error_type": "HwContextInitializingAlready" }));
        assert!(wire_value.get("error_data").is_none());
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (wire_value, status) = wire(InitHwError::HwError(HwRpcError::NoTrezorDeviceAvailable));
        assert_eq!(
            wire_value,
            json!({ "error_type": "HwError", "error_data": "NoTrezorDeviceAvailable" })
        );
        assert_eq!(status, StatusCode::GONE);

        let (wire_value, status) = wire(InitHwError::UnexpectedUserAction {
            expected: "TrezorPin".to_owned(),
        });
        assert_eq!(
            wire_value,
            json!({ "error_type": "UnexpectedUserAction", "error_data": { "expected": "TrezorPin" } })
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (wire_value, status) = wire(InitHwError::Timeout(Duration::from_secs(300)));
        assert_eq!(
            wire_value,
            json!({ "error_type": "Timeout", "error_data": { "secs": 300, "nanos": 0 } })
        );
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);

        let (wire_value, status) = wire(InitHwError::Internal("boom".to_owned()));
        assert_eq!(wire_value, json!({ "error_type": "Internal", "error_data": "boom" }));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // R55.2.17: the whole hardware vocabulary is a string and answers HTTP 410.
    #[test]
    fn hw_error_vocabulary_is_a_string() {
        for name in [
            HwRpcError::NoTrezorDeviceAvailable,
            HwRpcError::FoundMultipleDevices,
            HwRpcError::FoundUnexpectedDevice,
            HwRpcError::InvalidPin,
            HwRpcError::UnexpectedMessage,
            HwRpcError::ButtonExpected,
            HwRpcError::DataError,
            HwRpcError::PinExpected,
            HwRpcError::InvalidSignature,
            HwRpcError::ProcessError,
            HwRpcError::NotEnoughFunds,
            HwRpcError::NotInitialized,
            HwRpcError::WipeCodeMismatch,
            HwRpcError::InvalidSession,
            HwRpcError::FirmwareError,
            HwRpcError::FailureMessageNotFound,
            HwRpcError::UserCancelled,
            HwRpcError::PongMessageMismatch,
        ] {
            let (wire_value, status) = wire(InitHwError::HwError(name));
            assert_eq!(wire_value["error_data"], json!(name.to_string()));
            assert_eq!(status, StatusCode::GONE);
        }
    }

    // T55.4: a status-level error is placed directly in `details`; a payload-less one omits `error_data`.
    #[test]
    fn status_level_error_is_placed_directly_in_details() {
        let status: InitHwStatus = RpcTaskStatus::Error(MmError::new(InitHwError::HwContextInitializingAlready));
        let wire_value = serde_json::to_value(&status).unwrap();
        assert_eq!(wire_value["status"], json!("Error"));
        let details = &wire_value["details"];
        assert_eq!(details["error_type"], json!("HwContextInitializingAlready"));
        for key in ["error", "error_path", "error_trace"] {
            assert!(details[key].is_string(), "`{}` must be a string", key);
        }
        assert!(details.get("error_data").is_none());
        assert!(details.get("result").is_none());

        let status: InitHwStatus = RpcTaskStatus::Error(MmError::new(InitHwError::Timeout(Duration::from_secs(600))));
        let wire_value = serde_json::to_value(&status).unwrap();
        assert_eq!(wire_value["details"]["error_data"], json!({ "secs": 600, "nanos": 0 }));
    }

    // T55.8: the success details are the flat device object of R55.11.5.
    #[test]
    fn success_details_are_the_flat_device_object() {
        let status: InitHwStatus = RpcTaskStatus::Ok(InitHwSuccess {
            hw_wallet_type: "Trezor",
            model: Some("T".to_owned()),
            device_name: None,
            device_id: Some("ABC".to_owned()),
            device_pubkey: "11".repeat(20),
        });
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "status": "Ok",
                "details": {
                    "type": "Trezor",
                    "model": "T",
                    "device_name": null,
                    "device_id": "ABC",
                    "device_pubkey": "1111111111111111111111111111111111111111",
                },
            })
        );
    }

    // R55.11.4 / R55.2.19
    #[test]
    fn progress_and_awaiting_values_are_the_bound_vocabulary() {
        for (value, expected) in [
            (InitHwInProgressStatus::Initializing, "Initializing"),
            (
                InitHwInProgressStatus::WaitingForTrezorToConnect,
                "WaitingForTrezorToConnect",
            ),
            (
                InitHwInProgressStatus::FollowHwDeviceInstructions,
                "FollowHwDeviceInstructions",
            ),
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(expected));
        }
        assert_eq!(
            serde_json::to_value(InitHwAwaitingStatus::EnterTrezorPin).unwrap(),
            json!("EnterTrezorPin")
        );
        assert_eq!(
            serde_json::to_value(InitHwAwaitingStatus::EnterTrezorPassphrase).unwrap(),
            json!("EnterTrezorPassphrase")
        );
    }

    // T55.7: every empty form of the request is accepted, and so are `device_pubkey` and `client_id`.
    #[test]
    fn request_accepts_the_bound_forms() {
        let absent: InitTrezorRequest = serde_json::from_value(serde_json::Value::Null).unwrap();
        assert!(absent.is_none());
        let empty: InitTrezorRequest = serde_json::from_value(json!({})).unwrap();
        assert!(empty.expect("an empty object is a request").device_pubkey.is_none());

        let pubkey = "ab".repeat(20);
        let with_pubkey: InitTrezorRequest = serde_json::from_value(json!({ "device_pubkey": pubkey })).unwrap();
        let expected = H160::from_str(&pubkey).unwrap();
        assert_eq!(with_pubkey.unwrap().device_pubkey.map(|p| p.0), Some(expected));

        let with_client: InitTrezorRequest = serde_json::from_value(json!({ "client_id": 5 })).unwrap();
        assert_eq!(with_client.unwrap().client_id, 5);
    }

    #[test]
    fn request_rejects_a_malformed_device_pubkey_and_a_negative_client_id() {
        for bad in ["", "zz", "abcd", &"ab".repeat(21)] {
            let result: Result<InitTrezorRequest, _> = serde_json::from_value(json!({ "device_pubkey": bad }));
            assert!(result.is_err(), "'{}' must be rejected", bad);
        }
        let negative: Result<InitTrezorRequest, _> = serde_json::from_value(json!({ "client_id": -1 }));
        assert!(negative.is_err());
    }

    #[test]
    fn failures_map_onto_the_error_set() {
        assert!(matches!(
            InitHwError::from(HwCtxInitError::<RpcTaskError>::InitializingAlready),
            InitHwError::HwContextInitializingAlready
        ));
        assert!(matches!(
            InitHwError::from(HwError::NoTrezorDeviceAvailable),
            InitHwError::HwError(HwRpcError::NoTrezorDeviceAvailable)
        ));
        assert!(matches!(
            InitHwError::from(HwError::CannotChooseDevice { count: 2 }),
            InitHwError::HwError(HwRpcError::FoundMultipleDevices)
        ));
        assert!(matches!(
            InitHwError::from(HwError::DeviceFailure(HwRpcError::UserCancelled)),
            InitHwError::HwError(HwRpcError::UserCancelled)
        ));
        let timeout = Duration::from_secs(300);
        assert!(matches!(
            InitHwError::from(HwError::ConnectionTimedOut { timeout }),
            InitHwError::Timeout(t) if t == timeout
        ));
        assert!(matches!(
            InitHwError::from(RpcTaskError::Timeout(timeout)),
            InitHwError::Timeout(t) if t == timeout
        ));
        assert!(matches!(
            InitHwError::from(RpcTaskError::UnexpectedUserAction { expected: "TrezorPin" }),
            InitHwError::UnexpectedUserAction { ref expected } if expected == "TrezorPin"
        ));
        // The task stopped while it waited for a user action.
        assert!(matches!(
            InitHwError::from(RpcTaskError::Canceled),
            InitHwError::Internal(_)
        ));
        // A failure with no name in the vocabulary is an internal failure.
        assert!(matches!(
            InitHwError::from(HwError::UnderlyingError("usb".to_owned())),
            InitHwError::Internal(_)
        ));
    }

    fn task_for_cleanup(ctx: &MmArc, started: bool, lost_to_another_task: bool) -> InitHwTask {
        InitHwTask {
            ctx: ctx.clone(),
            hw_wallet_type: HwWalletType::Trezor,
            expected_device: None,
            started: Arc::new(AtomicBool::new(started)),
            lost_to_another_task: Arc::new(AtomicBool::new(lost_to_another_task)),
        }
    }

    fn ctx_with_initialized_trezor() -> MmArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "init trezor test passphrase").unwrap();
        crypto_ctx.init_trezor_ctx_for_tests(H264::from_str(DEVICE_PUBKEY).unwrap(), None);
        ctx
    }

    // T55.9: a cancelled initialisation leaves the hardware-wallet context not-initialised.
    #[test]
    fn cancel_cleanup_resets_the_context_of_a_task_that_owned_the_initialisation() {
        let ctx = ctx_with_initialized_trezor();
        let task = task_for_cleanup(&ctx, true, false);
        assert!(CryptoCtx::from_ctx(&ctx).unwrap().hw_ctx().is_some());

        let cleanup = task.cancel_cleanup().expect("the task has a cancel cleanup");
        block_on(cleanup());

        assert!(CryptoCtx::from_ctx(&ctx).unwrap().hw_ctx().is_none());
    }

    #[test]
    fn cancel_cleanup_leaves_a_context_the_task_never_owned_alone() {
        for (started, lost_to_another_task) in [(false, false), (true, true)] {
            let ctx = ctx_with_initialized_trezor();
            let task = task_for_cleanup(&ctx, started, lost_to_another_task);

            let cleanup = task.cancel_cleanup().expect("the task has a cancel cleanup");
            block_on(cleanup());

            assert!(CryptoCtx::from_ctx(&ctx).unwrap().hw_ctx().is_some());
        }
    }

    // T55.2: `cancel` of an unknown id answers `NoSuchTask` (integer, HTTP 404).
    #[test]
    fn cancel_of_an_unknown_task_is_no_such_task() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let req = RpcTaskStatusRequest {
            task_id: 4242,
            forget_if_finished: true,
        };
        let error = match block_on(init_trezor_cancel(ctx, req)) {
            Ok(_) => panic!("the cancel must fail"),
            Err(error) => error.into_inner(),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "error_type": "NoSuchTask", "error_data": 4242 })
        );
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
    }
}
