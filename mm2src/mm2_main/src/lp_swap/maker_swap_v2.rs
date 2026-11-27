//! # Purpose
//! Drives the maker side of an atomic-swap V2 trade as a persistent
//! state machine. The maker initiates negotiation, receives the
//! taker funding tx, broadcasts its own payment, generates the
//! funding-spend preimage, and ultimately spends the taker payment.
//!
//! # Public exports
//! - [`MakerSwapEvent`] — the persisted event variants
//! - [`MakerSwapDbRepr`] — DB row + replayable event log
//! - [`MakerSwapStateMachine`] — the state-machine driver
//! - [`SerializableKeypairBytes`] — wire-format wrapper around
//!   `[u8; 32]` for the optional p2p keypair
//! - [`maker_swap_v2_kickstart`] (via `swap_v2_common`) — entry point
//!   used by the recovery loop
//!
//! # Invariants
//! - Persisted state-machine variant names (the `MakerSwapEvent`
//!   discriminants and the `Stored*NegotiationData` field names) are
//!   serde-stable; never rename without a migration path.
//! - Happy-path order:
//!   `Initialize → Initialized → WaitingForTakerFunding →
//!    TakerFundingReceived →
//!    MakerPaymentSentFundingSpendGenerated →
//!    TakerPaymentReceived → TakerPaymentSpent → Completed`.
//! - Error path: `MakerPaymentRefundRequired → MakerPaymentRefunded`.
//! - Abort path: any pre-payment state → `Aborted`.
//! - Started-at clock skew between maker and taker must not exceed
//!   [`MAX_STARTED_AT_DIFF`] seconds.

use coins::swap_evidence::is_undetermined;
use coins::{CanRefundHtlc, FeeApproxStage, FundingTxSpend, MakerCoinSwapOpsV2, MmCoin, RefundMakerPaymentTimelockArgs,
            SearchForFundingSpendErr, SendMakerPaymentArgs, SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, ToBytes,
            TradePreimageValue, Transaction, ValidateTakerFundingArgs};
use common::executor::Timer;
use common::log::{error, info, warn};
use common::mm_number::MmNumber;
use common::now_ms;
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

use super::evidence_wait::{recovery_unanswered_text, Read};
use super::swap_lock::SwapLock;
use super::swap_v2_common::*;
use super::swap_v2_pb::*;
use super::SwapConfirmationsSettings;

/// Maximum allowed difference between maker and taker `started_at` timestamps (seconds).
const MAX_STARTED_AT_DIFF: u64 = 60;

// Events (persisted to DB for recovery) --------------------------------------

/// Every state transition emits one of these events, which is appended to the
/// swap's event log in storage.  On recovery, events are replayed to reach the
/// last known state.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum MakerSwapEvent {
    /// Swap initialisation completed; trade fees estimated.
    Initialized {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment_trade_fee: MmNumber,
        taker_payment_spend_trade_fee: MmNumber,
        /// The part of `taker_payment_spend_trade_fee` that must stay free in
        /// our own balance — zero when the taker coin takes the fee out of the
        /// payment we are claiming (CRD ch.52 R64). The coin that pays it is not
        /// persisted: it is that coin's platform ticker, which does not vary.
        ///
        /// Additive and defaulted: a log written before this field existed
        /// resumes with no headroom rather than failing to parse.
        #[serde(default)]
        taker_payment_spend_headroom: MmNumber,
        /// The reservable part of `maker_payment_trade_fee` (CRD ch.52 R58,
        /// R62): the whole amount unless the maker coin marks it payable out of
        /// the trading volume, in which case zero. The coin that pays it is not
        /// persisted — it is `maker_coin`'s platform ticker, which does not
        /// vary — but for a log written before this field existed there is no
        /// way to recover whether the marker was set, so a `None` falls back to
        /// reserving the full legacy amount (which was always correct in *how
        /// much*, only ever wrong in *which coin's bucket*).
        #[serde(default)]
        maker_payment_trade_fee_reservable: Option<MmNumber>,
    },
    /// Waiting for taker to send funding; negotiation data exchanged.
    WaitingForTakerFunding {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment_trade_fee: MmNumber,
    },
    /// Taker funding transaction received and validated.
    TakerFundingReceived {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        taker_funding: BytesJson,
        maker_payment_trade_fee: MmNumber,
    },
    /// Maker payment broadcast and funding-spend preimage generated.
    MakerPaymentSentFundingSpendGenerated {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_funding: BytesJson,
        funding_spend_preimage: StoredTxPreimage,
    },
    /// Maker payment needs to be refunded (error recovery).
    MakerPaymentRefundRequired {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        reason: AbortReason,
    },
    /// Maker payment was successfully refunded.
    MakerPaymentRefunded {
        maker_payment: BytesJson,
        maker_payment_refund: BytesJson,
        reason: AbortReason,
    },
    /// Taker payment transaction received and validated.
    TakerPaymentReceived {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
    },
    /// Like TakerPaymentReceived but taker sends no spend preimage (EVM coins).
    TakerPaymentReceivedPreimageSkipped {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
    },
    /// Taker payment was successfully spent (happy path near-completion).
    TakerPaymentSpent {
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
        taker_payment_spend: BytesJson,
        negotiation_data: StoredMakerNegotiationData,
    },
    /// Swap aborted before maker payment was sent.
    Aborted { reason: AbortReason },
    /// Swap completed successfully.
    Completed,
}

// Database representation ----------------------------------------------------

/// Serialisable representation of the maker swap stored in the DB.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MakerSwapDbRepr {
    pub maker_coin: String,
    pub maker_volume: MmNumber,
    pub maker_secret: H256Json,
    pub maker_secret_hash: BytesJson,
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
    pub p2p_keypair: Option<SerializableKeypairBytes>,
    pub events: Vec<MakerSwapEvent>,
    pub taker_p2p_pub: BytesJson,
    pub swap_version: u8,
}

/// Opaque serialized P2P keypair bytes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializableKeypairBytes(pub Vec<u8>);

// State machine --------------------------------------------------------------

pub struct MakerSwapStateMachine<MakerCoin: MmCoin + MakerCoinSwapOpsV2, TakerCoin: MmCoin + TakerCoinSwapOpsV2> {
    pub ctx: MmArc,
    pub storage: MakerSwapStorage,
    pub maker_coin: MakerCoin,
    pub maker_volume: MmNumber,
    pub secret: primitives::hash::H256,
    pub secret_hash_algo: SecretHashAlgo,
    pub started_at: u64,
    pub lock_duration: u64,
    pub taker_coin: TakerCoin,
    pub taker_volume: MmNumber,
    pub taker_premium: MmNumber,
    pub conf_settings: SwapConfirmationsSettings,
    pub uuid: Uuid,
    pub p2p_topic: String,
    pub p2p_keypair: Option<KeyPair>,
    pub taker_p2p_pubkey: Vec<u8>,
    pub require_taker_funding_confirm: bool,
    pub require_taker_payment_spend_confirm: bool,
    pub swap_version: u8,
    /// Taker-coin balance to keep free for spending the taker payment (ch.52
    /// R64). Learned from the `Initialized` event — on a live run when it is
    /// applied, on a resume by reading it back out of the persisted log — so
    /// that a swap resumed past initialisation still holds its headroom.
    pub taker_payment_spend_headroom: MmNumber,
    /// The reservable part of the fee to send the maker payment (ch.52 R58,
    /// R62). Populated the same way as `taker_payment_spend_headroom` — from
    /// the `Initialized` event, live or read back on resume — because
    /// `WaitingForTakerFunding` and `TakerFundingReceived`, which also
    /// re-create this reservation on resume (R63), do not themselves carry it.
    pub maker_payment_trade_fee_reservable: MmNumber,
}

impl<MakerCoin, TakerCoin> MakerSwapStateMachine<MakerCoin, TakerCoin>
where
    MakerCoin: MmCoin + MakerCoinSwapOpsV2,
    TakerCoin: MmCoin + TakerCoinSwapOpsV2,
{
    pub fn taker_payment_conf_timeout(&self) -> u64 { self.started_at + self.lock_duration * 2 / 3 }

    pub fn maker_payment_locktime(&self) -> u64 { self.started_at + 2 * self.lock_duration }

    pub fn secret_hash(&self) -> Vec<u8> { self.secret_hash_algo.hash_secret(self.secret.as_slice()) }

    pub fn unique_data(&self) -> Vec<u8> { self.secret_hash() }

    /// The coin whose balance pays for spending the taker payment. For an ERC20
    /// taker coin that is the platform coin, not the token — the same choice the
    /// coin makes when it reports the fee.
    fn taker_payment_spend_fee_coin(&self) -> &str { self.taker_coin.platform_ticker() }

    fn reserve_taker_payment_spend_headroom(&self) {
        super::reserve_v2_spend_headroom(
            &self.ctx,
            self.uuid,
            self.taker_payment_spend_fee_coin(),
            self.taker_payment_spend_headroom.clone(),
        );
    }

    fn release_taker_payment_spend_headroom(&self) {
        super::release_v2_spend_headroom(&self.ctx, &self.uuid, self.taker_payment_spend_fee_coin());
    }

    /// The coin whose balance pays for sending the maker payment. For an ERC20
    /// maker coin that is the platform coin, not the token — the same choice
    /// the coin makes when it reports the fee (ch.52 R58, R62).
    fn maker_payment_trade_fee_coin(&self) -> &str { self.maker_coin.platform_ticker() }

    /// Reserve both halves of the short-lived (R58) reservation: the maker's
    /// own outgoing volume, and the fee to send it — which may land in the same
    /// bucket as the volume or, for a token maker coin, in a different one.
    /// Both share the `Volume` kind and are released together by
    /// `release_maker_payment_volume`.
    fn reserve_maker_payment_volume(&self) {
        super::reserve_v2_amount(
            &self.ctx,
            self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.maker_coin.ticker(),
            self.maker_volume.clone(),
        );
        super::reserve_v2_amount(
            &self.ctx,
            self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.maker_payment_trade_fee_coin(),
            self.maker_payment_trade_fee_reservable.clone(),
        );
    }

    /// Release both halves reserved by `reserve_maker_payment_volume`, in
    /// whichever bucket(s) they ended up in.
    fn release_maker_payment_volume(&self) {
        super::release_v2_amount(
            &self.ctx,
            &self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.maker_coin.ticker(),
        );
        super::release_v2_amount(
            &self.ctx,
            &self.uuid,
            super::LockedAmountV2Kind::Volume,
            self.maker_payment_trade_fee_coin(),
        );
    }
}

