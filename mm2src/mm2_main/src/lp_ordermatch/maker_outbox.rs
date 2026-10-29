//! The maker outbox (CRD ch.32 §32.9B R-MP1 to R-MP4, Reloaded-original).
//!
//! The order messages a node publishes for its own maker orders wait here, per maker pubkey, with at
//! most one pending message per order, and leave at a rate that follows the capacity of the relays.
//! The logic is pure: every method takes the current time in milliseconds and the queue signal level as
//! arguments, so the tests inject a clock and need no sleeping. The publishing itself, and the
//! signing, are done by the caller with what the methods return.

use super::new_protocol::{MakerOrderCancelled, MakerOrderCreated, MakerOrderUpdated};
use common::mm_number::MmNumber;
use keys::KeyPair;
use parking_lot::Mutex as PaMutex;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use uuid::Uuid;

/// R-MP2 item 1: the default of `maker_update_rate_max`, messages per second per maker pubkey.
pub(crate) const DEFAULT_MAKER_UPDATE_RATE_MAX: f64 = 10.0;
/// R-MP2 item 1: the burst is this many times the rate.
const BURST_FACTOR: f64 = 10.0;
/// R-MP2 item 3: the rate in degraded mode, messages per second.
const RATE_FLOOR: f64 = 0.5;
/// R-MP2 item 3: the outbox leaves degraded mode this long after the last signal.
const DEGRADED_FOR_MS: u64 = 5 * 60_000;
/// R-MP2 item 3: the rate rises by this much, in messages per second, ...
const RAISE_STEP: f64 = 0.5;
/// ... this often.
const RAISE_INTERVAL_MS: u64 = 60_000;
/// R-MP2 item 3 and D7: the trip rate is forgotten this long after the last signal.
const TRIP_FORGOTTEN_AFTER_MS: u64 = 30 * 60_000;
/// R-MP2 item 5: the minimum time between two log lines of one pubkey.
const LOG_INTERVAL_MS: u64 = 60_000;
/// R-MP2 item 2: a relay close is a signal only if an outbox released a message within this long before it.
const RECENT_RELEASE_MS: u64 = 30_000;

/// The kind of a signal that a relay is not keeping up (R-MP2 item 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalKind {
    /// The data queue toward a relay holds data, or the pressure level of §28.9B RQ1 item 8 is raised.
    Queue,
    /// The last connection to a relay closed while maker data was pending or recently released.
    Close,
}

impl SignalKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SignalKind::Queue => "queue",
            SignalKind::Close => "close",
        }
    }
}

/// Whether the close of the last connection to a relay is a signal (R-MP2 item 2): an outbox holds a pending created
/// or updated message, or released one within the preceding 30 seconds. `last_release_ms` is `None` or 0 when
/// nothing was released yet.
pub(crate) fn close_is_signal(has_pending: bool, last_release_ms: Option<u64>, now_ms: u64) -> bool {
    has_pending
        || last_release_ms.is_some_and(|released| released > 0 && now_ms.saturating_sub(released) <= RECENT_RELEASE_MS)
}

/// A maker-order message waiting in the outbox.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutboxMessage {
    Created(MakerOrderCreated),
    Updated(MakerOrderUpdated),
    Cancelled(MakerOrderCancelled),
}

impl OutboxMessage {
    /// A cancellation is terminal: it is never held back and never merged away (R-MP1 items 1 and 2).
    fn is_cancelled(&self) -> bool { matches!(self, OutboxMessage::Cancelled(_)) }
}

/// The result of merging a new message into the pending one of the same order (R-MP1 item 1).
///
/// | Pending | New | Result |
/// |---|---|---|
/// | none | any | the new message |
/// | created | updated | created, carrying the updated values |
/// | updated | updated | updated, each field with its latest value set in either message |
/// | created or updated | cancelled | cancelled |
/// | cancelled | any | cancelled |
///
/// A new created message replaces a pending created or updated one: it carries the whole state.
pub(crate) fn merge_pending(pending: Option<OutboxMessage>, new: OutboxMessage) -> OutboxMessage {
    match (pending, new) {
        (None, new) => new,
        (Some(cancelled @ OutboxMessage::Cancelled(_)), _) => cancelled,
        (Some(_), cancelled @ OutboxMessage::Cancelled(_)) => cancelled,
        (Some(_), created @ OutboxMessage::Created(_)) => created,
        (Some(OutboxMessage::Created(created)), OutboxMessage::Updated(updated)) => {
            OutboxMessage::Created(apply_updated_to_created(created, &updated))
        },
        (Some(OutboxMessage::Updated(older)), OutboxMessage::Updated(newer)) => {
            OutboxMessage::Updated(merge_updated(&older, &newer))
        },
    }
}

fn apply_updated_to_created(mut created: MakerOrderCreated, updated: &MakerOrderUpdated) -> MakerOrderCreated {
    if let Some(price) = updated.new_price() {
        created.price = price.to_ratio();
    }
    if let Some(max_volume) = updated.new_max_volume() {
        created.max_volume = max_volume.to_ratio();
    }
    if let Some(min_volume) = updated.new_min_volume() {
        created.min_volume = min_volume.to_ratio();
    }
    if let Some(conf_settings) = updated.new_conf_settings() {
        created.conf_settings = conf_settings;
    }
    created.timestamp = created.timestamp.max(updated.timestamp());
    created
}

