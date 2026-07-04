use crate::handle::RpcTaskHandle;
use async_trait::async_trait;
use mm2_err_handle::prelude::*;
use serde::Serialize;

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

    async fn run(self, task_handle: &RpcTaskHandle<Self>) -> Result<Self::Item, MmError<Self::Error>>;
}
