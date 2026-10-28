//! Per-connection send queue with a bound on the queued data RPCs.
//!
//! Reloaded change: CRD ch.28 §28.9B RQ1. The queue is first in, first out. RPCs that carry
//! published or forwarded messages ("data RPCs") are capped by count and encoded size; when a new
//! data RPC does not fit, the oldest queued data RPCs are dropped. All other RPCs ("control
//! RPCs") are never counted and never dropped. A connection that stays at the cap for a whole
//! period is reported as stuck so the behaviour can close it.
//!
//! Reloaded change: CRD ch.28 §28.9B RQ1 items 8 and 9. The queue also keeps a separate FIFO
//! *priority lane* that is written before everything else, is never dropped and has its own count
//! cap (a message past it reports the connection for closing), and it derives a *pressure* level
//! from the data queue depth for the application.
//!
//! The queue takes the current time as an argument so the stuck-peer logic is testable without
//! sleeping.

use crate::rpc_proto::proto;
use instant::Instant;
use quick_protobuf::MessageWrite;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Default cap on the queued data RPCs of one connection.
pub const DEFAULT_SEND_QUEUE_MAX_MESSAGES: usize = 2048;
/// Default cap on the encoded size of the queued data RPCs of one connection (4 MiB).
pub const DEFAULT_SEND_QUEUE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Default time a connection may stay at the cap before it is closed. It is also the minimum
/// interval between two drop summary log lines.
pub const DEFAULT_SEND_QUEUE_STUCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Default data queue depth at which the pressure signal is raised (CRD ch.28 §28.9B RQ1 item 8).
pub const DEFAULT_SEND_QUEUE_PRESSURE_RAISE: usize = 256;
/// Default data queue depth at or below which the pressure signal is cleared.
pub const DEFAULT_SEND_QUEUE_PRESSURE_CLEAR: usize = 64;
/// Default cap on the queued RPCs of the priority lane (CRD ch.28 §28.9B RQ1 item 9).
pub const DEFAULT_SEND_QUEUE_PRIORITY_MAX: usize = 256;

/// Default time the data queue must hold something before the queue signal is raised (CRD ch.32 §32.9B R-MP2
/// item 2).
pub const DEFAULT_SEND_QUEUE_SIGNAL_RAISE_AFTER: Duration = Duration::from_secs(2);
/// Default time the data queue must stay empty before the queue signal is cleared.
pub const DEFAULT_SEND_QUEUE_SIGNAL_CLEAR_AFTER: Duration = Duration::from_secs(5);

/// Limits of the per-connection send queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendQueueConfig {
    /// Maximum number of queued data RPCs.
    pub max_messages: usize,
    /// Maximum total encoded size of the queued data RPCs.
    pub max_bytes: usize,
    /// Time at the cap after which the connection is closed.
    pub stuck_timeout: Duration,
    /// Data queue depth at which the pressure signal is raised.
    pub pressure_raise: usize,
    /// Data queue depth at or below which the pressure signal is cleared (with no period at the
    /// cap running).
    pub pressure_clear: usize,
    /// Maximum number of queued priority RPCs. A priority RPC past it closes the connection.
    pub priority_max: usize,
    /// How long the data queue must hold at least one RPC, without becoming empty, before the queue signal is
    /// raised.
    pub signal_raise_after: Duration,
    /// How long the data queue must stay empty before the queue signal is cleared.
    pub signal_clear_after: Duration,
}

impl Default for SendQueueConfig {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_SEND_QUEUE_MAX_MESSAGES,
            max_bytes: DEFAULT_SEND_QUEUE_MAX_BYTES,
            stuck_timeout: DEFAULT_SEND_QUEUE_STUCK_TIMEOUT,
            pressure_raise: DEFAULT_SEND_QUEUE_PRESSURE_RAISE,
            pressure_clear: DEFAULT_SEND_QUEUE_PRESSURE_CLEAR,
            priority_max: DEFAULT_SEND_QUEUE_PRIORITY_MAX,
            signal_raise_after: DEFAULT_SEND_QUEUE_SIGNAL_RAISE_AFTER,
            signal_clear_after: DEFAULT_SEND_QUEUE_SIGNAL_CLEAR_AFTER,
        }
    }
}

