use crate::{adex_ping::AdexPing,
            peers_exchange::{strip_p2p_suffix, PeerAddresses, PeersExchange},
            request_response::{build_request_response_behaviour, LocalFailure, PeerRequest, PeerResponse,
                               RequestResponseBehaviour, RequestResponseBehaviourEvent, RequestResponseSender},
            runtime::{SwarmRuntimeOps, SWARM_RUNTIME},
            NetworkInfo, NetworkPorts, RelayAddress, RelayAddressError};
use derive_more::Display;
use futures::{channel::{mpsc::{channel, Receiver, Sender},
                        oneshot},
              future::{abortable, join_all, poll_fn, AbortHandle},
              Future, SinkExt, StreamExt};
use libp2p::core::transport::Boxed as BoxedTransport;
use libp2p::floodsub::{Floodsub, FloodsubEvent, Topic as FloodsubTopic};
use libp2p::gossipsub::{Behaviour as Gossipsub, ConfigBuilder as GossipsubConfigBuilder, Event as GossipsubEvent,
                        IdentTopic as Topic, Message as GossipsubMessage, MessageAuthenticity, MessageId, TopicHash,
                        ValidationMode};
use libp2p::{core::{ConnectedPoint, Multiaddr, Transport},
             identity,
             multiaddr::Protocol,
             noise,
             request_response::ResponseChannel,
             swarm::{DialError, ListenError, NetworkBehaviour, Swarm, SwarmEvent},
             PeerId};
use log::{debug, error, info, warn, Level};
use rand::seq::SliceRandom;
use rand::Rng;
use std::{collections::hash_map::{DefaultHasher, HashMap},
          hash::{Hash, Hasher},
          net::IpAddr,
          task::{Context, Poll},
          time::Duration};
use wasm_timer::{Instant, Interval};

#[cfg(feature = "application")]
use crate::{decode_message, encode_message};
#[cfg(feature = "application")] use futures::FutureExt;
#[cfg(feature = "application")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "application")]
use std::time::{SystemTime, UNIX_EPOCH};

pub type AdexCmdTx = Sender<AdexBehaviourCmd>;
pub type AdexEventRx = Receiver<AdexBehaviourEvent>;

