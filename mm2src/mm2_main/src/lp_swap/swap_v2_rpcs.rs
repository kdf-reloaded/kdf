//! # Purpose
//! Unified RPC surface that returns swap data regardless of whether a swap
//! ran the legacy V1 protocol or the V2 (Trading Protocol Upgrade)
//! state machine.
//!
//! # Public exports
//! - [`my_swap_status_rpc`] — handles `my_swap_status` (single swap by UUID).
//! - [`my_recent_swaps_rpc`] — handles `my_recent_swaps` (paged list).
//! - [`active_swaps_rpc`] — handles `active_swaps` (currently running swaps).
//! - [`SwapRpcData`] — wire envelope tagged by swap type.
//! - [`MySwapForRpc`] — JSON projection of a V2 swap row.
//!
//! # Invariants
//! - Method strings `my_swap_status`, `my_recent_swaps`, `active_swaps`
//!   are wire-stable; do not rename the public handlers.
//! - The [`SwapRpcData`] discriminant strings (`MakerV1`, `TakerV1`,
//!   `MakerV2`, `TakerV2`) and the `swap_type` / `swap_data` tag/content
//!   keys are part of the JSON contract with GUI clients.
//! - The [`MySwapForRpc`] field names (`my_coin`, `other_coin`, `uuid`,
//!   `started_at`, `is_finished`, `events`, `maker_volume`, `taker_volume`,
//!   `premium`, `dex_fee`, `lock_duration`, `*_coin_confs`, `*_coin_nota`,
//!   `swap_version`) are wire-stable.
//! - Error enum variant names (`NoSwapWithUuid`, `UnsupportedSwapType`,
//!   `DbError`, `FromUuidSwapNotFound`, `InvalidTimeStampRange`, `Internal`)
//!   surface in the `error_type` JSON field and must not change.

use super::maker_swap::MakerSavedSwap;
use super::maker_swap_v2::MakerSwapEvent;
use super::my_swaps_storage::{MySwapsError, MySwapsOps, MySwapsStorage};
use super::taker_swap::TakerSavedSwap;
use super::taker_swap_v2::TakerSwapEvent;
use super::{active_swaps, active_swaps_using_coin, MySwapsFilter, SavedSwap, SavedSwapError, SavedSwapIo,
            LEGACY_SWAP_TYPE, MAKER_SWAP_V2_TYPE, TAKER_SWAP_V2_TYPE};
use common::log::{error, warn};
use common::mm_number::{BigDecimal, MmNumber, MmNumberMultiRepr};
use common::{calc_total_pages, HttpStatusCode, PagingOptions};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::str::FromStr;
use uuid::Uuid;

cfg_native!(
    use crate::mm2::database::my_swaps::SELECT_MY_SWAP_V2_FOR_RPC_BY_UUID;
    use common::async_blocking;
    use db_common::sqlite::query_single_row;
    use db_common::sqlite::rusqlite::{Result as SqlResult, Row, Error as SqlError};
    use db_common::sqlite::rusqlite::types::Type as SqlType;
);

cfg_wasm32!(
    use super::SwapsContext;
    use super::maker_swap_v2::MakerSwapDbRepr;
    use super::taker_swap_v2::TakerSwapDbRepr;
    use crate::mm2::lp_swap::swap_wasm_db::{MySwapsFiltersTable, SavedSwapTable};
    use mm2_db::indexed_db::{DbTransactionError, DbTransactionResult, InitDbError};
);

// Swap-type lookup ----------------------------------------------------------

/// Native lookup of the `swap_type` column for a swap UUID.
///
/// Returns `Ok(None)` when the UUID is unknown — this lets callers
/// translate "missing" into a 400-level RPC error rather than a 500.
#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn get_swap_type(ctx: &MmArc, uuid: &Uuid) -> MmResult<Option<u8>, SqlError> {
    let ctx = ctx.clone();
    let uuid = uuid.to_string();

    async_blocking(move || {
        // Positional `?1` binding is preferred over named binding for a
        // single-parameter query: it avoids the named-parameter map lookup.
        const SELECT_SWAP_TYPE_BY_UUID: &str = "SELECT swap_type FROM my_swaps WHERE uuid = ?1;";
        let maybe_swap_type = query_single_row(
            &ctx.sqlite_connection(),
            SELECT_SWAP_TYPE_BY_UUID,
            &[uuid.as_str()],
            |row| row.get(0),
        )?;
        Ok(maybe_swap_type)
    })
    .await
}

