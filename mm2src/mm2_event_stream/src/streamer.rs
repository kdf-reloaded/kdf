//! # Purpose
//! Defines the [`EventStreamer`] trait, the wire-stable [`StreamerId`]
//! enum, and the [`Broadcaster`] handle a running streamer uses to fan
//! events out to subscribers.
//!
//! # Public exports
//! - [`StreamerId`] — origin identifier; its `Display` form is on-the-wire.
//! - [`Broadcaster`] — cheap-to-clone handle that pushes events into the
//!   manager's per-client channels.
//! - [`NoDataIn`] — uninhabited marker for streamers with no external
//!   input.
//! - [`EventStreamer`] — async trait every streamer implements.
//!
//! # Invariants
//! - [`StreamerId::Display`] strings (`HEARTBEAT`, `BALANCE:<COIN>`, …)
//!   are part of the SSE wire surface — do not rename.
//! - Per-client send channels are bounded; broadcasts use `try_send` so a
//!   slow client never blocks the broadcaster.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::event::Event;
use crate::manager::StreamingManagerInner;

/// Identifies a specific event streamer type.
///
/// Each variant corresponds to one category of real-time events.
/// String payloads allow per-coin or per-entity disambiguation
/// (e.g., `Balance("KMD")` vs `Balance("BTC")`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamerId {
    Heartbeat,
    Balance(String),
    Network,
    SwapStatus,
    OrderStatus,
    OrderbookUpdate {
        topic: String,
    },
    /// Continuous EIP-1559 fee-per-gas estimate for an EVM coin; the payload is
    /// the coin ticker the estimate is produced for.
    FeeEstimation(String),
    /// Reactive transaction-history records for one coin ticker.
    TxHistory(String),
    /// Process termination signal notifications.
    ShutdownSignal,
    /// Carries an interactive data-asker "data needed" event; the payload is
    /// the data-type discriminator naming the kind of data being requested.
    DataNeeded(String),
}

impl fmt::Display for StreamerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamerId::Heartbeat => write!(f, "HEARTBEAT"),
            StreamerId::Balance(coin) => write!(f, "BALANCE:{}", coin),
            StreamerId::Network => write!(f, "NETWORK"),
            StreamerId::SwapStatus => write!(f, "SWAP_STATUS"),
            StreamerId::OrderStatus => write!(f, "ORDER_STATUS"),
            StreamerId::OrderbookUpdate { topic } => write!(f, "ORDERBOOK:{}", topic),
            StreamerId::FeeEstimation(coin) => write!(f, "FEE_ESTIMATION:{}", coin),
            StreamerId::TxHistory(coin) => write!(f, "TX_HISTORY:{}", coin),
            StreamerId::ShutdownSignal => write!(f, "SHUTDOWN_SIGNAL"),
            StreamerId::DataNeeded(data_type) => write!(f, "DATA_NEEDED:{}", data_type),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseStreamerIdError;

impl FromStr for StreamerId {
    type Err = ParseStreamerIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "HEARTBEAT" {
            return Ok(StreamerId::Heartbeat);
        }
        if value == "NETWORK" {
            return Ok(StreamerId::Network);
        }
        if value == "SWAP_STATUS" {
            return Ok(StreamerId::SwapStatus);
        }
        if value == "ORDER_STATUS" {
            return Ok(StreamerId::OrderStatus);
        }
        if value == "SHUTDOWN_SIGNAL" {
            return Ok(StreamerId::ShutdownSignal);
        }

        if let Some(ticker) = value.strip_prefix("BALANCE:").filter(|ticker| !ticker.is_empty()) {
            return Ok(StreamerId::Balance(ticker.to_owned()));
        }
        if let Some(topic) = value.strip_prefix("ORDERBOOK:").filter(|topic| !topic.is_empty()) {
            return Ok(StreamerId::OrderbookUpdate {
                topic: topic.to_owned(),
            });
        }
        if let Some(ticker) = value
            .strip_prefix("FEE_ESTIMATION:")
            .filter(|ticker| !ticker.is_empty())
        {
            return Ok(StreamerId::FeeEstimation(ticker.to_owned()));
        }
        if let Some(ticker) = value.strip_prefix("TX_HISTORY:").filter(|ticker| !ticker.is_empty()) {
            return Ok(StreamerId::TxHistory(ticker.to_owned()));
        }

