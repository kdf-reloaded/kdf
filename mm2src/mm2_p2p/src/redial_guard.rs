//! Redial guards of the relay-connection maintenance (CRD ch.28 R27a).
//!
//! The start-up dials (R23 item 2) and the 30 s maintenance of RQ1a item 4 consult one [`RedialGuards`] value:
//!
//! 1. [`DialRecord`] (R27a item 1) remembers the addresses that were dialed, as written, and refuses to dial one
//!    again before the interval has passed.
//! 2. [`SeedPeers`] (R27a item 2) remembers which peer the last outbound connection to each configured bootstrap
//!    address reached, so that the address is not dialed while that peer is connected through any connection.
//! 3. [`ShortOfRelaysLog`] (R27a item 3) limits the "below low watermark" WARN line.
//!
//! Every method takes the current time as an argument and never reads the clock itself, so that the tests drive it.
//! The time must not go backwards: the monotonic clock of the WebAssembly timer asserts on it.

use crate::atomicdex_behaviour::CONNECTED_RELAYS_CHECK_INTERVAL;
use libp2p::{swarm::ConnectionId, Multiaddr, PeerId};
use log::Level;
use std::{collections::HashMap, time::Duration};
use wasm_timer::Instant;

/// How long a dialed address is not dialed again (R27a item 1) and how long the short-of-relays WARN line is held
/// back while the number of connected relays is unchanged (R27a item 3).
pub(crate) const REDIAL_GUARD_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How many dialed addresses are remembered (R27a item 1). When the record is full, the oldest entry is dropped.
pub(crate) const MAX_DIALED_ADDRESSES: usize = 1024;

/// The timings of the maintenance and of its guards. The default is the production setting; a test shortens both.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RedialGuardConfig {
    /// The guard interval of R27a items 1 and 3.
    pub(crate) interval: Duration,
    /// The period of the relay-connection maintenance tick (RQ1a item 4).
    pub(crate) tick: Duration,
}

impl Default for RedialGuardConfig {
    fn default() -> Self {
        RedialGuardConfig {
            interval: REDIAL_GUARD_INTERVAL,
            tick: CONNECTED_RELAYS_CHECK_INTERVAL,
        }
    }
}

/// The addresses this node dialed, with the time of the last dial (R27a item 1). An address is compared as written,
/// that is, as the `Multiaddr` that was passed to the dial, before any name resolution.
pub(crate) struct DialRecord {
    interval: Duration,
    max_entries: usize,
    dialed_at: HashMap<Multiaddr, Instant>,
}

impl DialRecord {
    pub(crate) fn new(interval: Duration) -> Self {
        DialRecord {
            interval,
            max_entries: MAX_DIALED_ADDRESSES,
            dialed_at: HashMap::new(),
        }
    }

    /// Whether `address` was dialed less than the interval before `now`.
    pub(crate) fn is_guarded(&self, address: &Multiaddr, now: Instant) -> bool {
        self.dialed_at
            .get(address)
            .is_some_and(|dialed_at| now.duration_since(*dialed_at) < self.interval)
    }

    /// Records a dial of `address` at `now`. Entries older than the interval are forgotten first. When the record is
    /// still full, the oldest entry is dropped.
    pub(crate) fn record(&mut self, address: Multiaddr, now: Instant) {
        let interval = self.interval;
        self.dialed_at
            .retain(|_, dialed_at| now.duration_since(*dialed_at) < interval);
        if self.dialed_at.len() >= self.max_entries && !self.dialed_at.contains_key(&address) {
            let oldest = self
                .dialed_at
                .iter()
                .max_by_key(|(_, dialed_at)| now.duration_since(**dialed_at))
                .map(|(address, _)| address.clone());
            if let Some(oldest) = oldest {
                self.dialed_at.remove(&oldest);
            }
        }
        self.dialed_at.insert(address, now);
    }

    #[cfg(test)]
    fn len(&self) -> usize { self.dialed_at.len() }
}

