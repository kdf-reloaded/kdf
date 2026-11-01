//! The measured server order of a coin's Electrum pool (CRD ch.38 R38.6.4l).
//!
//! This module holds the decisions as pure functions of figures and of explicit times: which measured
//! servers are eligible, the `mixed` weights, the two orders, the smoothed latency, the poor-connection
//! tests and the replacement in the rank. The pool (`electrum_pool.rs`) gathers the figures, calls these
//! and acts on the result. Every random choice takes a [`Draw`], a source of numbers in `[0, 1)`, so tests
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
/// A server is eligible when its height is at most this many blocks below the best tip (item 4).
const ELIGIBLE_TIP_LAG: u64 = 1;
/// P1: the number of blocks below the best height that counts as stale (item 6).
const STALE_TIP_LAG: u64 = 2;
/// P1 with one maintained connection: the limit is this many block intervals (item 6).
const STALE_BLOCK_INTERVALS: u32 = 3;
/// P2: the smoothed latency is slow above this many times the connection's own reference latency ...
const SLOW_OWN_FACTOR: f64 = 4.;
/// ... and above this many times the best latency of the activation round (item 6).
const SLOW_BEST_FACTOR: f64 = 2.;

/// One server's figures from a measurement round (R38.6.4l item 2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Measurement {
    pub id: usize,
    pub latency_ms: f64,
    pub height: u64,
    /// Whether the server was suspended when it was measured (D38.6.4l.8). The release that follows a
    /// measurement does not count.
    pub suspended: bool,
}

/// One server as a round sees it: its measurement, if it has one, and the marks that bar it.
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// The eligible servers of a round, in the order of `round`. `reported_tip` is the highest height a
/// maintained connection currently reports (D38.6.4l.3); `latency_floor_ms` is the 300 ms floor.
pub(super) fn eligible_servers(
    round: &[RoundServer],
    reported_tip: Option<u64>,
    latency_floor_ms: f64,
) -> Vec<Eligible> {
    let measured = || round.iter().filter_map(|server| server.measurement);
    let best_tip = measured().map(|m| m.height).chain(reported_tip).max();
    let best_latency = measured().map(|m| m.latency_ms).fold(f64::INFINITY, f64::min);
    let (Some(best_tip), true) = (best_tip, best_latency.is_finite()) else {
        return Vec::new();
    };
    let latency_limit = (ELIGIBLE_LATENCY_FACTOR * best_latency).max(latency_floor_ms);
    round
        .iter()
        .filter_map(|server| {
            let m = server.measurement?;
            let eligible = !server.removed
                && !server.poor_demoted
                && !m.suspended
                && m.height + ELIGIBLE_TIP_LAG >= best_tip
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

/// P1 with several maintained connections (R38.6.4l item 6): `own` is 2 or more blocks below `best`.
pub(super) fn lags_behind(best: u64, own: u64) -> bool { best >= own.saturating_add(STALE_TIP_LAG) }

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
}

impl ServerQuality {
    /// The reference latency of P2: the activation latency, or without one the re-measure latency.
    pub(super) fn reference_ms(&self) -> Option<f64> { self.activation_ms.or(self.round_ms) }

    /// The counters that run "since joining" start again.
    pub(super) fn on_join(&mut self, now: f64) {
        self.smoothed = Smoothed::default();
        self.notified_at = now;
        self.lag_strikes = 0;
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
                suspended: false,
            }),
        }
    }

    fn eligible_ids(round: &[RoundServer]) -> Vec<usize> {
        eligible_servers(round, None, FLOOR).iter().map(|e| e.id).collect()
    }

    /// T38.6.4l.3: the height rule.
    #[test]
    fn a_server_two_blocks_behind_is_not_eligible() {
        let round = [server(0, 50., 100), server(1, 50., 99), server(2, 50., 98)];
        assert_eq!(eligible_ids(&round), vec![0, 1]);
        // The heights a maintained connection reports count for the best tip (D38.6.4l.3).
        let reported = eligible_servers(&round, Some(101), FLOOR);
        assert_eq!(reported.iter().map(|e| e.id).collect::<Vec<_>>(), vec![0]);
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
                unmeasured,
                was_suspended,
                released_after
            ]),
            vec![0, 5]
        );
        assert!(eligible_servers(&[unmeasured], None, FLOOR).is_empty());
    }

    /// The best latency is the lowest measured in the round, whoever is eligible; quality is best / latency.
    #[test]
    fn quality_is_the_best_latency_over_the_latency() {
        let eligible = eligible_servers(
            &[server(0, 50., 10), server(1, 100., 10), server(2, 200., 10)],
            None,
            FLOOR,
        );
        let q: Vec<f64> = eligible.iter().map(|e| e.quality).collect();
        assert_eq!(q, vec![1., 0.5, 0.25]);
    }

    /// T38.6.4l.3: the non-eligible servers follow every eligible one.
    #[test]
    fn non_eligible_servers_rank_after_every_eligible_one() {
        let round = [server(0, 50., 8), server(1, 60., 10), server(2, 70., 10)];
        let eligible = eligible_servers(&round, None, FLOOR);
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
            None,
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
            None,
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
            None,
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
            None,
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

    /// T38.6.4l.7: 2 or more blocks below is lagging; 1 is not.
    #[test]
    fn two_blocks_below_the_best_lag() {
        assert!(!lags_behind(100, 100));
        assert!(!lags_behind(100, 99));
        assert!(lags_behind(100, 98));
        assert!(!lags_behind(100, 101));
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
        let eligible = eligible_servers(&[server(2, 100., 10), server(3, 50., 10)], None, FLOOR);
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
        // With a reported tip of 10, every candidate at 8 is at least 2 blocks behind.
        let none = eligible_servers(&[server(2, 100., 8), server(3, 50., 8)], Some(10), FLOOR);
        assert!(none.is_empty());
        assert_eq!(pick_replacement(Measuring::Mixed, &none, &mut scripted(&[])), None);
        assert_eq!(pick_replacement(Measuring::Measured, &none, &mut scripted(&[])), None);
    }
}
