/// Streaming activation/deactivation RPC handlers.
///
/// These handlers respond to `stream::*` RPC namespace methods,
/// enabling or disabling real-time event streams per client.
pub mod balance;
pub mod fee_estimator;
pub mod heartbeat;
pub mod network;
pub mod orderbook;
pub mod orders;
#[cfg(all(unix, not(target_arch = "wasm32")))]
pub mod shutdown_signal;
pub mod swaps;
pub mod tx_history;

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::{StopStreamError, StreamerId};
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Common request wrapper for streaming activation RPCs.
#[derive(Deserialize)]
pub struct EnableStreamingRequest<T> {
    #[serde(default)]
    pub client_id: u64,
    #[serde(flatten)]
    pub inner: T,
}

/// Response returned when a streamer is successfully enabled.
#[derive(Serialize)]
pub struct EnableStreamingResponse {
    /// Identifier of the enabled streamer. Clients use it to correlate the
    /// subscription with the SSE events it emits.
    pub streamer_id: String,
}

impl EnableStreamingResponse {
    pub fn new(streamer_id: String) -> Self { Self { streamer_id } }
}

/// Errors that can occur during streaming operations.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum StreamingError {
    #[display(fmt = "Streamer initialization failed: {}", _0)]
    InitFailed(String),
    #[display(fmt = "Unknown streaming client: {}", _0)]
    UnknownClient(u64),
    #[display(fmt = "Streamer is not active: {}", _0)]
    StreamerNotActive(String),
    #[display(fmt = "Invalid streamer id: {}", _0)]
    InvalidStreamerId(String),
}

impl HttpStatusCode for StreamingError {
    fn status_code(&self) -> StatusCode {
        match self {
            StreamingError::InitFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            StreamingError::UnknownClient(_)
            | StreamingError::StreamerNotActive(_)
            | StreamingError::InvalidStreamerId(_) => StatusCode::BAD_REQUEST,
        }
    }
}

/// Request body for `stream::disable`.
#[derive(Debug, Deserialize)]
pub struct DisableStreamingRequest {
    pub client_id: u64,
    pub streamer_id: String,
}

/// Response returned when a stream subscription is successfully disabled.
#[derive(Debug, Serialize)]
pub struct DisableStreamingResponse {
    pub result: &'static str,
}

impl DisableStreamingResponse {
    fn success() -> Self { Self { result: "Success" } }
}

/// Handler for `stream::disable`.
pub async fn disable_streaming(
    ctx: MmArc,
    req: DisableStreamingRequest,
) -> MmResult<DisableStreamingResponse, StreamingError> {
    let streamer_id = StreamerId::from_str(&req.streamer_id)
        .map_to_mm(|_| StreamingError::InvalidStreamerId(req.streamer_id.clone()))?;
    ctx.event_stream_manager
        .stop_checked(req.client_id, &streamer_id)
        .map_to_mm(|err| match err {
            StopStreamError::UnknownClient => StreamingError::UnknownClient(req.client_id),
            StopStreamError::StreamerNotActive => StreamingError::StreamerNotActive(req.streamer_id),
        })?;
    Ok(DisableStreamingResponse::success())
}

#[cfg(test)]
mod tests {
    use super::heartbeat::HeartbeatStreamer;
    use super::*;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    #[tokio::test]
    async fn disable_streaming_returns_success_and_preserves_other_subscriptions() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let _h1 = ctx.event_stream_manager.new_client(1);
        let _h2 = ctx.event_stream_manager.new_client(2);
        ctx.event_stream_manager
            .add(1, HeartbeatStreamer::new(5))
            .await
            .unwrap();
        ctx.event_stream_manager
            .add(2, HeartbeatStreamer::new(5))
            .await
            .unwrap();

        let res = disable_streaming(ctx.clone(), DisableStreamingRequest {
            client_id: 1,
            streamer_id: "HEARTBEAT".to_owned(),
        })
        .await
        .unwrap();

        assert_eq!(serde_json::to_value(res).unwrap(), json!({ "result": "Success" }));
        assert!(!ctx.event_stream_manager.client_subscribed_to(1, &StreamerId::Heartbeat));
        assert!(ctx.event_stream_manager.client_subscribed_to(2, &StreamerId::Heartbeat));
        assert!(ctx.event_stream_manager.is_active(&StreamerId::Heartbeat));
    }

    #[tokio::test]
    async fn disable_streaming_maps_client_streamer_and_validation_errors_to_bad_request() {
        let ctx = MmCtxBuilder::default().into_mm_arc();

        let unknown_client = disable_streaming(ctx.clone(), DisableStreamingRequest {
            client_id: 1,
            streamer_id: "HEARTBEAT".to_owned(),
        })
        .await
        .unwrap_err();
        assert!(matches!(unknown_client.into_inner(), StreamingError::UnknownClient(1)));

        let _h1 = ctx.event_stream_manager.new_client(1);
        let not_active = disable_streaming(ctx.clone(), DisableStreamingRequest {
            client_id: 1,
            streamer_id: "HEARTBEAT".to_owned(),
        })
        .await
        .unwrap_err();
        assert!(matches!(not_active.into_inner(), StreamingError::StreamerNotActive(_)));

        let invalid = disable_streaming(ctx, DisableStreamingRequest {
            client_id: 1,
            streamer_id: "DATA_NEEDED:pin".to_owned(),
        })
        .await
        .unwrap_err();
        assert!(matches!(invalid.into_inner(), StreamingError::InvalidStreamerId(_)));
        assert_eq!(StreamingError::UnknownClient(1).status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            StreamingError::StreamerNotActive("HEARTBEAT".to_owned()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            StreamingError::InvalidStreamerId("DATA_NEEDED:pin".to_owned()).status_code(),
            StatusCode::BAD_REQUEST
        );
    }
}