/// The peer that each configured bootstrap address belongs to (R27a item 2): the peer that the last outbound
/// connection established to that address reached.
///
/// The connection is matched to the address by the identifier of the dial, which the dial options carry before the
/// dial starts. The address of the established connection is not used: a `/dns` address is resolved inside the
/// transport, and the endpoint of the connection should not be relied upon to name the address as written.
///
/// Both maps are keyed by a bootstrap address, so their size is bounded by the bootstrap list.
#[derive(Default)]
pub(crate) struct SeedPeers {
    /// The newest dial in flight per bootstrap address.
    pending: HashMap<Multiaddr, ConnectionId>,
    /// The peer reached by the last established outbound connection per bootstrap address.
    peers: HashMap<Multiaddr, PeerId>,
}

impl SeedPeers {
    /// A dial of the bootstrap `address` with the identifier `connection_id` has been started.
    pub(crate) fn dial_started(&mut self, address: Multiaddr, connection_id: ConnectionId) {
        self.pending.insert(address, connection_id);
    }

    /// The dial of `address` with `connection_id` has been refused by the swarm, so no event follows.
    pub(crate) fn dial_not_scheduled(&mut self, address: &Multiaddr, connection_id: ConnectionId) {
        if self.pending.get(address) == Some(&connection_id) {
            self.pending.remove(address);
        }
    }

    /// An outbound connection with `connection_id` was established and reached `peer`.
    pub(crate) fn connection_established(&mut self, connection_id: ConnectionId, peer: PeerId) {
        let address = self
            .pending
            .iter()
            .find(|(_, pending_id)| **pending_id == connection_id)
            .map(|(address, _)| address.clone());
        if let Some(address) = address {
            self.pending.remove(&address);
            self.peers.insert(address, peer);
        }
    }

    /// The pending dial with `connection_id` failed.
    pub(crate) fn dial_failed(&mut self, connection_id: ConnectionId) {
        self.pending.retain(|_, pending_id| *pending_id != connection_id);
    }

    /// The peer that the bootstrap `address` belongs to, once an outbound connection to it has been established.
    fn peer_of(&self, address: &Multiaddr) -> Option<&PeerId> { self.peers.get(address) }
}

/// Limits the short-of-relays WARN line (R27a item 3): at most once per interval while the number of connected
/// relays is unchanged. A changed number is reported at once.
pub(crate) struct ShortOfRelaysLog {
    interval: Duration,
    last_warned: Option<(Instant, usize)>,
}

impl ShortOfRelaysLog {
    pub(crate) fn new(interval: Duration) -> Self {
        ShortOfRelaysLog {
            interval,
            last_warned: None,
        }
    }

    /// The level of the line when `connected_relays` relays are connected at `now`: WARN when it was not logged at
    /// WARN within the interval with this number of relays (the line is then marked as logged), DEBUG otherwise.
    pub(crate) fn level(&mut self, connected_relays: usize, now: Instant) -> Level {
        let held_back = self.last_warned.is_some_and(|(warned_at, count)| {
            count == connected_relays && now.duration_since(warned_at) < self.interval
        });
        if held_back {
            return Level::Debug;
        }
        self.last_warned = Some((now, connected_relays));
        Level::Warn
    }
}

/// The state of all redial guards of one swarm.
pub(crate) struct RedialGuards {
    pub(crate) dialed: DialRecord,
    pub(crate) seeds: SeedPeers,
    pub(crate) short_of_relays: ShortOfRelaysLog,
}

impl RedialGuards {
    pub(crate) fn new(config: RedialGuardConfig) -> Self {
        RedialGuards {
            dialed: DialRecord::new(config.interval),
            seeds: SeedPeers::default(),
            short_of_relays: ShortOfRelaysLog::new(config.interval),
        }
    }

    /// The addresses of a known peer that maintenance may dial at `now`: those that are not guarded by the dial
    /// record (item 1) and are not the remote address of an open connection (RQ1a item 6).
    pub(crate) fn dialable_peer_addresses(
        &self,
        addresses: impl IntoIterator<Item = Multiaddr>,
        now: Instant,
        is_connected_to_addr: impl Fn(&Multiaddr) -> bool,
    ) -> Vec<Multiaddr> {
        addresses
            .into_iter()
            .filter(|address| !self.dialed.is_guarded(address, now) && !is_connected_to_addr(address))
            .collect()
    }