/// The hook used to spawn background futures on the swarm runtime.
type AdexSpawnFn = fn(Box<dyn Future<Output = ()> + Send + Unpin + 'static>) -> ();

#[cfg(test)] mod tests;

/// Content-addressed gossipsub message id (R13): the payload bytes followed by the sequence number are
/// fed through `Hash` into the standard library's default hasher, and the 64-bit result is encoded as
/// exactly 8 bytes, big-endian. The id is a wire value (IHAVE/IWANT), so the preimage and encoding
/// must not change.
fn gossipsub_message_id(data: &[u8], sequence_number: Option<u64>) -> MessageId {
    let mut s = DefaultHasher::new();
    // `Vec<u8>` and `[u8]` hash identically (length prefix, then bytes).
    data.hash(&mut s);
    sequence_number.hash(&mut s);
    MessageId::from(s.finish().to_be_bytes().to_vec())
}

pub const PEERS_TOPIC: &str = "PEERS";
const CONNECTED_RELAYS_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(600);
const ANNOUNCE_INITIAL_DELAY: Duration = Duration::from_secs(60);
const CHANNEL_BUF_SIZE: usize = 1024 * 8;
#[cfg(feature = "application")]
const MAX_ALLOWED_CLOCK_SKEW_SECS: u64 = 20;

#[cfg(feature = "application")]
enum PeerClockCheck {
    Passed,
    Failed,
    Inconclusive,
}

#[cfg(feature = "application")]
#[derive(Deserialize, Serialize)]
enum NetworkInfoRequest {
    GetMm2Version,
    CurrentTimestamp,
}

#[cfg(feature = "application")]
#[derive(Deserialize, Serialize)]
enum WireP2PRequest {
    Ordermatch,
    NetworkInfo(NetworkInfoRequest),
}

/// Returns info about connected peers
pub async fn get_peers_info(mut cmd_tx: AdexCmdTx) -> HashMap<String, Vec<String>> {
    let (result_tx, rx) = oneshot::channel();
    let cmd = AdexBehaviourCmd::GetPeersInfo { result_tx };
    cmd_tx.send(cmd).await.expect("Rx should be present");
    rx.await.expect("Tx should be present")
}

/// Returns current gossipsub mesh state
pub async fn get_gossip_mesh(mut cmd_tx: AdexCmdTx) -> HashMap<String, Vec<String>> {
    let (result_tx, rx) = oneshot::channel();
    let cmd = AdexBehaviourCmd::GetGossipMesh { result_tx };
    cmd_tx.send(cmd).await.expect("Rx should be present");
    rx.await.expect("Tx should be present")
}

pub async fn get_gossip_peer_topics(mut cmd_tx: AdexCmdTx) -> HashMap<String, Vec<String>> {
    let (result_tx, rx) = oneshot::channel();
    let cmd = AdexBehaviourCmd::GetGossipPeerTopics { result_tx };
    cmd_tx.send(cmd).await.expect("Rx should be present");
    rx.await.expect("Tx should be present")
}

pub async fn get_gossip_topic_peers(mut cmd_tx: AdexCmdTx) -> HashMap<String, Vec<String>> {
    let (result_tx, rx) = oneshot::channel();
    let cmd = AdexBehaviourCmd::GetGossipTopicPeers { result_tx };
    cmd_tx.send(cmd).await.expect("Rx should be present");
    rx.await.expect("Tx should be present")
}

pub async fn get_relay_mesh(mut cmd_tx: AdexCmdTx) -> Vec<String> {
    let (result_tx, rx) = oneshot::channel();
    let cmd = AdexBehaviourCmd::GetRelayMesh { result_tx };
    cmd_tx.send(cmd).await.expect("Rx should be present");
    rx.await.expect("Tx should be present")
}

#[derive(Debug)]
pub struct AdexResponseChannel(ResponseChannel<PeerResponse>);

impl From<ResponseChannel<PeerResponse>> for AdexResponseChannel {
    fn from(res: ResponseChannel<PeerResponse>) -> Self { AdexResponseChannel(res) }
}

impl From<AdexResponseChannel> for ResponseChannel<PeerResponse> {
    fn from(res: AdexResponseChannel) -> Self { res.0 }
}

#[derive(Debug)]
pub enum AdexBehaviourCmd {
    Subscribe {
        /// Subscribe to this topic
        topic: String,
    },
    PublishMsg {
        topics: Vec<String>,
        msg: Vec<u8>,
        /// Queue the message in the priority lane of every connection it is written to
        /// (CRD ch.28 §28.9B RQ1 item 9).
        priority: bool,
    },
    PublishMsgFrom {
        topics: Vec<String>,
        msg: Vec<u8>,
        from: PeerId,
        /// Queue the message in the priority lane of every connection it is written to
        /// (CRD ch.28 §28.9B RQ1 item 9).
        priority: bool,
    },
    /// Request relays sequential until a response is received.
    RequestAnyRelay {
        req: Vec<u8>,
        response_tx: oneshot::Sender<Option<(PeerId, Vec<u8>)>>,
    },
    /// Request given peers and collect all their responses.
    RequestPeers {
        req: Vec<u8>,
        peers: Vec<String>,
        response_tx: oneshot::Sender<Vec<(PeerId, AdexResponse)>>,
    },
    /// Request relays and collect all their responses.
    RequestRelays {
        req: Vec<u8>,
        response_tx: oneshot::Sender<Vec<(PeerId, AdexResponse)>>,
    },
    /// Send a response using a `response_channel`.
    SendResponse {
        /// Response to a request.
        res: AdexResponse,
        /// Pass the same `response_channel` as that was obtained from [`AdexBehaviourEvent::PeerRequest`].
        response_channel: AdexResponseChannel,
    },
    GetPeersInfo {
        result_tx: oneshot::Sender<HashMap<String, Vec<String>>>,
    },
    GetGossipMesh {
        result_tx: oneshot::Sender<HashMap<String, Vec<String>>>,
    },
    GetGossipPeerTopics {
        result_tx: oneshot::Sender<HashMap<String, Vec<String>>>,
    },
    GetGossipTopicPeers {
        result_tx: oneshot::Sender<HashMap<String, Vec<String>>>,
    },
    GetRelayMesh {
        result_tx: oneshot::Sender<Vec<String>>,
    },
    /// Add a reserved peer to the peer exchange.
    AddReservedPeer {
        peer: PeerId,
        addresses: PeerAddresses,
    },
    PropagateMessage {
        message_id: MessageId,
        propagation_source: PeerId,
        /// Queue the propagated message in the priority lane of every connection it is written
        /// to (CRD ch.28 §28.9B RQ1 item 9).
        priority: bool,
    },
    /// Mark a cached message as validated without forwarding it (CRD ch.32 R-F3b, D11).
    ValidateMessage {
        message_id: MessageId,
    },
}

/// The structure is the same as `PeerResponse`,
/// but is used to prevent `PeerResponse` from being used outside the network implementation.
#[derive(Debug, Eq, PartialEq)]
pub enum AdexResponse {
    Ok {
        response: Vec<u8>,
    },
    None,
    Err {
        error: String,
    },
    /// A failure of this node's own request (see [`LocalFailure`]); it is never sent to a peer.
    LocalErr {
        kind: LocalFailure,
        error: String,
    },
}

impl From<PeerResponse> for AdexResponse {
    fn from(res: PeerResponse) -> Self {
        match res {
            PeerResponse::Ok { res } => AdexResponse::Ok { response: res },
            PeerResponse::None => AdexResponse::None,
            PeerResponse::Err { err } => AdexResponse::Err { error: err },
            PeerResponse::LocalErr { kind, err } => AdexResponse::LocalErr { kind, error: err },
        }
    }
}

impl From<AdexResponse> for PeerResponse {
    fn from(res: AdexResponse) -> Self {
        match res {
            AdexResponse::Ok { response } => PeerResponse::Ok { res: response },
            AdexResponse::None => PeerResponse::None,
            // Only an answer to a peer's request is converted this way, and a local failure is never one.
            AdexResponse::Err { error } | AdexResponse::LocalErr { error, .. } => PeerResponse::Err { err: error },
        }
    }
}

/// The structure consists of GossipsubEvent and RequestResponse events.
/// It is used to prevent the network events from being used outside the network implementation.
#[derive(Debug)]
pub enum AdexBehaviourEvent {
    /// A message has been received.
    /// Derived from GossipsubEvent.
    Message(PeerId, MessageId, GossipsubMessage),
    /// A remote subscribed to a topic.
    Subscribed {
        /// Remote that has subscribed.
        peer_id: PeerId,
        /// The topic it has subscribed to.
        topic: TopicHash,
    },
    /// A remote unsubscribed from a topic.
    Unsubscribed {
        /// Remote that has unsubscribed.
        peer_id: PeerId,
        /// The topic it has subscribed from.
        topic: TopicHash,
    },
    /// A remote peer sent a request and waits for a response.
    PeerRequest {
        /// Remote that sent this request.
        peer_id: PeerId,
        /// The serialized data.
        request: Vec<u8>,
        /// A channel for sending a response to this request.
        /// The channel is used to identify the peer on the network that is waiting for an answer to this request.
        /// See [`AdexBehaviourCmd::SendResponse`].
        response_channel: AdexResponseChannel,
    },
    /// A connected relay joined this node's relay mesh (CRD ch.32 §32.9B R-MP4). The application
    /// uses it to publish the keep-alive round that follows a reconnect.
    RelayMeshJoined {
        /// The relay that joined the mesh.
        peer_id: PeerId,
    },
    /// The last connection to a relay closed (CRD ch.32 §32.9B R-MP2 item 2, the close signal).
    RelayConnectionClosed {
        /// The relay.
        peer_id: PeerId,
    },
}

/// Aggregate counters of the gossip send queues, for metrics (CRD ch.32 §32.9B R-MP5).
pub use libp2p::gossipsub::SendQueueSnapshot;

/// AtomicDEX libp2p Network behaviour implementation.
///
/// This composed behaviour contains exactly the five sub-behaviours bound by the substrate
/// contract (R6). Application state (the event-forwarding channel, the command receiver, the
/// spawn hook and the pending clock-check map) lives in the swarm driver loop in
/// [`start_gossipsub`] rather than in the behaviour, because the fork's 0.52-era
/// `#[derive(NetworkBehaviour)]` no longer supports ignored non-behaviour fields.
#[derive(NetworkBehaviour)]
pub struct AtomicDexBehaviour {
    gossipsub: Gossipsub,
    floodsub: Floodsub,
    request_response: RequestResponseBehaviour,
    peers_exchange: PeersExchange,
    ping: AdexPing,
}

/// Converts a gossipsub event into the application-facing [`AdexBehaviourEvent`], or `None` for
/// events that carry no application meaning.
fn gossipsub_event_to_adex(event: GossipsubEvent) -> Option<AdexBehaviourEvent> {
    match event {
        GossipsubEvent::Message {
            propagation_source,
            message_id,
            message,
        } => Some(AdexBehaviourEvent::Message(propagation_source, message_id, message)),
        GossipsubEvent::Subscribed { peer_id, topic } => Some(AdexBehaviourEvent::Subscribed { peer_id, topic }),
        GossipsubEvent::Unsubscribed { peer_id, topic } => Some(AdexBehaviourEvent::Unsubscribed { peer_id, topic }),
        GossipsubEvent::GossipsubNotSupported { .. } => None,
        // The pressure level feeds the queue signal inside the behaviour, and the application reads that
        // (`relay_queue_signal`), so it always sees the current level and not a stream of changes it could miss.
        GossipsubEvent::SendQueuePressure {
            peer_id,
            connection_id,
            raised,
        } => {
            debug!(
                "Gossip send queue pressure toward peer {} on connection {:?}: {}",
                peer_id,
                connection_id,
                if raised { "raised" } else { "cleared" }
            );
            None
        },
        GossipsubEvent::RelayMeshJoined { peer_id } => Some(AdexBehaviourEvent::RelayMeshJoined { peer_id }),
        // The queue signal is read from the behaviour (`relay_queue_signal`), so the application always sees the
        // current level.
        GossipsubEvent::SendQueueSignal { .. } => None,
        GossipsubEvent::RelayConnectionClosed { peer_id } => {
            Some(AdexBehaviourEvent::RelayConnectionClosed { peer_id })
        },
    }
}

impl AtomicDexBehaviour {
    #[cfg(feature = "application")]
    fn is_current_timestamp_request(request: &[u8]) -> bool {
        matches!(
            decode_message::<WireP2PRequest>(request),
            Ok(WireP2PRequest::NetworkInfo(NetworkInfoRequest::CurrentTimestamp))
        ) || matches!(
            decode_message::<NetworkInfoRequest>(request),
            Ok(NetworkInfoRequest::CurrentTimestamp)
        )
    }

    #[cfg(feature = "application")]
    fn request_peer_clock_check(
        swarm: &mut AtomicDexSwarm,
        pending_clock_checks: &mut HashMap<PeerId, oneshot::Receiver<PeerResponse>>,
        peer_id: PeerId,
    ) {
        let request = match encode_message(&WireP2PRequest::NetworkInfo(NetworkInfoRequest::CurrentTimestamp)) {
            Ok(req) => req,
            Err(e) => {
                error!("Error serializing clock-check request for peer {}: {}", peer_id, e);
                return;
            },
        };

        let (response_tx, response_rx) = oneshot::channel();
        let request = PeerRequest { req: request };
        swarm
            .behaviour_mut()
            .request_response
            .send_request(&peer_id, request, response_tx);
        pending_clock_checks.insert(peer_id, response_rx);
    }

    #[cfg(feature = "application")]
    fn current_utc_timestamp_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime must be greater than unix epoch")
            .as_secs()
    }

    #[cfg(feature = "application")]
    fn peer_clock_check_result(response: PeerResponse) -> PeerClockCheck {
        let peer_timestamp = match response {
            PeerResponse::Ok { res } => match decode_message::<u64>(&res) {
                Ok(timestamp) => timestamp,
                Err(e) => {
                    error!("Malformed peer timestamp response: {}", e);
                    return PeerClockCheck::Failed;
                },
            },
            PeerResponse::None => return PeerClockCheck::Inconclusive,
            PeerResponse::Err { err } | PeerResponse::LocalErr { err, .. } => {
                debug!("Peer clock check inconclusive: {}", err);
                return PeerClockCheck::Inconclusive;
            },
        };

        let now = Self::current_utc_timestamp_secs();
        let diff = now.abs_diff(peer_timestamp);
        if diff > MAX_ALLOWED_CLOCK_SKEW_SECS {
            error!(
                "Peer clock skew {}s exceeds allowed {}s",
                diff, MAX_ALLOWED_CLOCK_SKEW_SECS
            );
            return PeerClockCheck::Failed;
        }

        PeerClockCheck::Passed
    }

    #[cfg(feature = "application")]
    fn process_pending_clock_checks(
        swarm: &mut AtomicDexSwarm,
        pending_clock_checks: &mut HashMap<PeerId, oneshot::Receiver<PeerResponse>>,
        cx: &mut Context,
    ) {
        let pending_checks = std::mem::take(pending_clock_checks);
        let mut still_pending = HashMap::new();

        for (peer_id, mut response_rx) in pending_checks {
            match response_rx.poll_unpin(cx) {
                Poll::Ready(Ok(response)) => match Self::peer_clock_check_result(response) {
                    PeerClockCheck::Passed => (),
                    PeerClockCheck::Failed => {
                        if Swarm::disconnect_peer_id(swarm, peer_id).is_err() {
                            error!("Peer {} disconnect error after failed clock check", peer_id);
                        }
                    },
                    PeerClockCheck::Inconclusive => {
                        debug!("Keeping peer {} after inconclusive clock check", peer_id);
                    },
                },
                Poll::Ready(Err(_)) => {
                    debug!("Keeping peer {} after missing clock-check response", peer_id);
                },
                Poll::Pending => {
                    still_pending.insert(peer_id, response_rx);
                },
            }
        }

        *pending_clock_checks = still_pending;
    }

    fn notify_on_adex_event(event_tx: &mut Sender<AdexBehaviourEvent>, event: AdexBehaviourEvent) {
        if let Err(e) = event_tx.try_send(event) {
            error!("notify_on_adex_event error {}", e);
        }
    }

    fn process_cmd(swarm: &mut AtomicDexSwarm, spawn_fn: AdexSpawnFn, cmd: AdexBehaviourCmd) {
        match cmd {
            AdexBehaviourCmd::Subscribe { topic } => {
                let topic = Topic::new(topic);
                if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&topic) {
                    error!("Error subscribing to topic {}: {:?}", topic, e);
                }
            },
            AdexBehaviourCmd::PublishMsg { topics, msg, priority } => {
                for topic in topics {
                    if let Err(e) =
                        swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish_with_priority(Topic::new(topic), msg.clone(), priority)
                    {
                        error!("Error publishing message: {:?}", e);
                    }
                }
            },
            AdexBehaviourCmd::PublishMsgFrom {
                topics,
                msg,
                from,
                priority,
            } => {
                for topic in topics {
                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish_from_with_priority(
                        Topic::new(topic),
                        msg.clone(),
                        from,
                        priority,
                    ) {
                        error!("Error publishing message from {}: {:?}", from, e);
                    }
                }
            },
            AdexBehaviourCmd::RequestAnyRelay { req, response_tx } => {
                let relays = swarm.behaviour().gossipsub.get_relay_mesh();
                // spawn the `request_any_peer` future
                let future = request_any_peer(relays, req, swarm.behaviour().request_response.sender(), response_tx);
                spawn_fn(Box::new(Box::pin(future)));
            },
            AdexBehaviourCmd::RequestPeers {
                req,
                peers,
                response_tx,
            } => {
                let peers = peers
                    .into_iter()
                    .filter_map(|peer| match peer.parse() {
                        Ok(p) => Some(p),
                        Err(e) => {
                            error!("Error on parse peer id {:?}: {:?}", peer, e);
                            None
                        },
                    })
                    .collect();
                let future = request_peers(peers, req, swarm.behaviour().request_response.sender(), response_tx);
                spawn_fn(Box::new(Box::pin(future)));
            },
            AdexBehaviourCmd::RequestRelays { req, response_tx } => {
                let relays = swarm.behaviour().gossipsub.get_relay_mesh();
                // spawn the `request_peers` future
                let future = request_peers(relays, req, swarm.behaviour().request_response.sender(), response_tx);
                spawn_fn(Box::new(Box::pin(future)));
            },
            AdexBehaviourCmd::SendResponse { res, response_channel } => {
                if let Err(response) = swarm
                    .behaviour_mut()
                    .request_response
                    .send_response(response_channel.into(), res.into())
                {
                    error!("Error sending response: {:?}", response);
                }
            },
            AdexBehaviourCmd::GetPeersInfo { result_tx } => {
                let result = swarm
                    .behaviour()
                    .gossipsub
                    .get_peers_connections()
                    .into_iter()
                    .map(|(peer_id, connected_points)| {
                        let peer_id = peer_id.to_base58();
                        let connected_points = connected_points
                            .into_iter()
                            .map(|(_conn_id, point)| match point {
                                ConnectedPoint::Dialer { address, .. } => address.to_string(),
                                ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr.to_string(),
                            })
                            .collect();
                        (peer_id, connected_points)
                    })
                    .collect();
                if result_tx.send(result).is_err() {
                    debug!("Result rx is dropped");
                }
            },
            AdexBehaviourCmd::GetGossipMesh { result_tx } => {
                let result = swarm
                    .behaviour()
                    .gossipsub
                    .get_mesh()
                    .iter()
                    .map(|(topic, peers)| {
                        let topic = topic.to_string();
                        let peers = peers.iter().map(|peer| peer.to_string()).collect();
                        (topic, peers)
                    })
                    .collect();
                if result_tx.send(result).is_err() {
                    debug!("Result rx is dropped");
                }
            },
            AdexBehaviourCmd::GetGossipPeerTopics { result_tx } => {
                let result = swarm
                    .behaviour()
                    .gossipsub
                    .get_all_peer_topics()
                    .iter()
                    .map(|(peer, topics)| {
                        let peer = peer.to_string();
                        let topics = topics.iter().map(|topic| topic.to_string()).collect();
                        (peer, topics)
                    })
                    .collect();
                if result_tx.send(result).is_err() {
                    error!("Result rx is dropped");
                }
            },
            AdexBehaviourCmd::GetGossipTopicPeers { result_tx } => {
                let result = swarm
                    .behaviour()
                    .gossipsub
                    .get_all_topic_peers()
                    .iter()
                    .map(|(topic, peers)| {
                        let topic = topic.to_string();
                        let peers = peers.iter().map(|peer| peer.to_string()).collect();
                        (topic, peers)
                    })
                    .collect();
                if result_tx.send(result).is_err() {
                    error!("Result rx is dropped");
                }
            },
            AdexBehaviourCmd::GetRelayMesh { result_tx } => {
                let result = swarm
                    .behaviour()
                    .gossipsub
                    .get_relay_mesh()
                    .into_iter()
                    .map(|peer| peer.to_string())
                    .collect();
                if result_tx.send(result).is_err() {
                    error!("Result rx is dropped");
                }
            },
            AdexBehaviourCmd::AddReservedPeer { peer, addresses } => {
                swarm
                    .behaviour_mut()
                    .peers_exchange
                    .add_peer_addresses_to_reserved_peers(&peer, addresses);
            },
            AdexBehaviourCmd::PropagateMessage {
                message_id,
                propagation_source,
                priority,
            } => {
                if let Err(e) = swarm.behaviour_mut().gossipsub.propagate_message_with_priority(
                    &message_id,
                    &propagation_source,
                    priority,
                ) {
                    error!("Error propagating message {}: {:?}", message_id, e);
                }
            },
            AdexBehaviourCmd::ValidateMessage { message_id } => {
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .validate_message_without_forwarding(&message_id);
            },
        }
    }

    fn announce_listeners(&mut self, listeners: PeerAddresses) {
        let serialized = rmp_serde::to_vec(&listeners).expect("PeerAddresses serialization should never fail");
        self.floodsub.publish(FloodsubTopic::new(PEERS_TOPIC), serialized);
    }

    pub fn connected_relays_len(&self) -> usize { self.gossipsub.connected_relays_len() }

    pub fn relay_mesh_len(&self) -> usize { self.gossipsub.relay_mesh_len() }

    pub fn received_messages_in_period(&self) -> (Duration, usize) { self.gossipsub.get_received_messages_in_period() }

    pub fn connected_peers_len(&self) -> usize { self.gossipsub.get_num_peers() }

    /// The send queue counters of all connections (CRD ch.32 §32.9B R-MP5).
    pub fn send_queue_snapshot(&self) -> SendQueueSnapshot { self.gossipsub.send_queue_snapshot() }

    /// Whether the queue signal of CRD ch.32 §32.9B R-MP2 item 2 is raised toward any connection to a relay: the
    /// data queue has held something for 2 seconds, or the pressure level of CRD ch.28 §28.9B RQ1 item 8 is raised.
    pub fn relay_queue_signal(&self) -> bool { self.gossipsub.relay_queue_signal() }

    /// How many times the queue signal was raised toward a relay connection since start.
    pub fn queue_signals_raised(&self) -> u64 { self.gossipsub.queue_signals_raised() }
}

