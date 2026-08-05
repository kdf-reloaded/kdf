//! Order status SSE event streamer.
//!
//! Broadcasts order lifecycle events (matches, connections) to subscribed
//! SSE clients via the streaming infrastructure.

use super::{MakerMatch, TakerMatch};
use async_trait::async_trait;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use serde::Serialize;

/// Streamer that relays order status events to SSE clients.
pub struct OrderStatusStreamer;

/// Events emitted during order lifecycle.
#[derive(Serialize)]
#[serde(tag = "order_type", content = "order_data")]
pub enum OrderStatusEvent {
    MakerMatch(MakerMatch),
    TakerMatch(TakerMatch),
    MakerConnected(MakerMatch),
    TakerConnected(TakerMatch),
}

#[async_trait]
impl EventStreamer for OrderStatusStreamer {
    type DataInType = OrderStatusEvent;

    fn streamer_id(&self) -> StreamerId { StreamerId::OrderStatus }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        _shutdown_rx: oneshot::Receiver<()>,
        mut data_rx: mpsc::UnboundedReceiver<Self::DataInType>,
    ) {
        let _ = ready_tx.send(Ok(()));

        while let Some(order_data) = data_rx.recv().await {
            let event_data = serde_json::to_value(order_data).expect("Serialization shouldn't fail.");
            let event = Event::new(self.streamer_id(), event_data);
            broadcaster.broadcast(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ordermatch_types::{MakerReserved, MatchBy, TakerAction, TakerConnect, TakerRequest};
    use super::*;
    use common::mm_number::MmNumber;
    use mm2_event_stream::StreamingManager;
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    fn sample_taker_request() -> TakerRequest {
        TakerRequest {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: MmNumber::default(),
            rel_amount: MmNumber::default(),
            action: TakerAction::Buy,
            uuid: Uuid::default(),
            sender_pubkey: Default::default(),
            dest_pub_key: Default::default(),
            match_by: MatchBy::default(),
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: Default::default(),
        }
    }

    fn sample_taker_connect() -> TakerConnect {
        TakerConnect {
            taker_order_uuid: Uuid::default(),
            maker_order_uuid: Uuid::default(),
            sender_pubkey: Default::default(),
            dest_pub_key: Default::default(),
        }
    }

    fn sample_maker_match() -> MakerMatch {
        MakerMatch {
            request: sample_taker_request(),
            reserved: MakerReserved::default(),
            connect: None,
            connected: None,
            last_updated: 0,
        }
    }

    fn sample_taker_match() -> TakerMatch {
        TakerMatch {
            reserved: MakerReserved::default(),
            connect: sample_taker_connect(),
            connected: None,
            last_updated: 0,
        }
    }

    /// Registers the order-status streamer, pushes `event` through it, and
    /// returns the single [`Event`] delivered to a subscribed client.
    async fn deliver(event: OrderStatusEvent) -> Arc<Event> {
        let manager = StreamingManager::default();
        let mut client = manager.new_client(1);
        manager.add(1, OrderStatusStreamer).await.unwrap();
        manager.send_fn(&StreamerId::OrderStatus, || event).unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.rx.recv())
            .await
            .expect("timed out waiting for order-status event")
            .expect("streamer channel closed")
    }

    #[tokio::test]
    async fn taker_match_event_is_broadcast_with_stable_envelope() {
        let event = deliver(OrderStatusEvent::TakerMatch(sample_taker_match())).await;
        assert_eq!(event.origin(), "ORDER_STATUS");
        assert!(!event.is_error());
        assert_eq!(event.get().1["order_type"], "TakerMatch");
    }

    #[tokio::test]
    async fn maker_connected_event_is_broadcast_with_stable_envelope() {
        let event = deliver(OrderStatusEvent::MakerConnected(sample_maker_match())).await;
        assert_eq!(event.origin(), "ORDER_STATUS");
        assert!(!event.is_error());
        assert_eq!(event.get().1["order_type"], "MakerConnected");
    }

    #[tokio::test]
    async fn taker_connected_event_is_broadcast_with_stable_envelope() {
        let event = deliver(OrderStatusEvent::TakerConnected(sample_taker_match())).await;
        assert_eq!(event.origin(), "ORDER_STATUS");
        assert!(!event.is_error());
        assert_eq!(event.get().1["order_type"], "TakerConnected");
    }
}
