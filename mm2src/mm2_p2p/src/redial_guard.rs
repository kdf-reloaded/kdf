//! Redial guards of the relay-connection maintenance (CRD ch.28 R27a).
//!
//! The start-up dials (R23 item 2) and the 30 s maintenance of RQ1a item 4 consult one [`RedialGuards`] value:
//!
//! 1. [`DialRecord`] (R27a item 1) remembers the addresses that were dialed, as written, and refuses to dial one
//!    again before the interval has passed.
//! 2. [`SeedPeers`] (R27a item 2) remembers which peer the last outbound connection to each configured bootstrap
//!    address reached, so that the address is not dialed while that peer is connected through any connection.
//! 3. [`ShortOfRelaysLog`] (R27a item 3) limits the "below low watermark" WARN line.
//! 4. [`EarlyRedials`] (R27a item 1a) lets an address that this node dialed be dialed again before the interval of
//!    item 1 has passed, when the connection that the dial made lasted a while and was then cut.
//!
//! Every method takes the current time as an argument and never reads the clock itself, so that the tests drive it.
//! The time must not go backwards: the monotonic clock of the WebAssembly timer asserts on it.

use crate::atomicdex_behaviour::CONNECTED_RELAYS_CHECK_INTERVAL;
use libp2p::{swarm::{ConnectionError, ConnectionId},
             Multiaddr, PeerId};
use log::Level;
use std::{collections::HashMap, io, time::Duration};
use wasm_timer::Instant;

/// How long a dialed address is not dialed again (R27a item 1) and how long the short-of-relays WARN line is held
/// back while the number of connected relays is unchanged (R27a item 3).
pub(crate) const REDIAL_GUARD_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How many dialed addresses are remembered (R27a item 1). When the record is full, the oldest entry is dropped.
pub(crate) const MAX_DIALED_ADDRESSES: usize = 1024;
/// How long a dialed connection must have lasted for its address to be dialed again early (R27a item 1a).
pub(crate) const EARLY_REDIAL_MIN_LIFETIME: Duration = Duration::from_secs(30);
/// How many early redials of one address are allowed within the guard interval (R27a item 1a).
pub(crate) const MAX_EARLY_REDIALS: usize = 2;
/// The text of the error that the bounded close of the multiplexer ends with (R27c).
const CLOSE_TIMED_OUT_TEXT: &str = "close timed out";

/// The timings of the maintenance and of its guards. The default is the production setting; a test shortens both.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RedialGuardConfig {
    /// The guard interval of R27a items 1 and 3.
    pub(crate) interval: Duration,
    /// The period of the relay-connection maintenance tick (RQ1a item 4).
    pub(crate) tick: Duration,
    /// How long a dialed connection must have lasted for an early redial of its address (R27a item 1a).
    pub(crate) early_redial_min_lifetime: Duration,
}

