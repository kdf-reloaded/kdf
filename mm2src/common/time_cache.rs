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

//! This implements a time-based LRU cache for checking gossipsub message duplicates.

use fnv::FnvHashMap;
use std::collections::hash_map::{self,
                                 Entry::{Occupied, Vacant},
                                 Iter, Keys};
use std::collections::VecDeque;
use std::time::Duration;
use wasm_timer::Instant;

#[derive(Debug)]
pub struct ExpiringElement<Element> {
    /// The element that expires
    element: Element,
    /// The expire time.
    expires: Instant,
    /// Identifies the one element of the ordered list that stands for this key. It tells that
    /// element from a leftover of an earlier entry of the same key (removed, then inserted again).
    serial: u64,
}

/// An element of the ordered list: a key waiting for its expire time.
#[derive(Debug)]
struct QueuedKey<Key> {
    key: Key,
    /// The expire time the key was queued with. The entry in the map can expire later.
    expires: Instant,
    /// The `serial` of the entry the element was queued for.
    serial: u64,
}

impl<Element> ExpiringElement<Element> {
    pub fn get_element(&self) -> &Element { &self.element }

    pub fn update_expiration(&mut self, expires: Instant) { self.expires = expires }
}

#[derive(Debug)]
pub struct TimeCache<Key, Value> {
    /// Mapping a key to its value together with its latest expire time (can be updated through
    /// reinserts).
    map: FnvHashMap<Key, ExpiringElement<Value>>,
    /// An ordered list of keys by expires time. It holds one element per key; the expire time of an
    /// element can trail the one in the map, in which case the key is queued again when the element
    /// reaches the front (see [`TimeCache::remove_expired_keys`]).
    list: VecDeque<QueuedKey<Key>>,
    /// The `serial` the next new entry gets.
    next_serial: u64,
    /// The time elements remain in the cache.
    ttl: Duration,
}

/// Room (in elements) a map or list may keep beyond twice its length before [`TimeCache::shrink_if_sparse`]
/// shrinks it.
pub const SHRINK_FLOOR: usize = 16;

pub struct OccupiedEntry<'a, K, V> {
    expiration: Instant,
    entry: hash_map::OccupiedEntry<'a, K, ExpiringElement<V>>,
}

impl<'a, K, V> OccupiedEntry<'a, K, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    pub fn into_mut(self) -> &'a mut V { &mut self.entry.into_mut().element }

    #[allow(dead_code)]
    pub fn insert_without_updating_expiration(&mut self, value: V) -> V {
        //keep old expiration, only replace value of element
        ::std::mem::replace(&mut self.entry.get_mut().element, value)
    }

    #[allow(dead_code)]
    pub fn insert_and_update_expiration(&mut self, value: V) -> V {
        // The key keeps its single list element; the later expire time in the map makes the
        // element re-queue the key when it reaches the front.
        let serial = self.entry.get().serial;
        self.entry
            .insert(ExpiringElement {
                element: value,
                expires: self.expiration,
                serial,
            })
            .element
    }

    pub fn into_mut_with_update_expiration(mut self) -> &'a mut V {
        // The key keeps its single list element, see `insert_and_update_expiration`.
        self.entry.get_mut().update_expiration(self.expiration);
        &mut self.entry.into_mut().element
    }
}

pub struct VacantEntry<'a, K, V> {
    expiration: Instant,
    entry: hash_map::VacantEntry<'a, K, ExpiringElement<V>>,
    list: &'a mut VecDeque<QueuedKey<K>>,
    serial: u64,
}

impl<'a, K, V> VacantEntry<'a, K, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    pub fn insert(self, value: V) -> &'a mut V {
        self.list.push_back(QueuedKey {
            key: self.entry.key().clone(),
            expires: self.expiration,
            serial: self.serial,
        });
        &mut self
            .entry
            .insert(ExpiringElement {
                element: value,
                expires: self.expiration,
                serial: self.serial,
            })
            .element
    }
}

