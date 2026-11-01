//! # Purpose
//! Drives the taker side of an atomic-swap V2 trade as a persistent
//! state machine. The taker accepts the maker's negotiation, posts
//! funding, observes the maker payment plus the funding-spend
//! preimage, converts funding into the taker payment, and finally
//! spends the maker payment using the maker's revealed secret.
//!
//! # Public exports
//! - [`TakerSwapEvent`] — the persisted event variants
//! - [`TakerSwapDbRepr`] — DB row + replayable event log
//! - [`TakerSwapStateMachine`] — the state-machine driver
//! - [`taker_swap_v2_kickstart`] (via `swap_v2_common`) — entry point
//!   used by the recovery loop
//!
//! # Invariants
//! - Persisted state-machine variant names (the `TakerSwapEvent`
//!   discriminants and the `Stored*NegotiationData` field names) are
//!   serde-stable; never rename without a migration path.
//! - Happy-path order:
//!   `Initialize → Initialized → Negotiated → TakerFundingSent →
//!    MakerPaymentAndFundingSpendPreimgReceived →
//!    MakerPaymentConfirmed → TakerPaymentSent → TakerPaymentSpent →
//!    MakerPaymentSpent → Completed`.
//! - Error paths:
//!   `TakerFundingRefundRequired → TakerFundingRefunded`,
//!   `TakerPaymentRefundRequired → TakerPaymentRefunded`.
//! - Abort path: any pre-funding state → `Aborted`.

use coins::{CanRefundHtlc, FeeApproxStage, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs, MakerCoinSwapOpsV2,
            MmCoin, RefundFundingSecretArgs, RefundTakerPaymentArgs, SendTakerFundingArgs, SpendMakerPaymentArgs,
            SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, ToBytes, TradePreimageValue, Transaction, TxPreimageWithSig,
            ValidateMakerPaymentArgs};
use common::executor::Timer;
use common::log::{error, info, warn};
use common::mm_number::MmNumber;
use crypto::secret_hash_algo::SecretHashAlgo;
use futures::compat::Future01CompatExt;
use keys::KeyPair;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_state_machine::prelude::*;
use mm2_state_machine::storable_state_machine::*;
use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use uuid::Uuid;

use super::swap_lock::SwapLock;
use super::swap_v2_common::*;
use super::swap_v2_pb::*;
use super::SwapConfirmationsSettings;

// Events ---------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum TakerSwapEvent {
    Initialized {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        taker_payment_fee: MmNumber,
        maker_payment_spend_fee: MmNumber,
        /// The part of `maker_payment_spend_fee` that must stay free in our own
        /// balance — zero when the maker coin takes the fee out of the payment we
        /// are claiming (CRD ch.52 R64). The coin that pays it is not persisted:
        /// it is that coin's platform ticker, which does not vary.
        ///
        /// Additive and defaulted: a log written before this field existed
        /// resumes with no headroom rather than failing to parse.
        #[serde(default)]
        maker_payment_spend_headroom: MmNumber,
        /// The reservable part of `taker_payment_fee` (CRD ch.52 R58, R62): the
        /// whole amount unless the taker coin marks it payable out of the
        /// trading volume, in which case zero. The coin that pays it is not
        /// persisted — it is `taker_coin`'s platform ticker, which does not
        /// vary — but for a log written before this field existed there is no
        /// way to recover whether the marker was set, so a `None` falls back to
        /// reserving the full legacy amount (which was always correct in *how
        /// much*, only ever wrong in *which coin's bucket*).
        #[serde(default)]
        taker_payment_fee_reservable: Option<MmNumber>,
    },
    Negotiated {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_payment_fee: MmNumber,
        maker_payment_spend_fee: MmNumber,
    },
    TakerFundingSent {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_funding: BytesJson,
    },
    TakerFundingRefundRequired {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_funding: BytesJson,
        reason: AbortReason,
    },
    MakerPaymentAndFundingSpendPreimgReceived {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_funding: BytesJson,
        funding_spend_preimage: StoredTxPreimage,
        maker_payment: BytesJson,
    },
    MakerPaymentConfirmed {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_funding: BytesJson,
        funding_spend_preimage: StoredTxPreimage,
        maker_payment: BytesJson,
    },
    TakerPaymentSent {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_payment: BytesJson,
        maker_payment: BytesJson,
    },
    TakerPaymentSentPreimageSendingSkipped {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredTakerNegotiationData,
        taker_payment: BytesJson,
        maker_payment: BytesJson,
    },
    TakerPaymentRefundRequired {
        taker_payment: BytesJson,
        negotiation_data: StoredTakerNegotiationData,
        reason: AbortReason,
    },
    TakerPaymentSpent {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        taker_payment_spend: BytesJson,
        maker_payment: BytesJson,
        negotiation_data: StoredTakerNegotiationData,
        /// The taker's own payment transaction (CRD ch.52 D8). Additive and defaulted:
        /// a log written before this field existed resumes with empty bytes here rather
        /// than failing to parse -- which is correct for such a log, since no swap logged
        /// before this field existed can still be waiting at `MakerPaymentSpent` today (V2
        /// swaps don't live that long), not a signal that empty bytes are fine going
        /// forward.
        #[serde(default)]
        taker_payment: BytesJson,
    },
    MakerPaymentSpent {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment_spend: BytesJson,
        negotiation_data: StoredTakerNegotiationData,
        /// The taker's own payment transaction (CRD ch.52 D8). Additive and defaulted:
        /// a log written before this field existed resumes with empty bytes here rather
        /// than failing to parse -- which is correct for such a log, since no swap logged
        /// before this field existed can still be waiting at `MakerPaymentSpent` today (V2
        /// swaps don't live that long), not a signal that empty bytes are fine going
        /// forward.
        #[serde(default)]
        taker_payment: BytesJson,
    },
    TakerFundingRefunded {
        funding_tx: BytesJson,
        funding_tx_refund: BytesJson,
        reason: AbortReason,
    },
    TakerPaymentRefunded {
        taker_payment: BytesJson,
        taker_payment_refund: BytesJson,
        reason: AbortReason,
    },
    Aborted {
        reason: AbortReason,
    },
    Completed,
}

// Database representation ----------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TakerSwapDbRepr {
    pub maker_coin: String,
    pub maker_volume: MmNumber,
    pub taker_secret: H256Json,
    pub taker_secret_hash: BytesJson,
    pub secret_hash_algo: SecretHashAlgo,
    pub started_at: u64,
    pub lock_duration: u64,
    pub taker_coin: String,
    pub taker_volume: MmNumber,
    pub taker_premium: MmNumber,
    pub dex_fee_amount: MmNumber,
    pub dex_fee_burn: MmNumber,
    pub conf_settings: SwapConfirmationsSettings,
    pub uuid: Uuid,
    pub p2p_keypair: Option<super::maker_swap_v2::SerializableKeypairBytes>,
    pub events: Vec<TakerSwapEvent>,
    pub maker_p2p_pub: BytesJson,
    pub swap_version: u8,
}

// State machine --------------------------------------------------------------

pub struct TakerSwapStateMachine<MakerCoin: MmCoin + MakerCoinSwapOpsV2, TakerCoin: MmCoin + TakerCoinSwapOpsV2> {
    pub ctx: MmArc,
    pub storage: TakerSwapStorage,
    pub started_at: u64,
    pub lock_duration: u64,
    pub maker_coin: MakerCoin,
    pub maker_volume: MmNumber,
    pub taker_coin: TakerCoin,
    pub taker_volume: MmNumber,
    pub taker_premium: MmNumber,
    pub secret_hash_algo: SecretHashAlgo,
    pub conf_settings: SwapConfirmationsSettings,
    pub uuid: Uuid,
    pub p2p_topic: String,
    pub p2p_keypair: Option<KeyPair>,
    pub taker_secret: primitives::hash::H256,
    pub maker_p2p_pubkey: Vec<u8>,
    pub require_maker_payment_confirm: bool,
    pub require_maker_payment_spend_confirm: bool,
    pub swap_version: u8,
    /// Maker-coin balance to keep free for spending the maker payment (ch.52
    /// R64). Learned from the `Initialized` event — on a live run when it is
    /// applied, on a resume by reading it back out of the persisted log — so
    /// that a swap resumed past initialisation still holds its headroom.
    pub maker_payment_spend_headroom: MmNumber,
    /// The reservable part of the fee to send the taker payment (ch.52 R58,
    /// R62). Populated the same way as `maker_payment_spend_headroom` — from
    /// the `Initialized` event, live or read back on resume — because
    /// `Negotiated`, which also re-creates this reservation on resume (R63),
    /// does not itself carry it.
    pub taker_payment_fee_reservable: MmNumber,
}