fn merge_updated(older: &MakerOrderUpdated, newer: &MakerOrderUpdated) -> MakerOrderUpdated {
    let mut merged = MakerOrderUpdated::new(newer.uuid());
    let price: Option<MmNumber> = newer.new_price().or_else(|| older.new_price());
    if let Some(price) = price {
        merged.with_new_price(price.to_ratio());
    }
    if let Some(max_volume) = newer.new_max_volume().or_else(|| older.new_max_volume()) {
        merged.with_new_max_volume(max_volume.to_ratio());
    }
    if let Some(min_volume) = newer.new_min_volume().or_else(|| older.new_min_volume()) {
        merged.with_new_min_volume(min_volume.to_ratio());
    }
    if let Some(conf_settings) = newer.new_conf_settings().or_else(|| older.new_conf_settings()) {
        merged.with_new_conf_settings(conf_settings);
    }
    merged
}

/// A message the outbox hands back for publishing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    /// The orderbook topic of the order's pair.
    pub(crate) topic: String,
    pub(crate) message: OutboxMessage,
    /// The key the message is signed with; `None` for the persistent key of the node.
    pub(crate) key_pair: Option<KeyPair>,
}

/// What the caller logs after a call (R-MP2 item 5). Never carries order content.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RateLog {
    pub(crate) pubkey: String,
    pub(crate) rate: f64,
    pub(crate) degraded: bool,
    /// The kind of the last signal since the previous line, if there was one.
    pub(crate) signal: Option<SignalKind>,
}

/// The rate and the token bucket of one maker pubkey (R-MP2).
#[derive(Debug)]
struct Budget {
    ceiling: f64,
    rate: f64,
    tokens: f64,
    last_refill_ms: u64,
    /// Whether the outbox is in degraded mode: the rate is the floor and nothing is released.
    degraded: bool,
    /// The rate in effect when the last signal outside degraded mode came; the climb stops one step below it.
    trip_rate: Option<f64>,
    last_signal_ms: Option<u64>,
    /// The next rise of the rate after degraded mode.
    next_raise_ms: Option<u64>,
    /// Whether the rate, the degraded state or a signal came since the last log line.
    dirty: bool,
    unlogged_signal: Option<SignalKind>,
    /// The outbox entered degraded mode and the line that says so is not logged yet. That line is never held back by
    /// the limit of one line a minute (R-MP2 item 5).
    entry_line_due: bool,
    last_log_ms: Option<u64>,
}

impl Budget {
    fn new(ceiling: f64, now_ms: u64) -> Self {
        Budget {
            ceiling,
            rate: ceiling,
            tokens: ceiling * BURST_FACTOR,
            last_refill_ms: now_ms,
            degraded: false,
            trip_rate: None,
            last_signal_ms: None,
            next_raise_ms: None,
            dirty: false,
            unlogged_signal: None,
            entry_line_due: false,
            last_log_ms: None,
        }
    }

    fn floor(&self) -> f64 { RATE_FLOOR.min(self.ceiling) }

    /// The bucket holds at most the burst of the current rate: ten times the rate. A lowered rate therefore also
    /// lowers what can leave at once.
    fn capacity(&self) -> f64 { self.rate * BURST_FACTOR }

    /// The highest rate the climb reaches: one step below the trip rate, never below the floor, and the ceiling once
    /// the trip rate is forgotten.
    fn climb_limit(&self) -> f64 {
        match self.trip_rate {
            Some(trip) => (trip - RAISE_STEP).max(self.floor()).min(self.ceiling),
            None => self.ceiling,
        }
    }

    fn is_idle(&self) -> bool {
        self.rate >= self.ceiling
            && self.tokens >= self.ceiling * BURST_FACTOR
            && !self.degraded
            && self.trip_rate.is_none()
            && !self.dirty
    }

    /// A signal at `now_ms` (R-MP2 item 3). Outside degraded mode the rate in effect becomes the trip rate and the
    /// outbox drops to the floor in degraded mode. While degraded the trip rate stays as it is.
    fn signal(&mut self, now_ms: u64, kind: SignalKind) {
        if !self.degraded {
            self.trip_rate = Some(self.rate);
            self.rate = self.floor();
            self.degraded = true;
            self.next_raise_ms = None;
            self.tokens = self.tokens.min(self.capacity());
            self.dirty = true;
            self.unlogged_signal = Some(kind);
            self.entry_line_due = true;
        }
        self.last_signal_ms = Some(now_ms);
    }

    /// Advances the clock: refills the bucket at the rate that held up to now, then adapts the rate (R-MP2 item 3).
    /// `queue_signal` is the level of the queue signal; while it is raised it counts as a signal at every moment.
    fn advance(&mut self, now_ms: u64, queue_signal: bool, ceiling: f64) {
        if ceiling != self.ceiling {
            self.ceiling = ceiling;
            if self.rate > ceiling {
                self.rate = ceiling;
                self.dirty = true;
            }
        }
        let elapsed_ms = now_ms.saturating_sub(self.last_refill_ms);
        self.last_refill_ms = self.last_refill_ms.max(now_ms);
        self.tokens = (self.tokens + self.rate * elapsed_ms as f64 / 1000.0).min(self.capacity());

        if queue_signal {
            self.signal(now_ms, SignalKind::Queue);
        }
        let floor = self.floor();
        if self.degraded {
            if self
                .last_signal_ms
                .is_some_and(|last| now_ms >= last.saturating_add(DEGRADED_FOR_MS))
            {
                self.degraded = false;
                self.rate = floor;
                self.next_raise_ms = Some(now_ms + RAISE_INTERVAL_MS);
                self.dirty = true;
            }
        } else {
            if self.trip_rate.is_some()
                && self
                    .last_signal_ms
                    .is_some_and(|last| now_ms >= last.saturating_add(TRIP_FORGOTTEN_AFTER_MS))
            {
                self.trip_rate = None;
                self.dirty = true;
            }
            let limit = self.climb_limit();
            if self.rate < limit {
                let mut next = *self.next_raise_ms.get_or_insert(now_ms + RAISE_INTERVAL_MS);
                while self.rate < limit && now_ms >= next {
                    self.rate = (self.rate + RAISE_STEP).min(limit);
                    self.dirty = true;
                    next += RAISE_INTERVAL_MS;
                }
                self.next_raise_ms = Some(next);
            } else {
                self.next_raise_ms = None;
            }
        }
        self.tokens = self.tokens.min(self.capacity());
    }

