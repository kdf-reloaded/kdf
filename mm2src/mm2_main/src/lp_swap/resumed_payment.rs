//! The already-sent question of a legacy swap resumed at its payment step (CRD ch.39 §39.11.5A, status note of
//! 2026-11-22).
//!
//! A swap restored from a saved log that ends just before the role's payment event may have broadcast its
//! payment before the stop. When the already-sent question cannot be decided (the coin answers an error),
//! such a swap asks again within a bound instead of finishing as unsent. After the bound it treats the
//! payment as possibly sent: it sends nothing, keeps asking at a slower pace, and adopts a payment it finds,
//! so that the ordinary refund chain can recover it. A swap that was not resumed at this step asks once
//! (CRD ch.39 R39.11.19).

use coins::TransactionEnum;
use common::executor::Timer;
use common::log::{debug, warn};
use common::now_ms;
use std::future::Future;
use uuid::Uuid;

/// The wait between two attempts of the fast phase: the pace the refund stages use after a failed read.
const FAST_INTERVAL_SECS: f64 = 30.;
/// The attempts of the fast phase, the first one included: long enough for a node that is catching up, short
/// enough that an answer of "not sent" can still be followed by a send inside the step's deadline.
const FAST_ATTEMPTS: usize = 10;
/// No attempt of the fast phase starts later than this after the step began, so slow answers cannot stretch
/// the phase beyond a known length.
const FAST_WINDOW_SECS: f64 = 600.;
/// The wait between two attempts of the slow phase: nothing is sent in it, so it only has to find a payment
/// well before the refund deadline.
const SLOW_INTERVAL_SECS: f64 = 120.;

/// The waits of [`ask_already_sent_resumed`], given so a test can shorten them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResumedPaymentPacing {
    pub fast_interval_secs: f64,
    pub fast_attempts: usize,
    pub fast_window_secs: f64,
    pub slow_interval_secs: f64,
}

/// The pacing a running swap uses.
pub(crate) const RESUMED_PAYMENT_PACING: ResumedPaymentPacing = ResumedPaymentPacing {
    fast_interval_secs: FAST_INTERVAL_SECS,
    fast_attempts: FAST_ATTEMPTS,
    fast_window_secs: FAST_WINDOW_SECS,
    slow_interval_secs: SLOW_INTERVAL_SECS,
};

/// How the already-sent question of a swap resumed at its payment step ended.
pub(crate) enum ResumedPaymentAnswer {
    /// The coin returned the payment; the swap adopts it.
    Found(Box<TransactionEnum>),
    /// The fast phase got the answer "not sent": the step goes on to its deadline check and the send.
    NotSent,
    /// The slow phase got the answer "not sent": nothing is sent any more, the step fails.
    NotSentAfterBound,
    /// The question stayed undetermined until `cap_until`; the text is the coin's last error.
    UndeterminedAtCap(String),
}

/// Asks the already-sent question of a swap resumed at its payment step until it is decided or `cap_until`
/// (seconds since the epoch) has passed. `ask` runs one query. `role` is "maker" or "taker" and, like
/// `ticker`, is only used in the log lines.
pub(crate) async fn ask_already_sent_resumed<F, Fut>(
    swap_uuid: &Uuid,
    role: &str,
    ticker: &str,
    pacing: ResumedPaymentPacing,
    cap_until: u64,
    mut ask: F,
) -> ResumedPaymentAnswer
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<TransactionEnum>, String>>,
{
    let started_ms = now_ms();
    let mut last_error = String::new();
    for attempt in 1..=pacing.fast_attempts {
        if attempt > 1 {
            Timer::sleep(pacing.fast_interval_secs).await;
            let elapsed_secs = now_ms().saturating_sub(started_ms) as f64 / 1000.;
            if elapsed_secs >= pacing.fast_window_secs {
                break;
            }
        }
        match ask().await {
            Ok(Some(tx)) => return ResumedPaymentAnswer::Found(Box::new(tx)),
            Ok(None) => return ResumedPaymentAnswer::NotSent,
            Err(e) => {
                debug!(
                    "Swap {} ({} payment, {}): the already-sent check is undetermined (attempt {}): {}",
                    swap_uuid, role, ticker, attempt, e
                );
                last_error = e;
            },
        }
    }

    warn!(
        "Swap {} ({} payment, {}): the already-sent check stayed undetermined; the payment is treated as possibly \
         sent, nothing is sent, and the check is repeated every {} s until the refund deadline: {}",
        swap_uuid, role, ticker, pacing.slow_interval_secs, last_error
    );
    loop {
        let now = now_ms() / 1000;
        if now >= cap_until {
            return ResumedPaymentAnswer::UndeterminedAtCap(last_error);
        }
        Timer::sleep(pacing.slow_interval_secs.min((cap_until - now) as f64)).await;
        match ask().await {
            Ok(Some(tx)) => return ResumedPaymentAnswer::Found(Box::new(tx)),
            Ok(None) => return ResumedPaymentAnswer::NotSentAfterBound,
            Err(e) => {
                debug!(
                    "Swap {} ({} payment, {}): the already-sent check is still undetermined: {}",
                    swap_uuid, role, ticker, e
                );
                last_error = e;
            },
        }
    }
}

/// The text of the payment-failed event when the slow phase got the answer "not sent".
pub(crate) fn not_sent_after_bound_text(role: &str, ticker: &str) -> String {
    format!(
        "the already-sent check of the {} payment on {} was undetermined and now reports no payment; nothing was sent",
        role, ticker
    )
}

/// The text of the payment-failed event when the question stayed undetermined until the refund deadline.
pub(crate) fn undetermined_at_cap_text(role: &str, ticker: &str, last_error: &str) -> String {
    format!(
        "the already-sent check of the {} payment on {} stayed undetermined until the refund deadline; nothing was \
         sent; if a payment exists, recover_funds_of_swap recovers it; last error: {}",
        role, ticker, last_error
    )
}
