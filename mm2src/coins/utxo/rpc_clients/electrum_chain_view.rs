//! The chain view of an Electrum client: how the client sees that the chain it follows has changed (CRD ch.38 §38.5C,
//! D38.5C.2 status note, amendment of 2026-11-18).
//!
//! The view holds the base headers (the first 80 bytes, [`BASE_HEADER_LEN`]) of the last `2T + 2` heights of the chain
//! the client follows. It is fed only by header notifications and header answers of up-to-date maintained connections,
//! and not while the coin is in the disagreement state; the pool decides that. This module holds the data and the
//! decisions over it; the pool sends the requests. No block identifier is computed: base headers at one height are
//! compared with [`same_block`], which works for every header family.

use super::electrum_measure::{same_block, BASE_HEADER_LEN};
use std::collections::BTreeMap;

/// The number of heights the view holds: `2T + 2` (CRD ch.38 §38.5C amendment, item 1).
pub(super) fn window_of(tolerance: u64) -> u64 { tolerance.saturating_mul(2).saturating_add(2) }

/// What the client does when a connection first reports a height above the highest of the view.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TipPlan {
    /// The height is not above the highest in the view: nothing to check.
    Nothing,
    /// The view is empty, or the height is further above its highest than the view reaches: there is nothing to
    /// compare. The view starts again at the new tip.
    Restart,
    /// One directed request for `count` base headers from height `from` up to the one below the new tip.
    Request { from: u64, count: u64 },
}

#[derive(Debug, Default)]
pub(super) struct ChainView {
    window: u64,
    /// Height to base header, at most `window` heights, the highest last.
    headers: BTreeMap<u64, Vec<u8>>,
}

/// The first [`BASE_HEADER_LEN`] bytes of `header`.
pub(super) fn base_of(header: &[u8]) -> Vec<u8> { header[..header.len().min(BASE_HEADER_LEN)].to_vec() }

/// Splits the `count` consecutive headers that a `blockchain.block.headers` answer served as `bytes` into their base
/// headers. The headers are cut at an even stride, `bytes.len() / count` (the stride of the header-list rule of
/// CRD ch.37); `None` when `count` is zero, the bytes do not divide by it, or the stride is shorter than a base header.
pub(super) fn split_header_list(bytes: &[u8], count: u64) -> Option<Vec<Vec<u8>>> {
    let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
    if bytes.len() % count != 0 {
        return None;
    }
    let stride = bytes.len() / count;
    if stride < BASE_HEADER_LEN {
        return None;
    }
    Some(bytes.chunks(stride).map(base_of).collect())
}

impl ChainView {
    pub(super) fn new(tolerance: u64) -> ChainView {
        ChainView {
            window: window_of(tolerance),
            headers: BTreeMap::new(),
        }
    }

    pub(super) fn highest(&self) -> Option<u64> { self.headers.keys().next_back().copied() }

    pub(super) fn lowest(&self) -> Option<u64> { self.headers.keys().next().copied() }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> BTreeMap<u64, Vec<u8>> { self.headers.clone() }

    #[cfg(test)]
    pub(super) fn heights(&self) -> Vec<u64> { self.headers.keys().copied().collect() }

    /// What to do when a connection reports the tip `height`.
    pub(super) fn plan(&self, height: u64) -> TipPlan {
        let Some(highest) = self.highest() else {
            return TipPlan::Restart;
        };
        if height <= highest {
            return TipPlan::Nothing;
        }
        if height - highest > self.window {
            return TipPlan::Restart;
        }
        TipPlan::Request {
            from: highest,
            count: height - highest,
        }
    }

    /// Starts the view again at `height` with `base`.
    pub(super) fn restart(&mut self, height: u64, base: Vec<u8>) {
        self.headers.clear();
        self.headers.insert(height, base);
    }

    /// The lowest height among `headers` (consecutive, the first at `from`) at which the view holds another base
    /// header, if any.
    pub(super) fn lowest_difference(&self, from: u64, headers: &[Vec<u8>]) -> Option<u64> {
        headers.iter().enumerate().find_map(|(offset, header)| {
            let height = from + offset as u64;
            match self.headers.get(&height) {
                Some(held) if !same_block(held, header) => Some(height),
                _ => None,
            }
        })
    }

    /// Takes `headers` (consecutive, the first at `from`) into the view: the chain only grew. Drops the oldest heights
    /// beyond the window.
    pub(super) fn extend(&mut self, from: u64, headers: Vec<Vec<u8>>) {
        for (offset, header) in headers.into_iter().enumerate() {
            self.headers.entry(from + offset as u64).or_insert(header);
        }
        self.prune();
    }

