use crate::handle::RpcTaskHandle;
use async_trait::async_trait;
use futures::future::BoxFuture;
#[cfg(target_arch = "wasm32")]
use futures::future::LocalBoxFuture;
use mm2_err_handle::prelude::*;
use serde::Serialize;

/// The future a [`CancelCleanup`] produces. It is not required to be `Send` on `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub type CancelCleanupFuture = BoxFuture<'static, ()>;
#[cfg(target_arch = "wasm32")]
pub type CancelCleanupFuture = LocalBoxFuture<'static, ()>;

/// A cleanup that undoes the effects of a task that was cancelled (CRD ch. 55 R55.2.14).
///
/// The framework runs it once, after the cancelled task future has been dropped or has finished with
/// a result nobody can read any more, so the cleanup observes everything the task did.
pub type CancelCleanup = Box<dyn FnOnce() -> CancelCleanupFuture + Send>;

pub trait RpcTaskTypes {
    type Item: Serialize + Clone + Send + Sync + 'static;
    type Error: SerMmErrorType + Clone + Send + Sync + 'static;
    type InProgressStatus: Clone + Send + Sync + 'static;
    type AwaitingStatus: Clone + Send + Sync + 'static;
    type UserAction: NotMmError + Send + Sync + 'static;
}

// On native the task future must be `Send` (the multi-threaded executor
// spawns it across threads). On `wasm32` there is a single JS event loop and
// `common::executor::spawn` accepts a non-`Send` future, so relax the
// async-trait boxing to `?Send` there — this lets a task wrap a non-`Send`
// one-shot routine (e.g. the platform-coin-with-tokens activation). The `Send`
// supertrait and the `RpcTaskTypes` `Send + Sync` bounds are unchanged: every
// task struct and its associated types remain `Send`/`Sync` on both targets.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RpcTask: RpcTaskTypes + Sized + Send + 'static {
    fn initial_status(&self) -> Self::InProgressStatus;

    /// Returns the cleanup to run if the task is cancelled. It is taken before the task starts running.
    /// A task that leaves nothing behind keeps the default.
    fn cancel_cleanup(&self) -> Option<CancelCleanup> { None }

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>>;
}