/// Dispatches an event produced by the composed [`AtomicDexBehaviour`] to the application layer.
///
/// This replaces the pre-0.52 `NetworkBehaviourEventProcess` impls: the derive macro no longer
/// drives event handling, so the swarm loop invokes this function for every
/// `SwarmEvent::Behaviour`.
fn handle_behaviour_event(
    swarm: &mut AtomicDexSwarm,
    event_tx: &mut Sender<AdexBehaviourEvent>,
    event: AtomicDexBehaviourEvent,
) {
    match event {
        AtomicDexBehaviourEvent::Gossipsub(event) => {
            if let Some(adex_event) = gossipsub_event_to_adex(event) {
                AtomicDexBehaviour::notify_on_adex_event(event_tx, adex_event);
            }
        },
        AtomicDexBehaviourEvent::Floodsub(FloodsubEvent::Message(message)) => {
            for topic in &message.topics {
                if topic == &FloodsubTopic::new(PEERS_TOPIC) {
                    let addresses: PeerAddresses = match rmp_serde::from_read_ref(&message.data) {
                        Ok(a) => a,
                        Err(_) => return,
                    };
                    swarm
                        .behaviour_mut()
                        .peers_exchange
                        .add_peer_addresses_to_known_peers(&message.source, addresses);
                }
            }
        },
        AtomicDexBehaviourEvent::Floodsub(_) => {},
        AtomicDexBehaviourEvent::RequestResponse(RequestResponseBehaviourEvent::InboundRequest {
            peer_id,
            request,
            response_channel,
        }) => {
            #[cfg(feature = "application")]
            if AtomicDexBehaviour::is_current_timestamp_request(&request.req) {
                let response = match encode_message(&AtomicDexBehaviour::current_utc_timestamp_secs()) {
                    Ok(now) => PeerResponse::Ok { res: now },
                    Err(e) => PeerResponse::Err {
                        err: format!("Error serializing current timestamp: {}", e),
                    },
                };

                if let Err(response) = swarm
                    .behaviour_mut()
                    .request_response
                    .send_response(response_channel, response)
                {
                    error!("Error sending timestamp response: {:?}", response);
                }
                return;
            }

            let event = AdexBehaviourEvent::PeerRequest {
                peer_id,
                request: request.req,
                response_channel: response_channel.into(),
            };
            // forward the event to the AdexBehaviourCmd handler
            AtomicDexBehaviour::notify_on_adex_event(event_tx, event);
        },
        AtomicDexBehaviourEvent::PeersExchange(_) => {},
        AtomicDexBehaviourEvent::Ping(_) => {},
    }
}

