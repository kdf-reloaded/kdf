//! The stale-tip warning of a refund wait (CRD ch.51 R79, R80, R81).
//!
//! While a swap waits to refund its own payment, the coin only answers "wait this many seconds" and never
//! says why the answer does not change. A stalled node therefore looks like an ordinary wait. The wait is
//! cut into slices of at most five minutes; at the start of a slice the tip height of the paying coin is read
//! alongside the slice's sleep, and when the height has not changed for an hour one warning is written (and
//! again at most hourly while it stays the same).
//!
//! The read is never on the path of the wait: it is abandoned when the sleep of its slice ends. The warning
//! changes nothing else: the length of the wait, the refund decision, the events and the state are those of
//! the unchanged wait. The warning names the coin by its ticker only.

use coins::MmCoinEnum;
use common::executor::Timer;
use common::log::{debug, warn};
use common::now_ms;
use futures::compat::Future01CompatExt;
use futures::future::Either;
use std::future::Future;
use uuid::Uuid;

/// A height that has not changed for this long is a stalled tip (R80).
pub(crate) const STALE_TIP_AFTER_SECS: u64 = 3600;
/// While the height stays the same, the warning is repeated no more often than this (R80).
pub(crate) const STALE_TIP_REPEAT_SECS: u64 = 3600;
/// The tip is read once per this long, and a wait is cut into slices of at most this long (R79).
pub(crate) const STALE_TIP_SAMPLE_SECS: u64 = 300;
/// The clock counts whole seconds, so a read is due one second earlier than a full sample period (R79).
const STALE_TIP_SAMPLE_SLACK_SECS: u64 = 1;

/// A tip height that has not changed for `minutes` whole minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaleTip {
    pub(crate) height: u64,
    pub(crate) minutes: u64,
}

/// What a refund wait has seen of the tip height. The clock is passed in; this type reads none.
#[derive(Debug, Default)]
pub(crate) struct StaleTipWatch {
    /// The last observed height and the time (seconds) it was first seen.
    seen: Option<(u64, u64)>,
    /// The time (seconds) of the last warning for the current height.
    last_warned: Option<u64>,
    /// The time (seconds) the height was last asked for.
    last_read: Option<u64>,
}

impl StaleTipWatch {
    pub(crate) fn new() -> Self { Self::default() }

    /// Records the height `height` seen at `now_secs`. Returns the warning to write, if one is due.
    pub(crate) fn observe(&mut self, now_secs: u64, height: u64) -> Option<StaleTip> {
        let since = match self.seen {
            Some((seen_height, since)) if seen_height == height => since,
            _ => {
                self.seen = Some((height, now_secs));
                self.last_warned = None;
                return None;
            },
        };
        let stalled_secs = now_secs.saturating_sub(since);
        if stalled_secs < STALE_TIP_AFTER_SECS {
            return None;
        }
        if let Some(last_warned) = self.last_warned {
            if now_secs.saturating_sub(last_warned) < STALE_TIP_REPEAT_SECS {
                return None;
            }
        }
        self.last_warned = Some(now_secs);
        Some(StaleTip {
            height,
            minutes: stalled_secs / 60,
        })
    }

    /// Whether the height is to be read now: the first time, and then when a sample period has passed since
    /// the last read. A wait that is cut into many short steps thus reads the coin once per period, not once
    /// per step. Calling it counts as a read, whether or not the read finishes in time.
    fn read_is_due(&mut self, now_secs: u64) -> bool {
        if let Some(last_read) = self.last_read {
            if now_secs.saturating_sub(last_read) + STALE_TIP_SAMPLE_SLACK_SECS < STALE_TIP_SAMPLE_SECS {
                return false;
            }
        }
        self.last_read = Some(now_secs);
        true
    }
}

/// The text of the warning. It names the swap and the coin ticker, never a server address or response text.
fn stale_tip_text(uuid: &Uuid, ticker: &str, stale: &StaleTip) -> String {
    format!(
        "Swap {}: the {} tip height {} has not changed for {} minutes while this swap waits to refund its \
         payment; the {} node may be stalled or behind",
        uuid, ticker, stale.height, stale.minutes, ticker
    )
}

