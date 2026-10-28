//! Measurement knobs of the maker publication rate lab (feature `lab-knobs`, never enabled in a shipped build).
//!
//! They let a test switch two candidate behaviours on without changing any default:
//! - `rtt_pressure`: the ping round trip of a relay connection raises the send queue pressure level, besides the
//!   queue depth;
//! - `hold_ping_close_when_degraded`: while the maker outbox is degraded, a ping failure closes the connection only
//!   after pings have failed for more than [`HOLD_CLOSE_AFTER`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long pings must have failed before a connection is closed while the hold is active.
pub const HOLD_CLOSE_AFTER: Duration = Duration::from_secs(90);

static RTT_PRESSURE: AtomicBool = AtomicBool::new(false);
static HOLD_CLOSE: AtomicBool = AtomicBool::new(false);
static DEGRADED: AtomicBool = AtomicBool::new(false);

pub fn set_rtt_pressure(on: bool) { RTT_PRESSURE.store(on, Ordering::Relaxed) }

pub fn rtt_pressure() -> bool { RTT_PRESSURE.load(Ordering::Relaxed) }

pub fn set_hold_ping_close_when_degraded(on: bool) { HOLD_CLOSE.store(on, Ordering::Relaxed) }

pub fn hold_ping_close_when_degraded() -> bool { HOLD_CLOSE.load(Ordering::Relaxed) }

/// Set by the maker outbox pump: whether any outbox is in degraded mode.
pub fn set_degraded(on: bool) { DEGRADED.store(on, Ordering::Relaxed) }

pub fn degraded() -> bool { DEGRADED.load(Ordering::Relaxed) }

static C6_ON: AtomicBool = AtomicBool::new(false);

pub fn set_c6(on: bool) { C6_ON.store(on, Ordering::Relaxed) }

pub fn c6_on() -> bool { C6_ON.load(Ordering::Relaxed) }

/// How long the data queue must be non-empty before candidate C6 raises the pressure.
const C6_RAISE_AFTER: Duration = Duration::from_secs(2);
/// How long the data queue must be empty before candidate C6 clears the pressure.
const C6_CLEAR_AFTER: Duration = Duration::from_secs(5);

#[derive(Default)]
struct C6State {
    nonempty_since: Option<Instant>,
    empty_since: Option<Instant>,
    raised: bool,
    /// Wall clock (ms since the epoch) of the first sample with a non-empty queue, and of the first raise.
    first_nonempty_ms: Option<u64>,
    first_raised_ms: Option<u64>,
}

static C6: Mutex<Option<C6State>> = Mutex::new(None);

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Candidate C6: feeds the state with whether the data queue toward any connection is non-empty. It is called on
/// every poll of the swarm, which the gossip heartbeat makes at least about once a second. The first non-empty
/// sample is recorded in every run, whether C6 is on or not.
pub fn c6_sample(nonempty: bool) {
    let mut guard = C6.lock().unwrap();
    let state = guard.get_or_insert_with(C6State::default);
    let now = Instant::now();
    if nonempty {
        state.first_nonempty_ms.get_or_insert_with(wall_ms);
        state.empty_since = None;
        let since = *state.nonempty_since.get_or_insert(now);
        if !state.raised && now.duration_since(since) >= C6_RAISE_AFTER {
            state.raised = true;
            state.first_raised_ms.get_or_insert_with(wall_ms);
        }
    } else {
        state.nonempty_since = None;
        let since = *state.empty_since.get_or_insert(now);
        if state.raised && now.duration_since(since) >= C6_CLEAR_AFTER {
            state.raised = false;
        }
    }
}

pub fn c6_raised() -> bool { C6.lock().unwrap().as_ref().is_some_and(|state| state.raised) }

pub fn first_nonempty_ms() -> Option<u64> { C6.lock().unwrap().as_ref().and_then(|s| s.first_nonempty_ms) }

pub fn c6_first_raised_ms() -> Option<u64> { C6.lock().unwrap().as_ref().and_then(|s| s.first_raised_ms) }

/// Forgets what the earlier run of the same process recorded.
pub fn reset_run() {
    *C6.lock().unwrap() = None;
    RELAY_CLOSES.lock().unwrap().clear();
    PING_LOG.lock().unwrap().clear();
}

static RELAY_CLOSES: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Records the close of a connection to a relay (wall clock, milliseconds since the epoch).
pub fn note_relay_close() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    RELAY_CLOSES.lock().unwrap().push(now);
}

