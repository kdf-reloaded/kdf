use common::custom_futures::TimeoutError;
use derive_more::Display;
use futures::channel::oneshot;
use mm2_err_handle::prelude::*;
use mm2_rpc::mm_protocol::MmRpcResult;
use serde::Serialize;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

#[macro_use] extern crate ser_error_derive;
#[macro_use] extern crate serde_derive;

mod handle;
mod manager;
pub mod rpc_common;
mod task;

pub use handle::RpcTaskHandle;
pub use manager::{RpcTaskManager, RpcTaskManagerShared};
pub use task::{CancelCleanup, CancelCleanupFuture, RpcTask, RpcTaskTypes};

pub type FinishedTaskResult<Item, Error> = MmRpcResult<Item, Error>;
pub type RpcTaskResult<T> = Result<T, MmError<RpcTaskError>>;
pub type TaskId = u64;
pub type RpcTaskStatusAlias<Task> = RpcTaskStatus<
    <Task as RpcTaskTypes>::Item,
    <Task as RpcTaskTypes>::Error,
    <Task as RpcTaskTypes>::InProgressStatus,
    <Task as RpcTaskTypes>::AwaitingStatus,
>;

type AtomicTaskId = AtomicU64;
type TaskAbortHandle = oneshot::Sender<()>;
type TaskAbortHandler = oneshot::Receiver<()>;
type UserActionSender<UserAction> = oneshot::Sender<UserAction>;

#[derive(Clone, Debug, Display)]
pub enum RpcTaskError {
    #[display(fmt = "RPC task timeout '{:?}'", _0)]
    Timeout(Duration),
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
    Canceled,
    /// The user action delivered to the task is not the kind the task waits for.
    /// `expected` names the `action_type` the task waits for.
    #[display(fmt = "Unexpected user action: expected '{}'", expected)]
    UnexpectedUserAction {
        expected: &'static str,
    },
    Internal(String),
}

#[derive(Clone, Debug, Display, Serialize)]
pub enum TaskStatusError {
    Idle,
    InProgress,
    AwaitingUserAction,
    Finished,
}

impl From<TimeoutError> for RpcTaskError {
    fn from(e: TimeoutError) -> Self { RpcTaskError::Timeout(e.duration) }
}

/// We can't simplify the generic types because there are places where the [`RpcTaskStatus::map_err`] method is used.
///
/// The `status`/`details` wire representation must stay aligned with the canonical KDF API that GUI
/// clients expect: a finished task reports `{"status": "Ok", "details": <item>}` on success and
/// `{"status": "Error", "details": <error>}` on failure. Collapsing both into a single terminal
/// variant breaks clients that treat `Ok` as the completion signal.
///
/// The error object sits directly in `details`. Several task pages of the public docs describe a `details.result`
/// wrapper, but every published example and the reference release have none, so Reloaded has none either
/// (CRD ch.55 R55.1.6, ch.35 R35.3.5).
#[derive(Debug, Serialize)]
#[serde(tag = "status", content = "details")]
pub enum RpcTaskStatus<Item, Error, InProgressStatus, AwaitingStatus>
where
    Item: Serialize,
    Error: SerMmErrorType,
{
    Ok(Item),
    Error(MmError<Error>),
    InProgress(InProgressStatus),
    UserActionRequired(AwaitingStatus),
}

impl<Item, Error, InProgressStatus, AwaitingStatus> RpcTaskStatus<Item, Error, InProgressStatus, AwaitingStatus>
where
    Item: Serialize,
    Error: SerMmErrorType,
{
    pub fn map_err<NewError, F>(self, f: F) -> RpcTaskStatus<Item, NewError, InProgressStatus, AwaitingStatus>
    where
        F: FnOnce(Error) -> NewError,
        NewError: SerMmErrorType,
    {
        match self {
            RpcTaskStatus::Ok(item) => RpcTaskStatus::Ok(item),
            RpcTaskStatus::Error(error) => RpcTaskStatus::Error(error.map(f)),
            RpcTaskStatus::InProgress(in_progress) => RpcTaskStatus::InProgress(in_progress),
            RpcTaskStatus::UserActionRequired(awaiting) => RpcTaskStatus::UserActionRequired(awaiting),
        }
    }
}

enum TaskStatus<Task: RpcTaskTypes> {
    Ready(FinishedTaskResult<Task::Item, Task::Error>),
    InProgress(Task::InProgressStatus),
    UserActionRequired {
        awaiting_status: Task::AwaitingStatus,
        user_action_tx: UserActionSender<Task::UserAction>,
    },
}