/// Sleeps `total_secs` like `Timer::sleep(total_secs)`, reading the tip height of `coin` alongside the sleep
/// and writing the stale-tip warning when it is due. The total sleeping time is exactly `total_secs`.
pub(crate) async fn sleep_observing_tip(coin: &MmCoinEnum, uuid: &Uuid, total_secs: f64, watch: &mut StaleTipWatch) {
    sleep_observing(
        coin.ticker(),
        uuid,
        total_secs,
        watch,
        || coin.current_block().compat(),
        Timer::sleep,
        || now_ms() / 1000,
    )
    .await;
}

/// [`sleep_observing_tip`] with the height source, the sleeper and the clock given, so a test needs no real
/// time. Returns the warnings it wrote.
async fn sleep_observing<H, HF, S, SF, C>(
    ticker: &str,
    uuid: &Uuid,
    total_secs: f64,
    watch: &mut StaleTipWatch,
    mut read_height: H,
    mut sleep: S,
    mut now_secs: C,
) -> Vec<StaleTip>
where
    H: FnMut() -> HF,
    HF: Future<Output = Result<u64, String>>,
    S: FnMut(f64) -> SF,
    SF: Future<Output = ()>,
    C: FnMut() -> u64,
{
    let mut warnings = Vec::new();
    let mut remaining = total_secs;
    loop {
        let slice = remaining.min(STALE_TIP_SAMPLE_SECS as f64);
        let sleeper = sleep(slice);
        if watch.read_is_due(now_secs()) {
            sleep_beside_read(
                sleeper,
                read_height(),
                ticker,
                uuid,
                watch,
                &mut now_secs,
                &mut warnings,
            )
            .await;
        } else {
            sleeper.await;
        }
        remaining -= slice;
        if remaining.is_nan() || remaining <= 0. {
            break;
        }
    }
    warnings
}

