//! CRD ch.32 §32.7.3 R-Q5b: a pair still being synchronized is answered when its synchronization
//! lands.
//!
//! A relay forwards a keep-alive before it handles it, so its subscribers ask it for a pair while its
//! own keep-alive-driven request for that pair is still in flight, and the answer it can give at that
//! moment is the placeholder-root failure of R-Q5a. This module parks such a request instead of
//! answering it at once:
//!
//! * [`SyncAnswerDeferral`] holds two tables behind one lock: the own requests in flight (registered by
//!   the keep-alive path, [`SyncAnswerDeferral::track_own_request`]) and the parked requests;
//! * [`answer_sync_request`] is the entry point of the inbound dispatcher: it parks the request when it
//!   is deferrable and otherwise answers at once, as R-Q5 binds. Parking hands the response channel (as
//!   a [`SyncAnswerReply`]) to a task of its own and returns, so nothing waits on the inbound path
//!   (R-T3b item 3);
//! * [`SyncInFlight`] is the record of one own request. Whatever ends the keep-alive task (the answer was
//!   applied, the request failed, the future was dropped) drops it, and dropping it wakes the parked
//!   requests that waited for it;
//! * [`KeepAliveRegistration`] (R-Q5b item 6) is the record of a keep-alive a relay is about to compare, before
//!   its own request exists. A request for a pair such a keep-alive names is parked behind the registration,
//!   which ends the same way an own request does, and which expires at the bound whatever its task does;
//! * [`answer_orderbook_request`] (CRD ch.32 R-GH12a) lets the answer to an order-book request wait briefly
//!   for the trie worker; it holds a slot in the parked table, and nothing else of it is shared.
//!
//! Lock order (R-T3b): orderbook, then trie store, then the tables of this module, which are the
//! innermost lock and are never held across an await.

use super::*;
use futures::channel::oneshot;
use futures::future::{select, Either};
use mm2_p2p::atomicdex_behaviour::AdexResponse;
use mm2_p2p::PeerId;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// Default of the MM2.json integer `sync_answer_deferral_ms`.
pub(crate) const DEFAULT_SYNC_ANSWER_DEFERRAL_MS: u64 = 5000;
/// R-Q5b item 4: default of the MM2.json integer `sync_answer_deferral_max_per_peer`, parked requests per
/// requesting peer.
pub(crate) const DEFAULT_MAX_PARKED_PER_PEER: usize = 32;
/// R-Q5b item 4: the largest per-peer limit; a configured value above it is taken as this.
pub(crate) const CEILING_MAX_PARKED_PER_PEER: usize = 64;
/// R-Q5b item 4: default of the MM2.json integer `sync_answer_deferral_max_total`, parked requests in
/// total.
pub(crate) const DEFAULT_MAX_PARKED_TOTAL: usize = 2048;
/// R-Q5b item 4: the largest total limit; a configured value above it is taken as this.
pub(crate) const CEILING_MAX_PARKED_TOTAL: usize = 8192;

/// Number of inbound streams a connection negotiates concurrently. A parked request holds its inbound
/// stream in negotiation until the answer is written, so the per-peer cap must stay below it.
///
/// This is the default of `libp2p-swarm` (`connection/pool.rs`, `max_negotiating_inbound_streams: 128`)
/// in the pinned `rust-libp2p` tag `k-0.52.12`; the swarm of `mm2_p2p` is built with
/// `SwarmBuilder::with_executor(..).build()` and does not override it. The limit is per connection, so
/// it bounds [`CEILING_MAX_PARKED_PER_PEER`] (and so any configured per-peer limit);
/// [`CEILING_MAX_PARKED_TOTAL`] is a sum over connections and is not comparable with it.
const SWARM_MAX_NEGOTIATING_INBOUND_STREAMS: usize = 128;
/// Time after which the request-response layer abandons an inbound substream that has not been answered:
/// `RequestResponseConfig::default()` of `libp2p-request-response` 0.25.0 (`request_timeout`, 10 s),
/// which `build_request_response_behaviour` in `mm2_p2p` uses unchanged. It is also the requester's
/// timeout (R-Q6). The default bound must stay well below it.
const REQUEST_RESPONSE_TIMEOUT_MS: u64 = 10_000;

const _: () = assert!(CEILING_MAX_PARKED_PER_PEER < SWARM_MAX_NEGOTIATING_INBOUND_STREAMS);
const _: () = assert!(DEFAULT_SYNC_ANSWER_DEFERRAL_MS * 2 <= REQUEST_RESPONSE_TIMEOUT_MS);

/// Pause between two checks of the trie worker's progress.
const APPLIED_CHECK_STEP_MS: u64 = 2;

