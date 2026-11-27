//! # Purpose
//! Cross-cutting types, constants, and persistence helpers shared by
//! the Maker V2 and Taker V2 atomic-swap state machines.
//!
//! # Public exports
//! - [`AbortReason`] — tagged union of every reason a V2 swap can abort
//! - [`SwapStateMachineError`], [`SwapRecreateError`] — infrastructure errors
//! - [`SwapV2Type`], [`ActiveSwapV2Info`] — active-swap registry types
//! - [`StoredTxPreimage`], [`StoredMakerNegotiationData`],
//!   [`StoredTakerNegotiationData`] — DB-persisted negotiation artefacts
//! - [`SwapRecreateCtx`] — context plumbed through state-machine recreate
//! - `MakerSwapStorage`, `TakerSwapStorage` — `StateMachineStorage`
//!   impls (SQLite native / no-op WASM)
//! - [`acquire_reentrancy_lock_impl`], [`spawn_reentrancy_lock_renew`]
//! - On native: `read_swap_v2_events`, `get_swap_type` for RPC dispatch
//! - On `pub(super)`: `swap_kickstart_handler_for_{maker,taker}` and
//!   the [`GetSwapCoins`] trait used by recovery
//!
//! # Invariants
//! - DB schema (`my_swaps` columns) is shared with V1; do not change
//!   column names without a migration.
//! - `SwapV2Type` discriminant integers (1 = maker, 2 = taker) are
//!   persisted on disk: never reorder.
//! - `serde` tag/content names of [`AbortReason`] and the
//!   `Stored*NegotiationData` structs are part of the stored events
//!   format consumed by `swap_v2_rpcs`.
//! - The V2 protocol introduces a "funding" step before the actual
//!   payment. Confirmation gates (`require_*_confirm_before_*`) wait
//!   for `min(configured_confs, 1)` block confirmations; when disabled
//!   mempool visibility suffices, polled every
//!   [`SWAP_TX_VISIBILITY_POLL_SECS`] up to
//!   [`SWAP_TX_VISIBILITY_GRACE_SECS`].

use coins::lp_coinfind;
use coins::swap_evidence::{is_mined_and_failed, is_undetermined};
use coins::{MakerCoinSwapOpsV2, MmCoin, MmCoinEnum, ParseCoinAssocTypes, TakerCoinSwapOpsV2};
use common::executor::{spawn, Timer};
use common::log::log_crate::Level;
use common::log::{error, info, warn};
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_state_machine::storable_state_machine::{StateMachineDbRepr, StateMachineStorage, StorableStateMachine};
use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::evidence_wait::{funding_refund_cap_text, funding_refund_gate_cap_text, maker_refund_cap_text,
                           refund_cap_text, refund_gate_cap_text, Read, WaitLog};
use super::nft_maker_swap_v2::{select_nft_swap_v2_post_maker_payment_restart_from_tx,
                               select_nft_swap_v2_pre_maker_payment_restart, NftSwapV2PostMakerPaymentRestart,
                               NftSwapV2RestartDecision, NftSwapV2TakerAsset};
use super::swap_lock::{SwapLock, SwapLockOps};
use super::swap_versioning::{SwapVersion, NFT_SWAP_V2_VERSION};
use super::{maker_swap_v2::MakerSwapDbRepr, maker_swap_v2::MakerSwapEvent, maker_swap_v2::MakerSwapStateMachine};
use super::{taker_swap_v2::TakerSwapDbRepr, taker_swap_v2::TakerSwapEvent, taker_swap_v2::TakerSwapStateMachine};

// Constants ------------------------------------------------------------------

/// Maximum time (seconds) to wait for a transaction to appear in the mempool
/// before considering it missing.
pub const SWAP_TX_VISIBILITY_GRACE_SECS: f64 = 30.0;

/// Polling interval when checking mempool visibility.
pub const SWAP_TX_VISIBILITY_POLL_SECS: f64 = 1.0;

/// P2P negotiation timeout — how long each side waits for the counterparty's
/// negotiation messages before aborting.
pub const NEGOTIATION_TIMEOUT_SEC: u64 = 90;

/// The topic prefix used for V2 swap P2P messages (canonical definition in lp_swap.rs).
pub const SWAP_V2_PREFIX: &str = "swapv2";

pub(super) fn confirmation_gate_confs(configured_confs: u64) -> u64 { configured_confs.min(1) }

// Error / abort types --------------------------------------------------------

/// Reason a V2 swap was aborted.
#[derive(Clone, Debug, Deserialize, Display, Serialize)]
pub enum AbortReason {
    #[display(fmt = "Negotiation timed out")]
    NegotiationTimeout,
    #[display(fmt = "Negotiation failed: {}", _0)]
    NegotiationFailed(String),
    #[display(fmt = "Failed to send coin tx: {}", _0)]
    FailedToSendTx(String),
    #[display(fmt = "Failed to validate tx: {}", _0)]
    FailedToValidateTx(String),
    #[display(fmt = "Confirmation timed out: {}", _0)]
    ConfirmationTimeout(String),
    #[display(fmt = "Funding spend error: {}", _0)]
    FundingSpendError(String),
    #[display(fmt = "Taker aborted: {}", _0)]
    TakerAborted(String),
    #[display(fmt = "Maker aborted: {}", _0)]
    MakerAborted(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    // Taker-side abort reasons ----------------------------------------------
    #[display(fmt = "Failed to send payment: {}", _0)]
    FailedToSendPayment(String),
    #[display(fmt = "Did not receive maker payment: {}", _0)]
    DidNotReceiveMakerPayment(String),
    #[display(fmt = "Failed to parse maker payment: {}", _0)]
    FailedToParseMakerPayment(String),
    #[display(fmt = "Failed to parse funding spend preimage: {}", _0)]
    FailedToParseFundingSpendPreimg(String),
    #[display(fmt = "Failed to parse funding spend signature: {}", _0)]
    FailedToParseFundingSpendSig(String),
    #[display(fmt = "Maker payment validation failed: {}", _0)]
    MakerPaymentValidationFailed(String),
    #[display(fmt = "Funding spend preimage validation failed: {}", _0)]
    FundingSpendPreimageValidationFailed(String),
    #[display(fmt = "Maker payment not confirmed in time: {}", _0)]
    MakerPaymentNotConfirmedInTime(String),
    #[display(fmt = "Failed to generate spend preimage: {}", _0)]
    FailedToGenerateSpendPreimage(String),
    #[display(fmt = "Maker did not spend taker payment in time: {}", _0)]
    MakerDidNotSpendInTime(String),
    #[display(fmt = "Could not extract maker secret: {}", _0)]
    CouldNotExtractSecret(String),
    #[display(fmt = "Failed to spend maker payment: {}", _0)]
    FailedToSpendMakerPayment(String),
    #[display(fmt = "Maker payment spend not confirmed in time: {}", _0)]
    MakerPaymentSpendNotConfirmedInTime(String),
    #[display(fmt = "Taker funding refund failed: {}", _0)]
    TakerFundingRefundFailed(String),
    #[display(fmt = "Taker payment refund failed: {}", _0)]
    TakerPaymentRefundFailed(String),
}

/// Seconds between two reads of a confirmation (CRD ch.52 R19, R33).
pub(super) const CONFIRMATION_POLL_SECS: u64 = 10;

/// Seconds a bounded wait of a swap step lasts past its lock time, and at least from the moment the step
/// first could act (CRD ch.51 §51.6G R100 item 9).
pub(super) const OWN_CLAIM_CONFIRMATION_WAIT_SECS: u64 = 3700;

/// How many times one wait for the confirmation of the node's own claim sends the claim again.
pub(super) const MAX_CLAIM_RESENDS: u32 = 3;

/// Until when the confirmation of the node's own claim is read (CRD ch.52 R19, R33; ch.51 §51.6G R100
/// item 9): the later of `lock_time` plus 3700 seconds and 3700 seconds after `entered_at`, the moment the
/// state was entered in its current run. `entered_at` is not stored, so a restart starts a new wait.
pub(super) fn own_claim_confirmation_deadline(lock_time: u64, entered_at: u64) -> u64 {
    lock_time.max(entered_at) + OWN_CLAIM_CONFIRMATION_WAIT_SECS
}

/// How the node's own claim is named in the abort text and in the log: the coin and the claim transaction, by
/// its hash, or by its bytes when they do not parse.
pub(super) fn own_claim_name(ticker: &str, claim_tx_hash: Result<String, String>) -> String {
    match claim_tx_hash {
        Ok(hash) => format!("{ticker} claim tx hash {hash}"),
        Err(tx_bytes) => format!("{ticker} claim tx bytes {tx_bytes}"),
    }
}

/// Writes one line of [`await_own_claim_confirmation`] to the log.
pub(super) fn log_own_claim_line(level: Level, line: String) { common::log::log_crate::log!(level, "{}", line) }

/// Reads the confirmation of the node's own claim until it is read or `deadline` has passed (CRD ch.52 R19, R33).
/// The answer is whether the confirmation was read.
///
/// `read` is given `deadline` as the time its attempt may wait until, and `now` is the clock in seconds. A
/// failed attempt before `deadline` is repeated after `poll_secs`.
///
/// `report` receives the log lines. The first failed attempt of the wait gives one warning that names `claim`
/// and carries no error text. The error text of a failed attempt comes from a node call and may hold what an
/// operator keeps private, so it is reported at debug level only.
///
/// `sent` is the claim as it was broadcast, and `read` reads the confirmation of the claim it is given. A claim
/// that was mined but failed (the coin marks it, see [`coins::swap_evidence::is_mined_and_failed`]) is sent
/// again by `resend`, before the deadline, and the wait reads the claim `resend` answers from then on. A claim
/// that is not confirmed yet is only read again. Neither leads to a refund. A wait sends the claim again at most
/// [`MAX_CLAIM_RESENDS`] times (counted in memory, so a restart starts a new count); after that it only reads
/// until the deadline.
#[allow(clippy::too_many_arguments)]
pub(super) async fn await_own_claim_confirmation<C, F, Fut, R, RFut>(
    claim: &str,
    deadline: u64,
    poll_secs: u64,
    now: impl Fn() -> u64,
    mut report: impl FnMut(Level, String),
    sent: C,
    mut read: F,
    mut resend: R,
) -> bool
where
    C: Clone,
    F: FnMut(C, u64) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
    R: FnMut() -> RFut,
    RFut: std::future::Future<Output = Result<C, String>>,
{
    let mut warned = false;
    let mut resends = 0u32;
    let mut current = sent;
    loop {
        let error = match read(current.clone(), deadline).await {
            Ok(()) => return true,
            Err(e) => e,
        };
        report(
            Level::Debug,
            format!("Read of the confirmation of the own {claim} failed: {error}"),
        );
        if now() >= deadline {
            return false;
        }
        if is_mined_and_failed(&error) && resends < MAX_CLAIM_RESENDS {
            resends += 1;
            match resend().await {
                Ok(again) => {
                    current = again;
                    report(
                        Level::Warn,
                        format!("The own {claim} was mined but failed; the claim is sent again"),
                    );
                },
                Err(e) => report(Level::Debug, format!("Sending the own {claim} again failed: {e}")),
            }
        }
        if !warned {
            warned = true;
            report(
                Level::Warn,
                format!("Confirmation of the own {claim} could not be read; it is read again every {poll_secs}s"),
            );
        }
        Timer::sleep(poll_secs as f64).await;
    }
}

/// The fixed opening phrase of the abort text of a swap whose own claim was broadcast and whose confirmation
/// could not be read (CRD ch.52 R19, R33). By this phrase a reader of the event log tells such an abort from
/// one with nothing committed (CRD ch.52 R44); no other abort text starts with it.
pub const OWN_CLAIM_NOT_CONFIRMED_PHRASE: &str = "Own claim was broadcast but its confirmation could not be read: ";

/// The abort text of an own claim whose confirmation could not be read: [`OWN_CLAIM_NOT_CONFIRMED_PHRASE`],
/// then `claim`, the name [`own_claim_name`] gives. The text is stored with the swap and shown by the status
/// RPCs, so it carries no error text of a node call.
fn own_claim_not_confirmed_text(claim: &str) -> String { format!("{OWN_CLAIM_NOT_CONFIRMED_PHRASE}{claim}") }

/// The maker's abort reason when the confirmation of its claim of the taker payment could not be read
/// (CRD ch.52 R19).
pub(super) fn maker_own_claim_not_confirmed_reason(claim: &str) -> AbortReason {
    AbortReason::InternalError(own_claim_not_confirmed_text(claim))
}

/// The taker's abort reason when the confirmation of its claim of the maker payment could not be read
/// (CRD ch.52 R33).
pub(super) fn taker_own_claim_not_confirmed_reason(claim: &str) -> AbortReason {
    AbortReason::MakerPaymentSpendNotConfirmedInTime(own_claim_not_confirmed_text(claim))
}

// Waits on undetermined reads ------------------------------------------------

/// What a swap step does after one chain read it rests on (CRD ch.51 §51.6G R100 items 3 and 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AfterRead {
    /// The read gave a determined answer or failed in another way; the step handles it as its own rule says.
    Determined,
    /// The read is undetermined and the wait has not reached its bound: the step reads again.
    ReadAgain,
    /// The read is undetermined and the wait has reached its bound: the swap ends.
    BoundPassed,
}

/// How [`UndeterminedWait::read`] ended.
pub(super) enum ReadEnd<T, E> {
    /// A determined answer.
    Answer(T),
    /// A failure that is not an undetermined read.
    Failed(E),
    /// The read stayed undetermined until the bound of the wait. Carries the text of the last error, which may
    /// hold what an operator keeps private and is never put into an event.
    BoundPassed(String),
}

/// The wait of one swap step on chain reads that could not be determined (CRD ch.51 §51.6G R100 items 3 and 9).
///
/// A step neither refunds nor claims on an undetermined read; it reads again. The wait ends 3700 seconds after
/// the first undetermined answer that was read at or after `lock_time`, the moment the step first could act; an
/// undetermined answer before the lock time is waited on and does not start that count. So the wait never ends
/// before `lock_time` plus 3700 seconds. The moment is kept in memory only, so a resumed swap starts a new wait.
///
/// # What the coin must guarantee
///
/// A coin read with its own `wait_until` (a confirmation wait, a search for a spend) checks the clock before it
/// asks a node and answers "timed out" without a read once `wait_until` has passed. The step's own deadline and
/// the coin's `wait_until` are therefore two values: [`coin_wait_until`] gives every read after an undetermined
/// answer a `wait_until` that has not passed, and the step's deadline is judged here, by [`answered_too_late`].
/// The engine takes "timed out" from such a read as a determined "not in time". That rests on one rule for a coin
/// whose reads can be undetermined: a call whose `wait_until` has not passed asks its nodes at least once, and it
/// answers "timed out" only when its last read in that call was determined; an undetermined read is answered
/// with the marker and is never turned into "timed out", "nothing found" or "not confirmed".
pub(super) struct UndeterminedWait {
    lock_time: u64,
    waited: bool,
    first_at: Option<u64>,
    log: WaitLog,
}

/// The `wait_until` a coin read is given: the step's own `deadline` for the first read of a wait, and for a read
/// that follows an undetermined answer at least `min_wait_until`, so that the coin reads instead of answering
/// "timed out" from its clock alone. `min_wait_until` is what [`UndeterminedWait::read`] hands its `read`.
pub(super) fn coin_wait_until(deadline: u64, min_wait_until: u64) -> u64 { deadline.max(min_wait_until) }

impl UndeterminedWait {
    pub(super) fn new(lock_time: u64) -> Self {
        UndeterminedWait {
            lock_time,
            waited: false,
            first_at: None,
            log: WaitLog::new(),
        }
    }

    /// Whether a read of this wait was undetermined.
    pub(super) fn waited(&self) -> bool { self.waited }

    /// Judges one read at the time `now`: `error` is the text of its error, `None` for an answer. An error is an
    /// undetermined read when it carries the marker anywhere in its text.
    pub(super) fn after_read(&mut self, error: Option<&str>, now: u64) -> AfterRead {
        match error {
            Some(error) if is_undetermined(error) => self.after_unanswered(now),
            _ => AfterRead::Determined,
        }
    }

    /// Judges, at the time `now`, a read before a refund that gave no answer the step may act on: an undetermined
    /// read, a look that failed or found a spend it cannot act on, or a read whose inputs the swap's saved events
    /// do not hold. It counts toward the bound exactly as an undetermined read does, and never ends as
    /// [`AfterRead::Determined`].
    pub(super) fn after_unanswered(&mut self, now: u64) -> AfterRead {
        self.waited = true;
        if now >= self.lock_time && self.first_at.is_none() {
            self.first_at = Some(now);
        }
        match self.first_at {
            Some(first_at) if now >= own_claim_confirmation_deadline(self.lock_time, first_at) => {
                AfterRead::BoundPassed
            },
            _ => AfterRead::ReadAgain,
        }
    }

    /// Logs one wait of the step on `kind`: the first line at WARN, naming `ticker` and no error text; the repeats
    /// and `text` at DEBUG.
    pub(super) fn note(&mut self, swap_uuid: &Uuid, ticker: &str, kind: CappedRead, text: &str) {
        self.log.note(swap_uuid, ticker, kind.logged_as(), text);
    }

    /// Repeats `read` every `poll_secs` while it is undetermined, until it gives an answer, fails in another way,
    /// or the bound of the wait has passed. `now` is the clock in seconds. The first undetermined read gives one
    /// warning that names `ticker` and no error text; the repeats and every error text go to debug level.
    ///
    /// `read` is handed the earliest `wait_until` its coin call may use: zero for the first read, and one poll
    /// interval from now for a read that follows an undetermined answer (see [`coin_wait_until`]). The window is
    /// one poll interval and not the bound of the wait, because a coin read keeps polling until its `wait_until`.
    pub(super) async fn read<T, E, F, Fut>(
        &mut self,
        swap_uuid: &Uuid,
        ticker: &str,
        kind: CappedRead,
        poll_secs: u64,
        now: impl Fn() -> u64,
        mut read: F,
    ) -> ReadEnd<T, E>
    where
        E: std::fmt::Display,
        F: FnMut(u64) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let mut min_wait_until = 0;
        loop {
            let error = match read(min_wait_until).await {
                Ok(answer) => return ReadEnd::Answer(answer),
                Err(error) => error,
            };
            let text = error.to_string();
            match self.after_read(Some(&text), now()) {
                AfterRead::Determined => return ReadEnd::Failed(error),
                AfterRead::BoundPassed => return ReadEnd::BoundPassed(text),
                AfterRead::ReadAgain => {
                    self.log.note(swap_uuid, ticker, kind.logged_as(), &text);
                    Timer::sleep(poll_secs as f64).await;
                    min_wait_until = now() + poll_secs;
                },
            }
        }
    }

    /// Reads a confirmation a step rests on, as [`UndeterminedWait::read`] does. `deadline` is the step's own
    /// deadline for the confirmation; `read` is handed the `wait_until` of its coin call, which is `deadline` for
    /// the first read and may be later for a read after an undetermined answer. Whether a confirmation came in
    /// time is judged against `deadline` alone.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn read_confirmation<F, Fut>(
        &mut self,
        swap_uuid: &Uuid,
        ticker: &str,
        kind: CappedRead,
        deadline: u64,
        poll_secs: u64,
        now: impl Fn() -> u64,
        mut read: F,
    ) -> ConfirmationEnd
    where
        F: FnMut(u64) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let end = self
            .read(swap_uuid, ticker, kind, poll_secs, &now, |min_wait_until| {
                read(coin_wait_until(deadline, min_wait_until))
            })
            .await;
        confirmation_end(end, self.waited(), now(), deadline)
    }
}

/// How a confirmation read a step rests on ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ConfirmationEnd {
    Confirmed,
    /// Not confirmed, with the reason; the step handles it as its own rule says for a failed confirmation.
    NotConfirmed(String),
    /// Undetermined until the bound of the wait, with the text of the last error.
    BoundPassed(String),
}

/// Whether an answer that was read only after undetermined reads came too late to act on: the step's own
/// deadline for it has passed. This holds for a confirmation and for a funding spend that became the taker
/// payment. An answer read without such a wait is taken as the coin gave it.
pub(super) fn answered_too_late(waited: bool, now: u64, deadline: u64) -> bool { waited && now > deadline }

/// The reason text of a confirmation that [`answered_too_late`] refuses.
const CONFIRMED_AFTER_DEADLINE: &str = "the confirmation was read only after its deadline";

/// Turns the end of a confirmation read into what the step does. `waited` is whether a read of the wait was
/// undetermined, `now` the time the read ended, and `deadline` the step's own deadline for the confirmation.
fn confirmation_end(end: ReadEnd<(), String>, waited: bool, now: u64, deadline: u64) -> ConfirmationEnd {
    match end {
        ReadEnd::Answer(()) if answered_too_late(waited, now, deadline) => {
            ConfirmationEnd::NotConfirmed(CONFIRMED_AFTER_DEADLINE.to_string())
        },
        ReadEnd::Answer(()) => ConfirmationEnd::Confirmed,
        ReadEnd::Failed(error) => ConfirmationEnd::NotConfirmed(error),
        ReadEnd::BoundPassed(error) => ConfirmationEnd::BoundPassed(error),
    }
}

/// The read a step waits on while it is undetermined. It names the read in the log line of the wait and in the
/// abort of a wait that ended at its bound.
#[derive(Clone, Copy, Debug)]
pub(super) enum CappedRead {
    /// The maker's search for the spend of the taker funding.
    FundingSpendSearch,
    /// The maker's read of the taker payment's confirmation, before it claims.
    TakerPaymentConfirmation,
    /// The taker's read of the maker payment's confirmation, before it spends its funding.
    MakerPaymentConfirmation,
    /// The taker's search for the spend of the taker payment.
    TakerPaymentSpendSearch,
    /// The refund gate of the maker payment.
    MakerRefundGate,
    /// The refund gate of the taker payment.
    TakerRefundGate,
    /// The maker's look at the taker payment before it refunds the maker payment.
    MakerRefundLook,
    /// The taker's look at the taker payment before it refunds it.
    TakerRefundLook,
    /// The taker's search for the spend of its funding before it refunds the funding.
    FundingRefundSearch,
    /// The refund gate of the taker funding.
    FundingRefundGate,
}

/// The answer of `recover_funds_of_swap` for a version-two swap whose ending it does not recover.
pub(super) const NOT_RECOVERABLE_V2: &str =
    "Swap is not recoverable: only a swap that ended at the deadline of its refund step is recovered";

impl CappedRead {
    /// The kind of the read, for a text that names it.
    pub(super) fn read_kind(self) -> Read { self.logged_as() }

    /// How the read is named in the log line of its wait.
    fn logged_as(self) -> Read {
        match self {
            CappedRead::FundingSpendSearch | CappedRead::FundingRefundSearch => Read::TakerFundingSearch,
            CappedRead::TakerPaymentConfirmation => Read::TakerPaymentConfirmation,
            CappedRead::MakerPaymentConfirmation => Read::MakerPaymentConfirmation,
            CappedRead::TakerPaymentSpendSearch | CappedRead::MakerRefundLook | CappedRead::TakerRefundLook => {
                Read::Look
            },
            CappedRead::MakerRefundGate | CappedRead::TakerRefundGate | CappedRead::FundingRefundGate => Read::Gate,
        }
    }
}

