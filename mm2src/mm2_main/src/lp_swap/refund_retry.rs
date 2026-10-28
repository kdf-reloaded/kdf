//! The shielded refund's retry after a non-final rejection (CRD ch.39 R39.5.13).
//!
//! A node rejects a refund as non-final while the chain's median time past has not yet passed its lock
//! time. The refund gate of the shielded coin waits for that, so the rejection can only be a race with
//! the chain (a reorganization, a lagging server); the refund stage then waits and goes back to the gate
//! rather than ending the swap. The coin decides what a retriable rejection looks like; this module
//! decides when retrying stops: at the refund deadline of the leg (lock time + 3700 s, ch.51 R18/R31).

use coins::MmCoinEnum;

/// How long a refund stage waits after a non-final rejection before it asks the refund gate again.
pub(crate) const NON_FINAL_REFUND_RETRY_SECS: f64 = 30.;

/// The node's text when `rejection`, the plain text of a failed refund broadcast of `coin`, is a
/// rejection the stage retries, and `now_secs` is before `refund_deadline`; `None` when the rejection
/// is terminal. A coin without a retriable rejection never retries.
pub(crate) fn retried_refund_rejection(
    coin: &MmCoinEnum,
    rejection: &str,
    now_secs: u64,
    refund_deadline: u64,
) -> Option<String> {
    if now_secs >= refund_deadline {
        return None;
    }
    coin.retriable_refund_rejection(rejection)
}
