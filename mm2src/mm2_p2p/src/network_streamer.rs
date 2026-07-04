//! Network event streamer.
//!
//! Periodically snapshots the node's gossipsub / peer-connectivity state and
//! broadcasts it as a `NETWORK` SSE event. The streamer is self-driven
//! (`NoDataIn`) and timer-paced, emitting on change by default (or every cycle
//! when `always_send` is set). It lives here, in the p2p crate, because it
//! introspects this crate's gossipsub state via the `atomicdex_behaviour`
//! accessors.

use async_trait::async_trait;
use common::executor::Timer;
use futures::future::{select, Either};
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, NoDataIn, StreamerId};
use serde_json::{json, Value as Json};

use crate::atomicdex_behaviour::{get_gossip_mesh, get_gossip_peer_topics, get_gossip_topic_peers, get_peers_info,
                                 get_relay_mesh, AdexCmdTx};

/// Default delay (in seconds) between successive network-snapshot emissions.
const DEFAULT_STREAM_INTERVAL_SECONDS: f64 = 5.0;

/// The network streamer.
pub struct NetworkStreamer {
    /// Delay between successive network-snapshot emissions.
    stream_interval_seconds: f64,
    /// When `true`, emit every cycle even if the snapshot is unchanged.
    always_send: bool,
    /// Command channel used to introspect the gossipsub behaviour.
    cmd_tx: AdexCmdTx,
}

impl NetworkStreamer {
    pub fn new(stream_interval_seconds: Option<f64>, always_send: bool, cmd_tx: AdexCmdTx) -> Self {
        Self {
            stream_interval_seconds: stream_interval_seconds.unwrap_or(DEFAULT_STREAM_INTERVAL_SECONDS),
            always_send,
            cmd_tx,
        }
    }

    /// Assemble the current gossipsub / peer-connectivity snapshot (R29).
    async fn snapshot(&self) -> Json {
        let directly_connected_peers = get_peers_info(self.cmd_tx.clone()).await;
        let gossip_mesh = get_gossip_mesh(self.cmd_tx.clone()).await;
        let gossip_peer_topics = get_gossip_peer_topics(self.cmd_tx.clone()).await;
        let gossip_topic_peers = get_gossip_topic_peers(self.cmd_tx.clone()).await;
        let relay_mesh = get_relay_mesh(self.cmd_tx.clone()).await;

        json!({
            "directly_connected_peers": directly_connected_peers,
            "gossip_mesh": gossip_mesh,
            "gossip_peer_topics": gossip_peer_topics,
            "gossip_topic_peers": gossip_topic_peers,
            "relay_mesh": relay_mesh,
        })
    }
}

#[async_trait]
impl EventStreamer for NetworkStreamer {
    type DataInType = NoDataIn;

    fn streamer_id(&self) -> StreamerId { StreamerId::Network }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        _data_rx: mpsc::UnboundedReceiver<NoDataIn>,
    ) {
        // The peer-discovery substrate is always attached; signal readiness.
        let _ = ready_tx.send(Ok(()));

        let mut shutdown = core::pin::pin!(shutdown_rx);
        // Last broadcast snapshot; `None` means nothing emitted yet (first cycle always emits).
        let mut prev: Option<Json> = None;

        loop {
            let sleep = Timer::sleep(self.stream_interval_seconds);
            let sleep = core::pin::pin!(sleep);
            match select(sleep, &mut shutdown).await {
                Either::Left(_) => {
                    let snapshot = self.snapshot().await;
                    // Emit-on-change: the first cycle (prev == None) always emits.
                    let changed = prev.as_ref() != Some(&snapshot);
                    if self.always_send || changed {
                        broadcaster.broadcast(Event::new(StreamerId::Network, snapshot.clone()));
                        prev = Some(snapshot);
                    }
                },
                Either::Right(_) => break,
            }
        }
    }
}
