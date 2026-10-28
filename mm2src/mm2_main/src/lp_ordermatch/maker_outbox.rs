//! The maker outbox (CRD ch.32 §32.9B R-MP1 to R-MP4, Reloaded-original).
//!
//! The order messages a node publishes for its own maker orders wait here, per maker pubkey, with at
//! most one pending message per order, and leave at a rate that follows the capacity of the relays.
//! The logic is pure: every method takes the current time in milliseconds and the pressure level as
//! arguments, so the tests inject a clock and need no sleeping. The publishing itself, and the
//! signing, are done by the caller with what the methods return.

use super::new_protocol::{MakerOrderCancelled, MakerOrderCreated, MakerOrderUpdated};
use common::mm_number::MmNumber;
use keys::KeyPair;
use parking_lot::Mutex as PaMutex;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;
use uuid::Uuid;

/// R-MP2 item 1: the default of `maker_update_rate_max`, messages per second per maker pubkey.
pub(crate) const DEFAULT_MAKER_UPDATE_RATE_MAX: f64 = 10.0;
/// R-MP2 item 1: the burst is this many times the ceiling.
const BURST_FACTOR: f64 = 10.0;
/// R-MP2 item 3: the lowest rate under pressure, messages per second.
const RATE_FLOOR: f64 = 0.5;
/// R-MP2 item 3: while under pressure the rate is halved this often.
const HALVE_INTERVAL_MS: u64 = 5_000;
/// R-MP2 item 3: the rate starts to rise once the node has been out of pressure this long.
const RECOVERY_DELAY_MS: u64 = 30_000;
/// R-MP2 item 3: the rate rises by [`RAISE_STEP`] this often.
const RAISE_INTERVAL_MS: u64 = 5_000;
/// R-MP2 item 3: the amount, in messages per second, by which the rate rises.
const RAISE_STEP: f64 = 1.0;
/// R-MP2 item 5: the minimum time between two log lines of one pubkey.
const LOG_INTERVAL_MS: u64 = 60_000;

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
}

/// The rate adaptation and the token bucket of one maker pubkey (R-MP2).
#[derive(Debug)]
struct Budget {
    ceiling: f64,
    rate: f64,
    tokens: f64,
    last_refill_ms: u64,
    /// The next halving while under pressure.
    next_halve_ms: Option<u64>,
    /// Start of the current interval out of pressure.
    clear_since_ms: Option<u64>,
    /// The next rise while recovering.
    next_raise_ms: Option<u64>,
    /// Whether the rate or the degraded state changed since the last log line.
    dirty: bool,
    was_degraded: bool,
    last_log_ms: Option<u64>,
    /// Measurement only: the state of candidates C5a and C5b.
    #[cfg(test)]
    lab: LabBudget,
}

/// Measurement only: the state of candidates C5a (a relay connection close puts the outbox in degraded mode for 5
/// minutes, then the rate rises slowly) and C5b (AIMD on closes).
#[cfg(test)]
#[derive(Debug)]
struct LabBudget {
    degraded: bool,
    last_signal_ms: Option<u64>,
    next_raise_ms: Option<u64>,
    /// C5b: the rate ceiling recorded at the last close, and when it grows next.
    ceiling_b: f64,
    ceiling_grow_ms: Option<u64>,
    pressure_prev: bool,
}

impl Budget {
    fn new(ceiling: f64, now_ms: u64) -> Self {
        Budget {
            ceiling,
            rate: ceiling,
            tokens: ceiling * BURST_FACTOR,
            last_refill_ms: now_ms,
            next_halve_ms: None,
            clear_since_ms: Some(now_ms),
            next_raise_ms: None,
            dirty: false,
            was_degraded: false,
            last_log_ms: None,
            #[cfg(test)]
            lab: LabBudget {
                degraded: false,
                last_signal_ms: None,
                next_raise_ms: None,
                ceiling_b: ceiling,
                ceiling_grow_ms: None,
                pressure_prev: false,
            },
        }
    }

