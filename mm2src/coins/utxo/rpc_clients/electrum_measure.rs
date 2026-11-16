//! The measured server order of a coin's Electrum pool (CRD ch.38 R38.6.4l).
//!
//! This module holds the decisions as pure functions of figures and of explicit times: which measured
//! servers are eligible, the `mixed` weights, the two orders, the smoothed latency, the poor-connection
//! tests, the replacement in the rank, and the staleness decisions of R38.6.4o (the tolerance, the agreed
//! tip, the out-of-date state of a connection and the re-measure backoff). The pool (`electrum_pool.rs`)
//! gathers the figures, calls these and acts on the result. Every random choice takes a [`Draw`], a source of numbers in `[0, 1)`, so tests
//! inject the draw instead of seeding a generator.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::time::Duration;

/// A source of uniform numbers in `[0, 1)`.
pub(super) type Draw<'a> = &'a mut dyn FnMut() -> f64;

/// Which of the two measured values of `electrum_server_order` is in force (R38.6.4l item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Measuring {
    /// Weighted draw: half by measured quality, half by chance.
    Mixed,
    /// Measured quality alone; the draw only breaks ties.
    Measured,
}

/// The weight of the smoothed latency's newest sample (R38.6.4l item 2).
const SMOOTHING: f64 = 0.2;
/// Samples a connection needs before P2 may judge it (R38.6.4l item 6).
const MIN_SAMPLES: u32 = 5;
/// A server is eligible when its latency is at most this many times the best latency (item 4), ...
const ELIGIBLE_LATENCY_FACTOR: f64 = 4.;
/// ... or when it is below the floor of [`crate::utxo::rpc_clients::ElectrumPoolTimings::latency_floor`].
/// The window of the tolerance of R38.6.4o item 1: how long a server may be behind the others, in seconds.
const TOLERANCE_WINDOW_SECS: f64 = 180.;
/// The most blocks the tolerance of R38.6.4o item 1 can be.
const TOLERANCE_MAX_BLOCKS: u64 = 20;
/// P1 with one maintained connection: the limit is this many block intervals (item 6).
const STALE_BLOCK_INTERVALS: u32 = 3;
/// The stuck period of a connection among several is this many block intervals, at least the floor (R38.6.4o item 4,
/// amendment of 2026-11-15).
const STUCK_BLOCK_INTERVALS: u32 = 2;
/// The most heights the record of first reports holds (R38.6.4o item 4, amendment of 2026-11-15): a safety net above
/// the bound of `2 * tolerance + 1` (at most 41) that the pruning keeps. The lowest are dropped first.
pub(super) const MAX_FIRST_REPORTS: usize = 64;
/// P2: the smoothed latency is slow above this many times the connection's own reference latency ...
const SLOW_OWN_FACTOR: f64 = 4.;
/// ... and above this many times the best latency of the activation round (item 6).
const SLOW_BEST_FACTOR: f64 = 2.;

/// One server's figures from a measurement round (R38.6.4l item 2).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Measurement {
    pub id: usize,
    pub latency_ms: f64,
    pub height: u64,
    /// The header bytes the server answered at its height, when the answer carried a readable header (R38.6.4o
    /// item 7).
    pub header: Option<Vec<u8>>,
    /// Whether the server was suspended when it was measured (D38.6.4l.8). The release that follows a
    /// measurement does not count.
    pub suspended: bool,
}

/// One server as a round sees it: its measurement, if it has one, and the marks that bar it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RoundServer {
    pub id: usize,
    pub removed: bool,
    pub poor_demoted: bool,
    pub measurement: Option<Measurement>,
}

/// An eligible server and its quality (R38.6.4l item 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Eligible {
    pub id: usize,
    pub quality: f64,
}

/// The coin's tolerance *T* in blocks (CRD ch.38 R38.6.4o item 1): ceil(180 s / block interval), at least 1 and
/// at most 20. A coin without a block interval has *T* = 1.
pub(super) fn tolerance_blocks(block_interval: Option<Duration>) -> u64 {
    let Some(interval) = block_interval else {
        return 1;
    };
    let blocks = (TOLERANCE_WINDOW_SECS / interval.as_secs_f64()).ceil();
    // A float-to-integer cast saturates, so even a nonsensical interval lands inside the bounds.
    (blocks as u64).clamp(1, TOLERANCE_MAX_BLOCKS)
}

/// The agreed tip of a set of heights, one per server (R38.6.4o item 2): with 3 or more heights the
/// second-highest, with exactly 2 the higher, with 1 or none there is none (D38.6.4o.2).
pub(super) fn agreed_tip(heights: &[u64]) -> Option<u64> {
    let mut sorted = heights.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    match sorted.len() {
        0 | 1 => None,
        2 => sorted.first().copied(),
        _ => sorted.get(1).copied(),
    }
}

/// A server's lag: the agreed tip minus its height, or 0 when its height is at or above the agreed tip.
pub(super) fn lag(agreed_tip: u64, height: u64) -> u64 { agreed_tip.saturating_sub(height) }

/// A server's lead: its height minus the agreed tip, or 0 when its height is at or below the agreed tip (R38.6.4o
/// item 2, D38.6.4o.9).
pub(super) fn lead(agreed_tip: u64, height: u64) -> u64 { height.saturating_sub(agreed_tip) }

/// The lag as `electrum_servers` reports it: negative for a server above the agreed tip, its lead negated (R38.6.4o
/// item 10). `None` when the distance does not fit.
pub(super) fn signed_lag(agreed_tip: u64, height: u64) -> Option<i64> {
    if height > agreed_tip {
        i64::try_from(lead(agreed_tip, height)).ok().map(|lead| -lead)
    } else {
        i64::try_from(lag(agreed_tip, height)).ok()
    }
}

/// Whether `height` is more than `tolerance` blocks below the agreed tip. Without an agreed tip no server is judged
/// by height.
pub(super) fn is_beyond_tolerance(agreed_tip: Option<u64>, height: u64, tolerance: u64) -> bool {
    agreed_tip.map_or(false, |tip| lag(tip, height) > tolerance)
}

/// Whether `height` is more than `tolerance` blocks below or more than `tolerance` blocks above the agreed tip
/// (R38.6.4l item 4, R38.6.4o items 2 and 4, D38.6.4o.9).
pub(super) fn is_outside_tolerance(agreed_tip: Option<u64>, height: u64, tolerance: u64) -> bool {
    agreed_tip.map_or(false, |tip| {
        lag(tip, height) > tolerance || lead(tip, height) > tolerance
    })
}

/// Why a maintained connection is out of date (R38.6.4o item 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum OutOfDate {
    /// Its lag to the running agreed tip is more than the tolerance.
    Behind { lag: u64, tip: u64 },
    /// Its lead over the running agreed tip is more than the tolerance (D38.6.4o.9).
    Ahead { lead: u64, tip: u64 },
    /// Another connection reported `height`, the lowest height above its own, longer than the stuck period ago
    /// (`reported_secs`).
    Stuck { height: u64, reported_secs: f64 },
}

/// The state of an out-of-date connection without its figures: what a change of state compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OutOfDateKind {
    Behind,
    Ahead,
    Stuck,
}

impl OutOfDate {
    pub(super) fn kind(&self) -> OutOfDateKind {
        match self {
            OutOfDate::Behind { .. } => OutOfDateKind::Behind,
            OutOfDate::Ahead { .. } => OutOfDateKind::Ahead,
            OutOfDate::Stuck { .. } => OutOfDateKind::Stuck,
        }
    }
}

impl OutOfDateKind {
    /// The word `electrum_servers` reports (R38.6.4o item 10).
    pub(super) fn as_str(self) -> &'static str {
        match self {
            OutOfDateKind::Behind => "behind",
            OutOfDateKind::Ahead => "ahead",
            OutOfDateKind::Stuck => "stuck",
        }
    }
}

/// The stuck period of a connection among several maintained connections (R38.6.4o item 4, amendment of
/// 2026-11-15): the larger of 2 block intervals and `floor` (180 s). Without a block interval, `no_interval`
/// (10 minutes).
pub(super) fn stuck_period(block_interval: Option<Duration>, floor: Duration, no_interval: Duration) -> Duration {
    block_interval.map_or(no_interval, |interval| (interval * STUCK_BLOCK_INTERVALS).max(floor))
}

/// Records the moment each height was first reported by a maintained connection (R38.6.4o item 4, amendment of
/// 2026-11-15). `heights` are the current heights of the maintained connections, `reports` the record. Only the
/// heights within `tolerance` of the running agreed tip of `heights` are kept: above the agreed tip minus `tolerance`
/// and up to the agreed tip plus `tolerance`. A connection further behind is behind, which wins over stuck, and higher
/// heights do not count. So the record holds at most `2 * tolerance + 1` heights, however long a connection stays
/// behind or whatever a server reports. A height above the lowest of `heights` that the record lacks is entered with
/// `now`; the heights at or below the lowest are dropped. Nothing changes when fewer than two connections have a
/// height (there is no agreed tip). [`MAX_FIRST_REPORTS`] is a safety net only; it drops the lowest heights.
pub(super) fn note_first_reports(reports: &mut BTreeMap<u64, f64>, heights: &[u64], tolerance: u64, now: f64) {
    let Some(agreed) = agreed_tip(heights) else {
        return;
    };
    let lowest = heights.iter().copied().min().unwrap_or(agreed);
    // Kept: `above` < height <= `up_to`.
    let above = agreed.saturating_sub(tolerance).max(lowest);
    let up_to = agreed.saturating_add(tolerance);
    for height in heights.iter().filter(|height| **height > above && **height <= up_to) {
        reports.entry(*height).or_insert(now);
    }
    reports.retain(|height, _| *height > above && *height <= up_to);
    while reports.len() > MAX_FIRST_REPORTS {
        reports.pop_first();
    }
}

