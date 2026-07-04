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
use coins::{MakerCoinSwapOpsV2, MmCoin, MmCoinEnum, ParseCoinAssocTypes, TakerCoinSwapOpsV2};
use common::executor::{spawn, Timer};
use common::log::{error, info, warn};
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_state_machine::storable_state_machine::{StateMachineDbRepr, StateMachineStorage, StorableStateMachine};
use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

    /// Insert a new V2 swap record into the my_swaps table.
    /// For maker swaps: my_coin = maker_coin, other_coin = taker_coin.
    /// For taker swaps: my_coin = taker_coin, other_coin = maker_coin.
    fn insert_swap_v2_maker(ctx: &MmArc, uuid: &Uuid, repr: &MakerSwapDbRepr) -> Result<(), String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let events_json = serde_json::to_string(&serde_json::json!([])).unwrap();
        let maker_vol_str = repr.maker_volume.to_decimal().to_string();
        let taker_vol_str = repr.taker_volume.to_decimal().to_string();
        let premium_str = repr.taker_premium.to_decimal().to_string();
        let dex_fee_str = repr.dex_fee_amount.to_decimal().to_string();
        let dex_fee_burn_str = repr.dex_fee_burn.to_decimal().to_string();
        let secret: Vec<u8> = repr.maker_secret.0.to_vec();
        let secret_hash: Vec<u8> = repr.maker_secret_hash.to_vec();
        let secret_hash_algo: i64 = secret_hash_algo_to_i64(repr.secret_hash_algo);
        let p2p_privkey: Vec<u8> = repr.p2p_keypair.as_ref().map(|k| k.0.clone()).unwrap_or_default();
        let other_p2p_pub: Vec<u8> = repr.taker_p2p_pub.to_vec();

        conn.execute(
            "INSERT INTO my_swaps (
                my_coin, other_coin, uuid, started_at, swap_type, is_finished, events_json,
                maker_volume, taker_volume, premium, dex_fee, dex_fee_burn,
                secret, secret_hash, secret_hash_algo, p2p_privkey, lock_duration,
                maker_coin_confs, maker_coin_nota, taker_coin_confs, taker_coin_nota,
                other_p2p_pub, swap_version
            ) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                repr.maker_coin, repr.taker_coin, uuid_str, repr.started_at as i64,
                MAKER_SWAP_V2_TYPE as i64, events_json,
                maker_vol_str, taker_vol_str, premium_str, dex_fee_str, dex_fee_burn_str,
                secret, secret_hash, secret_hash_algo, p2p_privkey,
                repr.lock_duration as i64,
                repr.conf_settings.maker_coin_confs as i64, repr.conf_settings.maker_coin_nota as i64,
                repr.conf_settings.taker_coin_confs as i64, repr.conf_settings.taker_coin_nota as i64,
                other_p2p_pub, repr.swap_version as i64,
            ],
        )
        .map(|_| ())
        .map_err(|e| format!("Failed to insert V2 maker swap {}: {}", uuid, e))
    }

    fn insert_swap_v2_taker(ctx: &MmArc, uuid: &Uuid, repr: &TakerSwapDbRepr) -> Result<(), String> {
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();
        let events_json = serde_json::to_string(&serde_json::json!([])).unwrap();
        let maker_vol_str = repr.maker_volume.to_decimal().to_string();
        let taker_vol_str = repr.taker_volume.to_decimal().to_string();
        let premium_str = repr.taker_premium.to_decimal().to_string();
        let dex_fee_str = repr.dex_fee_amount.to_decimal().to_string();
        let dex_fee_burn_str = repr.dex_fee_burn.to_decimal().to_string();
        let secret: Vec<u8> = repr.taker_secret.0.to_vec();
        let secret_hash: Vec<u8> = repr.taker_secret_hash.to_vec();
        let secret_hash_algo: i64 = secret_hash_algo_to_i64(repr.secret_hash_algo);
        let p2p_privkey: Vec<u8> = repr.p2p_keypair.as_ref().map(|k| k.0.clone()).unwrap_or_default();
        let other_p2p_pub: Vec<u8> = repr.maker_p2p_pub.to_vec();

        conn.execute(
            "INSERT INTO my_swaps (
                my_coin, other_coin, uuid, started_at, swap_type, is_finished, events_json,
                maker_volume, taker_volume, premium, dex_fee, dex_fee_burn,
                secret, secret_hash, secret_hash_algo, p2p_privkey, lock_duration,
                maker_coin_confs, maker_coin_nota, taker_coin_confs, taker_coin_nota,
                other_p2p_pub, swap_version
            ) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                repr.taker_coin, repr.maker_coin, uuid_str, repr.started_at as i64,
                TAKER_SWAP_V2_TYPE as i64, events_json,
                maker_vol_str, taker_vol_str, premium_str, dex_fee_str, dex_fee_burn_str,
                secret, secret_hash, secret_hash_algo, p2p_privkey,
                repr.lock_duration as i64,
                repr.conf_settings.maker_coin_confs as i64, repr.conf_settings.maker_coin_nota as i64,
                repr.conf_settings.taker_coin_confs as i64, repr.conf_settings.taker_coin_nota as i64,
                other_p2p_pub, repr.swap_version as i64,
            ],
        )
        .map(|_| ())
        .map_err(|e| format!("Failed to insert V2 taker swap {}: {}", uuid, e))
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
        let conn = ctx.sqlite_connection();
        let uuid_str = uuid.to_string();

        // Read existing events
        let events_str: String = conn
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

        conn.execute(
            "UPDATE my_swaps SET events_json = ?1 WHERE uuid = ?2",
            params![updated, uuid_str],
        )
        .map(|_| ())
        .map_err(|e| format!("Failed to update events for swap {}: {}", uuid, e))
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
            },
            TakerSwapEvent::MakerPaymentSpent {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                maker_payment_spend: BytesJson::from(vec![0x22]),
                negotiation_data: negotiation_data.clone(),
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

    #[test]
    fn should_roundtrip_when_serializing_maker_swap_db_repr() {
        use super::super::maker_swap_v2::{MakerSwapDbRepr, MakerSwapEvent, SerializableKeypairBytes};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use rpc::v1::types::H256 as H256Json;

        let repr = MakerSwapDbRepr {
            maker_coin: "RICK".into(),
            maker_volume: MmNumber::from("10.5"),
            maker_secret: H256Json::from([0xABu8; 32]),
            maker_secret_hash: BytesJson::from(vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
            started_at: 1700000000,
            lock_duration: 7200,
            taker_coin: "MORTY".into(),
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
                },
                MakerSwapEvent::Completed,
            ],
            taker_p2p_pub: BytesJson::from(vec![0x02; 33]),
            swap_version: 2,
        };

        let json = serde_json::to_string(&repr).unwrap();
        let back: MakerSwapDbRepr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.maker_coin, "RICK");
        assert_eq!(back.taker_coin, "MORTY");
        assert_eq!(back.uuid, repr.uuid);
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.swap_version, 2);
        assert_eq!(back.conf_settings.taker_coin_nota, true);
    }

    #[test]
    fn should_roundtrip_when_serializing_taker_swap_db_repr() {
        use super::super::taker_swap_v2::{TakerSwapDbRepr, TakerSwapEvent};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use rpc::v1::types::H256 as H256Json;

        let repr = TakerSwapDbRepr {
            maker_coin: "RICK".into(),
            maker_volume: MmNumber::from("10.5"),
            taker_secret: H256Json::from([0xCDu8; 32]),
            taker_secret_hash: BytesJson::from(vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::SHA256,
            started_at: 1700000000,
            lock_duration: 7200,
            taker_coin: "MORTY".into(),
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
                },
                TakerSwapEvent::Completed,
            ],
            maker_p2p_pub: BytesJson::from(vec![0x03; 33]),
            swap_version: 2,
        };

        let json = serde_json::to_string(&repr).unwrap();
        let back: TakerSwapDbRepr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.maker_coin, "RICK");
        assert_eq!(back.taker_coin, "MORTY");
        assert_eq!(back.uuid, repr.uuid);
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.swap_version, 2);
        assert_eq!(back.conf_settings.maker_coin_nota, true);
    }

    #[test]
    fn should_use_stable_discriminants_when_encoding_swap_v2_type() {
        assert_eq!(SwapV2Type::MakerV2 as u8, 1);
        assert_eq!(SwapV2Type::TakerV2 as u8, 2);
    }

    #[test]
    fn should_carry_fields_when_constructing_active_swap_info() {
        let uuid = Uuid::new_v4();
        let info = ActiveSwapV2Info {
            uuid,
            maker_coin: "RICK".into(),
            taker_coin: "MORTY".into(),
            swap_type: SwapV2Type::MakerV2,
        };
        assert_eq!(info.uuid, uuid);
        assert_eq!(info.maker_coin, "RICK");
        assert_eq!(info.taker_coin, "MORTY");
        assert_eq!(info.swap_type, SwapV2Type::MakerV2);
    }

    #[test]
    fn should_append_when_calling_add_event_on_db_repr() {
        use super::super::maker_swap_v2::{MakerSwapDbRepr, MakerSwapEvent};
        use super::super::SwapConfirmationsSettings;
        use common::mm_number::MmNumber;
        use mm2_state_machine::storable_state_machine::StateMachineDbRepr;
        use rpc::v1::types::H256 as H256Json;

        let mut repr = MakerSwapDbRepr {
            maker_coin: "RICK".into(),
            maker_volume: MmNumber::from("1"),
            maker_secret: H256Json::from([0u8; 32]),
            maker_secret_hash: BytesJson::from(vec![0u8; 20]),
            secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
            started_at: 0,
            lock_duration: 3600,
            taker_coin: "MORTY".into(),
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
                maker_coin: "RICK".into(),
                maker_volume: MmNumber::from("10"),
                maker_secret: H256Json::from([0xABu8; 32]),
                maker_secret_hash: BytesJson::from(vec![1u8; 20]),
                secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::DHASH160,
                started_at: 1700000000,
                lock_duration: 7200,
                taker_coin: "MORTY".into(),
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
                maker_coin: "RICK".into(),
                maker_volume: MmNumber::from("10"),
                taker_secret: H256Json::from([0xCDu8; 32]),
                taker_secret_hash: BytesJson::from(vec![2u8; 20]),
                secret_hash_algo: crypto::secret_hash_algo::SecretHashAlgo::SHA256,
                started_at: 1700000000,
                lock_duration: 7200,
                taker_coin: "MORTY".into(),
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
            assert_eq!(loaded.maker_coin, "RICK");
            assert_eq!(loaded.taker_coin, "MORTY");
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
            assert_eq!(loaded.maker_coin, "RICK");
            assert_eq!(loaded.taker_coin, "MORTY");
            assert_eq!(loaded.started_at, 1700000000);
            assert_eq!(loaded.swap_version, 2);

            // Append events
            let event1 = TakerSwapEvent::Initialized {
                maker_coin_start_block: 100,
                taker_coin_start_block: 200,
                taker_payment_fee: MmNumber::from("0.001"),
                maker_payment_spend_fee: MmNumber::from("0.002"),
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
            assert_eq!(loaded.conf_settings.maker_coin_nota, true);
            assert_eq!(loaded.conf_settings.taker_coin_confs, 3);
            assert_eq!(loaded.conf_settings.taker_coin_nota, false);
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
