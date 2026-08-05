use crate::{adex_ping::AdexPing,
            peers_exchange::{PeerAddresses, PeersExchange},
            request_response::{build_request_response_behaviour, PeerRequest, PeerResponse, RequestResponseBehaviour,
                               RequestResponseBehaviourEvent, RequestResponseSender},
            runtime::{SwarmRuntimeOps, SWARM_RUNTIME},
            NetworkInfo, NetworkPorts, RelayAddress, RelayAddressError};
use derive_more::Display;
use futures::{channel::{mpsc::{channel, Receiver, Sender},
                        oneshot},
              future::{abortable, join_all, poll_fn, AbortHandle},
              Future, SinkExt, StreamExt};
use futures_rustls::rustls;
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
             swarm::{NetworkBehaviour, Swarm, SwarmEvent},
             PeerId};
use log::{debug, error, info, warn};
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
    },
    PublishMsgFrom {
        topics: Vec<String>,
        msg: Vec<u8>,
        from: PeerId,
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
    },
}

/// The structure is the same as `PeerResponse`,
/// but is used to prevent `PeerResponse` from being used outside the network implementation.
#[derive(Debug, Eq, PartialEq)]
pub enum AdexResponse {
    Ok { response: Vec<u8> },
    None,
    Err { error: String },
}

impl From<PeerResponse> for AdexResponse {
    fn from(res: PeerResponse) -> Self {
        match res {
            PeerResponse::Ok { res } => AdexResponse::Ok { response: res },
            PeerResponse::None => AdexResponse::None,
            PeerResponse::Err { err } => AdexResponse::Err { error: err },
        }
    }
}

impl From<AdexResponse> for PeerResponse {
    fn from(res: AdexResponse) -> Self {
        match res {
            AdexResponse::Ok { response } => PeerResponse::Ok { res: response },
            AdexResponse::None => PeerResponse::None,
            AdexResponse::Err { error } => PeerResponse::Err { err: error },
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
}

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
            PeerResponse::Err { err } => {
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
            AdexBehaviourCmd::PublishMsg { topics, msg } => {
                for topic in topics {
                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(Topic::new(topic), msg.clone()) {
                        error!("Error publishing message: {:?}", e);
                    }
                }
            },
            AdexBehaviourCmd::PublishMsgFrom { topics, msg, from } => {
                for topic in topics {
                    if let Err(e) = swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish_from(Topic::new(topic), msg.clone(), from)
                    {
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
            } => {
                if let Err(e) = swarm
                    .behaviour_mut()
                    .gossipsub
                    .propagate_message(&message_id, &propagation_source)
                {
                    error!("Error propagating message {}: {:?}", message_id, e);
                }
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

    let mut rng = rand::thread_rng();
    if connected_relays.len() < mesh_n_low {
        let to_connect_num = mesh_n - connected_relays.len();
        let to_connect = swarm
            .behaviour_mut()
            .peers_exchange
            .get_random_peers(to_connect_num, |peer| !connected_relays.contains(peer));

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

fn log_swarm_event<TBehaviourOutEvent, THandlerErr>(event: &SwarmEvent<TBehaviourOutEvent, THandlerErr>)
where
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
        } => warn!(
            "P2P incoming connection failed on {} from {}: {}",
            local_addr, send_back_addr, error
        ),
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            warn!("P2P outgoing connection failed for peer {:?}: {}", peer_id, error);
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

pub struct WssCerts {
    pub server_priv_key: rustls::PrivateKey,
    pub certs: Vec<rustls::Certificate>,
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
        NetworkInfo::Distributed { .. } => build_dns_ws_transport(&local_key, node_type.wss_certs()),
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
        let message_id_fn = |message: &GossipsubMessage| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            message.sequence_number.hash(&mut s);
            MessageId::from(s.finish().to_string())
        };

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

        let peers_exchange = PeersExchange::new(network_info);

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
                    log_swarm_event(&event);
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
) -> BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)> {
    let websocket = libp2p::wasm_ext::ffi::websocket_transport();
    let transport = libp2p::wasm_ext::ExtTransport::new(websocket);
    upgrade_transport(transport, keypair)
}

#[cfg(not(target_arch = "wasm32"))]
fn build_dns_ws_transport(
    keypair: &identity::Keypair,
    wss_certs: Option<&WssCerts>,
) -> BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)> {
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
        let server_priv_key = libp2p_tls::PrivateKey::new(certs.server_priv_key.0.clone());
        let certs = certs
            .certs
            .iter()
            .map(|cert| libp2p_tls::Certificate::new(cert.0.clone()));
        let wss_config = libp2p_tls::Config::new(server_priv_key, certs).unwrap();
        ws_dns_tcp.set_tls_config(wss_config);
    }

    let transport = dns_tcp.or_transport(ws_dns_tcp);
    upgrade_transport(transport, keypair)
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
            PeerResponse::Err { err } => {
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
        Err(e) => PeerResponse::Err {
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