/// For each maintained connection, the lowest height above its own that another maintained connection reported,
/// with the moment it was first reported (R38.6.4o item 4, amendment of 2026-11-15). `None` for a connection without
/// a height, when fewer than two connections have a height, and when no height above its own was reported. A height
/// above the agreed tip by more than `tolerance` (a server that may report a height that does not exist, item 2)
/// and a height that no connection has now do not count.
pub(super) fn first_reports_above(
    heights: &[Option<u64>],
    reports: &BTreeMap<u64, f64>,
    tolerance: u64,
) -> Vec<Option<(u64, f64)>> {
    let known: Vec<u64> = heights.iter().flatten().copied().collect();
    let (Some(agreed), Some(highest)) = (agreed_tip(&known), known.iter().copied().max()) else {
        return vec![None; heights.len()];
    };
    let upper = highest.min(agreed.saturating_add(tolerance));
    heights
        .iter()
        .map(|height| {
            let height = (*height)?;
            if upper <= height {
                return None;
            }
            reports
                .range((Bound::Excluded(height), Bound::Included(upper)))
                .next()
                .map(|(reported, at)| (*reported, *at))
        })
        .collect()
}

/// The out-of-date state of each maintained connection (CRD ch.38 R38.6.4o item 4). `heights` holds one entry per
/// maintained connection, `None` for one without a height, which no judgment applies to. The running agreed tip is
/// the agreed tip of the heights given. A connection is *behind* when its lag to it exceeds `tolerance`, and
/// *ahead* when its lead over it exceeds `tolerance` (D38.6.4o.9), and *stuck* when there are two or more
/// connections with a height and another one has reported a height above its own for longer than `stuck_after`,
/// counted from the first report of the lowest such height (`first_reports`, amendment of 2026-11-15); when no other
/// connection has a higher height, none is stuck. Behind and ahead win over stuck.
pub(super) fn out_of_date_states(
    heights: &[Option<u64>],
    first_reports: &BTreeMap<u64, f64>,
    tolerance: u64,
    stuck_after: Duration,
    now: f64,
) -> Vec<Option<OutOfDate>> {
    let known: Vec<u64> = heights.iter().flatten().copied().collect();
    let agreed = agreed_tip(&known);
    let stuck_after = stuck_after.as_secs_f64();
    let reported = first_reports_above(heights, first_reports, tolerance);
    heights
        .iter()
        .zip(reported)
        .map(|(height, reported)| {
            let height = (*height)?;
            if let Some(tip) = agreed {
                let behind = lag(tip, height);
                if behind > tolerance {
                    return Some(OutOfDate::Behind { lag: behind, tip });
                }
                let ahead = lead(tip, height);
                if ahead > tolerance {
                    return Some(OutOfDate::Ahead { lead: ahead, tip });
                }
            }
            let (reported_height, at) = reported?;
            let since = now - at;
            (since > stuck_after).then_some(OutOfDate::Stuck {
                height: reported_height,
                reported_secs: since,
            })
        })
        .collect()
}

/// The heights of a measurement round, one per server: those measured in the round, and for every other server the
/// height its maintained connection currently reports (`reported`, by server id) (R38.6.4l item 4, D38.6.4l.3).
fn round_heights(round: &[RoundServer], reported: &[(usize, u64)]) -> Vec<u64> {
    let mut heights: Vec<(usize, u64)> = round
        .iter()
        .filter_map(|server| server.measurement.as_ref().map(|m| (server.id, m.height)))
        .collect();
    for (id, height) in reported {
        if !heights.iter().any(|(known, _)| known == id) {
            heights.push((*id, *height));
        }
    }
    heights.into_iter().map(|(_, height)| height).collect()
}

/// The agreed tip of a measurement round (R38.6.4o item 2), over `round_heights`.
pub(super) fn round_agreed_tip(round: &[RoundServer], reported: &[(usize, u64)]) -> Option<u64> {
    agreed_tip(&round_heights(round, reported))
}

/// The pairs of servers measured at the same height that answered different base headers there ([`same_block`]),
/// with that height (CRD ch.38 R38.6.4o item 7): each starts a comparison (item 12) once both report heights at least
/// *T* above it.
pub(super) fn header_differences(round: &[RoundServer]) -> Vec<(usize, usize, u64)> {
    let measured: Vec<(usize, u64, &[u8])> = round
        .iter()
        .filter_map(|server| {
            let m = server.measurement.as_ref()?;
            Some((server.id, m.height, m.header.as_deref()?))
        })
        .collect();
    let mut pairs = Vec::new();
    for (i, (id_a, height_a, header_a)) in measured.iter().enumerate() {
        for (id_b, height_b, header_b) in measured.iter().skip(i + 1) {
            if height_a == height_b && !same_block(header_a, header_b) {
                pairs.push((*id_a, *id_b, *height_a));
            }
        }
    }
    pairs
}

/// The common height at which two servers are compared (CRD ch.38 R38.6.4o item 12): the lower of their heights
/// minus the tolerance, not below 0, so that a race at the tip never counts.
pub(super) fn common_height(height_a: u64, height_b: u64, tolerance: u64) -> u64 {
    height_a.min(height_b).saturating_sub(tolerance)
}

/// The length of the base header that every UTXO header family starts with: version, previous block hash, merkle
/// root, and the fields that follow up to 80 bytes.
pub(super) const BASE_HEADER_LEN: usize = 80;

/// Whether two headers at one height name the same block (CRD ch.38 R38.6.4o items 7 and 12): their first
/// [`BASE_HEADER_LEN`] bytes are equal. The bytes after them are not compared: on merged-mining (AuxPoW) coins they
/// carry the merged-mining proof, which the block hash does not cover, so two servers can store one block with
/// different proof bytes.
pub(super) fn same_block(header_a: &[u8], header_b: &[u8]) -> bool {
    header_a[..header_a.len().min(BASE_HEADER_LEN)] == header_b[..header_b.len().min(BASE_HEADER_LEN)]
}

/// Whether, with three or more heights, one of them has a lead over the agreed tip of more than `tolerance`
/// (R38.6.4o item 12, *far ahead*).
pub(super) fn is_far_ahead(heights: &[u64], tolerance: u64) -> bool {
    heights.len() >= 3
        && agreed_tip(heights).map_or(false, |tip| heights.iter().any(|height| lead(tip, *height) > tolerance))
}

/// The eligible servers of a round, in the order of `round`. `reported` holds the heights the maintained
/// connections currently report, by server id (D38.6.4l.3); `tolerance` is the coin's *T* (R38.6.4o item 1);
/// `latency_floor_ms` is the 300 ms floor. The best tip is the agreed tip (R38.6.4o item 2) over the heights of the
/// round and `reported`. A difference in the header at the same height does not exclude a server: it starts a
/// comparison (R38.6.4o item 7, [`header_differences`]).
pub(super) fn eligible_servers(
    round: &[RoundServer],
    reported: &[(usize, u64)],
    tolerance: u64,
    latency_floor_ms: f64,
) -> Vec<Eligible> {
    let measured = || round.iter().filter_map(|server| server.measurement.as_ref());
    let best_tip = round_agreed_tip(round, reported);
    let best_latency = measured().map(|m| m.latency_ms).fold(f64::INFINITY, f64::min);
    if !best_latency.is_finite() {
        return Vec::new();
    }
    let latency_limit = (ELIGIBLE_LATENCY_FACTOR * best_latency).max(latency_floor_ms);
    round
        .iter()
        .filter_map(|server| {
            let m = server.measurement.as_ref()?;
            let eligible = !server.removed
                && !server.poor_demoted
                && !m.suspended
                && !is_outside_tolerance(best_tip, m.height, tolerance)
                && m.latency_ms <= latency_limit;
            eligible.then(|| Eligible {
                id: server.id,
                quality: best_latency.max(f64::MIN_POSITIVE) / m.latency_ms.max(f64::MIN_POSITIVE),
            })
        })
        .collect()
}

/// The weights of the `mixed` draw: `0.5 * q / sum(q) + 0.5 / n`, computed once over the `n` eligible servers.
pub(super) fn mixed_weights(eligible: &[Eligible]) -> Vec<f64> {
    let n = eligible.len() as f64;
    let sum: f64 = eligible.iter().map(|server| server.quality).sum();
    eligible
        .iter()
        .map(|server| 0.5 * server.quality / sum + 0.5 / n)
        .collect()
}

/// A uniform shuffle of `items` (Fisher-Yates), taking one number from `draw` per step.
pub(super) fn shuffle(items: &mut [usize], draw: Draw) {
    for i in (1..items.len()).rev() {
        let j = ((draw() * (i + 1) as f64) as usize).min(i);
        items.swap(i, j);
    }
}

/// A weighted draw without replacement: each place goes to one of the items not yet placed, with probability
/// proportional to its weight; the weights are not recomputed. One number from `draw` per place.
fn weighted_order(mut items: Vec<(usize, f64)>, draw: Draw) -> Vec<usize> {
    let mut order = Vec::with_capacity(items.len());
    while !items.is_empty() {
        let total: f64 = items.iter().map(|(_, weight)| weight).sum();
        let target = draw() * total;
        let mut cumulative = 0.;
        let mut pick = items.len() - 1;
        for (index, (_, weight)) in items.iter().enumerate() {
            cumulative += weight;
            if target < cumulative {
                pick = index;
                break;
            }
        }
        order.push(items.remove(pick).0);
    }
    order
}

/// The eligible servers by descending quality; servers of equal quality are in a uniform random order.
fn quality_order(eligible: &[Eligible], draw: Draw) -> Vec<usize> {
    let mut sorted = eligible.to_vec();
    sorted.sort_by(|a, b| b.quality.total_cmp(&a.quality));
    let mut order = Vec::with_capacity(sorted.len());
    let mut start = 0;
    while start < sorted.len() {
        let end = sorted[start..]
            .iter()
            .position(|server| server.quality != sorted[start].quality)
            .map_or(sorted.len(), |offset| start + offset);
        let mut tied: Vec<usize> = sorted[start..end].iter().map(|server| server.id).collect();
        shuffle(&mut tied, draw);
        order.extend(tied);
        start = end;
    }
    order
}

