/// Swap status event streamer activation.
///
/// Enables a global SSE stream that broadcasts swap state transitions
/// (both V1 and V2, maker and taker) to connected clients.
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::EventStreamer;

use super::{EnableStreamingRequest, EnableStreamingResponse, StreamingError};
use crate::mm2::lp_swap::swap_events::SwapStatusStreamer;

/// Request body for `stream::swap_status::enable`.
///
/// No additional configuration needed; the stream is global.
use serde::Deserialize;

#[derive(Deserialize)]
pub struct EnableSwapStatusRequest {}

/// RPC handler for `stream::swap_status::enable`.
pub async fn enable_swap_status(
    ctx: MmArc,
    req: EnableStreamingRequest<EnableSwapStatusRequest>,
) -> MmResult<EnableStreamingResponse, StreamingError> {
    let client_id = req.client_id;
    let streamer = SwapStatusStreamer;
    let streamer_id = streamer.streamer_id().to_string();

    ctx.event_stream_manager
        .add(client_id, streamer)
        .await
        .map_err(|e| MmError::new(StreamingError::InitFailed(e)))?;

    Ok(EnableStreamingResponse::new(streamer_id))
}
