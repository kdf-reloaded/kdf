//! # Purpose
//!
//! The suspension backoff shared by the Electrum pool and the lightwalletd backend (CRD ch.38 R38.6.4d item 1,
//! ch.39 R39.11.31): a server that keeps failing stays out of the way for a growing time, and one answer from it
//! brings it back at once.
//!
//! # Public exports
//!
//! - [`Suspension`] — the state of one server.
//! - [`SUSPENSION_BASE`] / [`SUSPENSION_CAP`] — the schedule both backends use: 20 s, doubling, up to 12 hours.
//!
//! # Invariants
//!
//! - The state is a pure function of the failures it has seen and of the times they happened, so the cap is tested
//!   by feeding it timestamps instead of waiting.
//! - Nothing here knows what a failure is. The caller decides which failures suspend (a transport-level failure)
//!   and which never do (a service-level answer).

use std::time::Duration;

/// The first suspension of a server.
pub const SUSPENSION_BASE: Duration = Duration::from_secs(20);

/// The longest suspension of a server.
pub const SUSPENSION_CAP: Duration = Duration::from_secs(12 * 60 * 60);

/// The time a failing server stays suspended.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Suspension {
    /// Disconnections and failed attempts since the last successful answer.
    failures: u32,
    /// The time (seconds since the epoch) the suspension lapses.
    until: f64,
}

impl Suspension {
    /// Records a failed attempt at `now` and returns the length of the suspension in seconds: `base` for the
    /// first failure, doubled by each further one, never more than `cap`.
    pub(crate) fn on_failure(&mut self, now: f64, base: Duration, cap: Duration) -> f64 {
        let doubled = base.as_secs_f64() * 2f64.powi(self.failures.min(60) as i32);
        let secs = doubled.min(cap.as_secs_f64());
        self.failures = self.failures.saturating_add(1);
        self.until = now + secs;
        secs
    }

    /// A successful answer resets the server: its next suspension is the base one again.
    pub(crate) fn on_success(&mut self) { *self = Suspension::default(); }

    pub(crate) fn is_suspended(&self, now: f64) -> bool { now < self.until }

    /// The time (seconds since the epoch) the suspension lapses.
    #[cfg(test)]
    pub(crate) fn until(&self) -> f64 { self.until }

    /// Disconnections and failed attempts since the last successful answer.
    #[cfg(test)]
    pub(crate) fn failures(&self) -> u32 { self.failures }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_from_the_base_up_to_the_cap_and_resets_on_success() {
        let mut suspension = Suspension::default();
        assert!(!suspension.is_suspended(0.));
        assert_eq!(suspension.on_failure(0., SUSPENSION_BASE, SUSPENSION_CAP), 20.);
        assert_eq!(suspension.on_failure(0., SUSPENSION_BASE, SUSPENSION_CAP), 40.);
        assert!(suspension.is_suspended(39.5));
        assert!(!suspension.is_suspended(40.));
        for _ in 0..40 {
            suspension.on_failure(0., SUSPENSION_BASE, SUSPENSION_CAP);
        }
        assert_eq!(
            suspension.on_failure(0., SUSPENSION_BASE, SUSPENSION_CAP) as u64,
            12 * 3600
        );
        suspension.on_success();
        assert!(!suspension.is_suspended(0.));
        assert_eq!(suspension.on_failure(0., SUSPENSION_BASE, SUSPENSION_CAP), 20.);
    }
}
