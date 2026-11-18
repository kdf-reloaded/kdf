//! The absence rule of the history collectors (CRD ch.38 §38.5C, D38.5C.1 status note).
//!
//! A stored history row is removed only when two qualifying listings, at least one block apart, both lack its
//! transaction hash. This module holds the rule itself: the marks (the tip height at which a hash was first seen
//! absent) and the decision of one listing. The collectors own the listing, the storage and the writing.
//!
//! A listing qualifies when it was complete and answered by a server that was up to date at that moment, on a coin
//! whose servers do not disagree about the chain. A collector passes a listing here only then, together with the tip
//! height of the answering server; any other listing neither sets nor clears a mark.

use rpc::v1::types::H256 as H256Json;
use std::collections::{HashMap, HashSet};

/// The marks of one collector. Memory only: after a restart the counting starts again, which is safe.
#[derive(Debug, Default)]
pub(crate) struct AbsenceMarks {
    /// Transaction hash, to the tip height at which a qualifying listing first lacked it.
    marks: HashMap<H256Json, u64>,
}

impl AbsenceMarks {
    /// Whether any hash carries a mark. While one does, the collector's need-to-run test is true.
    pub(crate) fn any(&self) -> bool { !self.marks.is_empty() }

    /// Judges one qualifying listing taken at `tip`. `stored` are the transaction hashes of the stored rows (one entry
    /// per hash is enough; repeats are harmless), `listed` the hashes the listing contains.
    ///
    /// - A stored hash that the listing contains loses its mark.
    /// - A stored hash that it lacks and that has no mark gets the mark `tip`.
    /// - A stored hash that it lacks and whose mark is at most `tip - 1` is returned: it is to be removed, with all
    ///   its rows, and its mark is dropped. A listing at the same tip as the mark, or lower, changes nothing.
    /// - A mark of a hash that is no longer stored is dropped.
    pub(crate) fn judge(
        &mut self,
        tip: u64,
        listed: &HashSet<H256Json>,
        stored: impl IntoIterator<Item = H256Json>,
    ) -> Vec<H256Json> {
        let stored: HashSet<H256Json> = stored.into_iter().collect();
        self.marks.retain(|hash, _| stored.contains(hash));

        let mut remove = Vec::new();
        for hash in stored {
            if listed.contains(&hash) {
                self.marks.remove(&hash);
                continue;
            }
            match self.marks.get(&hash) {
                None => {
                    self.marks.insert(hash, tip);
                },
                Some(&mark) if tip > mark => {
                    self.marks.remove(&hash);
                    remove.push(hash);
                },
                Some(_) => (),
            }
        }
        remove
    }
}

/// The line logged once for a pass that removed rows.
pub(crate) fn removed_rows_line(ticker: &str, removed: usize) -> String {
    format!(
        "{}: removed {} history rows whose transactions the servers no longer list",
        ticker, removed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> H256Json { H256Json::from([byte; 32]) }

    fn set(bytes: &[u8]) -> HashSet<H256Json> { bytes.iter().map(|b| h(*b)).collect() }

    fn stored(bytes: &[u8]) -> Vec<H256Json> { bytes.iter().map(|b| h(*b)).collect() }

    #[test]
    fn absent_from_two_listings_one_block_apart_is_removed() {
        let mut marks = AbsenceMarks::default();
        assert!(marks.judge(100, &set(&[1]), stored(&[1, 2])).is_empty());
        assert!(marks.any());
        assert_eq!(marks.judge(101, &set(&[1]), stored(&[1, 2])), vec![h(2)]);
        assert!(!marks.any());
    }

    #[test]
    fn absent_from_one_listing_is_kept_and_marked() {
        let mut marks = AbsenceMarks::default();
        assert!(marks.judge(100, &set(&[]), stored(&[7])).is_empty());
        assert!(marks.any());
    }

    #[test]
    fn two_listings_at_the_same_tip_do_not_remove() {
        let mut marks = AbsenceMarks::default();
        assert!(marks.judge(100, &set(&[]), stored(&[7])).is_empty());
        assert!(marks.judge(100, &set(&[]), stored(&[7])).is_empty());
        // A lower tip (a reorganisation) does not remove either, and keeps the first mark.
        assert!(marks.judge(99, &set(&[]), stored(&[7])).is_empty());
        assert_eq!(marks.judge(101, &set(&[]), stored(&[7])), vec![h(7)]);
    }

    #[test]
    fn a_listing_that_contains_the_hash_clears_the_mark() {
        let mut marks = AbsenceMarks::default();
        marks.judge(100, &set(&[]), stored(&[7]));
        assert!(marks.judge(101, &set(&[7]), stored(&[7])).is_empty());
        assert!(!marks.any());
        // Counting starts again.
        assert!(marks.judge(102, &set(&[]), stored(&[7])).is_empty());
        assert_eq!(marks.judge(103, &set(&[]), stored(&[7])), vec![h(7)]);
    }

    #[test]
    fn a_mark_of_a_hash_that_is_no_longer_stored_is_dropped() {
        let mut marks = AbsenceMarks::default();
        marks.judge(100, &set(&[]), stored(&[7]));
        marks.judge(101, &set(&[]), stored(&[]));
        assert!(!marks.any());
    }

    #[test]
    fn the_log_line_names_the_coin_and_the_count() {
        assert_eq!(
            removed_rows_line("RIN", 3),
            "RIN: removed 3 history rows whose transactions the servers no longer list"
        );
    }
}
