//! The refund gate of a shielded HTLC leg (CRD ch.39 R39.5.13).
//!
//! A node judges a time-locked transaction final once its lock time is below the median time past (BIP
//! 113) of the chain tip, and that median lags the wall clock by several block intervals. A refund is
//! therefore not built, let alone broadcast, until the wall clock has passed the lock time *and* the
//! median time past of the last 11 blocks (the coin's `mtp_block_count`) ending at the admitted backend's
//! tip has too. The chain is read only after the wall clock has passed the lock time.

use super::ZCoin;
use crate::utxo::utxo_common::capped_refund_wait;
use crate::utxo::UtxoCommonOps;
use crate::CanRefundHtlc;
use common::now_ms;

/// What the gate says about a refund of a given lock time. The waits are not yet capped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefundGate {
    /// The refund may be built and broadcast now.
    Open,
    /// The wall clock has not passed the lock time; the chain was not read.
    WaitWallClock { secs: u64 },
    /// The wall clock has passed the lock time but the chain's median time past `mtp` has not.
    WaitChainTime { secs: u64, mtp: u32 },
}

/// The wall-clock half of the gate: `Some(wait)` while `now` has not passed `locktime`.
fn wall_clock_wait(now: u64, locktime: u64) -> Option<u64> {
    if now > locktime {
        None
    } else {
        Some(locktime - now + 1)
    }
}

/// The chain half of the gate, given the median time past: a node accepts the refund only when its lock
/// time is strictly below the median.
fn chain_time_gate(locktime: u64, mtp: u32) -> RefundGate {
    if u64::from(mtp) > locktime {
        RefundGate::Open
    } else {
        RefundGate::WaitChainTime {
            secs: locktime - u64::from(mtp) + 1,
            mtp,
        }
    }
}

impl RefundGate {
    /// The answer of `SwapOps::can_refund_htlc`: a wait is capped at one hour, after which the stage asks again.
    pub(crate) fn into_can_refund(self) -> CanRefundHtlc {
        match self {
            RefundGate::Open => CanRefundHtlc::CanRefundNow,
            RefundGate::WaitWallClock { secs } | RefundGate::WaitChainTime { secs, .. } => capped_refund_wait(secs),
        }
    }

    /// Why the refund may not be built yet, for `recover_funds_of_swap`; `None` when the gate is open.
    pub(crate) fn closed_note(self, locktime: u64) -> Option<String> {
        match self {
            RefundGate::Open => None,
            RefundGate::WaitWallClock { .. } => Some(format!(
                "the wall clock has not passed the lock time {locktime}, and the chain's median time past must \
                 then pass it too"
            )),
            RefundGate::WaitChainTime { mtp, .. } => Some(format!(
                "the chain's median time past must pass the lock time {locktime} (it is {mtp} now)"
            )),
        }
    }
}

/// The gate for `coin`. The median time past comes from the coin's own RPC client: for a Light coin the
/// compact-block times of the admitted lightwalletd endpoint around its tip, the coin's only height
/// authority (R39.11.4f, R39.11.8); a Native coin, which activation refuses in this release, reads its own
/// node's headers. A read failure is an error, never an open gate.
pub(crate) async fn refund_gate(coin: &ZCoin, locktime: u64) -> Result<RefundGate, String> {
    let now = now_ms() / 1000;
    if let Some(secs) = wall_clock_wait(now, locktime) {
        return Ok(RefundGate::WaitWallClock { secs });
    }
    let mtp = coin
        .get_current_mtp()
        .await
        .map_err(|e| format!("cannot read the chain's median time past: {}", e))?;
    Ok(chain_time_gate(locktime, mtp))
}

/// The marker a node's text carries when it rejects a transaction as not yet final.
const NON_FINAL_MARKER: &str = "non-final";

/// The characters of context kept before and after the marker when a rejection is quoted in a log line.
const QUOTE_BEFORE: usize = 120;
const QUOTE_AFTER: usize = 80;

