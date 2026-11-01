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

/// Lookups of the hostnames in peer-exchange answers (CRD ch.28 R8c item 7, maintainer decision BH). Native builds
/// only: a browser build skips every hostname.
///
/// A name goes through a bounded queue, and one lookup at a time runs on a blocking worker thread. While nothing is
/// queued and nothing runs, no timer, worker or task handle exists.
#[cfg(not(target_arch = "wasm32"))]
mod hostname_lookup {
    use super::PeerAddresses;
    use futures::FutureExt;
    use libp2p::{multiaddr::Multiaddr, PeerId};
    use std::collections::VecDeque;
    use std::io;
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::task::JoinHandle;
    use wasm_timer::{Delay, Instant};

    /// How many names wait for a lookup (item 7.2).
    pub(super) const MAX_QUEUED_NAMES: usize = 64;
    /// How many names the per-name record holds (item 7.5).
    pub(super) const MAX_NAME_RECORDS: usize = 64;
    /// The least time between two lookups of the same name (item 7.1).
    const RELOOKUP_INTERVAL: Duration = Duration::from_secs(30 * 60);
    /// A lookup that has not returned after this time stores nothing (item 7.2).
    const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

    /// The blocking name lookup. It is a trait so that a test can drive the results and their timing.
    pub(super) trait HostResolver: Send + Sync + 'static {
        fn lookup(&self, name: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
    }

    /// The operating system's resolver.
    struct SystemResolver;

    impl HostResolver for SystemResolver {
        fn lookup(&self, name: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            (name, port).to_socket_addrs().map(|addresses| addresses.collect())
        }
    }

    /// One name to look up, as received in an answer.
    #[derive(Clone, Debug)]
    pub(super) struct LookupJob {
        pub(super) name: String,
        /// The port of the received address, which is the network's TCP port.
        pub(super) port: u16,
        /// The peer the answer named the address for.
        pub(super) entry_peer: PeerId,
        /// The peer that sent the answer.
        pub(super) sender: PeerId,
        /// The address as received, the key of the skip report (item 7.6).
        pub(super) address: Multiaddr,
    }

    pub(super) enum LookupResult {
        Resolved(Vec<SocketAddr>),
        Failed(String),
        TimedOut,
    }

    pub(super) struct FinishedLookup {
        pub(super) job: LookupJob,
        pub(super) result: LookupResult,
    }

    /// What the node remembers about one name (item 7.5). It is never handed out.
    pub(super) struct NameRecord {
        pub(super) entry_peer: PeerId,
        pub(super) last_lookup: Instant,
        /// The addresses the lookups of the name stored, and no other.
        pub(super) stored: PeerAddresses,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub(super) enum Enqueued {
        Queued,
        /// The node is connected to the entry's peer (item 7.1).
        Connected,
        /// The name is queued or running already.
        AlreadyQueued,
        /// The name was looked up less than 30 minutes ago (item 7.1).
        TooSoon,
        /// The queue holds 64 names (item 7.2).
        QueueFull,
    }

    struct RunningLookup {
        job: LookupJob,
        handle: JoinHandle<io::Result<Vec<SocketAddr>>>,
        /// `None` once the 5 s have passed: the result is given up, but the worker thread may still be busy.
        timeout: Option<Delay>,
    }

    pub(super) struct HostnameLookups {
        pub(super) resolver: Arc<dyn HostResolver>,
        pub(super) timeout: Duration,
        pub(super) relookup_interval: Duration,
        pub(super) queue: VecDeque<LookupJob>,
        running: Option<RunningLookup>,
        /// The oldest record first.
        pub(super) records: VecDeque<(String, NameRecord)>,
    }

    impl Default for HostnameLookups {
        fn default() -> Self {
            HostnameLookups {
                resolver: Arc::new(SystemResolver),
                timeout: LOOKUP_TIMEOUT,
                relookup_interval: RELOOKUP_INTERVAL,
                queue: VecDeque::new(),
                running: None,
                records: VecDeque::new(),
            }
        }
    }

    impl HostnameLookups {
        /// Whether no lookup is queued or running, so that no timer, worker or task handle is pending.
        #[cfg(test)]
        pub(super) fn is_idle(&self) -> bool { self.queue.is_empty() && self.running.is_none() }

        pub(super) fn record(&self, name: &str) -> Option<&NameRecord> {
            self.records
                .iter()
                .find_map(|(recorded, record)| (recorded == name).then_some(record))
        }

        /// Queues `job` unless a rule of item 7.1 or 7.2 forbids it. `connected` tells whether the node is connected
        /// to the entry's peer.
        pub(super) fn enqueue(&mut self, job: LookupJob, connected: bool, now: Instant) -> Enqueued {
            if connected {
                return Enqueued::Connected;
            }
            let running = self.running.as_ref().map(|running| &running.job);
            if self.queue.iter().chain(running).any(|queued| queued.name == job.name) {
                return Enqueued::AlreadyQueued;
            }
            if let Some(record) = self.record(&job.name) {
                if now.duration_since(record.last_lookup) < self.relookup_interval {
                    return Enqueued::TooSoon;
                }
            }
            if self.queue.len() >= MAX_QUEUED_NAMES {
                return Enqueued::QueueFull;
            }
            self.queue.push_back(job);
            Enqueued::Queued
        }

        /// Notes the start of a lookup of `name`: its record gets the time, and it becomes the newest one. When the
        /// record is full, the oldest entry is dropped.
        fn touch_record(&mut self, name: &str, entry_peer: PeerId, now: Instant) {
            let mut record = match self.records.iter().position(|(recorded, _)| recorded == name) {
                Some(index) => self.records.remove(index).expect("index is in bounds").1,
                None => NameRecord {
                    entry_peer,
                    last_lookup: now,
                    stored: PeerAddresses::new(),
                },
            };
            record.last_lookup = now;
            if self.records.len() >= MAX_NAME_RECORDS {
                self.records.pop_front();
            }
            self.records.push_back((name.to_owned(), record));
        }

