//! Relay connection handler.
//!
//! The relay client drives this handler on its background task; every inbound
//! relay message, and every sign that the connection is gone, is forwarded onto
//! an unbounded channel that the subsystem's own loop drains. The loop routes
//! messages by message id and reopens the connection after a loss.

use relay_client::error::ClientError;
use relay_client::websocket::{CloseFrame, ConnectionHandler, PublishedMessage};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::relay_error_kind;

/// An event of the relay connection, as seen by the subsystem's processing loop.
#[derive(Debug)]
pub enum RelayEvent {
    /// A message arrived on a subscribed topic.
    Message(PublishedMessage),
    /// The connection was closed by the relay, or failed. The text describes the cause for the log (a close code and
    /// reason, or an error text); it never carries message content.
    Lost(String),
}

/// Forwards relay events to the subsystem's processing loop.
pub struct WcConnectionHandler {
    inbound_tx: UnboundedSender<RelayEvent>,
}

impl WcConnectionHandler {
    /// Creates a handler that forwards relay events onto `inbound_tx`.
    pub fn new(inbound_tx: UnboundedSender<RelayEvent>) -> Self { WcConnectionHandler { inbound_tx } }

    fn lost(&self, reason: String) {
        // If the receiver has been dropped there is nothing left to react.
        let _ = self.inbound_tx.send(RelayEvent::Lost(reason));
    }
}

impl ConnectionHandler for WcConnectionHandler {
    fn message_received(&mut self, message: PublishedMessage) {
        // If the receiver has been dropped there is nothing left to route to.
        let _ = self.inbound_tx.send(RelayEvent::Message(message));
    }

    fn disconnected(&mut self, frame: Option<CloseFrame<'static>>) {
        let reason = match frame {
            Some(frame) => format!("closed by the relay: {frame:?}"),
            None => "closed without a close frame".to_string(),
        };
        self.lost(reason);
    }

    fn inbound_error(&mut self, error: ClientError) {
        self.lost(format!("inbound error: {}", relay_error_kind(&error)));
    }

    fn outbound_error(&mut self, error: ClientError) {
        self.lost(format!("outbound error: {}", relay_error_kind(&error)));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_close_and_the_errors_each_raise_a_loss_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = WcConnectionHandler::new(tx);

        handler.disconnected(None);
        handler.inbound_error(ClientError::ChannelClosed);
        handler.outbound_error(ClientError::ChannelClosed);

        for _ in 0..3 {
            match rx.try_recv() {
                Ok(RelayEvent::Lost(reason)) => assert!(!reason.is_empty()),
                other => panic!("expected a loss event, got {other:?}"),
            }
        }
        assert!(rx.try_recv().is_err());
    }
}