/// The bound of R-Q5b item 3: the top-level MM2.json integer `sync_answer_deferral_ms`, default `5000`;
/// an absent or non-integer value means the default, `0` turns deferral off. Operators who raise it must
/// keep it below the request-response timeout ([`REQUEST_RESPONSE_TIMEOUT_MS`]), or the transport
/// abandons the stream before the answer is written.
pub(crate) fn sync_answer_deferral_ms(ctx: &MmArc) -> u64 {
    ctx.conf["sync_answer_deferral_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_SYNC_ANSWER_DEFERRAL_MS)
}

/// A limit of R-Q5b item 4: the top-level MM2.json integer `key`. An absent, non-integer, negative or zero
/// value means `default`; a value above `ceiling` is taken as `ceiling`, and that is logged once per process
/// (`warned`), not per request.
fn parking_limit(ctx: &MmArc, key: &str, default: usize, ceiling: usize, warned: &AtomicBool) -> usize {
    let configured = match ctx.conf[key].as_u64() {
        Some(configured) if configured > 0 => configured,
        _ => return default,
    };
    if configured <= ceiling as u64 {
        return configured as usize;
    }
    if !warned.swap(true, AtomicOrdering::Relaxed) {
        log::warn!(
            "{} is {}, above the ceiling: {} is used instead",
            key,
            configured,
            ceiling
        );
    }
    ceiling
}

/// The per-peer limit of R-Q5b item 4: the top-level MM2.json integer `sync_answer_deferral_max_per_peer`,
/// default `32`, at most `64`.
pub(crate) fn sync_answer_deferral_max_per_peer(ctx: &MmArc) -> usize {
    static WARNED: AtomicBool = AtomicBool::new(false);
    parking_limit(
        ctx,
        "sync_answer_deferral_max_per_peer",
        DEFAULT_MAX_PARKED_PER_PEER,
        CEILING_MAX_PARKED_PER_PEER,
        &WARNED,
    )
}

/// The total limit of R-Q5b item 4: the top-level MM2.json integer `sync_answer_deferral_max_total`,
/// default `2048`, at most `8192`.
pub(crate) fn sync_answer_deferral_max_total(ctx: &MmArc) -> usize {
    static WARNED: AtomicBool = AtomicBool::new(false);
    parking_limit(
        ctx,
        "sync_answer_deferral_max_total",
        DEFAULT_MAX_PARKED_TOTAL,
        CEILING_MAX_PARKED_TOTAL,
        &WARNED,
    )
}

/// Default of the MM2.json integer `sync_request_retry_ms`.
pub(crate) const DEFAULT_SYNC_REQUEST_RETRY_MS: u64 = 1000;

// R-Q5c item 3: the default pause leaves room within the default bound for the second answer.
const _: () = assert!(DEFAULT_SYNC_REQUEST_RETRY_MS < DEFAULT_SYNC_ANSWER_DEFERRAL_MS);

/// The pause of CRD ch.32 R-Q5c before a failed keep-alive-driven request is asked once more: the top-level
/// MM2.json integer `sync_request_retry_ms`, default `1000`; an absent or non-integer value means the default,
/// `0` turns the second request off. The second request is sent only while it can still be answered within the
/// bound of [`sync_answer_deferral_ms`], so a pause at or above that bound turns it off as well.
pub(crate) fn sync_request_retry_ms(ctx: &MmArc) -> u64 {
    ctx.conf["sync_request_retry_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_SYNC_REQUEST_RETRY_MS)
}

/// The top-level MM2.json boolean `sync_request_alternate_relay` (CRD ch.32 R-Q5d item 7), default `true`;
/// only a JSON `false` turns the request to one other relay off.
pub(crate) fn sync_request_alternate_relay(ctx: &MmArc) -> bool {
    ctx.conf["sync_request_alternate_relay"].as_bool() != Some(false)
}

/// The top-level MM2.json boolean `sync_answer_deferral_before_placeholder` (CRD ch.32 R-Q5b item 6 (e)),
/// default `true`; only a JSON `false` turns the parking before the placeholder exists off.
pub(crate) fn sync_answer_deferral_before_placeholder(ctx: &MmArc) -> bool {
    ctx.conf["sync_answer_deferral_before_placeholder"].as_bool() != Some(false)
}

/// The top-level MM2.json boolean `orderbook_answer_waits_for_worker` (CRD ch.32 R-GH12a item 5), default
/// `true`; only a JSON `false` makes the order-book answer fail at once, as R-GH12 describes.
pub(crate) fn orderbook_answer_waits_for_worker(ctx: &MmArc) -> bool {
    ctx.conf["orderbook_answer_waits_for_worker"].as_bool() != Some(false)
}

/// Sends the answer of a synchronization request to the peer that asked. The inbound dispatcher builds
/// it around the request's response channel; a parked request keeps it until it is answered.
pub(crate) type SyncAnswerReply = Box<dyn FnOnce(AdexResponse) -> Result<(), String> + Send>;

/// An own request is identified by a token, a parked request by an id; both come from one counter.
type Id = u64;
/// `(pubkey, ordered pair)`.
type PairKey = (String, String);

struct Parked {
    peer: PeerId,
    /// The own requests (tokens) this request waits for.
    waits: HashSet<Id>,
    wake: Option<oneshot::Sender<()>>,
}

/// One keep-alive a relay is handling before its comparison has finished (R-Q5b item 6 (a)).
struct Registration {
    /// The peer the keep-alive came from (`propagated_from_peer`).
    peer: String,
    pubkey: String,
    pairs: Vec<String>,
    started_ms: u64,
}

#[derive(Default)]
struct Tables {
    next_id: Id,
    /// Own requests in flight, with the pairs each covers.
    in_flight: HashMap<Id, Vec<PairKey>>,
    /// The same, indexed by pair. An entry is never empty.
    in_flight_by_pair: HashMap<PairKey, Vec<Id>>,
    parked: HashMap<Id, Parked>,
    /// The number of entries of `parked` per requesting peer. A peer without parked requests has no key.
    /// It changes together with `parked`, under the same lock.
    parked_per_peer: HashMap<PeerId, usize>,
    /// The keep-alives being handled (R-Q5b item 6), keyed by an id of the same counter. They are bounded
    /// apart from `parked`.
    registrations: HashMap<Id, Registration>,
    /// The same, indexed by pair. An entry is never empty.
    registrations_by_pair: HashMap<PairKey, Vec<Id>>,
    /// The number of entries of `registrations` per forwarding peer. A peer without registrations has no key.
    /// It changes together with `registrations`, under the same lock.
    registrations_per_peer: HashMap<String, usize>,
}

impl Tables {
    fn next_id(&mut self) -> Id {
        self.next_id += 1;
        self.next_id
    }

    /// Remove the parked request `id`, if present, and count it out of its peer.
    fn unpark(&mut self, id: Id) {
        if let Some(parked) = self.parked.remove(&id) {
            if let Entry::Occupied(mut count) = self.parked_per_peer.entry(parked.peer) {
                *count.get_mut() -= 1;
                if *count.get() == 0 {
                    count.remove();
                }
            }
        }
    }

    /// Remove the registration `id`, if present, from the table, its index and the count of its peer.
    fn remove_registration(&mut self, id: Id) {
        let registration = match self.registrations.remove(&id) {
            Some(registration) => registration,
            None => return,
        };
        for pair in registration.pairs {
            if let Entry::Occupied(mut ids) = self.registrations_by_pair.entry((registration.pubkey.clone(), pair)) {
                ids.get_mut().retain(|i| *i != id);
                if ids.get().is_empty() {
                    ids.remove();
                }
            }
        }
        if let Entry::Occupied(mut count) = self.registrations_per_peer.entry(registration.peer) {
            *count.get_mut() -= 1;
            if *count.get() == 0 {
                count.remove();
            }
        }
    }

    /// The own request or registration `id` has ended: wake the parked requests that no longer wait for
    /// anything. A parked order-book request waits for nothing and has no channel, so it is never touched.
    fn stop_waiting_for(&mut self, id: Id) {
        for parked in self.parked.values_mut() {
            if parked.waits.remove(&id) && parked.waits.is_empty() {
                if let Some(wake) = parked.wake.take() {
                    // The receiver is gone only when the parked task is; nothing to wake then.
                    let _ = wake.send(());
                }
            }
        }
    }

    /// The tokens of the own requests in flight that cover a pair of the request, or the reason the request is
    /// not deferrable under item 1 of R-Q5b. `trie_store` is the store the caller holds locked while asking.
    fn deferrable_waits(
        &self,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
    ) -> Result<HashSet<Id>, AtOnce> {
        // An unknown pubkey is the "no data" outcome, answered at once.
        let pubkey_state = trie_store.pubkeys_state.get(pubkey).ok_or(AtOnce::NotDeferrable)?;
        let mut waits = HashSet::new();
        let mut without_own_request = false;
        for pair in trie_roots.keys() {
            // A pair without an entry is the missing-pair failure, answered at once.
            let held = pubkey_state.trie_roots.get(pair).ok_or(AtOnce::NotDeferrable)?;
            if *held != H64::default() {
                continue;
            }
            // A placeholder without an own request in flight is answered at once.
            match self.in_flight_by_pair.get(&(pubkey.to_owned(), pair.clone())) {
                Some(tokens) => waits.extend(tokens.iter().copied()),
                None => without_own_request = true,
            }
        }
        if without_own_request {
            return Err(AtOnce::OwnRequestNotInFlight);
        }
        // Nothing to wait for: the answer is the same now as it would be later.
        if waits.is_empty() {
            return Err(AtOnce::NotDeferrable);
        }
        Ok(waits)
    }

    /// R-Q5b item 6 (b): the open registrations a request waits for, with the time the last of them expires.
    /// Every requested pair must meet the condition of deferral (a held root other than the placeholder, or the
    /// placeholder with an own request in flight) or have an open registration, and at least one must have
    /// one. `None` when the request does not qualify.
    fn registered_waits(
        &self,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
        pre: PreBound,
    ) -> Option<(HashSet<Id>, u64)> {
        let pubkey_state = trie_store.pubkeys_state.get(pubkey);
        let mut waits = HashSet::new();
        let mut expires_ms = 0;
        for pair in trie_roots.keys() {
            let key = (pubkey.to_owned(), pair.clone());
            let mut registered = false;
            for id in self.registrations_by_pair.get(&key).into_iter().flatten() {
                let registration = match self.registrations.get(id) {
                    Some(registration) => registration,
                    None => continue,
                };
                let expiry_ms = registration.started_ms.saturating_add(pre.bound_ms);
                if pre.now_ms < expiry_ms {
                    registered = true;
                    waits.insert(*id);
                    expires_ms = expires_ms.max(expiry_ms);
                }
            }
            if registered {
                continue;
            }
            let meets_deferral = match pubkey_state.and_then(|state| state.trie_roots.get(pair)) {
                Some(held) if *held != H64::default() => true,
                Some(_) => self.in_flight_by_pair.contains_key(&key),
                None => false,
            };
            if !meets_deferral {
                return None;
            }
        }
        if waits.is_empty() {
            return None;
        }
        Some((waits, expires_ms))
    }
}

/// How a request that is not deferrable under item 1 may still be parked (R-Q5b item 6).
#[derive(Clone, Copy)]
struct PreBound {
    /// The bound of item 3; a registration is open while it is younger.
    bound_ms: u64,
    now_ms: u64,
}

/// A request that has been parked.
struct ParkedRequest {
    id: Id,
    wake: oneshot::Receiver<()>,
    /// Set for a request parked under item 6: when the last registration it waits for expires.
    registrations_expire_ms: Option<u64>,
}

/// The limits of the registrations (R-Q5b item 6 (c)) and the bound that ends them.
#[derive(Clone, Copy)]
pub(crate) struct RegistrationLimits {
    pub(crate) bound_ms: u64,
    pub(crate) max_per_peer: usize,
    pub(crate) max_total: usize,
}

/// The in-flight and parked tables of one node (a field of [`OrdermatchContext`]). Cloning shares them.
#[derive(Clone, Default)]
pub(crate) struct SyncAnswerDeferral(Arc<PaMutex<Tables>>);

impl SyncAnswerDeferral {
    /// Record that this node's own keep-alive-driven request for `pairs` of `pubkey` is about to be
    /// sent. The caller holds the trie store lock that guards the placeholder entries of `pairs`, so a
    /// request arriving afterwards sees the placeholder and the record together (lock order: trie
    /// store, then these tables). The record lasts until the returned guard is dropped.
    pub(crate) fn track_own_request<'a>(&self, pubkey: &str, pairs: impl Iterator<Item = &'a String>) -> SyncInFlight {
        let keys: Vec<PairKey> = pairs.map(|pair| (pubkey.to_owned(), pair.clone())).collect();
        let mut tables = self.0.lock();
        let token = tables.next_id();
        for key in &keys {
            tables.in_flight_by_pair.entry(key.clone()).or_default().push(token);
        }
        tables.in_flight.insert(token, keys);
        SyncInFlight {
            table: self.clone(),
            token,
        }
    }

    /// The own request `token` has completed: forget it and wake the parked requests that no longer
    /// wait for anything.
    fn complete(&self, token: Id) {
        let mut tables = self.0.lock();
        if let Some(keys) = tables.in_flight.remove(&token) {
            for key in keys {
                if let Some(tokens) = tables.in_flight_by_pair.get_mut(&key) {
                    tokens.retain(|t| *t != token);
                    if tokens.is_empty() {
                        tables.in_flight_by_pair.remove(&key);
                    }
                }
            }
        }
        tables.stop_waiting_for(token);
    }

    /// R-Q5b item 6 (a), (c): register the keep-alive of `pubkey` that came from `peer`, which names `pairs`
    /// (one entry, whatever their number). A registration older than the bound counts as ended: it is dropped
    /// first, so it counts against no limit. `None` when either limit is full; the caller counts that. The
    /// registration lasts until the returned guard is dropped.
    pub(crate) fn register_keep_alive(
        &self,
        peer: &str,
        pubkey: &str,
        pairs: Vec<String>,
        limits: RegistrationLimits,
        now_ms: u64,
    ) -> Option<KeepAliveRegistration> {
        let mut tables = self.0.lock();
        let expired: Vec<Id> = tables
            .registrations
            .iter()
            .filter(|(_, registration)| now_ms.saturating_sub(registration.started_ms) >= limits.bound_ms)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            tables.remove_registration(id);
            tables.stop_waiting_for(id);
        }
        if tables.registrations.len() >= limits.max_total
            || tables.registrations_per_peer.get(peer).copied().unwrap_or(0) >= limits.max_per_peer
        {
            return None;
        }

        let id = tables.next_id();
        for pair in &pairs {
            tables
                .registrations_by_pair
                .entry((pubkey.to_owned(), pair.clone()))
                .or_default()
                .push(id);
        }
        *tables.registrations_per_peer.entry(peer.to_owned()).or_default() += 1;
        tables.registrations.insert(id, Registration {
            peer: peer.to_owned(),
            pubkey: pubkey.to_owned(),
            pairs,
            started_ms: now_ms,
        });
        Some(KeepAliveRegistration {
            table: self.clone(),
            id,
        })
    }

    /// The registration `id` has ended: forget it, if it is still there, and wake the parked requests that
    /// no longer wait for anything.
    fn end_registration(&self, id: Id) {
        let mut tables = self.0.lock();
        tables.remove_registration(id);
        tables.stop_waiting_for(id);
    }

    /// R-Q5b: park the request when it is deferrable (item 1) and within the limits (`max_per_peer` requests of
    /// one peer, `max_total` in all). `trie_store` is the store the caller holds locked while asking. The error
    /// says why the request is to be answered at once.
    #[cfg(test)]
    fn try_park(
        &self,
        peer: PeerId,
        max_per_peer: usize,
        max_total: usize,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
    ) -> Result<(Id, oneshot::Receiver<()>), AtOnce> {
        self.try_park_request(peer, max_per_peer, max_total, pubkey, trie_roots, trie_store, None)
            .map(|parked| (parked.id, parked.wake))
    }

    /// [`Self::try_park`], and with `pre` set also the parking of item 6, which is tried only for a request that
    /// item 1 does not park. A request parked under item 6 waits for the registrations that cover its pairs.
    #[allow(clippy::too_many_arguments)]
    fn try_park_request(
        &self,
        peer: PeerId,
        max_per_peer: usize,
        max_total: usize,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
        pre: Option<PreBound>,
    ) -> Result<ParkedRequest, AtOnce> {
        let mut tables = self.0.lock();
        let (waits, registrations_expire_ms) = match tables.deferrable_waits(pubkey, trie_roots, trie_store) {
            Ok(waits) => (waits, None),
            Err(reason) => {
                let pre = pre.ok_or(reason)?;
                let (waits, expires_ms) = tables
                    .registered_waits(pubkey, trie_roots, trie_store, pre)
                    .ok_or(reason)?;
                (waits, Some(expires_ms))
            },
        };
        if tables.parked.len() >= max_total || tables.parked_per_peer.get(&peer).copied().unwrap_or(0) >= max_per_peer {
            return Err(AtOnce::CapsFull);
        }

        let id = tables.next_id();
        let (wake_tx, wake_rx) = oneshot::channel();
        tables.parked.insert(id, Parked {
            peer,
            waits,
            wake: Some(wake_tx),
        });
        *tables.parked_per_peer.entry(peer).or_default() += 1;
        Ok(ParkedRequest {
            id,
            wake: wake_rx,
            registrations_expire_ms,
        })
    }

    /// R-Q5b item 6 (b), second step: the registrations the parked request `id` waited for have ended or
    /// expired. Classify it again under item 1 only; when it is deferrable keep its slot, wait for those
    /// own requests with a fresh set and channel, and return the channel. `None` when the request is
    /// answered at once.
    fn reclassify(
        &self,
        id: Id,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
    ) -> Option<oneshot::Receiver<()>> {
        let mut tables = self.0.lock();
        let waits = tables.deferrable_waits(pubkey, trie_roots, trie_store).ok()?;
        let parked = tables.parked.get_mut(&id)?;
        let (wake_tx, wake_rx) = oneshot::channel();
        parked.waits = waits;
        parked.wake = Some(wake_tx);
        Some(wake_rx)
    }

    /// R-GH12a item 4: take a slot of the parked table for a waiting order-book request, counted per asking
    /// peer and in total like a parked synchronization request. `None` when either limit is full. Such an
    /// entry waits for nothing and has no channel, so no end of an own request or registration touches it.
    fn park_orderbook_request(&self, peer: PeerId, max_per_peer: usize, max_total: usize) -> Option<ParkedSlot> {
        let mut tables = self.0.lock();
        if tables.parked.len() >= max_total || tables.parked_per_peer.get(&peer).copied().unwrap_or(0) >= max_per_peer {
            return None;
        }
        let id = tables.next_id();
        tables.parked.insert(id, Parked {
            peer,
            waits: HashSet::new(),
            wake: None,
        });
        *tables.parked_per_peer.entry(peer).or_default() += 1;
        Some(ParkedSlot {
            table: self.clone(),
            id,
        })
    }

    #[cfg(test)]
    pub(crate) fn parked_len(&self) -> usize { self.0.lock().parked.len() }

    /// The per-peer count agrees with `parked`: the count map is empty exactly when `parked` is, and
    /// otherwise holds, per peer, the number of its parked requests.
    #[cfg(test)]
    pub(crate) fn parked_counts_agree(&self) -> bool {
        let tables = self.0.lock();
        let mut counts: HashMap<PeerId, usize> = HashMap::new();
        for parked in tables.parked.values() {
            *counts.entry(parked.peer).or_default() += 1;
        }
        counts == tables.parked_per_peer
    }

    #[cfg(test)]
    pub(crate) fn in_flight_len(&self) -> usize { self.0.lock().in_flight.len() }

    #[cfg(test)]
    pub(crate) fn registrations_len(&self) -> usize { self.0.lock().registrations.len() }

    /// The per-peer count and the index by pair agree with `registrations`: the count map and the index are
    /// empty exactly when `registrations` is, and otherwise hold, per forwarding peer, the number of its
    /// registrations and, per pair, exactly the ids of the registrations that name it.
    #[cfg(test)]
    pub(crate) fn registration_counts_agree(&self) -> bool {
        let tables = self.0.lock();
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut by_pair: HashMap<PairKey, Vec<Id>> = HashMap::new();
        for (id, registration) in &tables.registrations {
            *counts.entry(registration.peer.clone()).or_default() += 1;
            for pair in &registration.pairs {
                by_pair
                    .entry((registration.pubkey.clone(), pair.clone()))
                    .or_default()
                    .push(*id);
            }
        }
        for ids in by_pair.values_mut() {
            ids.sort_unstable();
        }
        let mut indexed = tables.registrations_by_pair.clone();
        for ids in indexed.values_mut() {
            ids.sort_unstable();
        }
        counts == tables.registrations_per_peer && by_pair == indexed
    }
}

