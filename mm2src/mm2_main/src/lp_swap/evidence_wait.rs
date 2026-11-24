//! How a legacy swap step waits on a chain read that is undetermined (CRD ch.51 §51.6G R100 items 3 and 9).
//!
//! A read the coin could not determine (no two nodes agreed, or the trusted node did not answer) reaches the
//! engine as an error that [`coins::swap_evidence::is_undetermined`] recognizes. The step that rests on it waits
//! and reads again; it neither refunds nor claims on it. The text the operator sees, and the one that ends a
//! refund step at its cap, are written here so every site uses the same words.

use coins::swap_evidence::is_undetermined;
use common::executor::Timer;
use common::log::{debug, warn};
use common::now_ms;
use std::fmt::Display;
use std::future::Future;
use uuid::Uuid;

/// The clock the cap of an undetermined look or gate is counted on, in seconds since the epoch.
pub(crate) fn clock_secs() -> u64 { now_ms() / 1000 }

/// How long after the first moment a refund could act an undetermined look or gate keeps the step waiting
/// (the span of `wait_refund_until()` after the lock time).
const UNDETERMINED_REFUND_WAIT_SECS: u64 = 3700;

/// The deadline of an undetermined look or gate in a refund chain: the later of `wait_refund_until` and one hour
/// after the first moment the refund could act (`first_could_act`: the first `CanRefundNow` answer of the gate,
/// the first undetermined gate answer at or after the lock time, or the first undetermined look at or after the
/// lock time). `None` while that moment has not come: there is no cap yet.
pub(crate) fn undetermined_refund_deadline(wait_refund_until: u64, first_could_act: Option<u64>) -> Option<u64> {
    first_could_act.map(|first| wait_refund_until.max(first.saturating_add(UNDETERMINED_REFUND_WAIT_SECS)))
}

/// What the operator can do about an undetermined read (CRD ch.51 §51.6G R100 item 9).
const FIX_HINT: &str = "add a server on a second host to the coin's node list or set `swap_evidence_node`";

/// Which read of a refund step could not be determined.
#[derive(Clone, Copy)]
pub(crate) enum Read {
    /// The look at the taker payment for a spend or a refund.
    Look,
    /// The refund gate's check of the chain time.
    Gate,
    /// The check of the taker's payment (the maker's payment check, the watcher's validation).
    TakerPaymentCheck,
    /// The check of the maker's payment (the taker's payment check).
    MakerPaymentCheck,
}

impl Read {
    /// What could not be determined, for the log line.
    fn waiting_on(self) -> &'static str {
        match self {
            Read::Look => "the state of the taker payment",
            Read::Gate => "the chain time of the refund gate",
            Read::TakerPaymentCheck => "the check of the taker payment",
            Read::MakerPaymentCheck => "the check of the maker payment",
        }
    }

    /// What could not be determined, for the event that ends the step.
    fn capped_on(self) -> &'static str {
        match self {
            Read::Look => "the look for a spend or refund of the taker payment",
            Read::Gate => "the refundability check",
            Read::TakerPaymentCheck => "the check of the taker payment",
            Read::MakerPaymentCheck => "the check of the maker payment",
        }
    }

    fn plain_kind(self) -> &'static str {
        match self {
            Read::Look => "the search failed or did not finish",
            Read::Gate | Read::TakerPaymentCheck | Read::MakerPaymentCheck => "the check failed",
        }
    }
}

/// The kind of the failure, named without the error text (CRD ch.51 R80, R81: a warning does not carry an
/// address, host or response text). An undetermined read names the two-node cause; any other failure is named by
/// what kind of read it was, with nothing claimed about its cause.
fn kind(read: Read, reason: &str) -> String {
    if is_undetermined(reason) {
        "(no two nodes agreed, or the trusted node did not answer)".to_owned()
    } else {
        format!("({})", read.plain_kind())
    }
}

/// The text of the log line that tells an operator a step waits on a read that was not determined.
fn wait_text(ticker: &str, read: Read, reason: &str) -> String {
    let fix = if is_undetermined(reason) {
        format!("; {FIX_HINT}")
    } else {
        String::new()
    };
    format!(
        "{} on {ticker} could not be determined {}, the step waits and reads again{fix}",
        read.waiting_on(),
        kind(read, reason)
    )
}

fn cap_text(read: Read, ticker: &str, reason: &str, not_refunded: &str) -> String {
    let head = format!(
        "{} on {ticker} did not give a determined answer before the refund deadline {}; the {not_refunded} payment \
         is not refunded",
        read.capped_on(),
        kind(read, reason)
    );
    if is_undetermined(reason) {
        format!("{head}; {FIX_HINT}, then run `recover_funds_of_swap`")
    } else {
        format!("{head}; run `recover_funds_of_swap` once the coin answers again")
    }
}

