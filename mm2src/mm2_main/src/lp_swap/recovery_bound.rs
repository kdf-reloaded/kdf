//! The total bound and the stage tracking of `recover_funds_of_swap` (CRD ch.39 R39.5.5).
//!
//! Recovery walks a fixed sequence of stages, each of which waits on a coin. The maker and the taker side
//! share one tracker: a stage is entered (and logged) just before the coin call it names, and when the bound
//! is reached the tracker says which stage was waiting, on which coin and payment.

use super::RecoveredSwap;
use coins::MmCoinEnum;
use common::custom_futures::FutureTimerExt;
use common::log::info;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// `recover_funds_of_swap` returns within this long, for every coin (R39.5.5).
pub(crate) const RECOVER_FUNDS_TOTAL_BOUND: Duration = Duration::from_secs(600);

/// The stages of a recovery.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RecoveryStage {
    ResolveHtlcKey,
    CheckPaymentSent,
    SearchSpend,
    SpendOtherPayment,
    RefundMyPayment,
}

impl RecoveryStage {
    fn label(self) -> &'static str {
        match self {
            RecoveryStage::ResolveHtlcKey => "resolving the HTLC key",
            RecoveryStage::CheckPaymentSent => "checking whether this node's own payment was sent",
            RecoveryStage::SearchSpend => "searching for a spend of the payment",
            RecoveryStage::SpendOtherPayment => "spending the counterparty's payment",
            RecoveryStage::RefundMyPayment => "refunding this node's payment",
        }
    }

    /// A spend or a refund may have reached the network before the bound was hit.
    fn may_have_broadcast(self) -> bool {
        matches!(self, RecoveryStage::SpendOtherPayment | RecoveryStage::RefundMyPayment)
    }
}

/// The parameters of the HTLC a detection stage looks at, in the order the coin's detection builds the
/// payment script from them. The coin turns them into whatever it keys its progress by.
pub(crate) struct HtlcDetectionParams {
    pub(crate) time_lock: u32,
    /// The refund key.
    pub(crate) first_pub: Vec<u8>,
    /// The spend key.
    pub(crate) second_pub: Vec<u8>,
    pub(crate) secret_hash: Vec<u8>,
}

struct CurrentStage {
    stage: RecoveryStage,
    coin: MmCoinEnum,
    payment: String,
    detection: Option<HtlcDetectionParams>,
}

/// The stage a recovery is in. Shared by the maker and the taker `recover_funds`.
#[derive(Default)]
pub(crate) struct RecoveryStages {
    current: Mutex<Option<CurrentStage>>,
}

impl RecoveryStages {
    /// Records that `stage` starts on `coin` and logs it at `info`. `payment_tx` is the payment the stage is
    /// about, when it is known: its id is logged in the usual display order.
    pub(crate) fn enter(
        &self,
        stage: RecoveryStage,
        coin: &MmCoinEnum,
        payment_tx: Option<&[u8]>,
        detection: Option<HtlcDetectionParams>,
    ) {
        let payment = match payment_tx {
            Some(bytes) => match coin.tx_enum_from_bytes(bytes) {
                Ok(tx) => format!("{:02x}", tx.tx_hash()),
                Err(e) => format!("(its id could not be read: {})", e),
            },
            None => "(not found yet)".to_owned(),
        };
        info!(
            "Swap recovery: stage \"{}\": coin {}, payment {}",
            stage.label(),
            coin.ticker(),
            payment
        );
        *self.current.lock() = Some(CurrentStage {
            stage,
            coin: coin.clone(),
            payment,
            detection,
        });
    }

    /// The error returned when `bound` was reached: the stage, the coin and the payment; what a shielded
    /// coin's detection read last; and, for a spend or a refund, that the transaction may have been sent.
    fn deadline_error(&self, bound: Duration) -> String {
        let guard = self.current.lock();
        let Some(current) = guard.as_ref() else {
            return format!(
                "recover_funds_of_swap did not finish within {} s, before the first recovery stage started",
                bound.as_secs()
            );
        };
        let mut message = format!(
            "recover_funds_of_swap did not finish within {} s: it was in the stage \"{}\" on coin {}, payment {}",
            bound.as_secs(),
            current.stage.label(),
            current.coin.ticker(),
            current.payment
        );
        let progress = current.detection.as_ref().and_then(|d| {
            current
                .coin
                .htlc_detection_progress(d.time_lock, &d.first_pub, &d.second_pub, &d.secret_hash)
        });
        if let Some(progress) = progress {
            message.push_str(&format!("; the HTLC detection was reading last: {}", progress));
        }
        if current.stage.may_have_broadcast() {
            message.push_str(
                ". The transaction may already have been broadcast: check the chain before assuming the outcome",
            );
        }
        message
    }
}

/// Runs `work` with a stage tracker, and returns the error of [`RecoveryStages::deadline_error`] when it
/// has not finished within `bound`. The work in flight is dropped. Works on native and wasm.
pub(crate) async fn recover_within<F, Fut>(bound: Duration, work: F) -> Result<RecoveredSwap, String>
where
    F: FnOnce(Arc<RecoveryStages>) -> Fut,
    Fut: Future<Output = Result<RecoveredSwap, String>>,
{
    let stages = Arc::new(RecoveryStages::default());
    match Box::pin(work(stages.clone())).timeout(bound).await {
        Ok(result) => result,
        Err(_) => Err(stages.deadline_error(bound)),
    }
}
