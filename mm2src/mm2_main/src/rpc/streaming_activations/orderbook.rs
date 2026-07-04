//! RPC activation of the per-pair orderbook streamer.

use super::{EnableStreamingResponse, StreamingError};
use crate::mm2::lp_ordermatch::orderbook_events::OrderbookStreamer;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::EventStreamer;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct EnableOrderbookRequest {
    pub client_id: u64,
    pub base: String,
    pub rel: String,
}

pub async fn enable_orderbook(
    ctx: MmArc,
    req: EnableOrderbookRequest,
) -> MmResult<EnableStreamingResponse, StreamingError> {
    let streamer = OrderbookStreamer::new(ctx.clone(), req.base, req.rel);
    let streamer_id = streamer.streamer_id().to_string();
    ctx.event_stream_manager
        .add(req.client_id, streamer)
        .await
        .map(|_| EnableStreamingResponse::new(streamer_id))
        .map_to_mm(StreamingError::InitFailed)
}