    /// Replaces the view by `headers` (consecutive, the first at `from`, the last the new tip's), the chain having
    /// changed. Every change is a reorganization, a change back to a chain the view held before included.
    pub(super) fn replace(&mut self, from: u64, headers: Vec<Vec<u8>>) {
        self.headers.clear();
        for (offset, header) in headers.into_iter().enumerate() {
            self.headers.insert(from + offset as u64, header);
        }
        self.prune();
    }

    fn prune(&mut self) {
        let Some(highest) = self.highest() else { return };
        // `window` heights: the highest and the `window - 1` below it.
        let floor = (highest + 1).saturating_sub(self.window);
        self.headers = self.headers.split_off(&floor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(tag: u8) -> Vec<u8> { vec![tag; BASE_HEADER_LEN] }

    #[test]
    fn the_window_is_two_t_plus_two_heights() {
        assert_eq!(window_of(1), 4);
        assert_eq!(window_of(3), 8);
        let mut view = ChainView::new(1);
        view.restart(10, header(1));
        for height in 11..=20 {
            view.extend(height, vec![header(1)]);
        }
        assert_eq!(view.heights(), vec![17, 18, 19, 20]);
    }

    #[test]
    fn the_plan_follows_the_highest_height() {
        let mut view = ChainView::new(3);
        assert_eq!(view.plan(100), TipPlan::Restart);
        view.restart(100, header(1));
        assert_eq!(view.plan(100), TipPlan::Nothing);
        assert_eq!(view.plan(99), TipPlan::Nothing);
        assert_eq!(view.plan(101), TipPlan::Request { from: 100, count: 1 });
        assert_eq!(view.plan(104), TipPlan::Request { from: 100, count: 4 });
        assert_eq!(view.plan(108), TipPlan::Request { from: 100, count: 8 });
        assert_eq!(view.plan(109), TipPlan::Restart);
    }

    #[test]
    fn a_header_list_is_cut_at_an_even_stride() {
        let mut bytes = vec![1u8; 100];
        bytes.extend(vec![2u8; 100]);
        let list = split_header_list(&bytes, 2).unwrap();
        assert_eq!(list, vec![vec![1u8; 80], vec![2u8; 80]]);
        assert!(split_header_list(&bytes, 3).is_none());
        assert!(split_header_list(&bytes, 0).is_none());
        assert!(
            split_header_list(&[1u8; 150], 2).is_none(),
            "a stride below the base header"
        );
    }

    #[test]
    fn a_replacement_holds_the_new_chain_and_a_change_back_is_a_replacement_too() {
        let mut view = ChainView::new(1);
        view.restart(10, header(1));
        view.extend(11, vec![header(1)]);
        // Chain B replaces 10 and 11 and adds 12.
        view.replace(10, vec![header(2), header(2), header(2)]);
        assert_eq!(view.snapshot().get(&12), Some(&header(2)));
        // Chain A comes back at 10: the view holds it, as for any other change.
        assert_eq!(
            view.lowest_difference(10, &[header(1), header(1), header(1), header(1)]),
            Some(10)
        );
        view.replace(10, vec![header(1), header(1), header(1), header(1)]);
        assert_eq!(view.heights(), vec![10, 11, 12, 13]);
        assert_eq!(
            view.lowest_difference(10, &[header(1), header(1), header(1), header(1)]),
            None
        );
    }

    #[test]
    fn a_replacement_keeps_only_the_window() {
        let mut view = ChainView::new(1);
        view.restart(10, header(1));
        view.replace(10, vec![header(2); 10]);
        assert_eq!(view.heights(), vec![16, 17, 18, 19]);
    }

    #[test]
    fn the_lowest_difference_ignores_the_bytes_after_the_base_header() {
        let mut view = ChainView::new(3);
        let mut with_proof = header(5);
        with_proof.extend([9u8; 40]);
        view.restart(10, with_proof);
        view.extend(11, vec![header(6)]);
        assert_eq!(view.lowest_difference(10, &[header(5), header(6)]), None);
        assert_eq!(view.lowest_difference(10, &[header(5), header(7)]), Some(11));
        assert_eq!(view.lowest_difference(10, &[header(8), header(7)]), Some(10));
        // A height the view does not hold is no difference.
        assert_eq!(view.lowest_difference(12, &[header(8)]), None);
    }
}
