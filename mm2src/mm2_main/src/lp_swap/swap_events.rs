//! # Purpose
//! Fan-out of in-memory swap state changes to all subscribed SSE clients.
//!
//! # Public exports
//! - [`SwapStatusStreamer`] — the streamer instance registered on the
//!   `stream::swap_status::enable` RPC.
//! - [`SwapStatusEvent`] — the wire envelope emitted on each state change.
//!
//! # Invariants
//! - The JSON envelope shape `{ "swap_type": "<MakerV1|TakerV1|MakerV2|TakerV2>",
//!   "swap_data": { "uuid": ..., "event": ... } }` is wire-compatible with
//!   GUI clients; variant names and field names must not change.
//! - The streamer id [`StreamerId::SwapStatus`] is the channel name used by
//!   the SSE subscription RPC.

use async_trait::async_trait;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use serde::Serialize;
use uuid::Uuid;

// Local aliases keep the per-variant type names short and decouple the
// event-envelope module from the concrete state-machine event type names
// declared in the V1 and V2 swap modules.
use super::maker_swap::MakerSwapEvent as MakerLegacyEvent;
use super::maker_swap_v2::MakerSwapEvent as MakerV2StateEvent;
use super::taker_swap::TakerSwapEvent as TakerLegacyEvent;
use super::taker_swap_v2::TakerSwapEvent as TakerV2StateEvent;

/// A single swap-status update emitted to SSE subscribers.
///
/// The `swap_type` discriminant tells GUI clients which event schema to
/// expect inside `swap_data.event`.
#[derive(Serialize)]
#[serde(tag = "swap_type", content = "swap_data")]
pub enum SwapStatusEvent {
    MakerV1 { uuid: Uuid, event: MakerLegacyEvent },
    TakerV1 { uuid: Uuid, event: TakerLegacyEvent },
    MakerV2 { uuid: Uuid, event: MakerV2StateEvent },
    TakerV2 { uuid: Uuid, event: TakerV2StateEvent },
}

/// Global streamer that relays swap-status events to SSE subscribers.
///
/// One instance is registered per node and shared by every running swap.
pub struct SwapStatusStreamer;

#[async_trait]
impl EventStreamer for SwapStatusStreamer {
    type DataInType = SwapStatusEvent;

    fn streamer_id(&self) -> StreamerId { StreamerId::SwapStatus }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        _shutdown_rx: oneshot::Receiver<()>,
        mut data_rx: mpsc::UnboundedReceiver<Self::DataInType>,
    ) {
        // Receiver-dropped here means the supervisor is already tearing the
        // streamer down; swallow the error rather than panic during shutdown.
        let _ = ready_tx.send(Ok(()));

        while let Some(swap_data) = data_rx.recv().await {
            // Serialization of `SwapStatusEvent` cannot fail: every variant
            // serialises a `Uuid` and a `#[derive(Serialize)]` event enum.
            let event_data = serde_json::to_value(&swap_data).expect("SwapStatusEvent serialization is infallible");
            broadcaster.broadcast(Event::new(self.streamer_id(), event_data));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm2_event_stream::StreamingManager;
    use std::sync::Arc;
    use std::time::Duration;

    /// Registers the swap-status streamer, pushes `event` through it, and
    /// returns the single [`Event`] delivered to a subscribed client.
    async fn deliver(event: SwapStatusEvent) -> Arc<Event> {
        let manager = StreamingManager::default();
        let mut client = manager.new_client(1);
        manager.add(1, SwapStatusStreamer).await.unwrap();
        manager.send_fn(&StreamerId::SwapStatus, || event).unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.rx.recv())
            .await
            .expect("timed out waiting for swap-status event")
            .expect("streamer channel closed")
    }

    #[tokio::test]
    async fn maker_v1_event_is_broadcast_with_stable_envelope() {
        let uuid = Uuid::default();
        let event = deliver(SwapStatusEvent::MakerV1 {
            uuid,
            event: MakerLegacyEvent::Finished,
        })
        .await;

        assert_eq!(event.origin(), "SWAP_STATUS");
        assert!(!event.is_error());
        let (_, data) = event.get();
        assert_eq!(data["swap_type"], "MakerV1");
        assert_eq!(data["swap_data"]["uuid"], uuid.to_string());
        assert_eq!(data["swap_data"]["event"]["type"], "Finished");
    }

    #[tokio::test]
    async fn taker_v1_event_is_broadcast_with_stable_envelope() {
        let uuid = Uuid::default();
        let event = deliver(SwapStatusEvent::TakerV1 {
            uuid,
            event: TakerLegacyEvent::Finished,
        })
        .await;

        assert_eq!(event.origin(), "SWAP_STATUS");
        assert!(!event.is_error());
        let (_, data) = event.get();
        assert_eq!(data["swap_type"], "TakerV1");
        assert_eq!(data["swap_data"]["uuid"], uuid.to_string());
        assert_eq!(data["swap_data"]["event"]["type"], "Finished");
    }
}
