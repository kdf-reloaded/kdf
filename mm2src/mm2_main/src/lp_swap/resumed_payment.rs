//! The question whether a legacy swap's own payment is on chain, asked again within a bound when it cannot be
//! decided (CRD ch.39 §39.11.5A, status note of 2026-11-22; CRD ch.51, status note of 2026-11-28 after R29).
//!
//! A swap restored from a saved log that ends just before the role's payment event may have broadcast its
//! payment before the stop. When the already-sent question cannot be decided (the coin answers an error),
//! such a swap asks again within a bound instead of finishing as unsent. After the bound it treats the
//! payment as possibly sent: it sends nothing, keeps asking at a slower pace, and adopts a payment it finds,
//! so that the ordinary refund chain can recover it.
//!
//! A swap that was not resumed at this step does the same for an answer the coin marks as undetermined
//! ([`coins::swap_evidence::is_undetermined`]); any other error ends its step at once (CRD ch.39 R39.11.19).
//!
//! A payment send that ended with an error which kept the signed transaction is followed in the same way: the
//! coin's read for such a send is asked at the same pace, nothing is sent again, and the step ends as unsent
//! only when that read answers "not sent" twice in a row.

use coins::swap_evidence::{is_undetermined, undetermined_reason_kind};
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

/// Which question [`ask_already_sent_resumed`] asks, and so which answers it asks again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PaymentQuestion {
    /// The already-sent question of a swap resumed at its payment step: every error is asked again.
    Resumed,
    /// The already-sent question of a swap that was not resumed at its payment step: only an error the coin marks
    /// as undetermined is asked again; any other error ends the question at once.
    NotResumed,
    /// The coin's read for a payment whose send ended with an error which kept the signed transaction: every
    /// error is asked again, and "not sent" must be answered twice in a row.
    FailedSend,
}

/// How the question of [`ask_already_sent_resumed`] ended.
pub(crate) enum ResumedPaymentAnswer {
    /// The coin returned the payment; the swap adopts it.
    Found(Box<TransactionEnum>),
    /// The fast phase got the answer "not sent". For the already-sent question the step goes on to its deadline
    /// check and the send; for a failed send (two such answers in a row) the step fails with the send's error.
    NotSent,
    /// The slow phase got the answer "not sent" (for a failed send: the second of two in a row): nothing is sent
    /// any more, the step fails.
    NotSentAfterBound,
    /// The question stayed undetermined until `cap_until`; the text is the coin's last error.
    UndeterminedAtCap(String),
    /// [`PaymentQuestion::NotResumed`] only: the coin answered an error it does not mark as undetermined. The
    /// step fails with this error, as it always did.
    Failed(String),
}

/// What a log line or a stored reason shows of the last error of the question. An error the coin marks as
/// undetermined is named by its kind, which carries no text of a node; so is every error of the read for a failed
/// send. Any other error of the already-sent question of a resumed swap is shown as the coin gave it.
fn shown_error(question: PaymentQuestion, error: &str) -> String {
    match undetermined_reason_kind(error) {
        Some(kind) => kind.to_owned(),
        None if question == PaymentQuestion::FailedSend && error.is_empty() => "none".to_owned(),
        None if question == PaymentQuestion::FailedSend => "the read failed".to_owned(),
        None => error.to_owned(),
    }
}

