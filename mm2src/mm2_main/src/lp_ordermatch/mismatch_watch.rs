//! The watch behind the DEBUG tail of the trigger (b) line of CRD ch.32 R-Q5d (logging is left to the
//! implementation, R36).
//!
//! When trigger (b) finds a diverged pair and is about to ask another relay, the line says which candidate rule
//! would have skipped that pair:
//!
//! * (a) a newer keep-alive of the same pubkey named the pair with a root that differs from the one of the
//!   triggering keep-alive and equals the root this node holds at the check;
//! * (b) after the triggering keep-alive this node handled a gossip order message of the same pubkey for the pair
//!   (an order applied from a synchronization answer is not one);
//! * (d) at the end of some trie-worker group applied after the triggering keep-alive was handled, this node held
//!   exactly the root the triggering keep-alive advertised for the pair.
//!
//! The watch only observes: what the node sends, when and to whom does not depend on it. There is one entry per
//! watched keep-alive request. It is made only while DEBUG logging is on and the request to another relay is
//! switched on, it ends with its [`WatchGuard`], and entries that were forgotten are dropped by a backstop (30 s,
//! 1,024 entries). Every hook is a no-op when the table is empty, with one atomic read.
//!
//! The watch lock is a leaf: it is taken last and released first, never held across an await, and never held
//! while the order book lock or the trie store lock is taken.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// Entries older than this are dropped when a new one starts.
const MAX_ENTRY_AGE_MS: u64 = 30_000;
/// The most entries kept; the oldest are dropped when a new one starts.
const MAX_ENTRIES: usize = 1024;

/// What the watch saw for one requested pair.
struct PairWatch {
    /// The root the triggering keep-alive advertised for the pair.
    trigger_root: H64,
    /// The root of the newest later keep-alive that named the pair.
    newest_advertised: Option<H64>,
    /// A gossip order message for the pair was handled after the start.
    order_message: bool,
    /// At the end of a trie-worker group applied after the start, the node held `trigger_root`.
    held_advertised: bool,
}

/// One watched keep-alive request.
struct WatchEntry {
    pubkey: String,
    started_ms: u64,
    pairs: HashMap<AlbOrderedOrderbookPair, PairWatch>,
}

#[derive(Default)]
struct Table {
    next_id: u64,
    entries: HashMap<u64, WatchEntry>,
    /// The ids of the entries of each pubkey, so that a hook touches only the entries of its pubkey.
    by_pubkey: HashMap<String, Vec<u64>>,
}

impl Table {
    fn remove(&mut self, id: u64) {
        if let Some(entry) = self.entries.remove(&id) {
            if let Some(ids) = self.by_pubkey.get_mut(&entry.pubkey) {
                ids.retain(|other| *other != id);
                if ids.is_empty() {
                    self.by_pubkey.remove(&entry.pubkey);
                }
            }
        }
    }

    fn remove_older_than(&mut self, now_ms: u64) {
        let old: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| now_ms.saturating_sub(entry.started_ms) > MAX_ENTRY_AGE_MS)
            .map(|(id, _)| *id)
            .collect();
        for id in old {
            self.remove(id);
        }
    }

    fn remove_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(id, entry)| (entry.started_ms, **id))
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.remove(id);
        }
    }

    /// Applies `apply` to the watch of `pair` of every entry of `pubkey`.
    fn for_pair_of(&mut self, pubkey: &str, pair: &str, mut apply: impl FnMut(&mut PairWatch)) {
        let Some(ids) = self.by_pubkey.get(pubkey) else {
            return;
        };
        for id in ids {
            if let Some(pair_watch) = self.entries.get_mut(id).and_then(|entry| entry.pairs.get_mut(pair)) {
                apply(pair_watch);
            }
        }
    }
}

/// The candidate rules that would have skipped the diverged pairs, counted over those pairs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SkipCounts {
    /// The diverged pairs, which are the pairs the request to the other relay names.
    pub(crate) pairs: usize,
    pub(crate) skip_a: usize,
    pub(crate) skip_b: usize,
    pub(crate) skip_d: usize,
    /// The pairs with (a) or (d).
    pub(crate) skip_ad: usize,
    /// The pairs with (a) or (b).
    pub(crate) skip_ab: usize,
    /// From the start of the watch to the check.
    pub(crate) since_keep_alive_ms: u64,
}

/// The watch table of one node (shared by [`OrdermatchContext`] and [`TrieStore`], like `pair_liveness`).
#[derive(Default)]
pub(crate) struct MismatchWatch {
    /// The `sync_request_alternate_relay` switch.
    alternate_relay: bool,
    /// Forces the DEBUG half of the gate (tests only): with it, the gate reads this and never the logger.
    #[cfg(test)]
    force_debug: std::sync::atomic::AtomicBool,
    /// The number of entries, read by every hook before it takes the lock.
    count: AtomicUsize,
    table: PaMutex<Table>,
}

