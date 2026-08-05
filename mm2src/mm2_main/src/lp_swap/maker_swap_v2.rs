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
    _p: PhantomData<(M, T)>,
}

impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> Initialized<M, T> {
    pub fn new(
        maker_coin_start_block: u64,
        taker_coin_start_block: u64,
        maker_payment_trade_fee: MmNumber,
        taker_payment_spend_trade_fee: MmNumber,
    ) -> Self {
        Initialized {
            maker_coin_start_block,
            taker_coin_start_block,
            maker_payment_trade_fee,
            taker_payment_spend_trade_fee,
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

// TakerPaymentReceived →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentReceived<M, T>>
    for TakerPaymentSpent<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentReceived<M, T>>
    for MakerPaymentRefundRequired<M, T>
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

// TakerPaymentSpent →
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for Completed<M, T>
{
}
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> TransitionFrom<TakerPaymentSpent<M, T>>
    for MakerPaymentRefundRequired<M, T>
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

        let current_state: Box<dyn RestoredState<StateMachine = Self>> = match last_event {
            MakerSwapEvent::Initialized {
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee,
                taker_payment_spend_trade_fee,
            } => Box::new(Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee,
                taker_payment_spend_trade_fee,
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
        let swap_ctx = super::SwapsContext::from_ctx(&self.ctx).expect("SwapsContext should exist");
        swap_ctx.add_active_swap_v2(swap_info);
        let accept_from = secp256k1::PublicKey::from_slice(&self.taker_p2p_pubkey)
            .expect("taker_p2p_pubkey must be a valid 33-byte compressed pubkey");
        swap_ctx.init_v2_msg_store(self.uuid, accept_from);
    }

    fn clean_up_context(&mut self) {
        let swap_ctx = super::SwapsContext::from_ctx(&self.ctx).expect("SwapsContext should exist");
        swap_ctx.remove_active_swap_v2(&self.uuid);
        swap_ctx.remove_v2_msg_store(&self.uuid);

        // Clean up V2 locked amounts for both coins.
        let mut locked = swap_ctx.locked_amounts_v2.lock().unwrap();
        let maker_ticker = self.maker_coin.ticker();
        if let Some(entries) = locked.get_mut(maker_ticker) {
            entries.retain(|info| info.swap_uuid != self.uuid);
        }
        let taker_ticker = self.taker_coin.ticker();
        if let Some(entries) = locked.get_mut(taker_ticker) {
            entries.retain(|info| info.swap_uuid != self.uuid);
        }
    }

    fn on_event(&mut self, event: &MakerSwapEvent) {
        match event {
            MakerSwapEvent::Initialized {
                maker_payment_trade_fee,
                ..
            } => {
                let swaps_ctx =
                    super::SwapsContext::from_ctx(&self.ctx).expect("from_ctx should not fail at this point");
                let maker_coin_ticker: String = self.maker_coin.ticker().into();
                let new_locked = super::LockedAmountV2Info {
                    swap_uuid: self.uuid,
                    locked_amount: super::LockedAmount {
                        coin: maker_coin_ticker.clone(),
                        amount: self.maker_volume.clone(),
                        trade_fee: Some(coins::TradeFee {
                            coin: maker_coin_ticker.clone(),
                            amount: maker_payment_trade_fee.clone(),
                            paid_from_trading_vol: false,
                        }),
                    },
                };
                swaps_ctx
                    .locked_amounts_v2
                    .lock()
                    .unwrap()
                    .entry(maker_coin_ticker)
                    .or_default()
                    .push(new_locked);
            },
            MakerSwapEvent::MakerPaymentSentFundingSpendGenerated { .. } => {
                let swaps_ctx =
                    super::SwapsContext::from_ctx(&self.ctx).expect("from_ctx should not fail at this point");
                let ticker = self.maker_coin.ticker();
                if let Some(maker_coin_locked) = swaps_ctx.locked_amounts_v2.lock().unwrap().get_mut(ticker) {
                    maker_coin_locked.retain(|locked| locked.swap_uuid != self.uuid);
                };
            },
            MakerSwapEvent::WaitingForTakerFunding { .. }
            | MakerSwapEvent::TakerFundingReceived { .. }
            | MakerSwapEvent::MakerPaymentRefundRequired { .. }
            | MakerSwapEvent::MakerPaymentRefunded { .. }
            | MakerSwapEvent::TakerPaymentReceived { .. }
            | MakerSwapEvent::TakerPaymentReceivedPreimageSkipped { .. }
            | MakerSwapEvent::TakerPaymentSpent { .. }
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
        match event {
            MakerSwapEvent::Initialized {
                maker_payment_trade_fee,
                ..
            }
            | MakerSwapEvent::WaitingForTakerFunding {
                maker_payment_trade_fee,
                ..
            }
            | MakerSwapEvent::TakerFundingReceived {
                maker_payment_trade_fee,
                ..
            } => {
                let swaps_ctx =
                    super::SwapsContext::from_ctx(&self.ctx).expect("from_ctx should not fail at this point");
                let maker_coin_ticker: String = self.maker_coin.ticker().into();
                let new_locked = super::LockedAmountV2Info {
                    swap_uuid: self.uuid,
                    locked_amount: super::LockedAmount {
                        coin: maker_coin_ticker.clone(),
                        amount: self.maker_volume.clone(),
                        trade_fee: Some(coins::TradeFee {
                            coin: maker_coin_ticker.clone(),
                            amount: maker_payment_trade_fee,
                            paid_from_trading_vol: false,
                        }),
                    },
                };
                swaps_ctx
                    .locked_amounts_v2
                    .lock()
                    .unwrap()
                    .entry(maker_coin_ticker)
                    .or_default()
                    .push(new_locked);
            },
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

        info!("Maker swap {} has successfully started", sm.uuid);
        Self::change_state(
            Initialized::new(
                maker_coin_start_block,
                taker_coin_start_block,
                maker_payment_trade_fee.amount,
                taker_payment_spend_trade_fee.amount,
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
        let dex_fee = super::compute_dex_fee_with_taker_pubkey_from_coin(
            mm2_net_config::net_config_or_panic(sm.ctx.netid()),
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
                return Self::change_state(
                    super::maker_swap_v2::MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        let wait_until = sm.started_at + sm.lock_duration * 2 / 3;

        loop {
            let now = now_ms() / 1000;
            if now > wait_until {
                let reason = AbortReason::FundingSpendError("Taker funding not spent in time".into());
                break Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            }

            let search_result = sm
                .taker_coin
                .search_for_taker_funding_spend(
                    &taker_funding,
                    self.taker_coin_start_block,
                    &self.negotiation_data.taker_secret_hash,
                )
                .await;

            match search_result {
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
                    break Self::change_state(
                        MakerPaymentRefundRequired::new(
                            self.maker_coin_start_block,
                            self.taker_coin_start_block,
                            self.negotiation_data.clone(),
                            self.maker_payment.clone(),
                            reason,
                        ),
                        sm,
                    )
                    .await;
                },
                Ok(Some(FundingTxSpend::RefundedSecret { secret, .. })) => {
                    // Taker revealed their secret while reclaiming — maker can use it
                    // for a secret-based refund in MakerPaymentRefundRequired.
                    let reason = AbortReason::FundingSpendError(format!(
                        "Taker funding reclaimed via secret: {}",
                        hex::encode(secret)
                    ));
                    break Self::change_state(
                        MakerPaymentRefundRequired::new(
                            self.maker_coin_start_block,
                            self.taker_coin_start_block,
                            self.negotiation_data.clone(),
                            self.maker_payment.clone(),
                            reason,
                        ),
                        sm,
                    )
                    .await;
                },
                Ok(None) => {
                    Timer::sleep(30.).await;
                },
                Err(SearchForFundingSpendErr::Rpc(e)) => {
                    error!("RPC error in search_for_taker_funding_spend: {}", e);
                    Timer::sleep(30.).await;
                },
                Err(e) => {
                    let reason = AbortReason::FundingSpendError(format!("Irrecoverable search error: {:?}", e));
                    break Self::change_state(
                        MakerPaymentRefundRequired::new(
                            self.maker_coin_start_block,
                            self.taker_coin_start_block,
                            self.negotiation_data.clone(),
                            self.maker_payment.clone(),
                            reason,
                        ),
                        sm,
                    )
                    .await;
                },
            }
        }
    }
}

// TakerPaymentReceived → TakerPaymentSpent ------------------------------

// Wait for confirmation, receive + validate preimage, spend taker payment.

#[async_trait::async_trait]
impl<M: MmCoin + MakerCoinSwapOpsV2, T: MmCoin + TakerCoinSwapOpsV2> State for TakerPaymentReceived<M, T> {
    type StateMachine = MakerSwapStateMachine<M, T>;

    async fn on_changed(self: Box<Self>, sm: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
        let unique_data = sm.unique_data();

        // Step 1: Wait for taker payment confirmations.
        if let Err(e) = sm
            .taker_coin
            .wait_for_confirmations(
                &self.taker_payment,
                sm.conf_settings.taker_coin_confs,
                sm.conf_settings.taker_coin_nota,
                sm.taker_payment_conf_timeout(),
                10,
            )
            .compat()
            .await
        {
            let reason = AbortReason::ConfirmationTimeout(format!("Taker payment not confirmed in time: {}", e));
            return Self::change_state(
                MakerPaymentRefundRequired::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    self.maker_payment.clone(),
                    reason,
                ),
                sm,
            )
            .await;
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
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        // Step 3: Parse preimage and signature.
        let tx_preimage = match sm.taker_coin.parse_preimage(&preimage_data.tx_preimage) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker preimage: {:?}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };
        let signature = match sm.taker_coin.parse_signature(&preimage_data.signature) {
            Ok(s) => s,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker signature: {:?}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
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
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        let taker_coin_maker_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };
        let taker_coin_maker_addr = match sm.taker_coin.try_my_addr().await {
            Ok(address) => address,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };
        let taker_htlc_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker HTLC pub: {:?}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        let dex_fee = super::compute_dex_fee_with_taker_pubkey_from_coin(
            mm2_net_config::net_config_or_panic(sm.ctx.netid()),
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
        };

        // Step 5: Validate the preimage.
        if let Err(e) = sm
            .taker_coin
            .validate_taker_payment_spend_preimage(&gen_args, &preimage_with_sig)
            .await
        {
            let reason = AbortReason::FailedToValidateTx(format!("Taker payment spend preimage invalid: {}", e));
            return Self::change_state(
                MakerPaymentRefundRequired::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    self.maker_payment.clone(),
                    reason,
                ),
                sm,
            )
            .await;
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
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
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
        if let Err(e) = sm
            .taker_coin
            .wait_for_confirmations(
                &self.taker_payment,
                sm.conf_settings.taker_coin_confs,
                sm.conf_settings.taker_coin_nota,
                sm.taker_payment_conf_timeout(),
                10,
            )
            .compat()
            .await
        {
            let reason = AbortReason::ConfirmationTimeout(format!("Taker payment not confirmed in time: {}", e));
            return Self::change_state(
                MakerPaymentRefundRequired::new(
                    self.maker_coin_start_block,
                    self.taker_coin_start_block,
                    self.negotiation_data.clone(),
                    self.maker_payment.clone(),
                    reason,
                ),
                sm,
            )
            .await;
        }

        // Parse taker payment and build spend args.
        let taker_payment_tx = match sm.taker_coin.parse_tx(&self.taker_payment) {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to re-parse taker payment: {:?}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        let taker_coin_maker_pub = match sm.taker_coin.try_derive_htlc_pubkey_v2(&unique_data) {
            Ok(pubkey) => pubkey,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to derive taker-coin V2 HTLC pubkey: {}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };
        let taker_coin_maker_addr = match sm.taker_coin.try_my_addr().await {
            Ok(address) => address,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to select taker-coin V2 address: {}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };
        let taker_htlc_pub = match sm.taker_coin.parse_pubkey(&self.negotiation_data.taker_coin_htlc_pub) {
            Ok(p) => p,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Failed to parse taker pub: {:?}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
            },
        };

        let dex_fee = super::compute_dex_fee_with_taker_pubkey_from_coin(
            mm2_net_config::net_config_or_panic(sm.ctx.netid()),
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
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
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
            if let Err(e) = sm
                .taker_coin
                .wait_for_confirmations(
                    &self.taker_payment_spend,
                    confirmation_gate_confs(sm.conf_settings.taker_coin_confs),
                    sm.conf_settings.taker_coin_nota,
                    sm.maker_payment_locktime(),
                    10,
                )
                .compat()
                .await
            {
                let reason =
                    AbortReason::ConfirmationTimeout(format!("Taker payment spend not confirmed in time: {}", e));
                return Self::change_state(
                    MakerPaymentRefundRequired::new(
                        self.maker_coin_start_block,
                        self.taker_coin_start_block,
                        self.negotiation_data.clone(),
                        self.maker_payment.clone(),
                        reason,
                    ),
                    sm,
                )
                .await;
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

        // Wait until the refund timelock has expired.
        loop {
            match sm
                .maker_coin
                .can_refund_htlc(sm.maker_payment_locktime())
                .compat()
                .await
            {
                Ok(CanRefundHtlc::CanRefundNow) => break,
                Ok(CanRefundHtlc::HaveToWait(secs)) => {
                    info!("Swap {} waiting {} seconds until refund is possible", sm.uuid, secs);
                    Timer::sleep(secs as f64).await;
                },
                Err(e) => {
                    error!("Swap {} can_refund_htlc error: {}, retrying in 30s", sm.uuid, e);
                    Timer::sleep(30.).await;
                },
            }
        }

        // Attempt timelock-based refund.
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

        let refund_tx = match sm.maker_coin.refund_maker_payment_v2_timelock(refund_args).await {
            Ok(tx) => tx,
            Err(e) => {
                let reason = AbortReason::InternalError(format!("Maker payment refund failed: {:?}", e));
                return Self::change_state(Aborted::new(reason), sm).await;
            },
        };

        info!(
            "Refunded maker payment {} tx {:?} during swap {}",
            sm.maker_coin.ticker(),
            refund_tx.tx_hash(),
            sm.uuid
        );

        Self::change_state(
            MakerPaymentRefunded::new(
                self.maker_payment.clone(),
                refund_tx.tx_hex().into(),
                self.reason.clone(),
            ),
            sm,
        )
        .await
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