/// The record of one own keep-alive-driven request, kept from just before it is sent until its answer
/// has been applied by the trie worker or the request has failed. Dropping it ends the record, so a
/// task that fails, returns early or is cancelled can never leave it (or a request parked behind it)
/// stuck.
pub(crate) struct SyncInFlight {
    table: SyncAnswerDeferral,
    token: Id,
}

impl Drop for SyncInFlight {
    fn drop(&mut self) { self.table.complete(self.token); }
}

/// The registration of one keep-alive being handled (R-Q5b item 6), kept from before the handling waits for
/// the trie worker until the comparison has finished. Dropping it ends the registration, so an error, an
/// early return or a dropped future can never leave it (or a request parked behind it) stuck. If the
/// registration has expired meanwhile there is nothing left to remove.
pub(crate) struct KeepAliveRegistration {
    table: SyncAnswerDeferral,
    id: Id,
}

impl Drop for KeepAliveRegistration {
    fn drop(&mut self) { self.table.end_registration(self.id); }
}

/// R-Q5b item 6 (a), (d), (e): register the keep-alive of `from_pubkey`, received from `forwarding_peer`, which
/// names `roots`, before its handling waits for the trie worker. Nothing is registered unless this node is a
/// relay, the bound is above zero, the switch is on, the keep-alive is not from one of this node's own pubkeys
/// and it names a pair with a root other than the zero and the empty-trie root. A limit that is full is counted
/// in the summary and registers nothing (item (f)).
pub(crate) fn register_keep_alive_handling(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    forwarding_peer: &str,
    from_pubkey: &str,
    roots: &HashMap<AlbOrderedOrderbookPair, H64>,
    i_am_relay: bool,
) -> Option<KeepAliveRegistration> {
    let bound_ms = sync_answer_deferral_ms(ctx);
    if !i_am_relay || bound_ms == 0 || !sync_answer_deferral_before_placeholder(ctx) {
        return None;
    }
    let pairs: Vec<String> = roots
        .iter()
        .filter(|(_, root)| **root != H64::default() && **root != hashed_null_node::<Layout>())
        .map(|(pair, _)| pair.clone())
        .collect();
    if pairs.is_empty() {
        return None;
    }
    // Never for the relay's own pubkeys. When they cannot be told, nothing is registered.
    let my_pubkey = mm2_internal_pubkey_hex(ctx).ok()?;
    if is_my_order(from_pubkey, &my_pubkey, &ordermatch_ctx.orderbook.lock().my_p2p_pubkeys) {
        return None;
    }

    let limits = RegistrationLimits {
        bound_ms,
        max_per_peer: sync_answer_deferral_max_per_peer(ctx),
        max_total: sync_answer_deferral_max_total(ctx),
    };
    let now = now_ms();
    let registration =
        ordermatch_ctx
            .sync_answer_deferral
            .register_keep_alive(forwarding_peer, from_pubkey, pairs, limits, now);
    if registration.is_none() {
        ordermatch_ctx
            .sync_log
            .registration_capped(forwarding_peer, from_pubkey, now);
    }
    registration
}