// States — each carries PhantomData to bind to the generic state machine -----

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
    pub maker_payment_trade_fee: MmNumber,
    pub taker_payment_spend_trade_fee: MmNumber,
    pub taker_payment_spend_headroom: MmNumber,
    pub maker_payment_trade_fee_reservable: Option<MmNumber>,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Initialized<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment_trade_fee: MmNumber,
        taker_payment_spend_trade_fee: MmNumber,
        taker_payment_spend_headroom: MmNumber,
        maker_payment_trade_fee_reservable: Option<MmNumber>,
    ) -> Self {
        Initialized {
            maker_coin_start_block,
            taker_coin_start_block,
            maker_payment_trade_fee,
            taker_payment_spend_trade_fee,
            taker_payment_spend_headroom,
            maker_payment_trade_fee_reservable,
            _p: PhantomData,
        }
    }
}

pub struct WaitingForTakerFunding<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub maker_payment_trade_fee: MmNumber,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> WaitingForTakerFunding<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment_trade_fee: MmNumber,
    ) -> Self {
        WaitingForTakerFunding {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment_trade_fee,
            _p: PhantomData,
        }
    }
}

pub struct TakerFundingReceived<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub taker_funding: BytesJson,
    pub maker_payment_trade_fee: MmNumber,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerFundingReceived<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        taker_funding: BytesJson,
        maker_payment_trade_fee: MmNumber,
    ) -> Self {
        TakerFundingReceived {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            taker_funding,
            maker_payment_trade_fee,
            _p: PhantomData,
        }
    }
}

pub struct MakerPaymentSentFundingSpendGenerated<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub maker_payment: BytesJson,
    pub taker_funding: BytesJson,
    pub funding_spend_preimage: StoredTxPreimage,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentSentFundingSpendGenerated<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_funding: BytesJson,
        funding_spend_preimage: StoredTxPreimage,
    ) -> Self {
        MakerPaymentSentFundingSpendGenerated {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment,
            taker_funding,
            funding_spend_preimage,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `MakerPaymentRefundRequired` and differ only in `reason`.
    async fn abort_to_maker_payment_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut MakerSwapStateMachine<M, T>,
    ) -> StateResult<MakerSwapStateMachine<M, T>> {
        Self::change_state(
            MakerPaymentRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.maker_payment.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentReceived<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub maker_payment: BytesJson,
    pub taker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentReceived<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
    ) -> Self {
        TakerPaymentReceived {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment,
            taker_payment,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `MakerPaymentRefundRequired` and differ only in `reason`.
    async fn abort_to_maker_payment_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut MakerSwapStateMachine<M, T>,
    ) -> StateResult<MakerSwapStateMachine<M, T>> {
        Self::change_state(
            MakerPaymentRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.maker_payment.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentReceivedPreimageSkipped<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub maker_payment: BytesJson,
    pub taker_payment: BytesJson,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentReceivedPreimageSkipped<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
    ) -> Self {
        TakerPaymentReceivedPreimageSkipped {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment,
            taker_payment,
            _p: PhantomData,
        }
    }

    /// Shared body for this state's `on_changed` abort paths: they all carry the same
    /// fields forward into `MakerPaymentRefundRequired` and differ only in `reason`.
    async fn abort_to_maker_payment_refund_required(
        &self,
        reason: AbortReason,
        sm: &mut MakerSwapStateMachine<M, T>,
    ) -> StateResult<MakerSwapStateMachine<M, T>> {
        Self::change_state(
            MakerPaymentRefundRequired::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                self.maker_payment.clone(),
                reason,
            ),
            sm,
        )
        .await
    }
}

pub struct TakerPaymentSpent<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub maker_payment: BytesJson,
    pub taker_payment: BytesJson,
    pub taker_payment_spend: BytesJson,
    pub negotiation_data: StoredMakerNegotiationData,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TakerPaymentSpent<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment: BytesJson,
        taker_payment: BytesJson,
        taker_payment_spend: BytesJson,
        negotiation_data: StoredMakerNegotiationData,
    ) -> Self {
        TakerPaymentSpent {
            maker_coin_start_block,
            taker_coin_start_block,
            maker_payment,
            taker_payment,
            taker_payment_spend,
            negotiation_data,
            _p: PhantomData,
        }
    }
}

pub struct MakerPaymentRefundRequired<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_coin_start_block: u64,
    pub taker_coin_start_block: u64,
    pub negotiation_data: StoredMakerNegotiationData,
    pub maker_payment: BytesJson,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentRefundRequired<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        negotiation_data: StoredMakerNegotiationData,
        maker_payment: BytesJson,
        reason: AbortReason,
    ) -> Self {
        MakerPaymentRefundRequired {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment,
            reason,
            _p: PhantomData,
        }
    }
}