    /// The log line due now, if any, carrying the state at that time. The line that enters degraded mode is due at once;
    /// every other line is held to one per minute, and the entry line counts as the minute's line for them.
    fn take_log(&mut self, now_ms: u64, pubkey: &str) -> Option<RateLog> {
        if !self.dirty {
            return None;
        }
        if !self.entry_line_due {
            if let Some(last) = self.last_log_ms {
                if now_ms.saturating_sub(last) < LOG_INTERVAL_MS {
                    return None;
                }
            }
        }
        self.entry_line_due = false;
        self.dirty = false;
        self.last_log_ms = Some(now_ms);
        Some(RateLog {
            pubkey: pubkey.to_owned(),
            rate: self.rate,
            degraded: self.degraded,
            signal: self.unlogged_signal.take(),
        })
    }
}

#[derive(Debug)]
struct Entry {
    message: OutboxMessage,
    topic: String,
    /// Counted when the order's slot was first filled; the oldest slot is released first (R-MP1 item 4).
    seq: u64,
}

/// The outbox of one maker pubkey.
#[derive(Debug)]
struct PubkeyOutbox {
    key_pair: Option<KeyPair>,
    entries: HashMap<Uuid, Entry>,
    /// The pending orders by slot number, oldest first.
    by_age: BTreeMap<u64, Uuid>,
    next_seq: u64,
    budget: Budget,
}

impl PubkeyOutbox {
    fn new(key_pair: Option<KeyPair>, ceiling: f64, now_ms: u64) -> Self {
        PubkeyOutbox {
            key_pair,
            entries: HashMap::new(),
            by_age: BTreeMap::new(),
            next_seq: 0,
            budget: Budget::new(ceiling, now_ms),
        }
    }

    fn release(&mut self, uuid: &Uuid) -> Option<Release> {
        let entry = self.entries.remove(uuid)?;
        self.by_age.remove(&entry.seq);
        Some(Release {
            topic: entry.topic,
            message: entry.message,
            key_pair: self.key_pair,
        })
    }

    /// Stores `message`, merged into the order's pending one. A cancellation is returned at once.
    fn push(&mut self, uuid: Uuid, topic: String, message: OutboxMessage) -> Option<Release> {
        let pending = self.entries.remove(&uuid);
        let seq = match &pending {
            Some(entry) => entry.seq,
            None => {
                let seq = self.next_seq;
                self.next_seq += 1;
                seq
            },
        };
        let merged = merge_pending(pending.map(|entry| entry.message), message);
        let cancelled = merged.is_cancelled();
        self.by_age.insert(seq, uuid);
        self.entries.insert(uuid, Entry {
            message: merged,
            topic,
            seq,
        });
        if cancelled {
            // Not held back and not counted against the budget (R-MP1 item 2).
            return self.release(&uuid);
        }
        None
    }

    /// Releases pending messages, oldest first, while the bucket holds a token and the outbox is not
    /// degraded (R-MP2 items 1 and 4).
    fn release_budgeted(&mut self) -> Vec<Release> {
        let mut released = Vec::new();
        if self.budget.degraded {
            return released;
        }
        while self.budget.tokens >= 1.0 {
            let Some(uuid) = self.by_age.values().next().copied() else {
                break;
            };
            self.budget.tokens -= 1.0;
            released.extend(self.release(&uuid));
        }
        released
    }
}

/// The aggregate values of R-MP5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OutboxMetrics {
    /// Pending messages, summed over pubkeys.
    pub(crate) pending: usize,
    /// The lowest rate over pubkeys, the ceiling when there is none.
    pub(crate) rate: f64,
    /// Whether any outbox is in degraded mode.
    pub(crate) degraded: bool,
}

/// The outboxes of all maker pubkeys of the node.
#[derive(Debug, Default)]
pub(crate) struct MakerOutboxes {
    by_pubkey: HashMap<String, PubkeyOutbox>,
    /// When the last signal came. The signals are node-wide: an outbox that appears while the node is degraded starts
    /// degraded.
    last_signal: Option<(u64, SignalKind)>,
}

impl MakerOutboxes {
    fn outbox(&mut self, pubkey: &str, key_pair: Option<KeyPair>, ceiling: f64, now_ms: u64) -> &mut PubkeyOutbox {
        let last_signal = self.last_signal;
        self.by_pubkey.entry(pubkey.to_owned()).or_insert_with(|| {
            let mut outbox = PubkeyOutbox::new(key_pair, ceiling, now_ms);
            if let Some((at, kind)) = last_signal {
                if now_ms.saturating_sub(at) < DEGRADED_FOR_MS {
                    outbox.budget.signal(at, kind);
                }
            }
            outbox
        })
    }