/// Runs `sleeper` to its end and `read` alongside it. The slice ends exactly when the sleep ends: a read that
/// finishes first is observed and the sleep is awaited on; a read that has not finished when the sleep ends
/// is dropped, and a failed read is skipped (R79). Nothing here waits for the read after the sleep is over.
async fn sleep_beside_read<SF, HF, C>(
    sleeper: SF,
    read: HF,
    ticker: &str,
    uuid: &Uuid,
    watch: &mut StaleTipWatch,
    now_secs: &mut C,
    warnings: &mut Vec<StaleTip>,
) where
    SF: Future<Output = ()>,
    HF: Future<Output = Result<u64, String>>,
    C: FnMut() -> u64,
{
    futures::pin_mut!(sleeper);
    futures::pin_mut!(read);
    match futures::future::select(read, sleeper).await {
        Either::Left((Ok(height), sleeper)) => {
            if let Some(stale) = watch.observe(now_secs(), height) {
                warn!("{}", stale_tip_text(uuid, ticker, &stale));
                warnings.push(stale);
            }
            sleeper.await;
        },
        Either::Left((Err(_), sleeper)) => {
            debug!(
                "Swap {}: the {} tip height could not be read during the refund wait",
                uuid, ticker
            );
            sleeper.await;
        },
        Either::Right(((), _unfinished_read)) => {
            debug!(
                "Swap {}: the {} tip height read did not finish before the sleep ended",
                uuid, ticker
            );
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use std::cell::{Cell, RefCell};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    const START: u64 = 1_000_000;
    const TICKER: &str = "RIN";
    /// How many times the fake sleeper answers "not yet" before it ends the sleep.
    const SLEEP_POLLS: u32 = 3;

    fn ticks(minutes: u64) -> u64 { minutes * 60 }

    /// A future that answers "not yet" `polls` times and then is ready.
    struct Yields(u32);

    impl Future for Yields {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 == 0 {
                return Poll::Ready(());
            }
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    /// What the fake coin does with a read of the tip height.
    enum Read {
        /// Answers at once.
        Answer(Result<u64, String>),
        /// Answers after one "not yet", which is before the fake sleep ends.
        Slow(Result<u64, String>),
        /// Never answers.
        Never,
    }

    async fn fake_read(read: Read) -> Result<u64, String> {
        match read {
            Read::Answer(answer) => answer,
            Read::Slow(answer) => {
                Yields(1).await;
                answer
            },
            Read::Never => std::future::pending::<Result<u64, String>>().await,
        }
    }

    /// Runs the helper over a fake clock that moves when a fake sleep ends. `height_at` is asked, with the
    /// seconds since the start, whenever the helper starts a read. Returns the warnings and the sleeps.
    fn run_fake_wait<F>(total_secs: f64, mut height_at: F) -> (Vec<StaleTip>, Vec<f64>)
    where
        F: FnMut(u64) -> Read,
    {
        let clock = Cell::new(START);
        let sleeps = RefCell::new(Vec::new());
        let (clock_ref, sleeps_ref) = (&clock, &sleeps);
        let mut watch = StaleTipWatch::new();
        let warnings = block_on(sleep_observing(
            TICKER,
            &Uuid::nil(),
            total_secs,
            &mut watch,
            move || fake_read(height_at(clock_ref.get() - START)),
            move |secs| async move {
                sleeps_ref.borrow_mut().push(secs);
                Yields(SLEEP_POLLS).await;
                clock_ref.set(clock_ref.get() + secs as u64);
            },
            move || clock_ref.get(),
        ));
        (warnings, sleeps.into_inner())
    }

    fn sixty_minutes() -> StaleTip {
        StaleTip {
            height: 700,
            minutes: 60,
        }
    }

    #[test]
    fn t33_no_warning_at_59_minutes_one_at_60_naming_60() {
        let mut watch = StaleTipWatch::new();
        assert_eq!(watch.observe(START, 700), None);
        assert_eq!(watch.observe(START + ticks(59), 700), None);
        assert_eq!(watch.observe(START + ticks(60) - 1, 700), None);
        assert_eq!(
            watch.observe(START + ticks(60), 700),
            Some(StaleTip {
                height: 700,
                minutes: 60
            })
        );
    }

    #[test]
    fn t33_no_second_warning_before_120_minutes_second_at_120() {
        let mut watch = StaleTipWatch::new();
        assert_eq!(watch.observe(START, 700), None);
        assert!(watch.observe(START + ticks(60), 700).is_some());
        assert_eq!(watch.observe(START + ticks(90), 700), None);
        assert_eq!(watch.observe(START + ticks(119), 700), None);
        assert_eq!(
            watch.observe(START + ticks(120), 700),
            Some(StaleTip {
                height: 700,
                minutes: 120
            })
        );
    }

    #[test]
    fn t33_height_change_ends_the_stall_and_starts_a_new_clock() {
        let mut watch = StaleTipWatch::new();
        assert_eq!(watch.observe(START, 700), None);
        assert!(watch.observe(START + ticks(60), 700).is_some());
        // The height moves: no warning, and the stall counts from here.
        let moved_at = START + ticks(70);
        assert_eq!(watch.observe(moved_at, 701), None);
        assert_eq!(watch.observe(moved_at + ticks(59), 701), None);
        assert_eq!(
            watch.observe(moved_at + ticks(60), 701),
            Some(StaleTip {
                height: 701,
                minutes: 60
            })
        );
    }

    #[test]
    fn t33_height_change_before_the_first_warning_resets_the_clock() {
        let mut watch = StaleTipWatch::new();
        assert_eq!(watch.observe(START, 700), None);
        assert_eq!(watch.observe(START + ticks(50), 701), None);
        assert_eq!(watch.observe(START + ticks(109), 701), None);
    }

    #[test]
    fn t33_first_observation_only_records_the_height() {
        let mut watch = StaleTipWatch::new();
        // Even a late first observation is a baseline and gives no warning.
        assert_eq!(watch.observe(START + ticks(500), 700), None);
        assert_eq!(watch.observe(START + ticks(559), 700), None);
        assert!(watch.observe(START + ticks(560), 700).is_some());
    }

    #[test]
    fn t33_failed_reads_are_skipped_and_do_not_reset_the_clock() {
        // The height never changes; reads fail between the 10th and the 50th minute. The stall still counts
        // from the first successful read, so the warning comes at 60 minutes.
        let (warnings, _) = run_fake_wait(3900., |t| {
            if (ticks(10)..=ticks(50)).contains(&t) {
                Read::Answer(Err("node unreachable".to_string()))
            } else {
                Read::Answer(Ok(700))
            }
        });
        assert_eq!(warnings, vec![sixty_minutes()]);
    }

    #[test]
    fn t33_a_wait_with_only_failed_reads_gives_no_warning() {
        let (warnings, _) = run_fake_wait(7500., |_| Read::Answer(Err("node unreachable".to_string())));
        assert!(warnings.is_empty());
    }

    #[test]
    fn t33_a_read_that_does_not_finish_before_the_sleep_ends_is_skipped() {
        // Reads that start between the 10th and the 50th minute never finish and are dropped when their sleep
        // ends: no warning from them, the clock is not reset (the warning still comes at 60 minutes) and the
        // wait keeps its full length.
        let (warnings, sleeps) = run_fake_wait(3900., |t| {
            if (ticks(10)..=ticks(50)).contains(&t) {
                Read::Never
            } else {
                Read::Answer(Ok(700))
            }
        });
        assert_eq!(warnings, vec![sixty_minutes()]);
        assert_eq!(sleeps.iter().sum::<f64>(), 3900.);
    }

    #[test]
    fn t33_pending_reads_do_not_extend_the_wait() {
        // Every read hangs: no warning, and the slices still add up to the wait, each sleep awaited to its end.
        let (warnings, sleeps) = run_fake_wait(7500., |_| Read::Never);
        assert!(warnings.is_empty());
        assert!(sleeps.iter().all(|s| *s <= STALE_TIP_SAMPLE_SECS as f64));
        assert_eq!(sleeps.iter().sum::<f64>(), 7500.);
    }

    #[test]
    fn t33_a_read_that_finishes_during_the_sleep_is_used() {
        // Each read answers after one "not yet", before its sleep ends; the sleep is still awaited to its end.
        let (warnings, sleeps) = run_fake_wait(3900., |_| Read::Slow(Ok(700)));
        assert_eq!(warnings, vec![sixty_minutes()]);
        assert_eq!(sleeps.iter().sum::<f64>(), 3900.);
    }

    #[test]
    fn t33_warning_text_has_the_ticker_and_no_address() {
        let text = stale_tip_text(&Uuid::nil(), TICKER, &sixty_minutes());
        assert!(text.contains(TICKER));
        assert!(text.contains("700"));
        assert!(text.contains("60 minutes"));
        assert!(!text.contains("http"));
        assert!(!text.contains("://"));
    }

    #[test]
    fn t33_fixed_height_over_two_hours_warns_exactly_twice_in_slices_of_five_minutes() {
        // 25 slices; the reads start with the slices, at 0, 5, ... 120 minutes.
        let (warnings, sleeps) = run_fake_wait(7500., |_| Read::Answer(Ok(700)));
        assert!(sleeps.iter().all(|s| *s <= STALE_TIP_SAMPLE_SECS as f64));
        assert_eq!(sleeps.iter().sum::<f64>(), 7500.);
        assert_eq!(warnings, vec![sixty_minutes(), StaleTip {
            height: 700,
            minutes: 120
        }]);
    }

    #[test]
    fn t33_a_height_that_moves_every_half_hour_never_warns() {
        let (warnings, sleeps) = run_fake_wait(7200., |t| Read::Answer(Ok(700 + t / ticks(30))));
        assert!(warnings.is_empty());
        assert_eq!(sleeps.iter().sum::<f64>(), 7200.);
    }

    #[test]
    fn t33_sleep_total_is_kept_for_waits_that_are_not_multiples_of_a_slice() {
        let (_, sleeps) = run_fake_wait(1000., |_| Read::Answer(Ok(700)));
        assert_eq!(sleeps, vec![300., 300., 300., 100.]);
        let (_, sleeps) = run_fake_wait(120., |_| Read::Answer(Ok(700)));
        assert_eq!(sleeps, vec![120.]);
        // A zero wait still sleeps once, like the unchanged `Timer::sleep(0.)`.
        let (_, sleeps) = run_fake_wait(0., |_| Read::Answer(Ok(700)));
        assert_eq!(sleeps, vec![0.]);
    }

    #[test]
    fn t33_short_steps_read_the_height_once_per_sample_period() {
        // A wait cut into steps of 5 seconds by the caller reads the coin once per 5 minutes, not at every step.
        let clock = Cell::new(START);
        let reads = Cell::new(0u32);
        let (clock_ref, reads_ref) = (&clock, &reads);
        let mut watch = StaleTipWatch::new();
        for _ in 0..120 {
            block_on(sleep_observing(
                TICKER,
                &Uuid::nil(),
                5.,
                &mut watch,
                move || {
                    reads_ref.set(reads_ref.get() + 1);
                    fake_read(Read::Answer(Ok(700)))
                },
                move |secs| async move {
                    Yields(SLEEP_POLLS).await;
                    clock_ref.set(clock_ref.get() + secs as u64);
                },
                move || clock_ref.get(),
            ));
        }
        // 600 seconds in total: reads at the start and after 5 minutes.
        assert_eq!(reads.get(), 2);
    }
}