    /// Measurement only: a qualified close of a relay connection, or a rise of the pressure level.
    #[cfg(test)]
    fn lab_signal(&mut self, now_ms: u64, mode: u8) {
        let floor = self.floor();
        self.lab.last_signal_ms = Some(now_ms);
        self.dirty = true;
        if mode == lab::MODE_C5A {
            self.lab.degraded = true;
            self.rate = floor;
            self.lab.next_raise_ms = None;
        } else {
            let half = self.rate / 2.0;
            self.lab.ceiling_b = half;
            if half < floor {
                self.lab.degraded = true;
                self.rate = floor;
            } else {
                self.rate = half;
            }
            self.lab.next_raise_ms = Some(now_ms + 60_000);
            self.lab.ceiling_grow_ms = Some(now_ms + 300_000);
        }
    }

    /// Measurement only: the rate adaptation of C5a and C5b. The pressure-driven adaptation is off; a rise of the
    /// pressure level counts as a close.
    #[cfg(test)]
    fn lab_advance(&mut self, now_ms: u64, pressure: bool, cap: f64, mode: u8) {
        let floor = self.floor();
        if pressure && !self.lab.pressure_prev {
            self.lab_signal(now_ms, mode);
        } else if pressure && mode == lab::MODE_C5A {
            self.lab.last_signal_ms = Some(now_ms);
        }
        self.lab.pressure_prev = pressure;
        self.next_halve_ms = None;
        self.clear_since_ms = None;
        if mode == lab::MODE_C5A {
            if self.lab.degraded {
                if self.lab.last_signal_ms.is_some_and(|last| now_ms >= last + 300_000) {
                    self.lab.degraded = false;
                    self.rate = floor;
                    self.lab.next_raise_ms = Some(now_ms + 60_000);
                    self.dirty = true;
                }
            } else if let Some(mut next) = self.lab.next_raise_ms {
                while self.rate < cap && now_ms >= next {
                    self.rate = (self.rate + 0.5).min(cap);
                    next += 60_000;
                    self.dirty = true;
                }
                self.lab.next_raise_ms = Some(next);
            }
        } else if let Some(last) = self.lab.last_signal_ms {
            if self.lab.degraded {
                if now_ms >= last + 60_000 {
                    self.lab.degraded = false;
                    self.rate = floor;
                    self.lab.next_raise_ms = Some(now_ms + 30_000);
                    self.dirty = true;
                }
            } else if let Some(mut next) = self.lab.next_raise_ms {
                while self.rate < self.lab.ceiling_b && now_ms >= next {
                    self.rate = (self.rate + 0.5).min(self.lab.ceiling_b);
                    next += 30_000;
                    self.dirty = true;
                }
                self.lab.next_raise_ms = Some(next);
            }
            if let Some(mut grow) = self.lab.ceiling_grow_ms {
                while now_ms >= grow && self.lab.ceiling_b < cap {
                    self.lab.ceiling_b = (self.lab.ceiling_b + 0.5).min(cap);
                    grow += 300_000;
                }
                self.lab.ceiling_grow_ms = Some(grow);
            }
        }
    }

    fn floor(&self) -> f64 { RATE_FLOOR.min(self.ceiling) }

    /// The bucket holds at most the burst of the current rate: ten times the rate, at most ten times
    /// the ceiling. A lowered rate therefore also lowers what can leave at once.
    fn capacity(&self) -> f64 { self.rate * BURST_FACTOR }

    fn degraded(&self, pressure: bool) -> bool {
        #[cfg(test)]
        if lab::mode() != 0 {
            return self.lab.degraded;
        }
        pressure && self.rate <= self.floor()
    }

    fn is_idle(&self) -> bool {
        self.rate >= self.ceiling
            && self.tokens >= self.ceiling * BURST_FACTOR
            && self.next_halve_ms.is_none()
            && !self.dirty
    }