/// Counters of one connection's send queue, readable from outside the handler for metrics.
#[derive(Debug, Default)]
pub struct SendQueueStats {
    queued: AtomicUsize,
    dropped: AtomicU64,
}

impl SendQueueStats {
    /// Number of queued data RPCs.
    pub fn queued(&self) -> usize { self.queued.load(Ordering::Relaxed) }

    /// Number of data RPCs dropped on this connection since it was established.
    pub fn dropped(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
}

/// A snapshot of the send queues of all connections, for metrics (CRD ch.28 §28.9B RQ1 item 8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendQueueSnapshot {
    /// Queued data RPCs, summed over connections.
    pub queued: usize,
    /// The largest single connection's count of queued data RPCs.
    pub max_per_connection: usize,
    /// Data RPCs dropped since start, summed over all connections, closed ones included.
    pub dropped: u64,
}

/// What the caller should log after a push that dropped data RPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropLog {
    /// The first drop on this connection. Carries the data queue state when the cap was hit.
    First { queued_messages: usize, queued_bytes: usize },
    /// A summary of the drops since the previous log line.
    Summary { dropped: u64 },
}

/// A connection that stayed at the cap for the whole period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StuckReport {
    /// How long the connection has been at the cap.
    pub at_cap: Duration,
    /// Number of data RPCs dropped on this connection.
    pub total_dropped: u64,
}

struct Entry {
    rpc: proto::RPC,
    /// `Some(encoded size)` for a data RPC, `None` for a control RPC.
    data_size: Option<usize>,
}

pub struct SendQueue {
    config: SendQueueConfig,
    queue: VecDeque<Entry>,
    /// The priority lane: written before `queue`, never dropped (CRD ch.28 §28.9B RQ1 item 9).
    priority: VecDeque<proto::RPC>,
    /// Set when a priority RPC could not be queued because the lane was full.
    priority_overflow: bool,
    /// Current pressure level and the level last handed out by `take_pressure_change`.
    pressure: bool,
    pressure_reported: bool,
    /// The queue signal (CRD ch.32 §32.9B R-MP2 item 2): the data queue has held something for the raise time, or
    /// the pressure level is raised. It clears once the queue has been empty for the clear time and the pressure
    /// level is cleared.
    signal: bool,
    signal_reported: bool,
    nonempty_since: Option<Instant>,
    empty_since: Option<Instant>,
    stats: Arc<SendQueueStats>,
    data_count: usize,
    data_bytes: usize,
    /// Start of the current period at the cap, if any.
    period_start: Option<Instant>,
    /// Whether the current period has already been reported as stuck.
    stuck_reported: bool,
    total_dropped: u64,
    dropped_since_log: u64,
    last_log: Option<Instant>,
}