/// What a refund step's look at a payment, made before the refund is built, decided (CRD ch.51 R31a, R82;
/// §51.6G R100).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Look<S> {
    /// No spend the step must respect: the refund goes on.
    RefundGoesOn,
    /// The claim, proven by the swap's secret hash, is on chain: the step goes to the claim and never refunds.
    /// Carries the found spend.
    Claimed(S),
    /// No answer the step may act on: it builds no refund, waits and looks again. Carries the text for the log.
    Unanswered(String),
}

/// The text a look logs when it found a spend that it may not act on.
pub(super) const SPEND_NOT_ACTED_ON: &str = "a spend was found whose secret does not match the swap's secret hash";

/// Judges the look of a refund step. `found` is what the search read: `Ok(None)` when it asked its nodes and
/// found no spend, `Ok(Some((matches, spend)))` for a found `spend`, `matches` telling whether it carries the
/// secret of the hash the swap holds, and `Err(text)` when the search failed or was undetermined.
/// `other_spend_refunds` is whether a found spend without that secret lets the refund go on: true for the maker,
/// for whom it is the taker's refund of the taker payment (CRD ch.51 R82 item 2); false for the taker, who acts on
/// one answer only when the secret proves it (ch.51 R31a, status note of 2026-11-23).
pub(super) fn judge_look<S>(found: Result<Option<(bool, S)>, String>, other_spend_refunds: bool) -> Look<S> {
    match found {
        Ok(None) => Look::RefundGoesOn,
        Ok(Some((true, spend))) => Look::Claimed(spend),
        Ok(Some((false, _))) if other_spend_refunds => Look::RefundGoesOn,
        Ok(Some((false, _))) => Look::Unanswered(SPEND_NOT_ACTED_ON.to_owned()),
        Err(error) => Look::Unanswered(error),
    }
}

/// The text a maker's look logs when it found a spend from which no secret could be extracted.
pub(super) const SPEND_SECRET_NOT_EXTRACTED: &str = "a spend was found whose secret could not be extracted";

/// What the maker's look makes of a spend of the taker payment it found, in the form [`judge_look`] takes.
/// `is_refund` is the coin's answer, from the structure of the spend, whether it is the taker's own refund of the
/// taker payment (the refund path carries no secret). Then:
/// - a spend told to be the taker's refund is positively that refund (`false`: no secret of the maker is in it);
/// - any other spend is a claim: the maker's own secret is its own claim (`true`); a failed extraction or another
///   secret answers nothing the step may act on, so it is an `Err` with a fixed text.
pub(super) fn maker_look_found<S, E>(
    is_refund: bool,
    extracted: Result<[u8; 32], E>,
    own_secret: &[u8],
    spend: S,
) -> Result<Option<(bool, S)>, String> {
    if is_refund {
        return Ok(Some((false, spend)));
    }
    match extracted {
        Ok(secret) if secret.as_slice() == own_secret => Ok(Some((true, spend))),
        Ok(_) => Err(SPEND_NOT_ACTED_ON.to_owned()),
        Err(_) => Err(SPEND_SECRET_NOT_EXTRACTED.to_owned()),
    }
}

/// What a failed search for the spend of a payment means to the look before a refund: "timed out" is a
/// determined "not spent", because the coin asked its nodes within the window it was given (see
/// [`UndeterminedWait`]); any other error leaves the look without an answer, with its text for the log.
pub(super) fn spend_search_error<S>(error: &MmError<coins::FindPaymentSpendError>) -> Result<Option<S>, String> {
    match error.get_inner() {
        coins::FindPaymentSpendError::Timeout { .. } => Ok(None),
        _ => Err(error.to_string()),
    }
}

/// Which refund of the taker funding a refund attempt builds (CRD ch.52 R34, maintainer decision of 2026-11-24).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FundingRefundBranch {
    /// Before the funding locktime: the refund that reveals the taker's own secret.
    Secret,
    /// At or after the funding locktime: the timelock refund, after the refund gate.
    Timelock,
}

/// The branch of the funding refund at the time `now`.
pub(super) fn funding_refund_branch(now: u64, funding_locktime: u64) -> FundingRefundBranch {
    if now >= funding_locktime {
        FundingRefundBranch::Timelock
    } else {
        FundingRefundBranch::Secret
    }
}

/// How long one look before a refund may search, in seconds. A coin search answers "timed out" only after it
/// asked its nodes within this window (see [`UndeterminedWait`]); that answer is then a determined "not spent".
pub(super) const REFUND_LOOK_WINDOW_SECS: u64 = 30;

/// The log text of a read before a refund whose inputs the swap's saved events do not hold.
pub(super) const HISTORY_LACKS_INPUTS: &str = "the swap's saved events do not hold the transaction or start block \
                                               this read needs";

/// The abort reason of a refund step that could not make its read `read` on the coin `ticker` until the bound of
/// its wait, because the swap's saved events do not hold what the read needs. It carries no error text.
pub(super) fn history_lacks_inputs_reason(read: CappedRead, ticker: &str) -> AbortReason {
    let text = |what: &str, not_refunded: &str| {
        format!(
            "{what} on {ticker} could not be made before the refund deadline: the swap's saved events do not hold \
             the transaction or start block it needs; {not_refunded} is not refunded and needs a manual refund"
        )
    };
    match read {
        CappedRead::FundingRefundSearch | CappedRead::FundingRefundGate => AbortReason::TakerFundingRefundFailed(text(
            "the search for the spend of the taker funding",
            "the taker funding",
        )),
        CappedRead::TakerRefundLook | CappedRead::TakerRefundGate | CappedRead::TakerPaymentSpendSearch => {
            AbortReason::TakerPaymentRefundFailed(text(
                "the look for a spend or refund of the taker payment",
                "the taker payment",
            ))
        },
        _ => AbortReason::InternalError(text(
            "the look for a spend or refund of the taker payment",
            "the maker payment",
        )),
    }
}

/// Logs an event of a refund step at info level under the swap's uuid. The uuid is passed on as a value of its
/// own, as the wait of the undetermined reads does.
pub(super) fn log_refund_step(swap_uuid: &Uuid, step: &str) {
    info!("Swap {}: {}", swap_uuid, step);
}

/// Logs a failed refund gate at error level under the swap's uuid, with the retry in `retry_secs` seconds.
pub(super) fn log_refund_gate_failed(swap_uuid: &Uuid, error: &str, retry_secs: u64) {
    error!(
        "Swap {}: can_refund_htlc error: {}, retrying in {}s",
        swap_uuid, error, retry_secs
    );
}

/// How long a refund step waits before it repeats a read that gave no answer it may act on, in seconds.
pub(super) const REFUND_RETRY_SECS: u64 = 30;

/// Handles one read before a refund that gave no answer the step may act on (CRD ch.51 §51.6G R100 items 3 and
/// 9): once the bound of `wait` has passed it answers the abort reason, which names `read` on `ticker`;
/// otherwise it logs the wait and sleeps [`REFUND_RETRY_SECS`] and answers `None`. `text` is for the log only.
/// `history_lacks_inputs` tells that the read could not run because the swap's saved events lack its inputs.
pub(super) async fn wait_after_unanswered(
    wait: &mut UndeterminedWait,
    swap_uuid: &Uuid,
    ticker: &str,
    read: CappedRead,
    text: &str,
    history_lacks_inputs: bool,
) -> Option<AbortReason> {
    if wait.after_unanswered(common::now_ms() / 1000) == AfterRead::BoundPassed {
        return Some(if history_lacks_inputs {
            history_lacks_inputs_reason(read, ticker)
        } else {
            undetermined_cap_reason(read, ticker, text)
        });
    }
    wait.note(swap_uuid, ticker, read, text);
    Timer::sleep(REFUND_RETRY_SECS as f64).await;
    None
}

/// The text of the abort of a step whose read stayed undetermined until the bound of its wait: the kind of the
/// read, the coin, what was not done, and the fix (CRD ch.51 §51.6G R100 item 9). It carries no error text.
fn undetermined_cap_text(read: &str, ticker: &str, left_undone: &str) -> String {
    format!(
        "{read} on {ticker} did not give a determined answer before its deadline (no two nodes agreed, or the \
         trusted node did not answer); {left_undone}; add a server on a second host to the coin's node list or set \
         `swap_evidence_node`, then run `recover_funds_of_swap`"
    )
}

/// The abort reason of a step whose `read` on the coin `ticker` stayed undetermined until the bound of its wait.
/// Each read ends with the reason variant its state already uses. `last_error` only decides the kind named in the
/// texts shared with the legacy swap; no part of it is in the reason.
pub(super) fn undetermined_cap_reason(read: CappedRead, ticker: &str, last_error: &str) -> AbortReason {
    match read {
        CappedRead::FundingSpendSearch => AbortReason::FundingSpendError(undetermined_cap_text(
            "the search for the spend of the taker funding",
            ticker,
            "the maker payment is not refunded",
        )),
        // Not `ConfirmationTimeout`: the maker's aborts with that reason commit nothing (CRD ch.52 R19, R44),
        // and this one is reached with the maker payment on chain.
        CappedRead::TakerPaymentConfirmation => AbortReason::InternalError(undetermined_cap_text(
            "the read of the confirmation of the taker payment",
            ticker,
            "the maker payment is on chain, the taker payment is not claimed and the maker payment is not refunded",
        )),
        CappedRead::MakerPaymentConfirmation => AbortReason::MakerPaymentNotConfirmedInTime(undetermined_cap_text(
            "the read of the confirmation of the maker payment",
            ticker,
            "the taker funding is not spent and not refunded",
        )),
        CappedRead::TakerPaymentSpendSearch => AbortReason::MakerDidNotSpendInTime(refund_cap_text(ticker, last_error)),
        CappedRead::MakerRefundGate => AbortReason::InternalError(undetermined_cap_text(
            "the refundability check",
            ticker,
            "the maker payment is not refunded",
        )),
        CappedRead::TakerRefundGate => AbortReason::TakerPaymentRefundFailed(refund_gate_cap_text(ticker, last_error)),
        CappedRead::MakerRefundLook => AbortReason::InternalError(maker_refund_cap_text(ticker, last_error)),
        CappedRead::TakerRefundLook => AbortReason::TakerPaymentRefundFailed(refund_cap_text(ticker, last_error)),
        CappedRead::FundingRefundSearch => {
            AbortReason::TakerFundingRefundFailed(funding_refund_cap_text(ticker, last_error))
        },
        CappedRead::FundingRefundGate => {
            AbortReason::TakerFundingRefundFailed(funding_refund_gate_cap_text(ticker, last_error))
        },
    }
}

/// Errors produced by the V2 state machine infrastructure itself.
#[derive(Debug, Display)]
pub enum SwapStateMachineError {
    #[display(fmt = "Storage error: {}", _0)]
    Storage(String),
    #[display(fmt = "Reentrancy lock error: {}", _0)]
    ReentrancyLock(String),
    #[display(fmt = "Recreate error: {}", _0)]
    Recreate(String),
}

/// Errors when recreating a state machine from stored events.
#[derive(Debug, Display)]
pub enum SwapRecreateError {
    #[display(fmt = "No events to recreate from")]
    NoEvents,
    #[display(fmt = "Coin not found: {}", _0)]
    CoinNotFound(String),
    #[display(fmt = "Coin not active: {}", _0)]
    CoinNotActive(String),
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

/// Context needed to recreate coin instances during swap recovery.
pub struct SwapRecreateCtx<MakerCoin, TakerCoin> {
    pub maker_coin: MakerCoin,
    pub taker_coin: TakerCoin,
}

// Active swap tracking -------------------------------------------------------

/// Metadata about a running V2 swap, stored in `SwapsContext` for UI queries.
#[derive(Clone, Debug)]
pub struct ActiveSwapV2Info {
    pub uuid: Uuid,
    pub maker_coin: String,
    pub taker_coin: String,
    pub swap_type: SwapV2Type,
}

/// Distinguishes maker V2 from taker V2 swaps in storage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SwapV2Type {
    MakerV2 = 1,
    TakerV2 = 2,
}

// Serializable preimage (for DB persistence) ---------------------------------

/// A preimage + signature pair stored as raw bytes in the DB.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredTxPreimage {
    pub preimage: BytesJson,
    pub signature: BytesJson,
}

// Negotiation data (what gets stored in events for reconstruction) -----------

/// Stored negotiation data from the maker side (used in maker events).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredMakerNegotiationData {
    pub taker_secret_hash: BytesJson,
    pub taker_coin_htlc_pub: BytesJson,
    pub maker_coin_htlc_pub: BytesJson,
    pub taker_coin_swap_contract: Option<BytesJson>,
    pub maker_coin_swap_contract: Option<BytesJson>,
    pub taker_payment_locktime: u64,
    pub taker_funding_locktime: u64,
}

/// Stored negotiation data from the taker side (used in taker events).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredTakerNegotiationData {
    pub maker_secret_hash: BytesJson,
    pub maker_coin_htlc_pub: BytesJson,
    pub taker_coin_htlc_pub: BytesJson,
    pub maker_coin_swap_contract: Option<BytesJson>,
    pub taker_coin_swap_contract: Option<BytesJson>,
    pub maker_payment_locktime: u64,
    pub taker_coin_address: String,
}

// Reentrancy lock helpers ----------------------------------------------------

/// Acquire a reentrancy lock for the given swap UUID.
pub async fn acquire_reentrancy_lock_impl(
    ctx: &MmArc,
    uuid: Uuid,
    ttl_sec: f64,
) -> Result<SwapLock, MmError<SwapStateMachineError>> {
    match SwapLock::lock(ctx, uuid, ttl_sec).await {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => MmError::err(SwapStateMachineError::ReentrancyLock(format!(
            "Swap {} is already locked by another instance",
            uuid
        ))),
        Err(e) => MmError::err(SwapStateMachineError::ReentrancyLock(e.to_string())),
    }
}

/// Spawn a background loop that periodically touches the swap lock to renew its TTL.
pub fn spawn_reentrancy_lock_renew(lock: SwapLock, interval_sec: f64) {
    spawn(async move {
        loop {
            Timer::sleep(interval_sec).await;
            if let Err(e) = lock.touch().await {
                // If renewal fails the lock will eventually expire, allowing recovery.
                info!("Failed to renew swap lock: {}", e);
                break;
            }
        }
    });
}

// StateMachineDbRepr impls ---------------------------------------------------

impl StateMachineDbRepr for MakerSwapDbRepr {
    type Event = MakerSwapEvent;

    fn add_event(&mut self, event: Self::Event) { self.events.push(event); }
}

impl StateMachineDbRepr for TakerSwapDbRepr {
    type Event = TakerSwapEvent;

    fn add_event(&mut self, event: Self::Event) { self.events.push(event); }
}

// V2 Swap Storage — Native (SQLite) ------------------------------------------