pub enum Entry<'a, K: 'a, V: 'a> {
    Occupied(OccupiedEntry<'a, K, V>),
    Vacant(VacantEntry<'a, K, V>),
}

#[allow(dead_code)]
impl<'a, K: 'a, V: 'a> Entry<'a, K, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    pub fn or_insert_with<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(default()),
        }
    }

    pub fn or_insert_with_update_expiration<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Entry::Occupied(entry) => entry.into_mut_with_update_expiration(),
            Entry::Vacant(entry) => entry.insert(default()),
        }
    }
}

impl<Key, Value> TimeCache<Key, Value>
where
    Key: Eq + std::hash::Hash + Clone,
{
    pub fn new(ttl: Duration) -> Self {
        TimeCache {
            map: FnvHashMap::default(),
            list: VecDeque::new(),
            next_serial: 0,
            ttl,
        }
    }

    fn remove_expired_keys(&mut self, now: Instant) {
        while let Some(queued) = self.list.pop_front() {
            if queued.expires > now {
                self.list.push_front(queued);
                break;
            }
            if let Occupied(entry) = self.map.entry(queued.key.clone()) {
                if entry.get().serial != queued.serial {
                    // Left over from an entry that was removed and inserted again: the live element
                    // of the key is elsewhere in the list.
                    continue;
                }
                let expires = entry.get().expires;
                if expires <= now {
                    entry.remove();
                } else {
                    // The entry was refreshed after it was queued: queue the key again at its
                    // current expire time, keeping the list ordered.
                    let at = self.list.partition_point(|other| other.expires <= expires);
                    self.list.insert(at, QueuedKey {
                        key: queued.key,
                        expires,
                        serial: queued.serial,
                    });
                }
            }
        }
    }

    /// Removes the expired entries now, without waiting for the next write.
    pub fn remove_expired(&mut self) { self.remove_expired_keys(Instant::now()) }

    /// Gives memory back when the map or the list holds far more room than it needs, which they
    /// never do on their own after a burst: they are shrunk to their length once the capacity
    /// exceeds `2 * len + SHRINK_FLOOR`.
    pub fn shrink_if_sparse(&mut self) {
        if self.map.capacity() > 2 * self.map.len() + SHRINK_FLOOR {
            self.map.shrink_to_fit();
        }
        if self.list.capacity() > 2 * self.list.len() + SHRINK_FLOOR {
            self.list.shrink_to_fit();
        }
    }

    /// The capacity of the map and of the list of the cache.
    pub fn capacities(&self) -> (usize, usize) { (self.map.capacity(), self.list.capacity()) }

    /// The number of elements in the ordered list of the cache (one per key, plus those left over from
    /// removed keys until they reach the front).
    pub fn queued_len(&self) -> usize { self.list.len() }

    pub fn entry(&mut self, key: Key) -> Entry<'_, Key, Value> { self.entry_at(key, Instant::now()) }

    fn entry_at(&mut self, key: Key, now: Instant) -> Entry<'_, Key, Value> {
        self.remove_expired_keys(now);
        match self.map.entry(key) {
            Occupied(entry) => Entry::Occupied(OccupiedEntry {
                expiration: now + self.ttl,
                entry,
            }),
            Vacant(entry) => {
                let serial = self.next_serial;
                self.next_serial += 1;
                Entry::Vacant(VacantEntry {
                    expiration: now + self.ttl,
                    entry,
                    list: &mut self.list,
                    serial,
                })
            },
        }
    }

    // Inserts new element and removes any expired elements.
    //
    // If the key was not present this returns `true`. If the value was already present this
    // returns `false`.
    pub fn insert(&mut self, key: Key, value: Value) -> bool {
        if let Entry::Vacant(entry) = self.entry(key) {
            entry.insert(value);
            true
        } else {
            false
        }
    }

    // Removes a certain key even if it didn't expire plus removing other expired keys
    pub fn remove(&mut self, key: Key) -> Option<Value> {
        let result = self.map.remove(&key).map(|el| el.element);
        self.remove_expired_keys(Instant::now());
        result
    }

    /// Empties the entire cache.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.map.clear();
        self.list.clear();
    }

    pub fn contains_key(&self, key: &Key) -> bool { self.map.contains_key(key) }

    pub fn get(&self, key: &Key) -> Option<&Value> { self.map.get(key).map(|e| &e.element) }

    /// Like [`TimeCache::get`], but returns `None` for an entry whose time-to-live has elapsed,
    /// even if the entry has not been reclaimed yet (reclaiming happens on writes only).
    pub fn get_unexpired(&self, key: &Key) -> Option<&Value> { self.get_unexpired_at(key, Instant::now()) }

    fn get_unexpired_at(&self, key: &Key, now: Instant) -> Option<&Value> {
        self.map.get(key).filter(|e| e.expires > now).map(|e| &e.element)
    }

    pub fn len(&self) -> usize { self.map.len() }

    pub fn is_empty(&self) -> bool { self.map.is_empty() }

    pub fn ttl(&self) -> Duration { self.ttl }

    pub fn iter(&self) -> Iter<'_, Key, ExpiringElement<Value>> { self.map.iter() }

    pub fn keys(&self) -> Keys<'_, Key, ExpiringElement<Value>> { self.map.keys() }

    /// The values of the cache, mutably. Neither the keys nor the expire times change.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> { self.map.values_mut().map(|e| &mut e.element) }
}

