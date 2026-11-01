use super::{RpcTaskError, TaskId, TaskStatusError};
use common::{true_f, HttpStatusCode, StatusCode};
use derive_more::Display;

/// In most cases, the RPC task status request may fail with either [`RpcTaskStatusError::NoSuchTask`] or [`RpcTaskStatusError::Internal`].
/// Please do not add new error variants unless they are used in most cases.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum RpcTaskStatusError {
    NoSuchTask(TaskId),
    #[display(
        fmt = "RPC '{}' task is in unexpected status. Actual: '{}', expected: '{}'",
        task_id,
        actual,
        expected
    )]
    UnexpectedTaskStatus {
        task_id: TaskId,
        actual: TaskStatusError,
        expected: TaskStatusError,
    },
    Internal(String),
}

impl HttpStatusCode for RpcTaskStatusError {
    fn status_code(&self) -> StatusCode {
        match self {
            RpcTaskStatusError::NoSuchTask(_) | RpcTaskStatusError::UnexpectedTaskStatus { .. } => {
                StatusCode::BAD_REQUEST
            },
            RpcTaskStatusError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<RpcTaskError> for RpcTaskStatusError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::NoSuchTask(task_id) => RpcTaskStatusError::NoSuchTask(task_id),
            RpcTaskError::UnexpectedTaskStatus {
                task_id,
                actual,
                expected,
            } => RpcTaskStatusError::UnexpectedTaskStatus {
                task_id,
                actual,
                expected,
            },
            other => RpcTaskStatusError::Internal(other.to_string()),
        }
    }
}

/// In most cases, the RPC task action may fail with either [`RpcTaskStatusError::NoSuchTask`] or [`RpcTaskStatusError::Internal`].
/// Please do not add new error variants unless they are used in most cases.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum RpcTaskUserActionError {
    NoSuchTask(TaskId),
    Internal(String),
}

impl From<RpcTaskError> for RpcTaskUserActionError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::NoSuchTask(task_id) => RpcTaskUserActionError::NoSuchTask(task_id),
            rpc_err => RpcTaskUserActionError::Internal(rpc_err.to_string()),
        }
    }
}

impl HttpStatusCode for RpcTaskUserActionError {
    fn status_code(&self) -> StatusCode {
        match self {
            RpcTaskUserActionError::NoSuchTask(_) => StatusCode::BAD_REQUEST,
            RpcTaskUserActionError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The errors of the `cancel` method of every task family (CRD ch. 55 R55.2.15).
///
/// `NoSuchTask` and `TaskFinished` carry the requested task id as a JSON integer.
///
/// The response tables of the public docs type `error_data` as a string and head the `TaskFinished` example as a
/// success. Every example and the reference release carry an integer and answer `TaskFinished` as an mmrpc-level
/// error, which is what Reloaded does (CRD ch.55 R55.2.15).
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum CancelRpcTaskError {
    /// The task id was never issued by the family, was forgotten, or was cancelled already.
    NoSuchTask(TaskId),
    /// The task has a terminal result that has not been forgotten yet.
    TaskFinished(TaskId),
    Internal(String),
}

impl HttpStatusCode for CancelRpcTaskError {
    fn status_code(&self) -> StatusCode {
        match self {
            CancelRpcTaskError::NoSuchTask(_) => StatusCode::NOT_FOUND,
            CancelRpcTaskError::TaskFinished(_) => StatusCode::CONFLICT,
            CancelRpcTaskError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<RpcTaskError> for CancelRpcTaskError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::NoSuchTask(task_id) => CancelRpcTaskError::NoSuchTask(task_id),
            RpcTaskError::UnexpectedTaskStatus {
                task_id,
                actual: TaskStatusError::Finished,
                ..
            } => CancelRpcTaskError::TaskFinished(task_id),
            other => CancelRpcTaskError::Internal(other.to_string()),
        }
    }
}

/// In most cases, the RPC task status request consists of `task_id` and `forget_if_finished` fields only.
/// Please do not add new fields unless they are used in most cases.
#[derive(Deserialize)]
pub struct RpcTaskStatusRequest {
    pub task_id: TaskId,
    #[serde(default = "true_f")]
    pub forget_if_finished: bool,
}

/// Please do not add new fields unless they are used in most cases.
#[derive(Deserialize)]
pub struct RpcTaskUserActionRequest<UserAction> {
    pub task_id: TaskId,
    pub user_action: UserAction,
}

/// In most cases, the response to the RPC task initialization consists of `task_id` only.
/// Please do not add new fields unless they are used in most cases.
#[derive(Serialize)]
pub struct InitRpcTaskResponse {
    pub task_id: TaskId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cancel_error_wire(error: RpcTaskError) -> (serde_json::Value, StatusCode) {
        let error = CancelRpcTaskError::from(error);
        let status = error.status_code();
        (serde_json::to_value(&error).expect("serializable error"), status)
    }

    #[test]
    fn cancel_no_such_task_is_an_integer_with_http_404() {
        let (wire, status) = cancel_error_wire(RpcTaskError::NoSuchTask(42));
        assert_eq!(wire, json!({ "error_type": "NoSuchTask", "error_data": 42 }));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn cancel_of_a_finished_task_is_task_finished_with_http_409() {
        let (wire, status) = cancel_error_wire(RpcTaskError::UnexpectedTaskStatus {
            task_id: 7,
            actual: TaskStatusError::Finished,
            expected: TaskStatusError::InProgress,
        });
        assert_eq!(wire, json!({ "error_type": "TaskFinished", "error_data": 7 }));
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[test]
    fn cancel_internal_is_a_string_with_http_500() {
        let (wire, status) = cancel_error_wire(RpcTaskError::Internal("registry gone".to_owned()));
        assert_eq!(wire["error_type"], json!("Internal"));
        assert!(wire["error_data"].is_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn status_and_user_action_no_such_task_stay_http_400() {
        let status = RpcTaskStatusError::NoSuchTask(1);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({ "error_type": "NoSuchTask", "error_data": 1 })
        );
        assert_eq!(status.status_code(), StatusCode::BAD_REQUEST);
        let user_action = RpcTaskUserActionError::NoSuchTask(1);
        assert_eq!(
            serde_json::to_value(&user_action).unwrap(),
            json!({ "error_type": "NoSuchTask", "error_data": 1 })
        );
        assert_eq!(user_action.status_code(), StatusCode::BAD_REQUEST);
    }
}
