//! The measured server order of a coin's Electrum pool (CRD ch.38 R38.6.4l).
//!
//! This module holds the decisions as pure functions of figures and of explicit times: which measured
//! servers are eligible, the `mixed` weights, the two orders, the smoothed latency, the poor-connection
//! tests, the replacement in the rank, and the staleness decisions of R38.6.4o (the tolerance, the agreed
//! tip, the out-of-date state of a connection and the re-measure backoff). The pool (`electrum_pool.rs`)
//! gathers the figures, calls these and acts on the result. Every random choice takes a [`Draw`], a source of numbers in `[0, 1)`, so tests
//! inject the draw instead of seeding a generator.

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
    /// It sent no header notification for longer than the stuck period while another connection did.
    Stuck { silent_secs: f64 },
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

/// What the out-of-date judgment knows of one maintained connection that has a height (R38.6.4o items 3 and 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TipFigure {
    pub height: u64,
    /// When the answer to its header request arrived or its last notification, whichever is later (seconds since the
    /// epoch).
    pub since: f64,
    /// When its last header notification arrived, if one has.
    pub notified_at: Option<f64>,
}

/// The out-of-date state of each maintained connection (CRD ch.38 R38.6.4o item 4). `figures` holds one entry per
/// maintained connection, `None` for one without a height, which no judgment applies to. The running agreed tip is
/// the agreed tip of the heights given. A connection is *behind* when its lag to it exceeds `tolerance`, and
/// *ahead* when its lead over it exceeds `tolerance` (D38.6.4o.9), and *stuck* when there are two or more
/// connections with a height, it has had no news for longer than `stuck_after` and at least one other has received a
/// header notification within `stuck_after` (D38.6.4o.7); when none has, the chain itself may have stopped and none
/// is stuck. Behind and ahead win over stuck.
pub(super) fn out_of_date_states(
    figures: &[Option<TipFigure>],
    tolerance: u64,
    stuck_after: Duration,
    now: f64,
) -> Vec<Option<OutOfDate>> {
    let heights: Vec<u64> = figures.iter().flatten().map(|figure| figure.height).collect();
    let agreed = agreed_tip(&heights);
    let stuck_after = stuck_after.as_secs_f64();
    figures
        .iter()
        .enumerate()
        .map(|(index, figure)| {
            let figure = (*figure)?;
            if let Some(tip) = agreed {
                let behind = lag(tip, figure.height);
                if behind > tolerance {
                    return Some(OutOfDate::Behind { lag: behind, tip });
                }
                let ahead = lead(tip, figure.height);
                if ahead > tolerance {
                    return Some(OutOfDate::Ahead { lead: ahead, tip });
                }
            }
            let silent = now - figure.since;
            if heights.len() >= 2 && silent > stuck_after {
                let another_notified = figures.iter().enumerate().any(|(other, figure)| {
                    other != index
                        && figure
                            .and_then(|figure| figure.notified_at)
                            .map_or(false, |at| now - at <= stuck_after)
                });
                if another_notified {
                    return Some(OutOfDate::Stuck { silent_secs: silent });
                }
            }
            None
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

/// The header bytes at the agreed tip height that at least two measured servers answered and no other bytes
/// answered as often (R38.6.4o item 7). `None` when no bytes are shared by two servers, or when two different bytes
/// are each shared by as many servers as the most shared: no side is then known to be right.
fn consensus_header(round: &[RoundServer], tip: u64) -> Option<&[u8]> {
    let mut groups: Vec<(&[u8], usize)> = Vec::new();
    for header in round
        .iter()
        .filter_map(|server| server.measurement.as_ref())
        .filter(|m| m.height == tip)
        .filter_map(|m| m.header.as_deref())
    {
        match groups.iter_mut().find(|(known, _)| *known == header) {
            Some(group) => group.1 += 1,
            None => groups.push((header, 1)),
        }
    }
    let top = groups.iter().map(|group| group.1).max()?;
    if top < 2 {
        return None;
    }
    let mut leaders = groups.iter().filter(|group| group.1 == top);
    let first = leaders.next()?;
    leaders.next().is_none().then_some(first.0)
}

/// The eligible servers of a round, in the order of `round`. `reported` holds the heights the maintained
/// connections currently report, by server id (D38.6.4l.3); `tolerance` is the coin's *T* (R38.6.4o item 1);
/// `latency_floor_ms` is the 300 ms floor. The best tip is the agreed tip (R38.6.4o item 2) over the heights of the
/// round and `reported`, and a server whose header differs from the one two or more others answer at that height
/// is not eligible (R38.6.4o item 7).
pub(super) fn eligible_servers(
    round: &[RoundServer],
    reported: &[(usize, u64)],
    tolerance: u64,
    latency_floor_ms: f64,
) -> Vec<Eligible> {
    let measured = || round.iter().filter_map(|server| server.measurement.as_ref());
    let best_tip = round_agreed_tip(round, reported);
    let consensus = best_tip.and_then(|tip| consensus_header(round, tip));
    let best_latency = measured().map(|m| m.latency_ms).fold(f64::INFINITY, f64::min);
    if !best_latency.is_finite() {
        return Vec::new();
    }
    let latency_limit = (ELIGIBLE_LATENCY_FACTOR * best_latency).max(latency_floor_ms);
    round
        .iter()
        .filter_map(|server| {
            let m = server.measurement.as_ref()?;
            let header_agrees = match (best_tip, consensus, m.header.as_deref()) {
                (Some(tip), Some(consensus), Some(header)) if m.height == tip => header == consensus,
                _ => true,
            };
            let eligible = !server.removed
                && !server.poor_demoted
                && !m.suspended
                && !is_outside_tolerance(best_tip, m.height, tolerance)
                && header_agrees
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

    fn figure(height: u64, since: f64, notified_at: Option<f64>) -> Option<TipFigure> {
        Some(TipFigure {
            height,
            since,
            notified_at,
        })
    }

    /// T38.6.4o items 4 and 5: a connection more than the tolerance behind the running agreed tip is out of date as
    /// behind, and is up to date again when it catches up; a connection at the tolerance never is.
    #[test]
    fn a_connection_is_behind_only_beyond_the_tolerance() {
        let states = |heights: [u64; 3]| {
            let figures: Vec<_> = heights.iter().map(|height| figure(*height, 0., Some(0.))).collect();
            out_of_date_states(&figures, 3, Duration::from_secs(600), 1.)
        };
        assert_eq!(states([100, 100, 96]), vec![
            None,
            None,
            Some(OutOfDate::Behind { lag: 4, tip: 100 })
        ]);
        assert_eq!(states([100, 100, 97]), vec![None, None, None]);
        assert_eq!(states([100, 100, 100]), vec![None, None, None]);
        // The field case: two connections, one far behind. The agreed tip is the higher of the two.
        let two = out_of_date_states(
            &[figure(2_000, 0., Some(0.)), figure(1_000, 0., Some(0.))],
            3,
            Duration::from_secs(600),
            1.,
        );
        assert_eq!(two, vec![None, Some(OutOfDate::Behind { lag: 1_000, tip: 2_000 })]);
        // One height, or none, judges nobody; a connection without a height is never judged.
        assert_eq!(
            out_of_date_states(&[figure(1, 0., None), None], 3, Duration::from_secs(600), 1.),
            vec![None, None]
        );
    }

    /// T38.6.4o item 4: stuck needs two connections with a height, more silence than the period and another
    /// connection that was notified within the period.
    #[test]
    fn a_connection_is_stuck_when_another_one_hears_news_and_it_does_not() {
        let period = Duration::from_secs(600);
        // Joined at 0, silent since; the other notified at 700. At 700 the silent one has been quiet for 700 s.
        let figures = [figure(100, 0., None), figure(100, 700., Some(700.))];
        assert_eq!(out_of_date_states(&figures, 3, period, 700.), vec![
            Some(OutOfDate::Stuck { silent_secs: 700. }),
            None
        ]);
        // Not before the period: at exactly 600 s of silence it is not stuck (more than, not at).
        let figures = [figure(100, 0., None), figure(100, 600., Some(600.))];
        assert_eq!(out_of_date_states(&figures, 3, period, 600.), vec![None, None]);
        // Every connection silent for longer than the period: the chain may have halted, none is stuck.
        let figures = [figure(100, 0., Some(0.)), figure(100, 0., Some(0.))];
        assert_eq!(out_of_date_states(&figures, 3, period, 1_000.), vec![None, None]);
        // The answer to a header request is no notification: a connection that only answered recently does not
        // vouch for the chain.
        let figures = [figure(100, 0., None), figure(100, 900., None)];
        assert_eq!(out_of_date_states(&figures, 3, period, 1_000.), vec![None, None]);
        // A single connection with a height is never stuck.
        let figures = [figure(100, 0., None), None];
        assert_eq!(out_of_date_states(&figures, 3, period, 1_000.), vec![None, None]);
        // Behind wins over stuck.
        let figures = [
            figure(100, 0., None),
            figure(200, 700., Some(700.)),
            figure(200, 700., Some(700.)),
        ];
        assert_eq!(
            out_of_date_states(&figures, 3, period, 700.)[0],
            Some(OutOfDate::Behind { lag: 100, tip: 200 })
        );
    }

    /// T38.6.4o.6 (liar): one server far above the others does not make the honest ones look behind; it is itself
    /// out of date as ahead and not eligible, while a server at H + T is up to date and eligible.
    #[test]
    fn a_liar_above_the_others_is_ahead_and_does_not_make_them_look_behind() {
        const H: u64 = 700_000;
        let figures = [
            figure(H + 1_000_000, 0., Some(0.)),
            figure(H, 0., Some(0.)),
            figure(H, 0., Some(0.)),
        ];
        assert_eq!(out_of_date_states(&figures, 3, Duration::from_secs(600), 1.), vec![
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
        let figures = [
            figure(H + 3, 0., Some(0.)),
            figure(H, 0., Some(0.)),
            figure(H, 0., Some(0.)),
        ];
        assert_eq!(out_of_date_states(&figures, 3, Duration::from_secs(600), 1.), vec![
            None;
            3
        ]);
        let figures = [
            figure(H + 4, 0., Some(0.)),
            figure(H, 0., Some(0.)),
            figure(H, 0., Some(0.)),
        ];
        assert_eq!(
            out_of_date_states(&figures, 3, Duration::from_secs(600), 1.)[0],
            Some(OutOfDate::Ahead { lead: 4, tip: H })
        );
        let round = [server(0, 50., H + 3), server(1, 50., H), server(2, 50., H)];
        assert_eq!(eligible_servers(&round, &[], 3, FLOOR).len(), 3);
        // With two servers the agreed tip is the higher one: nobody is ahead.
        let figures = [figure(H + 1_000_000, 0., Some(0.)), figure(H, 0., Some(0.))];
        assert_eq!(out_of_date_states(&figures, 3, Duration::from_secs(600), 1.)[0], None);
    }

    fn with_header(mut server: RoundServer, header: &[u8]) -> RoundServer {
        if let Some(m) = server.measurement.as_mut() {
            m.header = Some(header.to_vec());
        }
        server
    }

    /// T38.6.4o.8: header agreement at the agreed tip height.
    #[test]
    fn t38_6_4o_8_a_header_that_differs_from_two_agreeing_ones_is_not_eligible() {
        let same = |id| with_header(server(id, 50., 100), b"aa");
        let other = |id| with_header(server(id, 50., 100), b"bb");
        // Two answer the same bytes and one other bytes: the third is not eligible.
        assert_eq!(eligible_ids(&[same(0), same(1), other(2)]), vec![0, 1]);
        // Two at the same height with different bytes and no third there: both stay eligible.
        assert_eq!(eligible_ids(&[same(0), other(1)]), vec![0, 1]);
        // Two and two: neither side is known to be right, so no one is excluded.
        assert_eq!(eligible_ids(&[same(0), same(1), other(2), other(3)]), vec![0, 1, 2, 3]);
        // Servers at other heights are not compared, and a server without readable header bytes is not either.
        let lower = with_header(server(3, 50., 99), b"cc");
        let unreadable = server(4, 50., 100);
        assert_eq!(eligible_ids(&[same(0), same(1), lower, unreadable]), vec![0, 1, 3, 4]);
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