pub struct MakerPaymentRefunded<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> {
    pub maker_payment: BytesJson,
    pub maker_payment_refund: BytesJson,
    pub reason: AbortReason,
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentRefunded<M, T> {
    pub fn new(maker_payment: BytesJson, maker_payment_refund: BytesJson, reason: AbortReason) -> Self {
        MakerPaymentRefunded {
            maker_payment,
            maker_payment_refund,
            reason,
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
    for WaitingForTakerFunding<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<Initialized<M, T>>
    for Aborted<M, T>
{
}

// WaitingForTakerFunding →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<WaitingForTakerFunding<M, T>>
    for TakerFundingReceived<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<WaitingForTakerFunding<M, T>>
    for Aborted<M, T>
{
}

// TakerFundingReceived →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingReceived<M, T>>
    for MakerPaymentSentFundingSpendGenerated<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerFundingReceived<M, T>>
    for Aborted<M, T>
{
}

// MakerPaymentSentFundingSpendGenerated →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentSentFundingSpendGenerated<M, T>> for TakerPaymentReceived<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentSentFundingSpendGenerated<M, T>> for TakerPaymentReceivedPreimageSkipped<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentSentFundingSpendGenerated<M, T>> for MakerPaymentRefundRequired<M, T>
{
}
// A search for the funding spend that stays undetermined until the bound of its wait ends the swap.
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<MakerPaymentSentFundingSpendGenerated<M, T>> for Aborted<M, T>
{
}

// TakerPaymentReceived →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentReceived<M, T>>
    for TakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentReceived<M, T>>
    for MakerPaymentRefundRequired<M, T>
{
}
// A confirmation read that stays undetermined until the bound of its wait ends the swap.
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentReceived<M, T>>
    for Aborted<M, T>
{
}

// TakerPaymentReceivedPreimageSkipped →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<TakerPaymentReceivedPreimageSkipped<M, T>> for TakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<TakerPaymentReceivedPreimageSkipped<M, T>> for MakerPaymentRefundRequired<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>
    TransitionFrom<TakerPaymentReceivedPreimageSkipped<M, T>> for Aborted<M, T>
{
}

// TakerPaymentSpent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for Completed<M, T>
{
}
// The claim of the taker payment is broadcast in this state, so it has no transition to a refund state.
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for Aborted<M, T>
{
}

// MakerPaymentRefundRequired →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentRefundRequired<M, T>>
    for MakerPaymentRefunded<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentRefundRequired<M, T>>
    for Aborted<M, T>
{
}
// The look before the refund found the maker's own claim of the taker payment on chain (CRD ch.51 R82 item 1).
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<MakerPaymentRefundRequired<M, T>>
    for TakerPaymentSpent<M, T>
{
}

// StorableStateMachine implementation ----------------------------------------

const MAKER_SWAP_LOCK_TTL: f64 = 120.0;
const MAKER_SWAP_LOCK_RENEW_INTERVAL: f64 = 30.0;

#[async_trait::async_trait]
impl<MakerCoin, TakerCoin> StorableStateMachine for MakerSwapStateMachine<MakerCoin, TakerCoin>
where
    MakerCoin: MmCoin + MakerCoinSwapOpsV2,
    TakerCoin: MmCoin + TakerCoinSwapOpsV2,
{
    type Storage = MakerSwapStorage;
    type Result = ();
    type Error = MmError<SwapStateMachineError>;
    type ReentrancyLock = SwapLock;
    type RecreateCtx = SwapRecreateCtx<MakerCoin, TakerCoin>;
    type RecreateError = MmError<SwapRecreateError>;

    fn to_db_repr(&self) -> MakerSwapDbRepr {
        MakerSwapDbRepr {
            maker_coin: self.maker_coin.ticker().into(),
            maker_volume: self.maker_volume.clone(),
            maker_secret: self.secret.into(),
            maker_secret_hash: self.secret_hash().into(),
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
                .map(|kp| SerializableKeypairBytes(kp.private().secret.to_vec())),
            events: Vec::new(),
            taker_p2p_pub: BytesJson::from(self.taker_p2p_pubkey.clone()),
            swap_version: self.swap_version,
        }
    }

    fn storage(&mut self) -> &mut Self::Storage { &mut self.storage }

    fn id(&self) -> <Self::Storage as StateMachineStorage>::MachineId { self.uuid }

    async fn recreate_machine(
        uuid: Uuid,
        storage: MakerSwapStorage,
        mut repr: MakerSwapDbRepr,
        recreate_ctx: Self::RecreateCtx,
    ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Self::RecreateError> {
        if repr.events.is_empty() {
            return MmError::err(SwapRecreateError::NoEvents);
        }

        let last_event = repr.events.remove(repr.events.len() - 1);

        // Only `Initialized` records the headroom, but the swap owes it for as
        // long as the taker payment is unspent, so recover it from the log
        // rather than from whichever event we happen to be resuming at.
        let (taker_payment_spend_headroom, maker_payment_trade_fee_reservable) = repr
            .events
            .iter()
            .chain(std::iter::once(&last_event))
            .find_map(|event| match event {
                MakerSwapEvent::Initialized {
                    maker_payment_trade_fee,
                    taker_payment_spend_headroom,
                    maker_payment_trade_fee_reservable,
                    ..
                } => Some((
                    taker_payment_spend_headroom.clone(),
                    // A log written before this field existed has no way to say
                    // whether the fee was payable out of the trading volume, so
                    // fall back to the legacy assumption that it was not — the
                    // full amount, which was always the correct *quantity* for
                    // every coin family implementing V2 today, only ever wrong
                    // in *which coin's bucket* it belonged to.
                    maker_payment_trade_fee_reservable
                        .clone()
                        .unwrap_or_else(|| maker_payment_trade_fee.clone()),
                )),
                _ => None,
            })
            .unwrap_or_default();

        let current_state: Box<dyn RestoredState<StateMachine = Self>> = match last_event {
            MakerSwapEvent::Initialized {
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee,
                taker_payment_spend_trade_fee,
                taker_payment_spend_headroom,
                maker_payment_trade_fee_reservable,
            } => Box::new(Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee,
                taker_payment_spend_trade_fee,
                taker_payment_spend_headroom,
                maker_payment_trade_fee_reservable,
            )),
            MakerSwapEvent::WaitingForTakerFunding {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment_trade_fee,
            } => Box::new(WaitingForTakerFunding::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment_trade_fee,
            )),
            MakerSwapEvent::TakerFundingReceived {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                maker_payment_trade_fee,
            } => Box::new(TakerFundingReceived::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                taker_funding,
                maker_payment_trade_fee,
            )),
            MakerSwapEvent::MakerPaymentSentFundingSpendGenerated {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_funding,
                funding_spend_preimage,
            } => Box::new(MakerPaymentSentFundingSpendGenerated::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_funding,
                funding_spend_preimage,
            )),
            MakerSwapEvent::MakerPaymentRefundRequired {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                reason,
            } => Box::new(MakerPaymentRefundRequired::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                reason,
            )),
            MakerSwapEvent::MakerPaymentRefunded {
                maker_payment,
                maker_payment_refund,
                reason,
            } => Box::new(MakerPaymentRefunded::new(maker_payment, maker_payment_refund, reason)),
            MakerSwapEvent::TakerPaymentReceived {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_payment,
            } => Box::new(TakerPaymentReceived::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_payment,
            )),
            MakerSwapEvent::TakerPaymentReceivedPreimageSkipped {
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_payment,
            } => Box::new(TakerPaymentReceivedPreimageSkipped::new(
                maker_coin_start_block,
                taker_coin_start_block,
                negotiation_data,
                maker_payment,
                taker_payment,
            )),
            MakerSwapEvent::TakerPaymentSpent {
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment,
                taker_payment,
                taker_payment_spend,
                negotiation_data,
            } => Box::new(TakerPaymentSpent::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment,
                taker_payment,
                taker_payment_spend,
                negotiation_data,
            )),
            MakerSwapEvent::Aborted { .. } => {
                return MmError::err(SwapRecreateError::Internal("Cannot recreate from Aborted".into()))
            },
            MakerSwapEvent::Completed => {
                return MmError::err(SwapRecreateError::Internal("Cannot recreate from Completed".into()))
            },
        };

        let p2p_topic = super::swap_v2_topic(&uuid);
        let machine = MakerSwapStateMachine {
            ctx: storage.get_ctx(),
            storage,
            maker_coin: recreate_ctx.maker_coin,
            maker_volume: repr.maker_volume,
            secret: repr.maker_secret.into(),
            secret_hash_algo: repr.secret_hash_algo,
            started_at: repr.started_at,
            lock_duration: repr.lock_duration,
            taker_coin: recreate_ctx.taker_coin,
            taker_volume: repr.taker_volume,
            taker_premium: repr.taker_premium,
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
            taker_p2p_pubkey: repr.taker_p2p_pub.into(),
            require_taker_funding_confirm: false,
            require_taker_payment_spend_confirm: true,
            swap_version: repr.swap_version,
            taker_payment_spend_headroom,
            maker_payment_trade_fee_reservable,
        };

        Ok((RestoredMachine::new(machine), current_state))
    }

    async fn acquire_reentrancy_lock(&self) -> Result<Self::ReentrancyLock, Self::Error> {
        acquire_reentrancy_lock_impl(&self.ctx, self.uuid, MAKER_SWAP_LOCK_TTL).await
    }

    fn spawn_reentrancy_lock_renew(&mut self, guard: Self::ReentrancyLock) {
        spawn_reentrancy_lock_renew(guard, MAKER_SWAP_LOCK_RENEW_INTERVAL);
    }

    fn init_additional_context(&mut self) {
        let swap_info = ActiveSwapV2Info {
            uuid: self.uuid,
            maker_coin: self.maker_coin.ticker().into(),
            taker_coin: self.taker_coin.ticker().into(),
            swap_type: SwapV2Type::MakerV2,
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
        // `MakerSwapStateMachine` is only ever built by `recreate_machine` — and
        // `recreate_machine` does not itself validate `taker_p2p_pub` before
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
        let accept_from = secp256k1::PublicKey::from_slice(&self.taker_p2p_pubkey)
            .expect("taker_p2p_pubkey must be a valid 33-byte compressed pubkey");
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

    fn on_event(&mut self, event: &MakerSwapEvent) {
        match event {
            MakerSwapEvent::Initialized {
                maker_payment_trade_fee,
                taker_payment_spend_headroom,
                maker_payment_trade_fee_reservable,
                ..
            } => {
                self.maker_payment_trade_fee_reservable = maker_payment_trade_fee_reservable
                    .clone()
                    .unwrap_or_else(|| maker_payment_trade_fee.clone());
                self.reserve_maker_payment_volume();

                self.taker_payment_spend_headroom = taker_payment_spend_headroom.clone();
                self.reserve_taker_payment_spend_headroom();
            },
            // Both halves of R58's reservation die together: the payment being
            // broadcast is what both the volume and its send fee were held
            // against.
            MakerSwapEvent::MakerPaymentSentFundingSpendGenerated { .. } => self.release_maker_payment_volume(),
            // The taker payment is ours now, so the fee to claim it is no longer
            // owed and its headroom is released (ch.52 R64).
            MakerSwapEvent::TakerPaymentSpent { .. } => self.release_taker_payment_spend_headroom(),
            MakerSwapEvent::WaitingForTakerFunding { .. }
            | MakerSwapEvent::TakerFundingReceived { .. }
            | MakerSwapEvent::MakerPaymentRefundRequired { .. }
            | MakerSwapEvent::MakerPaymentRefunded { .. }
            | MakerSwapEvent::TakerPaymentReceived { .. }
            | MakerSwapEvent::TakerPaymentReceivedPreimageSkipped { .. }
            | MakerSwapEvent::Aborted { .. }
            | MakerSwapEvent::Completed => (),
        }
        // Send a notification to the swap status streamer about a new event.
        self.ctx
            .event_stream_manager
            .send_fn(&mm2_event_stream::StreamerId::SwapStatus, || {
                super::swap_events::SwapStatusEvent::MakerV2 {
                    uuid: self.uuid,
                    event: event.clone(),
                }
            })
            .ok();
    }

    fn on_kickstart_event(&mut self, event: MakerSwapEvent) {
        // The volume entry comes back only where the maker payment is still
        // unsent, but the headroom is owed for as long as the taker payment is
        // unspent — which is every resumable event but the spend itself. This is
        // what the legacy protocol reserves at the same points (ch.51 R54), and
        // the two answers have to agree.
        if !matches!(event, MakerSwapEvent::TakerPaymentSpent { .. }) {
            self.reserve_taker_payment_spend_headroom();
        }

        // `self.maker_payment_trade_fee_reservable` was already recovered from
        // the persisted `Initialized` event during `recreate_machine`, so the
        // volume reservation of R58 does not need to re-extract anything from
        // whichever of the three events below we are actually resuming at.
        match event {
            MakerSwapEvent::Initialized { .. }
            | MakerSwapEvent::WaitingForTakerFunding { .. }
            | MakerSwapEvent::TakerFundingReceived { .. } => self.reserve_maker_payment_volume(),
            MakerSwapEvent::MakerPaymentSentFundingSpendGenerated { .. }
            | MakerSwapEvent::MakerPaymentRefundRequired { .. }
            | MakerSwapEvent::MakerPaymentRefunded { .. }
            | MakerSwapEvent::TakerPaymentReceived { .. }
            | MakerSwapEvent::TakerPaymentReceivedPreimageSkipped { .. }
            | MakerSwapEvent::TakerPaymentSpent { .. }
            | MakerSwapEvent::Aborted { .. }
            | MakerSwapEvent::Completed => (),
        }
    }
}

// InitialState / StorableState -----------------------------------------------

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> InitialState for Initialize<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Initialized<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::Initialized {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            maker_payment_trade_fee: self.maker_payment_trade_fee.clone(),
            taker_payment_spend_trade_fee: self.taker_payment_spend_trade_fee.clone(),
            taker_payment_spend_headroom: self.taker_payment_spend_headroom.clone(),
            maker_payment_trade_fee_reservable: self.maker_payment_trade_fee_reservable.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for WaitingForTakerFunding<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::WaitingForTakerFunding {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            maker_payment_trade_fee: self.maker_payment_trade_fee.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerFundingReceived<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::TakerFundingReceived {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            taker_funding: self.taker_funding.clone(),
            maker_payment_trade_fee: self.maker_payment_trade_fee.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for MakerPaymentSentFundingSpendGenerated<M, T>
{
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::MakerPaymentSentFundingSpendGenerated {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            maker_payment: self.maker_payment.clone(),
            taker_funding: self.taker_funding.clone(),
            funding_spend_preimage: self.funding_spend_preimage.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerPaymentReceived<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::TakerPaymentReceived {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            maker_payment: self.maker_payment.clone(),
            taker_payment: self.taker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for TakerPaymentReceivedPreimageSkipped<M, T>
{
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::TakerPaymentReceivedPreimageSkipped {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            maker_payment: self.maker_payment.clone(),
            taker_payment: self.taker_payment.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for TakerPaymentSpent<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::TakerPaymentSpent {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            maker_payment: self.maker_payment.clone(),
            taker_payment: self.taker_payment.clone(),
            taker_payment_spend: self.taker_payment_spend.clone(),
            negotiation_data: self.negotiation_data.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState
    for MakerPaymentRefundRequired<M, T>
{
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::MakerPaymentRefundRequired {
            maker_coin_start_block: self.maker_coin_start_block,
            taker_coin_start_block: self.taker_coin_start_block,
            negotiation_data: self.negotiation_data.clone(),
            maker_payment: self.maker_payment.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for MakerPaymentRefunded<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::MakerPaymentRefunded {
            maker_payment: self.maker_payment.clone(),
            maker_payment_refund: self.maker_payment_refund.clone(),
            reason: self.reason.clone(),
        }
    }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Completed<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent { MakerSwapEvent::Completed }
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> StorableState for Aborted<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    fn get_event(&self) -> MakerSwapEvent {
        MakerSwapEvent::Aborted {
            reason: self.reason.clone(),
        }
    }
}

// State implementations (P6.4 — swap execution logic) ------------------------

// Initialize → Initialized ----------------------------------------------

// Get start blocks, estimate fees, check balance.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for Initialize<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

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

        let preimage_value = TradePreimageValue::Exact(sm.maker_volume.to_decimal());
        let maker_payment_trade_fee = match sm
            .maker_coin
            .get_sender_trade_fee(preimage_value, FeeApproxStage::StartSwap)
            .await
        {
            Ok(fee) => fee,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get maker payment fee: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let taker_payment_spend_trade_fee = match sm
            .taker_coin
            .get_receiver_trade_fee(FeeApproxStage::StartSwap)
            .compat()
            .await
        {
            Ok(fee) => fee,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get taker payment spend fee: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Balance check: verify maker has sufficient funds for the swap amount plus network fees.
        let spendable = match sm.maker_coin.my_spendable_balance().compat().await {
            Ok(b) => b,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to get spendable balance: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let required = sm.maker_volume.to_decimal() + maker_payment_trade_fee.amount.to_decimal();
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
        let taker_payment_spend_headroom = super::fee_reservable_amount(&taker_payment_spend_trade_fee);
        // Same resolution for our own payment's send fee (ch.52 R58, closing
        // V5): `maker_payment_trade_fee.coin` need not be the maker coin — an
        // ERC20 maker coin bills it to the platform coin — and only the
        // descriptor in hand right now says so.
        let maker_payment_trade_fee_reservable = Some(super::fee_reservable_amount(&maker_payment_trade_fee));

        info!("Maker swap {} has successfully started", sm.uuid);
        Self::change_state(
            Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee.amount,
                taker_payment_spend_trade_fee.amount,
                taker_payment_spend_headroom,
                maker_payment_trade_fee_reservable,
            ),
            sm,
        )
        .await
    }
}

// Initialized → WaitingForTakerFunding ----------------------------------

// Exchange negotiation data via P2P, validate taker's parameters.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for Initialized<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
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
        let taker_coin_address = match sm.taker_coin.try_my_addr().await {
            Ok(address) => format!("{}", address),
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let negotiation_msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiation(MakerNegotiation {
                started_at: sm.started_at,
                payment_locktime: sm.maker_payment_locktime(),
                secret_hash: sm.secret_hash().to_vec(),
                maker_coin_htlc_pub: maker_coin_htlc_pub.to_vec(),
                taker_coin_htlc_pub: taker_coin_htlc_pub.to_vec(),
                maker_coin_swap_contract: sm.maker_coin.swap_contract_address().map(|b| b.0),
                taker_coin_swap_contract: sm.taker_coin.swap_contract_address().map(|b| b.0),
                taker_coin_address,
            })),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };

        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            negotiation_msg,
            super::NEGOTIATE_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        let taker_negotiation = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.taker_negotiation.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                let reason = AbortReason::NegotiationTimeout;
                warn!("Swap {}: failed to receive taker negotiation: {}", sm.uuid, e);
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        let taker_data = match taker_negotiation.action {
            Some(taker_negotiation::Action::Continue(data)) => data,
            Some(taker_negotiation::Action::Abort(abort)) => {
                return Self::change_state(Aborted::new(AbortReason::TakerAborted(abort.reason)), sm).await;
            },
            None => {
                let reason = AbortReason::NegotiationFailed("Empty taker negotiation action".into());
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Validate started_at difference.
        let started_at_diff = sm.started_at.abs_diff(taker_data.started_at);
        if started_at_diff > MAX_STARTED_AT_DIFF {
            let reason = AbortReason::NegotiationFailed(format!(
                "started_at difference too large: {} > {}",
                started_at_diff, MAX_STARTED_AT_DIFF
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Validate taker's locktimes.
        let expected_payment_locktime = taker_data.started_at + sm.lock_duration;
        if taker_data.payment_locktime != expected_payment_locktime {
            let reason = AbortReason::NegotiationFailed(format!(
                "Unexpected taker payment locktime: got {}, expected {}",
                taker_data.payment_locktime, expected_payment_locktime
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }
        let expected_funding_locktime = taker_data.started_at + 3 * sm.lock_duration;
        if taker_data.funding_locktime != expected_funding_locktime {
            let reason = AbortReason::NegotiationFailed(format!(
                "Unexpected taker funding locktime: got {}, expected {}",
                taker_data.funding_locktime, expected_funding_locktime
            ));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Parse taker's HTLC pubkeys (validates they're well-formed).
        if sm.taker_coin.parse_pubkey(&taker_data.taker_coin_htlc_pub).is_err() {
            let reason = AbortReason::NegotiationFailed("Invalid taker coin HTLC pubkey".into());
            return Self::change_state(Aborted::new(reason), sm).await;
        }
        if sm.maker_coin.parse_pubkey(&taker_data.maker_coin_htlc_pub).is_err() {
            let reason = AbortReason::NegotiationFailed("Invalid taker's maker-coin HTLC pubkey".into());
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        let negotiation_data = StoredMakerNegotiationData {
            taker_secret_hash: taker_data.taker_secret_hash.into(),
            taker_coin_htlc_pub: taker_data.taker_coin_htlc_pub.into(),
            maker_coin_htlc_pub: taker_data.maker_coin_htlc_pub.into(),
            taker_coin_swap_contract: taker_data.taker_coin_swap_contract.map(Into::into),
            maker_coin_swap_contract: taker_data.maker_coin_swap_contract.map(Into::into),
            taker_payment_locktime: taker_data.payment_locktime,
            taker_funding_locktime: taker_data.funding_locktime,
        };

        Self::change_state(
            WaitingForTakerFunding::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                negotiation_data,
                self.maker_payment_trade_fee.clone(),
            ),
            sm,
        )
        .await
    }
}

// WaitingForTakerFunding → TakerFundingReceived -------------------------

// Confirm negotiation to taker, receive funding tx.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for WaitingForTakerFunding<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let negotiated_msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerNegotiated(MakerNegotiated {
                negotiated: true,
                reason: None,
            })),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };
        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            negotiated_msg,
            super::NEGOTIATE_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        let funding_info = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.taker_funding.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                let reason = AbortReason::NegotiationFailed(format!("Did not receive taker funding info: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Parse the funding tx to verify it's well-formed.
        if sm.taker_coin.parse_tx(&funding_info.tx_bytes).is_err() {
            let reason = AbortReason::FailedToValidateTx("Failed to parse taker funding transaction".into());
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        Self::change_state(
            TakerFundingReceived::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                funding_info.tx_bytes.into(),
                self.maker_payment_trade_fee.clone(),
            ),
            sm,
        )
        .await
    }
}

// TakerFundingReceived → MakerPaymentSentFundingSpendGenerated ----------

// Validate taker funding, generate funding-spend preimage, send maker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerFundingReceived<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();

        // Step 1: Parse the funding tx.
        let taker_funding = match sm.taker_coin.parse_tx(&self.taker_funding) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToValidateTx(format!("Failed to parse taker funding: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Step 2: Offline semantic validation of taker funding.
        let taker_htlc_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker HTLC pub: {:?}", e));
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
            &self.negotiation_data.taker_coin_htlc_pub,
        );
        let validation_args = ValidateTakerFundingArgs {
            funding_tx: &taker_funding,
            payment_time_lock: self.negotiation_data.taker_payment_locktime,
            funding_time_lock: self.negotiation_data.taker_funding_locktime,
            taker_secret_hash: &self.negotiation_data.taker_secret_hash,
            maker_secret_hash: &sm.secret_hash(),
            taker_pub: &taker_htlc_pub,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            swap_unique_data: &unique_data,
        };
        if let Err(e) = sm.taker_coin.validate_taker_funding(validation_args).await {
            let reason = AbortReason::FailedToValidateTx(format!("Taker funding validation failed: {}", e));
            return Self::change_state(Aborted::new(reason), sm).await;
        }

        // Step 3: Confirmation gate (0-conf by default, 1-conf when strict mode is on).
        if sm.require_taker_funding_confirm {
            if let Err(e) = sm
                .taker_coin
                .wait_for_confirmations(
                    &taker_funding.tx_hex(),
                    confirmation_gate_confs(sm.conf_settings.taker_coin_confs),
                    sm.conf_settings.taker_coin_nota,
                    sm.maker_payment_locktime(),
                    10,
                )
                .compat()
                .await
            {
                let reason = AbortReason::ConfirmationTimeout(format!("Taker funding not confirmed in time: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            }
        }

        // Step 4: Generate funding-spend preimage.
        let maker_taker_coin_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason =
                    AbortReason::InternalError(format!("Failed to derive maker taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let gen_args = coins::GenTakerFundingSpendArgs {
            funding_tx: &taker_funding,
            maker_pub: &maker_taker_coin_pub,
            taker_pub: &taker_htlc_pub,
            funding_time_lock: self.negotiation_data.taker_funding_locktime,
            taker_secret_hash: &self.negotiation_data.taker_secret_hash,
            taker_payment_time_lock: self.negotiation_data.taker_payment_locktime,
            maker_secret_hash: &sm.secret_hash(),
        };
        let preimage_result = match sm
            .taker_coin
            .gen_taker_funding_spend_preimage(&gen_args, &unique_data)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to generate funding spend preimage: {}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        // Step 5: Send maker payment.
        let maker_coin_taker_pub = match sm.maker_coin.parse_pubkey(&self.negotiation_data.maker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker's maker-coin pub: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        let send_args = SendMakerPaymentArgs {
            time_lock: sm.maker_payment_locktime(),
            maker_secret_hash: &sm.secret_hash(),
            taker_secret_hash: &self.negotiation_data.taker_secret_hash,
            taker_pub: &maker_coin_taker_pub,
            amount: sm.maker_volume.to_decimal(),
            swap_unique_data: &unique_data,
        };
        let maker_payment = match sm.maker_coin.send_maker_payment_v2(send_args).await {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendTx(format!("Maker payment send failed: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };
        info!(
            "Sent maker payment {} tx {:?} during swap {}",
            sm.maker_coin.ticker(),
            maker_payment.tx_hash(),
            sm.uuid
        );

        let stored_preimage = StoredTxPreimage {
            preimage: preimage_result.preimage.to_bytes().into(),
            signature: preimage_result.signature.to_bytes().into(),
        };
        Self::change_state(
            MakerPaymentSentFundingSpendGenerated::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.negotiation_data.clone(),
                maker_payment.tx_hex().into(),
                self.taker_funding.clone(),
                stored_preimage,
            ),
            sm,
        )
        .await
    }
}

// MakerPaymentSentFundingSpendGenerated → (polling loop) ----------------

/// Whether a failed search for the funding spend is a node or transport error. The search is repeated after
/// such an error (CRD ch.52 R18); any other error ends the search.
fn funding_spend_search_error_is_transient(err: &SearchForFundingSpendErr) -> bool {
    matches!(err, SearchForFundingSpendErr::Rpc(_))
}

// Broadcast maker payment info, poll for taker funding spend on-chain.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State
    for MakerPaymentSentFundingSpendGenerated<M, T>
{
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let payment_info_msg = SwapMessage {
            inner: Some(swap_message::Inner::MakerPaymentInfo(MakerPaymentInfo {
                tx_bytes: self.maker_payment.0.clone(),
                next_step_instructions: None,
                funding_preimage_sig: self.funding_spend_preimage.signature.0.clone(),
                funding_preimage_tx: self.funding_spend_preimage.preimage.0.clone(),
            })),
            swap_uuid: sm.uuid.as_bytes().to_vec(),
        };
        let _abort_handle = super::broadcast_swap_v2_msg_every(
            sm.ctx.clone(),
            sm.p2p_topic.clone(),
            payment_info_msg,
            super::TX_INFO_SEND_INTERVAL,
            sm.p2p_keypair,
        );

        let taker_funding = match sm.taker_coin.parse_tx(&self.taker_funding) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to re-parse taker funding: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let wait_until = sm.started_at + sm.lock_duration * 2 / 3;
        // An undetermined search is never read as "nothing found": it is repeated inside `read`, also past
        // `wait_until`, up to the bound of the wait (CRD ch.51 R100 items 3 and 9).
        let mut undetermined_wait = UndeterminedWait::new(sm.maker_payment_locktime());

        loop {
            let now = now_ms() / 1000;
            if now > wait_until {
                let reason = AbortReason::FundingSpendError("Taker funding not spent in time".into());
                break self.abort_to_maker_payment_refund_required(reason, sm).await;
            }

            let search_end = undetermined_wait
                .read(
                    &sm.uuid,
                    sm.taker_coin.ticker(),
                    CappedRead::FundingSpendSearch,
                    30,
                    || now_ms() / 1000,
                    |_| {
                        sm.taker_coin.search_for_taker_funding_spend(
                            &taker_funding,
                            self.taker_coin_start_block,
                            &self.negotiation_data.taker_secret_hash,
                        )
                    },
                )
                .await;
            let search_result = match search_end {
                ReadEnd::Answer(found) => Ok(found),
                ReadEnd::Failed(e) => Err(e),
                ReadEnd::BoundPassed(e) => {
                    let reason = undetermined_cap_reason(CappedRead::FundingSpendSearch, sm.taker_coin.ticker(), &e);
                    break Self::change_state(Aborted::new(reason), sm).await;
                },
            };

            match search_result {
                // A taker payment that is found only after an undetermined wait and after the deadline is not
                // claimed: the maker's secret stays unused and the maker payment is refunded.
                Ok(Some(FundingTxSpend::TransferredToTakerPayment(_)))
                    if answered_too_late(undetermined_wait.waited(), now_ms() / 1000, wait_until) =>
                {
                    let reason = AbortReason::FundingSpendError("Taker funding not spent in time".into());
                    break self.abort_to_maker_payment_refund_required(reason, sm).await;
                },
                Ok(Some(FundingTxSpend::TransferredToTakerPayment(taker_payment))) => {
                    let taker_payment_bytes: BytesJson = taker_payment.tx_hex().into();
                    if sm.taker_coin.skip_taker_payment_spend_preimage() {
                        break Self::change_state(
                            TakerPaymentReceivedPreimageSkipped::new(
                                self.maker_coin_start_block,
                                self.taker_coin_start_block,
                                self.negotiation_data.clone(),
                                self.maker_payment.clone(),
                                taker_payment_bytes,
                            ),
                            sm,
                        )
                        .await;
                    } else {
                        break Self::change_state(
                            TakerPaymentReceived::new(
                                self.maker_coin_start_block,
                                self.taker_coin_start_block,
                                self.negotiation_data.clone(),
                                self.maker_payment.clone(),
                                taker_payment_bytes,
                            ),
                            sm,
                        )
                        .await;
                    }
                },
                Ok(Some(FundingTxSpend::RefundedTimelock(_))) => {
                    let reason = AbortReason::FundingSpendError("Taker funding reclaimed via timelock".into());
                    break self.abort_to_maker_payment_refund_required(reason, sm).await;
                },
                Ok(Some(FundingTxSpend::RefundedSecret { secret, .. })) => {
                    // Taker revealed their secret while reclaiming — maker can use it
                    // for a secret-based refund in MakerPaymentRefundRequired.
                    let reason = AbortReason::FundingSpendError(format!(
                        "Taker funding reclaimed via secret: {}",
                        hex::encode(secret)
                    ));
                    break self.abort_to_maker_payment_refund_required(reason, sm).await;
                },
                Ok(None) => {
                    Timer::sleep(30.).await;
                },
                Err(e) if funding_spend_search_error_is_transient(&e) => {
                    error!("Transient error in search_for_taker_funding_spend: {}", e);
                    Timer::sleep(30.).await;
                },
                Err(e) => {
                    let reason = AbortReason::FundingSpendError(format!("Irrecoverable search error: {:?}", e));
                    break self.abort_to_maker_payment_refund_required(reason, sm).await;
                },
            }
        }
    }
}

// TakerPaymentReceived → TakerPaymentSpent ------------------------------

/// Waits for the confirmations of the taker payment before the maker claims it (CRD ch.52 R19). An undetermined
/// read is repeated and never leads to the refund state; a confirmation that is read only after such a wait and
/// after the deadline is not acted on (CRD ch.51 R100 items 3 and 9).
async fn await_taker_payment_confirmation<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>(
    sm: &MakerSwapStateMachine<M, T>,
    taker_payment: &[u8],
) -> ConfirmationEnd {
    UndeterminedWait::new(sm.maker_payment_locktime())
        .read_confirmation(
            &sm.uuid,
            sm.taker_coin.ticker(),
            CappedRead::TakerPaymentConfirmation,
            sm.taker_payment_conf_timeout(),
            CONFIRMATION_POLL_SECS,
            || now_ms() / 1000,
            |wait_until| {
                sm.taker_coin
                    .wait_for_confirmations(
                        taker_payment,
                        sm.conf_settings.taker_coin_confs,
                        sm.conf_settings.taker_coin_nota,
                        wait_until,
                        10,
                    )
                    .compat()
            },
        )
        .await
}

// Wait for confirmation, receive + validate preimage, spend taker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentReceived<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();

        // Step 1: Wait for taker payment confirmations.
        match await_taker_payment_confirmation(sm, &self.taker_payment).await {
            ConfirmationEnd::Confirmed => (),
            ConfirmationEnd::NotConfirmed(e) => {
                let reason = AbortReason::ConfirmationTimeout(format!("Taker payment not confirmed in time: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
            ConfirmationEnd::BoundPassed(e) => {
                let reason = undetermined_cap_reason(CappedRead::TakerPaymentConfirmation, sm.taker_coin.ticker(), &e);
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        }

        // Step 2: Receive taker payment spend preimage.
        let preimage_data = match super::recv_swap_v2_msg(
            sm.ctx.clone(),
            |store| store.taker_payment_spend_preimage.take(),
            &sm.uuid,
            NEGOTIATION_TIMEOUT_SEC,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                let reason =
                    AbortReason::NegotiationFailed(format!("Did not receive taker payment spend preimage: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        // Step 3: Parse preimage and signature.
        let tx_preimage = match sm.taker_coin.parse_preimage(&preimage_data.tx_preimage) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker preimage: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let signature = match sm.taker_coin.parse_signature(&preimage_data.signature) {
            Ok(s) => s,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker signature: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let preimage_with_sig = coins::TxPreimageWithSig {
            preimage: tx_preimage,
            signature,
        };

        // Step 4: Parse taker payment and build spend args.
        let taker_payment_tx = match sm.taker_coin.parse_tx(&self.taker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to re-parse taker payment: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let taker_coin_maker_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let taker_coin_maker_addr = match sm.taker_coin.try_my_addr().await {
            Ok(address) => address,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let taker_htlc_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker HTLC pub: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &self.negotiation_data.taker_coin_htlc_pub,
        );
        let gen_args = coins::GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment_tx,
            time_lock: self.negotiation_data.taker_payment_locktime,
            maker_secret_hash: &sm.secret_hash(),
            maker_pub: &taker_coin_maker_pub,
            maker_address: &taker_coin_maker_addr,
            taker_pub: &taker_htlc_pub,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            dex_fee_addr_raw_pubkey: net_config.dex_fee_addr_raw_pubkey(),
        };

        // Step 5: Validate the preimage.
        if let Err(e) = sm
            .taker_coin
            .validate_taker_payment_spend_preimage(&gen_args, &preimage_with_sig)
            .await
        {
            let reason = AbortReason::FailedToValidateTx(format!("Taker payment spend preimage invalid: {}", e));
            return self.abort_to_maker_payment_refund_required(reason, sm).await;
        }

        // Step 6: Sign and broadcast the taker payment spend (reveals maker secret).
        let taker_payment_spend = match sm
            .taker_coin
            .sign_and_broadcast_taker_payment_spend(
                Some(&preimage_with_sig),
                &gen_args,
                sm.secret.as_slice(),
                &unique_data,
            )
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendTx(format!("Failed to spend taker payment: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        info!(
            "Spent taker payment {} tx {:?} during swap {}",
            sm.taker_coin.ticker(),
            taker_payment_spend.tx_hash(),
            sm.uuid
        );

        Self::change_state(
            TakerPaymentSpent::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.maker_payment.clone(),
                self.taker_payment.clone(),
                taker_payment_spend.tx_hex().into(),
                self.negotiation_data.clone(),
            ),
            sm,
        )
        .await
    }
}

// TakerPaymentReceivedPreimageSkipped → TakerPaymentSpent ---------------

// Same as TakerPaymentReceived but skips preimage exchange (EVM coins).

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State
    for TakerPaymentReceivedPreimageSkipped<M, T>
{
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();
        info!(
            "Skipping taker payment spend preimage validation for {} swap {}",
            sm.taker_coin.ticker(),
            sm.uuid
        );

        // Wait for taker payment confirmations.
        match await_taker_payment_confirmation(sm, &self.taker_payment).await {
            ConfirmationEnd::Confirmed => (),
            ConfirmationEnd::NotConfirmed(e) => {
                let reason = AbortReason::ConfirmationTimeout(format!("Taker payment not confirmed in time: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
            ConfirmationEnd::BoundPassed(e) => {
                let reason = undetermined_cap_reason(CappedRead::TakerPaymentConfirmation, sm.taker_coin.ticker(), &e);
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        }

        // Parse taker payment and build spend args.
        let taker_payment_tx = match sm.taker_coin.parse_tx(&self.taker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to re-parse taker payment: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let taker_coin_maker_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let taker_coin_maker_addr = match sm.taker_coin.try_my_addr().await {
            Ok(address) => address,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let taker_htlc_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker pub: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        let net_config = match mm2_net_config::net_config_for(sm.ctx.netid()) {
            Some(cfg) => cfg,
            None => {
                let reason = AbortReason::InternalError(format!("No net config for netid {}", sm.ctx.netid()));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };
        let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
            net_config,
            &sm.taker_coin,
            sm.maker_coin.ticker(),
            &sm.taker_volume,
            &self.negotiation_data.taker_coin_htlc_pub,
        );
        let gen_args = coins::GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment_tx,
            time_lock: self.negotiation_data.taker_payment_locktime,
            maker_secret_hash: &sm.secret_hash(),
            maker_pub: &taker_coin_maker_pub,
            maker_address: &taker_coin_maker_addr,
            taker_pub: &taker_htlc_pub,
            dex_fee: &dex_fee,
            premium_amount: sm.taker_premium.to_decimal(),
            trading_amount: sm.taker_volume.to_decimal(),
            dex_fee_addr_raw_pubkey: net_config.dex_fee_addr_raw_pubkey(),
        };

        // Sign and broadcast without preimage (None).
        let taker_payment_spend = match sm
            .taker_coin
            .sign_and_broadcast_taker_payment_spend(None, &gen_args, sm.secret.as_slice(), &[])
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::FailedToSendTx(format!("Failed to spend taker payment: {:?}", e));
                return self.abort_to_maker_payment_refund_required(reason, sm).await;
            },
        };

        info!(
            "Spent taker payment (preimage skipped) {} tx {:?} during swap {}",
            sm.taker_coin.ticker(),
            taker_payment_spend.tx_hash(),
            sm.uuid
        );

        Self::change_state(
            TakerPaymentSpent::new(
                self.maker_coin_start_block,
                self.taker_coin_start_block,
                self.maker_payment.clone(),
                self.taker_payment.clone(),
                taker_payment_spend.tx_hex().into(),
                self.negotiation_data.clone(),
            ),
            sm,
        )
        .await
    }
}

// TakerPaymentSpent → Completed -----------------------------------------

// Optionally wait for taker-payment-spend confirmation, then complete.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentSpent<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        if sm.require_taker_payment_spend_confirm {
            // The claim of the taker payment is broadcast. Its confirmation is read until 3700 seconds past
            // the maker payment locktime, and for at least 3700 seconds (CRD ch.52 R19; ch.51 R100 item 9). This
            // state never leads to a refund.
            let taker_coin = &sm.taker_coin;
            let confs = confirmation_gate_confs(sm.conf_settings.taker_coin_confs);
            let nota = sm.conf_settings.taker_coin_nota;
            let claim_tx_hash = match sm.taker_coin.parse_tx(&self.taker_payment_spend) {
                Ok(tx) => Ok(hex::encode(tx.tx_hash().0)),
                Err(_) => Err(hex::encode(&self.taker_payment_spend.0)),
            };
            let claim = own_claim_name(sm.taker_coin.ticker(), claim_tx_hash);
            let machine: &MakerSwapStateMachine<M, T> = sm;
            let confirmed = await_own_claim_confirmation(
                &claim,
                own_claim_confirmation_deadline(machine.maker_payment_locktime(), now_ms() / 1000),
                CONFIRMATION_POLL_SECS,
                || now_ms() / 1000,
                log_own_claim_line,
                self.taker_payment_spend.clone(),
                |claim_tx: BytesJson, wait_until| {
                    taker_coin
                        .wait_for_confirmations(&claim_tx, confs, nota, wait_until, CONFIRMATION_POLL_SECS)
                        .compat()
                },
                || send_claim_of_taker_payment_again(machine, &self.taker_payment, &self.negotiation_data),
            )
            .await;
            if !confirmed {
                let reason = maker_own_claim_not_confirmed_reason(&claim);
                return Self::change_state(Aborted::new(reason), sm).await;
            }
        }

        Self::change_state(Completed::new(), sm).await
    }
}

// MakerPaymentRefundRequired → MakerPaymentRefunded / Aborted -----------

// Wait for refund readiness, then refund via timelock (or secret if available).

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for MakerPaymentRefundRequired<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        warn!(
            "Swap {} entering maker payment refund; reason: {}",
            sm.uuid, self.reason
        );

        // The look before the refund reads the taker payment from the swap's saved events (CRD ch.51 R82).
        let claim_step = match sm.storage.get_repr(sm.uuid).await {
            Ok(repr) => claim_step_from_history(&repr.events),
            Err(_) => ClaimStep::Unknown,
        };

        // Neither the gate nor the look is ever read as "refundable" while it gives no answer the step may act
        // on: the attempt is repeated up to the bound of the wait (CRD ch.51 R100 items 3 and 9).
        let mut undetermined_wait = UndeterminedWait::new(sm.maker_payment_locktime());
        loop {
            let (read, text, history_lacks_inputs) = match self.refund_attempt(sm, &claim_step).await {
                MakerRefundAttempt::Refunded(refund_tx) => {
                    log_refund_step(
                        &sm.uuid,
                        &format!(
                            "refunded maker payment {} tx {:?}",
                            sm.maker_coin.ticker(),
                            refund_tx.tx_hash()
                        ),
                    );
                    let next = MakerPaymentRefunded::new(
                        self.maker_payment.clone(),
                        refund_tx.tx_hex().into(),
                        self.reason.clone(),
                    );
                    return Self::change_state(next, sm).await;
                },
                MakerRefundAttempt::Claimed {
                    taker_payment,
                    taker_payment_spend,
                } => {
                    log_refund_step(
                        &sm.uuid,
                        &format!(
                            "the claim of the {} taker payment is on chain, the maker payment is not refunded",
                            sm.taker_coin.ticker()
                        ),
                    );
                    let next = TakerPaymentSpent::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.maker_payment.clone(),
                        taker_payment,
                        taker_payment_spend,
                        self.negotiation_data.clone(),
                    );
                    return Self::change_state(next, sm).await;
                },
                MakerRefundAttempt::Wait(secs) => {
                    info!("Swap {} waiting {} seconds until refund is possible", sm.uuid, secs);
                    Timer::sleep(secs as f64).await;
                    continue;
                },
                MakerRefundAttempt::GateFailed(e) => {
                    error!("Swap {} can_refund_htlc error: {}, retrying in 30s", sm.uuid, e);
                    Timer::sleep(30.).await;
                    continue;
                },
                MakerRefundAttempt::RefundFailed(reason) => {
                    return Self::change_state(Aborted::new(AbortReason::InternalError(reason)), sm).await;
                },
                MakerRefundAttempt::Unanswered(read, text) => (read, text, false),
                MakerRefundAttempt::HistoryLacksInputs => {
                    (CappedRead::MakerRefundLook, HISTORY_LACKS_INPUTS.to_owned(), true)
                },
            };
            let ticker = match read {
                CappedRead::MakerRefundGate => sm.maker_coin.ticker(),
                _ => sm.taker_coin.ticker(),
            };
            let bound = wait_after_unanswered(
                &mut undetermined_wait,
                &sm.uuid,
                ticker,
                read,
                &text,
                history_lacks_inputs,
            )
            .await;
            if let Some(reason) = bound {
                return Self::change_state(Aborted::new(reason), sm).await;
            }
        }
    }
}

/// Sends the maker's claim of the taker payment again, built as the state that claims without a preimage builds
/// it, after the coin reported the claim mined but failed. Only a coin that skips the preimage exchange marks a
/// claim so; for any other coin this answers an error, and the claim is only read again. Errors are for the debug
/// log only.
async fn send_claim_of_taker_payment_again<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>(
    sm: &MakerSwapStateMachine<M, T>,
    taker_payment: &BytesJson,
    negotiation_data: &StoredMakerNegotiationData,
) -> Result<BytesJson, String> {
    if !sm.taker_coin.skip_taker_payment_spend_preimage() {
        return Err("the claim of this coin is not sent again".to_owned());
    }
    let unique_data = sm.unique_data();
    let taker_payment_tx = sm.taker_coin.parse_tx(taker_payment).map_err(|e| format!("{:?}", e))?;
    let taker_coin_maker_pub = sm
        .taker_coin
        .try_derive_htlc_pubkey_v2(&unique_data)
        .map_err(|e| e.to_string())?;
    let taker_coin_maker_addr = sm.taker_coin.try_my_addr().await.map_err(|e| e.to_string())?;
    let taker_htlc_pub = sm
        .taker_coin
        .parse_pubkey(&negotiation_data.taker_coin_htlc_pub)
        .map_err(|e| format!("{:?}", e))?;
    let net_config = mm2_net_config::net_config_for(sm.ctx.netid()).ok_or("no net config for this netid")?;
    let dex_fee = super::compute_dex_fee_v2_with_taker_pubkey_from_coin(
        net_config,
        &sm.taker_coin,
        sm.maker_coin.ticker(),
        &sm.taker_volume,
        &negotiation_data.taker_coin_htlc_pub,
    );
    let gen_args = coins::GenTakerPaymentSpendArgs {
        taker_tx: &taker_payment_tx,
        time_lock: negotiation_data.taker_payment_locktime,
        maker_secret_hash: &sm.secret_hash(),
        maker_pub: &taker_coin_maker_pub,
        maker_address: &taker_coin_maker_addr,
        taker_pub: &taker_htlc_pub,
        dex_fee: &dex_fee,
        premium_amount: sm.taker_premium.to_decimal(),
        trading_amount: sm.taker_volume.to_decimal(),
        dex_fee_addr_raw_pubkey: net_config.dex_fee_addr_raw_pubkey(),
    };
    let claim = sm
        .taker_coin
        .sign_and_broadcast_taker_payment_spend(None, &gen_args, sm.secret.as_slice(), &[])
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(claim.tx_hex().into())
}

/// The refund step a maker swap's manual recovery runs again (CRD ch.51 §51.6D, status note of 2026-11-24): the
/// swap's last event is `Aborted` with the reason variant its refund step ends with at the bound of its reads,
/// right after `MakerPaymentRefundRequired`. `None` for every other ending.
pub(super) fn recoverable_maker_refund<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2>(
    events: &[MakerSwapEvent],
) -> Option<MakerPaymentRefundRequired<M, T>> {
    match events {
        [.., MakerSwapEvent::MakerPaymentRefundRequired {
            maker_coin_start_block,
            taker_coin_start_block,
            negotiation_data,
            maker_payment,
            reason,
        }, MakerSwapEvent::Aborted {
            reason: AbortReason::InternalError(_),
        }] => Some(MakerPaymentRefundRequired::new(
            *maker_coin_start_block,
            *taker_coin_start_block,
            negotiation_data.clone(),
            maker_payment.clone(),
            reason.clone(),
        )),
        _ => None,
    }
}

/// `recover_funds_of_swap` for a maker swap of version two. A swap that ended at the bound of its refund step
/// runs one attempt of that step with fresh reads (the gate, and the look at the taker payment when the swap
/// reached the claim step); it refunds only when every read gave an answer the refund may rest on. The answer is
/// the RPC's existing one; a swap with any other ending gets the RPC's existing "not recoverable" answer.
pub(super) async fn recover_maker_swap_v2<M, T>(
    uuid: Uuid,
    repr: MakerSwapDbRepr,
    storage: MakerSwapStorage,
    maker_coin: M,
    taker_coin: T,
) -> Result<super::RecoveredSwap, String>
where
    M: MmCoin + MakerCoinSwapOpsV2,
    T: MmCoin + TakerCoinSwapOpsV2,
    M::Tx: Into<coins::TransactionEnum>,
{
    let state: MakerPaymentRefundRequired<M, T> = match recoverable_maker_refund(&repr.events) {
        Some(state) => state,
        None => return ERR!("{}", NOT_RECOVERABLE_V2),
    };
    let claim_step = claim_step_from_history(&repr.events);
    let mut repr = repr;
    repr.events.pop();
    let recreate_ctx = SwapRecreateCtx { maker_coin, taker_coin };
    let sm = match MakerSwapStateMachine::recreate_machine(uuid, storage, repr, recreate_ctx).await {
        Ok((machine, _)) => machine.into_inner(),
        Err(e) => return ERR!("{}", e),
    };
    match state.refund_attempt(&sm, &claim_step).await {
        MakerRefundAttempt::Refunded(refund_tx) => Ok(super::RecoveredSwap {
            action: super::RecoveredSwapAction::RefundedMyPayment,
            coin: sm.maker_coin.ticker().to_owned(),
            transaction: refund_tx.into(),
        }),
        MakerRefundAttempt::Claimed { .. } => ERR!(
            "The claim of the taker payment on {} is on the chain; the maker payment on {} belongs to the taker and is \
             not refunded",
            sm.taker_coin.ticker(),
            sm.maker_coin.ticker()
        ),
        MakerRefundAttempt::Wait(secs) => ERR!(
            "Too early to refund the maker payment on {}, wait {} seconds",
            sm.maker_coin.ticker(),
            secs
        ),
        MakerRefundAttempt::GateFailed(e) => ERR!(
            "{}",
            recovery_unanswered_text(sm.maker_coin.ticker(), Read::Gate, &e, "maker payment")
        ),
        MakerRefundAttempt::Unanswered(read, e) => {
            let ticker = match read {
                CappedRead::MakerRefundGate => sm.maker_coin.ticker(),
                _ => sm.taker_coin.ticker(),
            };
            ERR!(
                "{}",
                recovery_unanswered_text(ticker, read.read_kind(), &e, "maker payment")
            )
        },
        MakerRefundAttempt::HistoryLacksInputs => ERR!(
            "{}",
            history_lacks_inputs_reason(CappedRead::MakerRefundLook, sm.taker_coin.ticker())
        ),
        MakerRefundAttempt::RefundFailed(reason) => ERR!("{}", reason),
    }
}

/// What the maker's saved events say about its claim of the taker payment (CRD ch.51 R82).
#[derive(Debug, PartialEq)]
pub(super) enum ClaimStep {
    /// The swap did not reach the claim step: the maker's secret was never sent, and the refund needs no look.
    NotReached,
    /// The swap reached the claim step; the look reads this taker payment.
    Reached { taker_payment: BytesJson },
    /// The saved events hold neither the taker payment nor the maker payment's own event, so the look cannot run.
    Unknown,
}

/// Reads the claim step of a maker swap from its saved events.
pub(super) fn claim_step_from_history(events: &[MakerSwapEvent]) -> ClaimStep {
    let taker_payment = events.iter().rev().find_map(|event| match event {
        MakerSwapEvent::TakerPaymentReceived { taker_payment, .. }
        | MakerSwapEvent::TakerPaymentReceivedPreimageSkipped { taker_payment, .. }
        | MakerSwapEvent::TakerPaymentSpent { taker_payment, .. } => Some(taker_payment.clone()),
        _ => None,
    });
    let maker_paid = events
        .iter()
        .any(|event| matches!(event, MakerSwapEvent::MakerPaymentSentFundingSpendGenerated { .. }));
    match taker_payment {
        Some(taker_payment) if !taker_payment.0.is_empty() => ClaimStep::Reached { taker_payment },
        Some(_) => ClaimStep::Unknown,
        None if maker_paid => ClaimStep::NotReached,
        None => ClaimStep::Unknown,
    }
}

/// What one attempt of the maker's refund step ended with.
pub(super) enum MakerRefundAttempt<Tx> {
    /// The refund was broadcast.
    Refunded(Tx),
    /// The maker's own claim of the taker payment is on chain; nothing is refunded.
    Claimed {
        taker_payment: BytesJson,
        taker_payment_spend: BytesJson,
    },
    /// The refund gate asks to wait this many seconds.
    Wait(u64),
    /// The refund gate failed with an error that is not an undetermined read; it is retried as before.
    GateFailed(String),
    /// A read the refund rests on gave no answer the step may act on (the text is for the log only).
    Unanswered(CappedRead, String),
    /// The look cannot run, because the saved events do not hold the taker payment.
    HistoryLacksInputs,
    /// The refund broadcast failed, with the reason text of the abort.
    RefundFailed(String),
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> MakerPaymentRefundRequired<M, T> {
    /// One attempt of the refund step: the refund gate, then, for a swap that reached the claim step, the look at
    /// the taker payment, and the refund only when both allow it (CRD ch.52 R21; ch.51 R82, §51.6G R100). The
    /// state repeats it; the manual recovery makes it once.
    pub(super) async fn refund_attempt(
        &self,
        sm: &MakerSwapStateMachine<M, T>,
        claim_step: &ClaimStep,
    ) -> MakerRefundAttempt<M::Tx> {
        match sm
            .maker_coin
            .can_refund_htlc(sm.maker_payment_locktime())
            .compat()
            .await
        {
            Ok(CanRefundHtlc::CanRefundNow) => (),
            Ok(CanRefundHtlc::HaveToWait(secs)) => return MakerRefundAttempt::Wait(secs),
            Err(e) if is_undetermined(&e) => return MakerRefundAttempt::Unanswered(CappedRead::MakerRefundGate, e),
            Err(e) => return MakerRefundAttempt::GateFailed(e),
        }

        match claim_step {
            ClaimStep::NotReached => (),
            ClaimStep::Unknown => return MakerRefundAttempt::HistoryLacksInputs,
            ClaimStep::Reached { taker_payment } => {
                let taker_payment_tx = match sm.taker_coin.parse_tx(taker_payment) {
                    Ok(tx) => tx,
                    Err(_) => return MakerRefundAttempt::HistoryLacksInputs,
                };
                let wait_until = now_ms() / 1000 + REFUND_LOOK_WINDOW_SECS;
                let found = match sm
                    .taker_coin
                    .find_taker_payment_spend_tx(&taker_payment_tx, self.taker_coin_start_block, wait_until)
                    .await
                {
                    Ok(spend) => {
                        let is_refund = sm.taker_coin.is_taker_payment_refund_v2(&spend);
                        let extracted = sm.taker_coin.extract_secret_v2(&sm.secret_hash(), &spend).await;
                        maker_look_found(
                            is_refund,
                            extracted,
                            sm.secret.as_slice(),
                            BytesJson::from(spend.tx_hex()),
                        )
                    },
                    Err(e) => spend_search_error(&e),
                };
                match judge_look(found, true) {
                    Look::RefundGoesOn => (),
                    Look::Claimed(taker_payment_spend) => {
                        return MakerRefundAttempt::Claimed {
                            taker_payment: taker_payment.clone(),
                            taker_payment_spend,
                        }
                    },
                    Look::Unanswered(text) => return MakerRefundAttempt::Unanswered(CappedRead::MakerRefundLook, text),
                }
            },
        }

        let unique_data = sm.unique_data();
        let refund_args = RefundMakerPaymentTimelockArgs {
            payment_tx: &self.maker_payment,
            time_lock: sm.maker_payment_locktime(),
            taker_pub: &self.negotiation_data.maker_coin_htlc_pub,
            tx_type_with_secret_hash: SwapTxTypeWithSecretHash::MakerPaymentV2 {
                maker_secret_hash: &sm.secret_hash(),
                taker_secret_hash: &self.negotiation_data.taker_secret_hash,
            },
            swap_unique_data: &unique_data,
            watcher_reward: false,
            amount: sm.maker_volume.to_decimal(),
        };

        match sm.maker_coin.refund_maker_payment_v2_timelock(refund_args).await {
            Ok(tx) => MakerRefundAttempt::Refunded(tx),
            Err(e) => MakerRefundAttempt::RefundFailed(format!("Maker payment refund failed: {:?}", e)),
        }
    }
}

// Terminal states -------------------------------------------------------

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for MakerPaymentRefunded<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        info!(
            "Swap {} has been finished with maker payment refund; reason: {}",
            sm.uuid, self.reason
        );
    }
}

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for Completed<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        info!("Swap {} has been completed successfully", sm.uuid);
    }
}

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> LastState for Aborted<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;
    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> () {
        warn!("Swap {} was aborted with reason: {}", sm.uuid, self.reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `EthCoin` is a compile-time type parameter only; no coin instance is built.
    use coins::eth::EthCoin;

    /// CRD ch.52 R18: a node or transport error repeats the search for the funding spend.
    #[test]
    fn funding_spend_search_repeats_after_a_node_error() {
        let node_error = SearchForFundingSpendErr::Rpc("connection refused".into());
        assert!(funding_spend_search_error_is_transient(&node_error));
    }

    /// CRD ch.52 R18: an error that is not a node error ends the search.
    #[test]
    fn funding_spend_search_ends_on_an_error_that_is_not_a_node_error() {
        for err in [
            SearchForFundingSpendErr::InvalidInputTx("no output".into()),
            SearchForFundingSpendErr::FailedToProcessSpendTx("no input".into()),
            SearchForFundingSpendErr::FromBlockConversionErr("height".into()),
            SearchForFundingSpendErr::Internal("contract not set".into()),
        ] {
            assert!(!funding_spend_search_error_is_transient(&err), "{err}");
        }
    }

    /// CRD ch.52 R19: after the claim of the taker payment is broadcast, `TakerPaymentSpent` ends in `Completed`
    /// or `Aborted` only. The transition markers are what `change_state` accepts, so a transition to the refund
    /// state from this state does not compile.
    #[test]
    fn taker_payment_spent_has_no_transition_to_the_refund_state() {
        fn accepts<Next: TransitionFrom<Prev>, Prev>() {}
        accepts::<Completed<EthCoin, EthCoin>, TakerPaymentSpent<EthCoin, EthCoin>>();
        accepts::<Aborted<EthCoin, EthCoin>, TakerPaymentSpent<EthCoin, EthCoin>>();

        let whole_file = include_str!("maker_swap_v2.rs");
        let mod_tests_pos = whole_file.find("mod tests {").expect("this test module should exist");
        let source: String = whole_file[..mod_tests_pos]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!source.contains("TransitionFrom<TakerPaymentSpent<M, T>> for MakerPaymentRefundRequired<M, T>"));
    }

    /// CRD ch.51 R100 item 9: a state whose read stays undetermined until the bound of its wait ends in `Aborted`,
    /// and it keeps the transitions of its own rule for a determined read.
    #[test]
    fn states_that_wait_on_an_undetermined_read_can_end_in_aborted() {
        fn accepts<Next: TransitionFrom<Prev>, Prev>() {}
        type Abort = Aborted<EthCoin, EthCoin>;
        accepts::<Abort, MakerPaymentSentFundingSpendGenerated<EthCoin, EthCoin>>();
        accepts::<Abort, TakerPaymentReceived<EthCoin, EthCoin>>();
        accepts::<Abort, TakerPaymentReceivedPreimageSkipped<EthCoin, EthCoin>>();
        accepts::<Abort, MakerPaymentRefundRequired<EthCoin, EthCoin>>();

        type Refund = MakerPaymentRefundRequired<EthCoin, EthCoin>;
        accepts::<Refund, MakerPaymentSentFundingSpendGenerated<EthCoin, EthCoin>>();
        accepts::<Refund, TakerPaymentReceived<EthCoin, EthCoin>>();
        accepts::<Refund, TakerPaymentReceivedPreimageSkipped<EthCoin, EthCoin>>();
    }

    /// The search for the funding spend and the refund gate are each read at one place, the one inside the wait
    /// on undetermined reads; both states that wait for the taker payment's confirmation use the same wait.
    #[test]
    fn the_reads_before_a_refund_are_made_at_one_place_each() {
        let whole_file = include_str!("maker_swap_v2.rs");
        let mod_tests_pos = whole_file.find("mod tests {").expect("this test module should exist");
        let source = &whole_file[..mod_tests_pos];
        assert_eq!(source.matches(".search_for_taker_funding_spend(").count(), 1);
        assert_eq!(source.matches(".can_refund_htlc(").count(), 1);
        assert_eq!(source.matches("await_taker_payment_confirmation(sm,").count(), 2);
    }

    /// The look at the taker payment is made at one place, `refund_attempt`, which the refund state and the manual
    /// recovery both run; the look asks the coin whether a found spend is the taker's refund.
    #[test]
    fn the_state_and_the_recovery_share_the_look_that_tells_a_taker_refund() {
        let whole_file = include_str!("maker_swap_v2.rs");
        let mod_tests_pos = whole_file.find("mod tests {").expect("this test module should exist");
        let source = &whole_file[..mod_tests_pos];
        assert_eq!(source.matches(".find_taker_payment_spend_tx(").count(), 1);
        assert_eq!(source.matches(".is_taker_payment_refund_v2(").count(), 1);
        assert_eq!(source.matches("self.refund_attempt(sm, &claim_step)").count(), 1);
        assert_eq!(source.matches("state.refund_attempt(&sm, &claim_step)").count(), 1);
    }

    /// CRD ch.52 R18: the search for the funding spend judges a found taker payment by its own wait and the ⅔
    /// deadline before it is acted on, and the late case comes first.
    #[test]
    fn the_funding_spend_search_checks_a_found_taker_payment_for_being_too_late() {
        let whole_file = include_str!("maker_swap_v2.rs");
        let mod_tests_pos = whole_file.find("mod tests {").expect("this test module should exist");
        let source = &whole_file[..mod_tests_pos];
        let guard = "if answered_too_late(undetermined_wait.waited(), now_ms() / 1000, wait_until) =>";
        let guard_pos = source.find(guard).expect("the search has the late guard");
        let acted_on = "Ok(Some(FundingTxSpend::TransferredToTakerPayment(taker_payment))) =>";
        let acted_on_pos = source.find(acted_on).expect("the search acts on a found taker payment");
        assert!(guard_pos < acted_on_pos);
    }
}