impl SyncInFlight {
    /// The answer's trie events have been queued; `target_batches` is [`Orderbook::trie_ops_enqueued`]
    /// right after queueing them. The record ends once the worker has applied that many batches, or
    /// after `max_wait_ms` if it has not. Returns at once: the wait happens on a task of its own, so
    /// the forward of the keep-alive (R-F3b) does not wait for the worker.
    pub(crate) fn complete_after_apply(
        self,
        trie_store: Arc<PaMutex<TrieStore>>,
        target_batches: u64,
        max_wait_ms: u64,
    ) {
        if trie_applied(&trie_store, target_batches) {
            return;
        }
        spawn(async move {
            wait_until_applied(&trie_store, target_batches, max_wait_ms).await;
            drop(self);
        });
    }
}

/// Whether the trie worker has applied at least `target_batches` batches.
fn trie_applied(trie_store: &PaMutex<TrieStore>, target_batches: u64) -> bool {
    trie_store.lock().applied_batches >= target_batches
}

/// Waits until the trie worker has applied `target_batches` batches, at most `max_wait_ms`; whether it has.
pub(crate) async fn wait_until_applied(trie_store: &PaMutex<TrieStore>, target_batches: u64, max_wait_ms: u64) -> bool {
    let mut waited = 0;
    while waited < max_wait_ms && !trie_applied(trie_store, target_batches) {
        Timer::sleep(APPLIED_CHECK_STEP_MS as f64 / 1000.).await;
        waited += APPLIED_CHECK_STEP_MS;
    }
    trie_applied(trie_store, target_batches)
}