impl<Key, Value> TimeCache<Key, Value>
where
    Key: Eq + std::hash::Hash + Clone,
    Value: Clone,
{
    pub fn as_hash_map(&self) -> std::collections::HashMap<Key, Value> {
        self.map
            .iter()
            .map(|(key, expiring_el)| (key.clone(), expiring_el.element.clone()))
            .collect()
    }
}

#[allow(dead_code)]
pub struct DuplicateCache<Key: std::hash::Hash>(TimeCache<Key, ()>);

#[allow(dead_code)]
impl<Key> DuplicateCache<Key>
where
    Key: Eq + std::hash::Hash + Clone,
{
    pub fn new(ttl: Duration) -> Self { Self(TimeCache::new(ttl)) }

    // Inserts new elements and removes any expired elements.
    //
    // If the key was not present this returns `true`. If the value was already present this
    // returns `false`.
    pub fn insert(&mut self, key: Key) -> bool {
        if let Entry::Vacant(entry) = self.0.entry(key) {
            entry.insert(());
            true
        } else {
            false
        }
    }

    pub fn contains(&mut self, key: &Key) -> bool { self.0.contains_key(key) }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn time_cache_added_entries_exist() {
        let mut cache = TimeCache::new(Duration::from_secs(10));

        assert!(cache.insert("t", "tv".to_owned()));
        assert!(cache.insert("e", "ev".to_owned()));

        // Should report that 't' and 't' already exists
        assert!(!cache.insert("t", "td".to_owned()));
        assert!(!cache.insert("e", "ed".to_owned()));

        assert_eq!(cache.get(&"t"), Some(&"tv".to_owned()));
        assert_eq!(cache.get(&"e"), Some(&"ev".to_owned()));
        assert_eq!(cache.get(&"f"), None);
    }

    #[test]
    fn get_unexpired_filters_expired_entries_but_get_does_not() {
        let mut cache = TimeCache::new(Duration::from_millis(100));
        assert!(cache.insert("t", "tv".to_owned()));
        assert_eq!(cache.get_unexpired(&"t"), Some(&"tv".to_owned()));
        assert_eq!(cache.get_unexpired(&"missing"), None);

        std::thread::sleep(Duration::from_millis(250));
        // No write happened, so the entry is expired but not reclaimed.
        assert_eq!(cache.get(&"t"), Some(&"tv".to_owned()));
        assert_eq!(cache.get_unexpired(&"t"), None);
    }

    #[test]
    fn time_cache_expired() {
        let mut cache = TimeCache::new(Duration::from_secs(1));

        assert!(cache.insert("t", "tv".to_owned()));
        assert_eq!(cache.get(&"t"), Some(&"tv".to_owned()));

        std::thread::sleep(Duration::from_millis(500));
        assert!(cache.insert("e", "ev".to_owned()));
        assert_eq!(cache.get(&"t"), Some(&"tv".to_owned()));
        assert_eq!(cache.get(&"e"), Some(&"ev".to_owned()));

        std::thread::sleep(Duration::from_millis(700));
        // insert other value to initiate the expiration
        assert!(cache.insert("f", "fv".to_owned()));
        // must be expired already
        assert_eq!(cache.get(&"t"), None);
        assert_eq!(cache.get(&"e"), Some(&"ev".to_owned()));

        std::thread::sleep(Duration::from_millis(700));
        // insert other value to initiate the expiration
        assert!(cache.insert("d", "dv".to_owned()));
        // must be expired already
        assert_eq!(cache.get(&"t"), None);
        assert_eq!(cache.get(&"e"), None);
    }

    #[test]
    fn cache_added_entries_exist() {
        let mut cache = DuplicateCache::new(Duration::from_secs(10));

        cache.insert("t");
        cache.insert("e");

        // Should report that 't' and 't' already exists
        assert!(!cache.insert("t"));
        assert!(!cache.insert("e"));
    }

    #[test]
    fn cache_entries_expire() {
        let mut cache = DuplicateCache::new(Duration::from_millis(100));

        cache.insert("t");
        assert!(!cache.insert("t"));
        cache.insert("e");
        //assert!(!cache.insert("t"));
        assert!(!cache.insert("e"));
        // sleep until cache expiry
        std::thread::sleep(Duration::from_millis(101));
        // add another element to clear previous cache
        cache.insert("s");

        // should be removed from the cache
        assert!(cache.insert("t"));
    }

    #[test]
    fn test_remove() {
        let mut cache = TimeCache::new(Duration::from_secs(10));

        cache.insert("t", "");
        cache.insert("e", "");
        cache.remove("e");
        assert!(!cache.contains_key(&"e"));
    }

    /// Entry at the given time (the tests run on a virtual clock): refreshed when present, inserted otherwise.
    fn touch_at(cache: &mut TimeCache<&'static str, u32>, key: &'static str, now: Instant) {
        *cache.entry_at(key, now).or_insert_with_update_expiration(|| 0) += 1;
    }

    #[test]
    fn refreshed_entry_survives_its_original_expiry() {
        let t0 = Instant::now();
        let mut cache = TimeCache::new(Duration::from_secs(10));
        touch_at(&mut cache, "k", t0);
        touch_at(&mut cache, "k", t0 + Duration::from_secs(6));

        // past the expiry the entry had when it was first queued, before the refreshed one
        cache.remove_expired_keys(t0 + Duration::from_secs(11));
        assert_eq!(cache.get(&"k"), Some(&2));
        assert_eq!(cache.queued_len(), 1, "the key is queued again, not twice");

        cache.remove_expired_keys(t0 + Duration::from_secs(15));
        assert!(cache.contains_key(&"k"), "still within 10 s of the refresh");

        cache.remove_expired_keys(t0 + Duration::from_secs(17));
        assert!(!cache.contains_key(&"k"));
        assert_eq!(cache.queued_len(), 0);
    }

    #[test]
    fn refreshes_keep_one_list_element_per_key() {
        let t0 = Instant::now();
        let mut cache = TimeCache::new(Duration::from_secs(10));
        for i in 0..1000u64 {
            for key in ["a", "b", "c"] {
                touch_at(&mut cache, key, t0 + Duration::from_millis(i));
            }
        }
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.queued_len(), 3);
        assert_eq!(cache.get(&"a"), Some(&1000));

        // insert_and_update_expiration does not queue either
        if let Entry::Occupied(mut entry) = cache.entry_at("a", t0 + Duration::from_secs(1)) {
            entry.insert_and_update_expiration(7);
        } else {
            panic!("a is cached");
        }
        assert_eq!(cache.queued_len(), 3);
        assert_eq!(cache.get(&"a"), Some(&7));
    }

    #[test]
    fn requeued_key_keeps_the_list_ordered() {
        let t0 = Instant::now();
        let secs = |n: f64| t0 + Duration::from_secs_f64(n);
        let mut cache = TimeCache::new(Duration::from_secs(10));
        touch_at(&mut cache, "a", t0); // expires at 10
        touch_at(&mut cache, "b", secs(5.)); // 15
        touch_at(&mut cache, "a", secs(6.)); // refreshed: 16
        touch_at(&mut cache, "c", secs(7.)); // 17

        // "a" reaches the front at 11 and is queued again between "b" (15) and "c" (17)
        cache.remove_expired_keys(secs(11.));
        assert_eq!(cache.queued_len(), 3);
        cache.remove_expired_keys(secs(15.5));
        assert!(!cache.contains_key(&"b"));
        assert!(cache.contains_key(&"a") && cache.contains_key(&"c"));
        cache.remove_expired_keys(secs(16.5));
        assert!(!cache.contains_key(&"a"), "a is not held back behind c");
        assert!(cache.contains_key(&"c"));
        cache.remove_expired_keys(secs(17.5));
        assert!(cache.is_empty());
        assert_eq!(cache.queued_len(), 0);
    }

    #[test]
    fn unrefreshed_entries_still_expire_on_time() {
        let t0 = Instant::now();
        let mut cache = TimeCache::new(Duration::from_secs(10));
        touch_at(&mut cache, "a", t0);
        touch_at(&mut cache, "b", t0 + Duration::from_secs(4));
        cache.remove_expired_keys(t0 + Duration::from_secs(9));
        assert_eq!(cache.len(), 2);
        cache.remove_expired_keys(t0 + Duration::from_secs(10));
        assert!(!cache.contains_key(&"a") && cache.contains_key(&"b"));
        cache.remove_expired_keys(t0 + Duration::from_secs(14));
        assert!(cache.is_empty());
    }

    #[test]
    fn list_element_of_a_removed_key_does_not_hold_back_the_reinserted_one() {
        let t0 = Instant::now();
        let mut cache = TimeCache::new(Duration::from_secs(10));
        touch_at(&mut cache, "k", t0);
        assert_eq!(cache.remove("k"), Some(1));
        touch_at(&mut cache, "k", t0 + Duration::from_secs(1));
        assert_eq!(cache.queued_len(), 2);

        // the leftover element (10) is dropped, it must not queue the live key a second time
        cache.remove_expired_keys(t0 + Duration::from_millis(10_500));
        assert_eq!(cache.get(&"k"), Some(&1));
        assert_eq!(cache.queued_len(), 1);
        cache.remove_expired_keys(t0 + Duration::from_secs(11));
        assert!(cache.is_empty());
        assert_eq!(cache.queued_len(), 0);
    }

    #[test]
    fn remove_expired_works_on_the_real_clock() {
        let mut cache = TimeCache::new(Duration::from_millis(50));
        cache.insert("t", ());
        cache.remove_expired();
        assert!(cache.contains_key(&"t"));
        std::thread::sleep(Duration::from_millis(120));
        cache.remove_expired();
        assert!(cache.is_empty());
    }

    #[test]
    fn shrink_if_sparse_only_shrinks_when_far_above_the_length() {
        let t0 = Instant::now();
        let mut cache: TimeCache<u32, u32> = TimeCache::new(Duration::from_secs(10));
        for i in 0..1000u32 {
            cache.entry_at(i, t0).or_insert_with(|| i);
        }
        let (map_cap, list_cap) = cache.capacities();
        cache.shrink_if_sparse();
        assert_eq!(cache.capacities(), (map_cap, list_cap), "dense: nothing to shrink");

        cache.remove_expired_keys(t0 + Duration::from_secs(11));
        assert!(cache.is_empty());
        assert_eq!(cache.capacities(), (map_cap, list_cap), "expiry alone keeps the room");
        cache.shrink_if_sparse();
        let (map_cap, list_cap) = cache.capacities();
        assert!(
            map_cap <= SHRINK_FLOOR && list_cap <= SHRINK_FLOOR,
            "{map_cap} {list_cap}"
        );
    }
}
