// Copyright 2020 Sigma Prime Pty Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Data structure for efficiently storing known back-off's when pruning peers.
use crate::topic::TopicHash;
use instant::Instant;
use libp2p_identity::PeerId;
use std::collections::{
    hash_map::{Entry, HashMap},
    HashSet,
};
use std::time::Duration;

#[derive(Copy, Clone)]
struct HeartbeatIndex(usize);

/// Stores backoffs in an efficient manner.
pub(crate) struct BackoffStorage {
    /// Stores backoffs and the index in backoffs_by_heartbeat per peer per topic.
    backoffs: HashMap<TopicHash, HashMap<PeerId, (Instant, HeartbeatIndex)>>,
    /// Stores peer topic pairs per heartbeat (this is cyclic the current index is
    /// heartbeat_index).
    backoffs_by_heartbeat: Vec<HashSet<(TopicHash, PeerId)>>,
    /// The index in the backoffs_by_heartbeat vector corresponding to the current heartbeat.
    heartbeat_index: HeartbeatIndex,
    /// The heartbeat interval duration from the config.
    heartbeat_interval: Duration,
    /// Backoff slack from the config.
    backoff_slack: u32,
}

impl BackoffStorage {
    /// Largest [`Duration`] that can still be added to `now` without overflowing the
    /// platform's [`Instant`] representation. Found by bisection because that
    /// representation (and therefore the bound) differs between native and wasm.
    fn saturating_backoff(now: Instant) -> Duration {
        let mut lo = Duration::ZERO;
        let mut hi = Duration::MAX;
        for _ in 0..128 {
            let mid = lo + (hi - lo) / 2;
            if mid == lo {
                break;
            }
            if now.checked_add(mid).is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn heartbeats(d: &Duration, heartbeat_interval: &Duration) -> usize {
        ((d.as_nanos() + heartbeat_interval.as_nanos() - 1) / heartbeat_interval.as_nanos())
            as usize
    }

    pub(crate) fn new(
        prune_backoff: &Duration,
        heartbeat_interval: Duration,
        backoff_slack: u32,
    ) -> BackoffStorage {
        // We add one additional slot for partial heartbeat
        let max_heartbeats =
            Self::heartbeats(prune_backoff, &heartbeat_interval) + backoff_slack as usize + 1;
        BackoffStorage {
            backoffs: HashMap::new(),
            backoffs_by_heartbeat: vec![HashSet::new(); max_heartbeats],
            heartbeat_index: HeartbeatIndex(0),
            heartbeat_interval,
            backoff_slack,
        }
    }

    /// Updates the backoff for a peer (if there is already a more restrictive backoff then this call
    /// doesn't change anything).
    pub(crate) fn update_backoff(&mut self, topic: &TopicHash, peer: &PeerId, time: Duration) {
        // SECURITY (GHSA-gc42-3jg7-rxr2): `time` can originate from a remote peer's
        // PRUNE backoff. `Instant::now() + time` panics on overflow, which is remotely
        // reachable. Saturate instead: a backoff we cannot represent is treated as
        // "effectively forever", which is what the peer asked for anyway. Callers
        // additionally clamp peer-supplied values (see `remove_peer_from_mesh`).
        let now = Instant::now();
        let instant = now
            .checked_add(time)
            .unwrap_or_else(|| now + Self::saturating_backoff(now));
        let insert_into_backoffs_by_heartbeat =
            |heartbeat_index: HeartbeatIndex,
             backoffs_by_heartbeat: &mut Vec<HashSet<_>>,
             heartbeat_interval,
             backoff_slack| {
                let pair = (topic.clone(), *peer);
                // SECURITY (GHSA-gc42-3jg7-rxr2): `heartbeats(&time, ..)` is derived
                // from a peer-supplied backoff, so this sum overflows `usize` on a
                // large value -- a panic in debug builds and on 32-bit targets, both
                // of which this workspace ships (armv7, wasm32). The result is taken
                // modulo the ring length anyway, so saturating simply parks the entry
                // in some bucket; `heartbeat()` still evicts it by comparing the real
                // stored `Instant`.
                let index = heartbeat_index
                    .0
                    .saturating_add(Self::heartbeats(&time, heartbeat_interval))
                    .saturating_add(backoff_slack as usize)
                    % backoffs_by_heartbeat.len();
                backoffs_by_heartbeat[index].insert(pair);
                HeartbeatIndex(index)
            };
        match self
            .backoffs
            .entry(topic.clone())
            .or_insert_with(HashMap::new)
            .entry(*peer)
        {
            Entry::Occupied(mut o) => {
                let (backoff, index) = o.get();
                if backoff < &instant {
                    let pair = (topic.clone(), *peer);
                    if let Some(s) = self.backoffs_by_heartbeat.get_mut(index.0) {
                        s.remove(&pair);
                    }
                    let index = insert_into_backoffs_by_heartbeat(
                        self.heartbeat_index,
                        &mut self.backoffs_by_heartbeat,
                        &self.heartbeat_interval,
                        self.backoff_slack,
                    );
                    o.insert((instant, index));
                }
            }
            Entry::Vacant(v) => {
                let index = insert_into_backoffs_by_heartbeat(
                    self.heartbeat_index,
                    &mut self.backoffs_by_heartbeat,
                    &self.heartbeat_interval,
                    self.backoff_slack,
                );
                v.insert((instant, index));
            }
        };
    }

    /// Checks if a given peer is backoffed for the given topic. This method respects the
    /// configured BACKOFF_SLACK and may return true even if the backup is already over.
    /// It is guaranteed to return false if the backoff is not over and eventually if enough time
    /// passed true if the backoff is over.
    ///
    /// This method should be used for deciding if we can already send a GRAFT to a previously
    /// backoffed peer.
    pub(crate) fn is_backoff_with_slack(&self, topic: &TopicHash, peer: &PeerId) -> bool {
        self.backoffs
            .get(topic)
            .map_or(false, |m| m.contains_key(peer))
    }

    pub(crate) fn get_backoff_time(&self, topic: &TopicHash, peer: &PeerId) -> Option<Instant> {
        Self::get_backoff_time_from_backoffs(&self.backoffs, topic, peer)
    }

    fn get_backoff_time_from_backoffs(
        backoffs: &HashMap<TopicHash, HashMap<PeerId, (Instant, HeartbeatIndex)>>,
        topic: &TopicHash,
        peer: &PeerId,
    ) -> Option<Instant> {
        backoffs
            .get(topic)
            .and_then(|m| m.get(peer).map(|(i, _)| *i))
    }

    /// Applies a heartbeat. That should be called regularly in intervals of length
    /// `heartbeat_interval`.
    pub(crate) fn heartbeat(&mut self) {
        // Clean up backoffs_by_heartbeat
        if let Some(s) = self.backoffs_by_heartbeat.get_mut(self.heartbeat_index.0) {
            let backoffs = &mut self.backoffs;
            let slack = self.heartbeat_interval * self.backoff_slack;
            let now = Instant::now();
            s.retain(|(topic, peer)| {
                let keep = match Self::get_backoff_time_from_backoffs(backoffs, topic, peer) {
                    // SECURITY (GHSA-xqmp-fxgv-xvq5): `backoff_time` may sit near the
                    // representable upper bound because of a peer-supplied PRUNE
                    // backoff; `backoff_time + slack` then panics with "overflow when
                    // adding duration to instant" on an ordinary heartbeat. If the sum
                    // is not representable the backoff is by definition still in the
                    // future, so keep the entry.
                    Some(backoff_time) => backoff_time
                        .checked_add(slack)
                        .map_or(true, |deadline| deadline > now),
                    None => false,
                };
                if !keep {
                    //remove from backoffs
                    if let Entry::Occupied(mut m) = backoffs.entry(topic.clone()) {
                        if m.get_mut().remove(peer).is_some() && m.get().is_empty() {
                            m.remove();
                        }
                    }
                }

                keep
            });
        }

        // Increase heartbeat index
        self.heartbeat_index =
            HeartbeatIndex((self.heartbeat_index.0 + 1) % self.backoffs_by_heartbeat.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> BackoffStorage {
        // Mirrors the gossipsub defaults: 60s prune backoff, 1s heartbeat, slack 1.
        BackoffStorage::new(&Duration::from_secs(60), Duration::from_secs(1), 1)
    }

    /// Regression test for GHSA-gc42-3jg7-rxr2 and GHSA-xqmp-fxgv-xvq5.
    ///
    /// A remote peer's PRUNE backoff reaches `update_backoff` as a `Duration`. Before
    /// the fix, a near-maximum value panicked either immediately (`Instant::now() +
    /// time`) or on the next heartbeat (`backoff_time + slack`), crashing the swarm
    /// state machine from an unauthenticated peer.
    #[test]
    fn extreme_backoff_does_not_panic() {
        let topic = TopicHash::from_raw("test-topic");
        let peer = PeerId::random();

        for time in [
            Duration::from_secs(u64::MAX),
            Duration::MAX,
            Duration::from_secs(MAX_PRUNE_BACKOFF_SECS_FOR_TEST),
        ] {
            let mut storage = storage();
            storage.update_backoff(&topic, &peer, time);
            // The entry must survive a heartbeat sweep rather than panic in it: an
            // unrepresentable deadline is still in the future.
            storage.heartbeat();
            assert!(storage.get_backoff_time(&topic, &peer).is_some());
        }
    }

    /// A second PRUNE with an extreme backoff must not panic either: that path takes
    /// the `Entry::Occupied` branch and compares against the stored instant.
    #[test]
    fn extreme_backoff_after_normal_backoff_does_not_panic() {
        let topic = TopicHash::from_raw("test-topic");
        let peer = PeerId::random();
        let mut storage = storage();

        storage.update_backoff(&topic, &peer, Duration::from_secs(60));
        storage.update_backoff(&topic, &peer, Duration::MAX);
        storage.heartbeat();
        assert!(storage.get_backoff_time(&topic, &peer).is_some());
    }

    /// `saturating_backoff` must land on a value that is actually addable, and adding
    /// one more second to it must not be.
    #[test]
    fn saturating_backoff_is_the_representable_maximum() {
        let now = Instant::now();
        let max = BackoffStorage::saturating_backoff(now);
        assert!(now.checked_add(max).is_some());
        assert!(now.checked_add(max + Duration::from_secs(1)).is_none());
    }

    const MAX_PRUNE_BACKOFF_SECS_FOR_TEST: u64 = 24 * 60 * 60;
}