impl Default for RedialGuardConfig {
    fn default() -> Self {
        RedialGuardConfig {
            interval: REDIAL_GUARD_INTERVAL,
            tick: CONNECTED_RELAYS_CHECK_INTERVAL,
            early_redial_min_lifetime: EARLY_REDIAL_MIN_LIFETIME,
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

/// A close that this node started on purpose and recorded where it started it (R27a item 1a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalClose {
    /// The ping safeguard (R9). The only local close that allows an early redial.
    PingSafeguard,
    /// Any other close of this node that was recorded when it was started: the peer-clock admission check (RP2) and
    /// the relay-count maintenance (R27), which their call sites record, and the stuck-peer close (RQ1 item 5), which
    /// gossipsub marks.
    Other,
}

/// How a dialed connection ended, as far as R27a item 1a tells the causes apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseCause {
    /// A transport error or a close from the peer: the swarm reports both as an I/O error of the connection, and
    /// nothing on this node's side started the close. Allows an early redial.
    Transport,
    /// The ping safeguard (R9) closed it. Allows an early redial.
    PingSafeguard,
    /// The idle timeout (keep-alive). No early redial.
    IdleTimeout,
    /// The bounded close of R27c was abandoned at its deadline. No early redial.
    CloseTimedOut,
    /// A connection handler failed. R27a item 1a does not list it, so no early redial.
    HandlerError,
    /// A close that this node started and that was not recorded as the ping safeguard: the stuck-peer close (RQ1
    /// item 5, marked by gossipsub), the admission close (RP2), the relay-count close (R27), or a close with another
    /// purpose. No early redial.
    LocalClose,
}

impl CloseCause {
    /// Whether a connection that ended this way and lasted long enough allows an early redial of its address.
    pub(crate) fn allows_early_redial(self) -> bool {
        matches!(self, CloseCause::Transport | CloseCause::PingSafeguard)
    }
}

/// Whether `error` is the error of a close that was abandoned at its deadline (R27c). The multiplexer reports it as an
/// I/O error of kind `TimedOut` with this text, which the boxed multiplexer wraps into another I/O error.
fn is_close_timed_out(error: &io::Error) -> bool { error.to_string().contains(CLOSE_TIMED_OUT_TEXT) }

/// Tells the cause of a `SwarmEvent::ConnectionClosed` (R27a item 1a).
///
/// `cause` is the cause of the event. The swarm reports `None` when the close was started on this node's side and
/// finished without an error, whatever started it, so `None` alone does not name the reason: `local` is what was
/// recorded when the close was started (by its call site, or by gossipsub for the stuck-peer close). A close that ended with an error is a transport error or a close from the
/// peer, unless `local` says that this node had started it. The idle timeout, the handler error and the R27c timeout
/// are told by their error.
pub(crate) fn classify_close<E>(cause: Option<&ConnectionError<E>>, local: Option<LocalClose>) -> CloseCause {
    match cause {
        Some(ConnectionError::IO(error)) if is_close_timed_out(error) => CloseCause::CloseTimedOut,
        Some(ConnectionError::KeepAliveTimeout) => CloseCause::IdleTimeout,
        Some(ConnectionError::Handler(_)) => CloseCause::HandlerError,
        Some(ConnectionError::IO(_)) | None => match (local, cause.is_some()) {
            (Some(LocalClose::PingSafeguard), _) => CloseCause::PingSafeguard,
            (Some(LocalClose::Other), _) => CloseCause::LocalClose,
            (None, true) => CloseCause::Transport,
            (None, false) => CloseCause::LocalClose,
        },
    }
}

/// A connection this node dialed and that was established.
struct DialedConnection {
    peer: PeerId,
    /// The address that was dialed, as written.
    address: Multiaddr,
    established_at: Instant,
    /// A close of this node's that a call site recorded, other than the ping safeguard.
    local_close: Option<LocalClose>,
}

/// What a close of a dialed connection means for the early redial of its address (for the log).
pub(crate) struct DialedClose {
    /// The address that was dialed, as written.
    pub(crate) address: Multiaddr,
    pub(crate) cause: CloseCause,
    /// How long the connection lasted.
    pub(crate) lifetime: Duration,
    /// Whether the address may now be dialed before the interval of item 1 has passed.
    pub(crate) early_redial_allowed: bool,
}

/// The early-redial state of one address.
#[derive(Default)]
struct EarlyAddress {
    /// When the connection that allows an early redial ended; `None` once the address is not allowed.
    allowed_at: Option<Instant>,
    /// When the early redials of the address within the interval were made, oldest first.
    redialed_at: Vec<Instant>,
}

/// Early redial after a cut (R27a item 1a).
///
/// Every dial that this node makes is registered with the identifier of its connection (the dial options carry it
/// before the dial starts), so that the address as written is known when the connection is established and when it
/// closes. A connection that lasted at least the minimum lifetime and ended on a cause that [`CloseCause`] allows
/// marks its address as allowed. The marked address is not guarded by the dial record at a maintenance tick below the
/// low watermark, until it is dialed or the cap of [`MAX_EARLY_REDIALS`] within the interval is reached.
pub(crate) struct EarlyRedials {
    min_lifetime: Duration,
    interval: Duration,
    /// The dials in flight, by connection identifier. An entry ends with the connection or the failure of its dial.
    pending: HashMap<ConnectionId, Multiaddr>,
    /// The established connections that this node dialed. An entry ends with the close of the connection.
    established: HashMap<ConnectionId, DialedConnection>,
    /// The addresses that are allowed, or that were redialed early within the interval. Bounded by
    /// [`MAX_DIALED_ADDRESSES`].
    addresses: HashMap<Multiaddr, EarlyAddress>,
}

impl EarlyRedials {
    pub(crate) fn new(min_lifetime: Duration, interval: Duration) -> Self {
        EarlyRedials {
            min_lifetime,
            interval,
            pending: HashMap::new(),
            established: HashMap::new(),
            addresses: HashMap::new(),
        }
    }

    /// A dial of `address`, as written, with the identifier `connection_id` has been started.
    pub(crate) fn dial_started(&mut self, address: Multiaddr, connection_id: ConnectionId) {
        self.pending.insert(connection_id, address);
    }

    /// The dial with `connection_id` has failed, or the swarm refused to schedule it, so no connection follows.
    pub(crate) fn dial_ended(&mut self, connection_id: ConnectionId) { self.pending.remove(&connection_id); }