/// Ends its entry when dropped (a path that ends in any way, a dropped future included).
pub(crate) struct WatchGuard {
    watch: Arc<MismatchWatch>,
    id: u64,
}

impl Drop for WatchGuard {
    fn drop(&mut self) { self.watch.end(self.id); }
}

impl MismatchWatch {
    pub(crate) fn new(alternate_relay: bool) -> MismatchWatch {
        MismatchWatch {
            alternate_relay,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn force_debug(&self) { self.force_debug.store(true, AtomicOrdering::Relaxed); }

    fn debug_half(&self) -> bool {
        #[cfg(test)]
        {
            self.force_debug.load(AtomicOrdering::Relaxed)
        }
        #[cfg(not(test))]
        {
            debug_enabled()
        }
    }

    /// Whether the table holds no entry: every hook returns at once then.
    fn is_idle(&self) -> bool { self.count.load(AtomicOrdering::Relaxed) == 0 }

    /// Starts the watch of a keep-alive request of `pubkey` that names `advertised` (the roots the keep-alive
    /// advertised for the pairs of the request). Only when the request to another relay is switched on and DEBUG
    /// logging is on. Entries older than 30 s are dropped first, and the oldest are dropped when more than 1,024
    /// would remain.
    pub(crate) fn start(
        self: &Arc<Self>,
        pubkey: &str,
        advertised: &HashMap<AlbOrderedOrderbookPair, H64>,
        now_ms: u64,
    ) -> Option<WatchGuard> {
        if !self.alternate_relay || advertised.is_empty() || !self.debug_half() {
            return None;
        }
        let pairs = advertised
            .iter()
            .map(|(pair, root)| {
                (pair.clone(), PairWatch {
                    trigger_root: *root,
                    newest_advertised: None,
                    order_message: false,
                    held_advertised: false,
                })
            })
            .collect();
        let mut table = self.table.lock();
        table.remove_older_than(now_ms);
        let id = table.next_id;
        table.next_id += 1;
        table.entries.insert(id, WatchEntry {
            pubkey: pubkey.to_owned(),
            started_ms: now_ms,
            pairs,
        });
        table.by_pubkey.entry(pubkey.to_owned()).or_default().push(id);
        while table.entries.len() > MAX_ENTRIES {
            table.remove_oldest();
        }
        self.count.store(table.entries.len(), AtomicOrdering::Relaxed);
        Some(WatchGuard {
            watch: self.clone(),
            id,
        })
    }

    fn end(&self, id: u64) {
        let mut table = self.table.lock();
        table.remove(id);
        self.count.store(table.entries.len(), AtomicOrdering::Relaxed);
    }

    /// Hook (a): a keep-alive of `pubkey` that names `roots` reached the comparison of its roots. Called before
    /// the entry of this keep-alive is made, so a keep-alive never updates its own entry.
    pub(crate) fn note_keep_alive(&self, pubkey: &str, roots: &HashMap<AlbOrderedOrderbookPair, H64>) {
        if self.is_idle() {
            return;
        }
        let mut table = self.table.lock();
        for (pair, root) in roots {
            table.for_pair_of(pubkey, pair, |pair_watch| pair_watch.newest_advertised = Some(*root));
        }
    }

    /// Hook (b): the gossip order message that produced `ops` changed the index. Only inserts and removals count.
    pub(crate) fn note_order_ops(&self, ops: &[TrieOp]) {
        if self.is_idle() {
            return;
        }
        let mut table = self.table.lock();
        for op in ops {
            match op {
                TrieOp::Insert { pubkey, alb_pair, .. } | TrieOp::Remove { pubkey, alb_pair, .. } => {
                    table.for_pair_of(pubkey, alb_pair, |pair_watch| pair_watch.order_message = true)
                },
                _ => (),
            }
        }
    }

    /// Hook (d): a trie-worker group for (`pubkey`, `pair`) has been applied. `root` reads the root the trie store
    /// holds for the pair now (the all-zero root if absent); it is called only when the table is not empty.
    pub(crate) fn note_group_end(&self, pubkey: &str, pair: &str, root: impl FnOnce() -> H64) {
        if self.is_idle() {
            return;
        }
        let root = root();
        self.table.lock().for_pair_of(pubkey, pair, |pair_watch| {
            if pair_watch.trigger_root == root {
                pair_watch.held_advertised = true;
            }
        });
    }

    /// The counts for the `diverged` pairs of the entry of `guard` at the check at `now_ms`; `held` reads the root
    /// this node holds for a pair now. `None` when the entry has been dropped by the backstop.
    pub(crate) fn counts<'a>(
        &self,
        guard: &WatchGuard,
        now_ms: u64,
        diverged: impl IntoIterator<Item = &'a str>,
        held: impl Fn(&str) -> H64,
    ) -> Option<SkipCounts> {
        let table = self.table.lock();
        let entry = table.entries.get(&guard.id)?;
        let mut counts = SkipCounts {
            since_keep_alive_ms: now_ms.saturating_sub(entry.started_ms),
            ..Default::default()
        };
        for pair in diverged {
            counts.pairs += 1;
            let Some(pair_watch) = entry.pairs.get(pair) else {
                continue;
            };
            let a = pair_watch
                .newest_advertised
                .is_some_and(|root| root != pair_watch.trigger_root && root == held(pair));
            let b = pair_watch.order_message;
            let d = pair_watch.held_advertised;
            counts.skip_a += a as usize;
            counts.skip_b += b as usize;
            counts.skip_d += d as usize;
            counts.skip_ad += (a || d) as usize;
            counts.skip_ab += (a || b) as usize;
        }
        Some(counts)
    }

    /// The entries of the table (tests only).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize { self.table.lock().entries.len() }

    /// The table and its atomic count and index hold nothing (tests only).
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        let table = self.table.lock();
        table.entries.is_empty() && table.by_pubkey.is_empty() && self.count.load(AtomicOrdering::Relaxed) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const Q: &str = "03bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const X: &str = "C1:C2";
    const Y: &str = "C3:C4";
    const Z: &str = "C5:C6";
    const A: H64 = [1; 8];
    const B: H64 = [2; 8];
    const C: H64 = [3; 8];
    const R1: H64 = [9; 8];

    fn watch() -> Arc<MismatchWatch> {
        let watch = Arc::new(MismatchWatch::new(true));
        watch.force_debug();
        watch
    }

    fn roots(pairs: &[(&str, H64)]) -> HashMap<AlbOrderedOrderbookPair, H64> {
        pairs.iter().map(|(pair, root)| (pair.to_string(), *root)).collect()
    }

    fn insert_op(pubkey: &str, pair: &str) -> TrieOp {
        TrieOp::Insert {
            pubkey: pubkey.to_owned(),
            alb_pair: pair.to_owned(),
            uuid: Uuid::new_v4(),
            order: OrderbookItem {
                pubkey: pubkey.to_owned(),
                base: "C1".to_owned(),
                rel: "C2".to_owned(),
                price: BigRational::from_integer(1.into()),
                max_volume: BigRational::from_integer(1.into()),
                min_volume: BigRational::from_integer(0.into()),
                uuid: Uuid::new_v4(),
                created_at: 0,
                base_protocol_info: vec![],
                rel_protocol_info: vec![],
                conf_settings: None,
            },
        }
    }

    fn remove_op(pubkey: &str, pair: &str) -> TrieOp {
        TrieOp::Remove {
            pubkey: pubkey.to_owned(),
            alb_pair: pair.to_owned(),
            uuid: Uuid::new_v4(),
        }
    }

    /// The counts for `pairs` at 5 s after the start at 1,000 ms; every pair is held at `held`.
    fn counts(watch: &MismatchWatch, guard: &WatchGuard, pairs: &[&str], held: H64) -> SkipCounts {
        watch
            .counts(guard, 6_000, pairs.iter().copied(), |_| held)
            .expect("the entry exists")
    }

    fn started(watch: &Arc<MismatchWatch>, pubkey: &str, advertised: &[(&str, H64)]) -> WatchGuard {
        watch.start(pubkey, &roots(advertised), 1_000).expect("the gate is on")
    }

    #[test]
    fn a_newer_keep_alive_that_names_the_held_root_is_flag_a() {
        let watch = watch();
        let guard = started(&watch, P, &[(X, A)]);

        watch.note_keep_alive(P, &roots(&[(X, B)]));
        let c = counts(&watch, &guard, &[X], B);
        assert_eq!((c.pairs, c.skip_a, c.skip_ad, c.skip_ab), (1, 1, 1, 1));
        assert_eq!((c.skip_b, c.skip_d), (0, 0));
        assert_eq!(c.since_keep_alive_ms, 5_000);

        // The held root is another one.
        assert_eq!(counts(&watch, &guard, &[X], C).skip_a, 0);

        // The newest keep-alive names the triggering root again.
        watch.note_keep_alive(P, &roots(&[(X, A)]));
        assert_eq!(counts(&watch, &guard, &[X], A).skip_a, 0);
        assert_eq!(counts(&watch, &guard, &[X], B).skip_a, 0);
    }

    #[test]
    fn a_keep_alive_of_another_pubkey_or_pair_is_not_flag_a() {
        let watch = watch();
        let guard = started(&watch, P, &[(X, A)]);
        watch.note_keep_alive(Q, &roots(&[(X, B)]));
        watch.note_keep_alive(P, &roots(&[(Y, B)]));
        assert_eq!(counts(&watch, &guard, &[X], B).skip_a, 0);
    }

    #[test]
    fn an_entry_never_learns_from_its_own_keep_alive() {
        // The hook runs before the entry of the keep-alive exists.
        let watch = watch();
        watch.note_keep_alive(P, &roots(&[(X, A)]));
        let guard = started(&watch, P, &[(X, A)]);
        let c = counts(&watch, &guard, &[X], A);
        assert_eq!((c.skip_a, c.skip_ad, c.skip_ab), (0, 0, 0));
    }

    #[test]
    fn an_order_message_after_the_start_is_flag_b() {
        let watch = watch();
        // Before the start: no entry, nothing to set.
        watch.note_order_ops(&[insert_op(P, X)]);
        let guard = started(&watch, P, &[(X, A)]);
        assert_eq!(counts(&watch, &guard, &[X], C).skip_b, 0);

        // Another pair of the pubkey, and the same pair of another pubkey.
        watch.note_order_ops(&[insert_op(P, Y), insert_op(Q, X)]);
        assert_eq!(counts(&watch, &guard, &[X], C).skip_b, 0);

        // A created or updated message is an insert, a cancelled message a remove.
        watch.note_order_ops(&[insert_op(P, X)]);
        let c = counts(&watch, &guard, &[X], C);
        assert_eq!((c.skip_b, c.skip_ab, c.skip_ad), (1, 1, 0));

        let other = started(&watch, P, &[(X, A)]);
        assert_eq!(counts(&watch, &other, &[X], C).skip_b, 0);
        watch.note_order_ops(&[remove_op(P, X)]);
        assert_eq!(counts(&watch, &other, &[X], C).skip_b, 1);

        // Operations that are no order message do nothing.
        let third = started(&watch, P, &[(X, A)]);
        watch.note_order_ops(&[TrieOp::ClearPair {
            pubkey: P.to_owned(),
            alb_pair: X.to_owned(),
        }]);
        assert_eq!(counts(&watch, &third, &[X], C).skip_b, 0);
    }

    #[test]
    fn a_group_end_at_the_advertised_root_after_the_start_is_flag_d() {
        let watch = watch();
        let guard = started(&watch, P, &[(X, A)]);

        watch.note_group_end(P, X, || B);
        assert_eq!(counts(&watch, &guard, &[X], C).skip_d, 0);
        watch.note_group_end(Q, X, || A);
        watch.note_group_end(P, Y, || A);
        assert_eq!(counts(&watch, &guard, &[X], C).skip_d, 0);

        watch.note_group_end(P, X, || A);
        let c = counts(&watch, &guard, &[X], C);
        assert_eq!((c.skip_d, c.skip_ad, c.skip_ab), (1, 1, 0));

        // It stays set when the root moves on.
        watch.note_group_end(P, X, || R1);
        assert_eq!(counts(&watch, &guard, &[X], C).skip_d, 1);
    }

    #[test]
    fn add_then_cancel_does_not_make_the_old_root_flag_d() {
        let watch = watch();
        // The node held the root A before the keep-alive.
        watch.note_group_end(P, X, || A);
        let guard = started(&watch, P, &[(X, A)]);
        // The maker added an order and cancelled it again: the root after every group is R1.
        watch.note_group_end(P, X, || R1);
        watch.note_group_end(P, X, || R1);
        let c = counts(&watch, &guard, &[X], R1);
        assert_eq!((c.skip_d, c.skip_ad), (0, 0));
    }

    #[test]
    fn the_group_end_reads_its_root_only_when_there_is_an_entry() {
        let watch = watch();
        watch.note_group_end(P, X, || panic!("the table is empty"));
    }

    #[test]
    fn the_unions_count_a_pair_once() {
        let watch = watch();
        let w = "C7:C8";
        let guard = started(&watch, P, &[(X, A), (Y, A), (Z, A), (w, A)]);
        // X has (a) only.
        watch.note_keep_alive(P, &roots(&[(X, B), (w, B)]));
        // Y has (b) only.
        watch.note_order_ops(&[insert_op(P, Y), insert_op(P, w)]);
        // Z has (d) only.
        watch.note_group_end(P, Z, || A);

        // X and w are held at B, the others at C.
        let c = watch
            .counts(
                &guard,
                1_500,
                [X, Y, Z, w],
                |pair| if pair == X || pair == w { B } else { C },
            )
            .unwrap();
        assert_eq!(c, SkipCounts {
            pairs: 4,
            skip_a: 2,
            skip_b: 2,
            skip_d: 1,
            skip_ad: 3,
            skip_ab: 3,
            since_keep_alive_ms: 500,
        });

        // Only the diverged pairs are counted.
        let c = watch.counts(&guard, 1_500, [Y, Z], |_| C).unwrap();
        assert_eq!(
            (c.pairs, c.skip_a, c.skip_b, c.skip_d, c.skip_ad, c.skip_ab),
            (2, 0, 1, 1, 1, 1)
        );
    }

    #[test]
    fn dropping_the_guard_empties_the_table() {
        let watch = watch();
        assert!(watch.is_empty());
        let first = started(&watch, P, &[(X, A)]);
        let second = started(&watch, P, &[(Y, A)]);
        let third = started(&watch, Q, &[(X, A)]);
        assert_eq!(watch.len(), 3);
        assert_eq!(watch.count.load(AtomicOrdering::Relaxed), 3);
        drop(second);
        assert_eq!(watch.len(), 2);
        assert_eq!(watch.count.load(AtomicOrdering::Relaxed), 2);
        drop(first);
        drop(third);
        assert!(watch.is_empty());
        assert!(watch.is_idle());
    }

    #[test]
    fn entries_older_than_30_s_are_dropped_by_the_next_start() {
        let watch = watch();
        let old = watch.start(P, &roots(&[(X, A)]), 0).unwrap();
        // Exactly 30 s old is not older than 30 s.
        let edge = watch.start(P, &roots(&[(X, A)]), 30_000).unwrap();
        assert_eq!(watch.len(), 2);
        let newest = watch.start(Q, &roots(&[(X, A)]), 30_001).unwrap();
        assert_eq!(watch.len(), 2, "the entry of 0 ms is gone");
        assert!(watch.counts(&old, 30_001, [X], |_| A).is_none());
        assert!(watch.counts(&edge, 30_001, [X], |_| A).is_some());
        assert_eq!(watch.count.load(AtomicOrdering::Relaxed), 2);
        // The guard of a dropped entry ends nothing else.
        drop(old);
        assert_eq!(watch.len(), 2);
        drop(edge);
        drop(newest);
        assert!(watch.is_empty());
    }

    #[test]
    fn the_1025th_start_drops_the_oldest_entry() {
        let watch = watch();
        let mut guards = Vec::new();
        for i in 0..MAX_ENTRIES as u64 {
            guards.push(watch.start(P, &roots(&[(X, A)]), 1_000 + i).unwrap());
        }
        assert_eq!(watch.len(), MAX_ENTRIES);
        guards.push(watch.start(P, &roots(&[(X, A)]), 1_000 + MAX_ENTRIES as u64).unwrap());
        assert_eq!(watch.len(), MAX_ENTRIES);
        assert_eq!(watch.count.load(AtomicOrdering::Relaxed), MAX_ENTRIES);
        assert!(
            watch.counts(&guards[0], 3_000, [X], |_| A).is_none(),
            "the oldest is gone"
        );
        assert!(watch.counts(&guards[1], 3_000, [X], |_| A).is_some());
        assert!(watch.counts(guards.last().unwrap(), 3_000, [X], |_| A).is_some());
        guards.clear();
        assert!(watch.is_empty());
    }

    #[test]
    fn with_the_gate_off_nothing_is_ever_inserted() {
        // The switch is off, the DEBUG half is forced on.
        let switch_off = Arc::new(MismatchWatch::new(false));
        switch_off.force_debug();
        assert!(switch_off.start(P, &roots(&[(X, A)]), 1_000).is_none());

        // The switch is on, DEBUG is not forced.
        let debug_off = Arc::new(MismatchWatch::new(true));
        assert!(debug_off.start(P, &roots(&[(X, A)]), 1_000).is_none());

        // A request that names no pair is not watched.
        assert!(watch().start(P, &HashMap::new(), 1_000).is_none());

        for watch in [switch_off, debug_off] {
            watch.note_keep_alive(P, &roots(&[(X, A)]));
            watch.note_order_ops(&[insert_op(P, X)]);
            watch.note_group_end(P, X, || panic!("the table is empty"));
            assert!(watch.is_empty());
        }
    }
}