/// The eligible servers in the order of `mode` (R38.6.4l item 5).
fn eligible_order(mode: Measuring, eligible: &[Eligible], draw: Draw) -> Vec<usize> {
    match mode {
        Measuring::Mixed => {
            let weights = mixed_weights(eligible);
            weighted_order(eligible.iter().map(|server| server.id).zip(weights).collect(), draw)
        },
        Measuring::Measured => quality_order(eligible, draw),
    }
}

/// The rank after a measurement round: the eligible servers in the order of `mode`, then every other server of
/// `all` in a uniform random order (R38.6.4l item 5).
pub(super) fn rank_after_round(mode: Measuring, eligible: &[Eligible], all: &[usize], draw: Draw) -> Vec<usize> {
    let mut rank = eligible_order(mode, eligible, draw);
    let mut others: Vec<usize> = all.iter().copied().filter(|id| !rank.contains(id)).collect();
    shuffle(&mut others, draw);
    rank.extend(others);
    rank
}

/// The replacement for a poor connection: the first place of the order of `mode` over the eligible servers
/// (R38.6.4l item 7 step 3). `None` when no server is eligible.
pub(super) fn pick_replacement(mode: Measuring, eligible: &[Eligible], draw: Draw) -> Option<usize> {
    eligible_order(mode, eligible, draw).first().copied()
}

/// The rank after `replacement` took the place of `poor`: `poor` moves to the end and the other servers keep
/// their order (R38.6.4l item 7 step 3, D38.6.4l.7). `rank` lists the server ids from the highest rank down.
pub(super) fn rank_with_replacement(rank: &[usize], poor: usize, replacement: usize) -> Vec<usize> {
    let mut replaced = Vec::with_capacity(rank.len());
    for id in rank {
        if *id == replacement {
            continue;
        }
        replaced.push(if *id == poor { replacement } else { *id });
    }
    replaced.push(poor);
    replaced
}

/// The smoothed latency of a maintained connection and the number of samples behind it (R38.6.4l item 2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Smoothed {
    samples: u32,
    value_ms: Option<f64>,
}

impl Smoothed {
    /// The first sample starts the average; every later one *s* makes it `0.2 * s + 0.8 * previous`.
    pub(super) fn record(&mut self, sample_ms: f64) {
        self.value_ms = Some(match self.value_ms {
            None => sample_ms,
            Some(previous) => SMOOTHING * sample_ms + (1. - SMOOTHING) * previous,
        });
        self.samples = self.samples.saturating_add(1);
    }

    pub(super) fn value_ms(&self) -> Option<f64> { self.value_ms }

    pub(super) fn samples(&self) -> u32 { self.samples }
}

/// The default of `electrum_spread_near_factor` (CRD ch.38 R38.6.4a item 8, D38.6.4a.1). `0` would turn the near
/// sessions off. This is the only place that holds the default.
pub(super) const NEAR_FACTOR_DEFAULT: f64 = 2.0;
/// The default of `electrum_spread_near_offset_ms`, the offset *A* of R38.6.4a item 6 (D38.6.4a.1). This is the only
/// place that holds the default.
pub(super) const NEAR_OFFSET_DEFAULT_MS: f64 = 20.0;

/// Which of the maintained sessions are near (CRD ch.38 R38.6.4a item 6). `figures` holds the smoothed latency
/// of each maintained session in priority order, `None` for one that has none yet; the answer holds one flag per
/// figure, in the same order. With *f* the lowest figure, a session is near when its figure is at most the larger
/// of `factor` x *f* and *f* + `offset_ms` (the form of the limit is D38.6.4a.2, and this line is the only place
/// that holds it). A session without a figure is not near while another session has one: it has answered nothing
/// yet and takes no read before it has answered once (D38.6.4a.4). Without any figure, and with a `factor` of `0`,
/// every session is near. A `factor` above `0` and below `1` acts as `1`: `factor` x *f* is then below *f* + `offset_ms`, which
/// decides.
///
/// The floor of two (R38.6.4a item 6, D38.6.4a.3): when two or more sessions are given and the limit leaves fewer
/// than two of them near, the session with the next-lowest figure after the fastest is near as well, and between
/// equal figures the one earlier in the list, which is the priority order. Only a session with a figure is picked. A coin with two sessions therefore
/// always spreads over both, and one with more over at least its two fastest. This too is held here only.
pub(super) fn near_sessions(figures: &[Option<f64>], factor: f64, offset_ms: f64) -> Vec<bool> {
    let fastest = figures.iter().flatten().copied().reduce(f64::min);
    let Some(fastest) = fastest.filter(|_| factor > 0.) else {
        return vec![true; figures.len()];
    };
    let limit = (factor * fastest).max(fastest + offset_ms);
    let mut near: Vec<bool> = figures
        .iter()
        .map(|figure| figure.map_or(false, |figure| figure <= limit))
        .collect();
    if near.len() >= 2 && near.iter().filter(|near| **near).count() < 2 {
        // Only the fastest is near. The next-lowest figure among the other sessions, if any has one, is near too;
        // `min_by` keeps the first of equal figures, which is the earlier one in priority order.
        let next = figures
            .iter()
            .enumerate()
            .filter(|(i, _)| !near[*i])
            .filter_map(|(i, figure)| figure.map(|figure| (i, figure)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        if let Some(i) = next {
            near[i] = true;
        }
    }
    near
}

/// P2 (R38.6.4l item 6): after at least 5 samples, the smoothed latency exceeds all three of 4 times the
/// connection's own `reference_ms`, 2 times the `best_activation_ms` and `slow_floor_ms`. A connection with no
/// reference, or a pool with no activation figure, is not judged (D38.6.4l.5).
pub(super) fn is_slow(
    smoothed: &Smoothed,
    reference_ms: Option<f64>,
    best_activation_ms: Option<f64>,
    slow_floor_ms: f64,
) -> bool {
    let (Some(value), Some(reference), Some(best)) = (smoothed.value_ms, reference_ms, best_activation_ms) else {
        return false;
    };
    smoothed.samples >= MIN_SAMPLES
        && value > SLOW_OWN_FACTOR * reference
        && value > SLOW_BEST_FACTOR * best
        && value > slow_floor_ms
}

/// P1 with one maintained connection: the time without a header notification that makes it stale, the larger
/// of 3 block intervals and `floor` (10 minutes). Without a block interval, `floor`.
pub(super) fn stale_limit(block_interval: Option<Duration>, floor: Duration) -> Duration {
    block_interval.map_or(floor, |interval| (interval * STALE_BLOCK_INTERVALS).max(floor))
}

/// The coin's block interval from its `avg_blocktime`, if that is a positive number (R38.6.4l item 6).
pub(super) fn block_interval_from_conf(conf: &serde_json::Value) -> Option<Duration> {
    let secs = conf["avg_blocktime"].as_f64()?;
    (secs.is_finite() && secs > 0. && secs < u32::MAX as f64).then(|| Duration::from_secs_f64(secs))
}

/// When the next re-measure round may start: `limit` after the last one started (R38.6.4l item 7 step 2).
pub(super) fn next_round_at(last_round_started: Option<f64>, limit: Duration) -> f64 {
    last_round_started.map_or(0., |started| started + limit.as_secs_f64())
}

/// The least time between two re-measure rounds after `level` rounds without a replacement (R38.6.4o item 8): `base`
/// doubled once per level, never more than `cap`. With the production figures of 5 minutes and 60 minutes the levels
/// 0, 1, 2, 3 and 4 or more give 5, 10, 20, 40 and 60 minutes.
pub(super) fn remeasure_gap(base: Duration, cap: Duration, level: u32) -> Duration {
    let factor = 2u32.checked_pow(level.min(30)).unwrap_or(u32::MAX);
    base.checked_mul(factor).map_or(cap, |gap| gap.min(cap)).max(base)
}

/// Why a poor connection was marked (R38.6.4o item 9, the reason of a replacement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PoorReason {
    Behind,
    Stuck,
    Slow,
}

impl PoorReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            PoorReason::Behind => "behind",
            PoorReason::Stuck => "stuck",
            PoorReason::Slow => "slow",
        }
    }
}

/// The per-server figures the measured order keeps (R38.6.4l items 2 and 6).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ServerQuality {
    /// The latency of the activation round.
    pub activation_ms: Option<f64>,
    /// The latency of the re-measure round in which the server was drawn as a replacement.
    pub round_ms: Option<f64>,
    /// Since the connection joined the maintained set.
    pub smoothed: Smoothed,
    /// When the connection joined, or when it last received a header notification (seconds since the epoch).
    pub notified_at: f64,
    /// Consecutive header notifications at which the connection lagged behind the best height.
    pub lag_strikes: u32,
    /// The poor-demotion mark lasts until this time (seconds since the epoch).
    pub poor_until: f64,
    /// Why the connection was last marked poor.
    pub poor_reason: Option<PoorReason>,
    /// The out-of-date state last reported in the log, so that a line is logged once per change (R38.6.4o item 9).
    pub reported_state: Option<OutOfDateKind>,
}

impl ServerQuality {
    /// The reference latency of P2: the activation latency, or without one the re-measure latency.
    pub(super) fn reference_ms(&self) -> Option<f64> { self.activation_ms.or(self.round_ms) }

    /// The counters that run "since joining" start again.
    pub(super) fn on_join(&mut self, now: f64) {
        self.smoothed = Smoothed::default();
        self.notified_at = now;
        self.lag_strikes = 0;
        self.reported_state = None;
    }