    /// Advances the clock: refills the bucket at the rate that held up to now, then adapts the rate
    /// to the pressure level (R-MP2 item 3).
    ///
    /// While under pressure the rate halves every 5 s, the first time 5 s after the pressure began,
    /// down to the floor. Once out of pressure for 30 s it rises by 1 message per second, first at
    /// that moment and then every 5 s, up to the ceiling.
    fn advance(&mut self, now_ms: u64, pressure: bool, ceiling: f64) {
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

        let floor = self.floor();
        #[cfg(test)]
        let lab_mode = lab::mode();
        #[cfg(not(test))]
        let lab_mode = 0u8;
        if lab_mode != 0 {
            #[cfg(test)]
            self.lab_advance(now_ms, pressure, ceiling, lab_mode);
        } else if pressure {
            self.clear_since_ms = None;
            self.next_raise_ms = None;
            let next = *self.next_halve_ms.get_or_insert(now_ms + HALVE_INTERVAL_MS);
            let mut next = next;
            while self.rate > floor && now_ms >= next {
                self.rate = (self.rate / 2.0).max(floor);
                self.dirty = true;
                next += HALVE_INTERVAL_MS;
            }
            self.next_halve_ms = Some(next);
        } else {
            self.next_halve_ms = None;
            let since = *self.clear_since_ms.get_or_insert(now_ms);
            if self.rate < self.ceiling && now_ms >= since + RECOVERY_DELAY_MS {
                let mut next = *self.next_raise_ms.get_or_insert(since + RECOVERY_DELAY_MS);
                while self.rate < self.ceiling && now_ms >= next {
                    self.rate = (self.rate + RAISE_STEP).min(self.ceiling);
                    self.dirty = true;
                    next += RAISE_INTERVAL_MS;
                }
                self.next_raise_ms = Some(next);
            }
        }
        self.tokens = self.tokens.min(self.capacity());

        let degraded = self.degraded(pressure);
        if degraded != self.was_degraded {
            self.was_degraded = degraded;
            self.dirty = true;
        }
    }