/// R39.5.13: the node's own text when `rejection`, the plain text of a refund broadcast failure, says the
/// node rejected the transaction as non-final (zcashd's reject reason, matched case-insensitively);
/// `None` for any other failure. The rejection text of the shielded coin also carries the built
/// transaction, so only the neighborhood of the marker is returned.
pub(crate) fn non_final_rejection(rejection: &str) -> Option<String> {
    let chars: Vec<char> = rejection.chars().collect();
    let lower: Vec<char> = rejection.to_lowercase().chars().collect();
    // Lower-casing can change the length of a text; position the marker in the lower-cased text only
    // when the two are the same length, otherwise quote from the start.
    let marker: Vec<char> = NON_FINAL_MARKER.chars().collect();
    let at = lower.windows(marker.len()).position(|w| w == marker.as_slice())?;
    let at = if lower.len() == chars.len() { at } else { 0 };
    let from = at.saturating_sub(QUOTE_BEFORE);
    let to = (at + marker.len() + QUOTE_AFTER).min(chars.len());
    Some(chars[from..to].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_must_pass_the_lock_time_strictly() {
        assert_eq!(wall_clock_wait(100, 100), Some(1));
        assert_eq!(wall_clock_wait(99, 100), Some(2));
        assert_eq!(wall_clock_wait(101, 100), None);
    }

    #[test]
    fn median_time_must_pass_the_lock_time_strictly() {
        assert_eq!(chain_time_gate(1_000, 1_001), RefundGate::Open);
        assert_eq!(chain_time_gate(1_000, 1_000), RefundGate::WaitChainTime {
            secs: 1,
            mtp: 1_000
        });
        assert_eq!(chain_time_gate(1_000, 700), RefundGate::WaitChainTime {
            secs: 301,
            mtp: 700
        });
    }

    #[test]
    fn a_wait_is_capped_at_one_hour_and_an_open_gate_is_not_a_wait() {
        assert!(matches!(
            RefundGate::WaitChainTime { secs: 301, mtp: 1 }.into_can_refund(),
            CanRefundHtlc::HaveToWait(301)
        ));
        assert!(matches!(
            RefundGate::WaitChainTime { secs: 90_000, mtp: 1 }.into_can_refund(),
            CanRefundHtlc::HaveToWait(3_600)
        ));
        assert!(matches!(
            RefundGate::WaitWallClock { secs: 90_000 }.into_can_refund(),
            CanRefundHtlc::HaveToWait(3_600)
        ));
        assert!(matches!(
            RefundGate::Open.into_can_refund(),
            CanRefundHtlc::CanRefundNow
        ));
    }

    #[test]
    fn the_closed_note_names_the_lock_time_and_the_current_median() {
        assert_eq!(RefundGate::Open.closed_note(1_000), None);
        let note = RefundGate::WaitChainTime { secs: 5, mtp: 996 }
            .closed_note(1_000)
            .unwrap();
        assert!(
            note.contains("median time past") && note.contains("1000") && note.contains("996"),
            "{note}"
        );
    }

    #[test]
    fn only_a_non_final_rejection_is_recognized_whatever_its_case() {
        let long_tx_debug = "00".repeat(5_000);
        let text = format!(
            "z_swap_ops.rs:1] MmError {{ etype: TxRecoverable({long_tx_debug}, \"lightwalletd rejected the \
             transaction (code 64): NON-FINAL\") }} {long_tx_debug}"
        );
        let quoted = non_final_rejection(&text).expect("recognized");
        assert!(quoted.contains("code 64"), "{quoted}");
        assert!(quoted.to_lowercase().contains("non-final"), "{quoted}");
        assert!(
            quoted.len() < 400,
            "the built transaction is not quoted: {} bytes",
            quoted.len()
        );

        assert_eq!(
            non_final_rejection("lightwalletd rejected the transaction (code 16): mandatory-script-verify-flag-failed"),
            None
        );
        assert_eq!(
            non_final_rejection("lightwalletd rejected the transaction (code 18): non-BIP68-final"),
            None
        );
        assert_eq!(non_final_rejection("every endpoint is unreachable"), None);
    }
}
