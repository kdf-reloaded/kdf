//! How a legacy swap step waits on a chain read that is undetermined (CRD ch.51 §51.6G R100 items 3 and 9).
//!
//! A read the coin could not determine (no two nodes agreed, or the trusted node did not answer) reaches the
//! engine as an error that [`coins::swap_evidence::is_undetermined`] recognizes. The step that rests on it waits
//! and reads again; it neither refunds nor claims on it. The text the operator sees, and the one that ends a
//! refund step at its cap, are written here so every site uses the same words.

use coins::swap_evidence::is_undetermined;
use common::log::{debug, warn};
use uuid::Uuid;

/// What the operator can do about an undetermined read (CRD ch.51 §51.6G R100 item 9).
const FIX_HINT: &str = "add a server on a second host to the coin's node list or set `swap_evidence_node`";

/// Which read of a refund step could not be determined.
#[derive(Clone, Copy)]
pub(crate) enum Read {
    /// The look at the taker payment for a spend or a refund.
    Look,
    /// The refund gate's check of the chain time.
    Gate,
}

impl Read {
    /// What could not be determined, for the log line.
    fn waiting_on(self) -> &'static str {
        match self {
            Read::Look => "the state of the taker payment",
            Read::Gate => "the chain time of the refund gate",
        }
    }

    /// What could not be determined, for the event that ends the step.
    fn capped_on(self) -> &'static str {
        match self {
            Read::Look => "the look for a spend or refund of the taker payment",
            Read::Gate => "the refundability check",
        }
    }

    fn plain_kind(self) -> &'static str {
        match self {
            Read::Look => "the search failed or did not finish",
            Read::Gate => "the check failed",
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

fn cap_text(read: Read, ticker: &str, reason: &str) -> String {
    let head = format!(
        "{} on {ticker} did not give a determined answer before the refund deadline {}; the taker payment is not \
         refunded",
        read.capped_on(),
        kind(read, reason)
    );
    if is_undetermined(reason) {
        format!("{head}; {FIX_HINT}, then run `recover_funds_of_swap`")
    } else {
        format!("{head}; run `recover_funds_of_swap` once the coin answers again")
    }
}

/// The text of the event that ends a refund step whose look at the taker payment stayed undetermined until its
/// cap. `reason` is the last error of the look; it only decides the kind and is not part of the text.
pub(crate) fn refund_cap_text(ticker: &str, reason: &str) -> String { cap_text(Read::Look, ticker, reason) }

/// The text of the event that ends a refund step whose refund gate stayed undetermined until its cap.
pub(crate) fn refund_gate_cap_text(ticker: &str, reason: &str) -> String { cap_text(Read::Gate, ticker, reason) }

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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use coins::swap_evidence::undetermined;

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
    fn only_the_first_line_of_a_situation_is_a_warning() {
        let mut log = WaitLog::new();
        let uuid = Uuid::nil();
        assert!(log.note(&uuid, "BTC", Read::Look, "x"));
        assert!(!log.note(&uuid, "BTC", Read::Look, "x"));
        assert!(!log.note(&uuid, "BTC", Read::Gate, "y"));
    }
}