impl SendQueue {
    pub fn new(config: SendQueueConfig) -> Self {
        Self {
            config,
            queue: VecDeque::new(),
            priority: VecDeque::new(),
            priority_overflow: false,
            pressure: false,
            pressure_reported: false,
            signal: false,
            signal_reported: false,
            nonempty_since: None,
            empty_since: None,
            stats: Arc::new(SendQueueStats::default()),
            data_count: 0,
            data_bytes: 0,
            period_start: None,
            stuck_reported: false,
            total_dropped: 0,
            dropped_since_log: 0,
            last_log: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty() && self.priority.is_empty()
    }

    /// The counters of this queue, shared with the behaviour for metrics.
    pub fn stats(&self) -> Arc<SendQueueStats> {
        self.stats.clone()
    }

    /// Number of queued priority RPCs.
    pub fn priority_len(&self) -> usize {
        self.priority.len()
    }

    /// Number of data RPCs dropped on this connection.
    pub fn total_dropped(&self) -> u64 {
        self.total_dropped
    }

    /// Number of queued data RPCs.
    pub fn data_count(&self) -> usize {
        self.data_count
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Total encoded size of the queued data RPCs.
    #[cfg(test)]
    pub fn data_bytes(&self) -> usize {
        self.data_bytes
    }

    /// Queues an RPC. A data RPC that does not fit evicts the oldest queued data RPCs first.
    /// Returns what to log, if anything.
    pub fn push(&mut self, rpc: proto::RPC, now: Instant) -> Option<DropLog> {
        let log = self.push_inner(rpc, now);
        self.after_change(now);
        log
    }

    /// Queues an RPC in the priority lane. The lane is first in, first out, is written before
    /// every other RPC, does not count toward the data cap and is never dropped. When the lane
    /// already holds `priority_max` RPCs the RPC is not queued and `false` is returned; the
    /// connection is then reported for closing through `take_priority_overflow`.
    pub fn push_priority(&mut self, rpc: proto::RPC) -> bool {
        if self.priority.len() >= self.config.priority_max {
            self.priority_overflow = true;
            return false;
        }
        self.priority.push_back(rpc);
        true
    }

    /// Reports once that a priority RPC found the lane full.
    pub fn take_priority_overflow(&mut self) -> bool {
        std::mem::take(&mut self.priority_overflow)
    }

    /// The pressure level, if it changed since the last call (CRD ch.28 §28.9B RQ1 item 8): raised
    /// when the data queue reaches `pressure_raise` RPCs or a period at the cap starts, cleared
    /// when it has drained to `pressure_clear` RPCs or fewer and no period is running. A change
    /// that was undone before this call is not reported.
    pub fn take_pressure_change(&mut self) -> Option<bool> {
        if self.pressure == self.pressure_reported {
            return None;
        }
        self.pressure_reported = self.pressure;
        Some(self.pressure)
    }

    /// The queue signal change, if any, since the last call; each change is handed out once.
    pub fn take_signal_change(&mut self) -> Option<bool> {
        if self.signal == self.signal_reported {
            return None;
        }
        self.signal_reported = self.signal;
        Some(self.signal)
    }

    /// Re-evaluates the queue signal at `now`.
    pub fn update_signal(&mut self, now: Instant) {
        if !self.signal {
            if self.pressure
                || self
                    .nonempty_since
                    .map_or(false, |since| now.saturating_duration_since(since) >= self.config.signal_raise_after)
            {
                self.signal = true;
            }
        } else if !self.pressure
            && self
                .empty_since
                .map_or(false, |since| now.saturating_duration_since(since) >= self.config.signal_clear_after)
        {
            self.signal = false;
        }
    }

    /// The time at which the queue signal changes by itself, if no RPC is queued or taken before.
    pub fn signal_deadline(&self) -> Option<Instant> {
        if !self.signal {
            self.nonempty_since?.checked_add(self.config.signal_raise_after)
        } else if !self.pressure {
            self.empty_since?.checked_add(self.config.signal_clear_after)
        } else {
            None
        }
    }

    fn after_change(&mut self, now: Instant) {
        if !self.pressure {
            if self.data_count >= self.config.pressure_raise || self.period_start.is_some() {
                self.pressure = true;
            }
        } else if self.data_count <= self.config.pressure_clear && self.period_start.is_none() {
            self.pressure = false;
        }
        self.stats.queued.store(self.data_count, Ordering::Relaxed);
        self.stats.dropped.store(self.total_dropped, Ordering::Relaxed);
        if self.data_count > 0 {
            self.empty_since = None;
            self.nonempty_since.get_or_insert(now);
        } else {
            self.nonempty_since = None;
            self.empty_since.get_or_insert(now);
        }
        self.update_signal(now);
    }

    fn push_inner(&mut self, rpc: proto::RPC, now: Instant) -> Option<DropLog> {
        if rpc.publish.is_empty() {
            self.queue.push_back(Entry { rpc, data_size: None });
            return None;
        }

        let size = rpc.get_size();
        let queued_before = (self.data_count, self.data_bytes);
        let mut dropped = 0u64;
        // A lone RPC larger than the byte cap evicts everything else and is queued anyway.
        while self.data_count > 0
            && (self.data_count.saturating_add(1) > self.config.max_messages
                || self.data_bytes.saturating_add(size) > self.config.max_bytes)
        {
            self.evict_oldest_data();
            dropped += 1;
        }
        self.queue.push_back(Entry { rpc, data_size: Some(size) });
        self.data_count += 1;
        self.data_bytes = self.data_bytes.saturating_add(size);

        if dropped == 0 {
            return None;
        }
        if self.period_start.is_none() {
            self.period_start = Some(now);
            self.stuck_reported = false;
        }
        self.total_dropped += dropped;
        self.dropped_since_log += dropped;
        match self.last_log {
            None => {
                self.last_log = Some(now);
                self.dropped_since_log = 0;
                Some(DropLog::First {
                    queued_messages: queued_before.0,
                    queued_bytes: queued_before.1,
                })
            },
            Some(last) if now.saturating_duration_since(last) >= self.config.stuck_timeout => {
                self.last_log = Some(now);
                Some(DropLog::Summary {
                    dropped: std::mem::take(&mut self.dropped_since_log),
                })
            },
            Some(_) => None,
        }
    }

    /// Removes the next RPC to write: the oldest priority RPC if there is one, else the oldest
    /// queued RPC, control or data.
    pub fn pop(&mut self) -> Option<proto::RPC> { self.pop_at(Instant::now()) }

    /// Like [`Self::pop`], with the current time given.
    pub fn pop_at(&mut self, now: Instant) -> Option<proto::RPC> {
        if let Some(rpc) = self.priority.pop_front() {
            return Some(rpc);
        }
        let entry = self.queue.pop_front()?;
        if let Some(size) = entry.data_size {
            self.data_count -= 1;
            self.data_bytes -= size;
            // The period ends once the data queue is at or below half of both limits.
            if self.period_start.is_some()
                && self.data_count <= self.config.max_messages / 2
                && self.data_bytes <= self.config.max_bytes / 2
            {
                self.period_start = None;
            }
        }
        if self.queue.is_empty() && self.queue.capacity() > 16 {
            self.queue.shrink_to_fit();
        }
        self.after_change(now);
        Some(entry.rpc)
    }

    /// The time at which the current period at the cap, if any, becomes a stuck connection.
    /// `None` when no period is running or it was already reported.
    pub fn stuck_deadline(&self) -> Option<Instant> {
        if self.stuck_reported {
            return None;
        }
        self.period_start?.checked_add(self.config.stuck_timeout)
    }

    /// Reports the connection as stuck, once, if it has been at the cap for the whole period.
    pub fn take_stuck(&mut self, now: Instant) -> Option<StuckReport> {
        let deadline = self.stuck_deadline()?;
        if now < deadline {
            return None;
        }
        self.stuck_reported = true;
        Some(StuckReport {
            at_cap: now.saturating_duration_since(self.period_start?),
            total_dropped: self.total_dropped,
        })
    }

    fn evict_oldest_data(&mut self) {
        let Some(pos) = self.queue.iter().position(|e| e.data_size.is_some()) else {
            return;
        };
        if let Some(Entry {
            data_size: Some(size),
            ..
        }) = self.queue.remove(pos)
        {
            self.data_count -= 1;
            self.data_bytes -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_proto::proto;

    fn data_rpc(tag: u8, payload: usize) -> proto::RPC {
        proto::RPC {
            subscriptions: vec![],
            publish: vec![proto::Message {
                from: None,
                data: Some(vec![tag; payload]),
                seqno: None,
                topic: "t".into(),
                signature: None,
                key: None,
            }],
            control: None,
        }
    }

    fn control_rpc(topic: &str) -> proto::RPC {
        proto::RPC {
            subscriptions: vec![proto::SubOpts {
                subscribe: Some(true),
                topic_id: Some(topic.into()),
            }],
            publish: vec![],
            control: None,
        }
    }

    fn tag_of(rpc: &proto::RPC) -> u8 {
        rpc.publish[0].data.as_ref().unwrap()[0]
    }

    fn queue(max_messages: usize, max_bytes: usize, stuck_secs: u64) -> SendQueue {
        SendQueue::new(SendQueueConfig {
            max_messages,
            max_bytes,
            stuck_timeout: Duration::from_secs(stuck_secs),
            ..SendQueueConfig::default()
        })
    }

    fn drain_tags(q: &mut SendQueue) -> Vec<u8> {
        let mut tags = vec![];
        while let Some(rpc) = q.pop() {
            if !rpc.publish.is_empty() {
                tags.push(tag_of(&rpc));
            }
        }
        tags
    }

    #[test]
    fn fifo_order() {
        let mut q = queue(100, 1 << 20, 60);
        let now = Instant::now();
        for i in 0..10u8 {
            assert_eq!(q.push(data_rpc(i, 10), now), None);
        }
        assert_eq!(drain_tags(&mut q), (0..10u8).collect::<Vec<_>>());
        assert!(q.is_empty());
        assert_eq!((q.data_count(), q.data_bytes()), (0, 0));
    }

    #[test]
    fn count_cap_drops_oldest() {
        let mut q = queue(4, 1 << 20, 60);
        let now = Instant::now();
        for i in 0..4u8 {
            assert_eq!(q.push(data_rpc(i, 10), now), None);
        }
        assert_eq!(q.data_count(), 4);
        // The fifth and sixth evict tags 0 and 1.
        q.push(data_rpc(4, 10), now);
        q.push(data_rpc(5, 10), now);
        assert_eq!(q.data_count(), 4);
        assert_eq!(q.total_dropped(), 2);
        assert_eq!(drain_tags(&mut q), vec![2, 3, 4, 5]);
    }

    #[test]
    fn byte_cap_drops_oldest_until_it_fits() {
        let one = data_rpc(0, 1000).get_size();
        // Room for three 1000-byte RPCs but not four.
        let mut q = queue(100, one * 3 + one / 2, 60);
        let now = Instant::now();
        for i in 0..3u8 {
            q.push(data_rpc(i, 1000), now);
        }
        assert_eq!(q.data_bytes(), one * 3);
        // A double-size RPC needs two slots: it evicts the two oldest.
        q.push(data_rpc(9, 2000), now);
        assert_eq!(q.total_dropped(), 2);
        assert!(q.data_bytes() <= one * 3 + one / 2);
        assert_eq!(drain_tags(&mut q), vec![2, 9]);
    }

    #[test]
    fn lone_oversized_rpc_is_queued_after_evicting_the_rest() {
        let mut q = queue(100, 500, 60);
        let now = Instant::now();
        q.push(data_rpc(1, 100), now);
        q.push(data_rpc(2, 100), now);
        q.push(data_rpc(3, 5000), now);
        assert_eq!(q.data_count(), 1);
        assert_eq!(q.total_dropped(), 2);
        // The next push evicts the oversized RPC; nothing panics.
        q.push(data_rpc(4, 100), now);
        assert_eq!(drain_tags(&mut q), vec![4]);
    }

    #[test]
    fn control_rpcs_are_not_counted_and_never_evicted() {
        let mut q = queue(2, 1 << 20, 60);
        let now = Instant::now();
        q.push(control_rpc("a"), now);
        q.push(data_rpc(1, 10), now);
        q.push(control_rpc("b"), now);
        q.push(data_rpc(2, 10), now);
        assert_eq!(q.data_count(), 2);
        // At the cap: more data evicts data only, in between the control RPCs.
        q.push(data_rpc(3, 10), now);
        q.push(control_rpc("c"), now);
        q.push(data_rpc(4, 10), now);
        assert_eq!(q.data_count(), 2);
        assert_eq!(q.len(), 5);
        let mut order = vec![];
        while let Some(rpc) = q.pop() {
            order.push(if rpc.publish.is_empty() {
                rpc.subscriptions[0].topic_id.clone().unwrap()
            } else {
                format!("d{}", tag_of(&rpc))
            });
        }
        assert_eq!(order, vec!["a", "b", "d3", "c", "d4"]);
    }

    #[test]
    fn period_starts_with_first_drop_and_ends_at_half_limits() {
        let mut q = queue(8, 1 << 20, 60);
        let t0 = Instant::now();
        for i in 0..8u8 {
            q.push(data_rpc(i, 10), t0);
        }
        // At the cap but nothing dropped yet: no period.
        assert_eq!(q.stuck_deadline(), None);
        q.push(data_rpc(8, 10), t0);
        assert_eq!(q.stuck_deadline(), Some(t0 + Duration::from_secs(60)));
        // Draining to 5 (above half = 4) keeps the period.
        for _ in 0..3 {
            q.pop();
        }
        assert_eq!(q.data_count(), 5);
        assert!(q.stuck_deadline().is_some());
        // Draining to 4 (half) ends it.
        q.pop();
        assert_eq!(q.data_count(), 4);
        assert_eq!(q.stuck_deadline(), None);
        // Filling up again and dropping again starts a new period at the new time.
        let t1 = t0 + Duration::from_secs(10);
        for i in 0..5u8 {
            q.push(data_rpc(i, 10), t1);
        }
        assert_eq!(q.stuck_deadline(), Some(t1 + Duration::from_secs(60)));
    }

    #[test]
    fn byte_half_limit_also_counts_for_period_end() {
        let one = data_rpc(0, 1000).get_size();
        let mut q = queue(1000, one * 4, 60);
        let t0 = Instant::now();
        for i in 0..5u8 {
            q.push(data_rpc(i, 1000), t0);
        }
        assert!(q.stuck_deadline().is_some());
        q.pop(); // 3 left, 3 * one > 2 * one
        assert!(q.stuck_deadline().is_some());
        q.pop(); // 2 left, 2 * one <= half
        assert_eq!(q.stuck_deadline(), None);
    }

    #[test]
    fn stuck_after_full_period_and_reported_once() {
        let mut q = queue(2, 1 << 20, 60);
        let t0 = Instant::now();
        for i in 0..3u8 {
            q.push(data_rpc(i, 10), t0);
        }
        assert_eq!(q.take_stuck(t0 + Duration::from_secs(59)), None);
        let report = q.take_stuck(t0 + Duration::from_secs(60)).unwrap();
        assert_eq!(report.at_cap, Duration::from_secs(60));
        assert_eq!(report.total_dropped, 1);
        assert_eq!(q.take_stuck(t0 + Duration::from_secs(90)), None);
    }

    #[test]
    fn slow_reader_freeing_one_slot_at_a_time_stays_stuck() {
        let mut q = queue(8, 1 << 20, 60);
        let t0 = Instant::now();
        for i in 0..9u8 {
            q.push(data_rpc(i, 10), t0);
        }
        // Every 10 s the peer takes one RPC and one new one arrives.
        for step in 1..=5u64 {
            let t = t0 + Duration::from_secs(step * 10);
            q.pop();
            q.push(data_rpc(100 + step as u8, 10), t);
            assert_eq!(q.data_count(), 8);
        }
        assert!(q.take_stuck(t0 + Duration::from_secs(60)).is_some());
    }

    #[test]
    fn peer_draining_in_time_is_not_stuck() {
        let mut q = queue(8, 1 << 20, 60);
        let t0 = Instant::now();
        for i in 0..9u8 {
            q.push(data_rpc(i, 10), t0);
        }
        while q.data_count() > 4 {
            q.pop();
        }
        assert_eq!(q.take_stuck(t0 + Duration::from_secs(120)), None);
    }

    #[test]
    fn drop_log_is_rate_limited() {
        let mut q = queue(1, 1 << 20, 60);
        let t0 = Instant::now();
        assert_eq!(q.push(data_rpc(0, 10), t0), None);
        let size = data_rpc(0, 10).get_size();
        // First drop: one warning with the state when the cap was hit.
        assert_eq!(
            q.push(data_rpc(1, 10), t0),
            Some(DropLog::First {
                queued_messages: 1,
                queued_bytes: size
            })
        );
        // Further drops inside the interval log nothing.
        for s in 1..60u64 {
            assert_eq!(q.push(data_rpc(2, 10), t0 + Duration::from_secs(s)), None);
        }
        // The first drop at or after the interval logs one summary with the count since the
        // last line.
        assert_eq!(
            q.push(data_rpc(3, 10), t0 + Duration::from_secs(60)),
            Some(DropLog::Summary { dropped: 60 })
        );
        assert_eq!(q.push(data_rpc(4, 10), t0 + Duration::from_secs(61)), None);
        assert_eq!(
            q.push(data_rpc(5, 10), t0 + Duration::from_secs(120)),
            Some(DropLog::Summary { dropped: 2 })
        );
        assert_eq!(q.total_dropped(), 63);
    }

    #[test]
    fn huge_timeout_does_not_overflow() {
        let mut q = SendQueue::new(SendQueueConfig {
            max_messages: 1,
            max_bytes: 1 << 20,
            stuck_timeout: Duration::MAX,
            ..SendQueueConfig::default()
        });
        let now = Instant::now();
        q.push(data_rpc(0, 10), now);
        q.push(data_rpc(1, 10), now);
        assert_eq!(q.stuck_deadline(), None);
        assert_eq!(q.take_stuck(now), None);
    }

    fn pressure_queue(raise: usize, clear: usize, priority_max: usize) -> SendQueue {
        SendQueue::new(SendQueueConfig {
            max_messages: 1000,
            max_bytes: 1 << 20,
            stuck_timeout: Duration::from_secs(60),
            pressure_raise: raise,
            pressure_clear: clear,
            priority_max,
            ..SendQueueConfig::default()
        })
    }

    #[test]
    fn pressure_is_raised_at_the_threshold_and_cleared_at_the_low_mark_once_each() {
        let mut q = pressure_queue(8, 2, 4);
        let now = Instant::now();
        for i in 0..7u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert_eq!(q.take_pressure_change(), None);
        q.push(data_rpc(7, 10), now);
        assert_eq!(q.take_pressure_change(), Some(true));
        // Reported once, not on every later push.
        q.push(data_rpc(8, 10), now);
        assert_eq!(q.take_pressure_change(), None);
        // Draining to just above the low mark keeps it raised.
        while q.data_count() > 3 {
            q.pop();
        }
        assert_eq!(q.take_pressure_change(), None);
        q.pop();
        assert_eq!(q.data_count(), 2);
        assert_eq!(q.take_pressure_change(), Some(false));
        assert_eq!(q.take_pressure_change(), None);
    }

    #[test]
    fn a_period_at_the_cap_raises_pressure_and_holds_it_until_the_period_ends() {
        // Cap 8, pressure thresholds above and below the period behaviour.
        let mut q = SendQueue::new(SendQueueConfig {
            max_messages: 8,
            max_bytes: 1 << 20,
            stuck_timeout: Duration::from_secs(60),
            pressure_raise: 100,
            pressure_clear: 1,
            priority_max: 4,
            ..SendQueueConfig::default()
        });
        let now = Instant::now();
        for i in 0..8u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert_eq!(q.take_pressure_change(), None);
        // The first drop starts the period: the signal is raised although 8 < 100.
        q.push(data_rpc(8, 10), now);
        assert_eq!(q.take_pressure_change(), Some(true));
        // Draining to 1 ends the period (<= half) and is at the low mark: cleared.
        while q.data_count() > 1 {
            q.pop();
        }
        assert_eq!(q.take_pressure_change(), Some(false));
    }

    #[test]
    fn pressure_stays_raised_below_the_low_mark_while_the_period_runs() {
        let mut q = SendQueue::new(SendQueueConfig {
            max_messages: 8,
            max_bytes: 1 << 20,
            stuck_timeout: Duration::from_secs(60),
            pressure_raise: 100,
            pressure_clear: 6,
            priority_max: 4,
            ..SendQueueConfig::default()
        });
        let now = Instant::now();
        for i in 0..9u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert_eq!(q.take_pressure_change(), Some(true));
        // 5 is at or below the low mark, but the period (ends at 4) still runs.
        while q.data_count() > 5 {
            q.pop();
        }
        assert_eq!(q.take_pressure_change(), None);
        q.pop();
        assert_eq!(q.data_count(), 4);
        assert_eq!(q.take_pressure_change(), Some(false));
    }

    #[test]
    fn stats_follow_the_queue() {
        let mut q = queue(3, 1 << 20, 60);
        let stats = q.stats();
        let now = Instant::now();
        for i in 0..5u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert_eq!((stats.queued(), stats.dropped()), (3, 2));
        q.pop();
        assert_eq!((stats.queued(), stats.dropped()), (2, 2));
    }

    #[test]
    fn priority_lane_is_written_first_and_in_order() {
        let mut q = pressure_queue(256, 64, 4);
        let now = Instant::now();
        q.push(data_rpc(1, 10), now);
        q.push(control_rpc("c"), now);
        assert!(q.push_priority(data_rpc(10, 10)));
        q.push(data_rpc(2, 10), now);
        assert!(q.push_priority(data_rpc(11, 10)));
        assert!(!q.is_empty());
        assert_eq!(q.priority_len(), 2);
        let mut order = vec![];
        while let Some(rpc) = q.pop() {
            order.push(if rpc.publish.is_empty() {
                0
            } else {
                tag_of(&rpc)
            });
        }
        assert_eq!(order, vec![10, 11, 1, 0, 2]);
    }

    #[test]
    fn priority_rpcs_are_never_dropped_and_not_counted() {
        let mut q = queue(2, 1 << 20, 60);
        let now = Instant::now();
        for i in 0..2u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert!(q.push_priority(data_rpc(50, 10)));
        assert_eq!(q.data_count(), 2);
        // Dropping the oldest data does not touch the priority lane.
        for i in 2..6u8 {
            q.push(data_rpc(i, 10), now);
        }
        assert_eq!(q.priority_len(), 1);
        assert_eq!(q.total_dropped(), 4);
        assert_eq!(drain_tags(&mut q), vec![50, 4, 5]);
    }

    #[test]
    fn the_priority_lane_past_its_cap_reports_overflow_once() {
        let mut q = pressure_queue(256, 64, 3);
        for i in 0..3u8 {
            assert!(q.push_priority(data_rpc(i, 10)));
        }
        assert!(!q.take_priority_overflow());
        assert!(!q.push_priority(data_rpc(3, 10)));
        assert!(q.take_priority_overflow());
        assert!(!q.take_priority_overflow());
        // Nothing was lost from the lane.
        assert_eq!(drain_tags(&mut q), vec![0, 1, 2]);
    }

    #[test]
    fn signal_is_raised_after_two_seconds_of_a_non_empty_queue_and_not_before() {
        let mut q = queue(100, 1 << 20, 60);
        let t0 = Instant::now();
        q.push(data_rpc(1, 10), t0);
        assert_eq!(q.signal_deadline(), Some(t0 + Duration::from_secs(2)));
        q.update_signal(t0 + Duration::from_millis(1900));
        assert_eq!(q.take_signal_change(), None);
        q.update_signal(t0 + Duration::from_secs(2));
        assert_eq!(q.take_signal_change(), Some(true));
        // Reported once.
        q.update_signal(t0 + Duration::from_secs(3));
        assert_eq!(q.take_signal_change(), None);
    }

    #[test]
    fn a_queue_that_empties_within_two_seconds_raises_no_signal() {
        let mut q = queue(100, 1 << 20, 60);
        let t0 = Instant::now();
        q.push(data_rpc(1, 10), t0);
        q.pop_at(t0 + Duration::from_millis(1500));
        // Filling again restarts the two seconds.
        q.push(data_rpc(2, 10), t0 + Duration::from_secs(2));
        q.update_signal(t0 + Duration::from_secs(3));
        q.update_signal(t0 + Duration::from_millis(3900));
        assert_eq!(q.take_signal_change(), None);
        q.update_signal(t0 + Duration::from_secs(4));
        assert_eq!(q.take_signal_change(), Some(true));
    }

    #[test]
    fn signal_clears_after_five_seconds_of_an_empty_queue() {
        let mut q = queue(100, 1 << 20, 60);
        let t0 = Instant::now();
        q.push(data_rpc(1, 10), t0);
        q.update_signal(t0 + Duration::from_secs(2));
        assert_eq!(q.take_signal_change(), Some(true));
        let t1 = t0 + Duration::from_secs(10);
        q.pop_at(t1);
        assert_eq!(q.signal_deadline(), Some(t1 + Duration::from_secs(5)));
        q.update_signal(t1 + Duration::from_millis(4900));
        assert_eq!(q.take_signal_change(), None);
        q.update_signal(t1 + Duration::from_secs(5));
        assert_eq!(q.take_signal_change(), Some(false));
        // A queue that gets something again starts over.
        q.push(data_rpc(2, 10), t1 + Duration::from_secs(6));
        assert_eq!(q.signal_deadline(), Some(t1 + Duration::from_secs(8)));
    }

    #[test]
    fn a_raised_pressure_level_raises_the_signal_and_holds_it() {
        let mut q = pressure_queue(3, 1, 4);
        let t0 = Instant::now();
        for i in 0..3u8 {
            q.push(data_rpc(i, 10), t0);
        }
        // The pressure level (3 queued) raises the signal at once.
        assert_eq!(q.take_signal_change(), Some(true));
        // Empty, but the pressure level is cleared only at 1 or fewer: it is, and 5 s must pass.
        let t1 = t0 + Duration::from_secs(1);
        while q.pop_at(t1).is_some() {}
        q.update_signal(t1 + Duration::from_secs(4));
        assert_eq!(q.take_signal_change(), None);
        q.update_signal(t1 + Duration::from_secs(5));
        assert_eq!(q.take_signal_change(), Some(false));
    }
}
