/// Error types for task-based L2 activation.
use crate::prelude::CoinConfWithProtocolError;
use coins::CoinProtocol;
use common::{HttpStatusCode, StatusCode};
use derive_more::Display;
use rpc_task::rpc_common::{CancelRpcTaskError, RpcTaskStatusError, RpcTaskUserActionError};
use rpc_task::RpcTaskError;
use ser_error_derive::SerializeErrorType;
use serde_derive::Serialize;
use std::time::Duration;

pub type L2ActivationStatusError = RpcTaskStatusError;
pub type L2ActivationUserActionError = RpcTaskUserActionError;
pub type CancelL2ActivationError = CancelRpcTaskError;

/// The errors of `task::enable_lightning::init` and of the task status (CRD ch.41 R41.2.6, R41.2.7). The
/// variant names are the wire `error_type` values.
// Cold error path: created at most once per failed activation request, so the variant's size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum L2ActivationError {
    #[display(fmt = "Layer 2 {} is already activated", _0)]
    L2IsAlreadyActivated(String),
    #[display(fmt = "Layer 2 {} config is not found", _0)]
    L2ConfigIsNotFound(String),
    #[display(fmt = "Layer 2 {} protocol parse error: {}", ticker, error)]
    L2ProtocolParseError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Unexpected layer 2 protocol {:?} for {}", protocol, ticker)]
    UnexpectedL2Protocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    /// Carries the ticker of the layer 2 coin, not the platform's (CRD R41.2.6 row 6).
    #[display(fmt = "The platform coin of layer 2 {} is not activated", _0)]
    PlatformCoinIsNotActivated(String),
    #[display(fmt = "{} is not a valid platform for L2 {}", platform_coin_ticker, l2_ticker)]
    UnsupportedPlatformCoin {
        platform_coin_ticker: String,
        l2_ticker: String,
    },
    #[display(fmt = "Invalid platform configuration for {}: {}", platform_coin_ticker, err)]
    InvalidPlatformConfiguration {
        platform_coin_ticker: String,
        err: String,
    },
    #[display(fmt = "L2 configuration parsing failed: {}", _0)]
    L2ConfigParseError(String),
    #[display(fmt = "Activation task timed out after {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    Transport(String),
    Internal(String),
}

impl From<CoinConfWithProtocolError> for L2ActivationError {
    fn from(err: CoinConfWithProtocolError) -> Self {
        match err {
            CoinConfWithProtocolError::ConfigIsNotFound(ticker) => L2ActivationError::L2ConfigIsNotFound(ticker),
            CoinConfWithProtocolError::CoinProtocolParseError { ticker, err } => {
                L2ActivationError::L2ProtocolParseError {
                    ticker,
                    error: err.to_string(),
                }
            },
            CoinConfWithProtocolError::UnexpectedProtocol { ticker, protocol } => {
                L2ActivationError::UnexpectedL2Protocol { ticker, protocol }
            },
        }
    }
}

impl From<RpcTaskError> for L2ActivationError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::Timeout(duration) => L2ActivationError::TaskTimedOut { duration },
            other => L2ActivationError::Internal(other.to_string()),
        }
    }
}

impl HttpStatusCode for L2ActivationError {
    fn status_code(&self) -> StatusCode {
        match self {
            L2ActivationError::L2IsAlreadyActivated(_)
            | L2ActivationError::PlatformCoinIsNotActivated(_)
            | L2ActivationError::L2ConfigIsNotFound(_)
            | L2ActivationError::UnexpectedL2Protocol { .. } => StatusCode::BAD_REQUEST,
            L2ActivationError::TaskTimedOut { .. } => StatusCode::REQUEST_TIMEOUT,
            L2ActivationError::L2ProtocolParseError { .. }
            | L2ActivationError::UnsupportedPlatformCoin { .. }
            | L2ActivationError::InvalidPlatformConfiguration { .. }
            | L2ActivationError::L2ConfigParseError(_)
            | L2ActivationError::Transport(_)
            | L2ActivationError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
