//! Log level of the relay mesh watermark lines.
//!
//! Reloaded change (log levels, not security-related). `maintain_relays_mesh` runs every 10 seconds, so a
//! relay mesh that stays below its low watermark (or above its high watermark) would print the same line 360
//! times an hour. [`WatermarkReport`] decides the level of each sighting: INFO the first time the mesh is
//! outside the watermark, on every change of the mesh size while it stays outside, and as a reminder every
//! [`REMINDER_INTERVAL`]; DEBUG otherwise.
//!
//! The report takes the current time as an argument so it is testable without sleeping.

use instant::Instant;
use log::Level;
use std::time::Duration;

/// How often the INFO line is repeated while the mesh size does not change.
pub(crate) const REMINDER_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The state of one watermark line (low or high).
#[derive(Debug, Default)]
pub(crate) struct WatermarkReport {
    /// The mesh size of the last INFO line, `None` while the mesh is inside the watermark.
    reported_size: Option<usize>,
    last_info: Option<Instant>,
}

impl WatermarkReport {
    /// Observes the mesh `size` at `now` and returns the level of the line to log. `outside` tells whether the
    /// mesh is outside the watermark; it must be passed on every observation, so that leaving and re-entering
    /// the watermark range counts as a new first time. When `outside` is `false` there is nothing to log.
    pub(crate) fn level(&mut self, size: usize, outside: bool, now: Instant) -> Level {
        if !outside {
            self.reported_size = None;
            self.last_info = None;
            return Level::Debug;
        }
        let changed = self.reported_size != Some(size);
        let reminder_due = self
            .last_info
            .map_or(true, |last| now.saturating_duration_since(last) >= REMINDER_INTERVAL);
        if changed || reminder_due {
            self.reported_size = Some(size);
            self.last_info = Some(now);
            Level::Info
        } else {
            Level::Debug
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_on_first_change_and_reminder_debug_otherwise() {
        let mut report = WatermarkReport::default();
        let start = Instant::now();

        // The first sighting below the watermark.
        assert_eq!(report.level(3, true, start), Level::Info);
        // The same size every 10 s stays quiet.
        for i in 1..60 {
            assert_eq!(report.level(3, true, start + Duration::from_secs(10 * i)), Level::Debug);
        }
        // A change of the size is reported at once.
        assert_eq!(report.level(2, true, start + Duration::from_secs(601)), Level::Info);
        // The reminder is measured from the last INFO line.
        assert_eq!(report.level(2, true, start + Duration::from_secs(601 + 599)), Level::Debug);
        assert_eq!(report.level(2, true, start + Duration::from_secs(601 + 600)), Level::Info);
        assert_eq!(report.level(2, true, start + Duration::from_secs(601 + 610)), Level::Debug);
    }

    #[test]
    fn leaving_and_re_entering_is_a_new_first_time() {
        let mut report = WatermarkReport::default();
        let start = Instant::now();
        assert_eq!(report.level(3, true, start), Level::Info);
        assert_eq!(report.level(4, false, start + Duration::from_secs(10)), Level::Debug);
        assert_eq!(report.level(3, true, start + Duration::from_secs(20)), Level::Info);
    }

    #[test]
    fn inside_the_watermark_is_never_info() {
        let mut report = WatermarkReport::default();
        let start = Instant::now();
        for i in 0..5 {
            assert_eq!(report.level(5, false, start + Duration::from_secs(10 * i)), Level::Debug);
        }
    }
}
