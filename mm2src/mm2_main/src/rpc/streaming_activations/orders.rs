//! RPC activation of the order status streamer.

use super::{EnableStreamingRequest, EnableStreamingResponse, StreamingError};
use crate::mm2::lp_ordermatch::order_events::OrderStatusStreamer;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::EventStreamer;

pub async fn enable_order_status(
    ctx: MmArc,
    req: EnableStreamingRequest<()>,
) -> MmResult<EnableStreamingResponse, StreamingError> {
    let streamer = OrderStatusStreamer;
    let streamer_id = streamer.streamer_id().to_string();
    ctx.event_stream_manager
        .add(req.client_id, streamer)
        .await
        .map(|_| EnableStreamingResponse::new(streamer_id))
        .map_to_mm(StreamingError::InitFailed)
}