/// A parked request's slot in the table; dropping it frees the slot.
struct ParkedSlot {
    table: SyncAnswerDeferral,
    id: Id,
}

impl Drop for ParkedSlot {
    fn drop(&mut self) { self.table.0.lock().unpark(self.id); }
}

/// The answer R-Q5 gives to the request at this moment, in the form the transport sends. It is the one place
/// where the answer is decided, so it is also where the answer is logged: `path` says how the answer
/// was reached and `waited_ms` how long a parked request waited.
fn sync_answer_response(
    ctx: &MmArc,
    peer: &PeerId,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    path: AnswerPath,
    waited_ms: Option<u64>,
) -> AdexResponse {
    let (pairs, log_pubkey) = if debug_enabled() {
        (pairs_label(trie_roots.keys()), pubkey.clone())
    } else {
        (String::new(), String::new())
    };
    let mut entries = None;
    let (class, response) = match process_sync_pubkey_orderbook_state_classified(ctx.clone(), pubkey, trie_roots) {
        Ok(Some(answer)) => {
            let class = answer_class(&answer);
            entries = Some(answer_entries(&answer));
            match encode_message(&answer) {
                Ok(response) => (class, AdexResponse::Ok { response }),
                Err(e) => (AnswerClass::TrieError, AdexResponse::Err { error: e.to_string() }),
            }
        },
        Ok(None) => (AnswerClass::NoData, AdexResponse::None),
        Err(SyncAnswerError { failure, text }) => (failure.into(), AdexResponse::Err { error: text }),
    };
    match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx.sync_log.answer(
            &AnswerEvent {
                peer,
                pubkey: &log_pubkey,
                pairs: &pairs,
                class,
                path,
                waited_ms,
                entries,
            },
            now_ms(),
        ),
        Err(e) => log::error!("Cannot log a synchronization answer: {}", e),
    }
    response
}

/// `ok_full` when any pair is answered with a full set, `ok_delta` when all are deltas.
fn answer_class(answer: &SyncPubkeyOrderbookStateRes) -> AnswerClass {
    if answer
        .pair_orders_diff
        .values()
        .any(|diff| matches!(diff, DeltaOrFullTrie::FullTrie(_)))
    {
        AnswerClass::OkFull
    } else {
        AnswerClass::OkDelta
    }
}

/// The orders (or order changes) the answer carries over all its pairs.
fn answer_entries(answer: &SyncPubkeyOrderbookStateRes) -> usize {
    answer
        .pair_orders_diff
        .values()
        .map(|diff| match diff {
            DeltaOrFullTrie::Delta(delta) => delta.len(),
            DeltaOrFullTrie::FullTrie(orders) => orders.len(),
        })
        .sum()
}