/// WASM lookup of the `swap_type` column for a swap UUID.
#[cfg(target_arch = "wasm32")]
pub(super) async fn get_swap_type(ctx: &MmArc, uuid: &Uuid) -> MmResult<Option<u8>, SwapV2DbError> {
    let swaps_ctx = SwapsContext::from_ctx(ctx).unwrap();
    let db = swaps_ctx.swap_db().await.mm_err(Into::into)?;
    let transaction = db.transaction().await.mm_err(Into::into)?;
    let table = transaction.table::<MySwapsFiltersTable>().await.mm_err(Into::into)?;
    let row = table.get_item_by_unique_index("uuid", uuid).await.mm_err(Into::into)?;
    Ok(row.map(|(_id, item)| item.swap_type))
}

// WASM-only DB error taxonomy ----------------------------------------------

/// Errors raised by the WASM IndexedDB-backed swap reads.
///
/// Native builds use `SqlError` directly; this enum lifts the WASM-side
/// error variants into a single shape the surrounding handlers can map
/// into the wire-facing RPC errors.
#[cfg(target_arch = "wasm32")]
#[derive(Display)]
pub enum SwapV2DbError {
    DbTransaction(DbTransactionError),
    InitDb(InitDbError),
    Serde(serde_json::Error),
    UnsupportedSwapType(u8),
}

// One trivial conversion per WASM-side error source. Kept as
// hand-written impls so that adding a variant remains a one-block
// change reviewable in isolation.
#[cfg(target_arch = "wasm32")]
impl From<DbTransactionError> for SwapV2DbError {
    fn from(err: DbTransactionError) -> Self { SwapV2DbError::DbTransaction(err) }
}

#[cfg(target_arch = "wasm32")]
impl From<InitDbError> for SwapV2DbError {
    fn from(err: InitDbError) -> Self { SwapV2DbError::InitDb(err) }
}

#[cfg(target_arch = "wasm32")]
impl From<serde_json::Error> for SwapV2DbError {
    fn from(err: serde_json::Error) -> Self { SwapV2DbError::Serde(err) }
}

// V2 swap projection -------------------------------------------------------