/// Asks whether the swap's own payment is on chain until the question is decided or `cap_until` (seconds since
/// the epoch) has passed. `ask` runs one query; `question` says which answers are asked again. `role` is
/// "maker" or "taker" and, like `ticker`, is only used in the log lines.
pub(crate) async fn ask_already_sent_resumed<F, Fut>(
    swap_uuid: &Uuid,
    role: &str,
    ticker: &str,
    question: PaymentQuestion,
    pacing: ResumedPaymentPacing,
    cap_until: u64,
    mut ask: F,
) -> ResumedPaymentAnswer
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<TransactionEnum>, String>>,
{
    let failed_send = question == PaymentQuestion::FailedSend;
    if failed_send {
        warn!(
            "Swap {} ({} payment, {}): the payment send ended with an error after the transaction was signed; \
             nothing is sent again, and the chain is asked whether the payment exists until the refund deadline",
            swap_uuid, role, ticker
        );
    }
    let started_ms = now_ms();
    let mut last_error = String::new();
    // A failed send only: the answer before this one was "not sent".
    let mut not_sent_once = false;
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
            Ok(None) if failed_send && !not_sent_once => not_sent_once = true,
            Ok(None) => return ResumedPaymentAnswer::NotSent,
            Err(e) if question == PaymentQuestion::NotResumed && !is_undetermined(&e) => {
                return ResumedPaymentAnswer::Failed(e)
            },
            Err(e) => {
                debug!(
                    "Swap {} ({} payment, {}): the already-sent check is undetermined (attempt {}): {}",
                    swap_uuid, role, ticker, attempt, e
                );
                last_error = e;
                not_sent_once = false;
            },
        }
    }

    if failed_send {
        warn!(
            "Swap {} ({} payment, {}): whether the payment of the failed send exists stayed undetermined; nothing \
             is sent, and the chain is asked every {} s until the refund deadline; kind of the last error: {}",
            swap_uuid,
            role,
            ticker,
            pacing.slow_interval_secs,
            shown_error(question, &last_error)
        );
    } else {
        warn!(
            "Swap {} ({} payment, {}): the already-sent check stayed undetermined; the payment is treated as \
             possibly sent, nothing is sent, and the check is repeated every {} s until the refund deadline: {}",
            swap_uuid,
            role,
            ticker,
            pacing.slow_interval_secs,
            shown_error(question, &last_error)
        );
    }
    loop {
        let now = now_ms() / 1000;
        if now >= cap_until {
            return ResumedPaymentAnswer::UndeterminedAtCap(last_error);
        }
        // A "not sent" of a failed send is asked once more after one fast interval.
        let interval_secs = if not_sent_once {
            pacing.fast_interval_secs
        } else {
            pacing.slow_interval_secs
        };
        Timer::sleep(interval_secs.min((cap_until - now) as f64)).await;
        match ask().await {
            Ok(Some(tx)) => return ResumedPaymentAnswer::Found(Box::new(tx)),
            Ok(None) if failed_send && !not_sent_once => not_sent_once = true,
            Ok(None) => return ResumedPaymentAnswer::NotSentAfterBound,
            Err(e) if question == PaymentQuestion::NotResumed && !is_undetermined(&e) => {
                return ResumedPaymentAnswer::Failed(e)
            },
            Err(e) => {
                debug!(
                    "Swap {} ({} payment, {}): the already-sent check is still undetermined: {}",
                    swap_uuid, role, ticker, e
                );
                last_error = e;
                not_sent_once = false;
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
/// `question` is [`PaymentQuestion::Resumed`] or [`PaymentQuestion::NotResumed`]; an error the coin marks as
/// undetermined is named by its kind only.
pub(crate) fn undetermined_at_cap_text(
    role: &str,
    ticker: &str,
    question: PaymentQuestion,
    last_error: &str,
) -> String {
    format!(
        "the already-sent check of the {} payment on {} stayed undetermined until the refund deadline; nothing was \
         sent; if a payment exists, recover_funds_of_swap recovers it; last error: {}",
        role,
        ticker,
        shown_error(question, last_error)
    )
}

/// The text of the payment-failed event when, after a payment send that ended with an error, the question
/// whether the payment reached the chain stayed undetermined until the refund deadline. `last_error` only decides
/// the kind that is named; no part of it is in the text.
pub(crate) fn failed_send_undetermined_at_cap_text(role: &str, ticker: &str, last_error: &str) -> String {
    format!(
        "the send of the {} payment on {} ended with an error, and whether the payment reached the chain stayed \
         undetermined until the refund deadline; nothing more was sent; if a payment exists, recover_funds_of_swap \
         recovers it; kind of the last error: {}",
        role,
        ticker,
        shown_error(PaymentQuestion::FailedSend, last_error)
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use coins::swap_evidence::undetermined;

    const NODE: &str = "node-a.example:8545";
    const PLAIN: &str = "connection refused by node-b.example:8545";

    /// An error the coin marks as undetermined is shown by its kind for every question: a reason the core knows by
    /// its phrase, any other reason as "undetermined", and never with the node the coin named.
    #[test]
    fn a_marked_error_is_shown_by_its_kind_only() {
        let known = undetermined("the nodes disagree", &[NODE]);
        let free = undetermined("the read of the payment state failed", &[NODE]);
        let questions = [
            PaymentQuestion::Resumed,
            PaymentQuestion::NotResumed,
            PaymentQuestion::FailedSend,
        ];
        for question in questions {
            assert_eq!(shown_error(question, &known), "the nodes disagree");
            assert_eq!(shown_error(question, &free), "undetermined");
        }
    }

    /// A plain error of the already-sent question of a resumed swap is shown as the coin gave it, as before; the
    /// read for a failed send names the kind of a plain error only.
    #[test]
    fn a_plain_error_is_shown_in_full_only_for_a_resumed_swap() {
        assert_eq!(shown_error(PaymentQuestion::Resumed, PLAIN), PLAIN);
        assert_eq!(shown_error(PaymentQuestion::FailedSend, PLAIN), "the read failed");
        assert_eq!(shown_error(PaymentQuestion::FailedSend, ""), "none");
    }

    /// The texts of the payment-failed event at the refund deadline name the manual recovery, and carry no part
    /// of a marked error but its kind.
    #[test]
    fn the_cap_texts_name_the_manual_recovery_and_no_node() {
        let marked = undetermined("the nodes disagree", &[NODE]);
        let already_sent = undetermined_at_cap_text("maker", "ETH", PaymentQuestion::NotResumed, &marked);
        let failed_send = failed_send_undetermined_at_cap_text("taker", "ETH", PLAIN);
        for text in [&already_sent, &failed_send] {
            assert!(
                text.contains("recover_funds_of_swap"),
                "a cap text does not name the manual recovery"
            );
            assert!(
                !text.contains("node-a.example") && !text.contains("node-b.example"),
                "a cap text names a node"
            );
        }
        assert!(
            already_sent.ends_with("last error: the nodes disagree"),
            "the cap text of the already-sent question does not end with the kind of the last error"
        );
        assert!(
            failed_send.contains("nothing more was sent")
                && failed_send.ends_with("kind of the last error: the read failed"),
            "the cap text of a failed send does not say that nothing more was sent, or does not end with the kind"
        );
    }
}