    /// The bootstrap addresses that maintenance may dial at `now`: those that are not guarded by the dial record
    /// (item 1), are not the remote address of an open connection (RQ1a item 6) and do not belong to a connected
    /// peer (item 2).
    pub(crate) fn dialable_bootstrap<'a>(
        &self,
        bootstrap: &'a [Multiaddr],
        now: Instant,
        is_connected_to_addr: impl Fn(&Multiaddr) -> bool,
        is_peer_connected: impl Fn(&PeerId) -> bool,
    ) -> Vec<&'a Multiaddr> {
        bootstrap
            .iter()
            .filter(|address| {
                !self.dialed.is_guarded(address, now)
                    && !is_connected_to_addr(address)
                    && !self.seeds.peer_of(address).is_some_and(&is_peer_connected)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(10);

    fn addr(n: usize) -> Multiaddr { format!("/ip4/203.0.113.{}/tcp/42845", n % 250).parse().unwrap() }

    fn addr_port(n: usize) -> Multiaddr { format!("/ip4/203.0.113.7/tcp/{}", 1000 + n).parse().unwrap() }

    fn guards() -> RedialGuards {
        RedialGuards::new(RedialGuardConfig {
            interval: INTERVAL,
            tick: Duration::from_secs(1),
        })
    }

    fn secs(start: Instant, s: u64) -> Instant { start + Duration::from_secs(s) }

    /// R27a item 1: an address dialed at start-up is not a candidate before the interval has passed, however the
    /// dial ended, and is one at the first tick after it. `record` is what `dial_bootstrap_addr` calls for every
    /// dial, including a dial that the swarm refused or that fails later, so a failed dial is guarded like any.
    #[test]
    fn bootstrap_address_is_guarded_for_the_interval() {
        let start = Instant::now();
        let mut guards = guards();
        let bootstrap = vec![addr(1), addr(2)];

        // Start-up dial of the first address.
        guards.dialed.record(bootstrap[0].clone(), start);

        for tick in 1..10 {
            let candidates = guards.dialable_bootstrap(&bootstrap, secs(start, tick), |_| false, |_| false);
            assert_eq!(candidates, vec![&bootstrap[1]], "tick {}", tick);
        }
        let candidates = guards.dialable_bootstrap(&bootstrap, secs(start, 10), |_| false, |_| false);
        assert_eq!(candidates, vec![&bootstrap[0], &bootstrap[1]]);
    }

    /// R27a item 1 for the addresses of a known peer dialed by maintenance.
    #[test]
    fn peer_address_is_guarded_for_the_interval() {
        let start = Instant::now();
        let mut guards = guards();
        let addresses = vec![addr(1), addr(2)];

        guards.dialed.record(addresses[0].clone(), start);

        for tick in 1..10 {
            let dialable = guards.dialable_peer_addresses(addresses.clone(), secs(start, tick), |_| false);
            assert_eq!(dialable, vec![addresses[1].clone()], "tick {}", tick);
        }
        let dialable = guards.dialable_peer_addresses(addresses.clone(), secs(start, 10), |_| false);
        assert_eq!(dialable, addresses);

        // A peer whose addresses are all guarded has none left, so it is not chosen as a candidate.
        guards.dialed.record(addresses[1].clone(), secs(start, 11));
        guards.dialed.record(addresses[0].clone(), secs(start, 11));
        assert!(guards
            .dialable_peer_addresses(addresses, secs(start, 12), |_| false)
            .is_empty());
    }

    /// An address of an open connection is not dialed (RQ1a item 6), guarded or not.
    #[test]
    fn address_of_an_open_connection_is_not_dialable() {
        let start = Instant::now();
        let guards = guards();
        let bootstrap = vec![addr(1), addr(2)];
        let candidates = guards.dialable_bootstrap(&bootstrap, start, |a| *a == bootstrap[0], |_| false);
        assert_eq!(candidates, vec![&bootstrap[1]]);
        let dialable = guards.dialable_peer_addresses(bootstrap.clone(), start, |a| *a == bootstrap[1]);
        assert_eq!(dialable, vec![bootstrap[0].clone()]);
    }

    /// The guard compares the address as written: another spelling of the same host is another address.
    #[test]
    fn guard_compares_the_address_as_written() {
        let start = Instant::now();
        let mut record = DialRecord::new(INTERVAL);
        let named: Multiaddr = "/dns4/seed.example.org/tcp/42845".parse().unwrap();
        let resolved: Multiaddr = "/ip4/203.0.113.9/tcp/42845".parse().unwrap();
        record.record(named.clone(), start);
        assert!(record.is_guarded(&named, secs(start, 1)));
        assert!(!record.is_guarded(&resolved, secs(start, 1)));
    }

    /// R27a item 1: the record forgets an entry after the interval and holds at most 1,024 entries; a full record
    /// drops the oldest entry.
    #[test]
    fn dial_record_is_bounded_and_expires() {
        let start = Instant::now();
        // A long interval: nothing expires, so only the bound limits the record.
        let mut record = DialRecord::new(Duration::from_secs(1_000_000));
        let total = MAX_DIALED_ADDRESSES + 50;
        for n in 0..total {
            record.record(addr_port(n), start + Duration::from_millis(n as u64));
            assert!(record.len() <= MAX_DIALED_ADDRESSES);
        }
        assert_eq!(record.len(), MAX_DIALED_ADDRESSES);
        let now = start + Duration::from_secs(10);
        // The 50 oldest are gone, the rest remain.
        for n in 0..50 {
            assert!(!record.is_guarded(&addr_port(n), now), "entry {} should be dropped", n);
        }
        for n in 50..total {
            assert!(record.is_guarded(&addr_port(n), now), "entry {} should be kept", n);
        }
        // Dialing an address that is already in the full record updates it and drops nobody.
        record.record(addr_port(60), now);
        assert_eq!(record.len(), MAX_DIALED_ADDRESSES);
        assert!(record.is_guarded(&addr_port(51), now));

        // Expiry: entries older than the interval are forgotten when the next dial is recorded.
        let mut record = DialRecord::new(INTERVAL);
        for n in 0..100 {
            record.record(addr_port(n), start);
        }
        assert_eq!(record.len(), 100);
        assert!(!record.is_guarded(&addr_port(0), secs(start, 10)));
        record.record(addr_port(500), secs(start, 10));
        assert_eq!(record.len(), 1);
    }

    /// R27a item 2: a bootstrap address whose last outbound connection reached peer P is not a candidate while P is
    /// connected, on any connection; once P has no connection it is one again, subject to item 1.
    #[test]
    fn connected_seed_is_not_dialed() {
        let start = Instant::now();
        let mut guards = guards();
        let seed = addr(1);
        let bootstrap = vec![seed.clone()];
        let peer = PeerId::random();
        let connection = ConnectionId::new_unchecked(7);

        // Start-up dial, matched to the address by its connection ID.
        guards.dialed.record(seed.clone(), start);
        guards.seeds.dial_started(seed.clone(), connection);
        guards.seeds.connection_established(connection, peer);

        // P is connected, but not through the address (an inbound connection): the address check does not match.
        // Even after the guard interval the address is not a candidate.
        let connected = |p: &PeerId| *p == peer;
        for tick in [1, 5, 10, 11, 100] {
            let candidates = guards.dialable_bootstrap(&bootstrap, secs(start, tick), |_| false, connected);
            assert!(candidates.is_empty(), "tick {}", tick);
        }

        // P has no connection: the address is dialable after the interval, and not before it (item 1).
        let none = |_: &PeerId| false;
        assert!(guards
            .dialable_bootstrap(&bootstrap, secs(start, 9), |_| false, none)
            .is_empty());
        assert_eq!(
            guards.dialable_bootstrap(&bootstrap, secs(start, 10), |_| false, none),
            vec![&seed]
        );
    }

    /// R27a item 2: before the first outbound connection to the address was established, only the address check
    /// applies; a dial that failed leaves no peer behind.
    #[test]
    fn unknown_seed_peer_leaves_only_the_address_check() {
        let start = Instant::now();
        let mut guards = guards();
        let seed = addr(1);
        let bootstrap = vec![seed.clone()];
        let connection = ConnectionId::new_unchecked(1);

        guards.seeds.dial_started(seed.clone(), connection);
        guards.seeds.dial_failed(connection);
        assert!(guards.seeds.pending.is_empty());
        assert!(guards.seeds.peers.is_empty());

        // Every peer is "connected", yet nothing is known about the address, so only the address check decides.
        let candidates = guards.dialable_bootstrap(&bootstrap, start, |_| false, |_| true);
        assert_eq!(candidates, vec![&seed]);
        let candidates = guards.dialable_bootstrap(&bootstrap, start, |_| true, |_| true);
        assert!(candidates.is_empty());
    }

    /// An event of a connection that no dial of ours started (an inbound connection, a peer-exchange dial) changes
    /// nothing; a failed dial removes only its own pending entry; the maps stay bounded by the bootstrap list.
    #[test]
    fn seed_peers_follow_connection_ids_and_stay_bounded() {
        let mut seeds = SeedPeers::default();
        let seed = addr(1);
        let other = addr(2);
        let peer = PeerId::random();

        seeds.dial_started(seed.clone(), ConnectionId::new_unchecked(1));
        seeds.connection_established(ConnectionId::new_unchecked(99), PeerId::random());
        assert!(seeds.peers.is_empty());
        assert_eq!(seeds.pending.len(), 1);

        // A newer dial of the same address replaces the pending one; the failure of the older one is ignored.
        seeds.dial_started(seed.clone(), ConnectionId::new_unchecked(2));
        assert_eq!(seeds.pending.len(), 1);
        seeds.dial_failed(ConnectionId::new_unchecked(1));
        assert_eq!(seeds.pending.len(), 1);

        // A dial refused by the swarm removes its entry, but only if it is the newest one.
        seeds.dial_not_scheduled(&seed, ConnectionId::new_unchecked(1));
        assert_eq!(seeds.pending.len(), 1);
        seeds.dial_not_scheduled(&seed, ConnectionId::new_unchecked(2));
        assert!(seeds.pending.is_empty());

        seeds.dial_started(other.clone(), ConnectionId::new_unchecked(3));
        seeds.connection_established(ConnectionId::new_unchecked(3), peer);
        assert!(seeds.pending.is_empty());
        assert_eq!(seeds.peer_of(&other), Some(&peer));

        // The last outbound connection wins, and a later failed dial keeps the recorded peer.
        let newer_peer = PeerId::random();
        seeds.dial_started(other.clone(), ConnectionId::new_unchecked(4));
        seeds.dial_failed(ConnectionId::new_unchecked(4));
        assert_eq!(seeds.peer_of(&other), Some(&peer));
        seeds.dial_started(other.clone(), ConnectionId::new_unchecked(5));
        seeds.connection_established(ConnectionId::new_unchecked(5), newer_peer);
        assert_eq!(seeds.peer_of(&other), Some(&newer_peer));
        assert_eq!(seeds.peers.len(), 1);
    }

    /// R27a item 3: the line is logged at WARN at most once per interval while the count is unchanged; a changed
    /// count is logged at WARN at once; the interval counts from the last WARN.
    #[test]
    fn short_of_relays_line_is_limited() {
        let start = Instant::now();
        let mut log = ShortOfRelaysLog::new(INTERVAL);

        assert_eq!(log.level(1, start), Level::Warn);
        for tick in 1..10 {
            assert_eq!(log.level(1, secs(start, tick)), Level::Debug, "tick {}", tick);
        }
        assert_eq!(log.level(1, secs(start, 10)), Level::Warn);
        assert_eq!(log.level(1, secs(start, 11)), Level::Debug);

        // A changed count is reported at once, and starts a new interval.
        assert_eq!(log.level(0, secs(start, 12)), Level::Warn);
        assert_eq!(log.level(0, secs(start, 13)), Level::Debug);
        assert_eq!(log.level(1, secs(start, 14)), Level::Warn);
    }

    /// The production setting is the 5-minute interval and the 30 s tick.
    #[test]
    fn default_config_is_the_production_setting() {
        let config = RedialGuardConfig::default();
        assert_eq!(config.interval, Duration::from_secs(300));
        assert_eq!(config.tick, Duration::from_secs(30));
        assert_eq!(MAX_DIALED_ADDRESSES, 1024);
    }
}