    /// Stores a message of the maker `pubkey` and returns what is to be published now: a
    /// cancellation, and the oldest pending messages while the budget allows (R-MP1, R-MP2).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enqueue(
        &mut self,
        now_ms: u64,
        queue_signal: bool,
        ceiling: f64,
        pubkey: &str,
        key_pair: Option<KeyPair>,
        uuid: Uuid,
        topic: String,
        message: OutboxMessage,
    ) -> (Vec<Release>, Option<RateLog>) {
        let outbox = self.outbox(pubkey, key_pair, ceiling, now_ms);
        outbox.budget.advance(now_ms, queue_signal, ceiling);
        let mut released: Vec<Release> = outbox.push(uuid, topic, message).into_iter().collect();
        released.extend(outbox.release_budgeted());
        let log = outbox.budget.take_log(now_ms, pubkey);
        if queue_signal {
            self.last_signal = Some((now_ms, SignalKind::Queue));
        }
        (released, log)
    }

    /// A signal at `now_ms`: every outbox drops to the floor in degraded mode (R-MP2 item 3). The lines that enter
    /// degraded mode are returned to be logged at once (R-MP2 item 5).
    pub(crate) fn signal(&mut self, now_ms: u64, kind: SignalKind) -> Vec<RateLog> {
        self.last_signal = Some((now_ms, kind));
        let mut lines = Vec::new();
        for (pubkey, outbox) in self.by_pubkey.iter_mut() {
            outbox.budget.signal(now_ms, kind);
            lines.extend(outbox.budget.take_log(now_ms, pubkey));
        }
        lines
    }

    /// The periodic step: adapts every pubkey's rate and releases what the budget allows.
    pub(crate) fn tick(&mut self, now_ms: u64, queue_signal: bool, ceiling: f64) -> (Vec<Release>, Vec<RateLog>) {
        let mut released = Vec::new();
        let mut logs = Vec::new();
        if queue_signal {
            self.last_signal = Some((now_ms, SignalKind::Queue));
        }
        for (pubkey, outbox) in self.by_pubkey.iter_mut() {
            outbox.budget.advance(now_ms, queue_signal, ceiling);
            released.extend(outbox.release_budgeted());
            logs.extend(outbox.budget.take_log(now_ms, pubkey));
        }
        // An idle pubkey, at the ceiling with a full bucket and no memory of a signal, is the same as a new one.
        self.by_pubkey
            .retain(|_, outbox| !outbox.entries.is_empty() || !outbox.budget.is_idle());
        // Releases of different pubkeys are published in the order of their slots only within one
        // pubkey; across pubkeys the order is not bound.
        (released, logs)
    }

    pub(crate) fn metrics(&self, ceiling: f64) -> OutboxMetrics {
        OutboxMetrics {
            pending: self.by_pubkey.values().map(|outbox| outbox.entries.len()).sum(),
            rate: self
                .by_pubkey
                .values()
                .map(|outbox| outbox.budget.rate)
                .fold(ceiling, f64::min),
            degraded: self.by_pubkey.values().any(|outbox| outbox.budget.degraded),
        }
    }

    /// The pending messages of `pubkey` on the pairs `topics` (R-MP3 item 2).
    pub(crate) fn pending_for_topics(&self, pubkey: &str, topics: &[String]) -> usize {
        self.by_pubkey.get(pubkey).map_or(0, |outbox| {
            outbox
                .entries
                .values()
                .filter(|entry| topics.contains(&entry.topic))
                .count()
        })
    }

    /// Whether `pubkey` is in degraded mode.
    pub(crate) fn is_degraded(&self, pubkey: &str) -> bool {
        self.by_pubkey.get(pubkey).is_some_and(|outbox| outbox.budget.degraded)
    }

    /// Whether any message is pending.
    pub(crate) fn has_pending(&self) -> bool { self.by_pubkey.values().any(|outbox| !outbox.entries.is_empty()) }

    /// The number of pending messages of `pubkey`.
    #[cfg(test)]
    pub(crate) fn pending_of(&self, pubkey: &str) -> usize {
        self.by_pubkey.get(pubkey).map_or(0, |outbox| outbox.entries.len())
    }

    /// The current rate of `pubkey`, the ceiling if it has no outbox.
    #[cfg(test)]
    pub(crate) fn rate_of(&self, pubkey: &str, ceiling: f64) -> f64 {
        self.by_pubkey.get(pubkey).map_or(ceiling, |outbox| outbox.budget.rate)
    }
}

/// The outbox state kept in the ordermatch context.
#[derive(Debug, Default)]
pub(crate) struct MakerOutbox {
    pub(crate) outboxes: PaMutex<MakerOutboxes>,
    /// Set while a keep-alive round after a relay joined the mesh is scheduled (R-MP4).
    pub(crate) reconnect_round_pending: AtomicBool,
    /// When an outbox last released a created or updated message, in milliseconds since the epoch; 0 for never
    /// (R-MP2 item 2).
    pub(crate) last_release_ms: AtomicU64,
    /// Close signals since start (R-MP5).
    pub(crate) close_signals: AtomicU64,
    /// Messages the outbox released that are not yet handed to the swarm. The bounded wait of R-MP3 item 2 counts them as
    /// pending: a keep-alive must not overtake a message that was released but is still being signed and queued.
    pub(crate) in_flight: AtomicUsize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::lp_ordermatch::{new_protocol, OrderConfirmationsSettings, H64};
    use common::mm_number::BigRational;

    fn ratio(n: i64) -> BigRational { BigRational::from_integer(n.into()) }

    fn created(uuid: Uuid, price: i64, max: i64, min: i64) -> MakerOrderCreated {
        MakerOrderCreated {
            uuid: uuid.into(),
            base: "A".into(),
            rel: "B".into(),
            price: ratio(price),
            max_volume: ratio(max),
            min_volume: ratio(min),
            created_at: 1,
            conf_settings: OrderConfirmationsSettings::default(),
            timestamp: 1,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        }
    }

    fn updated(uuid: Uuid, price: Option<i64>, max: Option<i64>, min: Option<i64>) -> MakerOrderUpdated {
        let mut message = MakerOrderUpdated::new(uuid);
        if let Some(price) = price {
            message.with_new_price(ratio(price));
        }
        if let Some(max) = max {
            message.with_new_max_volume(ratio(max));
        }
        if let Some(min) = min {
            message.with_new_min_volume(ratio(min));
        }
        message
    }