        /// Sets the peer and the stored addresses of the record of `name` after a lookup returned.
        pub(super) fn set_stored(&mut self, name: &str, entry_peer: PeerId, stored: PeerAddresses) {
            if let Some((_, record)) = self.records.iter_mut().find(|(recorded, _)| recorded == name) {
                record.entry_peer = entry_peer;
                record.stored = stored;
            }
        }

        /// Drives the running lookup and starts the next queued one when the previous one has returned, even if its
        /// result was given up. At most one worker thread exists at any time. Returns the lookups that finished.
        pub(super) fn poll(&mut self, cx: &mut Context, is_connected: impl Fn(&PeerId) -> bool) -> Vec<FinishedLookup> {
            let mut finished = Vec::new();
            loop {
                if let Some(running) = self.running.as_mut() {
                    if let Poll::Ready(joined) = running.handle.poll_unpin(cx) {
                        let running = self.running.take().expect("the lookup is running");
                        // A result that was given up at the timeout was reported then.
                        if running.timeout.is_some() {
                            let result = match joined {
                                Ok(Ok(addresses)) => LookupResult::Resolved(addresses),
                                Ok(Err(error)) => LookupResult::Failed(error.to_string()),
                                Err(error) => LookupResult::Failed(format!("the lookup task failed: {}", error)),
                            };
                            finished.push(FinishedLookup {
                                job: running.job,
                                result,
                            });
                        }
                        continue;
                    }
                    if let Some(timeout) = running.timeout.as_mut() {
                        if timeout.poll_unpin(cx).is_ready() {
                            running.timeout = None;
                            finished.push(FinishedLookup {
                                job: running.job.clone(),
                                result: LookupResult::TimedOut,
                            });
                        }
                    }
                    return finished;
                }

                let job = match self.queue.pop_front() {
                    Some(job) => job,
                    None => return finished,
                };
                // The connection may have come up while the name waited (item 7.1).
                if is_connected(&job.entry_peer) {
                    continue;
                }
                self.touch_record(&job.name, job.entry_peer, Instant::now());
                let resolver = self.resolver.clone();
                let (name, port) = (job.name.clone(), job.port);
                self.running = Some(RunningLookup {
                    job,
                    // The system resolver blocks, so it runs on a blocking worker thread of the swarm's runtime and
                    // never on the swarm's own task.
                    handle: tokio::task::spawn_blocking(move || resolver.lookup(&name, port)),
                    timeout: Some(Delay::new(self.timeout)),
                });
            }
        }
    }
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
    /// `p2p_resolve_peer_hostnames` (CRD ch.28 R8c item 10, decision BH): look up the hostnames of an answer (item 7)
    /// and keep the answers this node gives to the accepted form (item 9).
    resolve_hostnames: bool,
    /// `p2p_allow_private_addresses` (CRD ch.28 R8c item 8, decision BH): the routability check is skipped.
    allow_private_addresses: bool,
    /// The queue, the running lookup and the per-name record of CRD ch.28 R8c item 7 (decision BH).
    #[cfg(not(target_arch = "wasm32"))]
    lookups: hostname_lookup::HostnameLookups,
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
            resolve_hostnames: true,
            allow_private_addresses: false,
            #[cfg(not(target_arch = "wasm32"))]
            lookups: hostname_lookup::HostnameLookups::default(),
        }
    }

    /// Sets the two configuration flags of CRD ch.28 R8c items 8 and 10 (decision BH). `PeersExchange::new` starts
    /// with their defaults: `resolve_hostnames` `true` and `allow_private_addresses` `false`.
    pub fn with_address_settings(mut self, resolve_hostnames: bool, allow_private_addresses: bool) -> Self {
        self.resolve_hostnames = resolve_hostnames;
        self.allow_private_addresses = allow_private_addresses;
        self
    }

    /// The answer this node gives (CRD ch.28 R8c item 9, decision BH). With `resolve_hostnames` set, an entry carries
    /// only addresses in the accepted form, so the dialed hostname of a live connection is left out, and an entry
    /// left with no address is not chosen. Without it, an entry carries every address of the peer as before.
    fn get_random_known_peers(&mut self, num: usize) -> HashMap<PeerIdSerde, PeerAddresses> {
        let mut result = HashMap::with_capacity(num);
        let mut rng = rand::thread_rng();
        let mut candidates = Vec::new();
        for peer in self.known_peers.iter().filter(|peer| **peer != self.local_peer_id) {
            let mut addresses: PeerAddresses = self.request_response.addresses_of_peer(peer).into_iter().collect();
            if self.resolve_hostnames {
                addresses.retain(|address| self.validate_global_multiaddr(address));
            }
            if !addresses.is_empty() {
                candidates.push((*peer, addresses));
            }
        }

        for (peer_id, addresses) in candidates.choose_multiple(&mut rng, num) {
            result.insert((*peer_id).into(), addresses.clone());
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
        self.store_accepted_addresses(entry_peer, addresses);
    }

    /// Stores addresses that passed `validate_global_multiaddr` under `entry_peer`, which becomes a known peer.
    /// Nothing is stored for this node's own peer ID (CRD ch.28 R8c item 5).
    fn store_accepted_addresses(&mut self, entry_peer: &PeerId, addresses: PeerAddresses) {
        if *entry_peer == self.local_peer_id || addresses.is_empty() {
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
                // `p2p_allow_private_addresses` skips only this check, not the port check (CRD ch.28 R8c item 8).
                if !self.allow_private_addresses && !crate::ip_helpers::ipv4_is_global(&addr) {
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
    /// are kept and the peer entries without one are skipped (CRD ch.28 R8c). The hostnames of the answer are
    /// queued for a lookup after its accepted addresses were processed (R8c item 7, decision BH). Returns whether
    /// the answer was accepted.
    fn process_known_peers_response(&mut self, sender: &PeerId, peers: HashMap<PeerIdSerde, PeerAddresses>) -> bool {
        if peers.len() > DEFAULT_PEERS_NUM {
            // if peer provides invalid response forget it and try to request from other peer
            self.forget_peer(sender);
            self.request_known_peers_from_random_peer();
            return false;
        }

        #[cfg(not(target_arch = "wasm32"))]
        let mut lookups = Vec::new();
        for (entry_peer, addresses) in peers {
            #[cfg(not(target_arch = "wasm32"))]
            let addresses = {
                let mut addresses = addresses;
                lookups.extend(self.take_hostnames_to_look_up(&entry_peer.0, sender, &mut addresses));
                addresses
            };
            self.add_received_peer_addresses(&entry_peer.0, sender, addresses);
        }
        #[cfg(not(target_arch = "wasm32"))]
        for job in lookups {
            self.queue_hostname_lookup(job);
        }
        true
    }

    /// Removes from `addresses`, the addresses of one peer entry of an answer, those that are looked up instead of
    /// skipped (CRD ch.28 R8c item 7, decision BH): `/dns/<name>/tcp/<port>` and `/dns4/<name>/tcp/<port>` with the
    /// network's TCP port, and returns them as lookups. Nothing is taken for this node's own peer ID, or when
    /// `p2p_resolve_peer_hostnames` is `false`. `/dns6`, `/dnsaddr` and another port stay in `addresses` and are
    /// skipped as before.
    #[cfg(not(target_arch = "wasm32"))]
    fn take_hostnames_to_look_up(
        &self,
        entry_peer: &PeerId,
        sender: &PeerId,
        addresses: &mut PeerAddresses,
    ) -> Vec<hostname_lookup::LookupJob> {
        if !self.resolve_hostnames || *entry_peer == self.local_peer_id {
            return Vec::new();
        }
        let mut jobs = Vec::new();
        addresses.retain(|address| {
            let mut components = address.iter();
            let name = match components.next() {
                Some(Protocol::Dns(name)) | Some(Protocol::Dns4(name)) => name.into_owned(),
                _ => return true,
            };
            let port = self.tcp_port();
            if !matches!(components.next(), Some(Protocol::Tcp(tcp)) if tcp == port) {
                return true;
            }
            jobs.push(hostname_lookup::LookupJob {
                name,
                port,
                entry_peer: *entry_peer,
                sender: *sender,
                address: address.clone(),
            });
            false
        });
        jobs
    }

    /// Queues `job` unless the node is connected to its peer, the name is queued or was looked up less than 30
    /// minutes ago, or the queue is full (CRD ch.28 R8c items 7.1 and 7.2). Only the last case is reported, at DEBUG.
    #[cfg(not(target_arch = "wasm32"))]
    fn queue_hostname_lookup(&mut self, job: hostname_lookup::LookupJob) {
        let connected = self.request_response.is_connected(&job.entry_peer);
        let (address, sender) = (job.address.clone(), job.sender);
        if self.lookups.enqueue(job, connected, Instant::now()) == hostname_lookup::Enqueued::QueueFull {
            debug!(
                "Peer exchange: skipped address {} from peer {}: the queue of {} names to look up is full",
                address,
                sender,
                hostname_lookup::MAX_QUEUED_NAMES
            );
        }
    }

    /// Drives the hostname lookups and applies the ones that finished.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_hostname_lookups(&mut self, cx: &mut Context) {
        let request_response = &self.request_response;
        let finished = self.lookups.poll(cx, |peer| request_response.is_connected(peer));
        for lookup in finished {
            self.finish_hostname_lookup(lookup);
        }
    }

    /// Applies the result of one lookup (CRD ch.28 R8c items 7.3, 7.5 and 7.6, decision BH) and returns the reason
    /// the name was reported with, if it was.
    ///
    /// Each IPv4 result is checked like a received address and stored under the entry's peer. A failed or
    /// timed-out lookup keeps what the name stored before. A lookup that returned changes the peer's addresses to
    /// the checked results: the addresses the name stored before and no longer returns are removed, never from a
    /// reserved peer.
    #[cfg(not(target_arch = "wasm32"))]
    fn finish_hostname_lookup(&mut self, finished: hostname_lookup::FinishedLookup) -> Option<String> {
        use hostname_lookup::LookupResult;

        let hostname_lookup::FinishedLookup { job, result } = finished;
        let reason = match result {
            LookupResult::Failed(error) => Some(format!("the lookup of the name failed: {}", error)),
            LookupResult::TimedOut => Some(format!(
                "the lookup of the name did not return within {:?}",
                self.lookups.timeout
            )),
            LookupResult::Resolved(results) => {
                let mut accepted = PeerAddresses::new();
                let mut rejected: Vec<std::net::Ipv4Addr> = Vec::new();
                for result in results {
                    if let std::net::SocketAddr::V4(result) = result {
                        let address = Multiaddr::empty()
                            .with(Protocol::Ip4(*result.ip()))
                            .with(Protocol::Tcp(job.port));
                        if self.validate_global_multiaddr(&address) {
                            accepted.insert(address);
                        } else if !rejected.contains(result.ip()) {
                            rejected.push(*result.ip());
                        }
                    }
                }
                let reason = if !accepted.is_empty() {
                    None
                } else if rejected.is_empty() {
                    Some("the name has no IPv4 address".to_owned())
                } else {
                    let rejected = rejected.iter().map(|ip| ip.to_string()).collect::<Vec<_>>();
                    Some(format!(
                        "the name resolves only to addresses that are not globally routable: {}",
                        rejected.join(", ")
                    ))
                };
                self.store_resolved_addresses(&job, accepted);
                reason
            },
        };
        if let Some(reason) = &reason {
            let level = self.skipped_addresses.record(&job.address);
            log::log!(
                level,
                "Peer exchange: skipped address {} from peer {}: {}",
                job.address,
                job.sender,
                reason
            );
        }
        reason
    }

    /// Stores the checked results of a lookup under the entry's peer and updates the per-name record (CRD ch.28 R8c
    /// items 7.3 and 7.5).
    #[cfg(not(target_arch = "wasm32"))]
    fn store_resolved_addresses(&mut self, job: &hostname_lookup::LookupJob, accepted: PeerAddresses) {
        let (previous_peer, previous) = match self.lookups.record(&job.name) {
            Some(record) => (record.entry_peer, record.stored.clone()),
            None => (job.entry_peer, PeerAddresses::new()),
        };
        let known_before: Vec<Multiaddr> = self.request_response.addresses_of_peer(&job.entry_peer);
        self.store_accepted_addresses(&job.entry_peer, accepted.clone());
        for stale in previous.difference(&accepted) {
            self.remove_stale_address(&previous_peer, stale);
        }
        // The record holds what the name stored: an address that the peer already had from another source is not
        // the name's to remove, unless an earlier lookup of the name stored it.
        let stored = accepted
            .into_iter()
            .filter(|address| !known_before.contains(address) || previous.contains(address))
            .collect();
        self.lookups.set_stored(&job.name, job.entry_peer, stored);
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

        // A name queued while the events above were processed starts its lookup in this poll. Nothing is pending
        // while the queue is empty, so no waker is registered for it then (CRD ch.28 R8c item 7.2).
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_hostname_lookups(cx);

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
        let mut behaviour =
            PeersExchange::new(NetworkInfo::InMemory, PeerId::random()).with_address_settings(false, false);
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

/// Tests of CRD ch.28 R8c items 7 to 10 (decision BH), T6a bullet "Added 2026-10-31". No network is used: the lookups
/// go through a scripted resolver. "Connected" needs a swarm connection, so the decision is tested on its inputs
/// (`HostnameLookups::enqueue` and `HostnameLookups::poll` take the answer as a parameter), and the real call
/// `PeersExchange::queue_hostname_lookup` passes `request_response.is_connected(entry_peer)` for it.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod hostname_tests {
    use super::hostname_lookup::{Enqueued, HostResolver, LookupJob, MAX_NAME_RECORDS, MAX_QUEUED_NAMES};
    use super::{PeerAddresses, PeerIdSerde, PeersExchange};
    use crate::{NetworkInfo, NetworkPorts, PeerId};
    use futures::task::{waker, ArcWake};
    use libp2p::core::Multiaddr;
    use std::collections::{HashMap, HashSet};
    use std::io;
    use std::net::{IpAddr, SocketAddr};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::task::Context;
    use std::time::Duration;
    use wasm_timer::Instant;

    const PORT: u16 = 3000;

    /// A resolver that answers from a table, records its calls, and can hold the lookup of chosen names until
    /// `release` is called.
    #[derive(Default)]
    struct ScriptedResolver {
        answers: Mutex<HashMap<String, Vec<IpAddr>>>,
        blocked: Mutex<HashSet<String>>,
        released: (Mutex<bool>, Condvar),
        calls: Mutex<Vec<String>>,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl ScriptedResolver {
        fn answer(&self, name: &str, ips: &[&str]) {
            let ips = ips.iter().map(|ip| ip.parse().unwrap()).collect();
            self.answers.lock().unwrap().insert(name.to_owned(), ips);
        }

        fn block(&self, name: &str) { self.blocked.lock().unwrap().insert(name.to_owned()); }

        fn release(&self) {
            *self.released.0.lock().unwrap() = true;
            self.released.1.notify_all();
        }

        fn calls(&self) -> Vec<String> { self.calls.lock().unwrap().clone() }
    }

    impl HostResolver for ScriptedResolver {
        fn lookup(&self, name: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            self.calls.lock().unwrap().push(name.to_owned());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            if self.blocked.lock().unwrap().contains(name) {
                let (lock, condvar) = &self.released;
                let _released = condvar
                    .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(20), |released| !*released)
                    .unwrap();
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            match self.answers.lock().unwrap().get(name) {
                Some(ips) => Ok(ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect()),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "no such host")),
            }
        }
    }

    fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

    fn behaviour_with(resolver: &Arc<ScriptedResolver>, resolve: bool, allow_private: bool) -> PeersExchange {
        let mut behaviour = PeersExchange::new(
            NetworkInfo::Distributed {
                network_ports: NetworkPorts { tcp: PORT, wss: 3010 },
            },
            PeerId::random(),
        )
        .with_address_settings(resolve, allow_private);
        behaviour.lookups.resolver = resolver.clone();
        behaviour
    }

    fn behaviour(resolver: &Arc<ScriptedResolver>) -> PeersExchange { behaviour_with(resolver, true, false) }

    fn addresses_of(addresses: &[&str]) -> PeerAddresses {
        addresses.iter().map(|address| address.parse().unwrap()).collect()
    }

    fn ip4(address: &str) -> Multiaddr { format!("/ip4/{}/tcp/{}", address, PORT).parse().unwrap() }

    fn dns(name: &str) -> String { format!("/dns/{}/tcp/{}", name, PORT) }

    /// Processes an answer from `sender` that holds the given `(entry peer, addresses)` pairs.
    fn answer(behaviour: &mut PeersExchange, sender: &PeerId, entries: &[(PeerId, Vec<String>)]) {
        let peers = entries
            .iter()
            .map(|(peer, addresses)| {
                let addresses = addresses.iter().map(|address| address.parse().unwrap()).collect();
                (PeerIdSerde(*peer), addresses)
            })
            .collect();
        assert!(behaviour.process_known_peers_response(sender, peers));
    }

    /// Polls once with a waker that does nothing and applies what finished. Returns `(name, reported reason)`.
    fn poll_once(behaviour: &mut PeersExchange) -> Vec<(String, Option<String>)> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let finished = behaviour.lookups.poll(&mut cx, |_| false);
        finished
            .into_iter()
            .map(|lookup| (lookup.job.name.clone(), behaviour.finish_hostname_lookup(lookup)))
            .collect()
    }

    /// Polls until nothing is queued or running.
    fn drain(behaviour: &mut PeersExchange) -> Vec<(String, Option<String>)> {
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            all.extend(poll_once(behaviour));
            if behaviour.lookups.is_idle() {
                return all;
            }
            assert!(Instant::now() < deadline, "the lookups did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(Instant::now() < deadline, "the condition did not come true");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn job(name: &str, entry_peer: PeerId) -> LookupJob {
        LookupJob {
            name: name.to_owned(),
            port: PORT,
            entry_peer,
            sender: PeerId::random(),
            address: dns(name).parse().unwrap(),
        }
    }

    #[test]
    fn test_global_result_is_stored_and_name_is_not() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142", "2001:db8::1"]);
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());

        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        // The name is queued, not stored, and the entry is not known before the lookup finished.
        assert_eq!(behaviour.lookups.queue.len(), 1);
        assert!(!behaviour.is_known_peer(&entry));
        assert!(behaviour.request_response.addresses_of_peer(&entry).is_empty());

        let finished = drain(&mut behaviour);
        assert_eq!(finished, vec![("seed.example.com".to_owned(), None)]);
        assert!(behaviour.is_known_peer(&entry));
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.142"
        )]);
        assert!(behaviour.skipped_addresses.reported.is_empty());
        assert_eq!(
            behaviour.lookups.record("seed.example.com").unwrap().stored,
            addresses_of(&["/ip4/216.58.210.142/tcp/3000"])
        );
        // Nothing hands the name out.
        for _ in 0..10 {
            let answer = behaviour.get_random_known_peers(10);
            assert!(answer
                .values()
                .flatten()
                .all(|address| address.to_string().starts_with("/ip4/")));
        }
        assert!(behaviour
            .get_random_peers(10, |_| true)
            .values()
            .flatten()
            .all(|address| address.to_string().starts_with("/ip4/")));
    }

    #[test]
    fn test_dns4_is_looked_up_and_accepted_addresses_come_first() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());

        // The entry has a hostname and an accepted address: the address is stored by the time the answer is
        // processed, and the name waits for the lookup.
        answer(&mut behaviour, &sender, &[(entry, vec![
            "/dns4/seed.example.com/tcp/3000".to_owned(),
            "/ip4/216.58.210.1/tcp/3000".to_owned(),
        ])]);
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.1"
        )]);
        assert_eq!(behaviour.lookups.queue.len(), 1);
        assert!(resolver.calls().is_empty());

        drain(&mut behaviour);
        let mut stored = behaviour.request_response.addresses_of_peer(&entry);
        stored.sort();
        assert_eq!(stored, vec![ip4("216.58.210.1"), ip4("216.58.210.142")]);
    }

    #[test]
    fn test_non_global_results_are_rejected_with_reason_and_accepted_with_allow_private() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("private.example.com", &["10.0.0.7"]);
        resolver.answer("loopback.example.com", &["127.0.0.1"]);
        resolver.answer("linklocal.example.com", &["169.254.1.1"]);
        resolver.answer("v6only.example.com", &["2001:db8::1"]);
        let names = ["private", "loopback", "linklocal", "v6only"].map(|n| format!("{}.example.com", n));

        let mut strict = behaviour(&resolver);
        let sender = PeerId::random();
        let entries: Vec<_> = names.iter().map(|n| (PeerId::random(), vec![dns(n)])).collect();
        answer(&mut strict, &sender, &entries);
        let mut finished = drain(&mut strict);
        finished.sort();
        assert_eq!(finished.len(), 4);
        for (name, reason) in &finished {
            let reason = reason.as_ref().unwrap_or_else(|| panic!("{} was not reported", name));
            if name == "v6only.example.com" {
                assert!(reason.contains("no IPv4 address"), "{}", reason);
            } else {
                assert!(reason.contains("not globally routable"), "{}", reason);
            }
        }
        assert!(finished[2].1.as_ref().unwrap().contains("10.0.0.7"));
        for (peer, _) in &entries {
            assert!(!strict.is_known_peer(peer));
            assert!(strict.request_response.addresses_of_peer(peer).is_empty());
        }
        // Every name is reported under the item 4 deduplication.
        assert_eq!(strict.skipped_addresses.reported.len(), 4);

        // With the switch the same names are stored, and so is a received private address. The port check stays.
        let mut permissive = behaviour_with(&resolver, true, true);
        let mut with_private = entries.clone();
        let (private_peer, wrong_port_peer) = (PeerId::random(), PeerId::random());
        with_private.truncate(3);
        with_private.push((private_peer, vec!["/ip4/192.168.1.5/tcp/3000".to_owned()]));
        with_private.push((wrong_port_peer, vec!["/ip4/192.168.1.6/tcp/3001".to_owned()]));
        answer(&mut permissive, &sender, &with_private);
        let finished = drain(&mut permissive);
        assert_eq!(finished.len(), 3);
        assert!(finished.iter().all(|(_, reason)| reason.is_none()));
        assert_eq!(permissive.request_response.addresses_of_peer(&with_private[0].0), vec![
            ip4("10.0.0.7")
        ]);
        assert_eq!(permissive.request_response.addresses_of_peer(&with_private[1].0), vec![
            ip4("127.0.0.1")
        ]);
        assert_eq!(permissive.request_response.addresses_of_peer(&with_private[2].0), vec![
            ip4("169.254.1.1")
        ]);
        assert_eq!(permissive.request_response.addresses_of_peer(&private_peer), vec![ip4(
            "192.168.1.5"
        )]);
        assert!(!permissive.is_known_peer(&wrong_port_peer));
    }

    #[test]
    fn test_failure_and_no_ipv4_are_reported_and_store_nothing() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());

        answer(&mut behaviour, &sender, &[(entry, vec![dns("unknown.example.com")])]);
        let finished = drain(&mut behaviour);
        let reason = finished[0].1.as_ref().expect("the failure is reported");
        assert!(
            reason.contains("failed") && reason.contains("no such host"),
            "{}",
            reason
        );
        assert!(!behaviour.is_known_peer(&entry));
        assert_eq!(behaviour.skipped_addresses.reported.len(), 1);
        assert!(idle_without_pending_work(&behaviour));
    }

    fn idle_without_pending_work(behaviour: &PeersExchange) -> bool {
        behaviour.lookups.queue.is_empty() && behaviour.lookups.is_idle()
    }

    #[test]
    fn test_other_forms_and_announcements_make_no_lookup() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());

        answer(&mut behaviour, &sender, &[(entry, vec![
            "/dns6/seed.example.com/tcp/3000".to_owned(),
            "/dnsaddr/seed.example.com".to_owned(),
            "/dns/seed.example.com/tcp/3001".to_owned(),
            "/dns4/seed.example.com/tcp/3001".to_owned(),
            "/dns/seed.example.com".to_owned(),
        ])]);
        assert!(idle_without_pending_work(&behaviour));
        assert!(!behaviour.is_known_peer(&entry));
        // Each one is skipped and reported as before.
        assert_eq!(behaviour.skipped_addresses.reported.len(), 5);

        // A hostname in a peers-topic announcement is skipped under every setting.
        let announcer = PeerId::random();
        behaviour.add_peer_addresses_to_known_peers(&announcer, addresses_of(&[&dns("seed.example.com")]));
        assert!(idle_without_pending_work(&behaviour));
        assert!(!behaviour.is_known_peer(&announcer));

        poll_once(&mut behaviour);
        assert!(resolver.calls().is_empty());
    }

    #[test]
    fn test_no_lookup_while_connected_and_none_within_30_minutes() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());
        let now = Instant::now();

        // The decision on its inputs: `queue_hostname_lookup` passes `request_response.is_connected(entry_peer)`.
        assert_eq!(
            behaviour.lookups.enqueue(job("seed.example.com", entry), true, now),
            Enqueued::Connected
        );
        assert!(idle_without_pending_work(&behaviour));

        // A name that waited while the connection came up is dropped when its turn comes, and leaves no record.
        assert_eq!(
            behaviour.lookups.enqueue(job("seed.example.com", entry), false, now),
            Enqueued::Queued
        );
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(behaviour.lookups.poll(&mut cx, |peer| *peer == entry).is_empty());
        assert!(idle_without_pending_work(&behaviour));
        assert!(behaviour.lookups.record("seed.example.com").is_none());
        assert!(resolver.calls().is_empty());

        // An entry whose peer has no connection is looked up through the real call.
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        assert_eq!(behaviour.lookups.queue.len(), 1);
        drain(&mut behaviour);
        assert_eq!(resolver.calls(), vec!["seed.example.com".to_owned()]);

        // A second sighting within 30 minutes makes no second lookup, from any peer.
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        answer(&mut behaviour, &sender, &[(PeerId::random(), vec![dns(
            "seed.example.com",
        )])]);
        assert!(idle_without_pending_work(&behaviour));
        assert_eq!(
            behaviour
                .lookups
                .enqueue(job("seed.example.com", entry), false, Instant::now()),
            Enqueued::TooSoon
        );
        // After the interval it is looked up again.
        behaviour.lookups.relookup_interval = Duration::ZERO;
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        drain(&mut behaviour);
        assert_eq!(resolver.calls().len(), 2);
    }

    #[test]
    fn test_name_queued_twice_is_looked_up_once() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        let sender = PeerId::random();
        answer(&mut behaviour, &sender, &[
            (PeerId::random(), vec![dns("seed.example.com")]),
            (PeerId::random(), vec![dns("seed.example.com")]),
        ]);
        assert_eq!(behaviour.lookups.queue.len(), 1);
    }

    #[test]
    fn test_changed_result_adds_new_address_and_removes_old_one() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142", "216.58.210.150"]);
        let mut behaviour = behaviour(&resolver);
        behaviour.lookups.relookup_interval = Duration::ZERO;
        let (sender, entry) = (PeerId::random(), PeerId::random());
        // An address the peer got from another source is not the name's to remove.
        behaviour.add_peer_addresses_to_known_peers(&entry, addresses_of(&["/ip4/216.58.210.150/tcp/3000"]));

        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        drain(&mut behaviour);
        let mut stored = behaviour.request_response.addresses_of_peer(&entry);
        stored.sort();
        assert_eq!(stored, vec![ip4("216.58.210.142"), ip4("216.58.210.150")]);
        assert_eq!(
            behaviour.lookups.record("seed.example.com").unwrap().stored,
            addresses_of(&["/ip4/216.58.210.142/tcp/3000"])
        );

        resolver.answer("seed.example.com", &["216.58.210.143"]);
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        drain(&mut behaviour);
        let mut stored = behaviour.request_response.addresses_of_peer(&entry);
        stored.sort();
        assert_eq!(stored, vec![ip4("216.58.210.143"), ip4("216.58.210.150")]);

        // A name that now resolves to nothing acceptable removes what it stored.
        resolver.answer("seed.example.com", &["10.0.0.1"]);
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        drain(&mut behaviour);
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.150"
        )]);
    }

    #[test]
    fn test_failed_lookup_keeps_what_the_name_stored_and_reserved_peer_is_kept() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        resolver.answer("reserved.example.com", &["216.58.210.160"]);
        let mut behaviour = behaviour(&resolver);
        behaviour.lookups.relookup_interval = Duration::ZERO;
        let (sender, entry, reserved) = (PeerId::random(), PeerId::random(), PeerId::random());
        behaviour.add_peer_addresses_to_reserved_peers(&reserved, addresses_of(&["/ip4/216.58.210.170/tcp/3000"]));

        answer(&mut behaviour, &sender, &[
            (entry, vec![dns("seed.example.com")]),
            (reserved, vec![dns("reserved.example.com")]),
        ]);
        drain(&mut behaviour);

        // A later failure of the name changes nothing.
        resolver.answers.lock().unwrap().remove("seed.example.com");
        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        let finished = drain(&mut behaviour);
        assert!(finished[0].1.is_some());
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.142"
        )]);

        // The name of the reserved peer no longer returns what it stored: the reserved peer keeps it.
        resolver.answer("reserved.example.com", &["216.58.210.161"]);
        answer(&mut behaviour, &sender, &[(reserved, vec![dns(
            "reserved.example.com",
        )])]);
        drain(&mut behaviour);
        let mut stored = behaviour.request_response.addresses_of_peer(&reserved);
        stored.sort();
        assert_eq!(stored, vec![
            ip4("216.58.210.160"),
            ip4("216.58.210.161"),
            ip4("216.58.210.170")
        ]);
    }

    #[test]
    fn test_queue_holds_64_names_and_a_65th_is_dropped() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        let mut behaviour = behaviour(&resolver);
        let sender = PeerId::random();

        // An answer holds at most 20 entries, so 64 names take four answers.
        let mut sent = 0;
        while sent < MAX_QUEUED_NAMES {
            let count = (MAX_QUEUED_NAMES - sent).min(20);
            let entries: Vec<_> = (sent..sent + count)
                .map(|i| (PeerId::random(), vec![dns(&format!("n{}.example.com", i))]))
                .collect();
            answer(&mut behaviour, &sender, &entries);
            sent += count;
        }
        assert_eq!(behaviour.lookups.queue.len(), MAX_QUEUED_NAMES);

        answer(&mut behaviour, &sender, &[(PeerId::random(), vec![dns(
            "overflow.example.com",
        )])]);
        assert_eq!(behaviour.lookups.queue.len(), MAX_QUEUED_NAMES);
        assert!(behaviour
            .lookups
            .queue
            .iter()
            .all(|queued| queued.name != "overflow.example.com"));
        assert_eq!(
            behaviour
                .lookups
                .enqueue(job("overflow.example.com", PeerId::random()), false, Instant::now()),
            Enqueued::QueueFull
        );
        // A name already queued is not queued twice, and a full queue does not change that.
        assert_eq!(
            behaviour
                .lookups
                .enqueue(job("n0.example.com", PeerId::random()), false, Instant::now()),
            Enqueued::AlreadyQueued
        );
    }

    #[test]
    fn test_name_record_is_bounded_and_drops_the_oldest() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        let mut behaviour = behaviour(&resolver);
        let sender = PeerId::random();
        for i in 0..=MAX_NAME_RECORDS {
            let name = format!("n{}.example.com", i);
            resolver.answer(&name, &["216.58.210.142"]);
            answer(&mut behaviour, &sender, &[(PeerId::random(), vec![dns(&name)])]);
            drain(&mut behaviour);
        }
        assert_eq!(behaviour.lookups.records.len(), MAX_NAME_RECORDS);
        assert!(behaviour.lookups.record("n0.example.com").is_none());
        assert!(behaviour.lookups.record("n1.example.com").is_some());
        assert!(behaviour
            .lookups
            .record(&format!("n{}.example.com", MAX_NAME_RECORDS))
            .is_some());
    }

    #[test]
    fn test_lookups_run_one_at_a_time_and_a_timeout_stores_nothing() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("slow.example.com", &["216.58.210.142"]);
        resolver.answer("next.example.com", &["216.58.210.143"]);
        resolver.block("slow.example.com");
        let mut behaviour = behaviour(&resolver);
        behaviour.lookups.timeout = Duration::from_millis(50);
        let (sender, slow, next) = (PeerId::random(), PeerId::random(), PeerId::random());

        answer(&mut behaviour, &sender, &[
            (slow, vec![dns("slow.example.com")]),
            (next, vec![dns("next.example.com")]),
        ]);
        // The slow name is looked up first or second; make the order deterministic.
        behaviour
            .lookups
            .queue
            .make_contiguous()
            .sort_by_key(|queued| queued.name != "slow.example.com");

        poll_once(&mut behaviour);
        wait_for(|| resolver.calls().len() == 1);
        assert_eq!(resolver.calls(), vec!["slow.example.com".to_owned()]);

        // After the timeout the result is given up and reported, but the worker is still busy: the next lookup
        // does not start.
        let mut finished = Vec::new();
        wait_for(|| {
            finished.extend(poll_once(&mut behaviour));
            !finished.is_empty()
        });
        assert_eq!(finished.len(), 1);
        assert!(finished[0].1.as_ref().unwrap().contains("did not return within"));
        assert!(!behaviour.lookups.is_idle());
        for _ in 0..5 {
            assert!(poll_once(&mut behaviour).is_empty());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(resolver.calls(), vec!["slow.example.com".to_owned()]);
        assert!(!behaviour.is_known_peer(&slow));

        // The worker returns. Its late result is not stored, and only then does the next lookup start.
        resolver.release();
        let finished = drain(&mut behaviour);
        assert_eq!(finished, vec![("next.example.com".to_owned(), None)]);
        assert_eq!(resolver.calls(), vec![
            "slow.example.com".to_owned(),
            "next.example.com".to_owned()
        ]);
        assert!(!behaviour.is_known_peer(&slow));
        assert!(behaviour.request_response.addresses_of_peer(&slow).is_empty());
        assert!(behaviour.is_known_peer(&next));
        assert_eq!(resolver.max_active.load(Ordering::SeqCst), 1);
        assert!(behaviour.lookups.is_idle());
    }

    #[test]
    fn test_nothing_is_pending_with_an_empty_queue() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        assert!(behaviour.lookups.is_idle());
        assert!(poll_once(&mut behaviour).is_empty());
        assert!(behaviour.lookups.is_idle());

        answer(&mut behaviour, &PeerId::random(), &[(PeerId::random(), vec![dns(
            "seed.example.com",
        )])]);
        assert!(!behaviour.lookups.is_idle());
        drain(&mut behaviour);
        // The lookup, its timer and its worker handle are gone with the last name.
        assert!(behaviour.lookups.is_idle());
        assert!(poll_once(&mut behaviour).is_empty());
        assert!(behaviour.lookups.is_idle());
    }

    struct FlagWaker(AtomicBool);

    impl ArcWake for FlagWaker {
        fn wake_by_ref(arc_self: &Arc<Self>) { arc_self.0.store(true, Ordering::SeqCst); }
    }

    /// `PeersExchange::poll_hostname_lookups` starts a name queued by the same poll, and the waker it registered is
    /// woken when the worker returns.
    #[test]
    fn test_name_queued_in_a_poll_starts_in_that_poll_and_wakes_the_task() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        resolver.block("seed.example.com");
        let mut behaviour = behaviour(&resolver);
        let (sender, entry) = (PeerId::random(), PeerId::random());
        let flag = Arc::new(FlagWaker(AtomicBool::new(false)));
        let task_waker = waker(flag.clone());
        let mut cx = Context::from_waker(&task_waker);

        answer(&mut behaviour, &sender, &[(entry, vec![dns("seed.example.com")])]);
        behaviour.poll_hostname_lookups(&mut cx);
        wait_for(|| resolver.calls().len() == 1);
        assert!(!flag.0.load(Ordering::SeqCst));

        resolver.release();
        wait_for(|| flag.0.load(Ordering::SeqCst));
        behaviour.poll_hostname_lookups(&mut cx);
        assert!(behaviour.lookups.is_idle());
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.142"
        )]);
    }

    #[test]
    fn test_own_peer_id_causes_no_lookup() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour(&resolver);
        let local = behaviour.local_peer_id;
        answer(&mut behaviour, &PeerId::random(), &[(local, vec![dns(
            "seed.example.com",
        )])]);
        assert!(idle_without_pending_work(&behaviour));
        assert!(resolver.calls().is_empty());
        assert!(!behaviour.is_known_peer(&local));
    }

    #[test]
    fn test_answers_given_carry_only_the_accepted_form() {
        let resolver = Arc::new(ScriptedResolver::default());
        let mut behaviour = behaviour(&resolver);
        let public: Multiaddr = "/ip4/216.58.210.142/tcp/3000".parse().unwrap();
        let dialed: Multiaddr = "/dns/seed.example.com/tcp/3000".parse().unwrap();
        let private: Multiaddr = "/ip4/10.0.0.1/tcp/3000".parse().unwrap();

        // A peer connected through a dialed hostname: the request-response layer reports the dialed address too.
        let via_hostname = PeerId::random();
        behaviour.add_known_peer(via_hostname);
        behaviour.request_response.add_address(&via_hostname, public.clone());
        behaviour.request_response.add_address(&via_hostname, dialed.clone());
        // An entry left with no address, and one that only has a private address.
        let only_hostname = PeerId::random();
        behaviour.add_known_peer(only_hostname);
        behaviour.request_response.add_address(&only_hostname, dialed.clone());
        let only_private = PeerId::random();
        behaviour.add_known_peer(only_private);
        behaviour.request_response.add_address(&only_private, private.clone());

        for _ in 0..20 {
            let answer = behaviour.get_random_known_peers(1);
            // The choice is among the peers that still have an address.
            assert_eq!(answer.len(), 1);
            assert_eq!(
                answer.get(&via_hostname.into()),
                Some(&addresses_of(&["/ip4/216.58.210.142/tcp/3000"]))
            );
        }
        let answer = behaviour.get_random_known_peers(10);
        assert_eq!(answer.len(), 1);

        // With the private switch the private entry is acceptable too.
        let mut permissive = behaviour_with(&resolver, true, true);
        permissive.add_known_peer(only_private);
        permissive.request_response.add_address(&only_private, private.clone());
        permissive.add_known_peer(only_hostname);
        permissive.request_response.add_address(&only_hostname, dialed.clone());
        let answer = permissive.get_random_known_peers(10);
        assert_eq!(answer.len(), 1);
        assert_eq!(
            answer.get(&only_private.into()),
            Some(&addresses_of(&["/ip4/10.0.0.1/tcp/3000"]))
        );

        // With the opt-back switch the answers are what they were before: every address, the dialed one included.
        let mut legacy = behaviour_with(&resolver, false, false);
        legacy.add_known_peer(via_hostname);
        legacy.request_response.add_address(&via_hostname, public.clone());
        legacy.request_response.add_address(&via_hostname, dialed.clone());
        legacy.add_known_peer(only_hostname);
        legacy.request_response.add_address(&only_hostname, dialed.clone());
        let answer = legacy.get_random_known_peers(10);
        assert_eq!(answer.len(), 2);
        assert_eq!(
            answer.get(&via_hostname.into()),
            Some(&HashSet::from([public, dialed.clone()]))
        );
        assert_eq!(answer.get(&only_hostname.into()), Some(&HashSet::from([dialed])));
    }

    #[test]
    fn test_resolve_hostnames_false_makes_no_lookup() {
        let rt = runtime();
        let _guard = rt.enter();
        let resolver = Arc::new(ScriptedResolver::default());
        resolver.answer("seed.example.com", &["216.58.210.142"]);
        let mut behaviour = behaviour_with(&resolver, false, false);
        let (sender, entry, other) = (PeerId::random(), PeerId::random(), PeerId::random());

        answer(&mut behaviour, &sender, &[
            (entry, vec![
                dns("seed.example.com"),
                "/ip4/216.58.210.1/tcp/3000".to_owned(),
            ]),
            (other, vec![
                dns("seed.example.com"),
                "/ip4/10.0.0.1/tcp/3000".to_owned(),
            ]),
        ]);
        poll_once(&mut behaviour);
        assert!(idle_without_pending_work(&behaviour));
        assert!(resolver.calls().is_empty());
        // Items 1 to 6 hold: the accepted address is kept, the rest is skipped and the entry without one too.
        assert_eq!(behaviour.request_response.addresses_of_peer(&entry), vec![ip4(
            "216.58.210.1"
        )]);
        assert!(!behaviour.is_known_peer(&other));
        assert_eq!(behaviour.skipped_addresses.reported.len(), 2);
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use super::{PeerIdSerde, PeersExchange};
    use crate::{NetworkInfo, NetworkPorts, PeerId};
    use std::collections::HashMap;

    /// A browser build skips every hostname (CRD ch.28 R8c item 7).
    #[test]
    fn test_browser_build_skips_every_hostname() {
        let mut behaviour = PeersExchange::new(
            NetworkInfo::Distributed {
                network_ports: NetworkPorts { tcp: 3000, wss: 3010 },
            },
            PeerId::random(),
        );
        let entry = PeerId::random();
        let peers = HashMap::from([(
            PeerIdSerde(entry),
            ["/dns/seed.example.com/tcp/3000".parse().unwrap()]
                .into_iter()
                .collect(),
        )]);
        assert!(behaviour.process_known_peers_response(&PeerId::random(), peers));
        assert!(!behaviour.is_known_peer(&entry));
    }
}
