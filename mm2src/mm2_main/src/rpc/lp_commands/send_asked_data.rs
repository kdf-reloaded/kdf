use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub type SendAskedDataResult<T> = Result<T, MmError<SendAskedDataError>>;

/// Errors returned by the `send_asked_data` RPC (R19).
#[derive(Debug, Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SendAskedDataError {
    /// No pending ask exists for the supplied data id — it was never allocated,
    /// or the ask already expired or was already answered.
    #[display(fmt = "No pending ask found for data id {}", _0)]
    NoSuchDataId(u64),
    /// The awaiting caller has already departed, so the answer could not be
    /// delivered.
    #[display(fmt = "The awaiting caller for data id {} is no longer present", _0)]
    WaiterGone(u64),
}

impl HttpStatusCode for SendAskedDataError {
    fn status_code(&self) -> StatusCode {
        match self {
            SendAskedDataError::NoSuchDataId(_) => StatusCode::NOT_FOUND,
            SendAskedDataError::WaiterGone(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Deserialize)]
pub struct SendAskedDataRequest {
    data_id: u64,
    data: Json,
}

/// Delivers an answer to a pending interactive data-asker ask (R19).
///
/// Returns the bare boolean `true` on success. A data id with no pending ask
/// yields `404 Not Found`; a departed awaiting caller yields `500`.
pub async fn send_asked_data(ctx: MmArc, req: SendAskedDataRequest) -> SendAskedDataResult<bool> {
    match ctx.data_asker.send_asked_data(req.data_id, req.data) {
        Ok(true) => Ok(true),
        Ok(false) => MmError::err(SendAskedDataError::NoSuchDataId(req.data_id)),
        Err(_waiter_dropped) => MmError::err(SendAskedDataError::WaiterGone(req.data_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    #[test]
    fn send_asked_data_unknown_id_is_not_found() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let req = SendAskedDataRequest {
            data_id: 99,
            data: json!("answer"),
        };
        let err = block_on(send_asked_data(ctx, req)).unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn send_asked_data_resolves_pending_ask() {
        let ctx = MmCtxBuilder::default().into_mm_arc();

        let ctx_for_ask = ctx.clone();
        let (answer, resp) = block_on(async {
            let ask = ctx_for_ask.ask_for_data::<_, String>("needed_data", "question", 30.0);
            let deliver = async {
                // `join` polls `ask` first, registering data id 0 before this runs.
                let req = SendAskedDataRequest {
                    data_id: 0,
                    data: json!("answer"),
                };
                send_asked_data(ctx.clone(), req).await
            };
            futures::future::join(ask, deliver).await
        });

        assert_eq!(answer.expect("ask should resolve with the answer"), "answer");
        assert!(resp.expect("send_asked_data should succeed"));
    }
}