/// The text of the event that ends the taker's refund step whose look at the taker payment stayed undetermined
/// until its cap. `reason` is the last error of the look; it only decides the kind and is not part of the text.
pub(crate) fn refund_cap_text(ticker: &str, reason: &str) -> String { cap_text(Read::Look, ticker, reason, "taker") }

/// The text of the event that ends the taker's refund step whose refund gate stayed undetermined until its cap.
pub(crate) fn refund_gate_cap_text(ticker: &str, reason: &str) -> String {
    cap_text(Read::Gate, ticker, reason, "taker")
}

/// The same for the maker's refund step; `ticker` is the taker coin's, whose payment is looked at.
pub(crate) fn maker_refund_cap_text(ticker: &str, reason: &str) -> String {
    cap_text(Read::Look, ticker, reason, "maker")
}

/// The same for the maker's refund gate; `ticker` is the maker coin's.
pub(crate) fn maker_refund_gate_cap_text(ticker: &str, reason: &str) -> String {
    cap_text(Read::Gate, ticker, reason, "maker")
}

/// Logs one situation of a step: the first line at WARN, its repeats at DEBUG.
#[derive(Default)]
pub(crate) struct WaitLog {
    warned: bool,
}

impl WaitLog {
    pub(crate) fn new() -> Self { Self::default() }

    /// Returns whether the line went out at WARN (the first one) rather than at DEBUG. The warning names the
    /// ticker and the kind only; the full `reason` goes to DEBUG, every time.
    pub(crate) fn note(&mut self, swap_uuid: &Uuid, ticker: &str, read: Read, reason: &str) -> bool {
        let text = wait_text(ticker, read, reason);
        let first = !std::mem::replace(&mut self.warned, true);
        if first {
            warn!("Swap {}: {}", swap_uuid, text);
        } else {
            debug!("Swap {}: {}", swap_uuid, text);
        }
        debug!("Swap {}: {} on {}: {}", swap_uuid, read.waiting_on(), ticker, reason);
        first
    }
}

/// `err` with the fix appended when it is an undetermined read, unchanged otherwise. It is for the error a
/// recovery returns to the RPC caller, not for a log line.
pub(crate) fn with_fix_hint(err: String) -> String {
    if is_undetermined(&err) {
        format!("{err}; to fix it, {FIX_HINT}")
    } else {
        err
    }
}