impl<MakerCoin, TakerCoin> TakerSwapStateMachine<MakerCoin, TakerCoin>
where
    MakerCoin: MmCoin + MakerCoinSwapOpsV2,
    TakerCoin: MmCoin + TakerCoinSwapOpsV2,
{
    pub fn maker_payment_conf_timeout(&self) -> u64 { self.started_at + self.lock_duration / 3 }
    pub fn taker_funding_locktime(&self) -> u64 { self.started_at + 3 * self.lock_duration }
    pub fn taker_payment_locktime(&self) -> u64 { self.started_at + self.lock_duration }
    pub fn unique_data(&self) -> Vec<u8> { self.uuid.as_bytes().to_vec() }
    pub fn taker_secret_hash(&self) -> Vec<u8> { self.secret_hash_algo.hash_secret(self.taker_secret.as_slice()) }

    /// The coin whose balance pays for spending the maker payment. For an ERC20
    /// maker coin that is the platform coin, not the token — the same choice the
    /// coin makes when it reports the fee.
    fn maker_payment_spend_fee_coin(&self) -> &str { self.maker_coin.platform_ticker() }

    fn reserve_maker_payment_spend_headroom(&self) {
        super::reserve_v2_spend_headroom(
            &self.ctx,
            self.uuid,
            self.maker_payment_spend_fee_coin(),
            self.maker_payment_spend_headroom.clone(),
        );
    }

    fn release_maker_payment_spend_headroom(&self) {
        super::release_v2_spend_headroom(&self.ctx, &self.uuid, self.maker_payment_spend_fee_coin());
    }

    /// The coin whose balance pays for sending the taker payment. For an ERC20
    /// taker coin that is the platform coin, not the token — the same choice
    /// the coin makes when it reports the fee (ch.52 R58, R62).
    fn taker_payment_trade_fee_coin(&self) -> &str { self.taker_coin.platform_ticker() }

    /// Reserve both halves of the short-lived (R58) reservation: the taker's
    /// own outgoing volume (plus premium), and the fee to send it — which may
    /// land in the same bucket as the volume or, for a token taker coin, in a
    /// different one. Both share the `Volume` kind and are released together by
    /// `release_taker_payment_volume`.
    fn reserve_taker_payment_volume(&self) {
        super::reserve_v2_amount(
            &self.ctx,
            self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.taker_coin.ticker(),
            &self.taker_volume + &self.taker_premium,
        );
        super::reserve_v2_amount(
            &self.ctx,
            self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.taker_payment_trade_fee_coin(),
            self.taker_payment_fee_reservable.clone(),
        );
    }

    /// Release both halves reserved by `reserve_taker_payment_volume`, in
    /// whichever bucket(s) they ended up in.
    fn release_taker_payment_volume(&self) {
        super::release_v2_amount(
            &self.ctx,
            &self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.taker_coin.ticker(),
        );
        super::release_v2_amount(
            &self.ctx,
            &self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.taker_payment_trade_fee_coin(),
        );
    }
}

// States (PhantomData-bound to the generic state machine) --------------------

pub struct Initialize<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>(PhantomData<(M, T)>);
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Default for Initialize<M, T> {
    fn default() -> Self { Initialize(PhantomData) }
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> std::fmt::Debug for Initialize<M, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("Initialize").finish() }
}

pub struct Initialized<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub taker_payment_fee: MmNumber,
    pub maker_payment_spend_fee: MmNumber,
    pub maker_payment_spend_headroom: MmNumber,
    pub taker_payment_fee_reservable: Option<MmNumber>,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Initialized<M, T> {
    pub fn new(mb: u64, tb: u64, tf: MmNumber, mf: MmNumber, mh: MmNumber, tfr: Option<MmNumber>) -> Self {
        Initialized {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            taker_payment_fee: tf,
            maker_payment_spend_fee: mf,
            maker_payment_spend_headroom: mh,
            taker_payment_fee_reservable: tfr,
            _p: PhantomData,
        }
    }
}

pub struct Negotiated<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_payment_fee: MmNumber,
    pub maker_payment_spend_fee: MmNumber,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Negotiated<M, T> {
    pub fn new(mb: u64, tb: u64, nd: StoredTakerNegotiationData, tf: MmNumber, mf: MmNumber) -> Self {
        Negotiated {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_payment_fee: tf,
            maker_payment_spend_fee: mf,
            _p: PhantomData,
        }
    }
}

