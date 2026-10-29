//! A bounded, content-addressed memo for the rational-to-decimal conversion.
//!
//! [`rational_to_dec`](crate::rational_to_dec) divides two big integers to a fixed number of digits,
//! which costs microseconds per call on a non-terminating rational. An RPC that renders thousands of
//! orders repeats that division for the same numbers on every call. [`DecimalMemo`] remembers the
//! result per *exact* rational, keyed by the full numerator and denominator, so a hit returns the
//! decimal that `rational_to_dec` produced for that very pair and a miss runs `rational_to_dec`
//! itself. The digits are therefore identical to the unmemoized conversion by construction, and the
//! memo cannot be stale: it caches a pure function of its key.
//!
//! The memo keeps the printed form of each decimal next to its value ([`RenderedDecimal`]), so that an
//! RPC answer serializes the stored text and does not format the same big decimal again on every call.

use crate::{rational_to_dec, BigDecimal, BigRational};
use num_bigint::BigInt;
use serde::{Serialize, Serializer};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

/// Number of entries the process-wide memo holds before it is emptied.
pub(crate) const GLOBAL_DECIMAL_MEMO_CAPACITY: usize = 32_768;

/// A decimal together with the text it prints as. Cloning shares both.
///
/// Serializes exactly as the [`BigDecimal`] it holds does, a JSON string with the same digits, because
/// the text is that decimal's `Display` output and `BigDecimal` serializes by writing that output.
#[derive(Clone, Debug)]
pub struct RenderedDecimal(Arc<RenderedParts>);

#[derive(Debug)]
struct RenderedParts {
    decimal: BigDecimal,
    text: String,
}

impl RenderedDecimal {
    fn new(decimal: BigDecimal) -> RenderedDecimal {
        let text = decimal.to_string();
        RenderedDecimal(Arc::new(RenderedParts { decimal, text }))
    }

    /// The decimal value.
    pub fn decimal(&self) -> &BigDecimal { &self.0.decimal }

    /// The decimal as printed.
    pub fn as_str(&self) -> &str { &self.0.text }
}

impl Serialize for RenderedDecimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.text)
    }
}

struct Entry {
    numer: BigInt,
    denom: BigInt,
    rendered: RenderedDecimal,
}

#[derive(Default)]
struct Inner {
    /// Entries grouped by a 64-bit hash of (numerator, denominator). A bucket is searched by comparing
    /// the full numerator and denominator, so a hash collision costs a comparison and never a wrong result.
    buckets: HashMap<u64, Vec<Entry>>,
    len: usize,
}

/// A bounded memo from an exact rational to `rational_to_dec` of that rational.
///
/// The lock is held only to look up or insert an entry, never while a division runs. When the memo
/// holds `capacity` entries and a new one is to be stored, all entries are dropped first, so a workload
/// whose values keep changing cannot grow it without limit.
pub struct DecimalMemo {
    capacity: usize,
    inner: Mutex<Inner>,
}

fn key_hash(r: &BigRational) -> u64 {
    // Hash numerator and denominator as they are stored. `BigRational`'s own `Hash` normalizes through
    // a division loop, which is the kind of work the memo is meant to avoid.
    let mut hasher = DefaultHasher::new();
    r.numer().hash(&mut hasher);
    r.denom().hash(&mut hasher);
    hasher.finish()
}

