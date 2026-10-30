use crate::request_response::Codec;
use crate::NetworkInfo;
use futures::StreamExt;
use libp2p::core::Endpoint;
use libp2p::{multiaddr::{Multiaddr, Protocol},
             request_response::{Behaviour as RequestResponse, Config as RequestResponseConfig,
                                Event as RequestResponseEvent, InboundFailure, Message as RequestResponseMessage,
                                OutboundFailure, ProtocolSupport},
             swarm::{ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, PollParameters, THandler,
                     THandlerInEvent, THandlerOutEvent, ToSwarm},
             PeerId};
use log::{debug, error, info, trace, warn};
use rand::seq::SliceRandom;
use serde::{de::Deserializer, ser::Serializer, Deserialize, Serialize};
use std::collections::HashSet;
use std::{collections::{HashMap, VecDeque},
          iter,
          task::{Context, Poll},
          time::Duration};
use wasm_timer::{Instant, Interval};

pub type PeerAddresses = HashSet<Multiaddr>;

#[derive(Debug, Clone)]
pub enum PeersExchangeProtocol {
    Version2,
}

impl AsRef<str> for PeersExchangeProtocol {
    fn as_ref(&self) -> &str {
        match self {
            PeersExchangeProtocol::Version2 => "/peers-exchange/2",
        }
    }
}

type PeersExchangeCodec = Codec<PeersExchangeProtocol, PeersExchangeRequest, PeersExchangeResponse>;

const DEFAULT_PEERS_NUM: usize = 20;
const REQUEST_PEERS_INITIAL_DELAY: u64 = 20;
const REQUEST_PEERS_INTERVAL: u64 = 300;
const MAX_PEERS: usize = 100;
/// How many distinct skipped addresses are remembered as already reported (CRD ch.28 R8c item 4).
/// When the set is full it is cleared, so the memory used by it stays bounded.
const MAX_REPORTED_SKIPPED_ADDRESSES: usize = 64;

/// The set of skipped peer addresses that were already reported at INFO level (CRD ch.28 R8c item 4).
#[derive(Debug, Default)]
struct SkippedAddressReports {
    reported: HashSet<Multiaddr>,
}

impl SkippedAddressReports {
    /// Records one sighting of a skipped `address` and returns the level the sighting is logged at: INFO the
    /// first time the address is seen, DEBUG after that. The set is cleared when it is full.
    fn record(&mut self, address: &Multiaddr) -> log::Level {
        if self.reported.contains(address) {
            return log::Level::Debug;
        }
        if self.reported.len() >= MAX_REPORTED_SKIPPED_ADDRESSES {
            self.reported.clear();
        }
        self.reported.insert(address.clone());
        log::Level::Info
    }
}