/// Custom types mapping the complex associated types of AtomicDexBehaviour to the ExpandedSwarm
type AtomicDexSwarm = Swarm<AtomicDexBehaviour>;

fn maintain_connection_to_relays(swarm: &mut AtomicDexSwarm, bootstrap_addresses: &[Multiaddr]) {
    let behaviour = swarm.behaviour();
    let connected_relays = behaviour.gossipsub.connected_relays();
    let mesh_n_low = behaviour.gossipsub.get_config().mesh_n_low();
    let mesh_n = behaviour.gossipsub.get_config().mesh_n();
    // allow 2 * mesh_n_high connections to other nodes
    let max_n = behaviour.gossipsub.get_config().mesh_n_high() * 2;

    let local_peer_id = *Swarm::local_peer_id(swarm);

    let mut rng = rand::thread_rng();
    if connected_relays.len() < mesh_n_low {
        let to_connect_num = mesh_n - connected_relays.len();
        // The node never dials itself (CRD ch.28 R8c item 5): peers exchange does not hold its own entry, and the
        // candidate filter excludes it as well.
        let to_connect = swarm
            .behaviour_mut()
            .peers_exchange
            .get_random_peers(to_connect_num, |peer| {
                *peer != local_peer_id && !connected_relays.contains(peer)
            });

        // choose some random bootstrap addresses to connect if peers exchange returned not enough peers
        if to_connect.len() < to_connect_num {
            let connect_bootstrap_num = to_connect_num - to_connect.len();
            let available_bootstrap = bootstrap_addresses
                .iter()
                .filter(|addr| !swarm.behaviour().gossipsub.is_connected_to_addr(addr))
                .collect::<Vec<_>>();

            if to_connect.is_empty() && available_bootstrap.is_empty() {
                warn!(
                    "P2P relays below low watermark: connected {}, need {}; no known peers or seednodes available to dial",
                    connected_relays.len(),
                    mesh_n_low
                );
            } else if available_bootstrap.is_empty() {
                debug!(
                    "P2P relays below low watermark: connected {}, need {}; peer exchange returned {}, no seednodes available",
                    connected_relays.len(),
                    mesh_n_low,
                    to_connect.len()
                );
            }

            let selected_bootstrap: Vec<Multiaddr> = available_bootstrap
                .choose_multiple(&mut rng, connect_bootstrap_num)
                .map(|addr| (*addr).clone())
                .collect();
            for addr in selected_bootstrap {
                dial_bootstrap_addr(swarm, addr, "relay maintenance");
            }
        }
        for (peer, addresses) in to_connect {
            for addr in addresses {
                if swarm.behaviour().gossipsub.is_connected_to_addr(&addr) {
                    continue;
                }
                dial_peer_addr(swarm, peer, addr, "peer exchange relay maintenance");
            }
        }
    }

    if connected_relays.len() > max_n {
        let to_disconnect_num = connected_relays.len() - max_n;
        let relays_mesh = swarm.behaviour().gossipsub.get_relay_mesh();
        let not_in_mesh: Vec<_> = connected_relays
            .iter()
            .filter(|peer| !relays_mesh.contains(peer))
            .collect();
        for peer in not_in_mesh.choose_multiple(&mut rng, to_disconnect_num) {
            if !swarm.behaviour().peers_exchange.is_reserved_peer(peer) {
                info!("Disconnecting peer {}", peer);
                if Swarm::disconnect_peer_id(swarm, **peer).is_err() {
                    error!("Peer {} disconnect error", peer);
                }
            }
        }
    }

    for relay in connected_relays {
        if !swarm.behaviour().peers_exchange.is_known_peer(&relay) {
            swarm.behaviour_mut().peers_exchange.add_known_peer(relay);
        }
    }
}

fn announce_my_addresses(swarm: &mut AtomicDexSwarm) {
    let global_listeners: PeerAddresses = Swarm::listeners(swarm)
        .filter(|listener| {
            for protocol in listener.iter() {
                if let Protocol::Ip4(ip) = protocol {
                    return crate::ip_helpers::ipv4_is_global(&ip);
                }
            }
            false
        })
        .take(1)
        .cloned()
        .collect();
    if !global_listeners.is_empty() {
        swarm.behaviour_mut().announce_listeners(global_listeners);
    }
}

#[derive(Debug, Display)]
pub enum AdexBehaviourError {
    #[display(fmt = "{}", _0)]
    ParsingRelayAddress(RelayAddressError),
    #[display(fmt = "Error listening on '{}': {}", address, error)]
    ListenOn { address: String, error: String },
    #[display(fmt = "Invalid WSS key or certificate configuration: {}", _0)]
    WssConfig(String),
}

impl From<RelayAddressError> for AdexBehaviourError {
    fn from(e: RelayAddressError) -> Self { AdexBehaviourError::ParsingRelayAddress(e) }
}

fn listen_on_addr(swarm: &mut AtomicDexSwarm, addr: Multiaddr) -> Result<(), AdexBehaviourError> {
    match Swarm::listen_on(swarm, addr.clone()) {
        Ok(listener_id) => {
            info!("P2P listener {:?} scheduled on {}", listener_id, addr);
            Ok(())
        },
        Err(e) => {
            error!("Failed to start P2P listener on {}: {}", addr, e);
            Err(AdexBehaviourError::ListenOn {
                address: addr.to_string(),
                error: e.to_string(),
            })
        },
    }
}

fn dial_bootstrap_addr(swarm: &mut AtomicDexSwarm, addr: Multiaddr, reason: &str) {
    match Swarm::dial(swarm, addr.clone()) {
        Ok(_) => info!("Dialed {} ({})", addr, reason),
        Err(e) => error!("P2P bootstrap dial scheduling failed for {} ({}): {}", addr, reason, e),
    }
}