pub struct TakerFundingSent<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_funding: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerFundingSent<M, T> {
    pub fn new(mb: u64, tb: u64, nd: StoredTakerNegotiationData, tf: BytesJson) -> Self {
        TakerFundingSent {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_funding: tf,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `TakerFundingRefundRequired` and differ only in `reason`.
    async fn abort_to_funding_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut TakerSwapStateMachine<M, T>,
    ) -> StateResult<TakerSwapStateMachine<M, T>> {
        Self::change_state(
            TakerFundingRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.taker_funding.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct MakerPaymentAndFundingSpendPreimgReceived<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_funding: BytesJson,
    pub funding_spend_preimage: StoredTxPreimage,
    pub maker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentAndFundingSpendPreimgReceived<M, T> {
    pub fn new(
        mb: u64,
        tb: u64,
        nd: StoredTakerNegotiationData,
        tf: BytesJson,
        fp: StoredTxPreimage,
        mp: BytesJson,
    ) -> Self {
        MakerPaymentAndFundingSpendPreimgReceived {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_funding: tf,
            funding_spend_preimage: fp,
            maker_payment: mp,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `TakerFundingRefundRequired` and differ only in `reason`.
    async fn abort_to_funding_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut TakerSwapStateMachine<M, T>,
    ) -> StateResult<TakerSwapStateMachine<M, T>> {
        Self::change_state(
            TakerFundingRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.taker_funding.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct MakerPaymentConfirmed<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_funding: BytesJson,
    pub funding_spend_preimage: StoredTxPreimage,
    pub maker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentConfirmed<M, T> {
    pub fn new(
        mb: u64,
        tb: u64,
        nd: StoredTakerNegotiationData,
        tf: BytesJson,
        fp: StoredTxPreimage,
        mp: BytesJson,
    ) -> Self {
        MakerPaymentConfirmed {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_funding: tf,
            funding_spend_preimage: fp,
            maker_payment: mp,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `TakerFundingRefundRequired` and differ only in `reason`.
    async fn abort_to_funding_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut TakerSwapStateMachine<M, T>,
    ) -> StateResult<TakerSwapStateMachine<M, T>> {
        Self::change_state(
            TakerFundingRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.taker_funding.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentSent<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_payment: BytesJson,
    pub maker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentSent<M, T> {
    pub fn new(mb: u64, tb: u64, nd: StoredTakerNegotiationData, tp: BytesJson, mp: BytesJson) -> Self {
        TakerPaymentSent {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_payment: tp,
            maker_payment: mp,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `TakerPaymentRefundRequired` and differ only in `reason`.
    async fn abort_to_payment_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut TakerSwapStateMachine<M, T>,
    ) -> StateResult<TakerSwapStateMachine<M, T>> {
        Self::change_state(
            TakerPaymentRefundRequired::new(self.taker_payment.clone(), self.negotiation_data.clone(), reason),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentSentPreimageSendingSkipped<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_payment: BytesJson,
    pub maker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentSentPreimageSendingSkipped<M, T> {
    pub fn new(mb: u64, tb: u64, nd: StoredTakerNegotiationData, tp: BytesJson, mp: BytesJson) -> Self {
        TakerPaymentSentPreimageSendingSkipped {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_payment: tp,
            maker_payment: mp,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `TakerPaymentRefundRequired` and differ only in `reason`.
    async fn abort_to_payment_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut TakerSwapStateMachine<M, T>,
    ) -> StateResult<TakerSwapStateMachine<M, T>> {
        Self::change_state(
            TakerPaymentRefundRequired::new(self.taker_payment.clone(), self.negotiation_data.clone(), reason),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentSpent<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub taker_payment_spend: BytesJson,
    pub maker_payment: BytesJson,
    pub negotiation_data: StoredTakerNegotiationData,
    /// The taker's own payment transaction, carried forward from `TakerPaymentSent`/
    /// `TakerPaymentSentPreimageSendingSkipped` (CRD ch.52 D8) so a later
    /// confirmation-timeout abort at `MakerPaymentSpent` still has real bytes to refund,
    /// instead of the state field scope dropping them two hops too early.
    pub taker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentSpent<M, T> {
    pub fn new(mb: u64, tb: u64, tps: BytesJson, mp: BytesJson, nd: StoredTakerNegotiationData, tp: BytesJson) -> Self {
        TakerPaymentSpent {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            taker_payment_spend: tps,
            maker_payment: mp,
            negotiation_data: nd,
            taker_payment: tp,
            _p: PhantomData,
        }
    }
}

pub struct MakerPaymentSpent<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub maker_payment_spend: BytesJson,
    pub negotiation_data: StoredTakerNegotiationData,
    /// The taker's own payment transaction, carried forward from `TakerPaymentSpent`
    /// (CRD ch.52 D8) so `on_changed`'s confirmation-timeout abort path can build a
    /// working `TakerPaymentRefundRequired` instead of falling back to empty bytes that
    /// `refund_combined_taker_payment` cannot use.
    pub taker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentSpent<M, T> {
    pub fn new(mb: u64, tb: u64, mps: BytesJson, nd: StoredTakerNegotiationData, tp: BytesJson) -> Self {
        MakerPaymentSpent {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            maker_payment_spend: mps,
            negotiation_data: nd,
            taker_payment: tp,
            _p: PhantomData,
        }
    }
}

pub struct TakerFundingRefundRequired<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredTakerNegotiationData,
    pub taker_funding: BytesJson,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerFundingRefundRequired<M, T> {
    pub fn new(mb: u64, tb: u64, nd: StoredTakerNegotiationData, tf: BytesJson, r: AbortReason) -> Self {
        TakerFundingRefundRequired {
            maker_coin_start_block: mb,
            taker_coin_start_block: tb,
            negotiation_data: nd,
            taker_funding: tf,
            reason: r,
            _p: PhantomData,
        }
    }
}

pub struct TakerPaymentRefundRequired<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub taker_payment: BytesJson,
    pub negotiation_data: StoredTakerNegotiationData,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentRefundRequired<M, T> {
    pub fn new(tp: BytesJson, nd: StoredTakerNegotiationData, r: AbortReason) -> Self {
        TakerPaymentRefundRequired {
            taker_payment: tp,
            negotiation_data: nd,
            reason: r,
            _p: PhantomData,
        }
    }
}

pub struct TakerFundingRefunded<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub funding_tx: BytesJson,
    pub funding_tx_refund: BytesJson,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerFundingRefunded<M, T> {
    pub fn new(ft: BytesJson, ftr: BytesJson, r: AbortReason) -> Self {
        TakerFundingRefunded {
            funding_tx: ft,
            funding_tx_refund: ftr,
            reason: r,
            _p: PhantomData,
        }
    }
}

pub struct TakerPaymentRefunded<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub taker_payment: BytesJson,
    pub taker_payment_refund: BytesJson,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentRefunded<M, T> {
    pub fn new(tp: BytesJson, tpr: BytesJson, r: AbortReason) -> Self {
        TakerPaymentRefunded {
            taker_payment: tp,
            taker_payment_refund: tpr,
            reason: r,
            _p: PhantomData,
        }
    }
}

pub struct Completed<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>(PhantomData<(M, T)>);
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Completed<M, T> {
    pub fn new() -> Self { Completed(PhantomData) }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Default for Completed<M, T> {
    fn default() -> Self { Self::new() }
}

pub struct Aborted<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Aborted<M, T> {
    pub fn new(reason: AbortReason) -> Self {
        Aborted {
            reason,
            _p: PhantomData,
        }
    }
}

// Transition declarations (state → state) ------------------------------------

// Initialize →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Initialize<M, T>>
    for Initialized<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Initialize<M, T>>
    for Aborted<M, T>
{
}

// Initialized →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Initialized<M, T>>
    for Negotiated<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Initialized<M, T>>
    for Aborted<M, T>
{
}

// Negotiated →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Negotiated<M, T>>
    for TakerFundingSent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Negotiated<M, T>>
    for Aborted<M, T>
{
}

// TakerFundingSent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingSent<M, T>>
    for MakerPaymentAndFundingSpendPreimgReceived<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingSent<M, T>>
    for TakerFundingRefundRequired<M, T>
{
}

// MakerPaymentAndFundingSpendPreimgReceived →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentAndFundingSpendPreimgReceived<M, T>> for MakerPaymentConfirmed<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentAndFundingSpendPreimgReceived<M, T>> for TakerPaymentSent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentAndFundingSpendPreimgReceived<M, T>> for TakerPaymentSentPreimageSendingSkipped<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentAndFundingSpendPreimgReceived<M, T>> for TakerFundingRefundRequired<M, T>
{
}

// MakerPaymentConfirmed →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentConfirmed<M, T>>
    for TakerPaymentSent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentConfirmed<M, T>>
    for TakerPaymentSentPreimageSendingSkipped<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentConfirmed<M, T>>
    for TakerFundingRefundRequired<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentConfirmed<M, T>>
    for TakerPaymentRefundRequired<M, T>
{
}

// TakerPaymentSent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSent<M, T>>
    for TakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSent<M, T>>
    for TakerPaymentRefundRequired<M, T>
{
}

// TakerPaymentSentPreimageSendingSkipped →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<TakerPaymentSentPreimageSendingSkipped<M, T>> for TakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<TakerPaymentSentPreimageSendingSkipped<M, T>> for TakerPaymentRefundRequired<M, T>
{
}

// TakerPaymentSpent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for MakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for Aborted<M, T>
{
}

// MakerPaymentSpent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentSpent<M, T>>
    for Completed<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentSpent<M, T>>
    for TakerPaymentRefundRequired<M, T>
{
}

// TakerFundingRefundRequired →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingRefundRequired<M, T>>
    for TakerFundingRefunded<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingRefundRequired<M, T>>
    for Aborted<M, T>
{
}

// TakerPaymentRefundRequired →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentRefundRequired<M, T>>
    for TakerPaymentRefunded<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentRefundRequired<M, T>>
    for Aborted<M, T>
{
}

// StorableStateMachine -------------------------------------------------------

const TAKER_SWAP_LOCK_TTL: f64 = 120.0;
const TAKER_SWAP_LOCK_RENEW_INTERVAL: f64 = 30.0;

#[async_trait::async_trait]
impl<MakerCoin, TakerCoin> StorableStateMachine for TakerSwapStateMachine<MakerCoin, TakerCoin>
where
    MakerCoin: MmCoin + MakerCoinSwapOpsV2,
    TakerCoin: MmCoin + TakerCoinSwapOpsV2,
{
    type Storage = TakerSwapStorage;
    type Result = ();
    type Error = MmError<SwapStateMachineError>;
    type ReentrancyLock = SwapLock;
    type RecreateCtx = SwapRecreateCtx<MakerCoin, TakerCoin>;
    type RecreateError = MmError<SwapRecreateError>;

    fn to_db_repr(&self) -> TakerSwapDbRepr {
        TakerSwapDbRepr {
            maker_coin: self.maker_coin.ticker().into(),
            maker_volume: self.maker_volume.clone(),
            taker_secret: self.taker_secret.into(),
            taker_secret_hash: self.taker_secret_hash().into(),
            secret_hash_algo: self.secret_hash_algo,
            started_at: self.started_at,
            lock_duration: self.lock_duration,
            taker_coin: self.taker_coin.ticker().into(),
            taker_volume: self.taker_volume.clone(),
            taker_premium: self.taker_premium.clone(),
            dex_fee_amount: MmNumber::default(),
            dex_fee_burn: MmNumber::default(),
            conf_settings: self.conf_settings,
            uuid: self.uuid,
            p2p_keypair: self
                .p2p_keypair
                .as_ref()
                .map(|kp| super::maker_swap_v2::SerializableKeypairBytes(kp.private().secret.to_vec())),
            events: Vec::new(),
            maker_p2p_pub: BytesJson::from(self.maker_p2p_pubkey.clone()),
            swap_version: self.swap_version,
        }
    }

    fn storage(&mut self) -> &mut Self::Storage { &mut self.storage }
    fn id(&self) -> <Self::Storage as StateMachineStorage>::MachineId { self.uuid }

    async fn recreate_machine(
        uuid: Uuid,
        storage: TakerSwapStorage,
        mut repr: TakerSwapDbRepr,
        recreate_ctx: Self::RecreateCtx,
    ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Self::RecreateError> {
        if repr.events.is_empty() {
            return MmError::err(SwapRecreateError::NoEvents);
        }
        let last_event = repr.events.remove(repr.events.len() - 1);

        // Only `Initialized` records the headroom, but the swap owes it for as
        // long as the maker payment is unspent, so recover it from the log
        // rather than from whichever event we happen to be resuming at.
        let (maker_payment_spend_headroom, taker_payment_fee_reservable) = repr
            .events
            .iter()
            .chain(std::iter::once(&last_event))
            .find_map(|event| match event {
                TakerSwapEvent::Initialized {
                    taker_payment_fee,
                    maker_payment_spend_headroom,
                    taker_payment_fee_reservable,
                    ..
                } => Some((
                    maker_payment_spend_headroom.clone(),
                    // A log written before this field existed has no way to say
                    // whether the fee was payable out of the trading volume, so
                    // fall back to the legacy assumption that it was not — the
                    // full amount, which was always the correct *quantity* for
                    // every coin family implementing V2 today, only ever wrong
                    // in *which coin's bucket* it belonged to.
                    taker_payment_fee_reservable
                        .clone()
                        .unwrap_or_else(|| taker_payment_fee.clone()),
                )),
                _ => None,
            })
            .unwrap_or_default();

        let current_state: Box<dyn RestoredState<StateMachine = Self>> = match last_event {
            TakerSwapEvent::Initialized {
                maker_coin_start_block,
                taker_coin_start_block,
                taker_payment_fee,
                maker_payment_spend_fee,
                maker_payment_spend_headroom,
                taker_payment_fee_reservable,
            } => Box::new(Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                taker_payment_fee,
                maker_payment_spend_fee,
                maker_payment_spend_headroom,
                taker_payment_fee_reservable,
            )),
            TakerSwapEvent::Negotiated {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment_fee,
                maker_payment_spend_fee,
            } => Box::new(Negotiated::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment_fee,
                maker_payment_spend_fee,
            )),
            TakerSwapEvent::TakerFundingSent {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
            } => Box::new(TakerFundingSent::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
            )),
            TakerSwapEvent::TakerFundingRefundRequired {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                reason,
            } => Box::new(TakerFundingRefundRequired::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                reason,
            )),
            TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                funding_spend_preimage,
                maker_payment,
            } => Box::new(MakerPaymentAndFundingSpendPreimgReceived::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                funding_spend_preimage,
                maker_payment,
            )),
            TakerSwapEvent::MakerPaymentConfirmed {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                funding_spend_preimage,
                maker_payment,
            } => Box::new(MakerPaymentConfirmed::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                funding_spend_preimage,
                maker_payment,
            )),
            TakerSwapEvent::TakerPaymentSent {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment,
                maker_payment,
            } => Box::new(TakerPaymentSent::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment,
                maker_payment,
            )),
            TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment,
                maker_payment,
            } => Box::new(TakerPaymentSentPreimageSendingSkipped::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_payment,
                maker_payment,
            )),
            TakerSwapEvent::TakerPaymentRefundRequired {
                taker_payment,
                negotiation_data,
                reason,
            } => Box::new(TakerPaymentRefundRequired::new(taker_payment, negotiation_data, reason)),
            TakerSwapEvent::TakerPaymentSpent {
                maker_coin_start_block,
                taker_coin_start_block,
                taker_payment_spend,
                maker_payment,
                negotiation_data,
                taker_payment,
            } => Box::new(TakerPaymentSpent::new(
                maker_coin_start_block,
                taker_coin_start_block,
                taker_payment_spend,
                maker_payment,
                negotiation_data,
                taker_payment,
            )),
            TakerSwapEvent::MakerPaymentSpent {
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_spend,
                negotiation_data,
                taker_payment,
            } => Box::new(MakerPaymentSpent::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_spend,
                negotiation_data,
                taker_payment,
            )),
            TakerSwapEvent::TakerFundingRefunded { .. } => {
                return MmError::err(SwapRecreateError::Internal(
                    "Cannot recreate from TakerFundingRefunded".into(),
                ))
            },
            TakerSwapEvent::TakerPaymentRefunded { .. } => {
                return MmError::err(SwapRecreateError::Internal(
                    "Cannot recreate from TakerPaymentRefunded".into(),
                ))
            },
            TakerSwapEvent::Aborted { .. } => {
                return MmError::err(SwapRecreateError::Internal("Cannot recreate from Aborted".into()))
            },
            TakerSwapEvent::Completed => {
                return MmError::err(SwapRecreateError::Internal("Cannot recreate from Completed".into()))
            },
        };

        let p2p_topic = super::swap_v2_topic(&uuid);
        let machine = TakerSwapStateMachine {
            ctx: storage.get_ctx(),
            storage,
            started_at: repr.started_at,
            lock_duration: repr.lock_duration,
            maker_coin: recreate_ctx.maker_coin,
            maker_volume: repr.maker_volume,
            taker_coin: recreate_ctx.taker_coin,
            taker_volume: repr.taker_volume,
            taker_premium: repr.taker_premium,
            secret_hash_algo: repr.secret_hash_algo,
            conf_settings: repr.conf_settings,
            uuid,
            p2p_topic,
            p2p_keypair: repr.p2p_keypair.and_then(|k| {
                let mut secret = keys::Secret::default();
                let len = k.0.len().min(secret.len());
                secret[..len].copy_from_slice(&k.0[..len]);
                let private = keys::Private {
                    prefix: 0,
                    secret,
                    compressed: true,
                    checksum_type: Default::default(),
                };
                KeyPair::from_private(private).ok()
            }),
            taker_secret: repr.taker_secret.into(),
            maker_p2p_pubkey: repr.maker_p2p_pub.into(),
            require_maker_payment_confirm: true,
            require_maker_payment_spend_confirm: true,
            swap_version: repr.swap_version,
            maker_payment_spend_headroom,
            taker_payment_fee_reservable,
        };

        Ok((RestoredMachine::new(machine), current_state))
    }

    async fn acquire_reentrancy_lock(&self) -> Result<Self::ReentrancyLock, Self::Error> {
        acquire_reentrancy_lock_impl(&self.ctx, self.uuid, TAKER_SWAP_LOCK_TTL).await
    }

    fn spawn_reentrancy_lock_renew(&mut self, guard: Self::ReentrancyLock) {
        spawn_reentrancy_lock_renew(guard, TAKER_SWAP_LOCK_RENEW_INTERVAL);
    }

    fn init_additional_context(&mut self) {
        let swap_info = ActiveSwapV2Info {
            uuid: self.uuid,
            maker_coin: self.maker_coin.ticker().into(),
            taker_coin: self.taker_coin.ticker().into(),
            swap_type: SwapV2Type::TakerV2,
        };
        // Safe: `from_ctx` (mm2_core::mm_ctx::from_ctx) memoizes a per-process
        // singleton keyed on `ctx.swaps_ctx`. Every other call site in this
        // codebase (dozens, across lp_swap.rs, swap_v2_rpcs.rs, swap_msg.rs, ...)
        // already treats it as infallible via `.unwrap()`, and the constructor
        // closure has no fallible/panicking step of its own — failure here would
        // require a poisoned `ctx.swaps_ctx` mutex, i.e. an earlier panic while
        // holding that exact lock, which nothing in `from_ctx`'s own closure does.
        // See docs/plans/v2-swap-engine-hardening.md (T10/P2.3) for the full
        // reachability analysis.
        let swap_ctx = super::SwapsContext::from_ctx(&self.ctx).expect("SwapsContext should exist");
        swap_ctx.add_active_swap_v2(swap_info);
        // KNOWN GAP (T10/P2.3, docs/plans/v2-swap-engine-hardening.md): unlike
        // the `SwapsContext` lookup above, this parse is NOT provably
        // unreachable. This codebase has no fresh-swap-initiation path yet —
        // `TakerSwapStateMachine` is only ever built by `recreate_machine` — and
        // `recreate_machine` does not itself validate `maker_p2p_pub` before
        // accepting a stored record, even though CRD ch.52 R9's "fifth case"
        // requires stored pubkey bytes that no longer parse to fail recreation
        // with a distinct, typed error rather than reach here. A malformed
        // persisted pubkey does reach this `.expect()`. Left as a panic rather
        // than a logged skip-and-continue of `init_v2_msg_store`: skipping it
        // would silently break this swap's entire P2P message routing, while the
        // reentrancy-lock-renewal task (already spawned in `on_start`, on the
        // shared executor, independent of this hook) keeps holding the swap's
        // lock forever in this process either way — so a graceful degrade here
        // does not clearly dominate a loud panic. The real fix belongs in
        // `recreate_machine` per R9, not this infallible hook.
        let accept_from = secp256k1::PublicKey::from_slice(&self.maker_p2p_pubkey)
            .expect("maker_p2p_pubkey must be a valid 33-byte compressed pubkey");
        swap_ctx.init_v2_msg_store(self.uuid, accept_from);
    }

    fn clean_up_context(&mut self) {
        // See the `init_additional_context` comment above — same guarantee applies.
        let swap_ctx = super::SwapsContext::from_ctx(&self.ctx).expect("SwapsContext should exist");
        swap_ctx.remove_active_swap_v2(&self.uuid);
        swap_ctx.remove_v2_msg_store(&self.uuid);

        // Every entry, not just the two the swap trades in: an ERC20 leg files
        // its spend headroom under the platform coin (ch.52 R59).
        super::release_all_v2_locked_amounts(&self.ctx, &self.uuid);
    }

    fn on_event(&mut self, event: &TakerSwapEvent) {
        match event {
            TakerSwapEvent::Initialized {
                taker_payment_fee,
                maker_payment_spend_headroom,
                taker_payment_fee_reservable,
                ..
            } => {
                self.taker_payment_fee_reservable = taker_payment_fee_reservable
                    .clone()
                    .unwrap_or_else(|| taker_payment_fee.clone());
                self.reserve_taker_payment_volume();

                self.maker_payment_spend_headroom = maker_payment_spend_headroom.clone();
                self.reserve_maker_payment_spend_headroom();
            },
            // Both halves of R58's reservation die together: the payment being
            // broadcast is what both the volume and its send fee were held
            // against.
            TakerSwapEvent::TakerFundingSent { .. } => self.release_taker_payment_volume(),
            // The maker payment is ours now, so the fee to claim it is no longer
            // owed and its headroom is released (ch.52 R64).
            TakerSwapEvent::MakerPaymentSpent { .. } => self.release_maker_payment_spend_headroom(),
            TakerSwapEvent::Negotiated { .. }
            | TakerSwapEvent::TakerFundingRefundRequired { .. }
            | TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived { .. }
            | TakerSwapEvent::TakerPaymentSent { .. }
            | TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped { .. }
            | TakerSwapEvent::TakerPaymentRefundRequired { .. }
            | TakerSwapEvent::MakerPaymentConfirmed { .. }
            | TakerSwapEvent::TakerPaymentSpent { .. }
            | TakerSwapEvent::TakerFundingRefunded { .. }
            | TakerSwapEvent::TakerPaymentRefunded { .. }
            | TakerSwapEvent::Aborted { .. }
            | TakerSwapEvent::Completed => (),
        }
        // Send a notification to the swap status streamer about a new event.
        self.ctx
            .event_stream_manager
            .send_fn(&mm2_event_stream::StreamerId::SwapStatus, || {
                super::swap_events::SwapStatusEvent::TakerV2 {
                    uuid: self.uuid,
                    event: event.clone(),
                }
            })
            .ok();
    }

    fn on_kickstart_event(&mut self, event: TakerSwapEvent) {
        // The volume entry comes back only where the funding is still unsent, but
        // the headroom is owed for as long as the maker payment is unspent —
        // which is every resumable event but the spend itself. This is what the
        // legacy protocol reserves at the same points (ch.51 R55), and the two
        // answers have to agree.
        if !matches!(event, TakerSwapEvent::MakerPaymentSpent { .. }) {
            self.reserve_maker_payment_spend_headroom();
        }

        match event {
            // `self.taker_payment_fee_reservable` was already recovered from the
            // persisted `Initialized` event during `recreate_machine`, so the
            // volume reservation of R58 does not need to re-extract anything
            // from whichever of the two events below we are actually resuming
            // at.
            TakerSwapEvent::Initialized { .. } | TakerSwapEvent::Negotiated { .. } => {
                self.reserve_taker_payment_volume()
            },
            TakerSwapEvent::TakerFundingSent { .. }
            | TakerSwapEvent::TakerFundingRefundRequired { .. }
            | TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived { .. }
            | TakerSwapEvent::TakerPaymentSent { .. }
            | TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped { .. }
            | TakerSwapEvent::TakerPaymentRefundRequired { .. }
            | TakerSwapEvent::MakerPaymentConfirmed { .. }
            | TakerSwapEvent::TakerPaymentSpent { .. }
            | TakerSwapEvent::MakerPaymentSpent { .. }
            | TakerSwapEvent::TakerFundingRefunded { .. }
            | TakerSwapEvent::TakerPaymentRefunded { .. }
            | TakerSwapEvent::Aborted { .. }
            | TakerSwapEvent::Completed => (),
        }
    }
}

// InitialState / StorableState -----------------------------------------------

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> InitialState for Initialize<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Initialized<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::Initialized {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            taker_payment_fee: self.taker_payment_fee.clone(),
            maker_payment_spend_fee: self.maker_payment_spend_fee.clone(),
            maker_payment_spend_headroom: self.maker_payment_spend_headroom.clone(),
            taker_payment_fee_reservable: self.taker_payment_fee_reservable.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Negotiated<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::Negotiated {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_payment_fee: self.taker_payment_fee.clone(),
            maker_payment_spend_fee: self.maker_payment_spend_fee.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerFundingSent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerFundingSent {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_funding: self.taker_funding.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for MakerPaymentAndFundingSpendPreimgReceived<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::MakerPaymentAndFundingSpendPreimgReceived {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_funding: self.taker_funding.clone(),
            funding_spend_preimage: self.funding_spend_preimage.clone(),
            maker_payment: self.maker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for MakerPaymentConfirmed<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::MakerPaymentConfirmed {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_funding: self.taker_funding.clone(),
            funding_spend_preimage: self.funding_spend_preimage.clone(),
            maker_payment: self.maker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerPaymentSent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerPaymentSent {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_payment: self.taker_payment.clone(),
            maker_payment: self.maker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for TakerPaymentSentPreimageSendingSkipped<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerPaymentSentPreimageSendingSkipped {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_payment: self.taker_payment.clone(),
            maker_payment: self.maker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerPaymentSpent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerPaymentSpent {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            taker_payment_spend: self.taker_payment_spend.clone(),
            maker_payment: self.maker_payment.clone(),
            negotiation_data: self.negotiation_data.clone(),
            taker_payment: self.taker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for MakerPaymentSpent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::MakerPaymentSpent {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            maker_payment_spend: self.maker_payment_spend.clone(),
            negotiation_data: self.negotiation_data.clone(),
            taker_payment: self.taker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for TakerFundingRefundRequired<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerFundingRefundRequired {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_funding: self.taker_funding.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for TakerPaymentRefundRequired<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerPaymentRefundRequired {
            taker_payment: self.taker_payment.clone(),
            negotiation_data: self.negotiation_data.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerFundingRefunded<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerFundingRefunded {
            funding_tx: self.funding_tx.clone(),
            funding_tx_refund: self.funding_tx_refund.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerPaymentRefunded<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::TakerPaymentRefunded {
            taker_payment: self.taker_payment.clone(),
            taker_payment_refund: self.taker_payment_refund.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Completed<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent { TakerSwapEvent::Completed }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Aborted<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    fn get_event(&self) -> TakerSwapEvent {
        TakerSwapEvent::Aborted {
            reason: self.reason.clone(),
        }
    }
}

// State / LastState implementations ------------------------------------------

const MAX_STARTED_AT_DIFF: u64 = 60;

// Initialize → Initialized ----------------------------------------------

// Fetch start blocks, estimate fees, check balance.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for Initialize<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let maker_coin_start_block = match sm.maker_coin.current_block().compat().await {
            Ok(b) => b,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get maker coin block: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let taker_coin_start_block = match sm.taker_coin.current_block().compat().await {
            Ok(b) => b,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get taker coin block: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let stage = FeeApproxStage::StartSwap;
        let taker_payment_trade_fee = match sm
            .taker_coin
            .get_sender_trade_fee(TradePreimageValue::Exact(sm.taker_volume.to_decimal()), stage.clone())
            .await
        {
            Ok(f) => f,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get taker payment fee: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let maker_payment_spend_trade_fee = match sm.maker_coin.get_receiver_trade_fee(stage).compat().await {
            Ok(f) => f,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get maker payment spend fee: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Inline balance check: taker needs to cover volume + premium + dex_fee + payment_fee.
        let spendable = match sm.taker_coin.my_spendable_balance().compat().await {
            Ok(b) => b,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get spendable balance: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let required =
            sm.taker_volume.to_decimal() + sm.taker_premium.to_decimal() + taker_payment_trade_fee.amount.to_decimal();
        if spendable < required {
            let reason = AbortReason::InternalError(format!(
                "Insufficient balance: need {} but have {}",
                required, spendable
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Resolved here, where the coin's own descriptor is still available: the
        // persisted event carries a bare number, which cannot say whether the
        // fee comes out of the payment being claimed or out of our balance.
        let maker_payment_spend_headroom = super::fee_reservable_amount(&maker_payment_spend_trade_fee);
        // Same resolution for our own payment's send fee (ch.52 R58, closing
        // V5): `taker_payment_trade_fee.coin` need not be the taker coin — an
        // ERC20 taker coin bills it to the platform coin — and only the
        // descriptor in hand right now says so.
        let taker_payment_fee_reservable = Some(super::fee_reservable_amount(&taker_payment_trade_fee));

        info!("Taker swap {} has successfully started", sm.uuid);
        Self::change_state(
            Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                taker_payment_trade_fee.amount,
                maker_payment_spend_trade_fee.amount,
                maker_payment_spend_headroom,
                taker_payment_fee_reservable,
            ),
            sm,
        )
        .await
    }
}

// Initialized → Negotiated ----------------------------------------------

// Receive maker's negotiation, validate, respond with taker negotiation.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for Initialized<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        // Receive maker's negotiation message.
        let maker_negotiation = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.maker_negotiation.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Swap {}: failed to receive maker negotiation: {}", sm.uuid, e);
                return Self::change_state(Aborted::new(AbortReason::NegotiationTimeout), sm).await;
            },
        };

        // Validate started_at difference.
        let started_at_diff = sm.started_at.abs_diff(maker_negotiation.started_at);
        if started_at_diff > MAX_STARTED_AT_DIFF {
            let reason = AbortReason::NegotiationFailed(format!(
                "started_at difference too large: {} > {}",
                started_at_diff, MAX_STARTED_AT_DIFF
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Validate maker's secret hash length.
        if maker_negotiation.secret_hash.len() != 20 && maker_negotiation.secret_hash.len() != 32 {
            let reason = AbortReason::NegotiationFailed(format!(
                "Invalid maker secret hash length: {}",
                maker_negotiation.secret_hash.len()
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Validate maker payment locktime: should be started_at + 2 * lock_duration.
        let expected_payment_locktime = sm.started_at + 2 * sm.lock_duration;
        if maker_negotiation.payment_locktime != expected_payment_locktime {
            let reason = AbortReason::NegotiationFailed(format!(
                "Unexpected maker payment locktime: got {}, expected {}",
                maker_negotiation.payment_locktime, expected_payment_locktime
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Parse maker HTLC pubkeys (validates they're well-formed).
        if sm
            .maker_coin
            .parse_pubkey(&maker_negotiation.maker_coin_htlc_pub)
            .is_err()
        {
            let reason = AbortReason::NegotiationFailed("Invalid maker's maker-coin HTLC pubkey".into());
            return Self::change_state(Aborted::new(reason), sm).await;
        }
        if sm
            .taker_coin
            .parse_pubkey(&maker_negotiation.taker_coin_htlc_pub)
            .is_err()
        {
            let reason = AbortReason::NegotiationFailed("Invalid maker's taker-coin HTLC pubkey".into());
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Build and broadcast taker negotiation response.
        let unique_data = sm.unique_data();
        let maker_coin_htlc_pub = match sm.maker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive maker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let taker_coin_htlc_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let taker_negotiation_msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerNegotiation(TakerNegotiation {
                action: Some(taker_negotiation::Action::Continue(TakerNegotiationData {
                    started_at: sm.started_at,
                    funding_locktime: sm.taker_funding_locktime(),
                    payment_locktime: sm.taker_payment_locktime(),
                    taker_secret_hash: sm.taker_secret_hash().to_vec(),
                    maker_coin_htlc_pub: maker_coin_htlc_pub.to_vec(),
                    taker_coin_htlc_pub: taker_coin_htlc_pub.to_vec(),
                    maker_coin_swap_contract: sm.maker_coin.swap_contract_address().map(|b| b.0),
                    taker_coin_swap_contract: sm.taker_coin.swap_contract_address().map(|b| b.0),
                })),
            })),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };

        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            taker_negotiation_msg,
            super::NEGOTIATE_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        // Wait for maker to acknowledge negotiation.
        let maker_negotiated = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.maker_negotiated.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Swap {}: failed to receive MakerNegotiated: {}", sm.uuid, e);
                return Self::change_state(Aborted::new(AbortReason::NegotiationTimeout), sm).await;
            },
        };

        if !maker_negotiated.negotiated {
            let reason = AbortReason::MakerAborted(maker_negotiated.reason.unwrap_or_else(|| "unknown".into()));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        let negotiation_data = StoredTakerNegotiationData {
            maker_secret_hash: maker_negotiation.secret_hash.into(),
            maker_coin_htlc_pub: maker_negotiation.maker_coin_htlc_pub.into(),
            taker_coin_htlc_pub: maker_negotiation.taker_coin_htlc_pub.into(),
            maker_coin_swap_contract: maker_negotiation.maker_coin_swap_contract.map(Into::into),
            taker_coin_swap_contract: maker_negotiation.taker_coin_swap_contract.map(Into::into),
            maker_payment_locktime: maker_negotiation.payment_locktime,
            taker_coin_address: maker_negotiation.taker_coin_address,
        };

        Self::change_state(
            Negotiated::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                negotiation_data,
                self.taker_payment_fee.clone(),
                self.maker_payment_spend_fee.clone(),
            ),
            sm,
        )
        .await
    }
}

// Negotiated → TakerFundingSent -----------------------------------------

// Send taker funding transaction.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for Negotiated<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();
        let taker_coin_htlc_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &taker_coin_htlc_pub,
        );

        let funding_args = SendTakerFundingArgs {
            funding_time_lock: sm.taker_funding_locktime(),
            payment_time_lock: sm.taker_payment_locktime(),
            taker_secret_hash: &taker_secret_hash,
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
            maker_pub: &self.negotiation_data.taker_coin_htlc_pub,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            swap_unique_data: &unique_data,
        };

        let funding_tx = match sm.taker_coin.send_taker_funding(funding_args).await {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendPayment(format!("Failed to send taker funding: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let funding_tx_bytes: BytesJson = funding_tx.tx_hex().into();
        info!("Taker swap {}: funding tx sent: {:?}", sm.uuid, funding_tx.tx_hash());

        // Broadcast funding info to maker.
        let funding_info_msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerFundingInfo(TakerFundingInfo {
                tx_bytes: funding_tx_bytes.0.clone(),
                next_step_instructions: None,
            })),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };
        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            funding_info_msg,
            super::TX_INFO_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        Self::change_state(
            TakerFundingSent::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                funding_tx_bytes,
            ),
            sm,
        )
        .await
    }
}

// TakerFundingSent → MakerPaymentAndFundingSpendPreimgReceived ----------

// Wait for maker's payment + funding-spend preimage.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerFundingSent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let maker_payment_info = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.maker_payment.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Swap {}: did not receive maker payment info: {}", sm.uuid, e);
                let reason = AbortReason::DidNotReceiveMakerPayment(e);
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };

        // Parse the maker payment tx.
        if let Err(e) = sm.maker_coin.parse_tx(&maker_payment_info.tx_bytes) {
            let reason = AbortReason::FailedToParseMakerPayment(format!("{:?}", e));
            return self.abort_to_funding_refund_required(reason, sm).await;
        }

        // Parse the funding spend preimage and signature.
        if let Err(e) = sm.taker_coin.parse_preimage(&maker_payment_info.funding_preimage_tx) {
            let reason = AbortReason::FailedToParseFundingSpendPreimg(format!("{:?}", e));
            return self.abort_to_funding_refund_required(reason, sm).await;
        }
        if let Err(e) = sm.taker_coin.parse_signature(&maker_payment_info.funding_preimage_sig) {
            let reason = AbortReason::FailedToParseFundingSpendSig(format!("{:?}", e));
            return self.abort_to_funding_refund_required(reason, sm).await;
        }

        let stored_preimage = StoredTxPreimage {
            preimage: maker_payment_info.funding_preimage_tx.clone().into(),
            signature: maker_payment_info.funding_preimage_sig.clone().into(),
        };

        Self::change_state(
            MakerPaymentAndFundingSpendPreimgReceived::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.taker_funding.clone(),
                stored_preimage,
                maker_payment_info.tx_bytes.into(),
            ),
            sm,
        )
        .await
    }
}

// MakerPaymentAndFundingSpendPreimgReceived → (various) -----------------

// Validate maker payment, validate funding spend preimage, optionally confirm,
// then spend funding to create taker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State
    for MakerPaymentAndFundingSpendPreimgReceived<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();

        let maker_payment_tx = match sm.maker_coin.parse_tx(&self.maker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToParseMakerPayment(format!("{:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_funding_tx = match sm.taker_coin.parse_tx(&self.taker_funding) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own funding tx: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };

        // Parse maker HTLC pubkeys for the validation call.
        let maker_maker_coin_pub = match sm.maker_coin.parse_pubkey(&self.negotiation_data.maker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's maker-coin pubkey: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let maker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's taker-coin pubkey: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };

        // Step 1: Validate maker payment (offline semantic validation).
        let validate_args = ValidateMakerPaymentArgs {
            maker_payment_tx: &maker_payment_tx,
            time_lock: self.negotiation_data.maker_payment_locktime,
            taker_secret_hash: &taker_secret_hash,
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
            amount: sm.maker_volume.to_decimal(),
            maker_pub: &maker_maker_coin_pub,
            swap_unique_data: &unique_data,
        };
        if let Err(e) = sm.maker_coin.validate_maker_payment_v2(validate_args).await {
            let reason = AbortReason::MakerPaymentValidationFailed(format!("{:?}", e));
            return self.abort_to_funding_refund_required(reason, sm).await;
        }

        // Derive taker's own taker-coin pubkey.
        let taker_taker_coin_pub_bytes = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason =
                    AbortReason::InternalError(format!("Failed to derive own taker-coin V2 HTLC pubkey: {}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&taker_taker_coin_pub_bytes) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own taker-coin pubkey: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };

        // Step 2: Validate funding spend preimage.
        let gen_funding_spend_args = GenTakerFundingSpendArgs {
            funding_tx: &taker_funding_tx,
            maker_pub: &maker_taker_coin_pub,
            taker_pub: &taker_taker_coin_pub,
            funding_time_lock: sm.taker_funding_locktime(),
            taker_secret_hash: &taker_secret_hash,
            taker_payment_time_lock: sm.taker_payment_locktime(),
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
        };

        let preimage = match sm.taker_coin.parse_preimage(&self.funding_spend_preimage.preimage) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::FailedToParseFundingSpendPreimg(format!("{:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let signature = match sm.taker_coin.parse_signature(&self.funding_spend_preimage.signature) {
            Ok(s) => s,
            Err(e) => {
                let reason = AbortReason::FailedToParseFundingSpendSig(format!("{:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let preimage_with_sig = TxPreimageWithSig { preimage, signature };

        if let Err(e) = sm
            .taker_coin
            .validate_taker_funding_spend_preimage(&gen_funding_spend_args, &preimage_with_sig)
            .await
        {
            let reason = AbortReason::FundingSpendPreimageValidationFailed(format!("{:?}", e));
            return self.abort_to_funding_refund_required(reason, sm).await;
        }

        // Step 3: Optional confirmation gate for maker payment.
        if sm.require_maker_payment_confirm {
            let confirm_result = sm
                .maker_coin
                .wait_for_confirmations(
                    &maker_payment_tx.tx_hex(),
                    confirmation_gate_confs(sm.conf_settings.maker_coin_confs),
                    sm.conf_settings.maker_coin_nota,
                    sm.maker_payment_conf_timeout(),
                    10,
                )
                .compat()
                .await;
            if let Err(e) = confirm_result {
                warn!("Swap {}: maker payment not confirmed in time: {}", sm.uuid, e);
                let reason = AbortReason::MakerPaymentNotConfirmedInTime(e.to_string());
                return self.abort_to_funding_refund_required(reason, sm).await;
            }
            info!("Swap {}: maker payment confirmed, proceeding to spend funding", sm.uuid);
        }

        // Step 4: Spend funding → create taker payment.
        let taker_payment_tx = match sm
            .taker_coin
            .sign_and_send_taker_funding_spend(&preimage_with_sig, &gen_funding_spend_args, &unique_data)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendPayment(format!("Failed to send taker payment: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_payment_bytes: BytesJson = taker_payment_tx.tx_hex().into();
        info!(
            "Taker swap {}: taker payment sent: {:?}",
            sm.uuid,
            taker_payment_tx.tx_hash()
        );

        if sm.taker_coin.skip_taker_payment_spend_preimage() {
            Self::change_state(
                TakerPaymentSentPreimageSendingSkipped::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    taker_payment_bytes,
                    self.maker_payment.clone(),
                ),
                sm,
            )
            .await
        } else {
            Self::change_state(
                TakerPaymentSent::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    taker_payment_bytes,
                    self.maker_payment.clone(),
                ),
                sm,
            )
            .await
        }
    }
}

// MakerPaymentConfirmed → TakerPaymentSent (deferred funding spend) -----

// Maker payment already confirmed in prior state; now spend funding.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for MakerPaymentConfirmed<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();

        let taker_funding_tx = match sm.taker_coin.parse_tx(&self.taker_funding) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own funding tx: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let maker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's taker-coin pubkey: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_taker_coin_pub_bytes = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason =
                    AbortReason::InternalError(format!("Failed to derive own taker-coin V2 HTLC pubkey: {}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&taker_taker_coin_pub_bytes) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own taker-coin pubkey: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };

        let gen_funding_spend_args = GenTakerFundingSpendArgs {
            funding_tx: &taker_funding_tx,
            maker_pub: &maker_taker_coin_pub,
            taker_pub: &taker_taker_coin_pub,
            funding_time_lock: sm.taker_funding_locktime(),
            taker_secret_hash: &taker_secret_hash,
            taker_payment_time_lock: sm.taker_payment_locktime(),
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
        };

        let preimage = match sm.taker_coin.parse_preimage(&self.funding_spend_preimage.preimage) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse preimage: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let signature = match sm.taker_coin.parse_signature(&self.funding_spend_preimage.signature) {
            Ok(s) => s,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse signature: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let preimage_with_sig = TxPreimageWithSig { preimage, signature };

        let taker_payment_tx = match sm
            .taker_coin
            .sign_and_send_taker_funding_spend(&preimage_with_sig, &gen_funding_spend_args, &unique_data)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendPayment(format!("Failed to send taker payment: {:?}", e));
                return self.abort_to_funding_refund_required(reason, sm).await;
            },
        };
        let taker_payment_bytes: BytesJson = taker_payment_tx.tx_hex().into();
        info!(
            "Taker swap {}: taker payment sent (post-confirm): {:?}",
            sm.uuid,
            taker_payment_tx.tx_hash()
        );

        if sm.taker_coin.skip_taker_payment_spend_preimage() {
            Self::change_state(
                TakerPaymentSentPreimageSendingSkipped::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    taker_payment_bytes,
                    self.maker_payment.clone(),
                ),
                sm,
            )
            .await
        } else {
            Self::change_state(
                TakerPaymentSent::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    taker_payment_bytes,
                    self.maker_payment.clone(),
                ),
                sm,
            )
            .await
        }
    }
}

// TakerPaymentSent → TakerPaymentSpent ----------------------------------

// Generate and broadcast taker payment spend preimage, then wait for maker
// to spend taker payment (revealing the maker secret).

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentSent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();

        let taker_payment_tx = match sm.taker_coin.parse_tx(&self.taker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own payment tx: {:?}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let maker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's taker-coin pubkey: {:?}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let taker_taker_coin_pub_bytes = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason =
                    AbortReason::InternalError(format!("Failed to derive own taker-coin V2 HTLC pubkey: {}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let taker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&taker_taker_coin_pub_bytes) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own taker-coin pubkey: {:?}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let maker_address = match sm.taker_coin.try_my_addr().await {
            Ok(address) => address,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &taker_taker_coin_pub_bytes,
        );

        let gen_spend_args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment_tx,
            time_lock: sm.taker_payment_locktime(),
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
            maker_pub: &maker_taker_coin_pub,
            maker_address: &maker_address,
            taker_pub: &taker_taker_coin_pub,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            dex_fee_addr_raw_pubkey: net_config.dex_fee_addr_raw_pubkey(),
        };

        // Generate taker payment spend preimage.
        let preimage_result = match sm
            .taker_coin
            .gen_taker_payment_spend_preimage(&gen_spend_args, &unique_data)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let reason = AbortReason::FailedToGenerateSpendPreimage(format!("{:?}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };

        // Broadcast preimage via P2P so maker can spend taker payment.
        let preimage_msg = SwapMessage {
            inner: Some(swap_message::Inner::TakerPaymentSpendPreimage(
                TakerPaymentSpendPreimage {
                    signature: preimage_result.signature.to_bytes().to_vec(),
                    tx_preimage: preimage_result.preimage.to_bytes().to_vec(),
                },
            )),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };
        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            preimage_msg,
            super::TX_INFO_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        // Wait for maker to spend taker payment (reveals maker secret).
        let wait_until = sm.taker_payment_locktime();
        match sm
            .taker_coin
            .find_taker_payment_spend_tx(&taker_payment_tx, self.maker_coin_start_block, wait_until)
            .await
        {
            Ok(spend_tx) => {
                let spend_tx_bytes: BytesJson = spend_tx.tx_hex().into();
                info!("Taker swap {}: taker payment spent by maker", sm.uuid);
                Self::change_state(
                    TakerPaymentSpent::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        spend_tx_bytes,
                        self.maker_payment.clone(),
                        self.negotiation_data.clone(),
                        self.taker_payment.clone(),
                    ),
                    sm,
                )
                .await
            },
            Err(e) => {
                warn!("Swap {}: maker did not spend taker payment in time: {:?}", sm.uuid, e);
                let reason = AbortReason::MakerDidNotSpendInTime(format!("{:?}", e));
                self.abort_to_payment_refund_required(reason, sm).await
            },
        }
    }
}

// TakerPaymentSentPreimageSendingSkipped → TakerPaymentSpent ------------

// EVM/contract coins: skip preimage, just poll for maker spend.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State
    for TakerPaymentSentPreimageSendingSkipped<M, T>
{
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        info!(
            "Taker swap {}: skipped generation of taker payment spend preimage (coin does not require it)",
            sm.uuid
        );

        let taker_payment_tx = match sm.taker_coin.parse_tx(&self.taker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own payment tx: {:?}", e));
                return self.abort_to_payment_refund_required(reason, sm).await;
            },
        };

        // Wait for maker to spend taker payment.
        let wait_until = sm.taker_payment_locktime();
        match sm
            .taker_coin
            .find_taker_payment_spend_tx(&taker_payment_tx, self.maker_coin_start_block, wait_until)
            .await
        {
            Ok(spend_tx) => {
                let spend_tx_bytes: BytesJson = spend_tx.tx_hex().into();
                info!(
                    "Taker swap {}: taker payment spent by maker (preimage skipped)",
                    sm.uuid
                );
                Self::change_state(
                    TakerPaymentSpent::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        spend_tx_bytes,
                        self.maker_payment.clone(),
                        self.negotiation_data.clone(),
                        self.taker_payment.clone(),
                    ),
                    sm,
                )
                .await
            },
            Err(e) => {
                warn!("Swap {}: maker did not spend taker payment in time: {:?}", sm.uuid, e);
                let reason = AbortReason::MakerDidNotSpendInTime(format!("{:?}", e));
                self.abort_to_payment_refund_required(reason, sm).await
            },
        }
    }
}

// TakerPaymentSpent → MakerPaymentSpent ---------------------------------

// Extract maker secret from taker payment spend tx, then spend maker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentSpent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();

        let spend_tx = match sm.taker_coin.parse_tx(&self.taker_payment_spend) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker payment spend tx: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Extract the maker secret that was revealed when maker spent the taker payment.
        let maker_secret = match sm
            .taker_coin
            .extract_secret_v2(&self.negotiation_data.maker_secret_hash, &spend_tx)
            .await
        {
            Ok(secret) => secret,
            Err(e) => {
                let reason = AbortReason::CouldNotExtractSecret(e.to_string());
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Now spend the maker payment using the extracted secret.
        let maker_payment_tx = match sm.maker_coin.parse_tx(&self.maker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker payment tx: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let maker_maker_coin_pub = match sm.maker_coin.parse_pubkey(&self.negotiation_data.maker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's maker-coin pubkey: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let spend_args = SpendMakerPaymentArgs {
            maker_payment_tx: &maker_payment_tx,
            time_lock: self.negotiation_data.maker_payment_locktime,
            taker_secret_hash: &taker_secret_hash,
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
            maker_secret,
            maker_pub: &maker_maker_coin_pub,
            swap_unique_data: &unique_data,
            amount: sm.maker_volume.to_decimal(),
        };

        let maker_payment_spend_tx = match sm.maker_coin.spend_maker_payment_v2(spend_args).await {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSpendMakerPayment(format!("{:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let maker_payment_spend_bytes: BytesJson = maker_payment_spend_tx.tx_hex().into();
        info!(
            "Taker swap {}: maker payment spent: {:?}",
            sm.uuid,
            maker_payment_spend_tx.tx_hash()
        );

        Self::change_state(
            MakerPaymentSpent::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                maker_payment_spend_bytes,
                self.negotiation_data.clone(),
                self.taker_payment.clone(),
            ),
            sm,
        )
        .await
    }
}

// MakerPaymentSpent → Completed -----------------------------------------

// Optionally wait for maker payment spend confirmation.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for MakerPaymentSpent<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        if sm.require_maker_payment_spend_confirm {
            let confirm_result = sm
                .maker_coin
                .wait_for_confirmations(
                    &self.maker_payment_spend,
                    confirmation_gate_confs(sm.conf_settings.maker_coin_confs),
                    sm.conf_settings.maker_coin_nota,
                    sm.taker_payment_locktime(),
                    10,
                )
                .compat()
                .await;
            if let Err(e) = confirm_result {
                warn!("Swap {}: maker payment spend not confirmed in time: {}", sm.uuid, e);
                let reason = AbortReason::MakerPaymentSpendNotConfirmedInTime(e.to_string());
                return Self::change_state(
                    TakerPaymentRefundRequired::new(self.taker_payment.clone(), self.negotiation_data.clone(), reason),
                    sm,
                )
                .await;
            }
        }
        info!("Taker swap {} completed successfully", sm.uuid);
        Self::change_state(Completed::new(), sm).await
    }
}

// TakerFundingRefundRequired → TakerFundingRefunded ---------------------

// Refund taker funding using taker's secret (no timelock needed).

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerFundingRefundRequired<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();

        let funding_tx = match sm.taker_coin.parse_tx(&self.taker_funding) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse own funding tx for refund: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let maker_taker_coin_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse maker's taker-coin pubkey: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let taker_coin_htlc_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &taker_coin_htlc_pub,
        );

        let refund_args = RefundFundingSecretArgs {
            funding_tx: &funding_tx,
            funding_time_lock: sm.taker_funding_locktime(),
            payment_time_lock: sm.taker_payment_locktime(),
            maker_pubkey: &maker_taker_coin_pub,
            taker_secret: &sm.taker_secret,
            taker_secret_hash: &taker_secret_hash,
            maker_secret_hash: &self.negotiation_data.maker_secret_hash,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            swap_unique_data: &unique_data,
            watcher_reward: false,
        };

        match sm.taker_coin.refund_taker_funding_secret(refund_args).await {
            Ok(refund_tx) => {
                let refund_tx_bytes: BytesJson = refund_tx.tx_hex().into();
                info!("Taker swap {}: funding refunded: {:?}", sm.uuid, refund_tx.tx_hash());
                Self::change_state(
                    TakerFundingRefunded::new(self.taker_funding.clone(), refund_tx_bytes, self.reason.clone()),
                    sm,
                )
                .await
            },
            Err(e) => {
                error!("Swap {}: failed to refund taker funding: {:?}", sm.uuid, e);
                let reason = AbortReason::TakerFundingRefundFailed(format!("{:?}", e));
                Self::change_state(Aborted::new(reason), sm).await
            },
        }
    }
}

// TakerPaymentRefundRequired → TakerPaymentRefunded ---------------------

// Wait for timelock, then refund taker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentRefundRequired<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        let taker_secret_hash = sm.taker_secret_hash();

        // Wait until we can refund.
        loop {
            match sm
                .taker_coin
                .can_refund_htlc(sm.taker_payment_locktime())
                .compat()
                .await
            {
                Ok(CanRefundHtlc::CanRefundNow) => break,
                Ok(CanRefundHtlc::HaveToWait(secs)) => {
                    info!(
                        "Swap {}: waiting {} seconds for taker payment refund window",
                        sm.uuid, secs
                    );
                    Timer::sleep(secs as f64).await;
                },
                Err(e) => {
                    error!("Swap {}: can_refund_htlc error: {}, retrying in 30s", sm.uuid, e);
                    Timer::sleep(30.).await;
                },
            }
        }

        let taker_coin_htlc_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2_bytes(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &taker_coin_htlc_pub,
        );
        let refund_args = RefundTakerPaymentArgs {
            payment_tx: &self.taker_payment,
            time_lock: sm.taker_payment_locktime(),
            maker_pub: &self.negotiation_data.taker_coin_htlc_pub,
            tx_type_with_secret_hash: SwapTxTypeWithSecretHash::TakerPaymentV2 {
                maker_secret_hash: &self.negotiation_data.maker_secret_hash,
                taker_secret_hash: &taker_secret_hash,
            },
            swap_unique_data: &unique_data,
            watcher_reward: false,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
        };

        match sm.taker_coin.refund_combined_taker_payment(refund_args).await {
            Ok(refund_tx) => {
                let refund_tx_bytes: BytesJson = refund_tx.tx_hex().into();
                info!(
                    "Taker swap {}: taker payment refunded: {:?}",
                    sm.uuid,
                    refund_tx.tx_hash()
                );
                Self::change_state(
                    TakerPaymentRefunded::new(self.taker_payment.clone(), refund_tx_bytes, self.reason.clone()),
                    sm,
                )
                .await
            },
            Err(e) => {
                error!("Swap {}: failed to refund taker payment: {:?}", sm.uuid, e);
                let reason = AbortReason::TakerPaymentRefundFailed(format!("{:?}", e));
                Self::change_state(Aborted::new(reason), sm).await
            },
        }
    }
}

// Terminal states -------------------------------------------------------

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for TakerFundingRefunded<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        info!("Taker swap {} ended: funding refunded ({})", sm.uuid, self.reason);
    }
}
#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for TakerPaymentRefunded<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        info!("Taker swap {} ended: taker payment refunded ({})", sm.uuid, self.reason);
    }
}
#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for Completed<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        info!("Taker swap {} completed successfully", sm.uuid);
    }
}
#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for Aborted<M, T> {
    type StateMachine = TakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        error!("Taker swap {} aborted: {}", sm.uuid, self.reason);
    }
}

// Regression tests for CRD ch.52 D8 ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // `EthCoin` is used only as a compile-time type parameter (the states carry no
    // coin-typed field, only `PhantomData<(M, T)>`): it is real production code that
    // implements `MmCoin + MakerCoinSwapOpsV2 + TakerCoinSwapOpsV2`, but no coin
    // instance, network access, or mocking is needed to exercise `new`/`get_event`.
    use coins::eth::EthCoin;

    fn negotiation_data_for_test() -> StoredTakerNegotiationData {
        StoredTakerNegotiationData {
            maker_secret_hash: BytesJson::from(vec![1, 2]),
            maker_coin_htlc_pub: BytesJson::from(vec![3, 4]),
            taker_coin_htlc_pub: BytesJson::from(vec![5, 6]),
            maker_coin_swap_contract: None,
            taker_coin_swap_contract: None,
            maker_payment_locktime: 3000,
            taker_coin_address: "Raddress".into(),
        }
    }

    /// CRD ch.52 D8, state-field-scope half: `TakerPaymentSpent` and `MakerPaymentSpent`
    /// must carry the real `taker_payment` bytes forward, not drop them. Before the fix,
    /// neither state (nor `TakerSwapEvent::TakerPaymentSpent`/`::MakerPaymentSpent`) had a
    /// `taker_payment` field at all, so this test would not have compiled against the
    /// pre-fix code -- the strongest possible "fails before, passes after".
    #[test]
    fn taker_payment_survives_taker_payment_spent_and_maker_payment_spent_events() {
        let negotiation_data = negotiation_data_for_test();
        let real_taker_payment = BytesJson::from(vec![0xEE, 0xAD, 0xBE, 0xEF]);

        let taker_payment_spent = TakerPaymentSpent::<EthCoin, EthCoin>::new(
            100,
            200,
            BytesJson::from(vec![0x11]), // taker_payment_spend
            BytesJson::from(vec![0xBB]), // maker_payment
            negotiation_data.clone(),
            real_taker_payment.clone(),
        );
        match taker_payment_spent.get_event() {
            TakerSwapEvent::TakerPaymentSpent { taker_payment, .. } => {
                assert_eq!(taker_payment, real_taker_payment);
            },
            other => panic!("unexpected event: {:?}", other),
        }

        let maker_payment_spent = MakerPaymentSpent::<EthCoin, EthCoin>::new(
            100,
            200,
            BytesJson::from(vec![0x22]), // maker_payment_spend
            negotiation_data,
            real_taker_payment.clone(),
        );
        match maker_payment_spent.get_event() {
            TakerSwapEvent::MakerPaymentSpent { taker_payment, .. } => {
                assert_eq!(
                    taker_payment, real_taker_payment,
                    "MakerPaymentSpent must retain the real taker_payment bytes (CRD ch.52 D8)"
                );
            },
            other => panic!("unexpected event: {:?}", other),
        }
    }

    /// CRD ch.52 D8, call-site half: `MakerPaymentSpent::on_changed`'s confirmation-timeout
    /// abort path must forward the real `self.taker_payment` to `TakerPaymentRefundRequired`,
    /// not the removed `BytesJson::default()` fallback. `on_changed` itself needs a live,
    /// confirmation-failing coin to actually reach that branch, which this codebase has no
    /// mocking infrastructure for on the V2 swap-ops traits (`MakerCoinSwapOpsV2` /
    /// `TakerCoinSwapOpsV2` have no test double anywhere in the tree); this checks the fixed
    /// call site directly instead, the same way `dex_fee.rs`'s `t16_4a`/`t16_4b` verify other
    /// call-site shapes via `include_str!`. Before the fix, `BytesJson::default()` appeared
    /// in this file exactly once, at this call site, so the first assertion alone would have
    /// failed against the pre-fix code.
    #[test]
    fn maker_payment_spent_confirmation_timeout_no_longer_falls_back_to_empty_bytes() {
        let whole_file = include_str!("taker_swap_v2.rs");
        // Only inspect the production code above this very test module -- otherwise this
        // assertion (and its own doc comment, which names the removed fallback call) would
        // trivially fail against itself once inlined by `include_str!`.
        let mod_tests_pos = whole_file.find("mod tests {").expect("this test module should exist");
        let source = &whole_file[..mod_tests_pos];
        assert!(
            !source.contains("BytesJson::default()"),
            "MakerPaymentSpent::on_changed's confirmation-timeout abort must not fall back \
             to empty taker_payment bytes (CRD ch.52 D8)"
        );

        let marker = "MakerPaymentSpendNotConfirmedInTime";
        let marker_pos = source.find(marker).expect("reason variant should still be used here");
        let window = &source[marker_pos..(marker_pos + 300).min(source.len())];
        assert!(
            window.contains("self.taker_payment"),
            "TakerPaymentRefundRequired::new(...) after a maker-payment-spend confirmation \
             timeout must be constructed from self.taker_payment"
        );
    }
}
