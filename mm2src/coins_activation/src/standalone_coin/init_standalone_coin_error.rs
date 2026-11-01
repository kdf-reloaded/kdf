use crate::prelude::CoinConfWithProtocolError;
use coins::{CoinProtocol, RegisterCoinError};
use common::{HttpStatusCode, StatusCode};
use crypto::HwRpcError;
use derive_more::Display;
use rpc_task::rpc_common::{RpcTaskStatusError, RpcTaskUserActionError};
use rpc_task::{RpcTaskError, TaskId};
use ser_error_derive::SerializeErrorType;
use serde_derive::Serialize;
use std::time::Duration;

pub type InitStandaloneCoinStatusError = RpcTaskStatusError;
pub type InitStandaloneCoinUserActionError = RpcTaskUserActionError;

#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
// `UnexpectedCoinProtocol` carries a `CoinProtocol` by value for diagnostics on
// a cold error path. Boxing it (clippy's suggestion) would force `Box::new` at
// every construction site in this compatibility-sensitive activation code for a
// negligible size win on a rarely built RPC error enum. The per-item allow is
// the established precedent for this lint across coins_activation.
#[allow(clippy::large_enum_variant)]
pub enum InitStandaloneCoinError {
    NoSuchTask(TaskId),
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CoinIsAlreadyActivated {
        ticker: String,
    },
    #[display(fmt = "Coin {} config is not found", _0)]
    CoinConfigIsNotFound(String),
    #[display(fmt = "Coin {} protocol parsing failed: {}", ticker, error)]
    CoinProtocolParseError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Unexpected platform protocol {:?} for {}", protocol, ticker)]
    UnexpectedCoinProtocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    #[display(fmt = "Error on platform coin {} creation: {}", ticker, error)]
    CoinCreationError {
        ticker: String,
        error: String,
    },
    /// A hardware-wallet failure classified into the public hardware RPC error names (CRD ch. 55 R55.2.17).
    #[display(fmt = "Hardware wallet error: {}", _0)]
    HwError(HwRpcError),
    Transport(String),
    Internal(String),
}

impl From<CoinConfWithProtocolError> for InitStandaloneCoinError {
    fn from(e: CoinConfWithProtocolError) -> Self {
        match e {
            CoinConfWithProtocolError::ConfigIsNotFound(error) => InitStandaloneCoinError::CoinConfigIsNotFound(error),
            CoinConfWithProtocolError::CoinProtocolParseError { ticker, err } => {
                InitStandaloneCoinError::CoinProtocolParseError {
                    ticker,
                    error: err.to_string(),
                }
            },
            CoinConfWithProtocolError::UnexpectedProtocol { ticker, protocol } => {
                InitStandaloneCoinError::UnexpectedCoinProtocol { ticker, protocol }
            },
        }
    }
}

impl From<RegisterCoinError> for InitStandaloneCoinError {
    fn from(e: RegisterCoinError) -> Self {
        match e {
            RegisterCoinError::CoinIsInitializedAlready { coin } => {
                InitStandaloneCoinError::CoinIsAlreadyActivated { ticker: coin }
            },
            RegisterCoinError::Internal(internal) => InitStandaloneCoinError::Internal(internal),
        }
    }
}

impl From<RpcTaskError> for InitStandaloneCoinError {
    fn from(e: RpcTaskError) -> Self {
        match e {
            RpcTaskError::NoSuchTask(task_id) => InitStandaloneCoinError::NoSuchTask(task_id),
            RpcTaskError::Timeout(duration) => InitStandaloneCoinError::TaskTimedOut { duration },
            rpc_internal => InitStandaloneCoinError::Internal(rpc_internal.to_string()),
        }
    }
}

impl HttpStatusCode for InitStandaloneCoinError {
    fn status_code(&self) -> StatusCode {
        match self {
            InitStandaloneCoinError::NoSuchTask(_)
            | InitStandaloneCoinError::CoinIsAlreadyActivated { .. }
            | InitStandaloneCoinError::CoinConfigIsNotFound { .. }
            | InitStandaloneCoinError::CoinProtocolParseError { .. }
            | InitStandaloneCoinError::UnexpectedCoinProtocol { .. }
            | InitStandaloneCoinError::CoinCreationError { .. } => StatusCode::BAD_REQUEST,
            InitStandaloneCoinError::HwError(_) => StatusCode::GONE,
            InitStandaloneCoinError::TaskTimedOut { .. } => StatusCode::REQUEST_TIMEOUT,
            InitStandaloneCoinError::Transport(_) | InitStandaloneCoinError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}