cfg_native! {
    use async_trait::async_trait;
    use crypto::secret_hash_algo::SecretHashAlgo;
    use db_common::sqlite::rusqlite::params;
    use serde_json;
    use std::str::FromStr;
    use super::{MAKER_SWAP_V2_TYPE, TAKER_SWAP_V2_TYPE};

    fn secret_hash_algo_to_i64(algo: SecretHashAlgo) -> i64 {
        match algo {
            SecretHashAlgo::DHASH160 => 0,
            SecretHashAlgo::SHA256 => 1,
        }
    }

    /// SQLite-backed storage for V2 maker swaps.
    #[derive(Clone)]
    pub struct MakerSwapStorage {
        ctx: MmArc,
    }

    impl MakerSwapStorage {
        pub fn new(ctx: MmArc) -> Self { MakerSwapStorage { ctx } }
        pub fn get_ctx(&self) -> MmArc { self.ctx.clone() }
    }

    #[async_trait]
    impl StateMachineStorage for MakerSwapStorage {
        type MachineId = Uuid;
        type DbRepr = MakerSwapDbRepr;
        type Error = MmError<SwapStateMachineError>;

        async fn store_repr(&mut self, id: Self::MachineId, repr: Self::DbRepr) -> Result<(), Self::Error> {
            insert_swap_v2_maker(&self.ctx, &id, &repr)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn get_repr(&self, id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
            get_swap_repr::<MakerSwapDbRepr>(&self.ctx, &id, MAKER_SWAP_V2_TYPE)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn has_record_for(&mut self, id: &Self::MachineId) -> Result<bool, Self::Error> {
            has_swap_v2_record(&self.ctx, id)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn store_event(&mut self, id: Self::MachineId, event: MakerSwapEvent) -> Result<(), Self::Error> {
            append_swap_v2_event::<MakerSwapEvent>(&self.ctx, &id, &event)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
            get_unfinished_swap_uuids(&self.ctx, MAKER_SWAP_V2_TYPE)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn mark_finished(&mut self, id: Self::MachineId) -> Result<(), Self::Error> {
            mark_swap_v2_finished(&self.ctx, &id)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }
    }

    /// SQLite-backed storage for V2 taker swaps.
    #[derive(Clone)]
    pub struct TakerSwapStorage {
        ctx: MmArc,
    }

    impl TakerSwapStorage {
        pub fn new(ctx: MmArc) -> Self { TakerSwapStorage { ctx } }
        pub fn get_ctx(&self) -> MmArc { self.ctx.clone() }
    }

    #[async_trait]
    impl StateMachineStorage for TakerSwapStorage {
        type MachineId = Uuid;
        type DbRepr = TakerSwapDbRepr;
        type Error = MmError<SwapStateMachineError>;

        async fn store_repr(&mut self, id: Self::MachineId, repr: Self::DbRepr) -> Result<(), Self::Error> {
            insert_swap_v2_taker(&self.ctx, &id, &repr)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn get_repr(&self, id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
            get_swap_repr::<TakerSwapDbRepr>(&self.ctx, &id, TAKER_SWAP_V2_TYPE)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn has_record_for(&mut self, id: &Self::MachineId) -> Result<bool, Self::Error> {
            has_swap_v2_record(&self.ctx, id)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn store_event(&mut self, id: Self::MachineId, event: TakerSwapEvent) -> Result<(), Self::Error> {
            append_swap_v2_event::<TakerSwapEvent>(&self.ctx, &id, &event)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
            get_unfinished_swap_uuids(&self.ctx, TAKER_SWAP_V2_TYPE)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }

        async fn mark_finished(&mut self, id: Self::MachineId) -> Result<(), Self::Error> {
            mark_swap_v2_finished(&self.ctx, &id)
                .map_to_mm(|e| SwapStateMachineError::Storage(e.to_string()))
        }
    }

    // SQL helper functions --------------------------------------------------

    /// Fields extracted from either a maker or taker V2 swap DB repr, already
    /// resolved to the plain values the shared `my_swaps` INSERT statement
    /// binds. `insert_swap_v2_maker`/`insert_swap_v2_taker` each populate one
    /// of these from their own repr type and hand it to [`insert_swap_v2`].
    struct SwapV2InsertFields {
        my_coin: String,
        other_coin: String,
        started_at: u64,
        swap_type: u8,
        maker_volume: String,
        taker_volume: String,
        premium: String,
        dex_fee: String,
        dex_fee_burn: String,
        secret: Vec<u8>,
        secret_hash: Vec<u8>,
        secret_hash_algo: i64,
        p2p_privkey: Vec<u8>,
        lock_duration: u64,
        maker_coin_confs: u64,
        maker_coin_nota: bool,
        taker_coin_confs: u64,
        taker_coin_nota: bool,
        other_p2p_pub: Vec<u8>,
        swap_version: u8,
    }

    /// Insert a new V2 swap record into the my_swaps table.
    /// For maker swaps: my_coin = maker_coin, other_coin = taker_coin.
    /// For taker swaps: my_coin = taker_coin, other_coin = maker_coin.
    fn insert_swap_v2(ctx: &MmArc, uuid: &Uuid, fields: SwapV2InsertFields, role: &str) -> Result<(), String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let events_json = serde_json::to_string(&serde_json::json!([])).unwrap();

        conn.execute(
            "INSERT INTO my_swaps (
                my_coin, other_coin, uuid, started_at, swap_type, is_finished, events_json,
                maker_volume, taker_volume, premium, dex_fee, dex_fee_burn,
                secret, secret_hash, secret_hash_algo, p2p_privkey, lock_duration,
                maker_coin_confs, maker_coin_nota, taker_coin_confs, taker_coin_nota,
                other_p2p_pub, swap_version
            ) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                fields.my_coin, fields.other_coin, uuid_str, fields.started_at as i64,
                fields.swap_type as i64, events_json,
                fields.maker_volume, fields.taker_volume, fields.premium, fields.dex_fee, fields.dex_fee_burn,
                fields.secret, fields.secret_hash, fields.secret_hash_algo, fields.p2p_privkey,
                fields.lock_duration as i64,
                fields.maker_coin_confs as i64, fields.maker_coin_nota as i64,
                fields.taker_coin_confs as i64, fields.taker_coin_nota as i64,
                fields.other_p2p_pub, fields.swap_version as i64,
            ],
        )
        .map(|_| ())
        .map_err(|e| format!("Failed to insert V2 {} swap {}: {}", role, uuid, e))
    }

    fn insert_swap_v2_maker(ctx: &MmArc, uuid: &Uuid, repr: &MakerSwapDbRepr) -> Result<(), String> {
        let fields = SwapV2InsertFields {
            my_coin: repr.maker_coin.clone(),
            other_coin: repr.taker_coin.clone(),
            started_at: repr.started_at,
            swap_type: MAKER_SWAP_V2_TYPE,
            maker_volume: repr.maker_volume.to_decimal().to_string(),
            taker_volume: repr.taker_volume.to_decimal().to_string(),
            premium: repr.taker_premium.to_decimal().to_string(),
            dex_fee: repr.dex_fee_amount.to_decimal().to_string(),
            dex_fee_burn: repr.dex_fee_burn.to_decimal().to_string(),
            secret: repr.maker_secret.0.to_vec(),
            secret_hash: repr.maker_secret_hash.to_vec(),
            secret_hash_algo: secret_hash_algo_to_i64(repr.secret_hash_algo),
            p2p_privkey: repr.p2p_keypair.as_ref().map(|k| k.0.clone()).unwrap_or_default(),
            lock_duration: repr.lock_duration,
            maker_coin_confs: repr.conf_settings.maker_coin_confs,
            maker_coin_nota: repr.conf_settings.maker_coin_nota,
            taker_coin_confs: repr.conf_settings.taker_coin_confs,
            taker_coin_nota: repr.conf_settings.taker_coin_nota,
            other_p2p_pub: repr.taker_p2p_pub.to_vec(),
            swap_version: repr.swap_version,
        };
        insert_swap_v2(ctx, uuid, fields, "maker")
    }

    fn insert_swap_v2_taker(ctx: &MmArc, uuid: &Uuid, repr: &TakerSwapDbRepr) -> Result<(), String> {
        let fields = SwapV2InsertFields {
            my_coin: repr.taker_coin.clone(),
            other_coin: repr.maker_coin.clone(),
            started_at: repr.started_at,
            swap_type: TAKER_SWAP_V2_TYPE,
            maker_volume: repr.maker_volume.to_decimal().to_string(),
            taker_volume: repr.taker_volume.to_decimal().to_string(),
            premium: repr.taker_premium.to_decimal().to_string(),
            dex_fee: repr.dex_fee_amount.to_decimal().to_string(),
            dex_fee_burn: repr.dex_fee_burn.to_decimal().to_string(),
            secret: repr.taker_secret.0.to_vec(),
            secret_hash: repr.taker_secret_hash.to_vec(),
            secret_hash_algo: secret_hash_algo_to_i64(repr.secret_hash_algo),
            p2p_privkey: repr.p2p_keypair.as_ref().map(|k| k.0.clone()).unwrap_or_default(),
            lock_duration: repr.lock_duration,
            maker_coin_confs: repr.conf_settings.maker_coin_confs,
            maker_coin_nota: repr.conf_settings.maker_coin_nota,
            taker_coin_confs: repr.conf_settings.taker_coin_confs,
            taker_coin_nota: repr.conf_settings.taker_coin_nota,
            other_p2p_pub: repr.maker_p2p_pub.to_vec(),
            swap_version: repr.swap_version,
        };
        insert_swap_v2(ctx, uuid, fields, "taker")
    }

    /// Check if a V2 swap record exists for the given UUID.
    fn has_swap_v2_record(ctx: &MmArc, uuid: &Uuid) -> Result<bool, String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM my_swaps WHERE uuid = ?1",
                params![uuid_str],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check swap record for {}: {}", uuid, e))?;
        Ok(count > 0)
    }

    /// Append an event to the events_json array of a V2 swap.
    fn append_swap_v2_event<E: Serialize>(ctx: &MmArc, uuid: &Uuid, event: &E) -> Result<(), String> {
        let mut conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();

        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start transaction for {}: {}", uuid, e))?;

        // Read existing events
        let events_str: String = tx
            .query_row(
                "SELECT events_json FROM my_swaps WHERE uuid = ?1",
                params![uuid_str],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read events for {}: {}", uuid, e))?;

        let mut events: Vec<serde_json::Value> = serde_json::from_str(&events_str)
            .map_err(|e| format!("Failed to parse events_json for {}: {}", uuid, e))?;

        let event_val = serde_json::to_value(event)
            .map_err(|e| format!("Failed to serialize event for {}: {}", uuid, e))?;
        events.push(event_val);

        let updated = serde_json::to_string(&events)
            .map_err(|e| format!("Failed to serialize updated events for {}: {}", uuid, e))?;

        tx.execute(
            "UPDATE my_swaps SET events_json = ?1 WHERE uuid = ?2",
            params![updated, uuid_str],
        )
        .map_err(|e| format!("Failed to update events for swap {}: {}", uuid, e))?;

        tx.commit()
            .map_err(|e| format!("Failed to commit transaction for {}: {}", uuid, e))
    }

    /// Get the full swap DB repr for a V2 swap.
    /// We reconstruct it from the row columns + deserialized events.
    fn get_swap_repr<R: for<'de> Deserialize<'de>>(ctx: &MmArc, uuid: &Uuid, swap_type: u8) -> Result<R, String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();

        let row = conn
            .query_row(
                "SELECT my_coin, other_coin, started_at, events_json,
                        maker_volume, taker_volume, premium, dex_fee, dex_fee_burn,
                        secret, secret_hash, secret_hash_algo, p2p_privkey,
                        lock_duration, maker_coin_confs, maker_coin_nota,
                        taker_coin_confs, taker_coin_nota, other_p2p_pub, swap_version
                 FROM my_swaps WHERE uuid = ?1",
                params![uuid_str],
                |row| {
                    Ok(SwapV2Row {
                        my_coin: row.get(0)?,
                        other_coin: row.get(1)?,
                        started_at: row.get::<_, i64>(2)? as u64,
                        events_json: row.get(3)?,
                        maker_volume: row.get(4)?,
                        taker_volume: row.get(5)?,
                        premium: row.get(6)?,
                        dex_fee: row.get(7)?,
                        dex_fee_burn: row.get(8)?,
                        secret: row.get(9)?,
                        secret_hash: row.get(10)?,
                        secret_hash_algo: row.get::<_, i64>(11)? as u8,
                        p2p_privkey: row.get(12)?,
                        lock_duration: row.get::<_, i64>(13)? as u64,
                        maker_coin_confs: row.get::<_, i64>(14)? as u64,
                        maker_coin_nota: row.get::<_, i64>(15)? != 0,
                        taker_coin_confs: row.get::<_, i64>(16)? as u64,
                        taker_coin_nota: row.get::<_, i64>(17)? != 0,
                        other_p2p_pub: row.get(18)?,
                        swap_version: row.get::<_, i64>(19)? as u8,
                    })
                },
            )
            .map_err(|e| format!("Failed to read swap repr for {}: {}", uuid, e))?;

        // Build the full repr as JSON, then deserialize to R.
        // This works because MakerSwapDbRepr and TakerSwapDbRepr are both Deserialize.
        let events_val: serde_json::Value = serde_json::from_str(&row.events_json)
            .map_err(|e| format!("Failed to parse events_json for {}: {}", uuid, e))?;

        let mut secret_arr = [0u8; 32];
        let len = row.secret.len().min(32);
        secret_arr[..len].copy_from_slice(&row.secret[..len]);
        let secret_h256 = H256Json::from(secret_arr);

        let p2p_keypair = if row.p2p_privkey.iter().any(|&b| b != 0) {
            Some(serde_json::json!(row.p2p_privkey))
        } else {
            None
        };

        let secret_hash_algo_str = match row.secret_hash_algo {
            1 => "SHA256",
            _ => "DHASH160",
        };

        let conf_settings = serde_json::json!({
            "maker_coin_confs": row.maker_coin_confs,
            "maker_coin_nota": row.maker_coin_nota,
            "taker_coin_confs": row.taker_coin_confs,
            "taker_coin_nota": row.taker_coin_nota,
        });

        // BytesJson (rpc::v1::types::Bytes) serializes to/from hex strings,
        // so we must wrap raw Vec<u8> in BytesJson before putting into json!().
        let secret_hash_bytes = BytesJson::from(row.secret_hash);
        let other_p2p_pub_bytes = BytesJson::from(row.other_p2p_pub);

        let repr_json = if swap_type == MAKER_SWAP_V2_TYPE {
            serde_json::json!({
                "maker_coin": row.my_coin,
                "maker_volume": row.maker_volume,
                "maker_secret": secret_h256,
                "maker_secret_hash": secret_hash_bytes,
                "secret_hash_algo": secret_hash_algo_str,
                "started_at": row.started_at,
                "lock_duration": row.lock_duration,
                "taker_coin": row.other_coin,
                "taker_volume": row.taker_volume,
                "taker_premium": row.premium,
                "dex_fee_amount": row.dex_fee,
                "dex_fee_burn": row.dex_fee_burn,
                "conf_settings": conf_settings,
                "uuid": uuid_str,
                "p2p_keypair": p2p_keypair,
                "events": events_val,
                "taker_p2p_pub": other_p2p_pub_bytes,
                "swap_version": row.swap_version,
            })
        } else {
            serde_json::json!({
                "maker_coin": row.other_coin,
                "maker_volume": row.maker_volume,
                "taker_secret": secret_h256,
                "taker_secret_hash": secret_hash_bytes,
                "secret_hash_algo": secret_hash_algo_str,
                "started_at": row.started_at,
                "lock_duration": row.lock_duration,
                "taker_coin": row.my_coin,
                "taker_volume": row.taker_volume,
                "taker_premium": row.premium,
                "dex_fee_amount": row.dex_fee,
                "dex_fee_burn": row.dex_fee_burn,
                "conf_settings": conf_settings,
                "uuid": uuid_str,
                "p2p_keypair": p2p_keypair,
                "events": events_val,
                "maker_p2p_pub": other_p2p_pub_bytes,
                "swap_version": row.swap_version,
            })
        };

        serde_json::from_value(repr_json)
            .map_err(|e| format!("Failed to deserialize swap repr for {}: {}", uuid, e))
    }

    /// Helper struct to hold a row from my_swaps.
    struct SwapV2Row {
        my_coin: String,
        other_coin: String,
        started_at: u64,
        events_json: String,
        maker_volume: String,
        taker_volume: String,
        premium: String,
        dex_fee: String,
        dex_fee_burn: String,
        secret: Vec<u8>,
        secret_hash: Vec<u8>,
        secret_hash_algo: u8,
        p2p_privkey: Vec<u8>,
        lock_duration: u64,
        maker_coin_confs: u64,
        maker_coin_nota: bool,
        taker_coin_confs: u64,
        taker_coin_nota: bool,
        other_p2p_pub: Vec<u8>,
        swap_version: u8,
    }

    /// Get UUIDs of all unfinished V2 swaps of the given type.
    fn get_unfinished_swap_uuids(ctx: &MmArc, swap_type: u8) -> Result<Vec<Uuid>, String> {
        let conn = ctx.sqlite_connection();
        let mut stmt = conn
            .prepare("SELECT uuid FROM my_swaps WHERE is_finished = 0 AND swap_type = ?1")
            .map_err(|e| format!("Failed to prepare unfinished swaps query: {}", e))?;

        let uuids = stmt
            .query_map(params![swap_type as i64], |row| {
                let uuid_str: String = row.get(0)?;
                Ok(uuid_str)
            })
            .map_err(|e| format!("Failed to query unfinished swaps: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(|s| Uuid::from_str(&s).ok())
            .collect();

        Ok(uuids)
    }

    /// Mark a V2 swap as finished.
    fn mark_swap_v2_finished(ctx: &MmArc, uuid: &Uuid) -> Result<(), String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        conn.execute(
            "UPDATE my_swaps SET is_finished = 1 WHERE uuid = ?1",
            params![uuid_str],
        )
        .map(|_| ())
        .map_err(|e| format!("Failed to mark swap {} as finished: {}", uuid, e))
    }

    /// Read all events for a V2 swap from the DB (for recovery).
    pub fn read_swap_v2_events<E: for<'de> Deserialize<'de>>(ctx: &MmArc, uuid: &Uuid) -> Result<Vec<E>, String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let events_str: String = conn
            .query_row(
                "SELECT events_json FROM my_swaps WHERE uuid = ?1",
                params![uuid_str],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read events for {}: {}", uuid, e))?;
        serde_json::from_str(&events_str)
            .map_err(|e| format!("Failed to deserialize events for {}: {}", uuid, e))
    }

    /// Read the swap_type for a given UUID (for dispatch during RPC).
    pub fn get_swap_type(ctx: &MmArc, uuid: &Uuid) -> Result<u8, String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let swap_type: i64 = conn
            .query_row(
                "SELECT swap_type FROM my_swaps WHERE uuid = ?1",
                params![uuid_str],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read swap_type for {}: {}", uuid, e))?;
        Ok(swap_type as u8)
    }
}

// V2 Swap Storage — WASM (IndexedDB) -----------------------------------------

cfg_wasm32! {
    use async_trait::async_trait;
    use serde_json;
    use super::{MAKER_SWAP_V2_TYPE, TAKER_SWAP_V2_TYPE};

    /// IndexedDB-backed storage for V2 maker swaps.
    pub struct MakerSwapStorage {
        ctx: MmArc,
    }

    impl MakerSwapStorage {
        pub fn new(ctx: MmArc) -> Self { MakerSwapStorage { ctx } }
        pub fn get_ctx(&self) -> MmArc { self.ctx.clone() }
    }

    #[async_trait]
    impl StateMachineStorage for MakerSwapStorage {
        type MachineId = Uuid;
        type DbRepr = MakerSwapDbRepr;
        type Error = MmError<SwapStateMachineError>;

        async fn store_repr(&mut self, _id: Self::MachineId, _repr: Self::DbRepr) -> Result<(), Self::Error> {
            // TODO: WASM IndexedDB storage
            Ok(())
        }

        async fn get_repr(&self, _id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
            MmError::err(SwapStateMachineError::Storage("WASM get_repr not yet implemented".into()))
        }

        async fn has_record_for(&mut self, _id: &Self::MachineId) -> Result<bool, Self::Error> {
            Ok(false)
        }

        async fn store_event(&mut self, _id: Self::MachineId, _event: MakerSwapEvent) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
            Ok(vec![])
        }

        async fn mark_finished(&mut self, _id: Self::MachineId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// IndexedDB-backed storage for V2 taker swaps.
    pub struct TakerSwapStorage {
        ctx: MmArc,
    }

    impl TakerSwapStorage {
        pub fn new(ctx: MmArc) -> Self { TakerSwapStorage { ctx } }
        pub fn get_ctx(&self) -> MmArc { self.ctx.clone() }
    }

    #[async_trait]
    impl StateMachineStorage for TakerSwapStorage {
        type MachineId = Uuid;
        type DbRepr = TakerSwapDbRepr;
        type Error = MmError<SwapStateMachineError>;

        async fn store_repr(&mut self, _id: Self::MachineId, _repr: Self::DbRepr) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn get_repr(&self, _id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
            MmError::err(SwapStateMachineError::Storage("WASM get_repr not yet implemented".into()))
        }

        async fn has_record_for(&mut self, _id: &Self::MachineId) -> Result<bool, Self::Error> {
            Ok(false)
        }

        async fn store_event(&mut self, _id: Self::MachineId, _event: TakerSwapEvent) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
            Ok(vec![])
        }

        async fn mark_finished(&mut self, _id: Self::MachineId) -> Result<(), Self::Error> {
            Ok(())
        }
    }
}

// V2 Swap Kickstart / Recovery -----------------------------------------------

/// Trait for extracting coin tickers from a swap DB repr so the kickstart
/// logic can wait for the required coins to activate.
pub(super) trait GetSwapCoins {
    fn maker_coin(&self) -> &str;
    fn taker_coin(&self) -> &str;
}

impl GetSwapCoins for MakerSwapDbRepr {
    fn maker_coin(&self) -> &str { &self.maker_coin }
    fn taker_coin(&self) -> &str { &self.taker_coin }
}

impl GetSwapCoins for TakerSwapDbRepr {
    fn maker_coin(&self) -> &str { &self.maker_coin }
    fn taker_coin(&self) -> &str { &self.taker_coin }
}

/// Waits until both maker and taker coins are activated, then returns them.
/// Returns `None` if an unrecoverable error occurs on coin lookup.
pub(super) async fn swap_kickstart_coins<T: GetSwapCoins>(
    ctx: &MmArc,
    swap_repr: &T,
    uuid: &Uuid,
) -> Option<(MmCoinEnum, MmCoinEnum)> {
    let taker_coin_ticker = swap_repr.taker_coin();
    let taker_coin = loop {
        match lp_coinfind(ctx, taker_coin_ticker).await {
            Ok(Some(c)) => break c,
            Ok(None) => {
                info!(
                    "Can't kickstart the swap {} until the coin {} is activated",
                    uuid, taker_coin_ticker,
                );
                Timer::sleep(5.).await;
            },
            Err(e) => {
                error!("Error {} on {} find attempt for swap {}", e, taker_coin_ticker, uuid);
                return None;
            },
        };
    };

    let maker_coin_ticker = swap_repr.maker_coin();
    let maker_coin = loop {
        match lp_coinfind(ctx, maker_coin_ticker).await {
            Ok(Some(c)) => break c,
            Ok(None) => {
                info!(
                    "Can't kickstart the swap {} until the coin {} is activated",
                    uuid, maker_coin_ticker,
                );
                Timer::sleep(5.).await;
            },
            Err(e) => {
                error!("Error {} on {} find attempt for swap {}", e, maker_coin_ticker, uuid);
                return None;
            },
        };
    };

    Some((maker_coin, taker_coin))
}

/// Generic V2 swap kickstart: recreates the state machine from stored events
/// and resumes execution from the last persisted state.
pub(super) async fn swap_kickstart_handler<
    T: StorableStateMachine<RecreateCtx = SwapRecreateCtx<MakerCoin, TakerCoin>>,
    MakerCoin: MmCoin + MakerCoinSwapOpsV2,
    TakerCoin: MmCoin + TakerCoinSwapOpsV2,
>(
    swap_repr: <T::Storage as StateMachineStorage>::DbRepr,
    storage: T::Storage,
    uuid: <T::Storage as StateMachineStorage>::MachineId,
    maker_coin: MakerCoin,
    taker_coin: TakerCoin,
) where
    <T::Storage as StateMachineStorage>::MachineId: Copy + std::fmt::Display,
    T::Error: std::fmt::Display,
    T::RecreateError: std::fmt::Display,
{
    let recreate_context = SwapRecreateCtx { maker_coin, taker_coin };

    let (mut state_machine, state) = match T::recreate_machine(uuid, storage, swap_repr, recreate_context).await {
        Ok((machine, from_state)) => (machine, from_state),
        Err(e) => {
            error!("Error {} on trying to recreate the swap {}", e, uuid);
            return;
        },
    };

    if let Err(e) = state_machine.kickstart(state).await {
        error!("Error {} on trying to run the swap {}", e, uuid);
    }
}

fn latest_maker_payment_from_maker_events(events: &[MakerSwapEvent]) -> Option<&BytesJson> {
    events.iter().rev().find_map(|event| match event {
        MakerSwapEvent::MakerPaymentSentFundingSpendGenerated { maker_payment, .. }
        | MakerSwapEvent::MakerPaymentRefundRequired { maker_payment, .. }
        | MakerSwapEvent::MakerPaymentRefunded { maker_payment, .. }
        | MakerSwapEvent::TakerPaymentReceived { maker_payment, .. }
        | MakerSwapEvent::TakerPaymentReceivedPreimageSkipped { maker_payment, .. }
        | MakerSwapEvent::TakerPaymentSpent { maker_payment, .. } => Some(maker_payment),
        MakerSwapEvent::Initialized { .. }
        | MakerSwapEvent::WaitingForTakerFunding { .. }
        | MakerSwapEvent::TakerFundingReceived { .. }
        | MakerSwapEvent::Aborted { .. }
        | MakerSwapEvent::Completed => None,
    })
}

fn latest_maker_payment_from_taker_events(events: &[TakerSwapEvent]) -> Option<&BytesJson> {
    events.iter().rev().find_map(|event| match event {
        TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived { maker_payment, .. }
        | TakerSwapEvent::MakerPaymentConfirmed { maker_payment, .. }
        | TakerSwapEvent::TakerPaymentSent { maker_payment, .. }
        | TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped { maker_payment, .. }
        | TakerSwapEvent::TakerPaymentSpent { maker_payment, .. } => Some(maker_payment),
        TakerSwapEvent::Initialized { .. }
        | TakerSwapEvent::Negotiated { .. }
        | TakerSwapEvent::TakerFundingSent { .. }
        | TakerSwapEvent::TakerFundingRefundRequired { .. }
        | TakerSwapEvent::TakerPaymentRefundRequired { .. }
        | TakerSwapEvent::MakerPaymentSpent { .. }
        | TakerSwapEvent::TakerFundingRefunded { .. }
        | TakerSwapEvent::TakerPaymentRefunded { .. }
        | TakerSwapEvent::Aborted { .. }
        | TakerSwapEvent::Completed => None,
    })
}

fn nft_swap_version(version: u8) -> SwapVersion { SwapVersion { version } }

fn nft_v2_restart_decision_for_eth_maker(
    maker_coin: &coins::eth::EthCoin,
    swap_version: u8,
    maker_payment: Option<&BytesJson>,
) -> NftSwapV2RestartDecision {
    let Some(maker_payment) = maker_payment else {
        return select_nft_swap_v2_pre_maker_payment_restart();
    };

    let maker_payment_tx = match maker_coin.parse_tx(&maker_payment.0) {
        Ok(tx) => tx,
        Err(e) => {
            return NftSwapV2RestartDecision::Park(
                super::nft_maker_swap_v2::NftSwapV2RestartParkReason::MakerPaymentCalldataMalformed(format!("{:?}", e)),
            )
        },
    };

    let version = nft_swap_version(swap_version);
    let input = NftSwapV2PostMakerPaymentRestart {
        maker_version: version,
        taker_version: version,
        configured_nft_contract: maker_coin.nft_swap_v2_contract_addr().ok(),
        taker_asset: NftSwapV2TakerAsset::Fungible,
        token_standard: None,
        tx_to: Default::default(),
        calldata: &[],
        expected: None,
    };
    select_nft_swap_v2_post_maker_payment_restart_from_tx(input, &maker_payment_tx)
}

fn intercept_nft_v2_maker_restart(
    uuid: Uuid,
    role: &str,
    swap_version: u8,
    maker_coin: &MmCoinEnum,
    maker_payment: Option<&BytesJson>,
) -> bool {
    if swap_version != NFT_SWAP_V2_VERSION {
        return false;
    }

    let decision = match maker_coin {
        MmCoinEnum::EthCoin(maker_coin) => {
            nft_v2_restart_decision_for_eth_maker(maker_coin, swap_version, maker_payment)
        },
        _ => NftSwapV2RestartDecision::Park(
            super::nft_maker_swap_v2::NftSwapV2RestartParkReason::PreMakerPaymentNftIdentityUnavailable,
        ),
    };
    warn!(
        "NFT V2 {} swap {} restart is parked/refused before generic V2 restoration: {:?}",
        role, uuid, decision
    );
    true
}

/// Kickstart a V2 maker swap: wait for coins, match variants, recreate + resume.
pub(super) async fn swap_kickstart_handler_for_maker(
    ctx: MmArc,
    swap_repr: MakerSwapDbRepr,
    storage: MakerSwapStorage,
    uuid: Uuid,
) {
    if let Some((maker_coin, taker_coin)) = swap_kickstart_coins(&ctx, &swap_repr, &uuid).await {
        if intercept_nft_v2_maker_restart(
            uuid,
            "maker",
            swap_repr.swap_version,
            &maker_coin,
            latest_maker_payment_from_maker_events(&swap_repr.events),
        ) {
            return;
        }

        match (maker_coin, taker_coin) {
            (MmCoinEnum::EthCoin(m), MmCoinEnum::EthCoin(t)) => {
                swap_kickstart_handler::<MakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                swap_kickstart_handler::<MakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::EthCoin(t)) => {
                swap_kickstart_handler::<MakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::EthCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                swap_kickstart_handler::<MakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            _ => {
                warn!(
                    "V2 kickstart for maker swap {} not supported for this coin pair ({}/{})",
                    uuid, swap_repr.maker_coin, swap_repr.taker_coin,
                );
            },
        }
    }
}

/// Kickstart a V2 taker swap: wait for coins, match variants, recreate + resume.
pub(super) async fn swap_kickstart_handler_for_taker(
    ctx: MmArc,
    swap_repr: TakerSwapDbRepr,
    storage: TakerSwapStorage,
    uuid: Uuid,
) {
    if let Some((maker_coin, taker_coin)) = swap_kickstart_coins(&ctx, &swap_repr, &uuid).await {
        if intercept_nft_v2_maker_restart(
            uuid,
            "taker",
            swap_repr.swap_version,
            &maker_coin,
            latest_maker_payment_from_taker_events(&swap_repr.events),
        ) {
            return;
        }

        match (maker_coin, taker_coin) {
            (MmCoinEnum::EthCoin(m), MmCoinEnum::EthCoin(t)) => {
                swap_kickstart_handler::<TakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                swap_kickstart_handler::<TakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::EthCoin(t)) => {
                swap_kickstart_handler::<TakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            (MmCoinEnum::EthCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                swap_kickstart_handler::<TakerSwapStateMachine<_, _>, _, _>(swap_repr, storage, uuid, m, t).await
            },
            _ => {
                warn!(
                    "V2 kickstart for taker swap {} not supported for this coin pair ({}/{})",
                    uuid, swap_repr.maker_coin, swap_repr.taker_coin,
                );
            },
        }
    }
}

/// The two coins of a swap that the manual recovery needs, as activated now.
async fn recovery_coins<T: GetSwapCoins>(ctx: &MmArc, swap_repr: &T) -> Result<(MmCoinEnum, MmCoinEnum), String> {
    let mut coins = Vec::with_capacity(2);
    for ticker in [swap_repr.maker_coin(), swap_repr.taker_coin()] {
        match lp_coinfind(ctx, ticker).await {
            Ok(Some(coin)) => coins.push(coin),
            Ok(None) => return ERR!("Coin {} must be activated to recover the swap", ticker),
            Err(e) => return ERR!("{}", e),
        }
    }
    let taker_coin = coins.pop().ok_or("the taker coin is not found")?;
    let maker_coin = coins.pop().ok_or("the maker coin is not found")?;
    Ok((maker_coin, taker_coin))
}

/// The answer of `recover_funds_of_swap` for a version-two swap whose recovery is already running.
pub(super) const RECOVERY_IN_FLIGHT: &str = "A recovery of this swap is already running; wait for its answer";

static RECOVERIES_IN_FLIGHT: std::sync::Mutex<Vec<Uuid>> = std::sync::Mutex::new(Vec::new());

/// Holds the right to run the recovery of one swap: a second call for the same uuid gets an error answer at
/// once. The right is given back when the guard is dropped.
pub(super) struct RecoveryInFlight(Uuid);

impl RecoveryInFlight {
    pub(super) fn enter(uuid: Uuid) -> Result<RecoveryInFlight, String> {
        let mut running = RECOVERIES_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&uuid) {
            return Err(RECOVERY_IN_FLIGHT.to_owned());
        }
        running.push(uuid);
        Ok(RecoveryInFlight(uuid))
    }
}

impl Drop for RecoveryInFlight {
    fn drop(&mut self) {
        let mut running = RECOVERIES_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|u| *u != self.0);
    }
}

/// `recover_funds_of_swap` for a swap of version two (CRD ch.51 §51.6D, status note of 2026-11-24): a swap that
/// ended at the bound of its refund step runs that step once more with fresh reads; any other ending gets the
/// RPC's "not recoverable" answer.
pub(super) async fn recover_funds_of_swap_v2(
    ctx: &MmArc,
    uuid: Uuid,
    swap_type: u8,
) -> Result<super::RecoveredSwap, String> {
    use super::maker_swap_v2::recover_maker_swap_v2;
    use super::taker_swap_v2::recover_taker_swap_v2;

    let _in_flight = RecoveryInFlight::enter(uuid)?;
    if swap_type == super::MAKER_SWAP_V2_TYPE {
        let storage = MakerSwapStorage::new(ctx.clone());
        let repr = try_s!(storage.get_repr(uuid).await);
        let (maker_coin, taker_coin) = recovery_coins(ctx, &repr).await?;
        match (maker_coin, taker_coin) {
            (MmCoinEnum::EthCoin(m), MmCoinEnum::EthCoin(t)) => recover_maker_swap_v2(uuid, repr, storage, m, t).await,
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                recover_maker_swap_v2(uuid, repr, storage, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::EthCoin(t)) => recover_maker_swap_v2(uuid, repr, storage, m, t).await,
            (MmCoinEnum::EthCoin(m), MmCoinEnum::UtxoCoin(t)) => recover_maker_swap_v2(uuid, repr, storage, m, t).await,
            _ => ERR!("{}", NOT_RECOVERABLE_V2),
        }
    } else if swap_type == super::TAKER_SWAP_V2_TYPE {
        let storage = TakerSwapStorage::new(ctx.clone());
        let repr = try_s!(storage.get_repr(uuid).await);
        let (maker_coin, taker_coin) = recovery_coins(ctx, &repr).await?;
        match (maker_coin, taker_coin) {
            (MmCoinEnum::EthCoin(m), MmCoinEnum::EthCoin(t)) => recover_taker_swap_v2(uuid, repr, storage, m, t).await,
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::UtxoCoin(t)) => {
                recover_taker_swap_v2(uuid, repr, storage, m, t).await
            },
            (MmCoinEnum::UtxoCoin(m), MmCoinEnum::EthCoin(t)) => recover_taker_swap_v2(uuid, repr, storage, m, t).await,
            (MmCoinEnum::EthCoin(m), MmCoinEnum::UtxoCoin(t)) => recover_taker_swap_v2(uuid, repr, storage, m, t).await,
            _ => ERR!("{}", NOT_RECOVERABLE_V2),
        }
    } else {
        ERR!("{}", NOT_RECOVERABLE_V2)
    }
}

#[cfg(test)]
mod tests {
    use super::super::swap_v2_pb::*;
    use super::*;

    #[test]
    fn should_roundtrip_when_encoding_swap_v2_protobuf() {
        use prost::Message;

        let maker_neg = MakerNegotiation {
            started_at: 1234567890,
            payment_locktime: 9999,
            secret_hash: vec![1, 2, 3],
            maker_coin_htlc_pub: vec![4, 5, 6],
            taker_coin_htlc_pub: vec![7, 8, 9],
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            taker_coin_address: "R9abc123".into(),
        };
        let swap_msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiation(maker_neg)),
            swap_uuid: vec![0u8; 16],
        };
        let encoded = swap_msg.encode_to_vec();
        let decoded = SwapMessage::decode(encoded.as_slice()).unwrap();
        match decoded.inner {
            Some(swap_message::Inner::MakerNegotiation(n)) => {
                assert_eq!(n.started_at, 1234567890);
                assert_eq!(n.payment_locktime, 9999);
                assert_eq!(n.taker_coin_address, "R9abc123");
            },
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn should_preserve_reason_when_serializing_taker_abort() {
        use prost::Message;

        let taker_neg = TakerNegotiation {
            action: Some(taker_negotiation::Action::Abort(Abort {
                reason: "bad coin".into(),
            })),
        };
        let swap_msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerNegotiation(taker_neg)),
            swap_uuid: vec![0u8; 16],
        };
        let encoded = swap_msg.encode_to_vec();
        let decoded = SwapMessage::decode(encoded.as_slice()).unwrap();
        match decoded.inner {
            Some(swap_message::Inner::TakerNegotiation(tn)) => match tn.action {
                Some(taker_negotiation::Action::Abort(abort)) => {
                    assert_eq!(abort.reason, "bad coin");
                },
                _ => panic!("Expected Abort"),
            },
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn should_roundtrip_when_serializing_stored_tx_preimage() {
        let stored = StoredTxPreimage {
            preimage: BytesJson::from(vec![0xAA, 0xBB]),
            signature: BytesJson::from(vec![0xCC, 0xDD]),
        };
        let json = serde_json::to_string(&stored).unwrap();
        let back: StoredTxPreimage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preimage.0, vec![0xAA, 0xBB]);
        assert_eq!(back.signature.0, vec![0xCC, 0xDD]);
    }

    #[test]
    fn should_format_message_when_displaying_abort_reason() {
        let reason = AbortReason::NegotiationTimeout;
        assert_eq!(format!("{}", reason), "Negotiation timed out");

        let reason = AbortReason::FailedToSendTx("insufficient funds".into());
        assert!(format!("{}", reason).contains("insufficient funds"));
    }

    /// ch.52 R58/R64: both fields are additive, so a log written before either
    /// existed — the JSON below has neither — must still deserialize, defaulting
    /// the headroom to zero (its pre-existing, always-correct-for-a-gap value)
    /// and the reservable-fee marker to absent (recovered from the raw fee
    /// amount by the caller, not by serde; see `recreate_machine`).
    #[test]
    fn old_persisted_initialized_event_without_the_new_fields_still_deserializes() {
        use common::mm_number::MmNumber;

        // Round-trip through JSON rather than a hand-written literal, so this
        // test does not depend on guessing the enum's wire tagging shape — only
        // on the two new fields genuinely being optional. Serialise a fully
        // populated event, delete the two fields a pre-fix log would never have
        // written, and confirm it still parses.
        let maker_event = MakerSwapEvent::Initialized {
            maker_coin_start_block: 100,
            taker_coin_start_block: 200,
            maker_payment_trade_fee: MmNumber::from("0.001"),
            taker_payment_spend_trade_fee: MmNumber::from("0.002"),
            taker_payment_spend_headroom: MmNumber::from("0.002"),
            maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
        };
        let mut maker_value = serde_json::to_value(&maker_event).unwrap();
        let maker_fields = maker_value
            .get_mut("Initialized")
            .expect("externally-tagged Initialized");
        maker_fields
            .as_object_mut()
            .unwrap()
            .remove("taker_payment_spend_headroom");
        maker_fields
            .as_object_mut()
            .unwrap()
            .remove("maker_payment_trade_fee_reservable");
        let event: MakerSwapEvent = serde_json::from_value(maker_value).unwrap();
        match event {
            MakerSwapEvent::Initialized {
                taker_payment_spend_headroom,
                maker_payment_trade_fee_reservable,
                ..
            } => {
                assert_eq!(taker_payment_spend_headroom, MmNumber::from(0));
                assert_eq!(maker_payment_trade_fee_reservable, None);
            },
            other => panic!("expected Initialized, got {other:?}"),
        }

        let taker_event = TakerSwapEvent::Initialized {
            maker_coin_start_block: 100,
            taker_coin_start_block: 200,
            taker_payment_fee: MmNumber::from("0.001"),
            maker_payment_spend_fee: MmNumber::from("0.002"),
            maker_payment_spend_headroom: MmNumber::from("0.002"),
            taker_payment_fee_reservable: Some(MmNumber::from("0.001")),
        };
        let mut taker_value = serde_json::to_value(&taker_event).unwrap();
        let taker_fields = taker_value
            .get_mut("Initialized")
            .expect("externally-tagged Initialized");
        taker_fields
            .as_object_mut()
            .unwrap()
            .remove("maker_payment_spend_headroom");
        taker_fields
            .as_object_mut()
            .unwrap()
            .remove("taker_payment_fee_reservable");
        let event: TakerSwapEvent = serde_json::from_value(taker_value).unwrap();
        match event {
            TakerSwapEvent::Initialized {
                maker_payment_spend_headroom,
                taker_payment_fee_reservable,
                ..
            } => {
                assert_eq!(maker_payment_spend_headroom, MmNumber::from(0));
                assert_eq!(taker_payment_fee_reservable, None);
            },
            other => panic!("expected Initialized, got {other:?}"),
        }
    }

    #[test]
    fn t17_9_10_maker_kickstart_guard_extracts_latest_maker_payment() {
        let old_payment = BytesJson::from(vec![0x01]);
        let latest_payment = BytesJson::from(vec![0x02]);
        let events = vec![
            MakerSwapEvent::MakerPaymentRefunded {
                maker_payment: old_payment,
                maker_payment_refund: BytesJson::from(vec![0xAA]),
                reason: AbortReason::InternalError("old".into()),
            },
            MakerSwapEvent::MakerPaymentRefunded {
                maker_payment: latest_payment.clone(),
                maker_payment_refund: BytesJson::from(vec![0xBB]),
                reason: AbortReason::InternalError("latest".into()),
            },
        ];

        assert_eq!(latest_maker_payment_from_maker_events(&events), Some(&latest_payment));
    }

    #[test]
    fn t17_9_10_taker_kickstart_guard_extracts_latest_maker_payment() {
        let old_payment = BytesJson::from(vec![0x11]);
        let latest_payment = BytesJson::from(vec![0x22]);
        let negotiation_data = StoredTakerNegotiationData {
            maker_secret_hash: BytesJson::from(vec![0x01; 32]),
            maker_coin_htlc_pub: BytesJson::from(vec![0x02; 33]),
            taker_coin_htlc_pub: BytesJson::from(vec![0x03; 33]),
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            maker_payment_locktime: 1,
            taker_coin_address: "0x0".into(),
        };
        let events = vec![
            TakerSwapEvent::TakerPaymentSent {
                maker_coin_start_block: 1,
                taker_coin_start_block: 1,
                negotiation_data: negotiation_data.clone(),
                taker_payment: BytesJson::from(vec![0xAA]),
                maker_payment: old_payment,
            },
            TakerSwapEvent::TakerPaymentSent {
                maker_coin_start_block: 2,
                taker_coin_start_block: 2,
                negotiation_data,
                taker_payment: BytesJson::from(vec![0xBB]),
                maker_payment: latest_payment.clone(),
            },
        ];

        assert_eq!(latest_maker_payment_from_taker_events(&events), Some(&latest_payment));
    }

    /// An error text as a node call may give it, with a part an operator keeps private.
    const NODE_ERROR: &str = "error sending request for url (https://node.example/v3/PRIVATE-KEY-MARKER)";
    const PRIVATE_MARKER: &str = "PRIVATE-KEY-MARKER";

    /// A scripted confirmation read: each attempt takes the next answer and moves the clock on by `step`. The
    /// answer is whether the confirmation was read, the number of attempts, and the reported log lines.
    fn run_own_claim_confirmation(
        deadline: u64,
        start: u64,
        step: u64,
        answers: Vec<Result<(), String>>,
    ) -> (bool, usize, Vec<(Level, String)>) {
        let (confirmed, attempts, lines, _) = run_own_claim_confirmation_sending_again(deadline, start, step, answers);
        (confirmed, attempts, lines)
    }

    /// As [`run_own_claim_confirmation`], with a scripted coin that numbers the claims it sends: the first claim
    /// is number 0, each claim sent again the next number. Each read checks that it reads the latest claim. The
    /// fourth value is the number of claims sent again.
    fn run_own_claim_confirmation_sending_again(
        deadline: u64,
        start: u64,
        step: u64,
        answers: Vec<Result<(), String>>,
    ) -> (bool, usize, Vec<(Level, String)>, usize) {
        use std::cell::{Cell, RefCell};

        let clock = Cell::new(start);
        let attempts = Cell::new(0usize);
        let sent_again = Cell::new(0usize);
        let lines = RefCell::new(Vec::new());
        let mut answers = answers.into_iter();
        let confirmed = common::block_on(await_own_claim_confirmation(
            "ETH claim tx hash ab12",
            deadline,
            0,
            || clock.get(),
            |level, line| lines.borrow_mut().push((level, line)),
            0usize,
            |claim, wait_until| {
                assert_eq!(wait_until, deadline);
                assert_eq!(claim, sent_again.get(), "the wait reads the latest claim");
                attempts.set(attempts.get() + 1);
                clock.set(clock.get() + step);
                let answer = answers.next().expect("no further attempt is expected");
                async move { answer }
            },
            || {
                sent_again.set(sent_again.get() + 1);
                let claim = sent_again.get();
                async move { Ok::<usize, String>(claim) }
            },
        ));
        (confirmed, attempts.get(), lines.into_inner(), sent_again.get())
    }

    /// The error text of a confirmation read whose claim was mined but failed, as the coin marks it.
    fn mined_and_failed_error() -> String {
        format!(
            "{} (ETH receipt status failed)",
            coins::swap_evidence::mined_and_failed("ab12")
        )
    }

    /// A claim that was mined but failed is sent again, and the wait goes on with the new claim; nothing is
    /// refunded (the wait ends only in a confirmation or at its deadline).
    #[test]
    fn own_claim_mined_and_failed_is_sent_again() {
        let answers = vec![Err(mined_and_failed_error()), Ok(())];
        let (confirmed, attempts, lines, sent_again) = run_own_claim_confirmation_sending_again(1000, 100, 10, answers);
        assert!(confirmed, "the claim sent again is confirmed");
        assert_eq!(attempts, 2, "the wait reads twice");
        assert_eq!(sent_again, 1, "the claim is sent again once");
        assert!(
            lines.iter().all(|(_, line)| !line.contains("refund")),
            "no line speaks of a refund"
        );
    }

    /// A claim that is not confirmed yet is only read again; it is never sent again.
    #[test]
    fn own_claim_not_confirmed_yet_is_only_read_again() {
        let answers = vec![
            Err("not confirmed yet".to_string()),
            Err(NODE_ERROR.to_string()),
            Ok(()),
        ];
        let (confirmed, attempts, _, sent_again) = run_own_claim_confirmation_sending_again(1000, 100, 10, answers);
        assert!(confirmed, "the claim is confirmed on the third read");
        assert_eq!(attempts, 3, "the wait reads three times");
        assert_eq!(sent_again, 0, "nothing is sent again");
    }

    /// A second recovery of the same swap is answered at once, and the right is given back when the first ends.
    #[test]
    fn a_second_recovery_of_the_same_swap_is_refused_at_once() {
        let uuid = Uuid::from_slice(&[1u8; 16]).expect("sixteen bytes");
        let first = RecoveryInFlight::enter(uuid).expect("the first recovery runs");
        assert_eq!(RecoveryInFlight::enter(uuid).err().as_deref(), Some(RECOVERY_IN_FLIGHT));
        let another_uuid = Uuid::from_slice(&[2u8; 16]).expect("sixteen bytes");
        let other = RecoveryInFlight::enter(another_uuid).expect("another swap is not held");
        drop(other);
        drop(first);
        drop(RecoveryInFlight::enter(uuid).expect("the right is given back"));
    }

    /// A wait sends a failed claim again at most three times; after that it only reads until the deadline.
    #[test]
    fn own_claim_resends_are_capped_per_wait() {
        let answers = vec![Err(mined_and_failed_error()); 9];
        let (confirmed, attempts, _, sent_again) = run_own_claim_confirmation_sending_again(1000, 100, 100, answers);
        assert!(!confirmed, "the wait ends unconfirmed at the deadline");
        assert_eq!(sent_again, 3, "the claim is sent again three times only");
        assert!(attempts > 4, "the wait goes on reading after the cap");

        let mut answers = vec![Err(mined_and_failed_error()); 4];
        answers.push(Ok(()));
        let (confirmed, _, _, sent_again) = run_own_claim_confirmation_sending_again(100_000, 100, 10, answers);
        assert!(confirmed, "the last claim sent is read until it is confirmed");
        assert_eq!(sent_again, 3);
    }

    /// A claim that was mined but failed at the deadline is not sent again, and the wait ends unconfirmed.
    #[test]
    fn own_claim_mined_and_failed_at_the_deadline_ends_the_wait() {
        let answers = vec![Err(mined_and_failed_error())];
        let (confirmed, _, _, sent_again) = run_own_claim_confirmation_sending_again(1000, 995, 10, answers);
        assert!(!confirmed, "the wait ends unconfirmed");
        assert_eq!(sent_again, 0, "nothing is sent again after the deadline");
    }

    /// A UTXO coin never marks a confirmation error as a mined claim that failed; only the marker does.
    #[test]
    fn only_the_marker_tells_a_mined_claim_that_failed() {
        assert!(coins::swap_evidence::is_mined_and_failed(&mined_and_failed_error()));
        for other in [
            "not confirmed yet",
            NODE_ERROR,
            "Waited too long until 10 for transaction",
        ] {
            assert!(
                !coins::swap_evidence::is_mined_and_failed(other),
                "an unmarked error is not a failed claim"
            );
        }
    }

    // The reads before a refund (CRD ch.51 R31a, R82, §51.6G R100) -------------------------------------------

    /// The search answer as the coin gives it: "timed out" only after it asked its nodes (clock-first, as a coin
    /// whose reads can be undetermined must answer), and the marker while the nodes do not agree.
    fn scripted_search(answer: &str) -> Result<Option<(bool, &'static str)>, String> {
        let error = match answer {
            "timed out" => coins::FindPaymentSpendError::Timeout {
                wait_until: 10,
                now: 11,
            },
            _ => coins::FindPaymentSpendError::Transport(undetermined_error()),
        };
        spend_search_error(&MmError::new(error))
    }

    /// A determined "not spent" lets the refund go on, for the maker and for the taker.
    #[test]
    fn a_determined_not_spent_look_lets_the_refund_go_on() {
        for other_spend_refunds in [false, true] {
            assert_eq!(
                judge_look(scripted_search("timed out"), other_spend_refunds),
                Look::RefundGoesOn,
                "a determined not-spent refunds"
            );
        }
    }

    /// An undetermined look never refunds, for the maker and for the taker.
    #[test]
    fn an_undetermined_look_never_refunds() {
        for other_spend_refunds in [false, true] {
            assert!(
                matches!(
                    judge_look(scripted_search("undetermined"), other_spend_refunds),
                    Look::Unanswered(_)
                ),
                "an undetermined look does not refund"
            );
        }
    }

    /// A spend whose secret matches the swap's secret hash leads to the claim, never to the refund.
    #[test]
    fn a_spent_payment_leads_to_the_claim_not_the_refund() {
        for other_spend_refunds in [false, true] {
            assert_eq!(
                judge_look(Ok(Some((true, "spend"))), other_spend_refunds),
                Look::Claimed("spend"),
                "a proven spend leads to the claim"
            );
        }
    }

    /// The maker's look: a spend told to be the taker's refund lets the refund go on whatever the extraction says;
    /// a claim with the maker's own secret is its own claim; a claim whose secret cannot be extracted or does not
    /// match is no answer: the refund does not go on and the look waits.
    #[test]
    fn the_maker_look_refunds_after_a_taker_refund_and_waits_on_an_unreadable_claim() {
        let own = [7u8; 32];
        let no_secret = || Err::<[u8; 32], _>("no secret".to_owned());
        assert_eq!(
            judge_look(maker_look_found(true, no_secret(), &own, "spend"), true),
            Look::RefundGoesOn,
            "the taker's refund of the taker payment"
        );
        assert_eq!(
            judge_look(maker_look_found(false, Ok::<_, String>(own), &own, "spend"), true),
            Look::Claimed("spend"),
            "a claim with the maker's secret"
        );
        assert_eq!(
            judge_look(maker_look_found(false, no_secret(), &own, "spend"), true),
            Look::Unanswered(SPEND_SECRET_NOT_EXTRACTED.to_owned()),
            "a claim whose secret cannot be extracted, or a spend of an unknown kind"
        );
        assert_eq!(
            judge_look(maker_look_found(false, Ok::<_, String>([8u8; 32]), &own, "spend"), true),
            Look::Unanswered(SPEND_NOT_ACTED_ON.to_owned()),
            "a claim with another secret"
        );
    }

    /// A failed read of a spend search is no "not spent": the look stays unanswered, for both sides.
    #[test]
    fn a_failed_spend_read_leaves_the_look_unanswered() {
        let failed = MmError::new(coins::FindPaymentSpendError::Transport(undetermined_error()));
        for other_spend_refunds in [false, true] {
            assert!(matches!(
                judge_look::<&str>(spend_search_error(&failed), other_spend_refunds),
                Look::Unanswered(_)
            ));
        }
    }

    /// A spend without the secret is the taker's own refund for the maker (the refund goes on), and no answer
    /// the taker may act on.
    #[test]
    fn a_spend_without_the_secret_refunds_only_for_the_maker() {
        assert_eq!(judge_look(Ok(Some((false, "spend"))), true), Look::RefundGoesOn);
        assert_eq!(
            judge_look(Ok(Some((false, "spend"))), false),
            Look::Unanswered(SPEND_NOT_ACTED_ON.to_owned())
        );
    }

    /// A refund step whose reads give no answer waits; the count starts at the lock time, and the step ends
    /// at the bound of R100 item 9 and not before.
    #[test]
    fn a_refund_step_without_an_answer_waits_until_the_bound() {
        let mut wait = UndeterminedWait::new(10_000);
        assert_eq!(
            wait.after_unanswered(9_000),
            AfterRead::ReadAgain,
            "before the lock time"
        );
        assert_eq!(wait.after_unanswered(10_100), AfterRead::ReadAgain, "the count starts");
        assert_eq!(wait.after_unanswered(13_799), AfterRead::ReadAgain, "before the bound");
        assert_eq!(wait.after_unanswered(13_800), AfterRead::BoundPassed, "at the bound");
    }

    /// The abort of a refund step whose saved events lack the read's inputs says so, names the coin and keeps
    /// the reason variant of its state.
    #[test]
    fn the_abort_of_a_read_without_its_inputs_says_so() {
        let cases = [
            (CappedRead::MakerRefundLook, "maker payment"),
            (CappedRead::TakerRefundLook, "taker payment"),
            (CappedRead::FundingRefundSearch, "taker funding"),
        ];
        for (read, payment) in cases {
            let reason = history_lacks_inputs_reason(read, "RIN");
            let own_variant = match read {
                CappedRead::MakerRefundLook => matches!(reason, AbortReason::InternalError(_)),
                CappedRead::TakerRefundLook => matches!(reason, AbortReason::TakerPaymentRefundFailed(_)),
                _ => matches!(reason, AbortReason::TakerFundingRefundFailed(_)),
            };
            assert!(own_variant, "the state's own reason variant");
            let text = reason.to_string();
            assert!(text.contains("RIN"), "the coin is named");
            assert!(text.contains("saved events"), "the missing inputs are named");
            assert!(text.contains(payment), "what is not refunded is named");
        }
    }

    /// Before the funding locktime the funding is refunded with the taker's secret; from the locktime on by
    /// its timelock, after the refund gate.
    #[test]
    fn the_funding_refund_takes_the_timelock_branch_from_the_locktime_on() {
        assert_eq!(funding_refund_branch(9_999, 10_000), FundingRefundBranch::Secret);
        assert_eq!(funding_refund_branch(10_000, 10_000), FundingRefundBranch::Timelock);
        assert_eq!(funding_refund_branch(20_000, 10_000), FundingRefundBranch::Timelock);
    }

    // What the refund steps and the recovery read from the saved events -------------------------------------

    fn maker_negotiation() -> StoredMakerNegotiationData {
        StoredMakerNegotiationData {
            taker_secret_hash: BytesJson::from(vec![1; 20]),
            taker_coin_htlc_pub: BytesJson::from(vec![2; 33]),
            maker_coin_htlc_pub: BytesJson::from(vec![3; 33]),
            taker_coin_swap_contract: None,
            maker_coin_swap_contract: None,
            taker_payment_locktime: 1,
            taker_funding_locktime: 2,
        }
    }

    fn taker_negotiation() -> StoredTakerNegotiationData {
        StoredTakerNegotiationData {
            maker_secret_hash: BytesJson::from(vec![1; 20]),
            maker_coin_htlc_pub: BytesJson::from(vec![2; 33]),
            taker_coin_htlc_pub: BytesJson::from(vec![3; 33]),
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            maker_payment_locktime: 1,
            taker_coin_address: "0x0".into(),
        }
    }

    fn maker_refund_required() -> MakerSwapEvent {
        MakerSwapEvent::MakerPaymentRefundRequired {
            maker_coin_start_block: 1,
            taker_coin_start_block: 2,
            negotiation_data: maker_negotiation(),
            maker_payment: BytesJson::from(vec![0xAA]),
            reason: AbortReason::MakerDidNotSpendInTime("late".into()),
        }
    }

    /// The maker's look reads the taker payment of its claim step; a swap that never reached it needs no look,
    /// and saved events with neither leave the look without its inputs.
    #[test]
    fn the_maker_reads_its_claim_step_from_the_saved_events() {
        use super::super::maker_swap_v2::{claim_step_from_history, ClaimStep};

        let paid = MakerSwapEvent::MakerPaymentSentFundingSpendGenerated {
            maker_coin_start_block: 1,
            taker_coin_start_block: 2,
            negotiation_data: maker_negotiation(),
            maker_payment: BytesJson::from(vec![0xAA]),
            taker_funding: BytesJson::from(vec![0xBB]),
            funding_spend_preimage: StoredTxPreimage {
                preimage: BytesJson::from(vec![0xCC]),
                signature: BytesJson::from(vec![0xDD]),
            },
        };
        let received = MakerSwapEvent::TakerPaymentReceived {
            maker_coin_start_block: 1,
            taker_coin_start_block: 2,
            negotiation_data: maker_negotiation(),
            maker_payment: BytesJson::from(vec![0xAA]),
            taker_payment: BytesJson::from(vec![0xEE]),
        };
        let reached = vec![paid.clone(), received, maker_refund_required()];
        assert_eq!(claim_step_from_history(&reached), ClaimStep::Reached {
            taker_payment: BytesJson::from(vec![0xEE])
        });
        assert_eq!(
            claim_step_from_history(&[paid, maker_refund_required()]),
            ClaimStep::NotReached
        );
        assert_eq!(claim_step_from_history(&[maker_refund_required()]), ClaimStep::Unknown);
    }

    /// The taker's look reads the maker payment and the start blocks from the latest event that holds them.
    #[test]
    fn the_taker_reads_the_claim_inputs_from_the_saved_events() {
        use super::super::taker_swap_v2::{claim_inputs_from_history, ClaimInputs};

        let sent = TakerSwapEvent::TakerPaymentSent {
            maker_coin_start_block: 7,
            taker_coin_start_block: 8,
            negotiation_data: taker_negotiation(),
            taker_payment: BytesJson::from(vec![0xBB]),
            maker_payment: BytesJson::from(vec![0xAA]),
        };
        let refund = TakerSwapEvent::TakerPaymentRefundRequired {
            taker_payment: BytesJson::from(vec![0xBB]),
            negotiation_data: taker_negotiation(),
            reason: AbortReason::MakerDidNotSpendInTime("late".into()),
        };
        assert_eq!(
            claim_inputs_from_history(&[sent, refund.clone()]),
            Some(ClaimInputs {
                maker_coin_start_block: 7,
                taker_coin_start_block: 8,
                maker_payment: BytesJson::from(vec![0xAA]),
            })
        );
        assert_eq!(claim_inputs_from_history(&[refund]), None, "no event holds the inputs");
    }

    /// The recovery accepts a maker swap that ended at the bound of its refund step, and no other ending.
    #[test]
    fn the_recovery_accepts_only_a_maker_swap_that_ended_at_its_refund_bound() {
        use super::super::maker_swap_v2::recoverable_maker_refund;
        use coins::eth::EthCoin;

        let bound = MakerSwapEvent::Aborted {
            reason: undetermined_cap_reason(CappedRead::MakerRefundLook, "RIN", &undetermined_error()),
        };
        let ended_at_bound = vec![maker_refund_required(), bound.clone()];
        assert!(recoverable_maker_refund::<EthCoin, EthCoin>(&ended_at_bound).is_some());

        let other_endings = [
            vec![maker_refund_required(), MakerSwapEvent::Completed],
            vec![bound.clone()],
            vec![maker_refund_required(), MakerSwapEvent::Aborted {
                reason: AbortReason::TakerPaymentRefundFailed("not the maker's".into()),
            }],
            vec![maker_refund_required(), MakerSwapEvent::MakerPaymentRefunded {
                maker_payment: BytesJson::from(vec![0xAA]),
                maker_payment_refund: BytesJson::from(vec![0xAB]),
                reason: AbortReason::MakerDidNotSpendInTime("late".into()),
            }],
        ];
        for events in other_endings {
            assert!(
                recoverable_maker_refund::<EthCoin, EthCoin>(&events).is_none(),
                "another ending is not recovered"
            );
        }
    }

    /// The recovery accepts a taker swap that ended at the bound of either refund step, and no other ending.
    #[test]
    fn the_recovery_accepts_only_a_taker_swap_that_ended_at_a_refund_bound() {
        use super::super::taker_swap_v2::{recoverable_taker_refund, RecoverableTakerRefund};
        use coins::eth::EthCoin;

        let payment_refund = TakerSwapEvent::TakerPaymentRefundRequired {
            taker_payment: BytesJson::from(vec![0xBB]),
            negotiation_data: taker_negotiation(),
            reason: AbortReason::MakerDidNotSpendInTime("late".into()),
        };
        let funding_refund = TakerSwapEvent::TakerFundingRefundRequired {
            maker_coin_start_block: 1,
            taker_coin_start_block: 2,
            negotiation_data: taker_negotiation(),
            taker_funding: BytesJson::from(vec![0xCC]),
            reason: AbortReason::MakerDidNotSpendInTime("late".into()),
        };
        let payment_bound = TakerSwapEvent::Aborted {
            reason: undetermined_cap_reason(CappedRead::TakerRefundLook, "RIN", &undetermined_error()),
        };
        let funding_bound = TakerSwapEvent::Aborted {
            reason: undetermined_cap_reason(CappedRead::FundingRefundSearch, "RIN", &undetermined_error()),
        };
        assert!(matches!(
            recoverable_taker_refund::<EthCoin, EthCoin>(&[payment_refund.clone(), payment_bound.clone()]),
            Some(RecoverableTakerRefund::Payment(_))
        ));
        assert!(matches!(
            recoverable_taker_refund::<EthCoin, EthCoin>(&[funding_refund.clone(), funding_bound.clone()]),
            Some(RecoverableTakerRefund::Funding(_))
        ));
        let other_endings = [
            vec![payment_refund.clone(), funding_bound],
            vec![funding_refund, payment_bound],
            vec![payment_refund, TakerSwapEvent::Completed],
        ];
        for events in other_endings {
            assert!(
                recoverable_taker_refund::<EthCoin, EthCoin>(&events).is_none(),
                "another ending is not recovered"
            );
        }
    }

    /// The new transitions: the refund states go to the claim when the look finds it, and the funding refund
    /// goes to the refund of the taker payment when the funding became that payment.
    #[test]
    fn the_refund_states_may_go_to_the_claim_and_the_funding_refund_to_the_payment_refund() {
        use super::super::maker_swap_v2 as maker;
        use super::super::taker_swap_v2 as taker;
        use coins::eth::EthCoin;
        use mm2_state_machine::prelude::TransitionFrom;

        fn accepts<Next: TransitionFrom<Prev>, Prev>() {}
        accepts::<maker::TakerPaymentSpent<EthCoin, EthCoin>, maker::MakerPaymentRefundRequired<EthCoin, EthCoin>>();
        accepts::<taker::TakerPaymentSpent<EthCoin, EthCoin>, taker::TakerPaymentRefundRequired<EthCoin, EthCoin>>();
        accepts::<
            taker::TakerPaymentRefundRequired<EthCoin, EthCoin>,
            taker::TakerFundingRefundRequired<EthCoin, EthCoin>,
        >();
    }

    /// CRD ch.52 R19, R33: a confirmed own claim ends the wait at once, with nothing logged.
    #[test]
    fn own_claim_confirmation_succeeds_on_the_first_read() {
        let (confirmed, attempts, lines) = run_own_claim_confirmation(1000, 100, 10, vec![Ok(())]);
        assert!(confirmed);
        assert_eq!(attempts, 1);
        assert!(lines.is_empty());
    }

    /// CRD ch.52 R19, R33: a failed read before the deadline is repeated, and a later success completes.
    #[test]
    fn own_claim_confirmation_is_read_again_after_a_failure_before_the_deadline() {
        let answers = vec![Err("node down".to_string()), Err("node down".to_string()), Ok(())];
        let (confirmed, attempts, _) = run_own_claim_confirmation(1000, 100, 10, answers);
        assert!(confirmed);
        assert_eq!(attempts, 3);
    }

    /// CRD ch.52 R19, R33: a read that fails at or after the deadline ends the wait unconfirmed, which the
    /// state turns into `Aborted`.
    #[test]
    fn own_claim_confirmation_ends_unconfirmed_once_the_deadline_has_passed() {
        let answers = vec![Err("first".to_string()), Err("second".to_string())];
        let (confirmed, attempts, _) = run_own_claim_confirmation(1000, 985, 10, answers);
        assert!(!confirmed);
        assert_eq!(attempts, 2);
    }

    /// One wait gives one warning, at its first failed read, which names the claim and carries no error text.
    /// The error text of every failed read, the first one included, is reported at debug level only.
    #[test]
    fn own_claim_confirmation_warns_once_per_wait_and_keeps_error_text_at_debug_level() {
        let answers = vec![Err(NODE_ERROR.to_string()); 4];
        let (confirmed, attempts, lines) = run_own_claim_confirmation(1000, 965, 10, answers);
        assert!(!confirmed);
        assert_eq!(attempts, 4);

        let warnings: Vec<&String> = lines
            .iter()
            .filter(|(l, _)| *l == Level::Warn)
            .map(|(_, t)| t)
            .collect();
        assert_eq!(warnings.len(), 1, "{lines:?}");
        assert!(warnings[0].contains("ETH claim tx hash ab12"), "{}", warnings[0]);
        assert_eq!(lines[1].0, Level::Warn, "the warning follows the first failed read");

        let debug: Vec<&String> = lines
            .iter()
            .filter(|(l, _)| *l == Level::Debug)
            .map(|(_, t)| t)
            .collect();
        assert_eq!(debug.len(), 4, "{lines:?}");
        assert!(debug.iter().all(|line| line.contains(NODE_ERROR)));
        for (level, line) in &lines {
            assert!(*level == Level::Debug || !line.contains(PRIVATE_MARKER), "{line}");
            assert!(matches!(level, Level::Debug | Level::Warn));
        }
    }

    /// CRD ch.51 R100 item 9: the wait lasts 3700 seconds past the lock time, and a state entered later, as
    /// after a restart, has a full 3700 seconds.
    #[test]
    fn own_claim_confirmation_deadline_is_the_later_of_the_two_moments() {
        assert_eq!(own_claim_confirmation_deadline(10_000, 2_000), 13_700);
        assert_eq!(own_claim_confirmation_deadline(10_000, 10_000), 13_700);
        assert_eq!(own_claim_confirmation_deadline(10_000, 50_000), 53_700);
    }

    /// The abort text is the fixed phrase and then the claim, named by its hash, or by its bytes when they do
    /// not parse.
    #[test]
    fn own_claim_reason_text_opens_with_the_phrase_and_names_the_claim_transaction() {
        let by_hash = own_claim_not_confirmed_text(&own_claim_name("ETH", Ok("ab12".into())));
        assert_eq!(
            by_hash,
            "Own claim was broadcast but its confirmation could not be read: ETH claim tx hash ab12"
        );
        let by_bytes = own_claim_not_confirmed_text(&own_claim_name("ETH", Err("f86b".into())));
        assert_eq!(
            by_bytes,
            format!("{OWN_CLAIM_NOT_CONFIRMED_PHRASE}ETH claim tx bytes f86b")
        );
    }

    /// The abort reason of a wait whose reads failed with a node error carries no part of that error text.
    #[test]
    fn own_claim_abort_reasons_carry_no_error_text() {
        let claim = own_claim_name("ETH", Ok("ab12".into()));
        let (confirmed, _, _) = run_own_claim_confirmation(1000, 2000, 10, vec![Err(NODE_ERROR.to_string())]);
        assert!(!confirmed);
        for reason in [
            maker_own_claim_not_confirmed_reason(&claim),
            taker_own_claim_not_confirmed_reason(&claim),
        ] {
            let shown = reason.to_string();
            let stored = serde_json::to_string(&reason).unwrap();
            for text in [shown, stored] {
                assert!(!text.contains(PRIVATE_MARKER), "{text}");
                assert!(!text.contains("url"), "{text}");
                assert!(text.ends_with("ETH claim tx hash ab12") || text.ends_with("ETH claim tx hash ab12\"}"));
            }
        }
    }

    /// CRD ch.52 R19, R44: the maker's abort after its own claim is `InternalError` with the phrase first.
    #[test]
    fn maker_own_claim_abort_is_an_internal_error_that_opens_with_the_phrase() {
        match maker_own_claim_not_confirmed_reason(&own_claim_name("ETH", Ok("ab12".into()))) {
            AbortReason::InternalError(text) => {
                assert!(text.starts_with(OWN_CLAIM_NOT_CONFIRMED_PHRASE), "{text}");
                assert!(text.contains("ETH") && text.contains("ab12"), "{text}");
            },
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    /// CRD ch.52 R33, R44: the taker's abort after its own claim is `MakerPaymentSpendNotConfirmedInTime` with
    /// the phrase first.
    #[test]
    fn taker_own_claim_abort_is_a_spend_not_confirmed_reason_that_opens_with_the_phrase() {
        match taker_own_claim_not_confirmed_reason(&own_claim_name("RIN", Ok("cd34".into()))) {
            AbortReason::MakerPaymentSpendNotConfirmedInTime(text) => {
                assert!(text.starts_with(OWN_CLAIM_NOT_CONFIRMED_PHRASE), "{text}");
                assert!(text.contains("RIN") && text.contains("cd34"), "{text}");
            },
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    /// CRD ch.52 R44: the phrase is written in one place, and the reasons of both machines that carry it are
    /// built there. An abort with nothing committed, such as the maker's `ConfirmationTimeout`, is built from
    /// its own text and so never carries the phrase.
    #[test]
    fn only_the_own_claim_abort_carries_the_phrase() {
        fn production(file: &str) -> &str {
            let end = file.find("mod tests {").expect("the file has a test module");
            &file[..end]
        }
        let phrase = OWN_CLAIM_NOT_CONFIRMED_PHRASE.trim_end();
        let maker = production(include_str!("maker_swap_v2.rs"));
        let taker = production(include_str!("taker_swap_v2.rs"));
        let common = production(include_str!("swap_v2_common.rs"));

        assert_eq!(common.matches(phrase).count(), 1);
        assert_eq!(common.matches("own_claim_not_confirmed_text(").count(), 3);
        for source in [maker, taker] {
            assert!(!source.contains(phrase));
            assert!(!source.contains("OWN_CLAIM_NOT_CONFIRMED_PHRASE"));
            assert!(!source.contains("own_claim_not_confirmed_text"));
        }
        assert_eq!(maker.matches("maker_own_claim_not_confirmed_reason(").count(), 1);
        assert_eq!(taker.matches("taker_own_claim_not_confirmed_reason(").count(), 1);

        let timeouts: Vec<&str> = maker.split("AbortReason::ConfirmationTimeout(").skip(1).collect();
        assert!(!timeouts.is_empty());
        for after in timeouts {
            assert!(after.starts_with("format!(\"Taker "), "{}", &after[..40]);
        }
        let nothing_committed = AbortReason::ConfirmationTimeout("Taker funding not confirmed in time: late".into());
        assert!(!nothing_committed.to_string().contains(phrase));
    }

    /// An undetermined read as a coin reports it, with node names that must not reach an event.
    fn undetermined_error() -> String {
        coins::swap_evidence::undetermined("the nodes disagree", &["node.example:443", "node-b.example"])
    }

    /// A scripted read under [`UndeterminedWait::read`]: each attempt takes the next answer and moves the clock on
    /// by `step`. The answer is how the read ended, the number of attempts, and whether the wait saw an
    /// undetermined read.
    fn run_read<T, E: std::fmt::Display>(
        lock_time: u64,
        start: u64,
        step: u64,
        answers: Vec<Result<T, E>>,
    ) -> (ReadEnd<T, E>, usize, bool) {
        use std::cell::Cell;

        let clock = Cell::new(start);
        let attempts = Cell::new(0usize);
        let mut answers = answers.into_iter();
        let mut wait = UndeterminedWait::new(lock_time);
        let end = common::block_on(wait.read(
            &Uuid::nil(),
            "RIN",
            CappedRead::TakerPaymentSpendSearch,
            0,
            || clock.get(),
            |_| {
                attempts.set(attempts.get() + 1);
                clock.set(clock.get() + step);
                let answer = answers.next().expect("no further attempt is expected");
                async move { answer }
            },
        ));
        (end, attempts.get(), wait.waited())
    }

    /// CRD ch.51 R100, the note on how the code applies it, item 6: the marker is recognized wherever the error
    /// text carries it, also inside the text of another error.
    #[test]
    fn an_undetermined_read_is_recognized_when_its_text_is_wrapped() {
        let wrapped = format!("lp_coins:1234] RPC error: \"{}\"", undetermined_error());
        let mut wait = UndeterminedWait::new(10_000);
        assert_eq!(wait.after_read(Some(&wrapped), 100), AfterRead::ReadAgain);
        assert!(wait.waited());

        let in_a_search_error = coins::SearchForFundingSpendErr::Rpc(undetermined_error()).to_string();
        assert_eq!(wait.after_read(Some(&in_a_search_error), 100), AfterRead::ReadAgain);

        let in_a_find_error = MmError::new(coins::FindPaymentSpendError::Transport(undetermined_error())).to_string();
        assert_eq!(wait.after_read(Some(&in_a_find_error), 100), AfterRead::ReadAgain);
    }

    /// An answer, and an error without the marker, are determined: the step keeps its own handling of them.
    #[test]
    fn an_answer_and_a_plain_error_are_determined() {
        let mut wait = UndeterminedWait::new(10_000);
        assert_eq!(wait.after_read(None, 100), AfterRead::Determined);
        assert_eq!(wait.after_read(Some(NODE_ERROR), 100), AfterRead::Determined);
        assert_eq!(wait.after_read(Some(NODE_ERROR), 99_999), AfterRead::Determined);
        assert!(!wait.waited());
    }

    /// CRD ch.51 R100 item 9, its second note: only an undetermined answer at or after the lock time is the
    /// moment the step first could act. One before the lock time is waited on and does not start the 3700
    /// seconds.
    #[test]
    fn an_undetermined_answer_before_the_lock_time_does_not_start_the_bound() {
        let undetermined = undetermined_error();
        let lock_time = 10_000;
        let mut wait = UndeterminedWait::new(lock_time);
        assert_eq!(wait.after_read(Some(&undetermined), 2_000), AfterRead::ReadAgain);
        assert!(wait.waited());
        assert_eq!(wait.after_read(None, 9_000), AfterRead::Determined);
        assert_eq!(wait.after_read(None, lock_time + 2_999), AfterRead::Determined);
        assert_eq!(
            wait.after_read(Some(&undetermined), lock_time + 3_000),
            AfterRead::ReadAgain
        );
        assert_eq!(
            wait.after_read(Some(&undetermined), lock_time + 3_700),
            AfterRead::ReadAgain
        );
        assert_eq!(
            wait.after_read(Some(&undetermined), lock_time + 6_699),
            AfterRead::ReadAgain
        );
        assert_eq!(
            wait.after_read(Some(&undetermined), lock_time + 6_700),
            AfterRead::BoundPassed
        );

        // Undetermined answers that all come before the lock time never end the wait.
        let mut before = UndeterminedWait::new(lock_time);
        assert_eq!(before.after_read(Some(&undetermined), 100), AfterRead::ReadAgain);
        assert_eq!(
            before.after_read(Some(&undetermined), lock_time - 1),
            AfterRead::ReadAgain
        );
    }

    /// CRD ch.51 R100 item 9 with its notes: the wait ends at the later of the lock time plus 3700 seconds and
    /// 3700 seconds after its first undetermined read at or after the lock time. A determined read in between
    /// does not move that moment.
    #[test]
    fn an_undetermined_wait_ends_at_the_later_of_the_two_moments() {
        let undetermined = undetermined_error();

        let mut early = UndeterminedWait::new(10_000);
        assert_eq!(early.after_read(Some(&undetermined), 2_000), AfterRead::ReadAgain);
        assert_eq!(early.after_read(Some(&undetermined), 10_000), AfterRead::ReadAgain);
        assert_eq!(early.after_read(None, 12_000), AfterRead::Determined);
        assert_eq!(early.after_read(Some(&undetermined), 13_699), AfterRead::ReadAgain);
        assert_eq!(early.after_read(Some(&undetermined), 13_700), AfterRead::BoundPassed);

        let mut late = UndeterminedWait::new(10_000);
        assert_eq!(late.after_read(Some(&undetermined), 50_000), AfterRead::ReadAgain);
        assert_eq!(late.after_read(Some(&undetermined), 53_699), AfterRead::ReadAgain);
        assert_eq!(late.after_read(Some(&undetermined), 53_700), AfterRead::BoundPassed);

        // A wait that is started again, as after a restart, has a full 3700 seconds.
        let mut resumed = UndeterminedWait::new(10_000);
        assert_eq!(resumed.after_read(Some(&undetermined), 53_700), AfterRead::ReadAgain);
    }

    /// An undetermined read is read again, and the determined answer that follows is what the step gets.
    #[test]
    fn a_read_goes_on_after_undetermined_answers_once_it_is_determined() {
        let answers: Vec<Result<u8, String>> = vec![Err(undetermined_error()), Err(undetermined_error()), Ok(7)];
        let (end, attempts, waited) = run_read(10_000, 100, 10, answers);
        assert!(matches!(end, ReadEnd::Answer(7)));
        assert_eq!(attempts, 3);
        assert!(waited);
    }

    /// A read that stays undetermined through the lock time gives no answer and no other failure: it ends only at
    /// the bound, and then as `BoundPassed`.
    #[test]
    fn a_read_that_stays_undetermined_ends_only_at_the_bound() {
        // The first undetermined read is at 9_000, so the bound is 10_000 + 3700; the clock moves 1000 per read.
        let answers: Vec<Result<u8, String>> = vec![Err(undetermined_error()); 6];
        let (end, attempts, waited) = run_read(10_000, 8_000, 1_000, answers);
        assert!(matches!(end, ReadEnd::BoundPassed(_)));
        assert_eq!(
            attempts, 6,
            "reads at 9000, 10000, 11000, 12000 and 13000 wait; the one at 14000 ends"
        );
        assert!(waited);
    }

    /// An error without the marker ends the read at once as a failure, at any time, with its own error.
    #[test]
    fn a_read_ends_at_once_on_an_error_that_is_not_undetermined() {
        let (end, attempts, waited) = run_read::<u8, String>(10_000, 100, 10, vec![Err(NODE_ERROR.to_string())]);
        assert!(matches!(end, ReadEnd::Failed(ref e) if e == NODE_ERROR));
        assert_eq!(attempts, 1);
        assert!(!waited);

        let answers: Vec<Result<u8, String>> = vec![Err(undetermined_error()), Err(NODE_ERROR.to_string())];
        let (end, attempts, _) = run_read(10_000, 100, 10, answers);
        assert!(matches!(end, ReadEnd::Failed(_)));
        assert_eq!(attempts, 2);
    }

    /// CRD ch.52 R18: an undetermined search for the funding spend is never taken as "nothing found" or as a
    /// passing node error; the answer of the search that follows is taken.
    #[test]
    fn the_funding_spend_search_waits_on_an_undetermined_answer() {
        use coins::SearchForFundingSpendErr;

        let answers: Vec<Result<Option<u8>, SearchForFundingSpendErr>> =
            vec![Err(SearchForFundingSpendErr::Rpc(undetermined_error())), Ok(Some(1))];
        let (end, attempts, _) = run_read(10_000, 100, 10, answers);
        assert!(matches!(end, ReadEnd::Answer(Some(1))));
        assert_eq!(attempts, 2);

        // Past the search deadline and the lock time the search still waits, up to the bound.
        let answers: Vec<Result<Option<u8>, SearchForFundingSpendErr>> = vec![
            Err(SearchForFundingSpendErr::Rpc(undetermined_error())),
            Err(SearchForFundingSpendErr::Rpc(undetermined_error())),
            Ok(None),
        ];
        let (end, attempts, _) = run_read(10_000, 11_000, 1_000, answers);
        assert!(matches!(end, ReadEnd::Answer(None)));
        assert_eq!(attempts, 3);

        let plain: Vec<Result<Option<u8>, SearchForFundingSpendErr>> =
            vec![Err(SearchForFundingSpendErr::Rpc(NODE_ERROR.to_string()))];
        let (end, _, _) = run_read(10_000, 100, 10, plain);
        assert!(matches!(end, ReadEnd::Failed(SearchForFundingSpendErr::Rpc(_))));
    }

    /// What the nodes of [`ClockFirstCoin`] answer to one read.
    #[derive(Clone, Copy)]
    enum NodeRead {
        /// The spend is found, or the transaction is confirmed.
        Yes,
        /// A determined "not spent" or "not confirmed".
        NotYet,
        Undetermined,
    }

    const TIMED_OUT: &str = "Waited too long";

    /// A scripted coin read that keeps the order of the real ones: it checks the clock first and answers "timed
    /// out" without asking a node once `wait_until` has passed; otherwise it asks every ten seconds until
    /// `wait_until`. An undetermined read is answered with the marker at once.
    struct ClockFirstCoin {
        clock: std::cell::Cell<u64>,
        calls: std::cell::Cell<usize>,
        node_reads: std::cell::Cell<usize>,
        script: std::cell::RefCell<std::collections::VecDeque<NodeRead>>,
        wait_untils: std::cell::RefCell<Vec<u64>>,
    }

    impl ClockFirstCoin {
        fn new(start: u64, script: Vec<NodeRead>) -> Self {
            ClockFirstCoin {
                clock: std::cell::Cell::new(start),
                calls: std::cell::Cell::new(0),
                node_reads: std::cell::Cell::new(0),
                script: std::cell::RefCell::new(script.into()),
                wait_untils: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn read(&self, wait_until: u64) -> Result<(), String> {
            self.calls.set(self.calls.get() + 1);
            self.wait_untils.borrow_mut().push(wait_until);
            loop {
                if self.clock.get() > wait_until {
                    return Err(TIMED_OUT.to_string());
                }
                self.node_reads.set(self.node_reads.get() + 1);
                let answer = self
                    .script
                    .borrow_mut()
                    .pop_front()
                    .expect("no further node read is expected");
                self.clock.set(self.clock.get() + 10);
                match answer {
                    NodeRead::Yes => return Ok(()),
                    NodeRead::Undetermined => return Err(undetermined_error()),
                    NodeRead::NotYet => (),
                }
            }
        }

        /// The search for a spend under [`UndeterminedWait::read`], as the taker's states run it: the step's
        /// deadline is the lock time.
        fn search(&self, lock_time: u64) -> ReadEnd<(), String> {
            common::block_on(UndeterminedWait::new(lock_time).read(
                &Uuid::nil(),
                "RIN",
                CappedRead::TakerPaymentSpendSearch,
                0,
                || self.clock.get(),
                |min_wait_until| {
                    let answer = self.read(coin_wait_until(lock_time, min_wait_until));
                    async move { answer }
                },
            ))
        }

        /// A confirmation wait under [`UndeterminedWait::read_confirmation`].
        fn confirmation(&self, lock_time: u64, deadline: u64) -> ConfirmationEnd {
            common::block_on(UndeterminedWait::new(lock_time).read_confirmation(
                &Uuid::nil(),
                "RIN",
                CappedRead::TakerPaymentConfirmation,
                deadline,
                0,
                || self.clock.get(),
                |wait_until| {
                    let answer = self.read(wait_until);
                    async move { answer }
                },
            ))
        }
    }

    /// CRD ch.52 R31: a search for the spend of the taker payment that is undetermined just before the locktime
    /// is never ended by the coin's clock. Every search after it reaches the nodes, also past the locktime, and
    /// the wait ends only at its bound.
    #[test]
    fn an_undetermined_spend_search_near_the_locktime_is_not_ended_by_the_clock() {
        let coin = ClockFirstCoin::new(9_995, vec![NodeRead::Undetermined; 400]);
        let end = coin.search(10_000);
        assert!(matches!(end, ReadEnd::BoundPassed(_)));
        // The first answer at or after the locktime comes at 10_005, so the bound is 13_705.
        assert!(coin.clock.get() >= 13_705);
        assert_eq!(
            coin.calls.get(),
            coin.node_reads.get(),
            "every call of the coin reached the nodes"
        );
        assert_eq!(coin.wait_untils.borrow()[0], 10_000);
        assert!(coin.wait_untils.borrow()[1..]
            .iter()
            .all(|wait_until| *wait_until > 10_000));
    }

    /// CRD ch.52 R31: after an undetermined search, "timed out" past the locktime is taken as "the maker did not
    /// spend" only from a search that reached the nodes and found no spend; a spend that is found is taken.
    #[test]
    fn a_spend_search_after_an_undetermined_one_times_out_only_after_a_real_read() {
        let coin = ClockFirstCoin::new(9_995, vec![NodeRead::Undetermined, NodeRead::NotYet]);
        let end = coin.search(10_000);
        assert!(matches!(end, ReadEnd::Failed(ref e) if e == TIMED_OUT));
        assert_eq!(
            coin.node_reads.get(),
            2,
            "the second search asked the nodes before it timed out"
        );

        let coin = ClockFirstCoin::new(9_995, vec![NodeRead::Undetermined, NodeRead::Yes]);
        assert!(matches!(coin.search(10_000), ReadEnd::Answer(())));

        let coin = ClockFirstCoin::new(9_000, vec![
            NodeRead::Undetermined,
            NodeRead::Undetermined,
            NodeRead::Yes,
        ]);
        assert!(matches!(coin.search(10_000), ReadEnd::Answer(())));
        assert_eq!(*coin.wait_untils.borrow(), vec![10_000, 10_000, 10_000]);
    }

    /// A search that starts after the locktime without an undetermined answer before it keeps its own handling:
    /// the coin answers "timed out" and the step takes it.
    #[test]
    fn a_first_spend_search_after_the_locktime_keeps_its_own_handling() {
        let coin = ClockFirstCoin::new(10_001, vec![]);
        assert!(matches!(coin.search(10_000), ReadEnd::Failed(ref e) if e == TIMED_OUT));
        assert_eq!(coin.node_reads.get(), 0);
    }

    /// CRD ch.52 R21, R34: an undetermined answer of the refund gate is never taken as "refundable"; the gate is
    /// read again and its later answer is taken, and an error without the marker keeps its own retry.
    #[test]
    fn the_refund_gate_waits_on_an_undetermined_answer() {
        use coins::CanRefundHtlc;

        let answers: Vec<Result<CanRefundHtlc, String>> =
            vec![Err(undetermined_error()), Ok(CanRefundHtlc::CanRefundNow)];
        let (end, attempts, _) = run_read(10_000, 10_000, 30, answers);
        assert!(matches!(end, ReadEnd::Answer(CanRefundHtlc::CanRefundNow)));
        assert_eq!(attempts, 2);

        let answers: Vec<Result<CanRefundHtlc, String>> = (0..3).map(|_| Err(undetermined_error())).collect();
        let (end, attempts, _) = run_read(10_000, 10_000, 1_850, answers);
        assert!(matches!(end, ReadEnd::BoundPassed(_)));
        assert_eq!(attempts, 3, "reads at 11850 and 13700 wait; the one at 15550 ends");

        let (end, _, _) = run_read::<CanRefundHtlc, String>(10_000, 10_000, 30, vec![Err(NODE_ERROR.to_string())]);
        assert!(matches!(end, ReadEnd::Failed(_)));
    }

    /// CRD ch.52 R18: a funding spend that became the taker payment and is found after undetermined searches and
    /// after the ⅔ deadline is not acted on, so the maker takes its refund path. Found without such a wait, or
    /// before the deadline, it is acted on as the search gave it.
    #[test]
    fn a_taker_payment_found_after_a_wait_and_after_the_deadline_is_too_late() {
        let deadline = 6_000;
        assert!(answered_too_late(true, 6_001, deadline));
        assert!(answered_too_late(true, 13_000, deadline));
        assert!(!answered_too_late(true, 6_000, deadline));
        assert!(!answered_too_late(true, 100, deadline));
        assert!(!answered_too_late(false, 6_001, deadline));
        assert!(!answered_too_late(false, 13_000, deadline));

        // The same through a scripted search: the wait of the search is what makes a late find too late.
        let found_late: Vec<Result<Option<u8>, String>> = vec![Err(undetermined_error()), Ok(Some(1))];
        let (end, _, waited) = run_read(10_000, 6_500, 500, found_late);
        assert!(matches!(end, ReadEnd::Answer(Some(1))));
        assert!(answered_too_late(waited, 7_500, deadline));

        let found_at_once: Vec<Result<Option<u8>, String>> = vec![Ok(Some(1))];
        let (end, _, waited) = run_read(10_000, 6_500, 500, found_at_once);
        assert!(matches!(end, ReadEnd::Answer(Some(1))));
        assert!(!answered_too_late(waited, 7_000, deadline));
    }

    /// CRD ch.52 R19, R30: a confirmation read keeps its own outcome when it was determined at once; after
    /// undetermined reads a confirmation is acted on only when it is read before the step's deadline.
    #[test]
    fn a_confirmation_read_after_a_wait_is_acted_on_only_before_its_deadline() {
        let ok = || ReadEnd::<(), String>::Answer(());
        assert_eq!(confirmation_end(ok(), false, 100, 1_000), ConfirmationEnd::Confirmed);
        assert_eq!(confirmation_end(ok(), false, 1_001, 1_000), ConfirmationEnd::Confirmed);
        assert_eq!(confirmation_end(ok(), true, 1_000, 1_000), ConfirmationEnd::Confirmed);
        assert_eq!(
            confirmation_end(ok(), true, 1_001, 1_000),
            ConfirmationEnd::NotConfirmed(CONFIRMED_AFTER_DEADLINE.to_string())
        );
        assert_eq!(
            confirmation_end(ReadEnd::Failed("timed out".to_string()), false, 1_001, 1_000),
            ConfirmationEnd::NotConfirmed("timed out".to_string())
        );
        assert_eq!(
            confirmation_end(ReadEnd::BoundPassed("x".to_string()), true, 9_999, 1_000),
            ConfirmationEnd::BoundPassed("x".to_string())
        );
    }

    /// CRD ch.52 R19, R30: a confirmation read that is undetermined just before the step's deadline is never
    /// ended by the coin's clock as "not confirmed", which would lead to a refund state. Every read after it
    /// reaches the nodes, and the wait ends only at its bound.
    #[test]
    fn an_undetermined_confirmation_read_near_the_deadline_is_not_ended_by_the_clock() {
        let coin = ClockFirstCoin::new(2_995, vec![NodeRead::Undetermined; 1_200]);
        let end = coin.confirmation(10_000, 3_000);
        assert!(matches!(end, ConfirmationEnd::BoundPassed(_)));
        assert!(coin.clock.get() >= 13_700);
        assert_eq!(
            coin.calls.get(),
            coin.node_reads.get(),
            "every call of the coin reached the nodes"
        );
    }

    /// CRD ch.52 R19, R30: the step's deadline is judged apart from the `wait_until` the coin is given. A
    /// confirmation read in time after a wait is taken; one read after the deadline is not, although the coin was
    /// given a later `wait_until`; "timed out" is taken as "not confirmed" only after a read of the nodes.
    #[test]
    fn a_confirmation_after_an_undetermined_read_is_judged_by_the_steps_deadline() {
        let coin = ClockFirstCoin::new(2_000, vec![NodeRead::Undetermined, NodeRead::Yes]);
        assert_eq!(coin.confirmation(10_000, 3_000), ConfirmationEnd::Confirmed);
        assert_eq!(*coin.wait_untils.borrow(), vec![3_000, 3_000]);

        let coin = ClockFirstCoin::new(2_995, vec![NodeRead::Undetermined, NodeRead::Yes]);
        assert_eq!(
            coin.confirmation(10_000, 3_000),
            ConfirmationEnd::NotConfirmed(CONFIRMED_AFTER_DEADLINE.to_string())
        );
        assert_eq!(*coin.wait_untils.borrow(), vec![3_000, 3_005]);

        let coin = ClockFirstCoin::new(2_995, vec![NodeRead::Undetermined, NodeRead::NotYet]);
        assert_eq!(
            coin.confirmation(10_000, 3_000),
            ConfirmationEnd::NotConfirmed(TIMED_OUT.to_string())
        );
        assert_eq!(
            coin.node_reads.get(),
            2,
            "the second read asked the nodes before it timed out"
        );

        // Without an undetermined read the coin's own answer is taken as it is.
        let coin = ClockFirstCoin::new(3_001, vec![]);
        assert_eq!(
            coin.confirmation(10_000, 3_000),
            ConfirmationEnd::NotConfirmed(TIMED_OUT.to_string())
        );
    }

    /// The `wait_until` of a coin read is the step's deadline until a read after an undetermined answer needs a
    /// later one.
    #[test]
    fn the_coin_is_given_the_steps_deadline_or_a_later_window() {
        assert_eq!(coin_wait_until(10_000, 0), 10_000);
        assert_eq!(coin_wait_until(10_000, 9_000), 10_000);
        assert_eq!(coin_wait_until(10_000, 10_015), 10_015);
    }

    /// Every abort of a wait that reached its bound uses the reason variant its state already has, names the
    /// coin and the fix, and carries no part of the error text of the read.
    #[test]
    fn the_abort_of_a_wait_at_its_bound_names_the_coin_and_the_fix_and_no_error_text() {
        let last_error = format!("{} at {NODE_ERROR}", undetermined_error());
        let all = [
            CappedRead::FundingSpendSearch,
            CappedRead::TakerPaymentConfirmation,
            CappedRead::MakerPaymentConfirmation,
            CappedRead::TakerPaymentSpendSearch,
            CappedRead::MakerRefundGate,
            CappedRead::TakerRefundGate,
            CappedRead::MakerRefundLook,
            CappedRead::TakerRefundLook,
            CappedRead::FundingRefundSearch,
            CappedRead::FundingRefundGate,
        ];
        for read in all {
            let reason = undetermined_cap_reason(read, "RIN", &last_error);
            let variant_is_the_states_own = match read {
                CappedRead::MakerRefundLook => matches!(reason, AbortReason::InternalError(_)),
                CappedRead::TakerRefundLook => matches!(reason, AbortReason::TakerPaymentRefundFailed(_)),
                CappedRead::FundingRefundSearch | CappedRead::FundingRefundGate => {
                    matches!(reason, AbortReason::TakerFundingRefundFailed(_))
                },
                CappedRead::FundingSpendSearch => matches!(reason, AbortReason::FundingSpendError(_)),
                CappedRead::TakerPaymentConfirmation => matches!(reason, AbortReason::InternalError(_)),
                CappedRead::MakerPaymentConfirmation => {
                    matches!(reason, AbortReason::MakerPaymentNotConfirmedInTime(_))
                },
                CappedRead::TakerPaymentSpendSearch => matches!(reason, AbortReason::MakerDidNotSpendInTime(_)),
                CappedRead::MakerRefundGate => matches!(reason, AbortReason::InternalError(_)),
                CappedRead::TakerRefundGate => matches!(reason, AbortReason::TakerPaymentRefundFailed(_)),
            };
            assert!(variant_is_the_states_own, "{read:?}");

            let shown = reason.to_string();
            let stored = serde_json::to_string(&reason).unwrap();
            for text in [&shown, &stored] {
                assert!(text.contains("RIN"), "{read:?}");
                assert!(text.contains("second host"), "{read:?}");
                assert!(text.contains("`swap_evidence_node`"), "{read:?}");
                assert!(text.contains("`recover_funds_of_swap`"), "{read:?}");
                assert!(text.contains("no two nodes agreed"), "{read:?}");
                assert!(!text.contains(PRIVATE_MARKER), "{read:?}");
                assert!(!text.contains("node.example"), "{read:?}");
                assert!(!text.contains("node-b"), "{read:?}");
                assert!(!text.contains("http"), "{read:?}");
                assert!(!text.contains(OWN_CLAIM_NOT_CONFIRMED_PHRASE.trim_end()), "{read:?}");
            }
        }
    }

    /// The warning of each wait names its own read and coin, the cause and the fix, and no part of the error
    /// text of the read.
    #[test]
    fn the_warning_of_each_wait_names_its_read_and_no_error_text() {
        use super::super::evidence_wait::wait_text;

        let reason = format!("{} at {NODE_ERROR}", undetermined_error());
        let expected = [
            (
                CappedRead::FundingSpendSearch,
                "the search for the spend of the taker funding on RIN could not be determined",
            ),
            (
                CappedRead::TakerPaymentConfirmation,
                "the confirmation of the taker payment on RIN could not be determined",
            ),
            (
                CappedRead::MakerPaymentConfirmation,
                "the confirmation of the maker payment on RIN could not be determined",
            ),
            (
                CappedRead::TakerPaymentSpendSearch,
                "the state of the taker payment on RIN could not be determined",
            ),
            (
                CappedRead::MakerRefundGate,
                "the chain time of the refund gate on RIN could not be determined",
            ),
            (
                CappedRead::TakerRefundGate,
                "the chain time of the refund gate on RIN could not be determined",
            ),
        ];
        for (read, opening) in expected {
            let warning = wait_text("RIN", read.logged_as(), &reason);
            assert!(warning.starts_with(opening), "{read:?}");
            assert!(warning.contains("no two nodes agreed"), "{read:?}");
            assert!(warning.contains("`swap_evidence_node`"), "{read:?}");
            assert!(!warning.contains(PRIVATE_MARKER), "{read:?}");
            assert!(!warning.contains("node.example"), "{read:?}");
            assert!(!warning.contains("node-b"), "{read:?}");
            assert!(!warning.contains("http"), "{read:?}");
        }
    }

    /// The abort text says which payment was left as it is, so a reader knows what to recover.
    #[test]
    fn the_abort_of_a_wait_at_its_bound_says_what_was_not_done() {
        let text = |read| undetermined_cap_reason(read, "RIN", &undetermined_error()).to_string();
        assert!(text(CappedRead::FundingSpendSearch).contains("the maker payment is not refunded"));
        assert!(text(CappedRead::TakerPaymentConfirmation).contains("the taker payment is not claimed"));
        assert!(text(CappedRead::TakerPaymentConfirmation).contains("the maker payment is on chain"));
        assert!(text(CappedRead::MakerPaymentConfirmation).contains("the taker funding is not spent"));
        assert!(text(CappedRead::TakerPaymentSpendSearch).contains("the taker payment is not refunded"));
        assert!(text(CappedRead::MakerRefundGate).contains("the maker payment is not refunded"));
        assert!(text(CappedRead::TakerRefundGate).contains("the taker payment is not refunded"));
    }

    /// CRD ch.52 R19, R44: the maker's `ConfirmationTimeout` aborts commit nothing. No abort of a wait that
    /// reached its bound carries that reason, because each of them is reached with funds on chain.
    #[test]
    fn no_abort_of_a_wait_at_its_bound_is_a_confirmation_timeout() {
        for read in [
            CappedRead::FundingSpendSearch,
            CappedRead::TakerPaymentConfirmation,
            CappedRead::MakerPaymentConfirmation,
            CappedRead::TakerPaymentSpendSearch,
            CappedRead::MakerRefundGate,
            CappedRead::TakerRefundGate,
        ] {
            let reason = undetermined_cap_reason(read, "RIN", &undetermined_error());
            assert!(
                !matches!(reason, AbortReason::ConfirmationTimeout(_)),
                "a wait that ends with funds on chain must not abort as a confirmation timeout"
            );
        }
    }

    #[test]
    fn should_format_message_when_displaying_abort_reason_all_variants() {
        let cases: Vec<(AbortReason, &str)> = vec![
            (AbortReason::NegotiationTimeout, "Negotiation timed out"),
            (AbortReason::NegotiationFailed("bad".into()), "bad"),
            (AbortReason::FailedToSendTx("tx err".into()), "tx err"),
            (AbortReason::FailedToValidateTx("invalid".into()), "invalid"),
            (AbortReason::ConfirmationTimeout("timeout".into()), "timeout"),
            (AbortReason::FundingSpendError("spend".into()), "spend"),
            (AbortReason::TakerAborted("taker".into()), "taker"),
            (AbortReason::MakerAborted("maker".into()), "maker"),
            (AbortReason::InternalError("internal".into()), "internal"),
            (AbortReason::FailedToSendPayment("pay".into()), "pay"),
            (AbortReason::DidNotReceiveMakerPayment("no pay".into()), "no pay"),
            (AbortReason::FailedToParseMakerPayment("parse".into()), "parse"),
            (AbortReason::FailedToParseFundingSpendPreimg("preimg".into()), "preimg"),
            (AbortReason::FailedToParseFundingSpendSig("sig".into()), "sig"),
            (AbortReason::MakerPaymentValidationFailed("fail".into()), "fail"),
            (AbortReason::FundingSpendPreimageValidationFailed("bad".into()), "bad"),
            (AbortReason::MakerPaymentNotConfirmedInTime("slow".into()), "slow"),
            (AbortReason::FailedToGenerateSpendPreimage("gen".into()), "gen"),
            (AbortReason::MakerDidNotSpendInTime("late".into()), "late"),
            (AbortReason::CouldNotExtractSecret("secret".into()), "secret"),
            (AbortReason::FailedToSpendMakerPayment("spend".into()), "spend"),
            (AbortReason::MakerPaymentSpendNotConfirmedInTime("conf".into()), "conf"),
            (AbortReason::TakerFundingRefundFailed("refund".into()), "refund"),
            (AbortReason::TakerPaymentRefundFailed("refund".into()), "refund"),
        ];
        for (reason, expected_substr) in cases {
            let display = format!("{}", reason);
            assert!(
                display.contains(expected_substr),
                "AbortReason display '{}' doesn't contain '{}'",
                display,
                expected_substr
            );
        }
    }

    #[test]
    fn should_roundtrip_when_encoding_each_swap_message_variant() {
        use prost::Message;

        let test_uuid = vec![1u8; 16];

        // MakerNegotiation
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiation(MakerNegotiation {
                started_at: 100,
                payment_locktime: 200,
                secret_hash: vec![1],
                maker_coin_htlc_pub: vec![2],
                taker_coin_htlc_pub: vec![3],
                maker_coin_swap_contract: Some(vec![4]),
                taker_coin_swap_contract: None,
                taker_coin_address: "addr".into(),
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::MakerNegotiation(_))));

        // TakerNegotiation::Continue
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerNegotiation(TakerNegotiation {
                action: Some(taker_negotiation::Action::Continue(TakerNegotiationData {
                    started_at: 100,
                    funding_locktime: 300,
                    payment_locktime: 200,
                    taker_secret_hash: vec![5],
                    maker_coin_htlc_pub: vec![6],
                    taker_coin_htlc_pub: vec![7],
                    maker_coin_swap_contract: None,
                    taker_coin_swap_contract: Some(vec![8]),
                })),
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::TakerNegotiation(_))));

        // MakerNegotiated (true)
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiated(MakerNegotiated {
                negotiated: true,
                reason: None,
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::MakerNegotiated(_))));

        // MakerNegotiated (false + reason)
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiated(MakerNegotiated {
                negotiated: false,
                reason: Some("bad terms".into()),
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        match decoded.inner {
            Some(swap_message::Inner::MakerNegotiated(n)) => {
                assert!(!n.negotiated);
                assert_eq!(n.reason.unwrap(), "bad terms");
            },
            _ => panic!("Wrong variant"),
        }

        // TakerFundingInfo
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerFundingInfo(TakerFundingInfo {
                tx_bytes: vec![0xAA],
                next_step_instructions: Some(vec![1, 2, 3]),
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::TakerFundingInfo(_))));

        // MakerPaymentInfo
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerPaymentInfo(MakerPaymentInfo {
                tx_bytes: vec![0xBB],
                next_step_instructions: None,
                funding_preimage_sig: vec![0xCC],
                funding_preimage_tx: vec![0xDD],
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::MakerPaymentInfo(_))));

        // TakerPaymentInfo
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerPaymentInfo(TakerPaymentInfo {
                tx_bytes: vec![0xEE],
                next_step_instructions: None,
            })),
            swap_uuid: test_uuid.clone(),
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(decoded.inner, Some(swap_message::Inner::TakerPaymentInfo(_))));

        // TakerPaymentSpendPreimage
        let msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerPaymentSpendPreimage(
                TakerPaymentSpendPreimage {
                    signature: vec![0xFF],
                    tx_preimage: vec![0x11],
                },
            )),
            swap_uuid: test_uuid,
        };
        let decoded = SwapMessage::decode(msg.encode_to_vec().as_slice()).unwrap();
        assert!(matches!(
            decoded.inner,
            Some(swap_message::Inner::TakerPaymentSpendPreimage(_))
        ));
    }

    #[test]
    fn should_track_messages_when_populating_swap_v2_msg_store() {
        use super::super::SwapV2MsgStore;

        // secp256k1 requires a valid public key; use an uncompressed generator point
        let pubkey = secp256k1::PublicKey::from_slice(&[
            2, 0xc6, 0x04, 0x7f, 0x94, 0x41, 0xed, 0x7d, 0x6d, 0x30, 0x45, 0x40, 0x6e, 0x95, 0xc0, 0x7c, 0xd8, 0x5c,
            0x77, 0x8e, 0x4b, 0x8c, 0xef, 0x3c, 0xa7, 0xab, 0xac, 0x09, 0xb9, 0x5c, 0x70, 0x9e, 0xe5,
        ])
        .unwrap();

        let mut store = SwapV2MsgStore::new(pubkey);

        // All fields start as None
        assert!(store.maker_negotiation.is_none());
        assert!(store.taker_negotiation.is_none());
        assert!(store.maker_negotiated.is_none());
        assert!(store.taker_funding.is_none());
        assert!(store.maker_payment.is_none());
        assert!(store.taker_payment.is_none());
        assert!(store.taker_payment_spend_preimage.is_none());

        // Populate all fields with protobuf types
        store.maker_negotiation = Some(MakerNegotiation {
            started_at: 1,
            payment_locktime: 2,
            secret_hash: vec![],
            maker_coin_htlc_pub: vec![],
            taker_coin_htlc_pub: vec![],
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            taker_coin_address: "addr".into(),
        });
        store.taker_negotiation = Some(TakerNegotiation {
            action: Some(taker_negotiation::Action::Abort(Abort {
                reason: "cancel".into(),
            })),
        });
        store.maker_negotiated = Some(MakerNegotiated {
            negotiated: true,
            reason: None,
        });
        store.taker_funding = Some(TakerFundingInfo {
            tx_bytes: vec![1],
            next_step_instructions: None,
        });
        store.maker_payment = Some(MakerPaymentInfo {
            tx_bytes: vec![2],
            next_step_instructions: None,
            funding_preimage_sig: vec![3],
            funding_preimage_tx: vec![4],
        });
        store.taker_payment = Some(TakerPaymentInfo {
            tx_bytes: vec![5],
            next_step_instructions: None,
        });
        store.taker_payment_spend_preimage = Some(TakerPaymentSpendPreimage {
            signature: vec![6],
            tx_preimage: vec![7],
        });

        // All fields now populated
        assert!(store.maker_negotiation.is_some());
        assert!(store.taker_negotiation.is_some());
        assert!(store.maker_negotiated.is_some());
        assert!(store.taker_funding.is_some());
        assert!(store.maker_payment.is_some());
        assert!(store.taker_payment.is_some());
        assert!(store.taker_payment_spend_preimage.is_some());

        // Verify specific stored data
        assert_eq!(store.maker_negotiation.as_ref().unwrap().started_at, 1);
        match &store.taker_negotiation.as_ref().unwrap().action {
            Some(taker_negotiation::Action::Abort(abort)) => assert_eq!(abort.reason, "cancel"),
            _ => panic!("Expected Abort"),
        }
    }

    #[test]
    fn should_roundtrip_when_serializing_each_maker_swap_event() {
        use super::super::maker_swap_v2::MakerSwapEvent;
        use common::mm_number::MmNumber;

        let negotiation_data = StoredMakerNegotiationData {
            taker_secret_hash: BytesJson::from(vec![1, 2]),
            taker_coin_htlc_pub: BytesJson::from(vec![3, 4]),
            maker_coin_htlc_pub: BytesJson::from(vec![5, 6]),
            taker_coin_swap_contract: None,
            maker_coin_swap_contract: None,
            taker_payment_locktime: 1000,
            taker_funding_locktime: 2000,
        };

        let events = vec![
            MakerSwapEvent::Initialized {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                maker_payment_trade_fee: MmNumber::from("0.001"),
                taker_payment_spend_trade_fee: MmNumber::from("0.002"),
                taker_payment_spend_headroom: MmNumber::from("0.002"),
                maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
            },
            MakerSwapEvent::WaitingForTakerFunding {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                maker_payment_trade_fee: MmNumber::from("0.001"),
            },
            MakerSwapEvent::TakerFundingReceived {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_funding: BytesJson::from(vec![0xAA]),
                maker_payment_trade_fee: MmNumber::from("0.001"),
            },
            MakerSwapEvent::MakerPaymentSentFundingSpendGenerated {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                maker_payment: BytesJson::from(vec![0xBB]),
                taker_funding: BytesJson::from(vec![0xAA]),
                funding_spend_preimage: StoredTxPreimage {
                    preimage: BytesJson::from(vec![0xCC]),
                    signature: BytesJson::from(vec![0xDD]),
                },
            },
            MakerSwapEvent::MakerPaymentRefundRequired {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                maker_payment: BytesJson::from(vec![0xBB]),
                reason: AbortReason::NegotiationTimeout,
            },
            MakerSwapEvent::MakerPaymentRefunded {
                maker_payment: BytesJson::from(vec![0xBB]),
                maker_payment_refund: BytesJson::from(vec![0xEE]),
                reason: AbortReason::NegotiationTimeout,
            },
            MakerSwapEvent::TakerPaymentReceived {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                maker_payment: BytesJson::from(vec![0xBB]),
                taker_payment: BytesJson::from(vec![0xFF]),
            },
            MakerSwapEvent::TakerPaymentReceivedPreimageSkipped {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                maker_payment: BytesJson::from(vec![0xBB]),
                taker_payment: BytesJson::from(vec![0xFF]),
            },
            MakerSwapEvent::TakerPaymentSpent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                maker_payment: BytesJson::from(vec![0xBB]),
                taker_payment: BytesJson::from(vec![0xFF]),
                taker_payment_spend: BytesJson::from(vec![0x11]),
                negotiation_data: negotiation_data.clone(),
            },
            MakerSwapEvent::Aborted {
                reason: AbortReason::InternalError("oops".into()),
            },
            MakerSwapEvent::Completed,
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let back: MakerSwapEvent = serde_json::from_str(&json).unwrap();
            // Verify roundtrip by re-serializing
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2, "MakerSwapEvent serde roundtrip mismatch");
        }
    }

    #[test]
    fn should_roundtrip_when_serializing_each_taker_swap_event() {
        use super::super::taker_swap_v2::TakerSwapEvent;
        use common::mm_number::MmNumber;

        let negotiation_data = StoredTakerNegotiationData {
            maker_secret_hash: BytesJson::from(vec![1, 2]),
            maker_coin_htlc_pub: BytesJson::from(vec![3, 4]),
            taker_coin_htlc_pub: BytesJson::from(vec![5, 6]),
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            maker_payment_locktime: 3000,
            taker_coin_address: "Raddress".into(),
        };

        let events = vec![
            TakerSwapEvent::Initialized {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                taker_payment_fee: MmNumber::from("0.001"),
                maker_payment_spend_fee: MmNumber::from("0.002"),
                maker_payment_spend_headroom: MmNumber::from("0.002"),
                taker_payment_fee_reservable: Some(MmNumber::from("0.001")),
            },
            TakerSwapEvent::Negotiated {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_payment_fee: MmNumber::from("0.001"),
                maker_payment_spend_fee: MmNumber::from("0.002"),
            },
            TakerSwapEvent::TakerFundingSent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_funding: BytesJson::from(vec![0xAA]),
            },
            TakerSwapEvent::TakerFundingRefundRequired {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_funding: BytesJson::from(vec![0xAA]),
                reason: AbortReason::NegotiationTimeout,
            },
            TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_funding: BytesJson::from(vec![0xAA]),
                funding_spend_preimage: StoredTxPreimage {
                    preimage: BytesJson::from(vec![0xCC]),
                    signature: BytesJson::from(vec![0xDD]),
                },
                maker_payment: BytesJson::from(vec![0xBB]),
            },
            TakerSwapEvent::MakerPaymentConfirmed {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_funding: BytesJson::from(vec![0xAA]),
                funding_spend_preimage: StoredTxPreimage {
                    preimage: BytesJson::from(vec![0xCC]),
                    signature: BytesJson::from(vec![0xDD]),
                },
                maker_payment: BytesJson::from(vec![0xBB]),
            },
            TakerSwapEvent::TakerPaymentSent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_payment: BytesJson::from(vec![0xEE]),
                maker_payment: BytesJson::from(vec![0xBB]),
            },
            TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                negotiation_data: negotiation_data.clone(),
                taker_payment: BytesJson::from(vec![0xEE]),
                maker_payment: BytesJson::from(vec![0xBB]),
            },
            TakerSwapEvent::TakerPaymentRefundRequired {
                taker_payment: BytesJson::from(vec![0xEE]),
                negotiation_data: negotiation_data.clone(),
                reason: AbortReason::ConfirmationTimeout("too slow".into()),
            },
            TakerSwapEvent::TakerPaymentSpent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                taker_payment_spend: BytesJson::from(vec![0x11]),
                maker_payment: BytesJson::from(vec![0xBB]),
                negotiation_data: negotiation_data.clone(),
                taker_payment: BytesJson::from(vec![0xEE]),
            },
            TakerSwapEvent::MakerPaymentSpent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                maker_payment_spend: BytesJson::from(vec![0x22]),
                negotiation_data: negotiation_data.clone(),
                taker_payment: BytesJson::from(vec![0xEE]),
            },
            TakerSwapEvent::TakerFundingRefunded {
                funding_tx: BytesJson::from(vec![0xAA]),
                funding_tx_refund: BytesJson::from(vec![0x33]),
                reason: AbortReason::TakerFundingRefundFailed("failed".into()),
            },
            TakerSwapEvent::TakerPaymentRefunded {
                taker_payment: BytesJson::from(vec![0xEE]),
                taker_payment_refund: BytesJson::from(vec![0x44]),
                reason: AbortReason::TakerPaymentRefundFailed("failed".into()),
            },
            TakerSwapEvent::Aborted {
                reason: AbortReason::InternalError("oops".into()),
            },
            TakerSwapEvent::Completed,
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let back: TakerSwapEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2, "TakerSwapEvent serde roundtrip mismatch");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_roundtrip_when_serializing_maker_swap_db_repr() {
        use super::super::maker_swap_v2::{MakerSwapDbRepr, MakerSwapEvent, SerializableKeypairBytes};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use rpc::v1::types::H256 as H256Json;

        let repr = MakerSwapDbRepr {
            maker_coin: "DOC".into(),
            maker_volume: MmNumber::from("10.5"),
            maker_secret: H256Json::from([0xABu8; 32]),
            maker_secret_hash: BytesJson::from(vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
            started_at: 1700000000,
            lock_duration: 7200,
            taker_coin: "MARTY".into(),
            taker_volume: MmNumber::from("20.5"),
            taker_premium: MmNumber::from("0.1"),
            dex_fee_amount: MmNumber::from("0.01"),
            dex_fee_burn: MmNumber::from("0.005"),
            conf_settings: SwapConfirmationsSettings {
                maker_coin_confs: 1,
                maker_coin_nota: false,
                taker_coin_confs: 2,
                taker_coin_nota: true,
            },
            uuid: Uuid::new_v4(),
            p2p_keypair: Some(SerializableKeypairBytes(vec![0xDE; 32])),
            events: vec![
                MakerSwapEvent::Initialized {
                    maker_coin_start_block: 100,
                    taker_coin_start_block: 200,
                    maker_payment_trade_fee: MmNumber::from("0.001"),
                    taker_payment_spend_trade_fee: MmNumber::from("0.002"),
                    taker_payment_spend_headroom: MmNumber::from("0.002"),
                    maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
                },
                MakerSwapEvent::Completed,
            ],
            taker_p2p_pub: BytesJson::from(vec![0x02; 33]),
            swap_version: 2,
        };

        let json = serde_json::to_string(&repr).unwrap();
        let back: MakerSwapDbRepr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.maker_coin, "DOC");
        assert_eq!(back.taker_coin, "MARTY");
        assert_eq!(back.uuid, repr.uuid);
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.swap_version, 2);
        assert!(back.conf_settings.taker_coin_nota);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_roundtrip_when_serializing_taker_swap_db_repr() {
        use super::super::taker_swap_v2::{TakerSwapDbRepr, TakerSwapEvent};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use rpc::v1::types::H256 as H256Json;

        let repr = TakerSwapDbRepr {
            maker_coin: "DOC".into(),
            maker_volume: MmNumber::from("10.5"),
            taker_secret: H256Json::from([0xCDu8; 32]),
            taker_secret_hash: BytesJson::from(vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::SHA256,
            started_at: 1700000000,
            lock_duration: 7200,
            taker_coin: "MARTY".into(),
            taker_volume: MmNumber::from("20.5"),
            taker_premium: MmNumber::from("0.1"),
            dex_fee_amount: MmNumber::from("0.01"),
            dex_fee_burn: MmNumber::from("0.005"),
            conf_settings: SwapConfirmationsSettings {
                maker_coin_confs: 3,
                maker_coin_nota: true,
                taker_coin_confs: 1,
                taker_coin_nota: false,
            },
            uuid: Uuid::new_v4(),
            p2p_keypair: None,
            events: vec![
                TakerSwapEvent::Initialized {
                    maker_coin_start_block: 100,
                    taker_coin_start_block: 200,
                    taker_payment_fee: MmNumber::from("0.001"),
                    maker_payment_spend_fee: MmNumber::from("0.002"),
                    maker_payment_spend_headroom: MmNumber::from("0.002"),
                    taker_payment_fee_reservable: Some(MmNumber::from("0.001")),
                },
                TakerSwapEvent::Completed,
            ],
            maker_p2p_pub: BytesJson::from(vec![0x03; 33]),
            swap_version: 2,
        };

        let json = serde_json::to_string(&repr).unwrap();
        let back: TakerSwapDbRepr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.maker_coin, "DOC");
        assert_eq!(back.taker_coin, "MARTY");
        assert_eq!(back.uuid, repr.uuid);
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.swap_version, 2);
        assert!(back.conf_settings.maker_coin_nota);
    }

    #[test]
    fn should_use_stable_discriminants_when_encoding_swap_v2_type() {
        assert_eq!(SwapV2Type::MakerV2 as u8, 1);
        assert_eq!(SwapV2Type::TakerV2 as u8, 2);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_carry_fields_when_constructing_active_swap_info() {
        let uuid = Uuid::new_v4();
        let info = ActiveSwapV2Info {
            uuid,
            maker_coin: "DOC".into(),
            taker_coin: "MARTY".into(),
            swap_type: SwapV2Type::MakerV2,
        };
        assert_eq!(info.uuid, uuid);
        assert_eq!(info.maker_coin, "DOC");
        assert_eq!(info.taker_coin, "MARTY");
        assert_eq!(info.swap_type, SwapV2Type::MakerV2);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_append_when_calling_add_event_on_db_repr() {
        use super::super::maker_swap_v2::{MakerSwapDbRepr, MakerSwapEvent};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use mm2_state_machine::storable_state_machine::StateMachineDbRepr;
        use rpc::v1::types::H256 as H256Json;

        let mut repr = MakerSwapDbRepr {
            maker_coin: "DOC".into(),
            maker_volume: MmNumber::from("1"),
            maker_secret: H256Json::from([0u8; 32]),
            maker_secret_hash: BytesJson::from(vec![0u8; 20]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
            started_at: 0,
            lock_duration: 3600,
            taker_coin: "MARTY".into(),
            taker_volume: MmNumber::from("1"),
            taker_premium: MmNumber::from("0"),
            dex_fee_amount: MmNumber::from("0"),
            dex_fee_burn: MmNumber::from("0"),
            conf_settings: SwapConfirmationsSettings {
                maker_coin_confs: 1,
                maker_coin_nota: false,
                taker_coin_confs: 1,
                taker_coin_nota: false,
            },
            uuid: Uuid::new_v4(),
            p2p_keypair: None,
            events: vec![],
            taker_p2p_pub: BytesJson::from(vec![0u8; 33]),
            swap_version: 2,
        };

        assert_eq!(repr.events.len(), 0);

        repr.add_event(MakerSwapEvent::Initialized {
            maker_coin_start_block: 1,
            taker_coin_start_block: 2,
            maker_payment_trade_fee: MmNumber::from("0.001"),
            taker_payment_spend_trade_fee: MmNumber::from("0.002"),
            taker_payment_spend_headroom: MmNumber::from("0.002"),
            maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
        });
        assert_eq!(repr.events.len(), 1);

        repr.add_event(MakerSwapEvent::Completed);
        assert_eq!(repr.events.len(), 2);
    }

    // Native-only DB integration tests --------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    mod db_tests {
        use super::super::super::maker_swap_v2::{MakerSwapDbRepr, MakerSwapEvent, SerializableKeypairBytes};
        use super::super::super::taker_swap_v2::{TakerSwapDbRepr, TakerSwapEvent};
        use super::super::super::{SwapConfirmationsSettings, MAKER_SWAP_V2_TYPE, TAKER_SWAP_V2_TYPE};
        use super::super::*;
        use common::block_on;
        use common::mm_number::MmNumber;
        use db_common::sqlite::rusqlite::Connection;
        use mm2_core::mm_ctx::MmCtxBuilder;
        use mm2_state_machine::storable_state_machine::StateMachineStorage;
        use rpc::v1::types::H256 as H256Json;
        use std::sync::{Arc, Mutex};
        use uuid::Uuid;

        /// Create an MmArc with an in-memory SQLite DB, fully migrated.
        fn setup_test_ctx() -> mm2_core::mm_ctx::MmArc {
            let ctx = MmCtxBuilder::default().into_mm_arc();
            let conn = Connection::open_in_memory().unwrap();
            let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(conn)));
            // Run init + migrations to create schema with V2 columns
            block_on(crate::mm2::database::init_and_migrate_db(&ctx)).unwrap();
            ctx
        }

        fn sample_maker_repr(uuid: Uuid) -> MakerSwapDbRepr {
            MakerSwapDbRepr {
                maker_coin: "DOC".into(),
                maker_volume: MmNumber::from("10"),
                maker_secret: H256Json::from([0xABu8; 32]),
                maker_secret_hash: BytesJson::from(vec![1u8; 20]),
                secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
                started_at: 1700000000,
                lock_duration: 7200,
                taker_coin: "MARTY".into(),
                taker_volume: MmNumber::from("20"),
                taker_premium: MmNumber::from("0.1"),
                dex_fee_amount: MmNumber::from("0.01"),
                dex_fee_burn: MmNumber::from("0.005"),
                conf_settings: SwapConfirmationsSettings {
                    maker_coin_confs: 1,
                    maker_coin_nota: false,
                    taker_coin_confs: 2,
                    taker_coin_nota: true,
                },
                uuid,
                p2p_keypair: Some(SerializableKeypairBytes(vec![0xDE; 32])),
                events: vec![],
                taker_p2p_pub: BytesJson::from(vec![0x02; 33]),
                swap_version: 2,
            }
        }

        fn sample_taker_repr(uuid: Uuid) -> TakerSwapDbRepr {
            TakerSwapDbRepr {
                maker_coin: "DOC".into(),
                maker_volume: MmNumber::from("10"),
                taker_secret: H256Json::from([0xCDu8; 32]),
                taker_secret_hash: BytesJson::from(vec![2u8; 20]),
                secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::SHA256,
                started_at: 1700000000,
                lock_duration: 7200,
                taker_coin: "MARTY".into(),
                taker_volume: MmNumber::from("20"),
                taker_premium: MmNumber::from("0.1"),
                dex_fee_amount: MmNumber::from("0.01"),
                dex_fee_burn: MmNumber::from("0.005"),
                conf_settings: SwapConfirmationsSettings {
                    maker_coin_confs: 3,
                    maker_coin_nota: true,
                    taker_coin_confs: 1,
                    taker_coin_nota: false,
                },
                uuid,
                p2p_keypair: None,
                events: vec![],
                maker_p2p_pub: BytesJson::from(vec![0x03; 33]),
                swap_version: 2,
            }
        }

        /// CRD ch.21 R-D1, R21.B3: while TRON swaps are switched off, an unfinished saved swap whose maker or taker
        /// coin is a TRON coin (by the coins-config protocol) is not resumed at startup, in the legacy swap files
        /// and in both v2 storages. Its record stays as it was, unfinished, and its coins are not asked for. An
        /// unrelated unfinished swap is still kick-started.
        #[test]
        fn saved_swaps_with_a_tron_coin_are_not_resumed_at_startup() {
            use crate::mm2::lp_swap::maker_swap::{MakerSavedSwap, MakerSwapEvent as LegacyMakerEvent};
            use crate::mm2::lp_swap::saved_swap::{SavedSwap, SavedSwapIo};
            use crate::mm2::lp_swap::{my_swap_file_path, my_swaps_dir, swap_kick_starts, tron_swap_skip_message};
            use std::collections::HashSet;

            const { assert!(!coins::TRON_SWAPS_AVAILABLE) };
            let dbdir = std::env::temp_dir().join(format!("kdf-tron-saved-swaps-{}", Uuid::new_v4()));
            let ctx = MmCtxBuilder::default()
                .with_conf(serde_json::json!({
                    "dbdir": dbdir.to_str().unwrap(),
                    "coins": [
                        { "coin": "DOC", "asset": "DOC", "mm2": 1, "protocol": { "type": "UTXO" } },
                        { "coin": "MARTY", "asset": "MARTY", "mm2": 1, "protocol": { "type": "UTXO" } },
                        { "coin": "TRX", "mm2": 1, "wallet_only": false,
                          "protocol": { "type": "TRX", "protocol_data": { "network": "Mainnet" } } },
                        { "coin": "USDT-TRC20", "mm2": 1, "decimals": 6, "protocol": { "type": "TRC20",
                          "protocol_data": { "platform": "TRX",
                                             "contract_address": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t" } } },
                    ],
                }))
                .into_mm_arc();
            let conn = Connection::open_in_memory().unwrap();
            let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(conn)));
            block_on(crate::mm2::database::init_and_migrate_db(&ctx)).unwrap();
            std::fs::create_dir_all(my_swaps_dir(&ctx)).unwrap();

            // An unfinished legacy maker swap: the saved log holds the `Started` event only.
            let legacy = |maker_coin: &str, taker_coin: &str| {
                let mut swap = MakerSavedSwap::new(&MmNumber::from("1"), &MmNumber::from("1"));
                swap.uuid = Uuid::new_v4();
                swap.events.truncate(1);
                match &mut swap.events[0].event {
                    LegacyMakerEvent::Started(data) => {
                        data.maker_coin = maker_coin.to_owned();
                        data.taker_coin = taker_coin.to_owned();
                        data.uuid = swap.uuid;
                    },
                    _ => panic!("the first event of the sample is Started"),
                }
                let saved = SavedSwap::from(swap);
                assert!(!saved.is_finished());
                block_on(saved.save_to_db(&ctx)).unwrap();
                *saved.uuid()
            };
            let legacy_tron = legacy("DOC", "TRX");
            let legacy_plain = legacy("DOC", "MARTY");
            let legacy_tron_file = std::fs::read(my_swap_file_path(&ctx, &legacy_tron)).unwrap();

            let mut maker_storage = MakerSwapStorage::new(ctx.clone());
            let (v2_maker_tron, v2_maker_plain) = (Uuid::new_v4(), Uuid::new_v4());
            let mut repr = sample_maker_repr(v2_maker_tron);
            repr.maker_coin = "USDT-TRC20".into();
            block_on(maker_storage.store_repr(v2_maker_tron, repr)).unwrap();
            block_on(maker_storage.store_repr(v2_maker_plain, sample_maker_repr(v2_maker_plain))).unwrap();

            let mut taker_storage = TakerSwapStorage::new(ctx.clone());
            let v2_taker_tron = Uuid::new_v4();
            let mut repr = sample_taker_repr(v2_taker_tron);
            repr.taker_coin = "TRX".into();
            block_on(taker_storage.store_repr(v2_taker_tron, repr)).unwrap();

            let needed = block_on(swap_kick_starts(ctx.clone())).unwrap();

            // The coins to enable come from the swaps that are resumed alone: no TRON ticker is among them.
            assert_eq!(
                needed,
                ["DOC", "MARTY"].iter().map(|c| c.to_string()).collect::<HashSet<_>>()
            );

            // The skipped records are untouched and still unfinished.
            assert_eq!(
                std::fs::read(my_swap_file_path(&ctx, &legacy_tron)).unwrap(),
                legacy_tron_file
            );
            let unfinished: HashSet<Uuid> = block_on(maker_storage.get_unfinished()).unwrap().into_iter().collect();
            assert!(unfinished.contains(&v2_maker_tron) && unfinished.contains(&v2_maker_plain));
            assert_eq!(
                block_on(maker_storage.get_repr(v2_maker_tron)).unwrap().maker_coin,
                "USDT-TRC20"
            );
            assert_eq!(block_on(taker_storage.get_unfinished()).unwrap(), vec![v2_taker_tron]);

            // The log line names the swap and the reason; a swap without a TRON coin gets none.
            assert_eq!(
                tron_swap_skip_message(&ctx, &legacy_tron, "DOC", "TRX").unwrap(),
                format!(
                    "Not resuming saved swap {legacy_tron} (DOC/TRX): TRON swaps are not available in this build; \
                     the saved swap is kept"
                )
            );
            assert!(tron_swap_skip_message(&ctx, &v2_maker_tron, "USDT-TRC20", "MARTY").is_some());
            assert!(tron_swap_skip_message(&ctx, &legacy_plain, "DOC", "MARTY").is_none());
        }

        /// CRD ch.18 and ch.36 beta notes: a swap that has already started still completes or is recovered. Only
        /// TRON's saved swaps are skipped at startup while their swaps are off; a saved swap that names a Tendermint
        /// coin is resumed although `coins::TENDERMINT_SWAPS_AVAILABLE` is `false`, so `tron_swap_skip_message`
        /// gives no skip line for a Tendermint pair or a Tendermint/UTXO pair.
        #[test]
        fn a_saved_swap_with_a_tendermint_coin_is_resumed_while_tendermint_swaps_are_off() {
            use crate::mm2::lp_swap::tron_swap_skip_message;

            const { assert!(!coins::TENDERMINT_SWAPS_AVAILABLE) };
            let ctx = MmCtxBuilder::default()
                .with_conf(serde_json::json!({
                    "coins": [
                        { "coin": "DOC", "asset": "DOC", "mm2": 1, "protocol": { "type": "UTXO" } },
                        { "coin": "IRIS", "mm2": 1, "protocol": { "type": "TENDERMINT", "protocol_data": {
                            "decimals": 6, "denom": "uiris", "account_prefix": "iaa", "chain_id": "irishub-1" } } },
                        { "coin": "IRIS-NYAN", "mm2": 1, "protocol": { "type": "TENDERMINTTOKEN",
                            "protocol_data": { "platform": "IRIS", "decimals": 6, "denom": "nyan" } } },
                    ],
                }))
                .into_mm_arc();
            let uuid = Uuid::new_v4();
            assert!(tron_swap_skip_message(&ctx, &uuid, "IRIS", "IRIS-NYAN").is_none());
            assert!(tron_swap_skip_message(&ctx, &uuid, "IRIS-NYAN", "IRIS").is_none());
            assert!(tron_swap_skip_message(&ctx, &uuid, "IRIS", "DOC").is_none());
            assert!(tron_swap_skip_message(&ctx, &uuid, "DOC", "IRIS-NYAN").is_none());
        }

        #[test]
        fn should_handle_full_lifecycle_when_using_maker_swap_storage() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let repr = sample_maker_repr(uuid);

            let mut storage = MakerSwapStorage::new(ctx.clone());

            // Initially no record
            assert!(!block_on(storage.has_record_for(&uuid)).unwrap());

            // Store repr
            block_on(storage.store_repr(uuid, repr.clone())).unwrap();

            // Now has record
            assert!(block_on(storage.has_record_for(&uuid)).unwrap());

            // get_repr roundtrip — verify core fields
            let loaded: MakerSwapDbRepr = block_on(storage.get_repr(uuid)).unwrap();
            assert_eq!(loaded.maker_coin, "DOC");
            assert_eq!(loaded.taker_coin, "MARTY");
            assert_eq!(loaded.started_at, 1700000000);
            assert_eq!(loaded.lock_duration, 7200);
            assert_eq!(loaded.swap_version, 2);
            assert_eq!(loaded.events.len(), 0);

            // Append events
            let event1 = MakerSwapEvent::Initialized {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                maker_payment_trade_fee: MmNumber::from("0.001"),
                taker_payment_spend_trade_fee: MmNumber::from("0.002"),
                taker_payment_spend_headroom: MmNumber::from("0.002"),
                maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
            };
            block_on(storage.store_event(uuid, event1)).unwrap();

            let event2 = MakerSwapEvent::Completed;
            block_on(storage.store_event(uuid, event2)).unwrap();

            // Read events back
            let events: Vec<MakerSwapEvent> = read_swap_v2_events(&ctx, &uuid).unwrap();
            assert_eq!(events.len(), 2);
            // Verify first event
            match &events[0] {
                MakerSwapEvent::Initialized {
                    maker_coin_start_block,
                    taker_coin_start_block,
                    ..
                } => {
                    assert_eq!(*maker_coin_start_block, 100);
                    assert_eq!(*taker_coin_start_block, 200);
                },
                _ => panic!("Expected Initialized event"),
            }
            match &events[1] {
                MakerSwapEvent::Completed => {},
                _ => panic!("Expected Completed event"),
            }

            // Swap type
            assert_eq!(get_swap_type(&ctx, &uuid).unwrap(), MAKER_SWAP_V2_TYPE);

            // Unfinished list
            let unfinished = block_on(storage.get_unfinished()).unwrap();
            assert!(unfinished.contains(&uuid));

            // Mark finished
            block_on(storage.mark_finished(uuid)).unwrap();

            // No longer in unfinished
            let unfinished = block_on(storage.get_unfinished()).unwrap();
            assert!(!unfinished.contains(&uuid));
        }

        #[test]
        fn should_handle_full_lifecycle_when_using_taker_swap_storage() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let repr = sample_taker_repr(uuid);

            let mut storage = TakerSwapStorage::new(ctx.clone());

            assert!(!block_on(storage.has_record_for(&uuid)).unwrap());

            block_on(storage.store_repr(uuid, repr.clone())).unwrap();
            assert!(block_on(storage.has_record_for(&uuid)).unwrap());

            let loaded: TakerSwapDbRepr = block_on(storage.get_repr(uuid)).unwrap();
            assert_eq!(loaded.maker_coin, "DOC");
            assert_eq!(loaded.taker_coin, "MARTY");
            assert_eq!(loaded.started_at, 1700000000);
            assert_eq!(loaded.swap_version, 2);

            // Append events
            let event1 = TakerSwapEvent::Initialized {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                taker_payment_fee: MmNumber::from("0.001"),
                maker_payment_spend_fee: MmNumber::from("0.002"),
                maker_payment_spend_headroom: MmNumber::from("0.002"),
                taker_payment_fee_reservable: Some(MmNumber::from("0.001")),
            };
            block_on(storage.store_event(uuid, event1)).unwrap();

            let event2 = TakerSwapEvent::Completed;
            block_on(storage.store_event(uuid, event2)).unwrap();

            let events: Vec<TakerSwapEvent> = read_swap_v2_events(&ctx, &uuid).unwrap();
            assert_eq!(events.len(), 2);

            assert_eq!(get_swap_type(&ctx, &uuid).unwrap(), TAKER_SWAP_V2_TYPE);

            let unfinished = block_on(storage.get_unfinished()).unwrap();
            assert!(unfinished.contains(&uuid));

            block_on(storage.mark_finished(uuid)).unwrap();
            let unfinished = block_on(storage.get_unfinished()).unwrap();
            assert!(!unfinished.contains(&uuid));
        }

        #[test]
        fn should_track_per_role_when_querying_unfinished_swaps() {
            let ctx = setup_test_ctx();
            let mut maker_storage = MakerSwapStorage::new(ctx.clone());
            let mut taker_storage = TakerSwapStorage::new(ctx.clone());

            let maker_uuid1 = Uuid::new_v4();
            let maker_uuid2 = Uuid::new_v4();
            let taker_uuid1 = Uuid::new_v4();

            block_on(maker_storage.store_repr(maker_uuid1, sample_maker_repr(maker_uuid1))).unwrap();
            block_on(maker_storage.store_repr(maker_uuid2, sample_maker_repr(maker_uuid2))).unwrap();
            block_on(taker_storage.store_repr(taker_uuid1, sample_taker_repr(taker_uuid1))).unwrap();

            // All maker swaps unfinished
            let maker_unfinished = block_on(maker_storage.get_unfinished()).unwrap();
            assert_eq!(maker_unfinished.len(), 2);
            assert!(maker_unfinished.contains(&maker_uuid1));
            assert!(maker_unfinished.contains(&maker_uuid2));

            // Taker unfinished
            let taker_unfinished = block_on(taker_storage.get_unfinished()).unwrap();
            assert_eq!(taker_unfinished.len(), 1);
            assert!(taker_unfinished.contains(&taker_uuid1));

            // Finish one maker swap
            block_on(maker_storage.mark_finished(maker_uuid1)).unwrap();
            let maker_unfinished = block_on(maker_storage.get_unfinished()).unwrap();
            assert_eq!(maker_unfinished.len(), 1);
            assert!(maker_unfinished.contains(&maker_uuid2));

            // Taker unfinished unchanged
            let taker_unfinished = block_on(taker_storage.get_unfinished()).unwrap();
            assert_eq!(taker_unfinished.len(), 1);
        }

        #[test]
        fn should_return_role_when_dispatching_via_get_swap_type() {
            let ctx = setup_test_ctx();
            let maker_uuid = Uuid::new_v4();
            let taker_uuid = Uuid::new_v4();

            let mut maker_storage = MakerSwapStorage::new(ctx.clone());
            let mut taker_storage = TakerSwapStorage::new(ctx.clone());

            block_on(maker_storage.store_repr(maker_uuid, sample_maker_repr(maker_uuid))).unwrap();
            block_on(taker_storage.store_repr(taker_uuid, sample_taker_repr(taker_uuid))).unwrap();

            assert_eq!(get_swap_type(&ctx, &maker_uuid).unwrap(), MAKER_SWAP_V2_TYPE);
            assert_eq!(get_swap_type(&ctx, &taker_uuid).unwrap(), TAKER_SWAP_V2_TYPE);

            // Non-existent UUID should error
            let missing = Uuid::new_v4();
            assert!(get_swap_type(&ctx, &missing).is_err());
        }

        #[test]
        fn should_preserve_order_when_appending_multiple_events() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let mut storage = MakerSwapStorage::new(ctx.clone());

            block_on(storage.store_repr(uuid, sample_maker_repr(uuid))).unwrap();

            let negotiation_data = StoredMakerNegotiationData {
                taker_secret_hash: BytesJson::from(vec![1, 2]),
                taker_coin_htlc_pub: BytesJson::from(vec![3, 4]),
                maker_coin_htlc_pub: BytesJson::from(vec![5, 6]),
                taker_coin_swap_contract: None,
                maker_coin_swap_contract: None,
                taker_payment_locktime: 1000,
                taker_funding_locktime: 2000,
            };

            // Append a sequence of events matching the happy path
            let events_to_store = vec![
                MakerSwapEvent::Initialized {
                    maker_coin_start_block: 100,
                    taker_coin_start_block: 200,
                    maker_payment_trade_fee: MmNumber::from("0.001"),
                    taker_payment_spend_trade_fee: MmNumber::from("0.002"),
                    taker_payment_spend_headroom: MmNumber::from("0.002"),
                    maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
                },
                MakerSwapEvent::WaitingForTakerFunding {
                    maker_coin_start_block: 100,
                    taker_coin_start_block: 200,
                    negotiation_data: negotiation_data.clone(),
                    maker_payment_trade_fee: MmNumber::from("0.001"),
                },
                MakerSwapEvent::TakerFundingReceived {
                    maker_coin_start_block: 100,
                    taker_coin_start_block: 200,
                    negotiation_data: negotiation_data.clone(),
                    taker_funding: BytesJson::from(vec![0xAA]),
                    maker_payment_trade_fee: MmNumber::from("0.001"),
                },
                MakerSwapEvent::Completed,
            ];

            for event in &events_to_store {
                block_on(storage.store_event(uuid, event.clone())).unwrap();
            }

            let loaded_events: Vec<MakerSwapEvent> = read_swap_v2_events(&ctx, &uuid).unwrap();
            assert_eq!(loaded_events.len(), 4);

            // Verify order by checking discriminants
            assert!(matches!(loaded_events[0], MakerSwapEvent::Initialized { .. }));
            assert!(matches!(
                loaded_events[1],
                MakerSwapEvent::WaitingForTakerFunding { .. }
            ));
            assert!(matches!(loaded_events[2], MakerSwapEvent::TakerFundingReceived { .. }));
            assert!(matches!(loaded_events[3], MakerSwapEvent::Completed));
        }

        #[test]
        fn should_include_events_when_loading_maker_repr_from_db() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let mut storage = MakerSwapStorage::new(ctx.clone());

            block_on(storage.store_repr(uuid, sample_maker_repr(uuid))).unwrap();

            // Store events
            block_on(storage.store_event(uuid, MakerSwapEvent::Initialized {
                maker_coin_start_block: 42,
                taker_coin_start_block: 84,
                maker_payment_trade_fee: MmNumber::from("0.001"),
                taker_payment_spend_trade_fee: MmNumber::from("0.002"),
                taker_payment_spend_headroom: MmNumber::from("0.002"),
                maker_payment_trade_fee_reservable: Some(MmNumber::from("0.001")),
            }))
            .unwrap();

            // get_repr should include the events
            let loaded: MakerSwapDbRepr = block_on(storage.get_repr(uuid)).unwrap();
            assert_eq!(loaded.events.len(), 1);
            match &loaded.events[0] {
                MakerSwapEvent::Initialized {
                    maker_coin_start_block, ..
                } => assert_eq!(*maker_coin_start_block, 42),
                _ => panic!("Expected Initialized"),
            }

            // Verify numeric fields survived the roundtrip
            assert_eq!(loaded.maker_volume, MmNumber::from("10"));
            assert_eq!(loaded.taker_volume, MmNumber::from("20"));
            assert_eq!(loaded.taker_premium, MmNumber::from("0.1"));
            assert_eq!(loaded.dex_fee_amount, MmNumber::from("0.01"));
            assert_eq!(loaded.dex_fee_burn, MmNumber::from("0.005"));
        }

        #[test]
        fn should_include_events_when_loading_taker_repr_from_db() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let mut storage = TakerSwapStorage::new(ctx.clone());

            block_on(storage.store_repr(uuid, sample_taker_repr(uuid))).unwrap();

            block_on(storage.store_event(uuid, TakerSwapEvent::Initialized {
                maker_coin_start_block: 42,
                taker_coin_start_block: 84,
                taker_payment_fee: MmNumber::from("0.001"),
                maker_payment_spend_fee: MmNumber::from("0.002"),
                maker_payment_spend_headroom: MmNumber::from("0.002"),
                taker_payment_fee_reservable: Some(MmNumber::from("0.001")),
            }))
            .unwrap();

            let loaded: TakerSwapDbRepr = block_on(storage.get_repr(uuid)).unwrap();
            assert_eq!(loaded.events.len(), 1);
            match &loaded.events[0] {
                TakerSwapEvent::Initialized {
                    maker_coin_start_block, ..
                } => assert_eq!(*maker_coin_start_block, 42),
                _ => panic!("Expected Initialized"),
            }
            assert_eq!(loaded.maker_volume, MmNumber::from("10"));
            assert_eq!(loaded.taker_volume, MmNumber::from("20"));
        }

        #[test]
        fn should_roundtrip_conf_settings_when_persisting_via_db() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let mut storage = MakerSwapStorage::new(ctx.clone());

            let mut repr = sample_maker_repr(uuid);
            repr.conf_settings = SwapConfirmationsSettings {
                maker_coin_confs: 5,
                maker_coin_nota: true,
                taker_coin_confs: 3,
                taker_coin_nota: false,
            };

            block_on(storage.store_repr(uuid, repr)).unwrap();
            let loaded: MakerSwapDbRepr = block_on(storage.get_repr(uuid)).unwrap();
            assert_eq!(loaded.conf_settings.maker_coin_confs, 5);
            assert!(loaded.conf_settings.maker_coin_nota);
            assert_eq!(loaded.conf_settings.taker_coin_confs, 3);
            assert!(!loaded.conf_settings.taker_coin_nota);
        }

        #[test]
        fn confirmation_gate_confs_caps_configured_confs_to_one() {
            assert_eq!(confirmation_gate_confs(0), 0);
            assert_eq!(confirmation_gate_confs(1), 1);
            assert_eq!(confirmation_gate_confs(4), 1);
        }

        #[test]
        fn should_return_false_when_checking_record_for_wrong_uuid() {
            let ctx = setup_test_ctx();
            let uuid = Uuid::new_v4();
            let wrong_uuid = Uuid::new_v4();
            let mut storage = MakerSwapStorage::new(ctx.clone());

            block_on(storage.store_repr(uuid, sample_maker_repr(uuid))).unwrap();
            assert!(block_on(storage.has_record_for(&uuid)).unwrap());
            assert!(!block_on(storage.has_record_for(&wrong_uuid)).unwrap());
        }
    }
}