    pub(super) fn is_poor_demoted(&self, now: f64) -> bool { now < self.poor_until }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: f64 = 300.;

    /// A draw that returns the given numbers in turn, then `0.5`.
    fn scripted(numbers: &[f64]) -> impl FnMut() -> f64 {
        let mut numbers = numbers.to_vec().into_iter();
        move || numbers.next().unwrap_or(0.5)
    }

    fn server(id: usize, latency_ms: f64, height: u64) -> RoundServer {
        RoundServer {
            id,
            removed: false,
            poor_demoted: false,
            measurement: Some(Measurement {
                id,
                latency_ms,
                height,
                header: None,
                suspended: false,
            }),
        }
    }

    fn eligible_ids(round: &[RoundServer]) -> Vec<usize> {
        eligible_servers(round, &[], 1, FLOOR).iter().map(|e| e.id).collect()
    }

    fn near_with_offset(figures: &[f64], factor: f64, offset_ms: f64) -> Vec<bool> {
        near_sessions(
            &figures.iter().map(|figure| Some(*figure)).collect::<Vec<_>>(),
            factor,
            offset_ms,
        )
    }

    /// The chapter's cases are stated with the offset of 20 ms.
    fn near_for(figures: &[f64], factor: f64) -> Vec<bool> { near_with_offset(figures, factor, 20.) }

    const RIN_FIGURES: [f64; 6] = [25., 27., 32., 108., 230., 266.];

    /// T38.6.4a.1: the near sessions from the figures of the chapter.
    #[test]
    fn t38_6_4a_1_the_near_sessions_follow_the_factor_and_the_margin() {
        // F = 2: the limit is the larger of 50 ms and 45 ms, so exactly the first three stay.
        assert_eq!(near_for(&RIN_FIGURES, 2.), [true, true, true, false, false, false]);
        // F = 0 turns it off.
        assert_eq!(near_for(&RIN_FIGURES, 0.), [true; 6]);
        // F = 0.5 acts as 1: the 20 ms term keeps all three of 25, 27 and 32 near, and nothing above 45 ms.
        assert_eq!(near_for(&[25., 27., 32.], 0.5), [true; 3]);
        assert_eq!(near_for(&RIN_FIGURES, 0.5), near_for(&RIN_FIGURES, 1.));
        assert_eq!(near_for(&RIN_FIGURES, 1.), [true, true, true, false, false, false]);
        // The margin edge: 45 ms is near, a hair above is not (F = 1, f = 25).
        assert_eq!(near_for(&[25., 45., 45.1], 1.), [true, true, false]);
        // The factor edge: 100 ms is near, a hair above is not (F = 2, f = 50).
        assert_eq!(near_for(&[50., 100., 100.1], 2.), [true, true, false]);
    }

    /// T38.6.4a.1 as amended by D38.6.4a.4: a session without a figure is not near while another one has one; every
    /// session is near when none has one.
    #[test]
    fn t38_6_4a_1_a_session_without_a_figure_is_not_near_while_another_has_one() {
        let mut figures: Vec<Option<f64>> = RIN_FIGURES.iter().map(|figure| Some(*figure)).collect();
        figures[5] = None;
        // A session without a figure is not near while another one has a figure (D38.6.4a.4).
        assert_eq!(near_sessions(&figures, 2., 20.), [
            true, true, true, false, false, false
        ]);
        // The lowest figure is taken among the present ones only.
        figures[0] = None;
        assert_eq!(near_sessions(&figures, 2., 20.), [
            false, true, true, false, false, false
        ]);
        assert_eq!(near_sessions(&[None, None, None], 2., 20.), [true; 3]);
        assert_eq!(near_sessions(&[None, None, None], 0., 20.), [true; 3]);
        assert!(near_sessions(&[], 2., 20.).is_empty());
    }

    /// T38.6.4a.1: the floor of two near sessions (R38.6.4a item 6, D38.6.4a.3).
    #[test]
    fn t38_6_4a_1_at_least_two_sessions_are_near() {
        // 38 and 230: the limit is 76 ms, and the floor keeps both.
        assert_eq!(near_for(&[38., 230.], 2.), [true, true]);
        // 25, 230, 266: the fastest and the next-fastest.
        assert_eq!(near_for(&[25., 230., 266.], 2.), [true, true, false]);
        // 230, 25, 230: the lowest figure, then the earlier of the two equal ones.
        assert_eq!(near_for(&[230., 25., 230.], 2.), [true, true, false]);
        // 25, 230 and none: the one without a figure is not near, so the floor picks the 230 ms one, which has one.
        assert_eq!(near_sessions(&[Some(25.), Some(230.), None], 2., 20.), [
            true, true, false
        ]);
        // One session with a figure and the others without: only it is near, and the floor has nobody to add.
        assert_eq!(near_sessions(&[None, Some(25.), None], 2., 20.), [false, true, false]);
        // With the factor at 0 every session is near, figure or not.
        assert_eq!(near_sessions(&[None, Some(25.), None], 0., 20.), [true; 3]);
        // A single session is near.
        assert_eq!(near_for(&[230.], 2.), [true]);
    }

    /// T38.6.4a.1: with the offset at 0 the factor alone decides; with 50 a session up to 50 ms behind the
    /// fastest is near.
    #[test]
    fn t38_6_4a_1_the_offset_is_a_parameter() {
        // Offset 0: F = 2 and f = 25 give 50 ms, F = 1 gives f itself (with 20 ms it would admit 45 ms).
        assert_eq!(near_with_offset(&[25., 50., 50.1], 2., 0.), [true, true, false]);
        assert_eq!(near_with_offset(&[25., 25., 25.1], 1., 0.), [true, true, false]);
        assert_eq!(near_for(&[25., 25., 25.1], 1.), [true, true, true]);
        // A factor between 0 and 1 then makes the limit admit only the fastest figure; the floor of two near
        // sessions (D38.6.4a.3) adds the next-fastest one. A factor of 0 still turns the rule off.
        assert_eq!(near_with_offset(&[25., 27., 32.], 0.5, 0.), [true, true, false]);
        assert_eq!(near_with_offset(&RIN_FIGURES, 0., 0.), [true; 6]);
        // Offset 50: f = 25 admits up to 75 ms (the factor gives 50), and 76 ms is out.
        assert_eq!(near_with_offset(&[25., 27., 32., 75., 76., 108.], 2., 50.), [
            true, true, true, true, false, false
        ]);
        // The factor still wins where it is the larger: f = 100 gives 200 ms, not 150 ms.
        assert_eq!(near_with_offset(&[100., 200., 201.], 2., 50.), [true, true, false]);
    }

    /// T38.6.4a.1: the default is one constant; with the factor at it, the figures of the chapter decide.
    #[test]
    fn t38_6_4a_1_the_default_factor_is_the_one_constant() {
        assert!(NEAR_FACTOR_DEFAULT >= 0.);
        let expected = if NEAR_FACTOR_DEFAULT == 0. {
            [true; 6]
        } else {
            [true, true, true, false, false, false]
        };
        assert_eq!(near_for(&RIN_FIGURES, NEAR_FACTOR_DEFAULT), expected);
    }

