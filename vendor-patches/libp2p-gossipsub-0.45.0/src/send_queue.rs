//! Per-connection send queue with a bound on the queued data RPCs.
//!
//! Reloaded change: CRD ch.28 §28.9B RQ1. The queue is first in, first out. RPCs that carry
//! published or forwarded messages ("data RPCs") are capped by count and encoded size; when a new
//! data RPC does not fit, the oldest queued data RPCs are dropped. All other RPCs ("control
//! RPCs") are never counted and never dropped. A connection that stays at the cap for a whole
//! period is reported as stuck so the behaviour can close it.
//!
//! The queue takes the current time as an argument so the stuck-peer logic is testable without
//! sleeping.

use crate::rpc_proto::proto;
use instant::Instant;
use quick_protobuf::MessageWrite;
use std::{collections::VecDeque, time::Duration};

/// Default cap on the queued data RPCs of one connection.
pub const DEFAULT_SEND_QUEUE_MAX_MESSAGES: usize = 2048;
/// Default cap on the encoded size of the queued data RPCs of one connection (4 MiB).
pub const DEFAULT_SEND_QUEUE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Default time a connection may stay at the cap before it is closed. It is also the minimum
/// interval between two drop summary log lines.
pub const DEFAULT_SEND_QUEUE_STUCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Limits of the per-connection send queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendQueueConfig {
    /// Maximum number of queued data RPCs.
    pub max_messages: usize,
    /// Maximum total encoded size of the queued data RPCs.
    pub max_bytes: usize,
    /// Time at the cap after which the connection is closed.
    pub stuck_timeout: Duration,
}

impl Default for SendQueueConfig {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_SEND_QUEUE_MAX_MESSAGES,
            max_bytes: DEFAULT_SEND_QUEUE_MAX_BYTES,
            stuck_timeout: DEFAULT_SEND_QUEUE_STUCK_TIMEOUT,
        }
    }
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
        self.queue.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Number of queued data RPCs.
    #[cfg(test)]
    pub fn data_count(&self) -> usize {
        self.data_count
    }

    /// Total encoded size of the queued data RPCs.
    #[cfg(test)]
    pub fn data_bytes(&self) -> usize {
        self.data_bytes
    }

    #[cfg(test)]
    pub fn total_dropped(&self) -> u64 {
        self.total_dropped
    }

    /// Queues an RPC. A data RPC that does not fit evicts the oldest queued data RPCs first.
    /// Returns what to log, if anything.
    pub fn push(&mut self, rpc: proto::RPC, now: Instant) -> Option<DropLog> {
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

    /// Removes the oldest RPC, control or data.
    pub fn pop(&mut self) -> Option<proto::RPC> {
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
        });
        let now = Instant::now();
        q.push(data_rpc(0, 10), now);
        q.push(data_rpc(1, 10), now);
        assert_eq!(q.stuck_deadline(), None);
        assert_eq!(q.take_stuck(now), None);
    }
}