/// Park the request when it is deferrable (R-Q5b); give `reply` back, with the reason, when it is to be
/// answered at once.
fn park_sync_request(
    ctx: &MmArc,
    peer: PeerId,
    pubkey: &str,
    trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
    reply: SyncAnswerReply,
) -> Result<(), (SyncAnswerReply, AtOnce)> {
    let bound_ms = sync_answer_deferral_ms(ctx);
    if bound_ms == 0 {
        return Err((reply, AtOnce::DeferralOff));
    }
    let ordermatch_ctx = match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx,
        Err(e) => {
            log::error!("Cannot consider deferring a synchronization request: {}", e);
            // Logged above; the request is simply not deferred.
            return Err((reply, AtOnce::NotDeferrable));
        },
    };

    let max_per_peer = sync_answer_deferral_max_per_peer(ctx);
    let max_total = sync_answer_deferral_max_total(ctx);
    let before_placeholder = sync_answer_deferral_before_placeholder(ctx);
    let table = ordermatch_ctx.sync_answer_deferral.clone();
    let parked = {
        let trie_store = ordermatch_ctx.trie_store.lock();
        let pre = before_placeholder.then(|| PreBound {
            bound_ms,
            now_ms: now_ms(),
        });
        table.try_park_request(peer, max_per_peer, max_total, pubkey, trie_roots, &trie_store, pre)
    };
    let ParkedRequest {
        id,
        wake: wake_rx,
        registrations_expire_ms,
    } = match parked {
        Ok(parked) => parked,
        Err(reason) => return Err((reply, reason)),
    };

    let slot = ParkedSlot { table, id };
    let ctx = ctx.clone();
    let pubkey = pubkey.to_owned();
    let trie_roots = trie_roots.clone();
    let parked_at_ms = now_ms();
    spawn(async move {
        let path = match registrations_expire_ms {
            // Woken when every own request it waits for has completed, or at the bound, whichever is first.
            None => {
                let timeout = Box::pin(Timer::sleep(bound_ms as f64 / 1000.));
                match select(wake_rx, timeout).await {
                    Either::Left(_) => AnswerPath::ParkedWoken,
                    Either::Right(_) => AnswerPath::ParkedBound,
                }
            },
            Some(expires_ms) => {
                let wait = PreWait {
                    ordermatch_ctx: &ordermatch_ctx,
                    id,
                    pubkey: &pubkey,
                    trie_roots: &trie_roots,
                    registrations_expire_ms: expires_ms,
                    bound_ends_ms: parked_at_ms.saturating_add(bound_ms),
                };
                wait.run(wake_rx).await
            },
        };
        // The slot is freed before the answer is sent, so the table is empty by the time it arrives.
        drop(slot);
        let waited_ms = now_ms().saturating_sub(parked_at_ms);
        let res = sync_answer_response(&ctx, &peer, pubkey, trie_roots, path, Some(waited_ms));
        if let Err(e) = reply(res) {
            log::error!("Error on sending the answer of a parked synchronization request: {}", e);
        }
    });
    Ok(())
}

/// The seconds until `deadline_ms`, for a timer.
fn seconds_until(deadline_ms: u64) -> f64 { deadline_ms.saturating_sub(now_ms()) as f64 / 1000. }

/// The wait of a request parked under R-Q5b item 6 (b), on the request's own task.
struct PreWait<'a> {
    ordermatch_ctx: &'a OrdermatchContext,
    id: Id,
    pubkey: &'a str,
    trie_roots: &'a HashMap<AlbOrderedOrderbookPair, H64>,
    /// When the last registration the request waits for expires; never later than `bound_ends_ms`, because
    /// every such registration started before the request arrived.
    registrations_expire_ms: u64,
    /// The arrival of the request plus the bound of item 3, which the whole wait never outlasts.
    bound_ends_ms: u64,
}

impl PreWait<'_> {
    /// Phase 1: until the registrations have all ended, or the last of them expires. Then the request is
    /// classified again under item 1 only: when it is deferrable, the same slot waits for the own requests
    /// in flight at that moment, until they complete or the bound ends. Any other request is answered at
    /// once. A registration made after the request arrived never extends the wait.
    ///
    /// The path says how the wait ended: woken when everything awaited ended, bound when a timer did.
    async fn run(self, registrations_ended: oneshot::Receiver<()>) -> AnswerPath {
        let phase1_ends_ms = self.registrations_expire_ms.min(self.bound_ends_ms);
        let timeout = Box::pin(Timer::sleep(seconds_until(phase1_ends_ms)));
        let mut at_the_bound = matches!(select(registrations_ended, timeout).await, Either::Right(_));

        if let Some(own_requests_ended) = self.reclassify() {
            let timeout = Box::pin(Timer::sleep(seconds_until(self.bound_ends_ms)));
            at_the_bound = matches!(select(own_requests_ended, timeout).await, Either::Right(_));
        }
        if at_the_bound {
            AnswerPath::PreParkedBound
        } else {
            AnswerPath::PreParkedWoken
        }
    }

    /// Item 1 only, under the trie store lock and then the tables lock.
    fn reclassify(&self) -> Option<oneshot::Receiver<()>> {
        let trie_store = self.ordermatch_ctx.trie_store.lock();
        self.ordermatch_ctx
            .sync_answer_deferral
            .reclassify(self.id, self.pubkey, self.trie_roots, &trie_store)
    }
}

/// The answer to a `SyncPubkeyOrderbookState` request (R-Q5, R-Q5b): the request is parked when it is
/// deferrable and answered at once otherwise. Either way the call returns without waiting (R-T3b item
/// 3); a parked request is answered later from a task of its own through `reply`.
///
/// # Errors
///
/// The error of `reply` when the answer was sent at once and sending failed.
pub(crate) fn answer_sync_request(
    ctx: &MmArc,
    peer: PeerId,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    reply: SyncAnswerReply,
) -> Result<(), String> {
    match park_sync_request(ctx, peer, &pubkey, &trie_roots, reply) {
        Ok(()) => Ok(()),
        Err((reply, reason)) => reply(sync_answer_response(
            ctx,
            &peer,
            pubkey,
            trie_roots,
            AnswerPath::AtOnce(reason),
            None,
        )),
    }
}

/// The answer to an order-book request in the form the transport sends.
fn orderbook_response(result: Result<Vec<u8>, OrderbookAnswerError>) -> AdexResponse {
    match result {
        Ok(response) => AdexResponse::Ok { response },
        Err(e) => AdexResponse::Err { error: e.text() },
    }
}

/// Whether every pubkey in `pubkeys` has a record in the trie store. It takes the trie store lock only.
fn records_exist(trie_store: &PaMutex<TrieStore>, pubkeys: &[String]) -> bool {
    let trie_store = trie_store.lock();
    pubkeys
        .iter()
        .all(|pubkey| trie_store.pubkeys_state.contains_key(pubkey))
}