/// The times of the relay connection closes seen so far.
pub fn relay_close_times() -> Vec<u64> { RELAY_CLOSES.lock().unwrap().clone() }

static PING_LOG: Mutex<Vec<(u64, Option<u64>)>> = Mutex::new(Vec::new());

/// Records one ping result for the lab report: (milliseconds since the first call, round trip in milliseconds or
/// `None` for a failure).
pub fn record_ping(rtt: Option<Duration>) {
    static BASE: OnceLock<Instant> = OnceLock::new();
    let at = BASE.get_or_init(Instant::now).elapsed().as_millis() as u64;
    PING_LOG.lock().unwrap().push((at, rtt.map(|r| r.as_millis() as u64)));
}

/// The ping results recorded so far.
pub fn ping_log() -> Vec<(u64, Option<u64>)> { PING_LOG.lock().unwrap().clone() }

/// Ping round-trip pressure of one connection.
///
/// Raised when the round trip exceeds the larger of 3 times the connection's baseline and 2 s, for two samples in
/// a row; cleared when it is below 1.5 times the baseline, for two samples in a row. The baseline is the lowest
/// round trip seen while the pressure is clear. A failed or timed out ping counts as above the raise threshold and
/// never as below the clear threshold.
#[derive(Debug, Default)]
pub struct RttPressure {
    baseline: Option<Duration>,
    pressured: bool,
    streak: u32,
}

const RAISE_FLOOR: Duration = Duration::from_secs(2);
const SAMPLES: u32 = 2;

impl RttPressure {
    pub fn is_pressured(&self) -> bool { self.pressured }

    /// Feeds one ping result: `Some(rtt)` for a success, `None` for a failure or a timeout.
    pub fn sample(&mut self, rtt: Option<Duration>) {
        let baseline = match (self.baseline, rtt) {
            (None, Some(rtt)) => {
                self.baseline = Some(rtt);
                rtt
            },
            (Some(baseline), _) => baseline,
            // No success yet: nothing to compare with, only the 2 s floor applies.
            (None, None) => Duration::ZERO,
        };
        if !self.pressured {
            let raise_above = (baseline * 3).max(RAISE_FLOOR);
            if rtt.is_none_or(|rtt| rtt > raise_above) {
                self.streak += 1;
                if self.streak >= SAMPLES {
                    self.pressured = true;
                    self.streak = 0;
                }
            } else {
                self.streak = 0;
                if let (Some(rtt), Some(b)) = (rtt, self.baseline) {
                    self.baseline = Some(b.min(rtt));
                }
            }
        } else {
            let clear_below = baseline.mul_f64(1.5);
            if rtt.is_some_and(|rtt| rtt < clear_below) {
                self.streak += 1;
                if self.streak >= SAMPLES {
                    self.pressured = false;
                    self.streak = 0;
                }
            } else {
                self.streak = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Option<Duration> { Some(Duration::from_millis(n)) }

    #[test]
    fn raises_after_two_slow_samples_and_clears_after_two_fast_ones() {
        let mut p = RttPressure::default();
        p.sample(ms(10));
        p.sample(ms(12));
        assert!(!p.is_pressured());
        // Above 3 x 10 ms but below the 2 s floor: not pressure.
        p.sample(ms(500));
        p.sample(ms(500));
        assert!(!p.is_pressured());
        p.sample(ms(2500));
        assert!(!p.is_pressured(), "one sample is not enough");
        p.sample(ms(2500));
        assert!(p.is_pressured());
        // Between the baseline and 1.5 x baseline is needed to clear.
        p.sample(ms(14));
        p.sample(ms(30));
        p.sample(ms(14));
        assert!(p.is_pressured(), "the streak was broken by 30 ms");
        p.sample(ms(14));
        assert!(!p.is_pressured());
    }

    #[test]
    fn a_failure_counts_as_slow_and_never_as_fast() {
        let mut p = RttPressure::default();
        p.sample(ms(10));
        p.sample(None);
        p.sample(None);
        assert!(p.is_pressured());
        p.sample(None);
        p.sample(ms(10));
        assert!(p.is_pressured());
        p.sample(ms(10));
        assert!(!p.is_pressured());
    }

    #[test]
    fn a_large_baseline_raises_the_threshold() {
        let mut p = RttPressure::default();
        p.sample(ms(1000));
        // 3 s is below 3 x 1 s.
        p.sample(ms(2900));
        p.sample(ms(2900));
        assert!(!p.is_pressured());
        p.sample(ms(3100));
        p.sample(ms(3100));
        assert!(p.is_pressured());
    }
}