impl DecimalMemo {
    /// Creates a memo that holds at most `capacity` entries (at least one).
    pub fn with_capacity(capacity: usize) -> DecimalMemo {
        DecimalMemo {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The decimal for `r`, equal to `rational_to_dec(r)`, with its printed form.
    ///
    /// The key is the exact numerator and denominator as stored in `r`: two rationals that are equal
    /// as numbers but written with different numerators and denominators are separate entries.
    pub fn get_or_compute(&self, r: &BigRational) -> RenderedDecimal {
        self.get_or_compute_with(r, || rational_to_dec(r))
    }

    /// Like [`DecimalMemo::get_or_compute`], for a decimal that `compute` derives from `key` alone.
    ///
    /// `compute` must be a pure function of the exact numerator and denominator of `key`: the memo stores
    /// its result under that key and returns it for every later call with the same key without calling
    /// `compute` again. Use one memo per derivation, never one memo for two different derivations.
    pub fn get_or_compute_with(&self, key: &BigRational, compute: impl FnOnce() -> BigDecimal) -> RenderedDecimal {
        let hash = key_hash(key);
        if let Some(found) = self.lookup(hash, key) {
            return found;
        }
        let rendered = RenderedDecimal::new(compute());
        self.insert(hash, key, &rendered);
        rendered
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize { self.lock().len }

    /// `true` if the memo holds no entry.
    pub fn is_empty(&self) -> bool { self.len() == 0 }

    // The guarded state is only ever changed by whole-entry pushes and a full clear, none of which can
    // panic midway, so the state behind a poisoned lock is still consistent.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> { self.inner.lock().unwrap_or_else(PoisonError::into_inner) }

    fn lookup(&self, hash: u64, r: &BigRational) -> Option<RenderedDecimal> {
        let inner = self.lock();
        inner
            .buckets
            .get(&hash)?
            .iter()
            .find(|e| e.numer == *r.numer() && e.denom == *r.denom())
            .map(|e| e.rendered.clone())
    }

    fn insert(&self, hash: u64, r: &BigRational, rendered: &RenderedDecimal) {
        let mut inner = self.lock();
        // Another thread may have stored the same key while this one was dividing.
        if let Some(bucket) = inner.buckets.get(&hash) {
            if bucket.iter().any(|e| e.numer == *r.numer() && e.denom == *r.denom()) {
                return;
            }
        }
        if inner.len >= self.capacity {
            inner.buckets.clear();
            inner.len = 0;
        }
        inner.buckets.entry(hash).or_default().push(Entry {
            numer: r.numer().clone(),
            denom: r.denom().clone(),
            rendered: rendered.clone(),
        });
        inner.len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MmNumber;
    use std::sync::Arc;

    /// Deterministic spread of numerators and denominators, non-terminating for most pairs.
    fn sample_rationals(n: usize) -> Vec<BigRational> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 17
        };
        (0..n)
            .map(|_| {
                let numer = BigInt::from(next() % 9_999_999_999 + 1);
                let denom = BigInt::from(next() % 99_999 + 1_000);
                BigRational::new(numer, denom)
            })
            .collect()
    }

    #[test]
    fn memoized_decimal_equals_rational_to_dec() {
        let memo = DecimalMemo::with_capacity(1_000);
        let rationals = sample_rationals(500);
        for _round in 0..2 {
            for r in &rationals {
                // The second round is served from the memo and must print the same digits.
                assert_eq!(memo.get_or_compute(r).as_str(), rational_to_dec(r).to_string());
            }
        }
        assert!(memo.len() <= 500);
    }

    #[test]
    fn derived_decimal_is_computed_once_per_key() {
        let memo = DecimalMemo::with_capacity(10);
        let key = BigRational::new(7.into(), 9.into());
        let calls = std::cell::Cell::new(0);
        let derive = || {
            calls.set(calls.get() + 1);
            rational_to_dec(&key) * BigDecimal::from(2)
        };
        let first = memo.get_or_compute_with(&key, derive);
        let second = memo.get_or_compute_with(&key, derive);
        assert_eq!(calls.get(), 1);
        assert_eq!(first.as_str(), second.as_str());
        assert_eq!(
            first.as_str(),
            (rational_to_dec(&key) * BigDecimal::from(2)).to_string()
        );
    }

    #[test]
    fn key_is_the_full_rational() {
        let memo = DecimalMemo::with_capacity(100);
        let a = BigRational::new(1.into(), 3.into());
        let b = BigRational::new(1.into(), 7.into());
        let c = BigRational::new(2.into(), 3.into());
        // Equal numerator, different denominator, and the other way round.
        assert_ne!(memo.get_or_compute(&a).as_str(), memo.get_or_compute(&b).as_str());
        assert_ne!(memo.get_or_compute(&a).as_str(), memo.get_or_compute(&c).as_str());
        assert_eq!(memo.get_or_compute(&a).as_str(), rational_to_dec(&a).to_string());
        assert_eq!(memo.get_or_compute(&b).as_str(), rational_to_dec(&b).to_string());
        assert_eq!(memo.get_or_compute(&c).as_str(), rational_to_dec(&c).to_string());
        assert_eq!(memo.len(), 3);

        // The same number written with a different numerator and denominator is a separate entry that
        // holds whatever `rational_to_dec` gives for that pair.
        let unreduced = BigRational::new_raw(2.into(), 6.into());
        assert_eq!(
            memo.get_or_compute(&unreduced).as_str(),
            rational_to_dec(&unreduced).to_string()
        );
        assert_eq!(memo.len(), 4);
        assert_eq!(memo.get_or_compute(&a).as_str(), rational_to_dec(&a).to_string());
        assert_eq!(memo.len(), 4);
    }

    #[test]
    fn size_stays_bounded_and_results_stay_correct() {
        const CAP: usize = 16;
        let memo = DecimalMemo::with_capacity(CAP);
        let rationals = sample_rationals(200);
        for _round in 0..3 {
            for r in &rationals {
                assert_eq!(memo.get_or_compute(r).as_str(), rational_to_dec(r).to_string());
                assert!(memo.len() <= CAP, "memo holds {} entries, cap {CAP}", memo.len());
            }
        }
        assert!(!memo.is_empty());
    }

    #[test]
    fn concurrent_use_gives_identical_results() {
        let rationals = Arc::new(sample_rationals(400));
        let expected: Arc<Vec<String>> = Arc::new(rationals.iter().map(|r| rational_to_dec(r).to_string()).collect());
        // A small capacity makes the threads clear the memo under each other, a large one makes them hit.
        for capacity in [64, 10_000] {
            let memo = Arc::new(DecimalMemo::with_capacity(capacity));
            let handles: Vec<_> = (0..8)
                .map(|t| {
                    let (memo, rationals, expected) = (memo.clone(), rationals.clone(), expected.clone());
                    std::thread::spawn(move || {
                        for round in 0..4 {
                            for i in 0..rationals.len() {
                                // Every thread walks the set from a different start.
                                let i = (i + t * 37 + round * 11) % rationals.len();
                                assert_eq!(memo.get_or_compute(&rationals[i]).as_str(), expected[i]);
                            }
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().expect("a worker thread panicked");
            }
            assert!(memo.len() <= capacity);
        }
    }

    #[test]
    fn rendered_decimal_serializes_like_big_decimal() {
        for r in sample_rationals(100) {
            let expected = rational_to_dec(&r);
            let rendered = RenderedDecimal::new(expected.clone());
            assert_eq!(
                serde_json::to_string(&rendered).unwrap(),
                serde_json::to_string(&expected).unwrap()
            );
        }
        for text in ["0", "-1.5", "100", "0.00000001", "123456789012345678901234567890.123"] {
            let d: BigDecimal = text.parse().unwrap();
            assert_eq!(
                serde_json::to_string(&RenderedDecimal::new(d.clone())).unwrap(),
                serde_json::to_string(&d).unwrap()
            );
        }
    }

    #[test]
    fn mm_number_memoized_decimal_equals_to_decimal() {
        for r in sample_rationals(100) {
            let n = MmNumber::from(r);
            let rendered = n.to_rendered_decimal();
            assert_eq!(rendered.as_str(), n.to_decimal().to_string());
            assert_eq!(rendered.decimal(), &n.to_decimal());
            // Second call is a hit.
            assert_eq!(n.to_rendered_decimal().as_str(), n.to_decimal().to_string());
        }
    }
}