fn dial_peer_addr(swarm: &mut AtomicDexSwarm, peer: PeerId, addr: Multiaddr, reason: &str) {
    match Swarm::dial(swarm, addr.clone()) {
        Ok(_) => info!("Dialed peer {} at {} ({})", peer, addr, reason),
        Err(e) => error!(
            "P2P peer dial scheduling failed for peer {} at {} ({}): {}",
            peer, addr, reason, e
        ),
    }
}

/// How often an address or a source is reported at the high log level (INFO or WARN) when its connections keep
/// failing; the reports in between are logged at DEBUG.
const CONNECTION_FAILURE_LOG_INTERVAL: Duration = Duration::from_secs(3600);
/// How many addresses or sources are remembered. The map is cleared when it is full, so its memory stays bounded.
const MAX_CONNECTION_FAILURE_LOG_KEYS: usize = 256;

/// The time of the last high-level report per key.
struct ConnectionFailureLog<K> {
    last_high: HashMap<K, Instant>,
}

impl<K> Default for ConnectionFailureLog<K> {
    fn default() -> Self {
        ConnectionFailureLog {
            last_high: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq> ConnectionFailureLog<K> {
    /// The level an event naming `keys` is logged at at `now`: `high` when any key was not reported at the high
    /// level within [`CONNECTION_FAILURE_LOG_INTERVAL`] (the keys that were not are marked as reported), DEBUG
    /// otherwise.
    fn level(&mut self, keys: impl IntoIterator<Item = K>, now: Instant, high: Level) -> Level {
        let mut level = Level::Debug;
        for key in keys {
            let reported_recently = self
                .last_high
                .get(&key)
                .is_some_and(|last| now.duration_since(*last) < CONNECTION_FAILURE_LOG_INTERVAL);
            if reported_recently {
                continue;
            }
            if self.last_high.len() >= MAX_CONNECTION_FAILURE_LOG_KEYS && !self.last_high.contains_key(&key) {
                self.last_high.clear();
            }
            self.last_high.insert(key, now);
            level = high;
        }
        level
    }
}

/// What an outgoing connection failure is keyed by.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DialFailureKey {
    /// An address the error names, without its trailing `/p2p/<peer id>` component.
    Address(Multiaddr),
    /// The peer the dial was for, when the error names no address.
    Peer(PeerId),
    /// The error names neither an address nor a peer.
    Unknown,
}

/// The keys of an outgoing connection failure: every address the error names, or the peer ID when it names none.
fn dial_failure_keys(peer_id: Option<PeerId>, error: &DialError) -> Vec<DialFailureKey> {
    let mut keys: Vec<DialFailureKey> = match error {
        DialError::Transport(failures) => failures
            .iter()
            .map(|(address, _)| DialFailureKey::Address(strip_p2p_suffix(address)))
            .collect(),
        DialError::WrongPeerId { endpoint, .. } | DialError::LocalPeerId { endpoint } => {
            vec![DialFailureKey::Address(strip_p2p_suffix(endpoint.get_remote_address()))]
        },
        _ => Vec::new(),
    };
    if keys.is_empty() {
        keys.push(peer_id.map_or(DialFailureKey::Unknown, DialFailureKey::Peer));
    }
    keys
}

/// The source IP of an incoming connection, if its address carries one.
fn source_ip(address: &Multiaddr) -> Option<IpAddr> {
    address.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(ip) => Some(IpAddr::V4(ip)),
        Protocol::Ip6(ip) => Some(IpAddr::V6(ip)),
        _ => None,
    })
}

/// Rate limits of the connection failure log lines (CRD leaves the logging to the implementation).
#[derive(Default)]
struct ConnectionFailureLogs {
    outgoing: ConnectionFailureLog<DialFailureKey>,
    incoming: ConnectionFailureLog<Option<IpAddr>>,
}

impl ConnectionFailureLogs {
    /// An outgoing connection failure is logged at WARN at most once per address per hour, at DEBUG otherwise.
    fn outgoing_level(&mut self, peer_id: Option<PeerId>, error: &DialError, now: Instant) -> Level {
        self.outgoing.level(dial_failure_keys(peer_id, error), now, Level::Warn)
    }

    /// An incoming transport negotiation failure is logged at INFO at most once per source IP per hour, at DEBUG
    /// otherwise. Other incoming failures stay at WARN.
    fn incoming_level(&mut self, send_back_addr: &Multiaddr, error: &ListenError, now: Instant) -> Level {
        match error {
            ListenError::Transport(_) => self.incoming.level([source_ip(send_back_addr)], now, Level::Info),
            _ => Level::Warn,
        }
    }
}

fn log_swarm_event<TBehaviourOutEvent, THandlerErr>(
    event: &SwarmEvent<TBehaviourOutEvent, THandlerErr>,
    failure_logs: &mut ConnectionFailureLogs,
) where
    TBehaviourOutEvent: std::fmt::Debug,
    THandlerErr: std::fmt::Debug,
{
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id,
            endpoint,
            num_established,
            concurrent_dial_errors,
            ..
        } => {
            info!(
                "P2P connection established with peer {} via {:?}; total connections to peer: {}",
                peer_id, endpoint, num_established
            );
            if let Some(errors) = concurrent_dial_errors {
                for (addr, error) in errors {
                    warn!(
                        "P2P concurrent dial attempt to peer {} at {} failed before successful connection: {}",
                        peer_id, addr, error
                    );
                }
            }
        },
        SwarmEvent::ConnectionClosed {
            peer_id,
            endpoint,
            num_established,
            cause,
            ..
        } => match cause {
            Some(cause) => warn!(
                "P2P connection to peer {} via {:?} closed with error: {:?}; remaining connections to peer: {}",
                peer_id, endpoint, cause, num_established
            ),
            None => info!(
                "P2P connection to peer {} via {:?} closed cleanly; remaining connections to peer: {}",
                peer_id, endpoint, num_established
            ),
        },
        SwarmEvent::IncomingConnection {
            local_addr,
            send_back_addr,
            ..
        } => debug!(
            "P2P incoming connection attempt on {} from {}",
            local_addr, send_back_addr
        ),
        SwarmEvent::IncomingConnectionError {
            local_addr,
            send_back_addr,
            error,
            ..
        } => {
            let level = failure_logs.incoming_level(send_back_addr, error, Instant::now());
            log::log!(
                level,
                "P2P incoming connection failed on {} from {}: {}",
                local_addr,
                send_back_addr,
                error
            );
        },
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            let level = failure_logs.outgoing_level(*peer_id, error, Instant::now());
            log::log!(
                level,
                "P2P outgoing connection failed for peer {:?}: {}",
                peer_id,
                error
            );
        },
        SwarmEvent::NewListenAddr { listener_id, address } => {
            info!("P2P listener {:?} is listening on {}", listener_id, address);
        },
        SwarmEvent::ExpiredListenAddr { listener_id, address } => {
            info!("P2P listener {:?} address expired: {}", listener_id, address);
        },
        SwarmEvent::ListenerClosed {
            listener_id,
            addresses,
            reason,
        } => match reason {
            Ok(()) => info!(
                "P2P listener {:?} closed cleanly; addresses: {:?}",
                listener_id, addresses
            ),
            Err(e) => warn!(
                "P2P listener {:?} closed with error {}; addresses: {:?}",
                listener_id, e, addresses
            ),
        },
        SwarmEvent::ListenerError { listener_id, error } => {
            warn!("P2P listener {:?} reported non-fatal error: {}", listener_id, error);
        },
        SwarmEvent::Dialing { peer_id, .. } => {
            debug!("P2P dialing peer {:?}", peer_id);
        },
        other => debug!("Swarm event {:?}", other),
    }
}

/// DER-encoded WSS server key and certificate chain.
pub struct WssCerts {
    pub server_priv_key: Vec<u8>,
    pub certs: Vec<Vec<u8>>,
}

pub enum NodeType {
    Light {
        network_ports: NetworkPorts,
    },
    LightInMemory,
    Relay {
        ip: IpAddr,
        network_ports: NetworkPorts,
        wss_certs: Option<WssCerts>,
    },
    RelayInMemory {
        port: u64,
    },
}

impl NodeType {
    pub fn to_network_info(&self) -> NetworkInfo {
        match self {
            NodeType::Light { network_ports } | NodeType::Relay { network_ports, .. } => NetworkInfo::Distributed {
                network_ports: *network_ports,
            },
            NodeType::LightInMemory | NodeType::RelayInMemory { .. } => NetworkInfo::InMemory,
        }
    }

    pub fn is_relay(&self) -> bool { matches!(self, NodeType::Relay { .. } | NodeType::RelayInMemory { .. }) }