/// Runs `read` until it does not fail with an undetermined read, or until `deadline_secs` (seconds since the
/// epoch) has passed; the last answer is returned then, so the step fails as it does for any other error. Errors
/// that are not undetermined are returned at once. `interval_secs` is the wait between two reads.
pub(crate) async fn retry_while_undetermined<T, E, F, Fut>(
    swap_uuid: &Uuid,
    ticker: &str,
    what: Read,
    interval_secs: f64,
    deadline_secs: u64,
    mut read: F,
) -> Result<T, E>
where
    E: Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut log = WaitLog::new();
    loop {
        let err = match read().await {
            Err(err) => err,
            ok => return ok,
        };
        let text = err.to_string();
        let now = clock_secs();
        if !is_undetermined(&text) || now >= deadline_secs {
            return Err(err);
        }
        log.note(swap_uuid, ticker, what, &text);
        Timer::sleep(interval_secs.min((deadline_secs - now) as f64)).await;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use coins::swap_evidence::undetermined;
    use common::block_on;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn undetermined_reason() -> String { undetermined("the nodes differ", &["node.example:443", "node-b.example"]) }

    const PLAIN_REASON: &str = "error sending request for url (http://node.example:443/): connection refused";

    fn all_texts(reason: &str) -> Vec<String> {
        vec![
            wait_text("BTC", Read::Look, reason),
            wait_text("BTC", Read::Gate, reason),
            refund_cap_text("BTC", reason),
            refund_gate_cap_text("BTC", reason),
        ]
    }

    #[test]
    fn no_text_carries_an_address_a_host_or_the_reason() {
        for reason in [undetermined_reason(), PLAIN_REASON.to_owned()] {
            for text in all_texts(&reason) {
                assert!(!text.contains("node.example"), "{text}");
                assert!(!text.contains("node-b"), "{text}");
                assert!(!text.contains("http"), "{text}");
                assert!(!text.contains("connection refused"), "{text}");
                assert!(text.contains("BTC"), "{text}");
            }
        }
    }

    #[test]
    fn an_undetermined_reason_names_the_cause_and_the_fix() {
        for text in all_texts(&undetermined_reason()) {
            assert!(text.contains("no two nodes agreed"), "{text}");
            assert!(text.contains("second host"), "{text}");
            assert!(text.contains("`swap_evidence_node`"), "{text}");
        }
        for text in [
            refund_cap_text("BTC", &undetermined_reason()),
            refund_gate_cap_text("BTC", &undetermined_reason()),
        ] {
            assert!(text.ends_with("then run `recover_funds_of_swap`"), "{text}");
        }
    }

    #[test]
    fn a_plain_failure_is_named_by_its_kind_and_no_cause_is_claimed() {
        let texts = all_texts(PLAIN_REASON);
        for text in &texts {
            assert!(!text.contains("swap_evidence_node"), "{text}");
            assert!(!text.contains("no two nodes agreed"), "{text}");
        }
        assert!(
            texts[0].contains("(the search failed or did not finish)"),
            "{}",
            texts[0]
        );
        assert!(texts[1].contains("(the check failed)"), "{}", texts[1]);
        assert!(
            texts[2].contains("(the search failed or did not finish)"),
            "{}",
            texts[2]
        );
        assert!(texts[3].contains("(the check failed)"), "{}", texts[3]);
        for text in &texts[2..] {
            assert!(
                text.ends_with("; run `recover_funds_of_swap` once the coin answers again"),
                "{text}"
            );
            assert!(text.contains("the taker payment is not refunded;"), "{text}");
        }
    }

    #[test]
    fn the_maker_texts_name_the_maker_payment_and_carry_no_reason() {
        for reason in [undetermined_reason(), PLAIN_REASON.to_owned()] {
            for text in [
                maker_refund_cap_text("BTC", &reason),
                maker_refund_gate_cap_text("KMD", &reason),
                wait_text("BTC", Read::TakerPaymentCheck, &reason),
                wait_text("BTC", Read::MakerPaymentCheck, &reason),
            ] {
                assert!(!text.contains("node.example") && !text.contains("http"), "{text}");
                assert!(!text.contains("connection refused"), "{text}");
            }
            for text in [
                maker_refund_cap_text("BTC", &reason),
                maker_refund_gate_cap_text("KMD", &reason),
            ] {
                assert!(text.contains("the maker payment is not refunded"), "{text}");
                assert!(!text.contains("the taker payment is not refunded"), "{text}");
            }
        }
        assert!(maker_refund_cap_text("BTC", PLAIN_REASON).contains("(the search failed or did not finish)"));
        assert!(maker_refund_gate_cap_text("KMD", PLAIN_REASON).contains("(the check failed)"));
        assert!(maker_refund_cap_text("BTC", &undetermined_reason()).contains("`swap_evidence_node`"));
    }

    #[test]
    fn the_fix_is_added_to_an_undetermined_error_only() {
        let text = with_fix_hint(undetermined_reason());
        assert!(
            text.contains("second host") && text.contains("`swap_evidence_node`"),
            "{text}"
        );
        assert_eq!(with_fix_hint("plain".to_owned()), "plain");
    }

    #[test]
    fn a_read_is_repeated_while_undetermined_and_returns_the_first_answer() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let deadline = clock_secs() + 100;
        let answer = block_on(retry_while_undetermined(
            &Uuid::nil(),
            "BTC",
            Read::TakerPaymentCheck,
            0.01,
            deadline,
            || async {
                if CALLS.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(undetermined_reason())
                } else {
                    Ok(7)
                }
            },
        ));
        assert_eq!(answer, Ok(7));
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_read_that_stays_undetermined_fails_at_the_deadline() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let answer: Result<u8, String> = block_on(retry_while_undetermined(
            &Uuid::nil(),
            "BTC",
            Read::MakerPaymentCheck,
            0.01,
            clock_secs(),
            || async {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Err(undetermined_reason())
            },
        ));
        assert!(is_undetermined(&answer.unwrap_err()));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn another_error_is_returned_at_once() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let answer: Result<u8, String> = block_on(retry_while_undetermined(
            &Uuid::nil(),
            "BTC",
            Read::TakerPaymentCheck,
            0.01,
            clock_secs() + 100,
            || async {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Err("bad payment".to_owned())
            },
        ));
        assert_eq!(answer.unwrap_err(), "bad payment");
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_the_first_line_of_a_situation_is_a_warning() {
        let mut log = WaitLog::new();
        let uuid = Uuid::nil();
        assert!(log.note(&uuid, "BTC", Read::Look, "x"));
        assert!(!log.note(&uuid, "BTC", Read::Look, "x"));
        assert!(!log.note(&uuid, "BTC", Read::Gate, "y"));
    }
}