    fn cancelled(uuid: Uuid) -> MakerOrderCancelled {
        MakerOrderCancelled {
            uuid: uuid.into(),
            timestamp: 1,
            pair_trie_root: H64::default(),
        }
    }

    fn as_updated(message: OutboxMessage) -> MakerOrderUpdated {
        match message {
            OutboxMessage::Updated(updated) => updated,
            other => panic!("expected an updated message, got {:?}", other),
        }
    }

    // T-MP1: every row of the R-MP1 item 1 table.
    #[test]
    fn merge_table() {
        let uuid = Uuid::new_v4();

        // none + any
        for new in [
            OutboxMessage::Created(created(uuid, 1, 10, 1)),
            OutboxMessage::Updated(updated(uuid, Some(2), None, None)),
            OutboxMessage::Cancelled(cancelled(uuid)),
        ] {
            assert_eq!(merge_pending(None, new.clone()), new);
        }

        // created + updated = created, carrying the updated values; the rest is kept
        let merged = merge_pending(
            Some(OutboxMessage::Created(created(uuid, 1, 10, 1))),
            OutboxMessage::Updated(updated(uuid, Some(5), None, Some(2))),
        );
        let OutboxMessage::Created(merged) = merged else {
            panic!("created expected")
        };
        assert_eq!(merged.price, ratio(5));
        assert_eq!(merged.max_volume, ratio(10));
        assert_eq!(merged.min_volume, ratio(2));

        // updated + updated = updated, each field with its latest value set in either
        let merged = as_updated(merge_pending(
            Some(OutboxMessage::Updated(updated(uuid, Some(2), Some(20), None))),
            OutboxMessage::Updated(updated(uuid, Some(3), None, Some(4))),
        ));
        assert_eq!(merged.new_price(), Some(ratio(3).into()));
        assert_eq!(merged.new_max_volume(), Some(ratio(20).into()));
        assert_eq!(merged.new_min_volume(), Some(ratio(4).into()));
        assert_eq!(merged.new_conf_settings(), None);

        // created or updated + cancelled = cancelled, the pending message is discarded
        for pending in [
            OutboxMessage::Created(created(uuid, 1, 10, 1)),
            OutboxMessage::Updated(updated(uuid, Some(2), None, None)),
        ] {
            assert_eq!(
                merge_pending(Some(pending), OutboxMessage::Cancelled(cancelled(uuid))),
                OutboxMessage::Cancelled(cancelled(uuid))
            );
        }

        // cancelled + any = cancelled
        for new in [
            OutboxMessage::Created(created(uuid, 1, 10, 1)),
            OutboxMessage::Updated(updated(uuid, Some(2), None, None)),
            OutboxMessage::Cancelled(cancelled(uuid)),
        ] {
            assert_eq!(
                merge_pending(Some(OutboxMessage::Cancelled(cancelled(uuid))), new),
                OutboxMessage::Cancelled(cancelled(uuid))
            );
        }

        // A new created message carries the whole state and replaces a pending update.
        let replaced = merge_pending(
            Some(OutboxMessage::Updated(updated(uuid, Some(2), None, None))),
            OutboxMessage::Created(created(uuid, 7, 10, 1)),
        );
        assert_eq!(replaced, OutboxMessage::Created(created(uuid, 7, 10, 1)));
    }

    const CEILING: f64 = 10.0;
    const PK: &str = "02aa";
    const MIN: u64 = 60_000;

    fn enqueue_with(
        outboxes: &mut MakerOutboxes,
        now_ms: u64,
        queue_signal: bool,
        ceiling: f64,
        uuid: Uuid,
        topic: &str,
        message: OutboxMessage,
    ) -> Vec<Release> {
        outboxes
            .enqueue(now_ms, queue_signal, ceiling, PK, None, uuid, topic.to_owned(), message)
            .0
    }

    fn enqueue(
        outboxes: &mut MakerOutboxes,
        now_ms: u64,
        queue_signal: bool,
        uuid: Uuid,
        topic: &str,
        message: OutboxMessage,
    ) -> Vec<Release> {
        enqueue_with(outboxes, now_ms, queue_signal, CEILING, uuid, topic, message)
    }

    fn update_msg(uuid: Uuid, price: i64) -> OutboxMessage {
        OutboxMessage::Updated(updated(uuid, Some(price), None, None))
    }

    /// An outbox for `PK` that exists, with the bucket full, at `ceiling`.
    fn outboxes_with_pk(now_ms: u64, ceiling: f64) -> MakerOutboxes {
        let mut outboxes = MakerOutboxes::default();
        let uuid = Uuid::new_v4();
        enqueue_with(&mut outboxes, now_ms, false, ceiling, uuid, "t", update_msg(uuid, 1));
        outboxes
    }

    /// Ticks from `from_ms` to `to_ms` in steps of `step_ms`; the queue signal is raised until `raised_until_ms`.
    fn run(outboxes: &mut MakerOutboxes, from_ms: u64, to_ms: u64, step_ms: u64, ceiling: f64, raised_until_ms: u64) {
        let mut t = from_ms;
        while t <= to_ms {
            outboxes.tick(t, t < raised_until_ms, ceiling);
            t += step_ms;
        }
    }