    /// The outbound connection with `connection_id` was established at `now` and reached `peer`. A connection that no
    /// dial of ours started is ignored.
    pub(crate) fn connection_established(&mut self, connection_id: ConnectionId, peer: PeerId, now: Instant) {
        if let Some(address) = self.pending.remove(&connection_id) {
            self.established.insert(connection_id, DialedConnection {
                peer,
                address,
                established_at: now,
                local_close: None,
            });
        }
    }

    /// This node has started to close the connections to `peer` for the reason `close`. It marks the connections to
    /// `peer` that it dialed, so that their close is not taken for a transport error. The ping safeguard and the
    /// stuck-peer close are not recorded here: [`AdexPing`](crate::adex_ping::AdexPing) and gossipsub record them.
    pub(crate) fn mark_local_close(&mut self, peer: PeerId, close: LocalClose) {
        for connection in self
            .established
            .values_mut()
            .filter(|connection| connection.peer == peer)
        {
            connection.local_close = Some(close);
        }
    }

    /// The connection with `connection_id` closed at `now` with `cause`. `closed_by_ping` is whether the ping
    /// safeguard had started the close, `closed_by_stuck` whether the stuck-peer close of RQ1 item 5 had (gossipsub
    /// marks it); the latter counts as [`LocalClose::Other`]. Returns what the close means for its address, or `None` when this node did
    /// not dial the connection.
    pub(crate) fn connection_closed<E>(
        &mut self,
        connection_id: ConnectionId,
        cause: Option<&ConnectionError<E>>,
        closed_by_ping: bool,
        closed_by_stuck: bool,
        now: Instant,
    ) -> Option<DialedClose> {
        let connection = self.established.remove(&connection_id)?;
        // When more than one recorded a close, the one that does not allow an early redial wins.
        let local = match (connection.local_close, closed_by_ping, closed_by_stuck) {
            (Some(LocalClose::Other), ..) | (_, _, true) => Some(LocalClose::Other),
            (_, true, false) => Some(LocalClose::PingSafeguard),
            (recorded, false, false) => recorded,
        };
        let cause = classify_close(cause, local);
        let lifetime = now.duration_since(connection.established_at);
        let early_redial_allowed = cause.allows_early_redial() && lifetime >= self.min_lifetime;
        if early_redial_allowed {
            self.allow(connection.address.clone(), now);
        }
        Some(DialedClose {
            address: connection.address,
            cause,
            lifetime,
            early_redial_allowed,
        })
    }

    /// Marks `address` as allowed to be dialed early.
    fn allow(&mut self, address: Multiaddr, now: Instant) {
        self.prune(now);
        if self.addresses.len() >= MAX_DIALED_ADDRESSES && !self.addresses.contains_key(&address) {
            let oldest = self
                .addresses
                .iter()
                .min_by_key(|(_, state)| state.last_activity())
                .map(|(address, _)| address.clone());
            if let Some(oldest) = oldest {
                self.addresses.remove(&oldest);
            }
        }
        self.addresses.entry(address).or_default().allowed_at = Some(now);
    }

    /// Forgets what is older than the interval. An allowance older than the interval is of no use: the dial that the
    /// connection came from was made before the connection ended, so its guard has lapsed.
    fn prune(&mut self, now: Instant) {
        let interval = self.interval;
        self.addresses.retain(|_, state| {
            state.redialed_at.retain(|at| now.duration_since(*at) < interval);
            state.allowed_at = state.allowed_at.filter(|at| now.duration_since(*at) < interval);
            state.allowed_at.is_some() || !state.redialed_at.is_empty()
        });
    }

    /// Whether `address` may be dialed at `now` despite the dial record: it is allowed, and it has had fewer than
    /// [`MAX_EARLY_REDIALS`] early redials within the interval.
    pub(crate) fn allows(&self, address: &Multiaddr, now: Instant) -> bool {
        self.addresses.get(address).is_some_and(|state| {
            let recent = state
                .redialed_at
                .iter()
                .filter(|at| now.duration_since(**at) < self.interval)
                .count();
            state.allowed_at.is_some() && recent < MAX_EARLY_REDIALS
        })
    }