    /// The log line due now, if any: at most one per minute, carrying the state at that time.
    fn take_log(&mut self, now_ms: u64, pubkey: &str, pressure: bool) -> Option<RateLog> {
        if !self.dirty {
            return None;
        }
        if let Some(last) = self.last_log_ms {
            if now_ms.saturating_sub(last) < LOG_INTERVAL_MS {
                return None;
            }
        }
        self.dirty = false;
        self.last_log_ms = Some(now_ms);
        Some(RateLog {
            pubkey: pubkey.to_owned(),
            rate: self.rate,
            degraded: self.degraded(pressure),
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
    pubkey: String,
    key_pair: Option<KeyPair>,
    entries: HashMap<Uuid, Entry>,
    /// The pending orders by slot number, oldest first.
    by_age: BTreeMap<u64, Uuid>,
    next_seq: u64,
    budget: Budget,
}

impl PubkeyOutbox {
    fn new(pubkey: String, key_pair: Option<KeyPair>, ceiling: f64, now_ms: u64) -> Self {
        PubkeyOutbox {
            pubkey,
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
    fn release_budgeted(&mut self, pressure: bool) -> Vec<Release> {
        let mut released = Vec::new();
        if self.budget.degraded(pressure) {
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

    /// Releases every pending message of `topic`, oldest first, without regard to the budget
    /// (R-MP3 item 2).
    fn release_topic(&mut self, topic: &str) -> Vec<Release> {
        let uuids: Vec<Uuid> = self
            .by_age
            .values()
            .filter(|uuid| self.entries.get(uuid).is_some_and(|entry| entry.topic == topic))
            .copied()
            .collect();
        uuids.iter().filter_map(|uuid| self.release(uuid)).collect()
    }
}

/// The aggregate values of R-MP5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OutboxMetrics {
    /// Pending messages, summed over pubkeys.
    pub(crate) pending: usize,
    /// The lowest rate over pubkeys, the ceiling when there is none.
    pub(crate) rate: f64,
}

/// The outboxes of all maker pubkeys of the node.
#[derive(Debug, Default)]
pub(crate) struct MakerOutboxes {
    by_pubkey: HashMap<String, PubkeyOutbox>,
}

impl MakerOutboxes {
    fn outbox(&mut self, pubkey: &str, key_pair: Option<KeyPair>, ceiling: f64, now_ms: u64) -> &mut PubkeyOutbox {
        self.by_pubkey
            .entry(pubkey.to_owned())
            .or_insert_with(|| PubkeyOutbox::new(pubkey.to_owned(), key_pair, ceiling, now_ms))
    }

    /// Stores a message of the maker `pubkey` and returns what is to be published now: a
    /// cancellation, and the oldest pending messages while the budget allows (R-MP1, R-MP2).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enqueue(
        &mut self,
        now_ms: u64,
        pressure: bool,
        ceiling: f64,
        pubkey: &str,
        key_pair: Option<KeyPair>,
        uuid: Uuid,
        topic: String,
        message: OutboxMessage,
    ) -> (Vec<Release>, Option<RateLog>) {
        let outbox = self.outbox(pubkey, key_pair, ceiling, now_ms);
        outbox.budget.advance(now_ms, pressure, ceiling);
        let mut released: Vec<Release> = outbox.push(uuid, topic, message).into_iter().collect();
        released.extend(outbox.release_budgeted(pressure));
        let log = outbox.budget.take_log(now_ms, pubkey, pressure);
        (released, log)
    }

    /// Releases the pending messages of the pair `topic` of `pubkey` before the keep-alive of that
    /// pair (R-MP3 item 2). In degraded mode nothing is released (R-MP2 item 4).
    pub(crate) fn flush_pair(
        &mut self,
        now_ms: u64,
        pressure: bool,
        ceiling: f64,
        pubkey: &str,
        topic: &str,
    ) -> (Vec<Release>, Option<RateLog>) {
        let Some(outbox) = self.by_pubkey.get_mut(pubkey) else {
            return (Vec::new(), None);
        };
        outbox.budget.advance(now_ms, pressure, ceiling);
        let released = if outbox.budget.degraded(pressure) {
            Vec::new()
        } else {
            outbox.release_topic(topic)
        };
        let log = outbox.budget.take_log(now_ms, pubkey, pressure);
        (released, log)
    }

    /// The periodic step: adapts every pubkey's rate and releases what the budget allows.
    pub(crate) fn tick(&mut self, now_ms: u64, pressure: bool, ceiling: f64) -> (Vec<Release>, Vec<RateLog>) {
        let mut released = Vec::new();
        let mut logs = Vec::new();
        for (pubkey, outbox) in self.by_pubkey.iter_mut() {
            outbox.budget.advance(now_ms, pressure, ceiling);
            released.extend(outbox.release_budgeted(pressure));
            logs.extend(outbox.budget.take_log(now_ms, pubkey, pressure));
        }
        // An idle pubkey, at the ceiling with a full bucket, is the same as a new one.
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
        }
    }

    /// Measurement only: a qualified close of a relay connection (candidates C5a, C5b).
    #[cfg(test)]
    pub(crate) fn lab_signal(&mut self, now_ms: u64, mode: u8) {
        for outbox in self.by_pubkey.values_mut() {
            outbox.budget.lab_signal(now_ms, mode);
        }
    }

    /// Measurement only: whether any message is pending.
    #[cfg(test)]
    pub(crate) fn has_pending(&self) -> bool { self.by_pubkey.values().any(|outbox| !outbox.entries.is_empty()) }

    /// Measurement only: the pending messages of `pubkey` on the pairs `topics`.
    #[cfg(test)]
    pub(crate) fn pending_for_topics(&self, pubkey: &str, topics: &[String]) -> usize {
        self.by_pubkey.get(pubkey).map_or(0, |outbox| {
            outbox
                .entries
                .values()
                .filter(|entry| topics.contains(&entry.topic))
                .count()
        })
    }

    /// Measurement only: whether `pubkey` is in degraded mode under `pressure`.
    #[cfg(test)]
    pub(crate) fn degraded_of(&self, pubkey: &str, pressure: bool) -> bool {
        self.by_pubkey
            .get(pubkey)
            .is_some_and(|outbox| outbox.budget.degraded(pressure))
    }

    /// Measurement only: whether any pubkey is in degraded mode under `pressure`.
    #[cfg(test)]
    pub(crate) fn any_degraded(&self, pressure: bool) -> bool {
        self.by_pubkey.values().any(|outbox| outbox.budget.degraded(pressure))
    }

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
}

/// Measurement knobs of the maker publication rate lab (tests only; the defaults are the shipped behaviour).
#[cfg(test)]
pub(crate) mod lab {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    /// Candidate C1: the flush before a keep-alive and the keep-alive round release pending messages only within
    /// the budget. A keep-alive goes out when its pair has nothing pending, or when `SLOT_MS` (a round: `ROUND_MS`)
    /// has passed, or at once in degraded mode.
    pub(crate) static BUDGETED_FLUSH: AtomicBool = AtomicBool::new(false);
    pub(crate) static SLOT_MS: AtomicU64 = AtomicU64::new(30_000);
    pub(crate) static ROUND_MS: AtomicU64 = AtomicU64::new(5_000);
    /// Messages handed to the swarm by the outbox, for the backlog estimate.
    pub(crate) static RELEASED: AtomicU64 = AtomicU64::new(0);
    /// Candidate C1': the longest a keep-alive waits for its pairs to drain (`u64::MAX`: until the slot).
    pub(crate) static WAIT_CAP_MS: AtomicU64 = AtomicU64::new(u64::MAX);
    /// Candidates C5a (1) and C5b (2); 0 is off.
    pub(crate) static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    pub(crate) const MODE_C5A: u8 = 1;
    pub(crate) const MODE_C5B: u8 = 2;
    pub(crate) fn mode() -> u8 { MODE.load(std::sync::atomic::Ordering::Relaxed) }
    /// How many relay connection closes the pump has looked at.
    pub(crate) static CLOSES_SEEN: AtomicU64 = AtomicU64::new(0);
    /// When the outbox last released data (ms since the epoch).
    pub(crate) static LAST_RELEASE_MS: AtomicU64 = AtomicU64::new(0);
    /// Keep-alive rounds after a relay joined the mesh that are running now.
    pub(crate) static ROUND_ACTIVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
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

    fn enqueue(
        outboxes: &mut MakerOutboxes,
        now_ms: u64,
        pressure: bool,
        uuid: Uuid,
        topic: &str,
        message: OutboxMessage,
    ) -> Vec<Release> {
        outboxes
            .enqueue(now_ms, pressure, CEILING, PK, None, uuid, topic.to_owned(), message)
            .0
    }

    fn update_msg(uuid: Uuid, price: i64) -> OutboxMessage {
        OutboxMessage::Updated(updated(uuid, Some(price), None, None))
    }

    // T-MP2: a cancellation is released at once, with the budget exhausted and in degraded mode.
    #[test]
    fn cancellation_is_never_held_back() {
        let mut outboxes = MakerOutboxes::default();
        let mut now = 1_000_000;
        // Exhaust the burst of 100 and put the node under pressure to the floor.
        let uuids: Vec<Uuid> = (0..600).map(|_| Uuid::new_v4()).collect();
        for uuid in &uuids {
            enqueue(&mut outboxes, now, true, *uuid, "t", update_msg(*uuid, 1));
        }
        assert!(outboxes.pending_of(PK) > 0);
        for _ in 0..30 {
            now += 1_000;
            outboxes.tick(now, true, CEILING);
        }
        assert_eq!(outboxes.rate_of(PK, CEILING), 0.5);
        let pending_before = outboxes.pending_of(PK);
        assert!(pending_before > 0, "degraded mode keeps the pending messages");

        let victim = uuids[599];
        let released = enqueue(
            &mut outboxes,
            now,
            true,
            victim,
            "t",
            OutboxMessage::Cancelled(cancelled(victim)),
        );
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].message, OutboxMessage::Cancelled(cancelled(victim)));
        // The pending update of the order was discarded, nothing else changed.
        assert_eq!(outboxes.pending_of(PK), pending_before - 1);
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

    // T-MP4: adaptation with an injected clock.
    #[test]
    fn rate_halves_under_pressure_and_recovers_after_thirty_seconds() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        let uuid = Uuid::new_v4();
        // Create the outbox (an update that leaves at once).
        enqueue(&mut outboxes, t0, false, uuid, "t", update_msg(uuid, 1));
        let rate = |outboxes: &MakerOutboxes| outboxes.rate_of(PK, CEILING);
        assert_eq!(rate(&outboxes), 10.0);

        // Pressure from t0: halving at +5 s, +10 s, ...
        let steps = [
            (0, 10.0),
            (4_999, 10.0),
            (5_000, 5.0),
            (9_999, 5.0),
            (10_000, 2.5),
            (15_000, 1.25),
            (20_000, 0.625),
            (25_000, 0.5),
            (60_000, 0.5),
        ];
        for (offset, expected) in steps {
            outboxes.tick(t0 + offset, true, CEILING);
            assert_eq!(rate(&outboxes), expected, "at +{} ms", offset);
        }

        // Out of pressure from +60 s: nothing for 30 s, then +1/s every 5 s up to the ceiling.
        let t1 = t0 + 60_000;
        let steps = [
            (0, 0.5),
            (29_999, 0.5),
            (30_000, 1.5),
            (34_999, 1.5),
            (35_000, 2.5),
            (40_000, 3.5),
            (70_000, 9.5),
            (75_000, 10.0),
            (200_000, 10.0),
        ];
        for (offset, expected) in steps {
            outboxes.tick(t1 + offset, false, CEILING);
            assert_eq!(rate(&outboxes), expected, "recovery at +{} ms", offset);
        }
    }

    #[test]
    fn a_short_pressure_blip_does_not_lower_the_rate() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        let uuid = Uuid::new_v4();
        enqueue(&mut outboxes, t0, false, uuid, "t", update_msg(uuid, 1));
        outboxes.tick(t0 + 1_000, true, CEILING);
        outboxes.tick(t0 + 4_000, true, CEILING);
        outboxes.tick(t0 + 4_500, false, CEILING);
        outboxes.tick(t0 + 9_000, true, CEILING);
        outboxes.tick(t0 + 12_000, true, CEILING);
        assert_eq!(outboxes.rate_of(PK, CEILING), 10.0);
    }

    #[test]
    fn at_the_floor_under_pressure_nothing_is_released_and_the_flush_is_skipped() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        // Exhaust the burst, then leave 300 messages pending on one pair.
        for _ in 0..100 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, t0, true, uuid, "warm", update_msg(uuid, 1));
        }
        for _ in 0..300 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, t0, true, uuid, "pair-a", update_msg(uuid, 1));
        }
        for s in 1..=40 {
            outboxes.tick(t0 + s * 1_000, true, CEILING);
        }
        // The rate fell to the floor; some messages left on the way down, none leave now.
        assert_eq!(outboxes.rate_of(PK, CEILING), 0.5);
        let pending = outboxes.pending_of(PK);
        assert!(pending > 0);
        for s in 41..=100 {
            assert!(outboxes.tick(t0 + s * 1_000, true, CEILING).0.is_empty());
        }
        assert_eq!(outboxes.pending_of(PK), pending);
        let (flushed, _) = outboxes.flush_pair(t0 + 100_000, true, CEILING, PK, "pair-a");
        assert!(flushed.is_empty(), "the flush is skipped in degraded mode");
        // Out of pressure the flush works again, and releases the whole pair.
        let (flushed, _) = outboxes.flush_pair(t0 + 101_000, false, CEILING, PK, "pair-a");
        assert_eq!(flushed.len(), pending);
        assert_eq!(outboxes.pending_of(PK), 0);
    }

    // T-MP6 (part): the flush releases exactly the pending messages of the pair, oldest first, and
    // does not touch the budget.
    #[test]
    fn flush_releases_the_pair_oldest_first_without_the_budget() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        for _ in 0..100 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, now, false, uuid, "warm", update_msg(uuid, 1));
        }
        let ids: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
        for (i, uuid) in ids.iter().enumerate() {
            let topic = if i % 2 == 0 { "pair-a" } else { "pair-b" };
            enqueue(&mut outboxes, now, false, *uuid, topic, update_msg(*uuid, 1));
        }
        assert_eq!(outboxes.pending_of(PK), 6);
        let (flushed, _) = outboxes.flush_pair(now, false, CEILING, PK, "pair-a");
        let order: Vec<Uuid> = flushed.iter().map(|r| as_updated(r.message.clone()).uuid()).collect();
        assert_eq!(order, vec![ids[0], ids[2], ids[4]]);
        assert!(flushed.iter().all(|r| r.topic == "pair-a"));
        assert_eq!(outboxes.pending_of(PK), 3);
        // The budget was not charged: the bucket is still empty, so nothing else leaves yet.
        assert!(outboxes.tick(now, false, CEILING).0.is_empty());
    }

    #[test]
    fn log_lines_are_limited_to_one_per_minute_and_carry_the_current_state() {
        let mut outboxes = MakerOutboxes::default();
        let t0 = 1_000_000;
        let uuid = Uuid::new_v4();
        enqueue(&mut outboxes, t0, true, uuid, "t", update_msg(uuid, 1));
        let mut lines = Vec::new();
        for s in 1..=125 {
            let (_, logs) = outboxes.tick(t0 + s * 1_000, true, CEILING);
            for log in logs {
                lines.push((s, log));
            }
        }
        // Halved at +5 s (first line), the further halvings are folded into the next minute's line.
        assert_eq!(lines.len(), 2, "{:?}", lines);
        assert_eq!(lines[0].0, 5);
        assert_eq!(lines[0].1.rate, 5.0);
        assert!(!lines[0].1.degraded);
        assert_eq!(lines[1].0, 65);
        assert_eq!(lines[1].1.rate, 0.5);
        assert!(lines[1].1.degraded);
        assert_eq!(lines[1].1.pubkey, PK);
    }

    #[test]
    fn metrics_are_aggregates_over_pubkeys() {
        let mut outboxes = MakerOutboxes::default();
        let now = 1_000_000;
        assert_eq!(outboxes.metrics(CEILING), OutboxMetrics {
            pending: 0,
            rate: CEILING
        });
        for _ in 0..105 {
            let uuid = Uuid::new_v4();
            enqueue(&mut outboxes, now, false, uuid, "t", update_msg(uuid, 1));
        }
        let uuid = Uuid::new_v4();
        outboxes.enqueue(now, false, CEILING, "03bb", None, uuid, "t".into(), update_msg(uuid, 1));
        let metrics = outboxes.metrics(CEILING);
        assert_eq!(metrics.pending, 5);
        assert_eq!(metrics.rate, CEILING);
        // One pubkey under pressure for 5 s lowers the reported rate to the lowest.
        outboxes.tick(now, true, CEILING);
        outboxes.tick(now + 5_000, true, CEILING);
        assert_eq!(outboxes.metrics(CEILING).rate, 5.0);
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
