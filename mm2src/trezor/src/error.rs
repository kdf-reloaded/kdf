use crate::proto::messages::MessageType;
use crate::proto::messages_common::{failure::FailureType, Failure};
use crate::user_interaction::TrezorUserInteraction;
use derive_more::Display;
use mm2_err_handle::prelude::*;
use prost::{DecodeError, EncodeError};

pub type TrezorResult<T> = Result<T, MmError<TrezorError>>;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use hw_common::transport::UsbError;
#[cfg(target_arch = "wasm32")]
use hw_common::transport::WebUsbError;

#[derive(Debug, Display)]
pub enum TrezorError {
    #[display(fmt = "'{}' transport is not available on this platform", transport)]
    TransportNotSupported {
        transport: String,
    },
    /// Please note it's not the same as `PermissionDenied`.
    /// This error may appear in a browser when the user didn't allow the app to get the list of devices.
    ErrorRequestingAccessPermission(String),
    /// TODO put a device info
    DeviceDisconnected,
    /// The error depends on transport implementation.
    UnderlyingError(String),
    ProtocolError(String),
    #[display(fmt = "Received unexpected message type: {:?}", _0)]
    UnexpectedMessageType(MessageType),
    Failure(OperationFailure),
    #[display(fmt = "Unexpected interaction request: {:?}", _0)]
    UnexpectedInteractionRequest(TrezorUserInteraction),
    Internal(String),
}

/// The hardware RPC error names of the public error-types catalogue. They are the string payload of the
/// `HwError` discriminant of the hardware-wallet activation errors (CRD ch. 55 R55.2.17).
#[derive(Clone, Copy, Debug, Display, PartialEq, Eq, Serialize)]
pub enum HwRpcError {
    NoTrezorDeviceAvailable,
    FoundMultipleDevices,
    FoundUnexpectedDevice,
    InvalidPin,
    UnexpectedMessage,
    ButtonExpected,
    DataError,
    PinExpected,
    InvalidSignature,
    ProcessError,
    NotEnoughFunds,
    NotInitialized,
    WipeCodeMismatch,
    InvalidSession,
    FirmwareError,
    FailureMessageNotFound,
    UserCancelled,
    PongMessageMismatch,
}

#[derive(Clone, Debug, Display)]
pub enum OperationFailure {
    InvalidPin,
    /// A failure the device reported with a known failure code. The payload is never
    /// [`HwRpcError::InvalidPin`] (that is [`OperationFailure::InvalidPin`]).
    #[display(fmt = "Operation failed on the device: {}", _0)]
    Device(HwRpcError),
    /// TODO expand it to other types.
    #[display(fmt = "Operation failed due to unknown reason: {}", _0)]
    Other(String),
}

impl From<Failure> for OperationFailure {
    fn from(failure: Failure) -> Self {
        let code = match failure.code {
            Some(code) => code,
            None => return OperationFailure::Device(HwRpcError::FailureMessageNotFound),
        };
        match FailureType::try_from(code).ok() {
            Some(FailureType::FailurePinInvalid) | Some(FailureType::FailurePinMismatch) => {
                OperationFailure::InvalidPin
            },
            Some(FailureType::FailureUnexpectedMessage) => OperationFailure::Device(HwRpcError::UnexpectedMessage),
            Some(FailureType::FailureButtonExpected) => OperationFailure::Device(HwRpcError::ButtonExpected),
            Some(FailureType::FailureDataError) => OperationFailure::Device(HwRpcError::DataError),
            Some(FailureType::FailureActionCancelled) | Some(FailureType::FailurePinCancelled) => {
                OperationFailure::Device(HwRpcError::UserCancelled)
            },
            Some(FailureType::FailurePinExpected) => OperationFailure::Device(HwRpcError::PinExpected),
            Some(FailureType::FailureInvalidSignature) => OperationFailure::Device(HwRpcError::InvalidSignature),
            Some(FailureType::FailureProcessError) => OperationFailure::Device(HwRpcError::ProcessError),
            Some(FailureType::FailureNotEnoughFunds) => OperationFailure::Device(HwRpcError::NotEnoughFunds),
            Some(FailureType::FailureNotInitialized) => OperationFailure::Device(HwRpcError::NotInitialized),
            Some(FailureType::FailureWipeCodeMismatch) => OperationFailure::Device(HwRpcError::WipeCodeMismatch),
            Some(FailureType::FailureInvalidSession) => OperationFailure::Device(HwRpcError::InvalidSession),
            Some(FailureType::FailureFirmwareError) => OperationFailure::Device(HwRpcError::FirmwareError),
            None => OperationFailure::Other(format!("{:?}", failure)),
        }
    }
}

impl From<OperationFailure> for TrezorError {
    fn from(failure: OperationFailure) -> Self { TrezorError::Failure(failure) }
}

impl From<DecodeError> for TrezorError {
    fn from(e: DecodeError) -> Self { TrezorError::ProtocolError(e.to_string()) }
}

impl From<EncodeError> for TrezorError {
    fn from(e: EncodeError) -> Self { TrezorError::Internal(e.to_string()) }
}

#[cfg(target_arch = "wasm32")]
impl From<WebUsbError> for TrezorError {
    fn from(e: WebUsbError) -> Self {
        match e {
            WebUsbError::NotSupported => TrezorError::TransportNotSupported {
                transport: "WebUSB".to_owned(),
            },
            WebUsbError::ErrorRequestingDevice(e) => TrezorError::ErrorRequestingAccessPermission(e),
            WebUsbError::Internal(e) => TrezorError::Internal(e),
            e => TrezorError::UnderlyingError(e.to_string()),
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
impl From<UsbError> for TrezorError {
    fn from(e: UsbError) -> Self {
        match e {
            UsbError::DeviceDisconnected => TrezorError::DeviceDisconnected,
            UsbError::Internal(e) => TrezorError::Internal(e),
            e => TrezorError::UnderlyingError(e.to_string()),
        }
    }
}