        Err(ParseStreamerIdError)
    }
}

/// Broadcaster handle given to each streamer for emitting events.
#[derive(Clone)]
pub struct Broadcaster {
    pub(crate) inner: Arc<parking_lot::RwLock<StreamingManagerInner>>,
}

impl Broadcaster {
    /// Broadcast an event to all clients subscribed to its origin streamer.
    pub fn broadcast(&self, event: Arc<Event>) {
        let inner = self.inner.read();
        let origin = event.origin();
        for client in inner.clients.values() {
            if client.listening_to.contains(&origin) {
                // Best-effort: if the channel is full, skip this client for this event.
                let _ = client.tx.try_send(event.clone());
            }
        }
    }
}

/// Marker type for streamers that don't receive external data.
/// Since this enum has no variants, `mpsc::UnboundedReceiver<NoDataIn>`
/// will never yield a value, which is exactly what self-driven streamers need.
pub enum NoDataIn {}

/// Core trait for all event streamers.
///
/// Implementors define how to produce events. The streaming manager
/// spawns the `handle` method when the first client subscribes and
/// shuts it down when the last client unsubscribes.
#[async_trait]
pub trait EventStreamer: Sized + Send + 'static {
    /// The type of data this streamer can receive from external sources.
    /// Use `NoDataIn` if the streamer is self-driven (e.g., polling).
    type DataInType: Send;

    /// Unique identifier for this streamer instance.
    fn streamer_id(&self) -> StreamerId;

    /// Main event loop. Called once when the first client subscribes.
    ///
    /// * `broadcaster` — use to emit events to subscribed clients
    /// * `ready_tx` — send `Ok(())` when initialization is done, or `Err` to abort
    /// * `shutdown_rx` — resolves when the streamer should stop
    /// * `data_rx` — channel for receiving external data pushes (empty for `NoDataIn`)
    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
        shutdown_rx: tokio::sync::oneshot::Receiver<()>,
        data_rx: mpsc::UnboundedReceiver<Self::DataInType>,
    );
}

#[cfg(test)]
mod tests {
    use super::StreamerId;
    use std::str::FromStr;

    #[test]
    fn streamer_id_parses_enable_wire_strings() {
        assert_eq!(StreamerId::from_str("HEARTBEAT"), Ok(StreamerId::Heartbeat));
        assert_eq!(
            StreamerId::from_str("BALANCE:KMD"),
            Ok(StreamerId::Balance("KMD".to_owned()))
        );
        assert_eq!(StreamerId::from_str("NETWORK"), Ok(StreamerId::Network));
        assert_eq!(StreamerId::from_str("SWAP_STATUS"), Ok(StreamerId::SwapStatus));
        assert_eq!(StreamerId::from_str("ORDER_STATUS"), Ok(StreamerId::OrderStatus));
        assert_eq!(StreamerId::from_str("SHUTDOWN_SIGNAL"), Ok(StreamerId::ShutdownSignal));
        assert_eq!(
            StreamerId::from_str("ORDERBOOK:KMD/BTC"),
            Ok(StreamerId::OrderbookUpdate {
                topic: "KMD/BTC".to_owned()
            })
        );
        assert_eq!(
            StreamerId::from_str("FEE_ESTIMATION:ETH"),
            Ok(StreamerId::FeeEstimation("ETH".to_owned()))
        );
        assert_eq!(
            StreamerId::from_str("TX_HISTORY:KMD"),
            Ok(StreamerId::TxHistory("KMD".to_owned()))
        );
    }

    #[test]
    fn streamer_id_rejects_non_enable_wire_strings() {
        assert!(StreamerId::from_str("DATA_NEEDED:pin").is_err());
        assert!(StreamerId::from_str("BALANCE:").is_err());
        assert!(StreamerId::from_str("ORDERBOOK:").is_err());
        assert!(StreamerId::from_str("TX_HISTORY:").is_err());
        assert!(StreamerId::from_str("UNKNOWN").is_err());
    }
}