    pub fn wss_certs(&self) -> Option<&WssCerts> {
        match self {
            NodeType::Relay { wss_certs, .. } => wss_certs.as_ref(),
            _ => None,
        }
    }
}

/// Creates and spawns new AdexBehaviour Swarm returning:
/// 1. tx to send control commands
/// 2. rx emitting gossip events to processing side
/// 3. our peer_id
/// 4. abort handle to stop the P2P processing fut.
pub async fn spawn_gossipsub(
    force_key: Option<[u8; 32]>,
    spawn_fn: fn(Box<dyn Future<Output = ()> + Send + Unpin + 'static>) -> (),
    to_dial: Vec<RelayAddress>,
    node_type: NodeType,
    on_poll: impl Fn(&AtomicDexSwarm) + Send + 'static,
) -> Result<(Sender<AdexBehaviourCmd>, AdexEventRx, PeerId, AbortHandle), AdexBehaviourError> {
    let (result_tx, result_rx) = futures::channel::oneshot::channel();
    let fut = async move {
        let result = start_gossipsub(force_key, spawn_fn, to_dial, node_type, on_poll);
        result_tx.send(result).unwrap();
    };

    // `Libp2p` must be spawned on the tokio runtime
    SWARM_RUNTIME.spawn(fut);
    result_rx.await.expect("Fatal error on starting gossipsub")
}

/// Creates and spawns new AdexBehaviour Swarm returning:
/// 1. tx to send control commands
/// 2. rx emitting gossip events to processing side
/// 3. our peer_id
/// 4. abort handle to stop the P2P processing fut
///
/// Prefer using [`spawn_gossipsub`] to make sure the Swarm is initialized and spawned on the same runtime.
/// Otherwise, you can face the following error:
/// `panicked at 'there is no reactor running, must be called from the context of a Tokio 1.x runtime'`.
#[allow(clippy::too_many_arguments)]
fn start_gossipsub(
    force_key: Option<[u8; 32]>,
    spawn_fn: fn(Box<dyn Future<Output = ()> + Send + Unpin + 'static>) -> (),
    to_dial: Vec<RelayAddress>,
    node_type: NodeType,
    on_poll: impl Fn(&AtomicDexSwarm) + Send + 'static,
) -> Result<(Sender<AdexBehaviourCmd>, AdexEventRx, PeerId, AbortHandle), AdexBehaviourError> {
    let i_am_relay = node_type.is_relay();
    let mut rng = rand::thread_rng();
    let local_key = generate_ed25519_keypair(&mut rng, force_key);
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {:?}", local_peer_id);

    let network_info = node_type.to_network_info();
    let transport = match network_info {
        NetworkInfo::InMemory => build_memory_transport(&local_key),
        NetworkInfo::Distributed { .. } => build_dns_ws_transport(&local_key, node_type.wss_certs())?,
    };

    let (cmd_tx, mut cmd_rx) = channel(CHANNEL_BUF_SIZE);
    let (mut event_tx, event_rx) = channel(CHANNEL_BUF_SIZE);
    #[cfg(feature = "application")]
    let mut pending_clock_checks: HashMap<PeerId, oneshot::Receiver<PeerResponse>> = HashMap::new();

    let bootstrap = to_dial
        .into_iter()
        .map(|addr| addr.try_to_multiaddr(network_info))
        .collect::<Result<Vec<Multiaddr>, _>>()?;

    // Mesh-size watermarks per role (R13): relay `(4, 8, 12)`, client `(2, 4, 6)`.
    let (mesh_n_low, mesh_n, mesh_n_high) = if i_am_relay { (4, 8, 12) } else { (2, 4, 6) };
    // The fork's config builder rejects triples where `mesh_outbound_min > mesh_n_low` or
    // `mesh_outbound_min * 2 > mesh_n`, and its default panics for the small client mesh, so it is
    // set explicitly to `mesh_n_low / 2`.
    let mesh_outbound_min = mesh_n_low / 2;

    // Create a Swarm to manage peers and events
    let mut swarm = {
        // To content-address a message, take the hash of its payload plus sequence number and use
        // it as the message id, so duplicate payloads collapse to a single id (R13).
        let message_id_fn = |message: &GossipsubMessage| gossipsub_message_id(&message.data, message.sequence_number);

        // set custom gossipsub
        let gossipsub_config = GossipsubConfigBuilder::default()
            .message_id_fn(message_id_fn)
            .i_am_relay(i_am_relay)
            .mesh_n_low(mesh_n_low)
            .mesh_n(mesh_n)
            .mesh_n_high(mesh_n_high)
            .mesh_outbound_min(mesh_outbound_min)
            // Manual message propagation (R13): the application validates a message and then calls
            // the propagate-message command before it is forwarded.
            .validate_messages()
            // Author authenticity keeps a per-message sequence number without libp2p-level signing;
            // application payloads are signed separately (R16). `Permissive` validation accepts such
            // authored-but-unsigned messages.
            .validation_mode(ValidationMode::Permissive)
            .max_transmit_size(1024 * 1024 - 100)
            .build()
            .expect("valid gossipsub config");
        // build a gossipsub network behaviour
        let gossipsub = Gossipsub::new(MessageAuthenticity::Author(local_peer_id), gossipsub_config)
            .expect("initialising gossipsub behaviour");

        let floodsub = Floodsub::new(local_peer_id, true);

        let peers_exchange = PeersExchange::new(network_info, local_peer_id);

        // build a request-response network behaviour
        let request_response = build_request_response_behaviour();

        // ping wrapper that force-disconnects a peer after consecutive failures
        let ping = AdexPing::new();

        let adex_behavior = AtomicDexBehaviour {
            gossipsub,
            floodsub,
            request_response,
            peers_exchange,
            ping,
        };
        libp2p::swarm::SwarmBuilder::with_executor(transport, adex_behavior, local_peer_id, &*SWARM_RUNTIME).build()
    };
    swarm
        .behaviour_mut()
        .floodsub
        .subscribe(FloodsubTopic::new(PEERS_TOPIC.to_owned()));

    match node_type {
        NodeType::Relay {
            ip,
            network_ports,
            wss_certs,
        } => {
            let dns_addr: Multiaddr = format!("/ip4/{}/tcp/{}", ip, network_ports.tcp).parse().unwrap();
            listen_on_addr(&mut swarm, dns_addr)?;
            if wss_certs.is_some() {
                let wss_addr: Multiaddr = format!("/ip4/{}/tcp/{}/wss", ip, network_ports.wss).parse().unwrap();
                listen_on_addr(&mut swarm, wss_addr)?;
            }
        },
        NodeType::RelayInMemory { port } => {
            let memory_addr: Multiaddr = format!("/memory/{}", port).parse().unwrap();
            listen_on_addr(&mut swarm, memory_addr)?;
        },
        _ => (),
    }

    for relay in bootstrap.choose_multiple(&mut rng, mesh_n) {
        dial_bootstrap_addr(&mut swarm, relay.clone(), "initial bootstrap");
    }

    let mut check_connected_relays_interval = Interval::new_at(
        Instant::now() + CONNECTED_RELAYS_CHECK_INTERVAL,
        CONNECTED_RELAYS_CHECK_INTERVAL,
    );
    let mut announce_interval = Interval::new_at(Instant::now() + ANNOUNCE_INITIAL_DELAY, ANNOUNCE_INTERVAL);
    let mut listening = false;
    let mut failure_logs = ConnectionFailureLogs::default();
    let polling_fut = poll_fn(move |cx: &mut Context| {
        loop {
            match cmd_rx.poll_next_unpin(cx) {
                Poll::Ready(Some(cmd)) => AtomicDexBehaviour::process_cmd(&mut swarm, spawn_fn, cmd),
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        loop {
            match swarm.poll_next_unpin(cx) {
                Poll::Ready(Some(event)) => {
                    log_swarm_event(&event, &mut failure_logs);
                    match event {
                        SwarmEvent::Behaviour(behaviour_event) => {
                            handle_behaviour_event(&mut swarm, &mut event_tx, behaviour_event)
                        },
                        #[cfg(feature = "application")]
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            AtomicDexBehaviour::request_peer_clock_check(
                                &mut swarm,
                                &mut pending_clock_checks,
                                peer_id,
                            );
                        },
                        // A dial of an address stored for `peer` reached another peer: the stored address is stale
                        // (CRD ch.28 R8c item 6).
                        SwarmEvent::OutgoingConnectionError {
                            peer_id: Some(peer),
                            error: DialError::WrongPeerId { endpoint, .. },
                            ..
                        } => {
                            debug!(
                                "Removing the stale address {} of peer {} from peers exchange",
                                endpoint.get_remote_address(),
                                peer
                            );
                            swarm
                                .behaviour_mut()
                                .peers_exchange
                                .remove_stale_address(&peer, endpoint.get_remote_address());
                        },
                        _ => {},
                    }
                },
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        #[cfg(feature = "application")]
        AtomicDexBehaviour::process_pending_clock_checks(&mut swarm, &mut pending_clock_checks, cx);

        if swarm.behaviour().gossipsub.is_relay() {
            while let Poll::Ready(Some(())) = announce_interval.poll_next_unpin(cx) {
                announce_my_addresses(&mut swarm);
            }
        }

        while let Poll::Ready(Some(())) = check_connected_relays_interval.poll_next_unpin(cx) {
            maintain_connection_to_relays(&mut swarm, &bootstrap);
        }

        if !listening && i_am_relay {
            for listener in Swarm::listeners(&swarm) {
                info!("Listening on {}", listener);
                listening = true;
            }
        }
        on_poll(&swarm);
        Poll::Pending
    });

    let (polling_fut, abort_handle) = abortable(polling_fut);
    SWARM_RUNTIME.spawn(polling_fut);

    Ok((cmd_tx, event_rx, local_peer_id, abort_handle))
}

#[cfg(target_arch = "wasm32")]
fn build_dns_ws_transport(
    keypair: &identity::Keypair,
    _wss_certs: Option<&WssCerts>,
) -> Result<BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)>, AdexBehaviourError> {
    let websocket = libp2p::wasm_ext::ffi::websocket_transport();
    let transport = libp2p::wasm_ext::ExtTransport::new(websocket);
    Ok(upgrade_transport(transport, keypair))
}

#[cfg(not(target_arch = "wasm32"))]
fn build_dns_ws_transport(
    keypair: &identity::Keypair,
    wss_certs: Option<&WssCerts>,
) -> Result<BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)>, AdexBehaviourError> {
    use libp2p::websocket::tls as libp2p_tls;

    // The DNS-over-TCP transport is not `Clone` in the 0.52 API, so a fresh instance is built for
    // the plain and the WebSocket paths.
    let new_dns_tcp = || {
        let tcp = libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::new().nodelay(true));
        libp2p::dns::TokioDnsConfig::custom(tcp, libp2p::dns::ResolverConfig::google(), Default::default())
            .expect("Building the DNS-over-TCP transport should never fail")
    };

    let dns_tcp = new_dns_tcp();
    let mut ws_dns_tcp = libp2p::websocket::WsConfig::new(new_dns_tcp());

    if let Some(certs) = wss_certs {
        let server_priv_key = libp2p_tls::PrivateKey::new(certs.server_priv_key.clone())
            .map_err(|e| AdexBehaviourError::WssConfig(e.to_string()))?;
        let certs = certs
            .certs
            .iter()
            .map(|cert| libp2p_tls::Certificate::new(cert.clone()));
        let wss_config = libp2p_tls::Config::new(server_priv_key, certs)
            .map_err(|e| AdexBehaviourError::WssConfig(e.to_string()))?;
        ws_dns_tcp.set_tls_config(wss_config);
    }

    let transport = dns_tcp.or_transport(ws_dns_tcp);
    Ok(upgrade_transport(transport, keypair))
}

fn build_memory_transport(
    keypair: &identity::Keypair,
) -> BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)> {
    let transport = libp2p::core::transport::MemoryTransport::default();
    upgrade_transport(transport, keypair)
}

/// Set up an encrypted Transport over the Yamux protocol.
///
/// The connection upgrade pipeline is uniform across targets (R20): multistream-select V1, Noise
/// XX security, yamux stream multiplexing, and a 20-second upgrade timeout.
fn upgrade_transport<T>(
    transport: T,
    keypair: &identity::Keypair,
) -> BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)>
where
    T: Transport + Send + Sync + Unpin + 'static,
    T::Output: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
    T::ListenerUpgrade: Send,
    T::Dial: Send,
    T::Error: Send + Sync + 'static,
{
    transport
        .upgrade(libp2p::core::upgrade::Version::V1)
        .authenticate(noise::Config::new(keypair).expect("Signing the noise static DH keypair failed"))
        .multiplex(libp2p::yamux::Config::default())
        .timeout(Duration::from_secs(20))
        .map(|(peer, muxer), _| (peer, libp2p::core::muxing::StreamMuxerBox::new(muxer)))
        .boxed()
}

