//! How a legacy swap step waits on a chain read that is undetermined (CRD ch.51 §51.6G R100 items 3 and 9).
//!
//! A read the coin could not determine (no two nodes agreed, or the trusted node did not answer) reaches the
//! engine as an error that [`coins::swap_evidence::is_undetermined`] recognizes. The step that rests on it waits
//! and reads again; it neither refunds nor claims on it. The text the operator sees, and the one that ends a
//! refund step at its cap, are written here so every site uses the same words.

use common::log::{debug, warn};
use uuid::Uuid;

/// What the operator can do about an undetermined read (CRD ch.51 §51.6G R100 item 9).
const FIX_HINT: &str = "add a server on a second host to the coin's node list or set `swap_evidence_node`";

/// The text of the log line that tells an operator a step waits on an undetermined read.
fn wait_text(swap_uuid: &Uuid, ticker: &str, what: &str, reason: &str) -> String {
    format!(
        "Swap {swap_uuid}: {what} on {ticker} could not be determined (no two nodes agreed, or the trusted node did \
         not answer); the step waits and reads again. To fix it, {FIX_HINT}: {reason}"
    )
}

/// The text of the event that ends a refund step whose chain reads stayed undetermined until its cap.
pub(crate) fn refund_cap_text(ticker: &str) -> String {
    format!(
        "the chain reads behind the refund on {ticker} could not be determined (no two nodes agreed, or the trusted \
         node did not answer); {FIX_HINT}, then run `recover_funds_of_swap`"
    )
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
        let text = wait_text(swap_uuid, ticker, what, reason);
        let first = !std::mem::replace(&mut self.warned, true);
        if first {
            warn!("{}", text);
        } else {
            debug!("{}", text);
        }
        first
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn the_texts_name_the_second_node_and_the_trusted_node_field() {
        let uuid = Uuid::nil();
        for text in [
            wait_text(&uuid, "BTC", "the payment state", "x"),
            refund_cap_text("BTC"),
        ] {
            assert!(text.contains("second host"), "{text}");
            assert!(text.contains("`swap_evidence_node`"), "{text}");
        }
        assert!(refund_cap_text("BTC").contains("`recover_funds_of_swap`"));
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