/// The answer to a `GetOrderbook` request (R-Q1, R-GH12, R-GH12a). It is built at once. When it fails only
/// because pubkeys its index lists have no record in the trie store yet, and the switch
/// [`orderbook_answer_waits_for_worker`] is on, the request takes a slot of the parked table and a task of its
/// own waits for the trie worker, at most [`TRIE_CATCH_UP_BOUND_MS`] from the arrival; the call returns without
/// waiting (R-T3b item 3). Every other outcome, and a request past the limits of R-Q5b item 4, is answered
/// at once.
///
/// # Errors
///
/// The error of `reply` when the answer was sent at once and sending failed.
pub(crate) fn answer_orderbook_request(
    ctx: &MmArc,
    peer: PeerId,
    base: String,
    rel: String,
    reply: SyncAnswerReply,
) -> Result<(), String> {
    log::debug!(
        "Got ordermatch request GetOrderbook {{ base: {:?}, rel: {:?} }}",
        base,
        rel
    );
    let ordermatch_ctx = match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx,
        Err(e) => return reply(AdexResponse::Err { error: ERRL!("{}", e) }),
    };
    let missing = match build_orderbook_answer(&ordermatch_ctx, base.clone(), rel.clone()) {
        Err(OrderbookAnswerError::MissingRecords(missing)) if orderbook_answer_waits_for_worker(ctx) => missing,
        result => return reply(orderbook_response(result)),
    };
    let max_per_peer = sync_answer_deferral_max_per_peer(ctx);
    let max_total = sync_answer_deferral_max_total(ctx);
    let slot = match ordermatch_ctx
        .sync_answer_deferral
        .park_orderbook_request(peer, max_per_peer, max_total)
    {
        Some(slot) => slot,
        None => return reply(orderbook_response(Err(OrderbookAnswerError::MissingRecords(missing)))),
    };

    let arrived_ms = now_ms();
    spawn(async move {
        let deadline_ms = arrived_ms.saturating_add(TRIE_CATCH_UP_BOUND_MS);
        let mut missing = missing;
        let result = loop {
            // Until every pubkey found missing has a record, or the bound has passed.
            while now_ms() < deadline_ms && !records_exist(&ordermatch_ctx.trie_store, &missing) {
                Timer::sleep(APPLIED_CHECK_STEP_MS as f64 / 1000.).await;
            }
            // The answer comes from the index and the store as they stand now. Pubkeys that have left the
            // index are no longer listed, and the number of orders is checked again.
            match build_orderbook_answer(&ordermatch_ctx, base.clone(), rel.clone()) {
                Err(OrderbookAnswerError::MissingRecords(other)) if now_ms() < deadline_ms => missing = other,
                result => break result,
            }
        };
        // The slot is freed before the answer is sent.
        drop(slot);
        let outcome = match &result {
            Ok(_) => "ok",
            Err(OrderbookAnswerError::MissingRecords(_)) => "missing_record",
            Err(OrderbookAnswerError::TooManyOrders) => "too_large",
            Err(OrderbookAnswerError::Encoding(_)) => "encode_error",
        };
        log::debug!(
            "orderbook answer: peer={} pair={}:{} waited_ms={} outcome={}",
            peer,
            base,
            rel,
            now_ms().saturating_sub(arrived_ms),
            outcome
        );
        if let Err(e) = reply(orderbook_response(result)) {
            log::error!("Error on sending the answer of a waiting order-book request: {}", e);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;

    const PUBKEY: &str = "pubkey";
    const PAIR: &str = "C1:C2";

    fn store_with_placeholder() -> TrieStore {
        let mut store = TrieStore::default();
        let state = pubkey_state_mut(&mut store.pubkeys_state, PUBKEY);
        order_pair_root_mut(&mut state.trie_roots, PAIR);
        store
    }

    fn requested() -> HashMap<AlbOrderedOrderbookPair, H64> { HashMap::from([(PAIR.to_owned(), H64::default())]) }

    /// The own request counts as in flight until the worker has applied its batch, not until the answer
    /// has merely arrived and been queued (R-Q5b: "its answer's mutation events have been applied").
    #[test]
    fn the_record_ends_when_the_worker_has_applied_the_batch() {
        let deferral = SyncAnswerDeferral::default();
        let store = Arc::new(PaMutex::new(store_with_placeholder()));
        let in_flight = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, mut wake) = deferral
            .try_park(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                PUBKEY,
                &requested(),
                &store.lock(),
            )
            .expect("deferrable");

        // The answer is queued as batch 1; the worker has applied none yet.
        in_flight.complete_after_apply(store.clone(), 1, 5000);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(wake.try_recv(), Ok(None), "still waiting for the worker");
        assert_eq!(deferral.in_flight_len(), 1);

        store.lock().applied_batches = 1;
        block_on(wake).expect("woken");
        assert_eq!(deferral.in_flight_len(), 0);
    }

    /// A worker that never catches up does not keep the record forever: it ends at the bound.
    #[test]
    fn the_record_ends_at_the_bound_if_the_worker_does_not_catch_up() {
        let deferral = SyncAnswerDeferral::default();
        let store = Arc::new(PaMutex::new(store_with_placeholder()));
        let in_flight = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, wake) = deferral
            .try_park(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                PUBKEY,
                &requested(),
                &store.lock(),
            )
            .expect("deferrable");

        in_flight.complete_after_apply(store, 1, 50);
        block_on(wake).expect("woken at the bound");
        assert_eq!(deferral.in_flight_len(), 0);
    }

    /// Only a JSON `false` turns the request to one other relay off.
    #[test]
    fn the_alternate_relay_switch_is_off_only_for_a_json_false() {
        let switch = |conf: Json| {
            let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            sync_request_alternate_relay(&ctx)
        };
        assert!(switch(json!({})));
        assert!(switch(json!({ "sync_request_alternate_relay": true })));
        assert!(!switch(json!({ "sync_request_alternate_relay": false })));
        assert!(switch(json!({ "sync_request_alternate_relay": "false" })));
        assert!(switch(json!({ "sync_request_alternate_relay": 0 })));
        assert!(switch(json!({ "sync_request_alternate_relay": null })));
    }

    /// A request waits for the own requests in flight when it arrives, not for ones started later.
    #[test]
    fn a_request_waits_only_for_the_own_requests_in_flight_when_it_arrives() {
        let deferral = SyncAnswerDeferral::default();
        let store = store_with_placeholder();
        let first = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, mut wake) = deferral
            .try_park(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                PUBKEY,
                &requested(),
                &store,
            )
            .expect("deferrable");
        let second = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());

        drop(first);
        assert_eq!(wake.try_recv(), Ok(Some(())));
        assert_eq!(deferral.in_flight_len(), 1);
        drop(second);
        assert_eq!(deferral.in_flight_len(), 0);
    }

    fn limits(bound_ms: u64, max_per_peer: usize, max_total: usize) -> RegistrationLimits {
        RegistrationLimits {
            bound_ms,
            max_per_peer,
            max_total,
        }
    }

    fn register(
        deferral: &SyncAnswerDeferral,
        peer: &str,
        pubkey: &str,
        limits: RegistrationLimits,
        now_ms: u64,
    ) -> Option<KeepAliveRegistration> {
        deferral.register_keep_alive(peer, pubkey, vec![PAIR.to_owned()], limits, now_ms)
    }

    /// A registration older than the bound counts as ended: it is dropped before the next one is
    /// registered, so it counts against no limit, and its guard later finds nothing to remove.
    #[test]
    fn a_registration_expires_at_the_bound_and_counts_against_no_limit() {
        let deferral = SyncAnswerDeferral::default();
        let first = register(&deferral, "F", PUBKEY, limits(100, 1, 5), 1_000).expect("registered");
        assert!(register(&deferral, "F", "other", limits(100, 1, 5), 1_099).is_none());
        assert_eq!(deferral.registrations_len(), 1);

        let second = register(&deferral, "F", "other", limits(100, 1, 5), 1_100).expect("the first has expired");
        assert_eq!(deferral.registrations_len(), 1);
        assert!(deferral.registration_counts_agree());
        drop(first);
        assert_eq!(deferral.registrations_len(), 1);
        assert!(deferral.registration_counts_agree());
        drop(second);
        assert_eq!(deferral.registrations_len(), 0);
        assert!(deferral.registration_counts_agree());
    }

    #[test]
    fn registrations_are_limited_per_forwarding_peer_and_in_all() {
        let deferral = SyncAnswerDeferral::default();
        let limits = limits(1_000, 2, 3);
        let a = register(&deferral, "F", "a", limits, 0).expect("registered");
        let b = register(&deferral, "F", "b", limits, 0).expect("registered");
        assert!(register(&deferral, "F", "c", limits, 0).is_none(), "per-peer limit");
        let d = register(&deferral, "G", "d", limits, 0).expect("registered");
        assert!(register(&deferral, "H", "e", limits, 0).is_none(), "total limit");
        assert!(deferral.registration_counts_agree());

        // Ending one frees its place for its peer and in the total.
        drop(a);
        let f = register(&deferral, "F", "f", limits, 0).expect("registered");
        assert!(deferral.registration_counts_agree());
        drop((b, d, f));
        assert_eq!(deferral.registrations_len(), 0);
        assert!(deferral.registration_counts_agree());
    }

    #[test]
    fn the_end_of_a_registration_wakes_the_request_parked_behind_it_and_so_does_its_expiry() {
        let deferral = SyncAnswerDeferral::default();
        let store = TrieStore::default();
        let pre = |now_ms| Some(PreBound { bound_ms: 100, now_ms });
        let park = |now_ms| {
            deferral.try_park_request(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                PUBKEY,
                &requested(),
                &store,
                pre(now_ms),
            )
        };

        // Ended by its guard.
        let registration = register(&deferral, "F", PUBKEY, limits(100, 32, 2048), 1_000).expect("registered");
        let parked = park(1_050).expect("parked behind the registration");
        assert_eq!(parked.registrations_expire_ms, Some(1_100));
        let mut wake = parked.wake;
        assert_eq!(wake.try_recv(), Ok(None));
        drop(registration);
        assert_eq!(wake.try_recv(), Ok(Some(())));
        deferral.0.lock().unpark(parked.id);

        // Ended by its expiry, which the next registration notices.
        let registration = register(&deferral, "F", PUBKEY, limits(100, 32, 2048), 2_000).expect("registered");
        let parked = park(2_050).expect("parked behind the registration");
        let mut wake = parked.wake;
        let other = register(&deferral, "G", "other", limits(100, 32, 2048), 2_100).expect("registered");
        assert_eq!(wake.try_recv(), Ok(Some(())));
        drop((registration, other));
        deferral.0.lock().unpark(parked.id);
        assert!(deferral.parked_counts_agree());
        assert!(deferral.registration_counts_agree());
    }

    /// A request is parked behind registrations that are open: a registration older than the bound, or none,
    /// leaves it to be answered at once.
    #[test]
    fn only_an_open_registration_parks_a_request() {
        let deferral = SyncAnswerDeferral::default();
        let store = TrieStore::default();
        let _registration = register(&deferral, "F", PUBKEY, limits(100, 32, 2048), 1_000).expect("registered");
        let park = |pubkey: &str, now_ms: u64, pre: bool| {
            deferral.try_park_request(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                pubkey,
                &requested(),
                &store,
                pre.then_some(PreBound { bound_ms: 100, now_ms }),
            )
        };
        assert!(park(PUBKEY, 1_099, true).is_ok());
        assert_eq!(park(PUBKEY, 1_100, true).err(), Some(AtOnce::NotDeferrable));
        assert_eq!(park("another", 1_050, true).err(), Some(AtOnce::NotDeferrable));
        assert_eq!(park(PUBKEY, 1_050, false).err(), Some(AtOnce::NotDeferrable));
    }

    /// Every pair has to meet the condition of deferral or be registered, and one has to be registered: a pair
    /// held at a real root is waited for together with a registered one.
    #[test]
    fn a_pair_held_at_a_real_root_is_parked_with_a_registered_one_only() {
        const HELD: &str = "C5:C6";
        let deferral = SyncAnswerDeferral::default();
        let mut store = TrieStore::default();
        *order_pair_root_mut(&mut pubkey_state_mut(&mut store.pubkeys_state, PUBKEY).trie_roots, HELD) =
            H64::from([7; 8]);
        let both: HashMap<String, H64> = [HELD, PAIR].iter().map(|p| (p.to_string(), H64::default())).collect();
        let only_held: HashMap<String, H64> = [(HELD.to_owned(), H64::default())].into_iter().collect();
        let park = |roots: &HashMap<String, H64>| {
            deferral.try_park_request(
                PeerId::random(),
                DEFAULT_MAX_PARKED_PER_PEER,
                DEFAULT_MAX_PARKED_TOTAL,
                PUBKEY,
                roots,
                &store,
                Some(PreBound {
                    bound_ms: 100,
                    now_ms: 1_050,
                }),
            )
        };
        assert_eq!(park(&both).err(), Some(AtOnce::NotDeferrable), "nothing is registered");
        let _registration = register(&deferral, "F", PUBKEY, limits(100, 32, 2048), 1_000).expect("registered");
        assert!(park(&both).is_ok());
        assert_eq!(
            park(&only_held).err(),
            Some(AtOnce::NotDeferrable),
            "no pair is registered"
        );
    }

    /// The slot of a waiting order-book request is counted like a parked request, waits for nothing and has no
    /// channel: no end of an own request or registration touches it.
    #[test]
    fn an_orderbook_slot_is_not_touched_by_the_end_of_an_own_request_or_a_registration() {
        let deferral = SyncAnswerDeferral::default();
        let asking = PeerId::random();
        let slot = deferral.park_orderbook_request(asking, 1, 5).expect("a slot");
        assert!(
            deferral.park_orderbook_request(asking, 1, 5).is_none(),
            "per-peer limit"
        );
        assert!(
            deferral.park_orderbook_request(PeerId::random(), 1, 1).is_none(),
            "total limit"
        );
        assert_eq!(deferral.parked_len(), 1);

        let own = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let registration = register(&deferral, "F", PUBKEY, limits(100, 32, 2048), 1_000).expect("registered");
        drop(own);
        drop(registration);
        assert_eq!(deferral.parked_len(), 1);
        assert!(deferral.parked_counts_agree());
        drop(slot);
        assert_eq!(deferral.parked_len(), 0);
        assert!(deferral.parked_counts_agree());
    }
}