fn generate_ed25519_keypair<R: Rng>(rng: &mut R, force_key: Option<[u8; 32]>) -> identity::Keypair {
    let mut raw_key = match force_key {
        Some(key) => key,
        None => {
            let mut key = [0; 32];
            rng.fill_bytes(&mut key);
            key
        },
    };
    identity::Keypair::ed25519_from_bytes(&mut raw_key).expect("Secret length is 32 bytes")
}

/// Request the peers sequential until a `PeerResponse::Ok()` will not be received.
async fn request_any_peer(
    peers: Vec<PeerId>,
    request_data: Vec<u8>,
    request_response_tx: RequestResponseSender,
    response_tx: oneshot::Sender<Option<(PeerId, Vec<u8>)>>,
) {
    debug!("start request_any_peer loop: peers {}", peers.len());
    for peer in peers {
        match request_one_peer(peer, request_data.clone(), request_response_tx.clone()).await {
            PeerResponse::Ok { res } => {
                debug!("Received a response from peer {:?}, stop the request loop", peer);
                if response_tx.send(Some((peer, res))).is_err() {
                    error!("Response oneshot channel was closed");
                }
                return;
            },
            PeerResponse::None => {
                debug!("Received None from peer {:?}, request next peer", peer);
            },
            PeerResponse::Err { err } | PeerResponse::LocalErr { err, .. } => {
                error!("Error on request {:?} peer: {:?}. Request next peer", peer, err);
            },
        };
    }

    debug!("None of the peers responded to the request");
    if response_tx.send(None).is_err() {
        error!("Response oneshot channel was closed");
    };
}

/// Request the peers and collect all their responses.
async fn request_peers(
    peers: Vec<PeerId>,
    request_data: Vec<u8>,
    request_response_tx: RequestResponseSender,
    response_tx: oneshot::Sender<Vec<(PeerId, AdexResponse)>>,
) {
    debug!("start request_any_peer loop: peers {}", peers.len());
    let mut futures = Vec::with_capacity(peers.len());
    for peer in peers {
        let request_data = request_data.clone();
        let request_response_tx = request_response_tx.clone();
        futures.push(async move {
            let response = request_one_peer(peer, request_data, request_response_tx).await;
            (peer, response)
        })
    }

    let responses = join_all(futures)
        .await
        .into_iter()
        .map(|(peer_id, res)| {
            let res: AdexResponse = res.into();
            (peer_id, res)
        })
        .collect();

    if response_tx.send(responses).is_err() {
        error!("Response oneshot channel was closed");
    };
}

async fn request_one_peer(peer: PeerId, req: Vec<u8>, mut request_response_tx: RequestResponseSender) -> PeerResponse {
    // Use the internal receiver to receive a response to this request.
    let (internal_response_tx, internal_response_rx) = oneshot::channel();
    let request = PeerRequest { req };
    request_response_tx
        .send((peer, request, internal_response_tx))
        .await
        .unwrap();

    match internal_response_rx.await {
        Ok(response) => response,
        // The pending request was dropped without an answer: the local timeout expired.
        Err(e) => PeerResponse::LocalErr {
            kind: LocalFailure::Timeout,
            err: format!("Error on request the peer {:?}: \"{:?}\". Request next peer", peer, e),
        },
    }
}

#[cfg(all(test, feature = "application"))]
mod application_tests {
    use super::{AtomicDexBehaviour, NetworkInfoRequest, PeerClockCheck, PeerResponse, WireP2PRequest};
    use crate::encode_message;

    #[test]
    fn is_peer_clock_check_passed_accepts_small_diff() {
        let now = AtomicDexBehaviour::current_utc_timestamp_secs();
        let encoded = encode_message(&(now.saturating_sub(1))).unwrap();
        let response = PeerResponse::Ok { res: encoded };
        assert!(matches!(
            AtomicDexBehaviour::peer_clock_check_result(response),
            PeerClockCheck::Passed
        ));
    }

    #[test]
    fn is_peer_clock_check_passed_rejects_malformed_payload() {
        let response = PeerResponse::Ok {
            res: vec![1_u8, 2_u8, 3_u8],
        };
        assert!(matches!(
            AtomicDexBehaviour::peer_clock_check_result(response),
            PeerClockCheck::Failed
        ));
    }

