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

/// The cause and the fix, named only for a read that is undetermined by the two-node rule; any other error is
/// shown as the coin gave it, with nothing claimed about its cause.
fn cause_and_fix(reason: &str) -> String {
    if is_undetermined(reason) {
        format!(" (no two nodes agreed, or the trusted node did not answer); {FIX_HINT}")
    } else {
        String::new()
    }
}

/// The text of the log line that tells an operator a step waits on a read that was not determined.
fn wait_text(ticker: &str, what: &str, reason: &str) -> String {
    format!(
        "{what} on {ticker} could not be determined, the step waits and reads again: {reason}{}",
        cause_and_fix(reason)
    )
}

fn cap_text(what: &str, ticker: &str, reason: &str) -> String {
    format!(
        "{what} on {ticker} did not give a determined answer before the refund deadline: {reason}; the taker \
         payment is not refunded{}, then run `recover_funds_of_swap`",
        cause_and_fix(reason)
    )
}

/// The text of the event that ends a refund step whose look at the taker payment stayed undetermined until its
/// cap. `reason` is the last error of the look.
pub(crate) fn refund_cap_text(ticker: &str, reason: &str) -> String {
    cap_text("the look for a spend or refund of the taker payment", ticker, reason)
}

/// The text of the event that ends a refund step whose refund gate stayed undetermined until its cap.
pub(crate) fn refund_gate_cap_text(ticker: &str, reason: &str) -> String {
    cap_text("the refundability check", ticker, reason)
}

/// Logs one situation of a step: the first line at WARN, its repeats at DEBUG.
#[derive(Default)]
pub(crate) struct WaitLog {
    warned: bool,
}

impl WaitLog {
    pub(crate) fn new() -> Self { Self::default() }

    /// Returns whether the line went out at WARN (the first one) rather than at DEBUG.
    pub(crate) fn note(&mut self, swap_uuid: &Uuid, ticker: &str, what: &str, reason: &str) -> bool {
        let text = wait_text(ticker, what, reason);
        let first = !std::mem::replace(&mut self.warned, true);
        if first {
            warn!("Swap {}: {}", swap_uuid, text);
        } else {
            debug!("Swap {}: {}", swap_uuid, text);
        }
        first
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use coins::swap_evidence::undetermined;

    fn undetermined_reason() -> String { undetermined("the nodes differ", &["node-a.test", "node-b.test"]) }

    #[test]
    fn an_undetermined_reason_names_the_cause_and_the_fix() {
        let reason = undetermined_reason();
        for text in [
            wait_text("BTC", "the state of the taker payment", &reason),
            refund_cap_text("BTC", &reason),
            refund_gate_cap_text("BTC", &reason),
        ] {
            assert!(text.contains("no two nodes agreed"), "{text}");
            assert!(text.contains("second host"), "{text}");
            assert!(text.contains("`swap_evidence_node`"), "{text}");
            assert!(text.contains("BTC"), "{text}");
        }
        for text in [refund_cap_text("BTC", &reason), refund_gate_cap_text("BTC", &reason)] {
            assert!(text.contains("`recover_funds_of_swap`"), "{text}");
        }
    }

    #[test]
    fn a_plain_error_is_shown_as_given_and_no_cause_is_claimed() {
        for text in [
            wait_text("BTC", "the state of the taker payment", "all electrums are down"),
            refund_cap_text("BTC", "all electrums are down"),
            refund_gate_cap_text("BTC", "all electrums are down"),
        ] {
            assert!(text.contains("all electrums are down"), "{text}");
            assert!(!text.contains("swap_evidence_node"), "{text}");
            assert!(!text.contains("no two nodes agreed"), "{text}");
        }
        assert!(refund_cap_text("BTC", "x").contains("`recover_funds_of_swap`"));
    }

    #[test]
    fn only_the_first_line_of_a_situation_is_a_warning() {
        let mut log = WaitLog::new();
        let uuid = Uuid::nil();
        assert!(log.note(&uuid, "BTC", "a read", "x"));
        assert!(!log.note(&uuid, "BTC", "a read", "x"));
        assert!(!log.note(&uuid, "BTC", "a read", "y"));
    }
}