    // T-MP2: a cancellation is released at once with the budget exhausted and in degraded mode.
    #[test]
    fn cancellation_is_never_held_back() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        // 600 orders: the burst of 100 leaves, the rest waits.
        let uuids: Vec<Uuid> = (0..600).map(|_| Uuid::new_v4()).collect();
        for uuid in &uuids {
            enqueue(&mut outboxes, t0, false, *uuid, "t", update_msg(*uuid, 1));
        }
        let pending_before = outboxes.pending_of(PK);
        assert_eq!(pending_before, 500);
        // Exhausted budget: the cancellation leaves at once, the order's pending update is discarded.
        let victim = uuids[599];
        let released = enqueue(
            &mut outboxes,
            t0,
            false,
            victim,
            "t",
            OutboxMessage::Cancelled(cancelled(victim)),
        );
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].message, OutboxMessage::Cancelled(cancelled(victim)));
        assert_eq!(outboxes.pending_of(PK), pending_before - 1);
        // Degraded mode: the same.
        outboxes.signal(t0 + 1_000, SignalKind::Close);
        assert!(outboxes.is_degraded(PK));
        let victim = uuids[598];
        let released = enqueue(
            &mut outboxes,
            t0 + 2_000,
            false,
            victim,
            "t",
            OutboxMessage::Cancelled(cancelled(victim)),
        );
        assert_eq!(released.len(), 1);
        assert_eq!(outboxes.pending_of(PK), pending_before - 2);
    }

    #[test]
    fn healthy_relay_gets_every_update_at_once() {
        // T-MP9: below the ceiling every update is released as it is made, with no merging visible.
        let mut outboxes = MakerOutboxes::default();
        let uuid = Uuid::new_v4();
        let mut now = 1_000_000;
        for price in 1..=50 {
            now += 200; // 5 updates/s of one order, below the ceiling of 10
            let released = enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, price));
            assert_eq!(released.len(), 1, "update {} was held back", price);
            assert_eq!(
                as_updated(released[0].message.clone()).new_price(),
                Some(ratio(price).into())
            );
        }
        assert_eq!(outboxes.pending_of(PK), 0);
    }

    // T-MP4: the ceiling and the burst.
    #[test]
    fn burst_and_ceiling_are_respected() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        // 250 distinct orders at the same instant: the burst of 100 leaves, the rest waits.
        let uuids: Vec<Uuid> = (0..250).map(|_| Uuid::new_v4()).collect();
        let mut released = 0;
        for uuid in &uuids {
            released += enqueue(&mut outboxes, now, false, *uuid, "t", update_msg(*uuid, 1)).len();
        }
        assert_eq!(released, 100);
        assert_eq!(outboxes.pending_of(PK), 150);
        // One second later the ceiling lets 10 more leave.
        let (out, _) = outboxes.tick(now + 1_000, false, CEILING);
        assert_eq!(out.len(), 10);
        // Over 10 s, exactly 100 more.
        let mut total = 0;
        for s in 2..=11 {
            total += outboxes.tick(now + s * 1_000, false, CEILING).0.len();
        }
        assert_eq!(total, 100);
    }

    #[test]
    fn oldest_slot_leaves_first_and_a_merge_keeps_the_slot() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        // Exhaust the burst.
        for _ in 0..100 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, 1));
        }
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for uuid in [a, b, c] {
            assert!(enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, 1)).is_empty());
        }
        // A newer value of `a` merges into its slot: `a` is still the oldest.
        assert!(enqueue(&mut outboxes, now, false, a, "t", update_msg(a, 2)).is_empty());
        assert_eq!(outboxes.pending_of(PK), 3);
        let (out, _) = outboxes.tick(now + 200, false, CEILING);
        assert_eq!(out.len(), 2);
        let ids: Vec<Uuid> = out.iter().map(|r| as_updated(r.message.clone()).uuid()).collect();
        assert_eq!(ids, vec![a, b]);
        assert_eq!(
            as_updated(out[0].message.clone()).new_price(),
            Some(ratio(2).into()),
            "the merged value is the latest one"
        );
    }

    // T-MP4: a close signal, and the queue signal becoming raised, drop the rate to the floor in degraded mode;
    // nothing created or updated is released then.
    #[test]
    fn a_signal_drops_the_rate_to_the_floor_and_stops_the_releases() {
        for kind in [SignalKind::Close, SignalKind::Queue] {
            let t0 = 1_000_000;
            let mut outboxes = outboxes_with_pk(t0, CEILING);
            assert_eq!(outboxes.rate_of(PK, CEILING), 10.0);
            assert!(!outboxes.is_degraded(PK));
            match kind {
                SignalKind::Close => {
                    let _ = outboxes.signal(t0 + 1_000, kind);
                },
                SignalKind::Queue => {
                    outboxes.tick(t0 + 1_000, true, CEILING);
                },
            }
            assert_eq!(outboxes.rate_of(PK, CEILING), 0.5, "{:?}", kind);
            assert!(outboxes.is_degraded(PK), "{:?}", kind);
            // Plenty of tokens had been banked, but degraded mode releases nothing.
            for i in 0..50 {
                let uuid = Uuid::new_v4();
                assert!(enqueue(&mut outboxes, t0 + 2_000 + i, false, uuid, "t", update_msg(uuid, 1)).is_empty());
            }
            assert!(outboxes.tick(t0 + 60_000, false, CEILING).0.is_empty());
            assert_eq!(outboxes.pending_of(PK), 50);
        }
    }

    // T-MP4 / T-MP5: a relay close is a signal only with recent maker traffic.
    #[test]
    fn a_close_is_a_signal_only_with_pending_or_recent_traffic() {
        let now = 10_000_000;
        assert!(!close_is_signal(false, None, now));
        assert!(!close_is_signal(false, Some(0), now));
        assert!(close_is_signal(true, None, now), "a pending message");
        assert!(close_is_signal(false, Some(now - 29_000), now), "released 29 s ago");
        assert!(close_is_signal(false, Some(now - 30_000), now), "released 30 s ago");
        assert!(!close_is_signal(false, Some(now - 31_000), now), "released 31 s ago");
    }

    // T-MP4: the outbox leaves degraded mode 5 min after the last signal, and not earlier while the queue signal stays
    // raised.
    #[test]
    fn degraded_mode_ends_five_minutes_after_the_last_signal() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, CEILING);
        outboxes.signal(t0, SignalKind::Close);
        outboxes.tick(t0 + 5 * MIN - 1, false, CEILING);
        assert!(outboxes.is_degraded(PK));
        outboxes.tick(t0 + 5 * MIN, false, CEILING);
        assert!(!outboxes.is_degraded(PK));
        assert_eq!(outboxes.rate_of(PK, CEILING), 0.5);

        // A queue signal that stays raised counts at every moment: degraded 5 min after it clears.
        let t1 = t0 + 20 * MIN;
        outboxes.signal(t1, SignalKind::Close);
        run(&mut outboxes, t1, t1 + 10 * MIN, 10_000, CEILING, t1 + 10 * MIN);
        assert!(outboxes.is_degraded(PK), "still raised after 10 min");
        let cleared = t1 + 10 * MIN;
        run(&mut outboxes, cleared, cleared + 5 * MIN - 20_000, 10_000, CEILING, 0);
        assert!(outboxes.is_degraded(PK), "4 min 40 s after it cleared");
        outboxes.tick(cleared + 5 * MIN, false, CEILING);
        assert!(!outboxes.is_degraded(PK));
    }

    // T-MP4: the rate rises by 0.5/s every 60 s and stops one step below the trip rate: with a trip rate of 3/s, at
    // 2.5/s.
    #[test]
    fn the_climb_stops_one_step_below_the_trip_rate() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, 3.0);
        outboxes.signal(t0, SignalKind::Close);
        let left = t0 + 5 * MIN;
        run(&mut outboxes, t0, left, 10_000, 3.0, 0);
        assert_eq!(outboxes.rate_of(PK, 3.0), 0.5);
        for (minutes, expected) in [(1, 1.0), (2, 1.5), (3, 2.0), (4, 2.5), (5, 2.5), (20, 2.5)] {
            run(&mut outboxes, left, left + minutes * MIN, 10_000, 3.0, 0);
            // Between the steps the rate does not move.
            assert_eq!(outboxes.rate_of(PK, 3.0), expected, "after {} min of climbing", minutes);
        }
        // Not one step earlier.
        let mut outboxes = outboxes_with_pk(t0, 3.0);
        outboxes.signal(t0, SignalKind::Close);
        run(&mut outboxes, t0, left + MIN - 10_000, 10_000, 3.0, 0);
        assert_eq!(outboxes.rate_of(PK, 3.0), 0.5);
    }

    // T-MP4: a signal while degraded does not change the trip rate.
    #[test]
    fn a_signal_while_degraded_keeps_the_trip_rate() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, 3.0);
        outboxes.signal(t0, SignalKind::Close);
        // Another signal 2 min later, while degraded at the floor of 0.5.
        outboxes.signal(t0 + 2 * MIN, SignalKind::Close);
        outboxes.signal(t0 + 3 * MIN, SignalKind::Queue);
        // 5 min after the last signal, the climb goes up to 2.5, the limit of the first trip rate of 3, and not
        // to the floor, which a trip rate of 0.5 would give.
        let left = t0 + 8 * MIN;
        run(&mut outboxes, t0, left + 4 * MIN, 10_000, 3.0, 0);
        assert!(!outboxes.is_degraded(PK));
        assert_eq!(outboxes.rate_of(PK, 3.0), 2.5);
    }

    // T-MP4 and D7: after 30 min without a signal the trip rate is forgotten and the climb goes on to the ceiling.
    #[test]
    fn the_trip_rate_is_forgotten_after_thirty_minutes_without_a_signal() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, 3.0);
        outboxes.signal(t0, SignalKind::Close);
        run(&mut outboxes, t0, t0 + 30 * MIN - 10_000, 10_000, 3.0, 0);
        assert_eq!(outboxes.rate_of(PK, 3.0), 2.5, "held below the trip rate");
        run(&mut outboxes, t0 + 30 * MIN, t0 + 32 * MIN, 10_000, 3.0, 0);
        assert_eq!(outboxes.rate_of(PK, 3.0), 3.0, "the ceiling");
        // And the memory is gone: the outbox is idle again once the bucket is full.
        run(&mut outboxes, t0 + 32 * MIN, t0 + 34 * MIN, 10_000, 3.0, 0);
        assert!(outboxes.by_pubkey.is_empty());
    }

    // After a signal outside degraded mode during the climb, the rate then in effect is the trip rate.
    #[test]
    fn a_signal_during_the_climb_records_the_rate_then_in_effect() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, CEILING);
        outboxes.signal(t0, SignalKind::Close);
        let left = t0 + 5 * MIN;
        run(&mut outboxes, t0, left + 4 * MIN, 10_000, CEILING, 0);
        assert_eq!(outboxes.rate_of(PK, CEILING), 2.5);
        outboxes.signal(left + 4 * MIN + 1_000, SignalKind::Close);
        assert!(outboxes.is_degraded(PK));
        let again = left + 4 * MIN + 1_000 + 5 * MIN;
        run(&mut outboxes, left + 4 * MIN, again + 20 * MIN, 10_000, CEILING, 0);
        assert_eq!(
            outboxes.rate_of(PK, CEILING),
            2.0,
            "one step below the new trip rate of 2.5"
        );
    }

    #[test]
    fn an_outbox_that_appears_while_the_node_is_degraded_starts_degraded() {
        let t0 = 1_000_000;
        let mut outboxes = outboxes_with_pk(t0, CEILING);
        outboxes.signal(t0 + 1_000, SignalKind::Close);
        let (released, _) = outboxes.enqueue(
            t0 + 2_000,
            false,
            CEILING,
            "03bb",
            None,
            Uuid::new_v4(),
            "t".into(),
            update_msg(Uuid::new_v4(), 1),
        );
        assert!(released.is_empty());
        assert!(outboxes.is_degraded("03bb"));
    }

    // R-MP2 item 5: the line that enters degraded mode is logged at once, whatever came before, and names the signal
    // kind; the other lines are held to one a minute, and the entry line counts as the minute's line for them.
    #[test]
    fn the_line_that_enters_degraded_mode_is_logged_at_once() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        let uuid = Uuid::new_v4();
        enqueue(&mut outboxes, t0, false, uuid, "t", update_msg(uuid, 1));

        // The signal returns its line at once: a warning-level state (degraded) naming the kind.
        let entry = outboxes.signal(t0 + 1_000, SignalKind::Close);
        assert_eq!(entry.len(), 1);
        assert!(entry[0].degraded);
        assert_eq!(entry[0].signal, Some(SignalKind::Close));
        assert_eq!(entry[0].rate, 0.5);
        assert_eq!(entry[0].pubkey, PK);
        // A second signal while degraded is no entry: no new line.
        assert!(outboxes.signal(t0 + 2_000, SignalKind::Close).is_empty());

        // Leaving degraded mode is a plain line, 5 min after the last signal.
        let mut info = Vec::new();
        for s in 0..=300 {
            let (_, logs) = outboxes.tick(t0 + 2_000 + s * 1_000, false, CEILING);
            info.extend(logs.into_iter().map(|log| (s, log)));
        }
        assert_eq!(info.len(), 1, "{:?}", info);
        assert_eq!(info[0].0, 300);
        assert!(!info[0].1.degraded);
        assert_eq!(info[0].1.signal, None);
        let info_at = t0 + 2_000 + 300_000;

        // A signal 10 s after that info line: its line is not held back by the minute limit.
        let entry = outboxes.signal(info_at + 10_000, SignalKind::Queue);
        assert_eq!(entry.len(), 1, "an entry line is never delayed");
        assert!(entry[0].degraded);
        assert_eq!(entry[0].signal, Some(SignalKind::Queue));
        assert_eq!(entry[0].rate, 0.5);

        // The entry line counted as the minute's line: a change that comes 20 s later waits for the minute.
        let mut held = Vec::new();
        let mut now = info_at + 10_000;
        while now < info_at + 10_000 + 70_000 {
            now += 1_000;
            // The ceiling falls, a rate change the outbox would log.
            let (_, logs) = outboxes.tick(now, false, 0.25);
            held.extend(logs.into_iter().map(|log| (now - (info_at + 10_000), log)));
        }
        assert_eq!(held.len(), 1, "{:?}", held);
        assert!(
            held[0].0 >= 60_000,
            "the change was logged {} ms after the entry line",
            held[0].0
        );
        // The signal kind of a line is the last one since the previous line.
        assert_eq!(held[0].1.signal, None);
    }

    #[test]
    fn log_lines_without_a_signal_are_limited_to_one_per_minute() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        let uuid = Uuid::new_v4();
        enqueue(&mut outboxes, t0, false, uuid, "t", update_msg(uuid, 1));
        let _ = outboxes.signal(t0, SignalKind::Close);
        // The climb back up changes the rate every minute: one line each time, never faster.
        let mut lines = Vec::new();
        for s in 0..=1_200 {
            let (_, logs) = outboxes.tick(t0 + s * 1_000, false, CEILING);
            lines.extend(logs.into_iter().map(|log| (s, log)));
        }
        assert!(lines.len() >= 5, "{:?}", lines);
        for pair in lines.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= 60, "{:?}", lines);
        }
    }

    #[test]
    fn metrics_are_aggregates_over_pubkeys() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        assert_eq!(outboxes.metrics(CEILING), OutboxMetrics {
            pending: 0,
            rate: CEILING,
            degraded: false
        });
        for _ in 0..105 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, 1));
        }
        let uuid = Uuid::new_v4();
        outboxes.enqueue(now, false, CEILING, "03bb", None, uuid, "t".into(), update_msg(uuid, 1));
        let metrics = outboxes.metrics(CEILING);
        assert_eq!((metrics.pending, metrics.rate, metrics.degraded), (5, CEILING, false));
        outboxes.signal(now + 1_000, SignalKind::Close);
        let metrics = outboxes.metrics(CEILING);
        assert_eq!((metrics.rate, metrics.degraded), (0.5, true));
    }

    #[test]
    fn an_idle_pubkey_is_forgotten() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        let uuid = Uuid::new_v4();
        enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, 1));
        assert_eq!(outboxes.by_pubkey.len(), 1);
        // The bucket refills within a second (one token was used), then the pubkey is idle.
        outboxes.tick(now + 1_000, false, CEILING);
        assert!(outboxes.by_pubkey.is_empty());
    }

    #[test]
    fn classification_of_the_ordermatch_messages() {
        // T-MP12 (classification): the cancellation and the four swap-negotiation messages only.
        let uuid = Uuid::new_v4();
        let cancelled = new_protocol::OrdermatchMessage::MakerOrderCancelled(cancelled(uuid));
        assert!(cancelled.is_priority());
        let created = new_protocol::OrdermatchMessage::MakerOrderCreated(created(uuid, 1, 1, 1));
        assert!(!created.is_priority());
        let updated = new_protocol::OrdermatchMessage::MakerOrderUpdated(updated(uuid, Some(1), None, None));
        assert!(!updated.is_priority());
        let keep_alive = new_protocol::OrdermatchMessage::PubkeyKeepAlive(new_protocol::PubkeyKeepAlive {
            trie_roots: HashMap::new(),
            timestamp: 1,
        });
        assert!(!keep_alive.is_priority());
    }
}