    #[test]
    fn peer_clock_check_inconclusive_on_protocol_failure() {
        assert!(matches!(
            AtomicDexBehaviour::peer_clock_check_result(PeerResponse::None),
            PeerClockCheck::Inconclusive
        ));
        assert!(matches!(
            AtomicDexBehaviour::peer_clock_check_result(PeerResponse::Err {
                err: "unsupported".into()
            }),
            PeerClockCheck::Inconclusive
        ));
    }

    #[test]
    fn network_info_request_roundtrip() {
        let encoded = encode_message(&NetworkInfoRequest::CurrentTimestamp).unwrap();
        let decoded: NetworkInfoRequest = crate::decode_message(&encoded).unwrap();
        assert!(matches!(decoded, NetworkInfoRequest::CurrentTimestamp));
    }

    #[test]
    fn wire_p2p_request_roundtrip() {
        let encoded = encode_message(&WireP2PRequest::NetworkInfo(NetworkInfoRequest::CurrentTimestamp)).unwrap();
        let decoded: WireP2PRequest = crate::decode_message(&encoded).unwrap();
        assert!(matches!(
            decoded,
            WireP2PRequest::NetworkInfo(NetworkInfoRequest::CurrentTimestamp)
        ));
    }

    #[test]
    fn detects_timestamp_in_both_wire_shapes() {
        let wrapped = encode_message(&WireP2PRequest::NetworkInfo(NetworkInfoRequest::CurrentTimestamp)).unwrap();
        let bare = encode_message(&NetworkInfoRequest::CurrentTimestamp).unwrap();

        assert!(AtomicDexBehaviour::is_current_timestamp_request(&wrapped));
        assert!(AtomicDexBehaviour::is_current_timestamp_request(&bare));
    }
}

#[cfg(test)]
mod connection_failure_log_tests {
    use super::{dial_failure_keys, ConnectionFailureLog, ConnectionFailureLogs, DialFailureKey,
                CONNECTION_FAILURE_LOG_INTERVAL, MAX_CONNECTION_FAILURE_LOG_KEYS};
    use libp2p::core::transport::TransportError;
    use libp2p::core::{ConnectedPoint, Endpoint, Multiaddr};
    use libp2p::swarm::{DialError, ListenError};
    use libp2p::PeerId;
    use log::Level;
    use std::time::Duration;
    use wasm_timer::Instant;

    fn transport_error(addresses: &[&str]) -> DialError {
        DialError::Transport(
            addresses
                .iter()
                .map(|address| {
                    (
                        address.parse::<Multiaddr>().unwrap(),
                        TransportError::Other(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out")),
                    )
                })
                .collect(),
        )
    }

    fn incoming_transport_error() -> ListenError {
        ListenError::Transport(TransportError::Other(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negotiation failed",
        )))
    }

    /// An outgoing failure is logged at WARN the first time, at DEBUG within the hour, and at WARN again after it.
    #[test]
    fn outgoing_failure_is_warned_once_per_address_per_hour() {
        let mut logs = ConnectionFailureLogs::default();
        let error = transport_error(&["/ip4/203.0.113.7/tcp/42845"]);
        let start = Instant::now();

        assert_eq!(logs.outgoing_level(None, &error, start), Level::Warn);
        assert_eq!(
            logs.outgoing_level(None, &error, start + Duration::from_secs(30)),
            Level::Debug
        );
        assert_eq!(
            logs.outgoing_level(
                None,
                &error,
                start + CONNECTION_FAILURE_LOG_INTERVAL - Duration::from_secs(1)
            ),
            Level::Debug
        );
        assert_eq!(
            logs.outgoing_level(None, &error, start + CONNECTION_FAILURE_LOG_INTERVAL),
            Level::Warn
        );
        // The hour counts from the last high-level report.
        assert_eq!(
            logs.outgoing_level(
                None,
                &error,
                start + CONNECTION_FAILURE_LOG_INTERVAL + Duration::from_secs(60)
            ),
            Level::Debug
        );

        // Another address has its own hour. An event that names a new address among known ones is high.
        let other = transport_error(&["/ip4/203.0.113.8/tcp/42845"]);
        assert_eq!(
            logs.outgoing_level(None, &other, start + Duration::from_secs(40)),
            Level::Warn
        );
        let both = transport_error(&["/ip4/203.0.113.7/tcp/42845", "/ip4/203.0.113.9/tcp/42845"]);
        assert_eq!(
            logs.outgoing_level(
                None,
                &both,
                start + CONNECTION_FAILURE_LOG_INTERVAL + Duration::from_secs(70)
            ),
            Level::Warn
        );
    }

    /// The bare and the /p2p form of an address are one key, and an error without an address is keyed by the peer.
    #[test]
    fn outgoing_failure_keys() {
        let peer = PeerId::random();
        let bare = transport_error(&["/ip4/203.0.113.7/tcp/42845"]);
        let with_peer = transport_error(&[format!("/ip4/203.0.113.7/tcp/42845/p2p/{}", peer).as_str()]);
        assert_eq!(
            dial_failure_keys(None, &bare),
            dial_failure_keys(Some(peer), &with_peer)
        );

        assert_eq!(dial_failure_keys(Some(peer), &DialError::Aborted), vec![
            DialFailureKey::Peer(peer)
        ]);
        assert_eq!(dial_failure_keys(None, &DialError::NoAddresses), vec![
            DialFailureKey::Unknown
        ]);

        let endpoint = ConnectedPoint::Dialer {
            address: format!("/ip4/203.0.113.7/tcp/42845/p2p/{}", peer).parse().unwrap(),
            role_override: Endpoint::Dialer,
        };
        let wrong = DialError::WrongPeerId {
            obtained: PeerId::random(),
            endpoint,
        };
        assert_eq!(dial_failure_keys(Some(peer), &wrong), dial_failure_keys(None, &bare));
    }

    /// An incoming transport negotiation failure is logged at INFO once per source IP per hour; other incoming
    /// failures stay at WARN.
    #[test]
    fn incoming_transport_failure_is_reported_once_per_source_ip_per_hour() {
        let mut logs = ConnectionFailureLogs::default();
        let error = incoming_transport_error();
        let from = |address: &str| address.parse::<Multiaddr>().unwrap();
        let start = Instant::now();

        assert_eq!(
            logs.incoming_level(&from("/ip4/198.51.100.5/tcp/50001"), &error, start),
            Level::Info
        );
        // The same IP from another port is the same source.
        assert_eq!(
            logs.incoming_level(
                &from("/ip4/198.51.100.5/tcp/50002"),
                &error,
                start + Duration::from_secs(5)
            ),
            Level::Debug
        );
        assert_eq!(
            logs.incoming_level(
                &from("/ip4/198.51.100.6/tcp/50001"),
                &error,
                start + Duration::from_secs(6)
            ),
            Level::Info
        );
        assert_eq!(
            logs.incoming_level(
                &from("/ip4/198.51.100.5/tcp/50003"),
                &error,
                start + CONNECTION_FAILURE_LOG_INTERVAL
            ),
            Level::Info
        );

        // Other incoming failures are not limited.
        for _ in 0..3 {
            assert_eq!(
                logs.incoming_level(&from("/ip4/198.51.100.5/tcp/50001"), &ListenError::Aborted, start),
                Level::Warn
            );
        }
    }

    /// The remembered keys stay bounded: a full map is cleared.
    #[test]
    fn remembered_keys_stay_bounded() {
        let mut log = ConnectionFailureLog::<u32>::default();
        let start = Instant::now();
        for key in 0..MAX_CONNECTION_FAILURE_LOG_KEYS as u32 {
            assert_eq!(log.level([key], start, Level::Warn), Level::Warn);
        }
        assert_eq!(log.last_high.len(), MAX_CONNECTION_FAILURE_LOG_KEYS);
        assert_eq!(log.level([0], start, Level::Warn), Level::Debug);

        // One more distinct key clears the full map.
        assert_eq!(log.level([u32::MAX], start, Level::Warn), Level::Warn);
        assert_eq!(log.last_high.len(), 1);
        assert_eq!(log.level([0], start, Level::Warn), Level::Warn);

        for key in 1_000..1_000 + 10 * MAX_CONNECTION_FAILURE_LOG_KEYS as u32 {
            log.level([key], start, Level::Warn);
            assert!(log.last_high.len() <= MAX_CONNECTION_FAILURE_LOG_KEYS);
        }
    }
}