/// JSON projection of a V2 swap row.
///
/// The on-disk row carries seller-side secrets (HTLC preimages,
/// signing material) that are stripped here; only the fields safe to
/// expose to a RPC consumer are kept.
#[derive(Debug, Serialize)]
pub(crate) struct MySwapForRpc<T> {
    my_coin: String,
    other_coin: String,
    uuid: Uuid,
    started_at: i64,
    is_finished: bool,
    events: Vec<T>,
    maker_volume: MmNumberMultiRepr,
    taker_volume: MmNumberMultiRepr,
    premium: MmNumberMultiRepr,
    dex_fee: MmNumberMultiRepr,
    lock_duration: i64,
    maker_coin_confs: i64,
    maker_coin_nota: bool,
    taker_coin_confs: i64,
    taker_coin_nota: bool,
    swap_version: u8,
    maker_coin_usd_price: String,
    taker_coin_usd_price: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: DeserializeOwned> MySwapForRpc<T> {
    /// Decodes one row of the joined `my_swaps` × `my_swaps_v2` view.
    ///
    /// SQLite stores volumes as fraction strings (e.g. `"3/2"`) for
    /// exactness; this routine widens each one back into the multi-repr
    /// numeric form GUI clients expect.
    fn from_row(row: &Row) -> SqlResult<Self> {
        let read_decimal_column = |idx: usize| -> SqlResult<MmNumberMultiRepr> {
            let raw: String = row.get(idx)?;
            let decimal = BigDecimal::from_str(&raw)
                .map_err(|e| SqlError::FromSqlConversionFailure(idx, SqlType::Text, Box::new(e)))?;
            Ok(MmNumberMultiRepr::from(MmNumber::from(decimal)))
        };

        Ok(Self {
            my_coin: row.get(0)?,
            other_coin: row.get(1)?,
            uuid: row
                .get::<_, String>(2)?
                .parse()
                .map_err(|e| SqlError::FromSqlConversionFailure(2, SqlType::Text, Box::new(e)))?,
            started_at: row.get(3)?,
            is_finished: row.get(4)?,
            events: serde_json::from_str(&row.get::<_, String>(5)?)
                .map_err(|e| SqlError::FromSqlConversionFailure(5, SqlType::Text, Box::new(e)))?,
            maker_volume: read_decimal_column(6)?,
            taker_volume: read_decimal_column(7)?,
            premium: read_decimal_column(8)?,
            dex_fee: read_decimal_column(9)?,
            lock_duration: row.get(10)?,
            maker_coin_confs: row.get(11)?,
            maker_coin_nota: row.get(12)?,
            taker_coin_confs: row.get(13)?,
            taker_coin_nota: row.get(14)?,
            swap_version: row.get(15)?,
            maker_coin_usd_price: row.get(16)?,
            taker_coin_usd_price: row.get(17)?,
        })
    }
}

// Native V2 swap row reads -------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn get_maker_swap_data_for_rpc(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<MySwapForRpc<MakerSwapEvent>>, SqlError> {
    query_v2_swap_row(ctx, uuid).await
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn get_taker_swap_data_for_rpc(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<MySwapForRpc<TakerSwapEvent>>, SqlError> {
    query_v2_swap_row(ctx, uuid).await
}

/// Generic native row read — both maker and taker rows live in the
/// same joined view and only differ by the `T` event type they decode.
#[cfg(not(target_arch = "wasm32"))]
async fn query_v2_swap_row<T: DeserializeOwned + Send + 'static>(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<MySwapForRpc<T>>, SqlError> {
    let ctx = ctx.clone();
    let uuid_str = uuid.to_string();

    async_blocking(move || {
        let row = query_single_row(
            &ctx.sqlite_connection(),
            SELECT_MY_SWAP_V2_FOR_RPC_BY_UUID,
            &[uuid_str.as_str()],
            MySwapForRpc::from_row,
        )?;
        Ok(row)
    })
    .await
}

// WASM V2 swap row reads ---------------------------------------------------

#[cfg(target_arch = "wasm32")]
pub(super) async fn get_maker_swap_data_for_rpc(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<MySwapForRpc<MakerSwapEvent>>, SwapV2DbError> {
    let Some((repr, filter)) = load_v2_swap_repr::<MakerSwapDbRepr>(ctx, uuid).await? else {
        return Ok(None);
    };
    Ok(Some(MySwapForRpc {
        my_coin: repr.maker_coin,
        other_coin: repr.taker_coin,
        uuid: repr.uuid,
        started_at: repr.started_at as i64,
        is_finished: filter.is_finished.as_bool(),
        events: repr.events,
        maker_volume: repr.maker_volume.into(),
        taker_volume: repr.taker_volume.into(),
        premium: repr.taker_premium.into(),
        dex_fee: (repr.dex_fee_amount + repr.dex_fee_burn).into(),
        lock_duration: repr.lock_duration as i64,
        maker_coin_confs: repr.conf_settings.maker_coin_confs as i64,
        maker_coin_nota: repr.conf_settings.maker_coin_nota,
        taker_coin_confs: repr.conf_settings.taker_coin_confs as i64,
        taker_coin_nota: repr.conf_settings.taker_coin_nota,
        swap_version: repr.swap_version,
        maker_coin_usd_price: String::new(),
        taker_coin_usd_price: String::new(),
    }))
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn get_taker_swap_data_for_rpc(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<MySwapForRpc<TakerSwapEvent>>, SwapV2DbError> {
    let Some((repr, filter)) = load_v2_swap_repr::<TakerSwapDbRepr>(ctx, uuid).await? else {
        return Ok(None);
    };
    // The taker view flips maker_coin/other_coin compared to the maker view;
    // every other field has the same on-disk semantics.
    Ok(Some(MySwapForRpc {
        my_coin: repr.taker_coin,
        other_coin: repr.maker_coin,
        uuid: repr.uuid,
        started_at: repr.started_at as i64,
        is_finished: filter.is_finished.as_bool(),
        events: repr.events,
        maker_volume: repr.maker_volume.into(),
        taker_volume: repr.taker_volume.into(),
        premium: repr.taker_premium.into(),
        dex_fee: (repr.dex_fee_amount + repr.dex_fee_burn).into(),
        lock_duration: repr.lock_duration as i64,
        maker_coin_confs: repr.conf_settings.maker_coin_confs as i64,
        maker_coin_nota: repr.conf_settings.maker_coin_nota,
        taker_coin_confs: repr.conf_settings.taker_coin_confs as i64,
        taker_coin_nota: repr.conf_settings.taker_coin_nota,
        swap_version: repr.swap_version,
        maker_coin_usd_price: String::new(),
        taker_coin_usd_price: String::new(),
    }))
}

/// Pulls the `(repr, filter)` row pair from IndexedDB.
///
/// The two reads happen against the same transaction so that a swap
/// being concurrently finalised cannot present a torn view (saved-swap
/// updated but filter row still flagged as in-flight).
#[cfg(target_arch = "wasm32")]
async fn load_v2_swap_repr<R: DeserializeOwned>(
    ctx: &MmArc,
    uuid: &Uuid,
) -> MmResult<Option<(R, super::swap_wasm_db::MySwapsFiltersTable)>, SwapV2DbError> {
    let swaps_ctx = SwapsContext::from_ctx(ctx).unwrap();
    let db = swaps_ctx.swap_db().await.mm_err(Into::into)?;
    let transaction = db.transaction().await.mm_err(Into::into)?;

    let saved_swaps = transaction.table::<SavedSwapTable>().await.mm_err(Into::into)?;
    let saved_row = match saved_swaps
        .get_item_by_unique_index("uuid", uuid)
        .await
        .mm_err(Into::into)?
    {
        Some((_, item)) => item,
        None => return Ok(None),
    };

    let filters = transaction.table::<MySwapsFiltersTable>().await.mm_err(Into::into)?;
    let filter_row = match filters
        .get_item_by_unique_index("uuid", uuid)
        .await
        .mm_err(Into::into)?
    {
        Some((_, item)) => item,
        None => return Ok(None),
    };

    let repr: R = serde_json::from_value(saved_row.saved_swap)?;
    Ok(Some((repr, filter_row)))
}

// Wire envelope ------------------------------------------------------------

/// Single-swap response variant — tagged on the wire as
/// `{"swap_type":"<MakerV1|TakerV1|MakerV2|TakerV2>","swap_data":{...}}`.
#[derive(Serialize)]
#[serde(tag = "swap_type", content = "swap_data")]
pub(crate) enum SwapRpcData {
    MakerV1(MakerSavedSwap),
    TakerV1(TakerSavedSwap),
    MakerV2(MySwapForRpc<MakerSwapEvent>),
    TakerV2(MySwapForRpc<TakerSwapEvent>),
}

/// Internal collector for "fetch one swap by uuid" failure modes.
///
/// Not serialized to the wire — each public RPC error converts these
/// into its own variant set.
#[derive(Display)]
enum FetchSwapErr {
    UnsupportedSwapType(u8),
    DbError(String),
}

impl From<SavedSwapError> for FetchSwapErr {
    fn from(e: SavedSwapError) -> Self { FetchSwapErr::DbError(e.to_string()) }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<SqlError> for FetchSwapErr {
    fn from(e: SqlError) -> Self { FetchSwapErr::DbError(e.to_string()) }
}

#[cfg(target_arch = "wasm32")]
impl From<SwapV2DbError> for FetchSwapErr {
    fn from(e: SwapV2DbError) -> Self { FetchSwapErr::DbError(e.to_string()) }
}

/// Dispatches to the right backing store based on the persisted
/// `swap_type` discriminant, then projects the row into a
/// [`SwapRpcData`] variant.
async fn fetch_swap_data(ctx: &MmArc, uuid: Uuid, swap_type: u8) -> MmResult<Option<SwapRpcData>, FetchSwapErr> {
    match swap_type {
        LEGACY_SWAP_TYPE => {
            let saved = SavedSwap::load_my_swap_from_db(ctx, uuid).await.mm_err(Into::into)?;
            Ok(saved.map(|swap| match swap {
                SavedSwap::Maker(m) => SwapRpcData::MakerV1(m),
                SavedSwap::Taker(t) => SwapRpcData::TakerV1(t),
            }))
        },
        MAKER_SWAP_V2_TYPE => {
            let row = get_maker_swap_data_for_rpc(ctx, &uuid).await.mm_err(Into::into)?;
            Ok(row.map(SwapRpcData::MakerV2))
        },
        TAKER_SWAP_V2_TYPE => {
            let row = get_taker_swap_data_for_rpc(ctx, &uuid).await.mm_err(Into::into)?;
            Ok(row.map(SwapRpcData::TakerV2))
        },
        unknown => MmError::err(FetchSwapErr::UnsupportedSwapType(unknown)),
    }
}

// `my_swap_status` RPC -----------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct MySwapStatusRequest {
    uuid: Uuid,
}

/// Public error for `my_swap_status`. Variant names are wire-stable.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub(crate) enum MySwapStatusError {
    NoSwapWithUuid(Uuid),
    UnsupportedSwapType(u8),
    DbError(String),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<SqlError> for MySwapStatusError {
    fn from(e: SqlError) -> Self { MySwapStatusError::DbError(e.to_string()) }
}

#[cfg(target_arch = "wasm32")]
impl From<SwapV2DbError> for MySwapStatusError {
    fn from(e: SwapV2DbError) -> Self { MySwapStatusError::DbError(e.to_string()) }
}

impl From<FetchSwapErr> for MySwapStatusError {
    fn from(e: FetchSwapErr) -> Self {
        match e {
            FetchSwapErr::UnsupportedSwapType(t) => MySwapStatusError::UnsupportedSwapType(t),
            FetchSwapErr::DbError(m) => MySwapStatusError::DbError(m),
        }
    }
}

impl HttpStatusCode for MySwapStatusError {
    fn status_code(&self) -> StatusCode {
        match self {
            MySwapStatusError::NoSwapWithUuid(_) => StatusCode::BAD_REQUEST,
            MySwapStatusError::DbError(_) | MySwapStatusError::UnsupportedSwapType(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

/// Returns the full saved/in-flight state of one swap by UUID.
pub(crate) async fn my_swap_status_rpc(
    ctx: MmArc,
    req: MySwapStatusRequest,
) -> MmResult<SwapRpcData, MySwapStatusError> {
    let swap_type = get_swap_type(&ctx, &req.uuid)
        .await
        .mm_err(Into::into)?
        .or_mm_err(|| MySwapStatusError::NoSwapWithUuid(req.uuid))?;

    fetch_swap_data(&ctx, req.uuid, swap_type)
        .await
        .mm_err(Into::into)?
        .or_mm_err(|| MySwapStatusError::NoSwapWithUuid(req.uuid))
}

// `my_recent_swaps` RPC ----------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct MyRecentSwapsRequest {
    #[serde(flatten)]
    pub paging_options: PagingOptions,
    #[serde(flatten)]
    pub filter: MySwapsFilter,
}

#[derive(Serialize)]
pub(crate) struct MyRecentSwapsResponse {
    swaps: Vec<SwapRpcData>,
    from_uuid: Option<Uuid>,
    skipped: usize,
    limit: usize,
    total: usize,
    page_number: NonZeroUsize,
    total_pages: usize,
    found_records: usize,
}

/// Public error for `my_recent_swaps`. Variant names are wire-stable.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub(crate) enum MyRecentSwapsErr {
    FromUuidSwapNotFound(Uuid),
    InvalidTimeStampRange,
    DbError(String),
}

impl From<MySwapsError> for MyRecentSwapsErr {
    fn from(e: MySwapsError) -> Self {
        match e {
            MySwapsError::InvalidTimestampRange => MyRecentSwapsErr::InvalidTimeStampRange,
            MySwapsError::FromUuidNotFound(uuid) => MyRecentSwapsErr::FromUuidSwapNotFound(uuid),
            other => MyRecentSwapsErr::DbError(other.to_string()),
        }
    }
}

impl HttpStatusCode for MyRecentSwapsErr {
    fn status_code(&self) -> StatusCode {
        match self {
            MyRecentSwapsErr::FromUuidSwapNotFound(_) | MyRecentSwapsErr::InvalidTimeStampRange => {
                StatusCode::BAD_REQUEST
            },
            MyRecentSwapsErr::DbError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Returns a paged window of recent swaps, optionally narrowed by
/// the [`MySwapsFilter`] criteria carried inside the request.
pub(crate) async fn my_recent_swaps_rpc(
    ctx: MmArc,
    req: MyRecentSwapsRequest,
) -> MmResult<MyRecentSwapsResponse, MyRecentSwapsErr> {
    let db_result = MySwapsStorage::new(ctx.clone())
        .my_recent_swaps_with_filters(&req.filter, Some(&req.paging_options))
        .await
        .mm_err(Into::into)?;

    // Walk the page row-by-row; one missing detail row should not
    // collapse the whole page, so failures are logged and dropped.
    let mut swaps = Vec::with_capacity(db_result.uuids_and_types.len());
    for (uuid, swap_type) in db_result.uuids_and_types.iter() {
        match fetch_swap_data(&ctx, *uuid, *swap_type).await {
            Ok(Some(data)) => swaps.push(data),
            Ok(None) => warn!("Swap {} data doesn't exist in DB", uuid),
            Err(e) => error!("Error {} while trying to get swap {} data", e, uuid),
        }
    }

    Ok(MyRecentSwapsResponse {
        swaps,
        from_uuid: req.paging_options.from_uuid,
        skipped: db_result.skipped,
        limit: req.paging_options.limit,
        total: db_result.total_count,
        page_number: req.paging_options.page_number,
        total_pages: calc_total_pages(db_result.total_count, req.paging_options.limit),
        found_records: db_result.uuids_and_types.len(),
    })
}

// `active_swaps` RPC -------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ActiveSwapsRequest {
    #[serde(default)]
    include_status: bool,
    coin: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ActiveSwapsResponse {
    uuids: Vec<Uuid>,
    statuses: HashMap<Uuid, SwapRpcData>,
}

/// Public error for `active_swaps`. Variant names are wire-stable.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub(crate) enum ActiveSwapsErr {
    Internal(String),
}

impl HttpStatusCode for ActiveSwapsErr {
    fn status_code(&self) -> StatusCode {
        match self {
            ActiveSwapsErr::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Returns the UUIDs of in-flight swaps and, when `include_status` is
/// set, the same per-swap status payload as [`my_swap_status_rpc`].
pub(crate) async fn active_swaps_rpc(
    ctx: MmArc,
    req: ActiveSwapsRequest,
) -> MmResult<ActiveSwapsResponse, ActiveSwapsErr> {
    let mut uuids_with_types = active_swaps(&ctx).map_to_mm(ActiveSwapsErr::Internal)?;
    if let Some(coin) = req.coin {
        let allowed: std::collections::HashSet<_> = active_swaps_using_coin(&ctx, &coin)
            .map_to_mm(ActiveSwapsErr::Internal)?
            .into_iter()
            .collect();
        uuids_with_types.retain(|(uuid, _)| allowed.contains(uuid));
    }

    let statuses = if req.include_status {
        let mut acc = HashMap::with_capacity(uuids_with_types.len());
        for (uuid, swap_type) in uuids_with_types.iter() {
            match fetch_swap_data(&ctx, *uuid, *swap_type).await {
                Ok(Some(data)) => {
                    acc.insert(*uuid, data);
                },
                Ok(None) => warn!("Swap {} data doesn't exist in DB", uuid),
                Err(e) => error!("Error {} while trying to get swap {} data", e, uuid),
            }
        }
        acc
    } else {
        HashMap::new()
    };

    Ok(ActiveSwapsResponse {
        uuids: uuids_with_types.into_iter().map(|(uuid, _)| uuid).collect(),
        statuses,
    })
}