    /// `address` is being dialed at `now`. When `early` is set, the dial is an early redial and counts against the
    /// cap. Either way the allowance is used up: the address is guarded by the dial record again, and is allowed
    /// again only by a new connection that lasted long enough.
    fn dial_recorded(&mut self, address: &Multiaddr, now: Instant, early: bool) {
        self.prune(now);
        if let Some(state) = self.addresses.get_mut(address) {
            state.allowed_at = None;
            if early {
                state.redialed_at.push(now);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize { self.addresses.len() }
}

impl EarlyAddress {
    /// The latest time at which something was recorded for the address.
    fn last_activity(&self) -> Option<Instant> {
        self.allowed_at
            .into_iter()
            .chain(self.redialed_at.last().copied())
            .max()
    }
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
    pub(crate) early: EarlyRedials,
    pub(crate) short_of_relays: ShortOfRelaysLog,
}

impl RedialGuards {
    pub(crate) fn new(config: RedialGuardConfig) -> Self {
        RedialGuards {
            dialed: DialRecord::new(config.interval),
            seeds: SeedPeers::default(),
            early: EarlyRedials::new(config.early_redial_min_lifetime, config.interval),
            short_of_relays: ShortOfRelaysLog::new(config.interval),
        }
    }

    /// Whether `address` is guarded at `now`: it was dialed less than the interval before (item 1) and no early
    /// redial is allowed for it (item 1a).
    pub(crate) fn is_guarded(&self, address: &Multiaddr, now: Instant) -> bool {
        self.dialed.is_guarded(address, now) && !self.early.allows(address, now)
    }

    /// Records a dial of `address` at `now` for item 1. A dial that item 1a allowed counts against the cap of early
    /// redials of the address, and uses up the allowance.
    pub(crate) fn record_dial(&mut self, address: &Multiaddr, now: Instant) {
        let early = self.dialed.is_guarded(address, now) && self.early.allows(address, now);
        self.early.dial_recorded(address, now, early);
        self.dialed.record(address.clone(), now);
    }

    /// The addresses of a known peer that maintenance may dial at `now`: those that are not guarded by the dial
    /// record (item 1, unless item 1a allows an early redial) and are not the remote address of an open connection (RQ1a item 6).
    pub(crate) fn dialable_peer_addresses(
        &self,
        addresses: impl IntoIterator<Item = Multiaddr>,
        now: Instant,
        is_connected_to_addr: impl Fn(&Multiaddr) -> bool,
    ) -> Vec<Multiaddr> {
        addresses
            .into_iter()
            .filter(|address| !self.is_guarded(address, now) && !is_connected_to_addr(address))
            .collect()
    }

    /// The bootstrap addresses that maintenance may dial at `now`: those that are not guarded by the dial record
    /// (item 1, unless item 1a allows an early redial), are not the remote address of an open connection (RQ1a item 6) and do not belong to a connected
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
                !self.is_guarded(address, now)
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
    /// The shortened minimum lifetime of a dialed connection for an early redial (R27a item 1a).
    const MIN_LIFETIME: Duration = Duration::from_secs(3);

    fn addr(n: usize) -> Multiaddr { format!("/ip4/203.0.113.{}/tcp/42845", n % 250).parse().unwrap() }

    fn addr_port(n: usize) -> Multiaddr { format!("/ip4/203.0.113.7/tcp/{}", 1000 + n).parse().unwrap() }

    fn guards() -> RedialGuards {
        RedialGuards::new(RedialGuardConfig {
            interval: INTERVAL,
            tick: Duration::from_secs(1),
            early_redial_min_lifetime: MIN_LIFETIME,
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
        assert_eq!(config.early_redial_min_lifetime, Duration::from_secs(30));
        assert_eq!(MAX_EARLY_REDIALS, 2);
        assert_eq!(MAX_DIALED_ADDRESSES, 1024);
    }

    // ---- CRD ch.28 T15 (R27a item 1a): early redial after a cut ----

    /// A fresh set of guards. Tests that shadow `guards` with a local variable use this instead of `guards()`.
    fn new_guards() -> RedialGuards { guards() }

    fn io_error(kind: io::ErrorKind, text: &str) -> ConnectionError<()> {
        ConnectionError::IO(io::Error::new(kind, text.to_owned()))
    }

    /// The cause of a connection that a transport error ended, or that the peer closed: the swarm reports an I/O
    /// error.
    fn transport_error() -> ConnectionError<()> { io_error(io::ErrorKind::ConnectionReset, "reset") }

    /// Dials `address` at `at` (a bootstrap or maintenance dial, as the dial functions do) and establishes the
    /// connection with the identifier `id` to `peer` at the same time.
    fn dial_and_establish(guards: &mut RedialGuards, address: &Multiaddr, id: u64, peer: PeerId, at: Instant) {
        let connection = ConnectionId::new_unchecked(id as usize);
        guards.record_dial(address, at);
        guards.early.dial_started(address.clone(), connection);
        guards.early.connection_established(connection, peer, at);
    }

    /// Closes the connection with the identifier `id` at `at`.
    fn close(
        guards: &mut RedialGuards,
        id: u64,
        cause: Option<&ConnectionError<()>>,
        closed_by_ping: bool,
        at: Instant,
    ) -> DialedClose {
        close_with_stuck_mark(guards, id, cause, closed_by_ping, false, at)
    }

    /// Closes the connection with the identifier `id` at `at`, with gossipsub's answer whether the stuck-peer close
    /// had started the close.
    fn close_with_stuck_mark(
        guards: &mut RedialGuards,
        id: u64,
        cause: Option<&ConnectionError<()>>,
        closed_by_ping: bool,
        closed_by_stuck: bool,
        at: Instant,
    ) -> DialedClose {
        guards
            .early
            .connection_closed(
                ConnectionId::new_unchecked(id as usize),
                cause,
                closed_by_ping,
                closed_by_stuck,
                at,
            )
            .expect("the connection was dialed by this node")
    }

    /// Whether maintenance at a tick below the low watermark may dial `address` at `now`.
    fn dialable(guards: &RedialGuards, address: &Multiaddr, now: Instant) -> bool {
        guards
            .dialable_bootstrap(std::slice::from_ref(address), now, |_| false, |_| false)
            .len()
            == 1
    }

    /// T15: the causes are told apart as R27a item 1a lists them.
    #[test]
    fn close_causes_are_classified() {
        let timed_out = || io::Error::new(io::ErrorKind::TimedOut, CLOSE_TIMED_OUT_TEXT);
        // The multiplexer's error reaches the swarm wrapped into another I/O error.
        let wrapped_timed_out = ConnectionError::<()>::IO(io::Error::other(timed_out()));
        let bare_timed_out = ConnectionError::<()>::IO(timed_out());
        // A transport timeout that is not the bounded close is a transport error.
        let transport_timeout = io_error(io::ErrorKind::TimedOut, "connection timed out");

        for (cause, local, want) in [
            (Some(&transport_error()), None, CloseCause::Transport),
            (Some(&transport_timeout), None, CloseCause::Transport),
            (
                Some(&ConnectionError::<()>::KeepAliveTimeout),
                None,
                CloseCause::IdleTimeout,
            ),
            (
                Some(&ConnectionError::<()>::Handler(())),
                None,
                CloseCause::HandlerError,
            ),
            (Some(&bare_timed_out), None, CloseCause::CloseTimedOut),
            (Some(&wrapped_timed_out), None, CloseCause::CloseTimedOut),
            // A close that this node started and that ended without an error.
            (None, None, CloseCause::LocalClose),
            (None, Some(LocalClose::Other), CloseCause::LocalClose),
            (None, Some(LocalClose::PingSafeguard), CloseCause::PingSafeguard),
            // A close that this node started and that the transport cut short with an error.
            (
                Some(&transport_error()),
                Some(LocalClose::Other),
                CloseCause::LocalClose,
            ),
            (
                Some(&transport_error()),
                Some(LocalClose::PingSafeguard),
                CloseCause::PingSafeguard,
            ),
            // The error decides when the connection had ended on its own before the recorded close took effect.
            (
                Some(&ConnectionError::<()>::KeepAliveTimeout),
                Some(LocalClose::PingSafeguard),
                CloseCause::IdleTimeout,
            ),
            // The bounded close of R27c is never an early redial, whoever started the close.
            (
                Some(&wrapped_timed_out),
                Some(LocalClose::PingSafeguard),
                CloseCause::CloseTimedOut,
            ),
        ] {
            assert_eq!(classify_close(cause, local), want, "{:?} / {:?}", cause, local);
        }

        assert!(CloseCause::Transport.allows_early_redial());
        assert!(CloseCause::PingSafeguard.allows_early_redial());
        for cause in [
            CloseCause::IdleTimeout,
            CloseCause::CloseTimedOut,
            CloseCause::HandlerError,
            CloseCause::LocalClose,
        ] {
            assert!(!cause.allows_early_redial(), "{:?}", cause);
        }
    }

    /// T15: a dialed connection that lasted at least the minimum lifetime and ended on a transport error or a close
    /// from the peer (the swarm reports both as an I/O error), or on the ping safeguard, makes its address dialable at
    /// the next tick below the low watermark, well before the interval of item 1 has passed.
    #[test]
    fn exempt_cause_after_the_minimum_lifetime_allows_an_early_redial() {
        let start = Instant::now();
        let address = addr(1);

        for (id, cause, by_ping, want_cause) in [
            (1, Some(transport_error()), false, CloseCause::Transport),
            (2, None, true, CloseCause::PingSafeguard),
            (3, Some(transport_error()), true, CloseCause::PingSafeguard),
        ] {
            let mut guards = new_guards();
            dial_and_establish(&mut guards, &address, id, PeerId::random(), start);
            // While the connection is open the address is guarded, as before.
            assert!(!dialable(&guards, &address, secs(start, 2)));

            let closed = close(&mut guards, id, cause.as_ref(), by_ping, secs(start, 4));
            assert_eq!(closed.cause, want_cause);
            assert_eq!(closed.address, address);
            assert_eq!(closed.lifetime, Duration::from_secs(4));
            assert!(closed.early_redial_allowed);

            // The next tick, long before the interval has passed.
            assert!(guards.dialed.is_guarded(&address, secs(start, 5)));
            assert!(!guards.is_guarded(&address, secs(start, 5)));
            assert!(dialable(&guards, &address, secs(start, 5)));
            // The address of a known peer is dialable in the same way.
            assert_eq!(
                guards.dialable_peer_addresses(vec![address.clone()], secs(start, 5), |_| false),
                vec![address.clone()]
            );
        }
    }

    /// T15: "at least" the minimum lifetime: a connection that lasted exactly as long qualifies; one that lasted less
    /// does not, however it ended, and its address stays guarded until the interval of item 1 has passed.
    #[test]
    fn connection_shorter_than_the_minimum_lifetime_stays_guarded() {
        let start = Instant::now();
        let address = addr(1);

        let mut guards = new_guards();
        dial_and_establish(&mut guards, &address, 1, PeerId::random(), start);
        let closed = close(&mut guards, 1, Some(&transport_error()), false, secs(start, 3));
        assert!(closed.early_redial_allowed, "exactly the minimum lifetime qualifies");

        for (cause, by_ping) in [(Some(transport_error()), false), (None, true)] {
            let mut guards = new_guards();
            dial_and_establish(&mut guards, &address, 1, PeerId::random(), start);
            let closed = close(
                &mut guards,
                1,
                cause.as_ref(),
                by_ping,
                start + Duration::from_millis(2_999),
            );
            assert!(!closed.early_redial_allowed);
            for tick in 3..10 {
                assert!(!dialable(&guards, &address, secs(start, tick)), "tick {}", tick);
            }
            // The interval of item 1 ends the guard, as before.
            assert!(dialable(&guards, &address, secs(start, 10)));
        }
    }

    /// T15: the idle timeout, the R27c close timeout (bare and wrapped), a handler error, and every close that this
    /// node started for another reason (the stuck-peer close, the admission close of RP2, the relay-count close of
    /// R27) leave the address guarded, however long the connection lasted. Gossipsub's mark of the stuck-peer close
    /// is the input `closed_by_stuck`; the admission and relay-count closes are recorded by their call sites.
    #[test]
    fn non_exempt_causes_stay_guarded() {
        let start = Instant::now();
        let address = addr(1);
        let peer = PeerId::random();
        let timed_out = || io::Error::new(io::ErrorKind::TimedOut, CLOSE_TIMED_OUT_TEXT);

        // (cause of the event, whether a call site recorded a close of this node's other than the ping safeguard)
        let cases: Vec<(&str, Option<ConnectionError<()>>, bool)> = vec![
            ("idle timeout", Some(ConnectionError::KeepAliveTimeout), false),
            ("close timed out", Some(ConnectionError::IO(timed_out())), false),
            (
                "close timed out, wrapped",
                Some(ConnectionError::IO(io::Error::other(timed_out()))),
                false,
            ),
            ("handler error", Some(ConnectionError::Handler(())), false),
            // A close that ends without an error and that nothing marked.
            ("unmarked clean close", None, false),
            // The admission close and the relay-count close are recorded where the node calls the close.
            ("recorded close, clean", None, true),
            ("recorded close, cut short by an error", Some(transport_error()), true),
        ];
        for (name, cause, recorded) in cases {
            let mut guards = new_guards();
            dial_and_establish(&mut guards, &address, 1, peer, start);
            if recorded {
                guards.early.mark_local_close(peer, LocalClose::Other);
            }
            let closed = close(&mut guards, 1, cause.as_ref(), false, secs(start, 6));
            assert!(!closed.early_redial_allowed, "{}", name);
            assert!(!closed.cause.allows_early_redial(), "{}", name);
            for tick in 6..10 {
                assert!(
                    !dialable(&guards, &address, secs(start, tick)),
                    "{}, tick {}",
                    name,
                    tick
                );
            }
            assert!(dialable(&guards, &address, secs(start, 10)), "{}", name);
        }

        // The stuck-peer close, which gossipsub marks, whatever the connection ended with, and whether or not the
        // ping safeguard had also marked it (T15, R27a item 1a).
        for (name, cause, by_ping) in [
            ("stuck-peer close, clean", None, false),
            (
                "stuck-peer close, cut short by an error",
                Some(transport_error()),
                false,
            ),
            ("stuck-peer close and ping safeguard, clean", None, true),
            (
                "stuck-peer close and ping safeguard, cut short by an error",
                Some(transport_error()),
                true,
            ),
        ] {
            let mut guards = new_guards();
            dial_and_establish(&mut guards, &address, 1, peer, start);
            let closed = close_with_stuck_mark(&mut guards, 1, cause.as_ref(), by_ping, true, secs(start, 6));
            assert_eq!(closed.cause, CloseCause::LocalClose, "{}", name);
            assert!(!closed.early_redial_allowed, "{}", name);
            for tick in 6..10 {
                assert!(
                    !dialable(&guards, &address, secs(start, tick)),
                    "{}, tick {}",
                    name,
                    tick
                );
            }
            assert!(dialable(&guards, &address, secs(start, 10)), "{}", name);
        }

        // A recorded close of another purpose wins over the ping safeguard.
        let mut guards = new_guards();
        dial_and_establish(&mut guards, &address, 1, peer, start);
        guards.early.mark_local_close(peer, LocalClose::Other);
        let closed = close(&mut guards, 1, None, true, secs(start, 6));
        assert_eq!(closed.cause, CloseCause::LocalClose);
        assert!(!dialable(&guards, &address, secs(start, 7)));
    }

    /// T15: the stuck-peer mark of gossipsub changes only the connection that it names: without it, the same close
    /// that ended with an I/O error is a transport error and allows an early redial, as before.
    #[test]
    fn stuck_mark_decides_between_transport_error_and_local_close() {
        let start = Instant::now();
        let address = addr(1);
        for (id, marked, want_cause, want_early) in [
            (1, false, CloseCause::Transport, true),
            (2, true, CloseCause::LocalClose, false),
        ] {
            let mut guards = new_guards();
            dial_and_establish(&mut guards, &address, id, PeerId::random(), start);
            let closed =
                close_with_stuck_mark(&mut guards, id, Some(&transport_error()), false, marked, secs(start, 6));
            assert_eq!(closed.cause, want_cause);
            assert_eq!(closed.early_redial_allowed, want_early);
            assert_eq!(dialable(&guards, &address, secs(start, 7)), want_early);
        }
    }

    /// The close of a connection that this node did not dial changes nothing, and a record of a close of this node's
    /// marks only the connections to that peer.
    #[test]
    fn only_dialed_connections_of_the_marked_peer_are_affected() {
        let start = Instant::now();
        let (first, second) = (addr(1), addr(2));
        let (peer_a, peer_b) = (PeerId::random(), PeerId::random());
        let mut guards = new_guards();

        // An inbound connection, or one that a peer-exchange dial of another node made: no dial of ours started it.
        guards
            .early
            .connection_established(ConnectionId::new_unchecked(50), peer_a, start);
        assert!(guards
            .early
            .connection_closed(
                ConnectionId::new_unchecked(50),
                Some(&transport_error()),
                false,
                false,
                secs(start, 5)
            )
            .is_none());

        dial_and_establish(&mut guards, &first, 1, peer_a, start);
        dial_and_establish(&mut guards, &second, 2, peer_b, start);
        guards.early.mark_local_close(peer_a, LocalClose::Other);
        let closed_a = close(&mut guards, 1, Some(&transport_error()), false, secs(start, 5));
        let closed_b = close(&mut guards, 2, Some(&transport_error()), false, secs(start, 5));
        assert_eq!(closed_a.cause, CloseCause::LocalClose);
        assert_eq!(closed_b.cause, CloseCause::Transport);
        assert!(!dialable(&guards, &first, secs(start, 6)));
        assert!(dialable(&guards, &second, secs(start, 6)));

        // A dial that failed or was refused leaves no connection behind.
        let connection = ConnectionId::new_unchecked(9);
        guards.early.dial_started(first.clone(), connection);
        guards.early.dial_ended(connection);
        assert!(guards.early.pending.is_empty());
        guards.early.connection_established(connection, peer_a, secs(start, 6));
        assert!(guards.early.established.is_empty());
    }

    /// T15, the cap: after two early redials within the interval the third is guarded, and the address is dialable
    /// again once the interval has passed (the sliding window of the last two early redials, and at the latest the
    /// guard of item 1).
    #[test]
    fn third_early_redial_within_the_interval_is_guarded() {
        let start = Instant::now();
        let address = addr(1);
        let peer = PeerId::random();
        let mut guards = new_guards();

        // The start-up dial at 0; its connection is cut at 3 s.
        dial_and_establish(&mut guards, &address, 1, peer, start);
        assert!(close(&mut guards, 1, Some(&transport_error()), false, secs(start, 3)).early_redial_allowed);

        // First early redial at the 3 s tick; that connection is cut at 6 s.
        assert!(dialable(&guards, &address, secs(start, 3)));
        dial_and_establish(&mut guards, &address, 2, peer, secs(start, 3));
        assert!(
            !dialable(&guards, &address, secs(start, 4)),
            "an early redial is a dial for item 1"
        );
        assert!(close(&mut guards, 2, Some(&transport_error()), false, secs(start, 6)).early_redial_allowed);

        // Second early redial at the 6 s tick; that connection is cut at 9 s.
        assert!(dialable(&guards, &address, secs(start, 6)));
        dial_and_establish(&mut guards, &address, 3, peer, secs(start, 6));
        assert!(close(&mut guards, 3, Some(&transport_error()), false, secs(start, 9)).early_redial_allowed);

        // A third one at 9 s would be the third within the interval: guarded, until the first early redial (at 3 s)
        // is an interval old.
        for tick in 9..13 {
            assert!(!dialable(&guards, &address, secs(start, tick)), "tick {}", tick);
        }
        assert!(dialable(&guards, &address, secs(start, 13)));
    }

    /// T15: an early dial is recorded as a dial for item 1, and a failed early dial falls back to item 1: the address
    /// is guarded for the interval from the early dial, and allowed again only by a new connection that lasted long
    /// enough.
    #[test]
    fn failed_early_dial_falls_back_to_the_guard() {
        let start = Instant::now();
        let address = addr(1);
        let mut guards = new_guards();

        dial_and_establish(&mut guards, &address, 1, PeerId::random(), start);
        close(&mut guards, 1, Some(&transport_error()), false, secs(start, 4));
        assert!(dialable(&guards, &address, secs(start, 5)));

        // The early dial at the 5 s tick fails: no connection follows.
        guards.record_dial(&address, secs(start, 5));
        let connection = ConnectionId::new_unchecked(2);
        guards.early.dial_started(address.clone(), connection);
        guards.early.dial_ended(connection);

        assert!(guards.dialed.is_guarded(&address, secs(start, 14)));
        for tick in 5..15 {
            assert!(!dialable(&guards, &address, secs(start, tick)), "tick {}", tick);
        }
        assert!(dialable(&guards, &address, secs(start, 15)));
    }

    /// T15: an allowed address waits for a tick at which the node is below its low watermark: maintenance consults
    /// the candidates only then, and the allowance is neither used up nor lost by the ticks that skip it.
    #[test]
    fn allowance_waits_for_a_tick_below_the_low_watermark() {
        let start = Instant::now();
        let address = addr(1);
        let mut guards = new_guards();

        dial_and_establish(&mut guards, &address, 1, PeerId::random(), start);
        close(&mut guards, 1, Some(&transport_error()), false, secs(start, 4));

        // Ticks at which the node is not below its low watermark dial nothing, so nothing is recorded.
        // The first tick below it can dial the address.
        assert!(dialable(&guards, &address, secs(start, 8)));
        // An allowed address is still not dialed while a connection to its address is open (RQ1a item 6) or while its
        // peer is connected (item 2).
        assert!(guards
            .dialable_bootstrap(std::slice::from_ref(&address), secs(start, 8), |_| true, |_| false)
            .is_empty());
        assert!(guards
            .dialable_peer_addresses(vec![address.clone()], secs(start, 8), |_| true)
            .is_empty());
    }

    /// An allowance expires with the interval, and the early-redial state stays within the bound of the dial record.
    #[test]
    fn early_state_expires_and_is_bounded() {
        let start = Instant::now();
        let mut early = EarlyRedials::new(MIN_LIFETIME, INTERVAL);
        let total = MAX_DIALED_ADDRESSES + 50;
        for n in 0..total {
            let at = start + Duration::from_millis(n as u64);
            early.allow(addr_port(n), at);
            assert!(early.len() <= MAX_DIALED_ADDRESSES);
        }
        assert_eq!(early.len(), MAX_DIALED_ADDRESSES);
        let now = start + Duration::from_secs(5);
        // The oldest were dropped, the newest remain.
        assert!(!early.allows(&addr_port(0), now));
        assert!(early.allows(&addr_port(total - 1), now));

        // Everything is forgotten once the interval has passed and the next entry is made.
        early.allow(addr_port(9_999), secs(start, 60));
        assert_eq!(early.len(), 1);
    }
}