    /// T38.6.4l.3: the height rule, with the agreed tip (R38.6.4o item 2) and a tolerance of one block.
    #[test]
    fn a_server_two_blocks_behind_the_agreed_tip_is_not_eligible() {
        // H, H and H - 2: the agreed tip is H, so the third is two blocks behind.
        let round = [server(0, 50., 100), server(1, 50., 100), server(2, 50., 98)];
        assert_eq!(eligible_ids(&round), vec![0, 1]);
        // H, H - 1 and H - 2: the agreed tip is H - 1 and all three are eligible.
        let round = [server(0, 50., 100), server(1, 50., 99), server(2, 50., 98)];
        assert_eq!(eligible_ids(&round), vec![0, 1, 2]);
        // The heights a maintained connection reports count for the agreed tip, one height per server
        // (D38.6.4l.3): with 101 reported by server 7 the agreed tip is 100.
        let reported = eligible_servers(&round, &[(7, 101)], 1, FLOOR);
        assert_eq!(reported.iter().map(|e| e.id).collect::<Vec<_>>(), vec![0, 1]);
        // A server that is both measured and reported counts once, with its measured height.
        let again = eligible_servers(&round, &[(0, 150), (1, 150)], 1, FLOOR);
        assert_eq!(again.iter().map(|e| e.id).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    /// T38.6.4o item 1: the tolerance decides the limit; the cap and the floor keep it between 1 and 20.
    #[test]
    fn the_tolerance_replaces_the_fixed_block_limit() {
        let round = [server(0, 50., 100), server(1, 50., 100), server(2, 50., 97)];
        let at = |tolerance| {
            eligible_servers(&round, &[], tolerance, FLOOR)
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(at(1), vec![0, 1]);
        assert_eq!(at(2), vec![0, 1]);
        assert_eq!(at(3), vec![0, 1, 2]);
    }

    /// T38.6.4o.1: the tolerance from the block interval.
    #[test]
    fn t38_6_4o_1_the_tolerance_from_avg_blocktime() {
        let t = |value: serde_json::Value| {
            tolerance_blocks(block_interval_from_conf(&serde_json::json!({ "avg_blocktime": value })))
        };
        for (avg, expected) in [(600, 1), (150, 2), (60, 3), (15, 12), (10, 18)] {
            assert_eq!(t(serde_json::json!(avg)), expected, "avg_blocktime {avg}");
        }
        assert_eq!(t(serde_json::json!(1.5)), 20);
        // The cap.
        assert_eq!(t(serde_json::json!(0.5)), 20);
        assert_eq!(t(serde_json::json!(0.000_000_001)), 20);
        // A block time above the window, and every value that is not a positive number, give 1.
        assert_eq!(t(serde_json::json!(100_000)), 1);
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-60),
            serde_json::json!("60"),
            serde_json::json!(null),
        ] {
            assert_eq!(t(bad.clone()), 1, "{bad}");
        }
        assert_eq!(tolerance_blocks(block_interval_from_conf(&serde_json::json!({}))), 1);
        assert_eq!(tolerance_blocks(None), 1);
    }

    /// T38.6.4o.2: the agreed tip and the lag.
    #[test]
    fn t38_6_4o_2_the_agreed_tip_is_the_second_highest_height() {
        const H: u64 = 5_000_000;
        assert_eq!(agreed_tip(&[]), None);
        assert_eq!(agreed_tip(&[H]), None);
        assert_eq!(agreed_tip(&[H, H - 1000]), Some(H));
        assert_eq!(agreed_tip(&[H - 1000, H]), Some(H));
        assert_eq!(agreed_tip(&[H, H, H - 1]), Some(H));
        assert_eq!(agreed_tip(&[H + 1_000_000, H, H - 1]), Some(H));
        assert_eq!(agreed_tip(&[H + 5, H + 3, H]), Some(H + 3));
        assert_eq!(agreed_tip(&[H, H + 5, H + 3]), Some(H + 3));
        // A server above the agreed tip has lag 0; one below has the difference.
        assert_eq!(lag(H + 3, H + 5), 0);
        assert_eq!(lag(H + 3, H + 3), 0);
        assert_eq!(lag(H + 3, H), 3);
        assert!(!is_beyond_tolerance(Some(H + 3), H, 3));
        assert!(is_beyond_tolerance(Some(H + 3), H - 1, 3));
        assert!(!is_beyond_tolerance(None, 0, 1));
        // The lead is the distance above the agreed tip, 0 at or below it; the lag as reported is signed.
        assert_eq!(lead(H + 3, H + 5), 2);
        assert_eq!(lead(H + 3, H + 3), 0);
        assert_eq!(lead(H + 3, H), 0);
        assert_eq!(signed_lag(H + 3, H), Some(3));
        assert_eq!(signed_lag(H + 3, H + 5), Some(-2));
        assert_eq!(signed_lag(H, H), Some(0));
        assert_eq!(signed_lag(0, u64::MAX), None);
        // Outside the tolerance is more than T below or more than T above.
        assert!(!is_outside_tolerance(Some(H), H + 3, 3));
        assert!(is_outside_tolerance(Some(H), H + 4, 3));
        assert!(!is_outside_tolerance(Some(H), H - 3, 3));
        assert!(is_outside_tolerance(Some(H), H - 4, 3));
        assert!(!is_outside_tolerance(None, 0, 1));
    }

    fn secs(s: u64) -> Duration { Duration::from_secs(s) }

    /// The heights of connections that all have one.
    fn heights(heights: &[u64]) -> Vec<Option<u64>> { heights.iter().map(|height| Some(*height)).collect() }

    /// A record of first reports.
    fn reports(entries: &[(u64, f64)]) -> BTreeMap<u64, f64> { entries.iter().copied().collect() }

    /// The states with no first report on record, which is all that behind and ahead need.
    fn height_states(heights: &[Option<u64>], tolerance: u64) -> Vec<Option<OutOfDate>> {
        out_of_date_states(heights, &BTreeMap::new(), tolerance, secs(600), 1.)
    }

    /// T38.6.4o items 4 and 5: a connection more than the tolerance behind the running agreed tip is out of date as
    /// behind, and is up to date again when it catches up; a connection at the tolerance never is.
    #[test]
    fn a_connection_is_behind_only_beyond_the_tolerance() {
        let states = |heights_: [u64; 3]| height_states(&heights(&heights_), 3);
        assert_eq!(states([100, 100, 96]), vec![
            None,
            None,
            Some(OutOfDate::Behind { lag: 4, tip: 100 })
        ]);
        assert_eq!(states([100, 100, 97]), vec![None, None, None]);
        assert_eq!(states([100, 100, 100]), vec![None, None, None]);
        // The field case: two connections, one far behind. The agreed tip is the higher of the two.
        assert_eq!(height_states(&heights(&[2_000, 1_000]), 3), vec![
            None,
            Some(OutOfDate::Behind { lag: 1_000, tip: 2_000 })
        ]);
        // One height, or none, judges nobody; a connection without a height is never judged.
        assert_eq!(height_states(&[Some(1), None], 3), vec![None, None]);
    }

    const H: u64 = 700_000;

    /// The state of the second of two connections, at `a_height` and `b_height`, at `now`, with the first reports
    /// of `record` and a stuck period of `period` seconds.
    fn stuck_state_of_b(
        a_height: u64,
        b_height: u64,
        record: &[(u64, f64)],
        period: u64,
        now: f64,
    ) -> Option<OutOfDate> {
        out_of_date_states(&heights(&[a_height, b_height]), &reports(record), 3, secs(period), now)[1]
    }

    /// T38.6.4o item 4 (amended 2026-11-15), the slow block: A reports H + 1 at t0 and B reports it 5 s later, after
    /// 30 minutes without a block. B is never stuck: not before its report, and not after it.
    #[test]
    fn a_slow_block_does_not_make_the_second_server_stuck() {
        let t0 = 1_800.;
        // B has not reported yet, 5 s after A: the period is far from over.
        assert_eq!(stuck_state_of_b(H + 1, H, &[(H + 1, t0)], 180, t0 + 5.), None);
        assert_eq!(stuck_state_of_b(H + 1, H, &[(H + 1, t0)], 600, t0 + 5.), None);
        // B has reported: nothing is above it any more, and the record holds nothing it could be judged by.
        assert_eq!(stuck_state_of_b(H + 1, H + 1, &[], 180, t0 + 5.), None);
        assert_eq!(stuck_state_of_b(H + 1, H + 1, &[], 180, t0 + 3_600.), None);
    }

    /// T38.6.4o item 4 (amended 2026-11-15), a server that never delivers: B is not stuck at P - 1 s and is stuck at
    /// P + 1 s after A's first report, for the periods of the coins; and the stuck state names the height and the
    /// time since A reported it.
    #[test]
    fn a_server_that_never_delivers_is_stuck_after_the_period() {
        let t0 = 5_000.;
        for (interval, period) in [(Some(600.), 1_200.), (Some(60.), 180.), (Some(15.), 180.), (None, 600.)] {
            let p = stuck_period(interval.map(Duration::from_secs_f64), secs(180), secs(600));
            assert_eq!(p, Duration::from_secs_f64(period), "{interval:?}");
            let record = [(H + 1, t0)];
            assert_eq!(
                stuck_state_of_b(H + 1, H, &record, period as u64, t0 + period - 1.),
                None
            );
            // More than P, not at P.
            assert_eq!(stuck_state_of_b(H + 1, H, &record, period as u64, t0 + period), None);
            assert_eq!(
                stuck_state_of_b(H + 1, H, &record, period as u64, t0 + period + 1.),
                Some(OutOfDate::Stuck {
                    height: H + 1,
                    reported_secs: period + 1.
                })
            );
        }
        // A's own state is never stuck: nobody is above it.
        let record = [(H + 1, t0)];
        assert_eq!(
            out_of_date_states(&heights(&[H + 1, H]), &reports(&record), 3, secs(180), t0 + 1_000.)[0],
            None
        );
    }

    /// T38.6.4o item 4 (amended 2026-11-15), a configured interval much shorter than the real spacing: with an
    /// `avg_blocktime` of 60 s (P = 180 s) and blocks every 15 minutes, a second server 3 to 10 s late is never
    /// stuck, whatever the silence before the block; and the period is counted from the first report, not from the
    /// connection's own last notification.
    #[test]
    fn a_late_server_is_not_stuck_when_blocks_come_slower_than_configured() {
        let period = stuck_period(Some(secs(60)), secs(180), secs(600));
        assert_eq!(period, secs(180));
        for late in [3., 5., 10.] {
            for t0 in [900., 1_800., 2_700.] {
                // Before B's report: the block is `late` seconds old at most.
                assert_eq!(stuck_state_of_b(H + 1, H, &[(H + 1, t0)], 180, t0 + late), None);
                // After B's report: the record is empty.
                assert_eq!(stuck_state_of_b(H + 1, H + 1, &[], 180, t0 + late + 1.), None);
            }
        }
    }

    /// T38.6.4o item 4 (amended 2026-11-15), notifying without advancing: B's own notifications of H do not change
    /// the first report of H + 1, so B is stuck after the period.
    #[test]
    fn a_connection_that_notifies_without_advancing_is_stuck() {
        let mut record = BTreeMap::new();
        let t0 = 100.;
        note_first_reports(&mut record, &[H + 1, H], 3, t0);
        // B notifies H again and again.
        for repeat in 1..=5 {
            note_first_reports(&mut record, &[H + 1, H], 3, t0 + 40. * repeat as f64);
        }
        assert_eq!(record, reports(&[(H + 1, t0)]));
        assert_eq!(
            out_of_date_states(&heights(&[H + 1, H]), &record, 3, secs(180), t0 + 179.)[1],
            None
        );
        assert_eq!(
            out_of_date_states(&heights(&[H + 1, H]), &record, 3, secs(180), t0 + 181.)[1],
            Some(OutOfDate::Stuck {
                height: H + 1,
                reported_secs: 181.
            })
        );
    }

    /// T38.6.4o item 4 (amended 2026-11-15), nobody advances: all at H for an hour, none is stuck; and a single
    /// connection with a height is never stuck.
    #[test]
    fn when_nobody_advances_none_is_stuck() {
        assert_eq!(
            out_of_date_states(&heights(&[H, H, H]), &BTreeMap::new(), 3, secs(180), 3_600.),
            vec![None, None, None]
        );
        assert_eq!(
            out_of_date_states(&[Some(H), None], &reports(&[(H + 1, 0.)]), 3, secs(180), 3_600.),
            vec![None, None]
        );
        // A first report of a height that no connection has now (the connection left) judges nobody.
        assert_eq!(
            out_of_date_states(&heights(&[H, H]), &reports(&[(H + 1, 0.)]), 3, secs(180), 3_600.),
            vec![None, None]
        );
    }

    /// T38.6.4o item 4 (amended 2026-11-15): the time is counted from the first report of the lowest height above the
    /// connection's own, whichever connection reported it; a connection that is two blocks behind is judged by the
    /// older block.
    #[test]
    fn the_period_runs_from_the_first_report_of_the_lowest_height_above() {
        let record = reports(&[(H + 1, 100.), (H + 2, 400.)]);
        let states = |now: f64| out_of_date_states(&heights(&[H + 2, H + 1, H]), &record, 3, secs(180), now);
        assert_eq!(states(279.), vec![None, None, None]);
        // At 281 s the block H + 1 is 181 s old: the connection at H is stuck, the one at H + 1 is not (H + 2 is
        // only 0 s old).
        assert_eq!(states(281.), vec![
            None,
            None,
            Some(OutOfDate::Stuck {
                height: H + 1,
                reported_secs: 181.
            })
        ]);
        // At 581 s H + 2 is 181 s old, so the connection at H + 1 is stuck as well.
        assert_eq!(states(581.), vec![
            None,
            Some(OutOfDate::Stuck {
                height: H + 2,
                reported_secs: 181.
            }),
            Some(OutOfDate::Stuck {
                height: H + 1,
                reported_secs: 481.
            })
        ]);
    }

    /// T38.6.4o item 4: behind and ahead win over stuck.
    #[test]
    fn behind_and_ahead_win_over_stuck() {
        // Behind: B lags 4 blocks (T = 3), and the oldest block above it is old enough to make it stuck.
        let record = reports(&[(H + 1, 0.), (H + 4, 0.)]);
        assert_eq!(
            out_of_date_states(&heights(&[H + 4, H + 4, H]), &record, 3, secs(180), 1_000.)[2],
            Some(OutOfDate::Behind { lag: 4, tip: H + 4 })
        );
        // Ahead: C is above the agreed tip by more than T; nothing above it, so not stuck either way. B, at the agreed
        // tip's side, is not made stuck by the liar's height.
        let record = reports(&[(H + 1_000_000, 0.)]);
        assert_eq!(
            out_of_date_states(&heights(&[H + 1_000_000, H, H]), &record, 3, secs(180), 1_000.),
            vec![
                Some(OutOfDate::Ahead {
                    lead: 1_000_000,
                    tip: H
                }),
                None,
                None
            ]
        );
    }

    /// The first reports are recorded once, pruned at and below the lowest height, and bounded.
    #[test]
    fn the_record_of_first_reports_is_pruned_and_bounded() {
        let mut record = BTreeMap::new();
        note_first_reports(&mut record, &[H + 1, H, H], 3, 10.);
        assert_eq!(record, reports(&[(H + 1, 10.)]));
        // The second connection reaches H + 1: the first moment stays while one is still below.
        note_first_reports(&mut record, &[H + 1, H + 1, H], 3, 20.);
        assert_eq!(record, reports(&[(H + 1, 10.)]));
        // Every connection at H + 1: nothing at or below H + 1 is kept.
        note_first_reports(&mut record, &[H + 1, H + 1, H + 1], 3, 30.);
        assert!(record.is_empty(), "{record:?}");
        // The next block is entered at its first report, and a later report of it does not move it.
        note_first_reports(&mut record, &[H + 2, H + 1, H + 1], 3, 40.);
        note_first_reports(&mut record, &[H + 2, H + 2, H + 1], 3, 50.);
        assert_eq!(record, reports(&[(H + 2, 40.)]));
        // No connection has a height: nothing changes.
        note_first_reports(&mut record, &[], 3, 60.);
        assert_eq!(record, reports(&[(H + 2, 40.)]));
        // A single connection with a height has no agreed tip: nothing changes.
        note_first_reports(&mut record, &[H + 9], 3, 70.);
        assert_eq!(record, reports(&[(H + 2, 40.)]));
        // The record keeps the heights above the agreed tip minus T and up to the agreed tip plus T: here the agreed
        // tip is H + 10 and T = 3, so H + 7 and H + 14 are dropped and H + 8 to H + 13 stay; the lowest height bounds it too.
        let mut window = reports(&[(H + 7, 1.), (H + 8, 2.), (H + 9, 3.), (H + 13, 4.), (H + 14, 5.)]);
        note_first_reports(&mut window, &[H + 10, H + 10, H + 5], 3, 80.);
        assert_eq!(
            window,
            reports(&[(H + 8, 2.), (H + 9, 3.), (H + 10, 80.), (H + 13, 4.)])
        );
        let mut window = reports(&[(H + 1, 1.), (H + 2, 2.), (H + 7, 3.)]);
        note_first_reports(&mut window, &[H + 10, H + 10, H + 5, H + 8], 3, 80.);
        assert_eq!(window, reports(&[(H + 8, 80.), (H + 10, 80.)]));
        // A server reporting a new height every time cannot grow the record beyond 2T + 1, for any T up to the
        // maximum, and one that stays far behind does not change that.
        for tolerance in [0, 3, TOLERANCE_MAX_BLOCKS] {
            let mut flood = BTreeMap::new();
            for step in 1..=(MAX_FIRST_REPORTS as u64 * 4) {
                note_first_reports(&mut flood, &[H + step, H + step - 1, H - 1_000], tolerance, step as f64);
                assert!(
                    flood.len() as u64 <= 2 * tolerance + 1,
                    "T = {tolerance}: {}",
                    flood.len()
                );
            }
        }
        // The safety net: with a tolerance beyond any real one the cap drops the LOWEST heights.
        let mut capped = BTreeMap::new();
        let steps = MAX_FIRST_REPORTS as u64 * 2;
        for step in 1..=steps {
            note_first_reports(&mut capped, &[H + step, H + step - 1, 1], 1_000, step as f64);
        }
        assert_eq!(capped.len(), MAX_FIRST_REPORTS);
        assert_eq!(capped.keys().next_back().copied(), Some(H + steps));
        assert_eq!(
            capped.keys().next().copied(),
            Some(H + steps - MAX_FIRST_REPORTS as u64 + 1)
        );
        // The highest height there is does not overflow the pruning.
        let mut extreme = BTreeMap::new();
        note_first_reports(&mut extreme, &[u64::MAX, u64::MAX], 3, 1.);
        assert!(extreme.is_empty());
        note_first_reports(&mut extreme, &[u64::MAX, u64::MAX - 1], u64::MAX, 1.);
        assert_eq!(extreme, reports(&[(u64::MAX, 1.)]));
        // Near zero the window does not underflow.
        let mut low = BTreeMap::new();
        note_first_reports(&mut low, &[2, 1], 3, 1.);
        assert_eq!(low, reports(&[(2, 1.)]));
    }

    /// T38.6.4o item 4 (amended 2026-11-15), the record under a frozen server: one connection stays far behind (more
    /// than the record bound and more than T blocks), the chain moves on by 300 heights, and then a faster peer
    /// reports a block that another connection does not reach. The frozen one is behind and stays so; the stalled one
    /// is stuck just after the period counted from that first report, and not before.
    #[test]
    fn a_frozen_server_far_behind_does_not_hide_a_stalled_one() {
        let frozen = H - 500;
        let period = secs(120);
        let mut record = BTreeMap::new();
        // The chain advances: the two live connections move together, the third is frozen. Every update goes through
        // the record as the pool does.
        for step in 1..=300u64 {
            note_first_reports(&mut record, &[frozen, H + step, H + step], 3, step as f64);
            assert!(record.len() <= 7, "{record:?}");
        }
        let stalled = H + 300;
        // A faster peer reports the next block at t0; the stalled connection does not.
        let t0 = 1_000.;
        for repeat in 0..5 {
            note_first_reports(
                &mut record,
                &[frozen, stalled, stalled + 1],
                3,
                t0 + 10. * repeat as f64,
            );
        }
        assert_eq!(record.get(&(stalled + 1)), Some(&t0));
        let states = |now: f64| {
            out_of_date_states(
                &[Some(frozen), Some(stalled), Some(stalled + 1)],
                &record,
                3,
                period,
                now,
            )
        };
        let behind = Some(OutOfDate::Behind { lag: 800, tip: stalled });
        assert_eq!(states(t0 + 1.), vec![behind, None, None]);
        assert_eq!(states(t0 + 119.), vec![behind, None, None]);
        assert_eq!(states(t0 + 120.), vec![behind, None, None]);
        assert_eq!(states(t0 + 121.), vec![
            behind,
            Some(OutOfDate::Stuck {
                height: stalled + 1,
                reported_secs: 121.
            }),
            None
        ]);
        // The record still holds the heights the stalled one waits for, so the next check is computed from it.
        let reported = first_reports_above(&[Some(frozen), Some(stalled), Some(stalled + 1)], &record, 3);
        assert_eq!(reported[1], Some((stalled + 1, t0)));
        assert_eq!(reported[2], None);
    }

    /// T38.6.4o item 4 (amended 2026-11-15): a server above the agreed tip by more than the tolerance (D38.6.4o.9) does
    /// not make the honest ones stuck; one at the tolerance above does.
    #[test]
    fn a_liar_above_does_not_make_the_others_stuck() {
        let record = reports(&[(H + 1_000_000, 0.)]);
        assert_eq!(
            out_of_date_states(&heights(&[H + 1_000_000, H, H]), &record, 3, secs(180), 1_000.)[1],
            None
        );
        // At H + T the height counts.
        let record = reports(&[(H + 3, 0.)]);
        assert_eq!(
            out_of_date_states(&heights(&[H + 3, H, H]), &record, 3, secs(180), 1_000.)[1],
            Some(OutOfDate::Stuck {
                height: H + 3,
                reported_secs: 1_000.
            })
        );
    }

    /// T38.6.4o.6 (liar): one server far above the others does not make the honest ones look behind; it is itself
    /// out of date as ahead and not eligible, while a server at H + T is up to date and eligible.
    #[test]
    fn a_liar_above_the_others_is_ahead_and_does_not_make_them_look_behind() {
        const H: u64 = 700_000;
        assert_eq!(height_states(&heights(&[H + 1_000_000, H, H]), 3), vec![
            Some(OutOfDate::Ahead {
                lead: 1_000_000,
                tip: H
            }),
            None,
            None
        ]);
        // In a measurement round the two at H are eligible and the liar is not.
        let round = [server(0, 50., H + 1_000_000), server(1, 50., H), server(2, 50., H)];
        assert_eq!(eligible_ids(&round), vec![1, 2]);
        // A server at H + T stays eligible and up to date (T = 3); one block more is ahead.
        assert_eq!(height_states(&heights(&[H + 3, H, H]), 3), vec![None; 3]);
        assert_eq!(
            height_states(&heights(&[H + 4, H, H]), 3)[0],
            Some(OutOfDate::Ahead { lead: 4, tip: H })
        );
        let round = [server(0, 50., H + 3), server(1, 50., H), server(2, 50., H)];
        assert_eq!(eligible_servers(&round, &[], 3, FLOOR).len(), 3);
        // With two servers the agreed tip is the higher one: nobody is ahead.
        assert_eq!(height_states(&heights(&[H + 1_000_000, H]), 3)[0], None);
    }

    /// T38.6.4o.18 (items 7 and 12): two headers name the same block when their first 80 bytes are equal; the bytes after
    /// them do not count, a difference inside them does, and shorter headers are compared whole.
    #[test]
    fn t38_6_4o_18_the_same_block_is_the_same_base_header() {
        let base = [7u8; BASE_HEADER_LEN];
        let mut other_base = base;
        other_base[BASE_HEADER_LEN - 1] = 8;
        let with_tail = |base: &[u8], tail: &[u8]| [base, tail].concat();
        assert!(same_block(&with_tail(&base, &[1; 20]), &with_tail(&base, &[2; 38])));
        assert!(same_block(&with_tail(&base, &[1; 20]), &base));
        assert!(!same_block(
            &with_tail(&base, &[1; 20]),
            &with_tail(&other_base, &[1; 20])
        ));
        assert!(!same_block(&base[..40], &other_base));
        assert!(same_block(b"aa", b"aa"));
        assert!(!same_block(b"aa", b"bb"));
        assert!(!same_block(b"aa", &base));
    }

    fn with_header(mut server: RoundServer, header: &[u8]) -> RoundServer {
        if let Some(m) = server.measurement.as_mut() {
            m.header = Some(header.to_vec());
        }
        server
    }

    /// T38.6.4o.8 (item 7): a header that differs at the same height does not exclude a server; the pair starts a
    /// comparison.
    #[test]
    fn t38_6_4o_8_a_header_that_differs_starts_a_comparison_and_excludes_nobody() {
        let same = |id| with_header(server(id, 50., 100), b"aa");
        let other = |id| with_header(server(id, 50., 100), b"bb");
        let round = [same(0), same(1), other(2)];
        assert_eq!(eligible_ids(&round), vec![0, 1, 2]);
        assert_eq!(header_differences(&round), vec![(0, 2, 100), (1, 2, 100)]);
        // Servers at other heights, or without readable header bytes, are not compared.
        let lower = with_header(server(3, 50., 99), b"cc");
        assert!(header_differences(&[same(0), lower, server(4, 50., 100)]).is_empty());
        // Merged-mining headers: the same 80-byte base header with different bytes after it is the same block.
        let base = [7u8; BASE_HEADER_LEN];
        let proof = |tail: &[u8]| [&base[..], tail].concat();
        let aux = |id, tail: &[u8]| with_header(server(id, 50., 100), &proof(tail));
        assert!(header_differences(&[aux(0, &[1; 20]), aux(1, &[2; 38])]).is_empty());
        assert_eq!(common_height(100, 90, 3), 87);
        assert_eq!(common_height(2, 100, 3), 0);
        assert!(is_far_ahead(&[1_000_100, 1_000, 1_000], 3));
        assert!(!is_far_ahead(&[1_003, 1_000, 1_000], 3));
        assert!(!is_far_ahead(&[1_000_100, 1_000], 3));
    }

    /// T38.6.4o item 8: the least time between two re-measure rounds doubles, up to the cap.
    #[test]
    fn the_remeasure_gap_doubles_up_to_the_cap() {
        let base = Duration::from_secs(5 * 60);
        let cap = Duration::from_secs(60 * 60);
        let gaps: Vec<u64> = (0..8)
            .map(|level| remeasure_gap(base, cap, level).as_secs() / 60)
            .collect();
        assert_eq!(gaps, vec![5, 10, 20, 40, 60, 60, 60, 60]);
        assert_eq!(remeasure_gap(base, cap, u32::MAX), cap);
        assert_eq!(remeasure_gap(base, cap, 0), base);
    }

    /// T38.6.4l.3: the latency rule, with its 300 ms floor.
    #[test]
    fn the_latency_limit_is_four_times_the_best_with_a_floor() {
        // Best 50 ms: the limit is the 300 ms floor.
        let at = |latency| eligible_ids(&[server(0, 50., 10), server(1, latency, 10)]);
        assert_eq!(at(300.), vec![0, 1]);
        assert_eq!(at(301.), vec![0]);
        // Best 100 ms: 4 x 100 = 400 ms.
        let at = |latency| eligible_ids(&[server(0, 100., 10), server(1, latency, 10)]);
        assert_eq!(at(400.), vec![0, 1]);
        assert_eq!(at(401.), vec![0]);
    }

    /// T38.6.4l.3: a removed, a poor-demoted and an unmeasured server are not eligible; one that was suspended
    /// when it was measured is not either, but one released after its measurement stays eligible.
    #[test]
    fn marks_bar_a_server_from_the_round() {
        let mut removed = server(1, 50., 10);
        removed.removed = true;
        let mut poor = server(2, 50., 10);
        poor.poor_demoted = true;
        let unmeasured = RoundServer {
            id: 3,
            removed: false,
            poor_demoted: false,
            measurement: None,
        };
        let mut was_suspended = server(4, 50., 10);
        was_suspended.measurement.as_mut().unwrap().suspended = true;
        let released_after = server(5, 50., 10);
        assert_eq!(
            eligible_ids(&[
                server(0, 50., 10),
                removed,
                poor,
                unmeasured.clone(),
                was_suspended,
                released_after
            ]),
            vec![0, 5]
        );
        assert!(eligible_servers(&[unmeasured.clone()], &[], 1, FLOOR).is_empty());
    }

    /// The best latency is the lowest measured in the round, whoever is eligible; quality is best / latency.
    #[test]
    fn quality_is_the_best_latency_over_the_latency() {
        let eligible = eligible_servers(
            &[server(0, 50., 10), server(1, 100., 10), server(2, 200., 10)],
            &[],
            1,
            FLOOR,
        );
        let q: Vec<f64> = eligible.iter().map(|e| e.quality).collect();
        assert_eq!(q, vec![1., 0.5, 0.25]);
    }

    /// T38.6.4l.3: the non-eligible servers follow every eligible one.
    #[test]
    fn non_eligible_servers_rank_after_every_eligible_one() {
        let round = [server(0, 50., 8), server(1, 60., 10), server(2, 70., 10)];
        let eligible = eligible_servers(&round, &[], 1, FLOOR);
        for numbers in [[0.0; 6], [0.99; 6]] {
            let rank = rank_after_round(Measuring::Measured, &eligible, &[0, 1, 2], &mut scripted(&numbers));
            assert_eq!(&rank[..2], &[1, 2]);
            assert_eq!(rank[2], 0);
        }
    }

    /// T38.6.4l.4: three eligible servers at 50, 100 and 150 ms.
    #[test]
    fn mixed_weights_of_the_plan_example() {
        let eligible = eligible_servers(
            &[server(0, 50., 10), server(1, 100., 10), server(2, 150., 10)],
            &[],
            1,
            FLOOR,
        );
        let weights = mixed_weights(&eligible);
        let expected = [29. / 66., 20. / 66., 17. / 66.];
        for (weight, expected) in weights.iter().zip(expected) {
            assert!((weight - expected).abs() < 1e-4, "{weight} vs {expected}");
        }
        assert!((weights.iter().sum::<f64>() - 1.).abs() < 1e-9);
        assert!((weights[0] - 0.44).abs() < 0.005 && (weights[1] - 0.30).abs() < 0.005);
    }

    /// T38.6.4l.4: the injected draw gives the order of a weighted draw without replacement.
    #[test]
    fn mixed_order_follows_the_injected_draw() {
        let eligible = eligible_servers(
            &[server(0, 50., 10), server(1, 100., 10), server(2, 150., 10)],
            &[],
            1,
            FLOOR,
        );
        let order = |numbers: &[f64]| rank_after_round(Measuring::Mixed, &eligible, &[0, 1, 2], &mut scripted(numbers));
        // Weights 29, 20, 17 (of 66). 0.0 picks the first; then the remaining 20 : 17 pair.
        assert_eq!(order(&[0.0, 0.0]), vec![0, 1, 2]);
        assert_eq!(order(&[0.0, 0.99]), vec![0, 2, 1]);
        // 0.5 * 66 = 33: server 0 (29) is passed, server 1 (49 cumulative) is picked.
        assert_eq!(order(&[0.5, 0.0]), vec![1, 0, 2]);
        // 0.99 * 66 lands on the last one; then 0.0 on the first of the rest.
        assert_eq!(order(&[0.99, 0.0]), vec![2, 0, 1]);
    }

    /// T38.6.4l.4: over 2,000 draws with the real random source, each first-place share is within 0.05
    /// of its weight.
    #[test]
    fn mixed_first_places_follow_the_weights() {
        let eligible = eligible_servers(
            &[server(0, 50., 10), server(1, 100., 10), server(2, 150., 10)],
            &[],
            1,
            FLOOR,
        );
        let weights = mixed_weights(&eligible);
        let mut rng = common::small_rng();
        let mut draw = || rand::Rng::gen::<f64>(&mut rng);
        let mut first = [0usize; 3];
        for _ in 0..2_000 {
            first[rank_after_round(Measuring::Mixed, &eligible, &[0, 1, 2], &mut draw)[0]] += 1;
        }
        for (count, weight) in first.iter().zip(&weights) {
            let share = *count as f64 / 2_000.;
            assert!((share - weight).abs() < 0.05, "share {share} vs weight {weight}");
        }
    }

    /// T38.6.4l.5: `measured` orders by quality; equal quality is settled by the draw, both ways.
    #[test]
    fn measured_orders_by_quality_and_ties_by_the_draw() {
        let eligible = eligible_servers(
            &[
                server(0, 50., 10),
                server(1, 100., 10),
                server(2, 100., 10),
                server(3, 150., 10),
            ],
            &[],
            1,
            FLOOR,
        );
        let order =
            |numbers: &[f64]| rank_after_round(Measuring::Measured, &eligible, &[0, 1, 2, 3], &mut scripted(numbers));
        // The shuffle of the pair [1, 2] takes one number: below one half swaps them.
        let (a, b) = (order(&[0.9]), order(&[0.1]));
        assert_eq!((a[0], a[3]), (0, 3));
        assert_eq!((b[0], b[3]), (0, 3));
        assert_eq!([a[1], a[2]], [1, 2]);
        assert_eq!([b[1], b[2]], [2, 1]);
    }

    /// T38.6.4l.9: the smoothed figures of the plan.
    #[test]
    fn the_smoothed_latency_follows_the_formula() {
        let mut smoothed = Smoothed::default();
        for _ in 0..4 {
            smoothed.record(50.);
        }
        assert_eq!(smoothed.value_ms(), Some(50.));
        let mut seen = Vec::new();
        for _ in 0..3 {
            smoothed.record(2_000.);
            seen.push(smoothed.value_ms().unwrap());
        }
        for (value, expected) in seen.iter().zip([440., 752., 1_001.6]) {
            assert!((value - expected).abs() < 1e-9, "{value} vs {expected}");
        }
    }

    /// T38.6.4l.9: P2 with an activation latency of 50 ms and a best activation latency of 40 ms.
    #[test]
    fn p2_needs_five_samples_and_all_three_thresholds() {
        let slow = |smoothed: &Smoothed, reference| is_slow(smoothed, reference, Some(40.), 1_000.);
        // Four samples of 2,000 ms do not trigger, the fifth does.
        let mut smoothed = Smoothed::default();
        for _ in 0..4 {
            smoothed.record(2_000.);
            assert!(!slow(&smoothed, Some(50.)));
        }
        smoothed.record(2_000.);
        assert!(slow(&smoothed, Some(50.)));

        // After four samples of 50 ms, only the third sample of 2,000 ms triggers (1,001.6 ms).
        let mut smoothed = Smoothed::default();
        for _ in 0..4 {
            smoothed.record(50.);
        }
        let mut triggered = Vec::new();
        let mut late_reference = Vec::new();
        for _ in 0..3 {
            smoothed.record(2_000.);
            triggered.push(slow(&smoothed, Some(50.)));
            // The same samples for a server whose activation latency was 400 ms: never.
            late_reference.push(slow(&smoothed, Some(400.)));
        }
        assert_eq!(triggered, vec![false, false, true]);
        assert_eq!(late_reference, vec![false, false, false]);

        // No reference latency, or no activation figure at all, means no judgment.
        assert!(!slow(&smoothed, None));
        assert!(!is_slow(&smoothed, Some(50.), None, 1_000.));
        assert!(!is_slow(&Smoothed::default(), Some(50.), Some(40.), 1_000.));
    }

    /// The reference latency is the activation one, or without it the re-measure one (D38.6.4l.5).
    #[test]
    fn reference_latency_prefers_the_activation_round() {
        let mut quality = ServerQuality::default();
        assert_eq!(quality.reference_ms(), None);
        quality.round_ms = Some(70.);
        assert_eq!(quality.reference_ms(), Some(70.));
        quality.activation_ms = Some(50.);
        assert_eq!(quality.reference_ms(), Some(50.));
    }

    /// T38.6.4l.7 with the tolerance of one block: 2 or more blocks below is lagging; 1 is not.
    #[test]
    fn two_blocks_below_the_best_lag_with_a_tolerance_of_one() {
        assert!(!is_beyond_tolerance(Some(100), 100, 1));
        assert!(!is_beyond_tolerance(Some(100), 99, 1));
        assert!(is_beyond_tolerance(Some(100), 98, 1));
        assert!(!is_beyond_tolerance(Some(100), 101, 1));
    }

    /// T38.6.4l.8: the limit of P1 for one maintained connection, with the production floor of 10 minutes.
    #[test]
    fn the_stale_limit_of_a_lone_connection() {
        let floor = Duration::from_secs(600);
        let conf = |value: serde_json::Value| block_interval_from_conf(&serde_json::json!({ "avg_blocktime": value }));
        assert_eq!(
            stale_limit(conf(serde_json::json!(60)), floor),
            Duration::from_secs(600)
        );
        assert_eq!(
            stale_limit(conf(serde_json::json!(600)), floor),
            Duration::from_secs(1_800)
        );
        assert_eq!(
            stale_limit(block_interval_from_conf(&serde_json::json!({})), floor),
            floor
        );
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-5),
            serde_json::json!("60"),
            serde_json::json!(null),
        ] {
            assert_eq!(conf(bad.clone()), None, "{bad}");
        }
    }

    /// T38.6.4l.10: the mark lasts 15 minutes from the last trigger.
    #[test]
    fn the_poor_mark_lasts_until_its_time() {
        let mut quality = ServerQuality::default();
        let demotion = 15. * 60.;
        let trigger = 1_000.;
        quality.poor_until = trigger + demotion;
        assert!(quality.is_poor_demoted(trigger + 14. * 60.));
        assert!(!quality.is_poor_demoted(trigger + 15. * 60.));
        // A second trigger 10 minutes later restarts the 15 minutes.
        quality.poor_until = trigger + 10. * 60. + demotion;
        assert!(quality.is_poor_demoted(trigger + 24. * 60.));
        assert!(!quality.is_poor_demoted(trigger + 25. * 60.));
    }

    /// T38.6.4l.11: the limit of 5 minutes between rounds, over six hours of clock time.
    #[test]
    fn rounds_start_five_minutes_apart_at_most() {
        let limit = Duration::from_secs(300);
        assert_eq!(next_round_at(None, limit), 0.);
        assert_eq!(next_round_at(Some(1_000.), limit), 1_300.);
        // Triggers every minute for six hours: the rounds the limit lets through are 5 minutes apart.
        let mut last = None;
        let mut starts = Vec::new();
        for minute in 0..360 {
            let now = f64::from(minute) * 60.;
            if now >= next_round_at(last, limit) {
                starts.push(now);
                last = Some(now);
            }
        }
        assert_eq!(starts.len(), 72);
        assert!(starts.windows(2).all(|pair| pair[1] - pair[0] >= 300.));
    }

    /// T38.6.4l.12: the replacement takes the poor server's place and the poor server goes last.
    #[test]
    fn the_replacement_takes_the_place_of_the_poor_server() {
        // Rank A, B, C, D; B is poor and D replaces it: A, D, C, B.
        assert_eq!(rank_with_replacement(&[0, 1, 2, 3], 1, 3), vec![0, 3, 2, 1]);
        // A replacement ranked above the poor server takes its place; the others keep their order.
        assert_eq!(rank_with_replacement(&[0, 1, 2, 3], 2, 0), vec![1, 0, 3, 2]);
        assert_eq!(rank_with_replacement(&[0, 1, 2, 3], 0, 3), vec![3, 1, 2, 0]);
    }

    /// T38.6.4l.12: under `measured` the best quality is the replacement; under `mixed` the draw decides;
    /// without an eligible server there is none (T38.6.4l.13).
    #[test]
    fn the_replacement_is_picked_by_the_mode() {
        let eligible = eligible_servers(&[server(2, 100., 10), server(3, 50., 10)], &[], 1, FLOOR);
        assert_eq!(
            pick_replacement(Measuring::Measured, &eligible, &mut scripted(&[0.0])),
            Some(3)
        );
        // Weights 0.5 * (0.5 / 1.5) + 0.25 = 5/12 for server 2 and 0.5 * (1 / 1.5) + 0.25 = 7/12 for server 3.
        assert_eq!(
            pick_replacement(Measuring::Mixed, &eligible, &mut scripted(&[0.0])),
            Some(2)
        );
        assert_eq!(
            pick_replacement(Measuring::Mixed, &eligible, &mut scripted(&[0.9])),
            Some(3)
        );
        // With two maintained connections reporting 10, every candidate at 8 is 2 blocks behind the agreed tip.
        let none = eligible_servers(&[server(2, 100., 8), server(3, 50., 8)], &[(0, 10), (1, 10)], 1, FLOOR);
        assert!(none.is_empty());
        assert_eq!(pick_replacement(Measuring::Mixed, &none, &mut scripted(&[])), None);
        assert_eq!(pick_replacement(Measuring::Measured, &none, &mut scripted(&[])), None);
    }
}