/// `address` without its trailing `/p2p/<peer id>` component, if it has one.
pub(crate) fn strip_p2p_suffix(address: &Multiaddr) -> Multiaddr {
    let mut stripped = address.clone();
    if matches!(stripped.iter().last(), Some(Protocol::P2p(_))) {
        stripped.pop();
    }
    stripped
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct PeerIdSerde(PeerId);

impl From<PeerId> for PeerIdSerde {
    fn from(peer_id: PeerId) -> PeerIdSerde { PeerIdSerde(peer_id) }
}

impl Serialize for PeerIdSerde {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.to_bytes().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PeerIdSerde {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
        let peer_id = PeerId::from_bytes(&bytes).map_err(|_| serde::de::Error::custom("PeerId::from_bytes error"))?;
        Ok(PeerIdSerde(peer_id))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PeersExchangeRequest {
    GetKnownPeers { num: usize },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PeersExchangeResponse {
    KnownPeers { peers: HashMap<PeerIdSerde, PeerAddresses> },
}

/// Behaviour that requests known peers list from other peers at random
pub struct PeersExchange {
    request_response: RequestResponse<PeersExchangeCodec>,
    known_peers: Vec<PeerId>,
    reserved_peers: Vec<PeerId>,
    events: VecDeque<ToSwarm<(), THandlerInEvent<Self>>>,
    maintain_peers_interval: Interval,
    network_info: NetworkInfo,
    skipped_addresses: SkippedAddressReports,
    /// The peer ID of this node. No entry is ever stored for it (CRD ch.28 R8c item 5).
    local_peer_id: PeerId,
}

#[allow(clippy::new_without_default)]
impl PeersExchange {
    pub fn new(network_info: NetworkInfo, local_peer_id: PeerId) -> Self {
        let codec = Codec::default();
        let protocol = iter::once((PeersExchangeProtocol::Version2, ProtocolSupport::Full));
        let config = RequestResponseConfig::default();
        let request_response = RequestResponse::with_codec(codec, protocol, config);
        PeersExchange {
            request_response,
            known_peers: Vec::new(),
            reserved_peers: Vec::new(),
            events: VecDeque::new(),
            maintain_peers_interval: Interval::new_at(
                Instant::now() + Duration::from_secs(REQUEST_PEERS_INITIAL_DELAY),
                Duration::from_secs(REQUEST_PEERS_INTERVAL),
            ),
            network_info,
            skipped_addresses: SkippedAddressReports::default(),
            local_peer_id,
        }
    }

    fn get_random_known_peers(&mut self, num: usize) -> HashMap<PeerIdSerde, PeerAddresses> {
        let mut result = HashMap::with_capacity(num);
        let mut rng = rand::thread_rng();
        let peer_ids = self
            .known_peers
            .clone()
            .into_iter()
            .filter(|peer| *peer != self.local_peer_id && !self.request_response.addresses_of_peer(peer).is_empty())
            .collect::<Vec<_>>();

        let peer_ids = peer_ids.choose_multiple(&mut rng, num);
        for peer_id in peer_ids {
            let addresses = self.request_response.addresses_of_peer(peer_id).into_iter().collect();
            result.insert((*peer_id).into(), addresses);
        }
        result
    }

    fn forget_peer(&mut self, peer: &PeerId) {
        self.known_peers.retain(|known_peer| known_peer != peer);
        self.forget_peer_addresses(peer);
    }

    fn forget_peer_addresses(&mut self, peer: &PeerId) {
        for address in self.request_response.addresses_of_peer(peer) {
            if !self.is_reserved_peer(peer) {
                self.request_response.remove_address(peer, &address);
            }
        }
    }

    /// Removes `address` from the entry of `peer` after a dial of it reached a peer with a different peer ID (CRD
    /// ch.28 R8c item 6), and forgets `peer` when no address is left. The address matches with or without its
    /// trailing `/p2p/<peer id>` component. Nothing is stored for the peer that answered, and a reserved peer is
    /// kept as configured.
    pub fn remove_stale_address(&mut self, peer: &PeerId, address: &Multiaddr) {
        if self.is_reserved_peer(peer) {
            return;
        }
        let stale = strip_p2p_suffix(address);
        for stored in self.request_response.addresses_of_peer(peer) {
            if strip_p2p_suffix(&stored) == stale {
                self.request_response.remove_address(peer, &stored);
            }
        }
        if self.request_response.addresses_of_peer(peer).is_empty() {
            self.known_peers.retain(|known_peer| known_peer != peer);
        }
    }

    /// Remembers the addresses `peer` announced about itself. Addresses that are not accepted are skipped
    /// (CRD ch.28 R8c); the accepted ones are kept.
    pub fn add_peer_addresses_to_known_peers(&mut self, peer: &PeerId, addresses: PeerAddresses) {
        self.add_received_peer_addresses(peer, peer, addresses);
    }

    /// Keeps the accepted addresses of `entry_peer` received from `sender` (CRD ch.28 R8c items 1 and 2). A
    /// peer entry without any accepted address is skipped, and the `sender` is never forgotten because of it.
    fn add_received_peer_addresses(&mut self, entry_peer: &PeerId, sender: &PeerId, addresses: PeerAddresses) {
        if *entry_peer == self.local_peer_id {
            return;
        }
        let addresses = self.accepted_addresses(sender, addresses);
        if addresses.is_empty() {
            return;
        }
        if !self.known_peers.contains(entry_peer) {
            self.known_peers.push(*entry_peer);
        }
        let already_known = self.request_response.addresses_of_peer(entry_peer);
        for address in addresses {
            if !already_known.contains(&address) {
                self.request_response.add_address(entry_peer, address);
            }
        }
    }

    /// Returns the addresses `validate_global_multiaddr` accepts and reports each skipped one (CRD ch.28 R8c
    /// item 4): INFO the first time the address is seen, DEBUG after that.
    fn accepted_addresses(&mut self, sender: &PeerId, addresses: PeerAddresses) -> PeerAddresses {
        let mut accepted = PeerAddresses::with_capacity(addresses.len());
        for address in addresses {
            if self.validate_global_multiaddr(&address) {
                accepted.insert(address);
                continue;
            }
            let level = self.skipped_addresses.record(&address);
            log::log!(
                level,
                "Peer exchange: skipped address {} from peer {}: only /ip4/<public address>/tcp/{} addresses are accepted",
                address,
                sender,
                self.tcp_port()
            );
        }
        accepted
    }

    /// The TCP port of the network, the only port a received address may carry.
    fn tcp_port(&self) -> u16 {
        match self.network_info {
            NetworkInfo::Distributed { network_ports } => network_ports.tcp,
            NetworkInfo::InMemory => panic!("PeersExchange must not be used with in-memory network"),
        }
    }

    pub fn add_peer_addresses_to_reserved_peers(&mut self, peer: &PeerId, addresses: PeerAddresses) {
        if *peer == self.local_peer_id {
            return;
        }
        for address in addresses.iter() {
            if !self.validate_global_multiaddr(address) {
                return;
            }
        }

        if !self.reserved_peers.contains(peer) && !addresses.is_empty() {
            self.reserved_peers.push(*peer);
        }

        let already_reserved = self.request_response.addresses_of_peer(peer);
        for address in addresses {
            if !already_reserved.contains(&address) {
                self.request_response.add_address(peer, address);
            }
        }
    }

    fn maintain_known_peers(&mut self) {
        if self.known_peers.len() > MAX_PEERS {
            let mut rng = rand::thread_rng();
            let to_remove_num = self.known_peers.len() - MAX_PEERS;
            self.known_peers.shuffle(&mut rng);
            let removed_peers: Vec<_> = self.known_peers.drain(..to_remove_num).collect();
            for peer in removed_peers {
                self.forget_peer_addresses(&peer);
            }
        }
        self.request_known_peers_from_random_peer();
    }

    fn request_known_peers_from_random_peer(&mut self) {
        let mut rng = rand::thread_rng();
        if let Some(from_peer) = self.known_peers.choose(&mut rng) {
            debug!("Requesting up to {} peers from peer {}", DEFAULT_PEERS_NUM, from_peer);
            let request = PeersExchangeRequest::GetKnownPeers { num: DEFAULT_PEERS_NUM };
            self.request_response.send_request(from_peer, request);
        }
    }

    pub fn get_random_peers(
        &mut self,
        num: usize,
        mut filter: impl FnMut(&PeerId) -> bool,
    ) -> HashMap<PeerId, PeerAddresses> {
        let mut result = HashMap::with_capacity(num);
        let mut rng = rand::thread_rng();
        let peer_ids = self
            .known_peers
            .iter()
            .filter(|peer| **peer != self.local_peer_id && filter(peer))
            .collect::<Vec<_>>();
        for peer_id in peer_ids.choose_multiple(&mut rng, num) {
            let addresses = self.request_response.addresses_of_peer(peer_id).into_iter().collect();
            result.insert(**peer_id, addresses);
        }
        result
    }

    pub fn is_known_peer(&self, peer: &PeerId) -> bool { self.known_peers.contains(peer) }

    pub fn is_reserved_peer(&self, peer: &PeerId) -> bool { self.reserved_peers.contains(peer) }

    pub fn add_known_peer(&mut self, peer: PeerId) {
        if peer != self.local_peer_id && !self.is_known_peer(&peer) {
            self.known_peers.push(peer)
        }
    }

    fn validate_global_multiaddr(&self, address: &Multiaddr) -> bool {
        let network_ports = match self.network_info {
            NetworkInfo::Distributed { network_ports } => network_ports,
            NetworkInfo::InMemory => panic!("PeersExchange must not be used with in-memory network"),
        };

        let mut components = address.iter();
        match components.next() {
            Some(Protocol::Ip4(addr)) => {
                if !crate::ip_helpers::ipv4_is_global(&addr) {
                    return false;
                }
            },
            _ => return false,
        }

        match components.next() {
            Some(Protocol::Tcp(port)) => {
                // currently, `NetworkPorts::ws` is not supported by `PeersExchange`
                if port != network_ports.tcp {
                    return false;
                }
            },
            _ => return false,
        }

        true
    }

    /// Handles the peer list `sender` answered with. An answer with more than `DEFAULT_PEERS_NUM` entries is
    /// rejected as a whole: the sender is forgotten and another peer is asked. Otherwise the accepted addresses
    /// are kept and the peer entries without one are skipped (CRD ch.28 R8c). Returns whether the answer was
    /// accepted.
    fn process_known_peers_response(&mut self, sender: &PeerId, peers: HashMap<PeerIdSerde, PeerAddresses>) -> bool {
        if peers.len() > DEFAULT_PEERS_NUM {
            // if peer provides invalid response forget it and try to request from other peer
            self.forget_peer(sender);
            self.request_known_peers_from_random_peer();
            return false;
        }

        for (entry_peer, addresses) in peers {
            self.add_received_peer_addresses(&entry_peer.0, sender, addresses);
        }
        true
    }

    fn process_event(&mut self, event: RequestResponseEvent<PeersExchangeRequest, PeersExchangeResponse>) {
        match event {
            RequestResponseEvent::Message { message, peer } => match message {
                RequestResponseMessage::Request { request, channel, .. } => match request {
                    PeersExchangeRequest::GetKnownPeers { num } => {
                        if num > DEFAULT_PEERS_NUM {
                            info!(
                                "Skipping peers-exchange response to {:?} because requested peer count {} exceeds limit {}",
                                peer, num, DEFAULT_PEERS_NUM
                            );
                            return;
                        }

                        let response = PeersExchangeResponse::KnownPeers {
                            peers: self.get_random_known_peers(num),
                        };
                        if let Err(_response) = self.request_response.send_response(channel, response) {
                            warn!("Response channel has been closed already");
                        }
                    },
                },
                RequestResponseMessage::Response { response, .. } => match response {
                    PeersExchangeResponse::KnownPeers { peers } => {
                        debug!("Received {} peers from {}", peers.len(), peer);
                        trace!("Peers-exchange response from {}: {:?}", peer, peers);

                        self.process_known_peers_response(&peer, peers);
                    },
                },
            },
            RequestResponseEvent::OutboundFailure {
                peer,
                request_id,
                error,
            } => {
                if error == OutboundFailure::UnsupportedProtocols {
                    info!(
                        "Peer {} does not support peers-exchange protocol for request {:?}",
                        peer, request_id
                    );
                } else {
                    match &error {
                        // The swarm reports the attempted address and transport reason separately.
                        // Keep this automatic peers-exchange recovery detail available at debug level.
                        OutboundFailure::DialFailure => debug!(
                            "Peers-exchange dial failed for request {:?} to peer {}; trying another peer",
                            request_id, peer
                        ),
                        _ => warn!(
                            "Peers-exchange request {:?} to peer {} failed with {:?}; trying another peer",
                            request_id, peer, error
                        ),
                    }
                    self.forget_peer(&peer);
                    self.request_known_peers_from_random_peer();
                }
            },
            RequestResponseEvent::InboundFailure { peer, error, .. } => {
                if error == InboundFailure::UnsupportedProtocols {
                    info!("Peer {} requested unsupported peers-exchange protocol", peer);
                } else {
                    error!(
                        "Inbound failure {:?} while processing request from peer {}",
                        error, peer
                    );
                }
            },
            RequestResponseEvent::ResponseSent { .. } => (),
        }
    }
}

impl NetworkBehaviour for PeersExchange {
    type ConnectionHandler = THandler<RequestResponse<PeersExchangeCodec>>;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.request_response
            .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.request_response
            .handle_established_outbound_connection(connection_id, peer, addr, role_override)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.request_response
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.request_response
            .handle_pending_outbound_connection(connection_id, maybe_peer, addresses, effective_role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        self.request_response.on_swarm_event(event)
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.request_response
            .on_connection_handler_event(peer_id, connection_id, event)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        while let Poll::Ready(Some(())) = self.maintain_peers_interval.poll_next_unpin(cx) {
            self.maintain_known_peers();
        }

        // Drive the inner request-response behaviour, processing its generated events locally and
        // forwarding all other swarm actions unchanged.
        loop {
            match self.request_response.poll(cx, params) {
                Poll::Ready(ToSwarm::GenerateEvent(event)) => self.process_event(event),
                Poll::Ready(other) => return Poll::Ready(other.map_out(|_| unreachable!())),
                Poll::Pending => break,
            }
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::{PeerAddresses, PeerIdSerde, PeersExchange, PeersExchangeProtocol};
    use crate::{NetworkInfo, NetworkPorts, PeerId};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn protocol_name_is_version2() {
        assert_eq!(PeersExchangeProtocol::Version2.as_ref(), "/peers-exchange/2");
    }

    #[test]
    fn empty_response_is_accepted() {
        let network_info = NetworkInfo::Distributed {
            network_ports: NetworkPorts { tcp: 3000, wss: 3010 },
        };
        let mut behaviour = PeersExchange::new(network_info, PeerId::random());
        let sender = PeerId::random();
        let response: HashMap<PeerIdSerde, PeerAddresses> = HashMap::new();
        assert!(behaviour.process_known_peers_response(&sender, response));
    }

    #[test]
    fn too_many_entries_are_rejected() {
        let network_info = NetworkInfo::Distributed {
            network_ports: NetworkPorts { tcp: 3000, wss: 3010 },
        };
        let mut behaviour = PeersExchange::new(network_info, PeerId::random());
        let sender = PeerId::random();
        let mut response = HashMap::new();
        for _ in 0..21 {
            response.insert(PeerIdSerde(PeerId::random()), HashSet::new());
        }
        assert!(!behaviour.process_known_peers_response(&sender, response));
    }
}

#[cfg(test)]
mod tests {
    use super::{NetworkInfo, PeerIdSerde, PeersExchange};
    use crate::{NetworkPorts, PeerId};
    use libp2p::core::Multiaddr;
    use std::collections::{HashMap, HashSet};
    use std::iter::FromIterator;

    #[test]
    fn test_peer_id_serde() {
        let peer_id = PeerIdSerde(PeerId::random());
        let serialized = rmp_serde::to_vec(&peer_id).unwrap();
        let deserialized: PeerIdSerde = rmp_serde::from_read_ref(&serialized).unwrap();
        assert_eq!(peer_id.0, deserialized.0);
    }

    fn distributed_behaviour() -> PeersExchange { distributed_behaviour_of(PeerId::random()) }

    fn distributed_behaviour_of(local_peer_id: PeerId) -> PeersExchange {
        PeersExchange::new(
            NetworkInfo::Distributed {
                network_ports: NetworkPorts { tcp: 3000, wss: 3010 },
            },
            local_peer_id,
        )
    }

    fn addresses_of(addresses: &[&str]) -> HashSet<Multiaddr> {
        addresses.iter().map(|address| address.parse().unwrap()).collect()
    }

    #[test]
    fn test_accepted_addresses() {
        let mut behaviour = distributed_behaviour();
        let sender = PeerId::random();
        let mut accepts = |address: &str| {
            let accepted = behaviour.accepted_addresses(&sender, addresses_of(&[address]));
            !accepted.is_empty()
        };

        assert!(!accepts("/ip4/127.0.0.1/tcp/3000"));
        assert!(accepts("/ip4/216.58.210.142/tcp/3000"));
        assert!(!accepts("/ip4/216.58.210.142/tcp/3001"));
        assert!(!accepts("/ip4/216.58.210.142"));
        assert!(!accepts("/dns/seed.example.com/tcp/3000"));
        assert!(!accepts("/dns4/seed.example.com/tcp/3000"));
        assert!(accepts(
            "/ip4/168.119.236.241/tcp/3000/p2p/12D3KooWEsuiKcQaBaKEzuMtT6uFjs89P1E8MK3wGRZbeuCbCw6P"
        ));

        let both = behaviour.accepted_addresses(
            &sender,
            addresses_of(&[
                "/ip4/168.119.236.241/tcp/3000/p2p/12D3KooWEsuiKcQaBaKEzuMtT6uFjs89P1E8MK3wGRZbeuCbCw6P",
                "/ip4/168.119.236.241/tcp/3000",
            ]),
        );
        assert_eq!(both.len(), 2);
    }

    /// CRD ch.28 T6a: a mixed answer keeps the accepted addresses, skips an entry that has none and keeps the sender.
    #[test]
    fn test_mixed_answer_keeps_accepted_addresses_and_sender() {
        let mut behaviour = distributed_behaviour();
        let sender = PeerId::random();
        behaviour.add_known_peer(sender);
        let sender_address: Multiaddr = "/ip4/216.58.210.1/tcp/3000".parse().unwrap();
        behaviour.request_response.add_address(&sender, sender_address.clone());

        let mixed = PeerId::random();
        let only_dns = PeerId::random();
        let only_private = PeerId::random();
        let no_addresses = PeerId::random();
        let good: Multiaddr = "/ip4/216.58.210.142/tcp/3000".parse().unwrap();
        let response = HashMap::from_iter(vec![
            (
                PeerIdSerde(mixed),
                addresses_of(&[
                    "/dns/seed.example.com/tcp/3000",
                    "/ip4/10.0.0.1/tcp/3000",
                    "/ip4/216.58.210.142/tcp/3000",
                ]),
            ),
            (PeerIdSerde(only_dns), addresses_of(&["/dns/seed.example.com/tcp/3000"])),
            (PeerIdSerde(only_private), addresses_of(&["/ip4/127.0.0.1/tcp/3000"])),
            (PeerIdSerde(no_addresses), HashSet::new()),
        ]);

        assert!(behaviour.process_known_peers_response(&sender, response));

        assert!(behaviour.is_known_peer(&sender));
        assert_eq!(behaviour.request_response.addresses_of_peer(&sender), vec![
            sender_address
        ]);
        assert!(behaviour.is_known_peer(&mixed));
        assert_eq!(behaviour.request_response.addresses_of_peer(&mixed), vec![good]);
        for skipped in [only_dns, only_private, no_addresses] {
            assert!(!behaviour.is_known_peer(&skipped));
            assert!(behaviour.request_response.addresses_of_peer(&skipped).is_empty());
        }
    }

    /// CRD ch.28 T6a: an answer with more than 20 entries is rejected as a whole and the sender is forgotten.
    #[test]
    fn test_answer_with_21_entries_is_rejected_and_sender_forgotten() {
        let mut behaviour = distributed_behaviour();
        let sender = PeerId::random();
        behaviour.add_known_peer(sender);
        behaviour
            .request_response
            .add_address(&sender, "/ip4/216.58.210.1/tcp/3000".parse().unwrap());

        let entries: Vec<PeerId> = (0..21).map(|_| PeerId::random()).collect();
        let response = entries
            .iter()
            .map(|peer| (PeerIdSerde(*peer), addresses_of(&["/ip4/216.58.210.142/tcp/3000"])))
            .collect::<HashMap<_, _>>();

        assert!(!behaviour.process_known_peers_response(&sender, response));

        assert!(!behaviour.is_known_peer(&sender));
        assert!(behaviour.request_response.addresses_of_peer(&sender).is_empty());
        assert!(entries.iter().all(|peer| !behaviour.is_known_peer(peer)));

        // An answer with exactly 20 entries is still accepted.
        let response = entries
            .iter()
            .take(20)
            .map(|peer| (PeerIdSerde(*peer), addresses_of(&["/ip4/216.58.210.142/tcp/3000"])))
            .collect::<HashMap<_, _>>();
        assert!(behaviour.process_known_peers_response(&sender, response));
        assert!(entries.iter().take(20).all(|peer| behaviour.is_known_peer(peer)));
    }

    /// CRD ch.28 T6a: a peers-topic announcement keeps its accepted address when another one is skipped.
    #[test]
    fn test_announcement_keeps_accepted_address() {
        let mut behaviour = distributed_behaviour();
        let announcer = PeerId::random();
        let good: Multiaddr = "/ip4/216.58.210.142/tcp/3000".parse().unwrap();

        behaviour.add_peer_addresses_to_known_peers(
            &announcer,
            addresses_of(&["/dns/seed.example.com/tcp/3000", "/ip4/216.58.210.142/tcp/3000"]),
        );
        assert!(behaviour.is_known_peer(&announcer));
        assert_eq!(behaviour.request_response.addresses_of_peer(&announcer), vec![good]);

        // An announcement without an accepted address adds nothing.
        let other = PeerId::random();
        behaviour.add_peer_addresses_to_known_peers(&other, addresses_of(&["/dns/seed.example.com/tcp/3000"]));
        assert!(!behaviour.is_known_peer(&other));
    }

    /// CRD ch.28 T6a: the first sighting of a skipped address is reported at INFO, later ones at DEBUG, and the set
    /// of reported addresses is bounded.
    #[test]
    fn test_skipped_address_reports_are_bounded() {
        use super::{SkippedAddressReports, MAX_REPORTED_SKIPPED_ADDRESSES};
        use log::Level;

        let mut reports = SkippedAddressReports::default();
        let first: Multiaddr = "/dns/a.example.com/tcp/3000".parse().unwrap();
        assert_eq!(reports.record(&first), Level::Info);
        assert_eq!(reports.record(&first), Level::Debug);
        assert_eq!(reports.record(&first), Level::Debug);

        // Fill the set; the first address is still remembered while there is room.
        for i in 1..MAX_REPORTED_SKIPPED_ADDRESSES {
            let address: Multiaddr = format!("/dns/{}.example.com/tcp/3000", i).parse().unwrap();
            assert_eq!(reports.record(&address), Level::Info);
        }
        assert_eq!(reports.reported.len(), MAX_REPORTED_SKIPPED_ADDRESSES);
        assert_eq!(reports.record(&first), Level::Debug);

        // One more distinct address clears the full set, so the memory stays bounded.
        let overflow: Multiaddr = "/dns/overflow.example.com/tcp/3000".parse().unwrap();
        assert_eq!(reports.record(&overflow), Level::Info);
        assert_eq!(reports.reported.len(), 1);
        assert_eq!(reports.record(&overflow), Level::Debug);
        assert_eq!(reports.record(&first), Level::Info);
    }

    /// Both receive paths share one reported-address set.
    #[test]
    fn test_announcement_and_answer_share_reported_set() {
        let mut behaviour = distributed_behaviour();
        let sender = PeerId::random();
        let dns: Multiaddr = "/dns/seed.example.com/tcp/3000".parse().unwrap();

        behaviour.add_peer_addresses_to_known_peers(&sender, addresses_of(&["/dns/seed.example.com/tcp/3000"]));
        assert_eq!(behaviour.skipped_addresses.reported.len(), 1);
        assert_eq!(behaviour.skipped_addresses.record(&dns), log::Level::Debug);

        let response = HashMap::from_iter(vec![(
            PeerIdSerde(PeerId::random()),
            addresses_of(&["/dns/seed.example.com/tcp/3000"]),
        )]);
        assert!(behaviour.process_known_peers_response(&sender, response));
        assert_eq!(behaviour.skipped_addresses.reported.len(), 1);
    }

    /// CRD ch.28 T6a: an answer and an announcement that carry the local peer ID store nothing for it, and it is
    /// never picked as a dial candidate.
    #[test]
    fn test_own_identity_is_never_stored_or_returned() {
        let local = PeerId::random();
        let mut behaviour = distributed_behaviour_of(local);
        let sender = PeerId::random();
        let other = PeerId::random();
        let address = "/ip4/216.58.210.142/tcp/3000";

        // An answer with the local entry and another entry.
        let response = HashMap::from_iter(vec![
            (PeerIdSerde(local), addresses_of(&[address])),
            (PeerIdSerde(other), addresses_of(&[address])),
        ]);
        assert!(behaviour.process_known_peers_response(&sender, response));
        assert!(!behaviour.is_known_peer(&local));
        assert!(behaviour.request_response.addresses_of_peer(&local).is_empty());
        assert!(behaviour.is_known_peer(&other));

        // An announcement, a known-peer registration and a reserved peer, all naming the local ID.
        behaviour.add_peer_addresses_to_known_peers(&local, addresses_of(&[address]));
        behaviour.add_known_peer(local);
        behaviour.add_peer_addresses_to_reserved_peers(&local, addresses_of(&[address]));
        assert!(!behaviour.is_known_peer(&local));
        assert!(!behaviour.is_reserved_peer(&local));
        assert!(behaviour.request_response.addresses_of_peer(&local).is_empty());

        // Even a local entry that got in through the inner behaviour is neither returned nor handed out.
        behaviour.known_peers.push(local);
        behaviour.request_response.add_address(&local, address.parse().unwrap());
        for _ in 0..20 {
            assert!(!behaviour.get_random_peers(10, |_| true).contains_key(&local));
            assert!(!behaviour.get_random_known_peers(10).contains_key(&local.into()));
        }
        assert!(behaviour.get_random_peers(10, |_| true).contains_key(&other));
    }

    /// CRD ch.28 T6a: a dial of A's address that reached another peer removes that address from A's entry, keeps
    /// the other addresses, forgets A with the last one, and stores nothing for the peer that answered.
    #[test]
    fn test_stale_address_is_removed_and_peer_forgotten_with_the_last_one() {
        let mut behaviour = distributed_behaviour();
        let a = PeerId::random();
        let b = PeerId::random();
        let first: Multiaddr = "/ip4/216.58.210.142/tcp/3000".parse().unwrap();
        let second: Multiaddr = "/ip4/216.58.210.143/tcp/3000".parse().unwrap();
        behaviour.add_peer_addresses_to_known_peers(&a, HashSet::from_iter(vec![first.clone(), second.clone()]));
        assert_eq!(behaviour.request_response.addresses_of_peer(&a).len(), 2);

        // The dial endpoint carries the /p2p form of the stored bare address.
        let dialed: Multiaddr = format!("{}/p2p/{}", first, a).parse().unwrap();
        behaviour.remove_stale_address(&a, &dialed);
        assert!(behaviour.is_known_peer(&a));
        assert_eq!(behaviour.request_response.addresses_of_peer(&a), vec![second.clone()]);
        assert!(!behaviour.is_known_peer(&b));
        assert!(behaviour.request_response.addresses_of_peer(&b).is_empty());

        // An address that is not stored changes nothing.
        behaviour.remove_stale_address(&a, &first);
        assert_eq!(behaviour.request_response.addresses_of_peer(&a), vec![second.clone()]);

        // The last address removes the peer.
        behaviour.remove_stale_address(&a, &second);
        assert!(!behaviour.is_known_peer(&a));
        assert!(behaviour.request_response.addresses_of_peer(&a).is_empty());
    }

    /// A stored address in its /p2p form is removed by the bare dial address as well, and a reserved peer is kept.
    #[test]
    fn test_stale_address_matches_p2p_form_and_keeps_reserved_peer() {
        let mut behaviour = distributed_behaviour();
        let a = PeerId::random();
        let stored: Multiaddr = format!("/ip4/216.58.210.142/tcp/3000/p2p/{}", a).parse().unwrap();
        behaviour.add_peer_addresses_to_known_peers(&a, HashSet::from_iter(vec![stored]));
        behaviour.remove_stale_address(&a, &"/ip4/216.58.210.142/tcp/3000".parse().unwrap());
        assert!(!behaviour.is_known_peer(&a));

        let reserved = PeerId::random();
        let reserved_address: Multiaddr = "/ip4/216.58.210.150/tcp/3000".parse().unwrap();
        behaviour.add_peer_addresses_to_reserved_peers(&reserved, HashSet::from_iter(vec![reserved_address.clone()]));
        behaviour.remove_stale_address(&reserved, &reserved_address);
        assert_eq!(behaviour.request_response.addresses_of_peer(&reserved), vec![
            reserved_address
        ]);
    }

    #[test]
    fn test_get_random_known_peers() {
        let mut behaviour = PeersExchange::new(NetworkInfo::InMemory, PeerId::random());
        let peer_id = PeerId::random();
        behaviour.add_known_peer(peer_id);

        let result = behaviour.get_random_known_peers(1);
        assert!(result.is_empty());

        let address: Multiaddr = "/ip4/168.119.236.241/tcp/3000".parse().unwrap();
        behaviour.request_response.add_address(&peer_id, address.clone());

        let result = behaviour.get_random_known_peers(1);
        assert_eq!(result.len(), 1);

        let addresses = result.get(&peer_id.into()).unwrap();
        assert_eq!(addresses.len(), 1);
        assert!(addresses.contains(&address));
    }
}
