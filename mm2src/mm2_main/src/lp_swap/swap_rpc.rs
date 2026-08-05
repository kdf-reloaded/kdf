use super::*;
#[cfg(not(target_arch = "wasm32"))]
use crate::mm2::database::stats_swaps::FiatPriceSnapshot;
#[cfg(not(target_arch = "wasm32"))]
use mm2_net::transport::slurp_url;

#[cfg(not(target_arch = "wasm32"))]
const PRICE_SERVICE_ENDPOINT: &str = "https://prices.komodo.live:1313/api/v2/tickers";

#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct FiatTickerInfo {
    last_price: BigDecimal,
}

#[cfg(not(target_arch = "wasm32"))]
fn strip_platform_suffix(ticker: &str) -> &str { ticker.split('-').next().unwrap_or(ticker) }

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_completion_fiat_snapshot(swap: &SavedSwap) -> Option<FiatPriceSnapshot> {
    let maker_coin = swap.maker_coin_ticker().ok()?;
    let taker_coin = swap.taker_coin_ticker().ok()?;
    let maker_coin = strip_platform_suffix(&maker_coin);
    let taker_coin = strip_platform_suffix(&taker_coin);

    let (status, _, body) = slurp_url(PRICE_SERVICE_ENDPOINT).await.ok()?;
    if status != StatusCode::OK {
        return None;
    }

    let response = std::str::from_utf8(&body).ok()?.trim();
    let prices: HashMap<String, FiatTickerInfo> = serde_json::from_str(response).ok()?;

    let maker_coin_usd_price = prices.get(maker_coin)?.last_price.to_string();
    let taker_coin_usd_price = prices.get(taker_coin)?.last_price.to_string();

    Some(FiatPriceSnapshot {
        maker_coin_usd_price,
        taker_coin_usd_price,
    })
}

pub fn my_swaps_dir(ctx: &MmArc) -> PathBuf { ctx.dbdir().join("SWAPS").join("MY") }

pub fn my_swap_file_path(ctx: &MmArc, uuid: &Uuid) -> PathBuf { my_swaps_dir(ctx).join(format!("{}.json", uuid)) }

pub async fn insert_new_swap_to_db(
    ctx: MmArc,
    my_coin: &str,
    other_coin: &str,
    uuid: Uuid,
    started_at: u64,
) -> Result<(), String> {
    // Legacy V1 swaps default to LEGACY_SWAP_TYPE
    insert_new_swap_to_db_with_type(ctx, my_coin, other_coin, uuid, started_at, LEGACY_SWAP_TYPE).await
}

pub async fn insert_new_swap_to_db_with_type(
    ctx: MmArc,
    my_coin: &str,
    other_coin: &str,
    uuid: Uuid,
    started_at: u64,
    swap_type: u8,
) -> Result<(), String> {
    MySwapsStorage::new(ctx)
        .save_new_swap(my_coin, other_coin, uuid, started_at, swap_type)
        .await
        .map_err(|e| ERRL!("{}", e))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn save_stats_swap(ctx: &MmArc, swap: &SavedSwap) -> Result<(), String> {
    let fiat_snapshot = fetch_completion_fiat_snapshot(swap).await;

    try_s!(swap.save_to_stats_db(ctx).await);
    crate::mm2::database::stats_swaps::add_swap_to_index(&ctx.sqlite_connection(), swap, fiat_snapshot.as_ref());
    Ok(())
}

/// The helper structure that makes easier to parse the response for GUI devs
/// They won't have to parse the events themselves handling possible errors, index out of bounds etc.
#[derive(Debug, Serialize, Deserialize)]
pub struct MySwapInfo {
    pub my_coin: String,
    pub other_coin: String,
    pub my_amount: BigDecimal,
    pub other_amount: BigDecimal,
    pub started_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct SavedTradeFee {
    coin: String,
    amount: BigDecimal,
    #[serde(default)]
    paid_from_trading_vol: bool,
}

impl From<SavedTradeFee> for TradeFee {
    fn from(orig: SavedTradeFee) -> Self {
        // used to calculate locked amount so paid_from_trading_vol doesn't matter here
        TradeFee {
            coin: orig.coin,
            amount: orig.amount.into(),
            paid_from_trading_vol: orig.paid_from_trading_vol,
        }
    }
}

impl From<TradeFee> for SavedTradeFee {
    fn from(orig: TradeFee) -> Self {
        SavedTradeFee {
            coin: orig.coin,
            amount: orig.amount.into(),
            paid_from_trading_vol: orig.paid_from_trading_vol,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SwapError {
    pub(crate) error: String,
}

impl From<String> for SwapError {
    fn from(error: String) -> Self { SwapError { error } }
}

impl From<&str> for SwapError {
    fn from(e: &str) -> Self { SwapError { error: e.to_owned() } }
}

#[derive(Serialize)]
struct MySwapStatusResponse<'a> {
    #[serde(flatten)]
    swap: &'a SavedSwap,
    my_info: Option<MySwapInfo>,
    recoverable: bool,
}

impl<'a> From<&'a SavedSwap> for MySwapStatusResponse<'a> {
    fn from(swap: &'a SavedSwap) -> MySwapStatusResponse<'a> {
        MySwapStatusResponse {
            swap,
            my_info: swap.get_my_info(),
            recoverable: swap.is_recoverable(),
        }
    }
}

/// Returns the status of swap performed on `my` node
pub async fn my_swap_status(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let uuid: Uuid = try_s!(json::from_value(req["params"]["uuid"].clone()));
    let status = match SavedSwap::load_my_swap_from_db(&ctx, uuid).await {
        Ok(Some(status)) => status,
        Ok(None) => return Err("swap data is not found".to_owned()),
        Err(e) => return ERR!("{}", e),
    };

    let res_js = json!({ "result": MySwapStatusResponse::from(&status) });
    let res = try_s!(json::to_vec(&res_js));
    Ok(try_s!(Response::builder().body(res)))
}

#[cfg(target_arch = "wasm32")]
pub async fn stats_swap_status(_ctx: MmArc, _req: Json) -> Result<Response<Vec<u8>>, String> {
    ERR!("'stats_swap_status' is only supported in native mode")
}

/// Returns the status of requested swap, typically performed by other nodes and saved by `save_stats_swap_status`
#[cfg(not(target_arch = "wasm32"))]
pub async fn stats_swap_status(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let uuid: Uuid = try_s!(json::from_value(req["params"]["uuid"].clone()));

    let maker_status = try_s!(SavedSwap::load_from_maker_stats_db(&ctx, uuid).await);
    let taker_status = try_s!(SavedSwap::load_from_taker_stats_db(&ctx, uuid).await);

    if maker_status.is_none() && taker_status.is_none() {
        return ERR!("swap data is not found");
    }

    let res_js = json!({
        "result": {
            "maker": maker_status,
            "taker": taker_status,
        }
    });
    let res = try_s!(json::to_vec(&res_js));
    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SwapStatus {
    pub(crate) method: String,
    pub(crate) data: SavedSwap,
}

/// Broadcasts `my` swap status to P2P network
pub(crate) async fn broadcast_my_swap_status(ctx: &MmArc, uuid: Uuid) -> Result<(), String> {
    let mut status = match try_s!(SavedSwap::load_my_swap_from_db(ctx, uuid).await) {
        Some(status) => status,
        None => return ERR!("swap data is not found"),
    };
    status.hide_secrets();

    #[cfg(not(target_arch = "wasm32"))]
    try_s!(save_stats_swap(ctx, &status).await);

    let status = SwapStatus {
        method: "swapstatus".into(),
        data: status,
    };
    let msg = json::to_vec(&status).expect("Swap status ser should never fail");
    broadcast_p2p_msg(ctx, vec![swap_topic(&uuid)], msg, None);
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MySwapsFilter {
    pub my_coin: Option<String>,
    pub other_coin: Option<String>,
    pub from_timestamp: Option<u64>,
    pub to_timestamp: Option<u64>,
}

// TODO: Should return the result from SQL like in order history. So it can be clear the exact started_at time
// and the coins if they are not included in the filter request
/// Returns *all* uuids of swaps, which match the selected filter.
pub async fn all_swaps_uuids_by_filter(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let filter: MySwapsFilter = try_s!(json::from_value(req));
    let db_result = try_s!(
        MySwapsStorage::new(ctx)
            .my_recent_swaps_with_filters(&filter, None)
            .await
    );

    let uuids: Vec<Uuid> = db_result.uuids_and_types.iter().map(|(u, _)| *u).collect();
    let found_records = uuids.len();
    let res_js = json!({
        "result": {
            "uuids": uuids,
            "my_coin": filter.my_coin,
            "other_coin": filter.other_coin,
            "from_timestamp": filter.from_timestamp,
            "to_timestamp": filter.to_timestamp,
            "found_records": found_records,
        },
    });
    let res = try_s!(json::to_vec(&res_js));
    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Debug, Deserialize)]
pub struct MyRecentSwapsReq {
    #[serde(flatten)]
    pub paging_options: PagingOptions,
    #[serde(flatten)]
    pub filter: MySwapsFilter,
}

#[derive(Debug, Default, PartialEq)]
pub struct MyRecentSwapsUuids {
    /// UUIDs and types of swaps matching the query.
    /// The `u8` is the swap type discriminant (LEGACY_SWAP_TYPE / MAKER_SWAP_V2_TYPE / TAKER_SWAP_V2_TYPE).
    pub uuids_and_types: Vec<(Uuid, u8)>,
    /// Total count of swaps matching the query
    pub total_count: usize,
    /// The number of skipped UUIDs
    pub skipped: usize,
}

#[derive(Debug)]
pub struct MyRecentSwapsResponse {
    pub from_uuid: Option<Uuid>,
    pub limit: usize,
    pub skipped: usize,
    pub total: usize,
    pub found_records: usize,
    pub page_number: NonZeroUsize,
    pub total_pages: usize,
    pub swaps: Vec<SavedSwap>,
}

#[derive(Debug, Display, Deserialize, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MyRecentSwapsErr {
    #[display(fmt = "No such swap with the uuid '{}'", _0)]
    UUIDNotPresentInDb(Uuid),
    UnableToLoadSavedSwaps(SavedSwapError),
    #[display(fmt = "Unable to query swaps storage")]
    UnableToQuerySwapStorage,
}

pub type MyRecentSwapsResult = Result<MyRecentSwapsResponse, MmError<MyRecentSwapsErr>>;

fn recent_swap_or_log(uuid: &Uuid, loaded: SavedSwapResult<Option<SavedSwap>>) -> Option<SavedSwap> {
    match loaded {
        Ok(Some(swap)) => Some(swap),
        Ok(None) => {
            error!("No such swap with the uuid '{}'", uuid);
            None
        },
        Err(e) => {
            error!("Error loading a swap with the uuid '{}': {}", uuid, e);
            None
        },
    }
}

fn recent_swap_status_json(uuid: &Uuid, loaded: SavedSwapResult<Option<SavedSwap>>) -> Option<Json> {
    recent_swap_or_log(uuid, loaded).map(|swap| json::to_value(MySwapStatusResponse::from(&swap)).unwrap())
}

pub async fn my_recent_swaps(ctx: MmArc, req: MyRecentSwapsReq) -> MyRecentSwapsResult {
    let db_result = match MySwapsStorage::new(ctx.clone())
        .my_recent_swaps_with_filters(&req.filter, Some(&req.paging_options))
        .await
    {
        Ok(x) => x,
        Err(_) => return Err(MmError::new(MyRecentSwapsErr::UnableToQuerySwapStorage)),
    };

    let mut swaps = Vec::with_capacity(db_result.uuids_and_types.len());
    for (uuid, _swap_type) in db_result.uuids_and_types.iter() {
        if let Some(swap) = recent_swap_or_log(uuid, SavedSwap::load_my_swap_from_db(&ctx, *uuid).await) {
            swaps.push(swap);
        }
    }

    Ok(MyRecentSwapsResponse {
        from_uuid: req.paging_options.from_uuid,
        limit: req.paging_options.limit,
        skipped: db_result.skipped,
        total: db_result.total_count,
        found_records: db_result.uuids_and_types.len(),
        page_number: req.paging_options.page_number,
        total_pages: calc_total_pages(db_result.total_count, req.paging_options.limit),
        swaps,
    })
}

/// Returns the data of recent swaps of `my` node.
pub async fn my_recent_swaps_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: MyRecentSwapsReq = try_s!(json::from_value(req));
    let db_result = try_s!(
        MySwapsStorage::new(ctx.clone())
            .my_recent_swaps_with_filters(&req.filter, Some(&req.paging_options))
            .await
    );

    // iterate over uuids trying to parse the corresponding files content and add to result vector
    let mut swaps = Vec::with_capacity(db_result.uuids_and_types.len());
    for (uuid, _swap_type) in db_result.uuids_and_types.iter() {
        if let Some(swap_json) = recent_swap_status_json(uuid, SavedSwap::load_my_swap_from_db(&ctx, *uuid).await) {
            swaps.push(swap_json);
        }
    }

    let res_js = json!({
        "result": {
            "swaps": swaps,
            "from_uuid": req.paging_options.from_uuid,
            "skipped": db_result.skipped,
            "limit": req.paging_options.limit,
            "total": db_result.total_count,
            "page_number": req.paging_options.page_number,
            "total_pages": calc_total_pages(db_result.total_count, req.paging_options.limit),
            "found_records": db_result.uuids_and_types.len(),
        },
    });
    let res = try_s!(json::to_vec(&res_js));
    Ok(try_s!(Response::builder().body(res)))
}

/// Find out the swaps that need to be kick-started, continue from the point where swap was interrupted
/// Return the tickers of coins that must be enabled for swaps to continue
pub async fn swap_kick_starts(ctx: MmArc) -> Result<HashSet<String>, String> {
    let mut coins = HashSet::new();
    let swaps = try_s!(SavedSwap::load_all_my_swaps_from_db(&ctx).await);
    for swap in swaps {
        if swap.is_finished() {
            continue;
        }

        info!("Kick starting the swap {}", swap.uuid());
        let maker_coin_ticker = match swap.maker_coin_ticker() {
            Ok(t) => t,
            Err(e) => {
                error!("Error {} getting maker coin of swap: {}", e, swap.uuid());
                continue;
            },
        };
        let taker_coin_ticker = match swap.taker_coin_ticker() {
            Ok(t) => t,
            Err(e) => {
                error!("Error {} getting taker coin of swap {}", e, swap.uuid());
                continue;
            },
        };
        coins.insert(maker_coin_ticker.clone());
        coins.insert(taker_coin_ticker.clone());

        let ctx = ctx.clone();

        // kick-start the swap in a separate thread.
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            common::block_on(kickstart_thread_handler(
                ctx.clone(),
                swap,
                maker_coin_ticker,
                taker_coin_ticker,
            ))
        });

        #[cfg(target_arch = "wasm32")]
        common::executor::spawn(async move {
            kickstart_thread_handler(ctx, swap, maker_coin_ticker, taker_coin_ticker).await
        });
    }

    // === V2 maker swaps ===
    #[cfg(not(target_arch = "wasm32"))]
    {
        use mm2_state_machine::storable_state_machine::StateMachineStorage;
        use swap_v2_common::MakerSwapStorage;

        let maker_storage = MakerSwapStorage::new(ctx.clone());
        let unfinished_maker = try_s!(maker_storage.get_unfinished().await);
        for uuid in unfinished_maker {
            info!("Trying to kickstart maker V2 swap {}", uuid);
            let repr = match maker_storage.get_repr(uuid).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Error {} getting DB repr of maker swap {}", e, uuid);
                    continue;
                },
            };
            coins.insert(repr.maker_coin.clone());
            coins.insert(repr.taker_coin.clone());
            let ctx2 = ctx.clone();
            let maker_storage2 = maker_storage.clone();
            std::thread::spawn(move || {
                common::block_on(swap_v2_common::swap_kickstart_handler_for_maker(
                    ctx2,
                    repr,
                    maker_storage2,
                    uuid,
                ))
            });
        }
    }

    // === V2 taker swaps ===
    #[cfg(not(target_arch = "wasm32"))]
    {
        use mm2_state_machine::storable_state_machine::StateMachineStorage;
        use swap_v2_common::TakerSwapStorage;

        let taker_storage = TakerSwapStorage::new(ctx.clone());
        let unfinished_taker = try_s!(taker_storage.get_unfinished().await);
        for uuid in unfinished_taker {
            info!("Trying to kickstart taker V2 swap {}", uuid);
            let repr = match taker_storage.get_repr(uuid).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Error {} getting DB repr of taker swap {}", e, uuid);
                    continue;
                },
            };
            coins.insert(repr.maker_coin.clone());
            coins.insert(repr.taker_coin.clone());
            let ctx2 = ctx.clone();
            let taker_storage2 = taker_storage.clone();
            std::thread::spawn(move || {
                common::block_on(swap_v2_common::swap_kickstart_handler_for_taker(
                    ctx2,
                    repr,
                    taker_storage2,
                    uuid,
                ))
            });
        }
    }

    Ok(coins)
}

async fn kickstart_thread_handler(ctx: MmArc, swap: SavedSwap, maker_coin_ticker: String, taker_coin_ticker: String) {
    let taker_coin = loop {
        match lp_coinfind(&ctx, &taker_coin_ticker).await {
            Ok(Some(c)) => break c,
            Ok(None) => {
                info!(
                    "Can't kickstart the swap {} until the coin {} is activated",
                    swap.uuid(),
                    taker_coin_ticker
                );
                Timer::sleep(5.).await;
            },
            Err(e) => {
                error!("Error {} on {} find attempt", e, taker_coin_ticker);
                return;
            },
        };
    };

    let maker_coin = loop {
        match lp_coinfind(&ctx, &maker_coin_ticker).await {
            Ok(Some(c)) => break c,
            Ok(None) => {
                info!(
                    "Can't kickstart the swap {} until the coin {} is activated",
                    swap.uuid(),
                    maker_coin_ticker
                );
                Timer::sleep(5.).await;
            },
            Err(e) => {
                error!("Error {} on {} find attempt", e, maker_coin_ticker);
                return;
            },
        };
    };
    match swap {
        SavedSwap::Maker(saved_swap) => {
            run_maker_swap(
                RunMakerSwapInput::KickStart {
                    maker_coin,
                    taker_coin,
                    swap_uuid: saved_swap.uuid,
                },
                ctx,
            )
            .await;
        },
        SavedSwap::Taker(saved_swap) => {
            run_taker_swap(
                RunTakerSwapInput::KickStart {
                    maker_coin,
                    taker_coin,
                    swap_uuid: saved_swap.uuid,
                },
                ctx,
            )
            .await;
        },
    }
}

pub async fn coins_needed_for_kick_start(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let res = try_s!(json::to_vec(&json!({
        "result": *(try_s!(ctx.coins_needed_for_kick_start.lock()))
    })));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn recover_funds_of_swap(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let uuid: Uuid = try_s!(json::from_value(req["params"]["uuid"].clone()));
    let swap = match SavedSwap::load_my_swap_from_db(&ctx, uuid).await {
        Ok(Some(swap)) => swap,
        Ok(None) => return ERR!("swap data is not found"),
        Err(e) => return ERR!("{}", e),
    };

    let recover_data = try_s!(swap.recover_funds(ctx).await);
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "action": recover_data.action,
            "coin": recover_data.coin,
            "tx_hash": recover_data.transaction.tx_hash(),
            "tx_hex": BytesJson::from(recover_data.transaction.tx_hex()),
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn import_swaps(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let swaps: Vec<SavedSwap> = try_s!(json::from_value(req["swaps"].clone()));
    let mut imported = vec![];
    let mut skipped = HashMap::new();
    for swap in swaps {
        match swap.save_to_db(&ctx).await {
            Ok(_) => {
                if let Some(info) = swap.get_my_info() {
                    if let Err(e) = insert_new_swap_to_db(
                        ctx.clone(),
                        &info.my_coin,
                        &info.other_coin,
                        *swap.uuid(),
                        info.started_at,
                    )
                    .await
                    {
                        error!("Error {} on new swap insertion", e);
                    }
                }
                imported.push(swap.uuid().to_owned());
            },
            Err(e) => {
                skipped.insert(swap.uuid().to_owned(), ERRL!("{}", e));
            },
        }
    }
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "imported": imported,
            "skipped": skipped,
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Deserialize)]
struct ActiveSwapsReq {
    #[serde(default)]
    include_status: bool,
}

#[derive(Serialize)]
struct ActiveSwapsRes {
    uuids: Vec<Uuid>,
    statuses: Option<HashMap<Uuid, SavedSwap>>,
}

pub async fn active_swaps_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: ActiveSwapsReq = try_s!(json::from_value(req));
    let uuids_with_types = try_s!(active_swaps(&ctx));
    let uuids: Vec<Uuid> = uuids_with_types.iter().map(|(u, _)| *u).collect();
    let statuses = if req.include_status {
        let mut map = HashMap::new();
        for uuid in uuids.iter() {
            let status = match SavedSwap::load_my_swap_from_db(&ctx, *uuid).await {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(e) => {
                    error!("Error on loading_from_db: {}", e);
                    continue;
                },
            };
            map.insert(*uuid, status);
        }
        Some(map)
    } else {
        None
    };
    let result = ActiveSwapsRes { uuids, statuses };
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

#[cfg(test)]
mod lp_swap_tests {
    use coins::utxo::sat_from_big_decimal;
    use coins::{DexFee, DexFeeBurnDestination, MarketCoinOps, MmCoin, MmCoinEnum, TestCoin};
    use mm2_net_config::NetConfig;
    use mocktopus::mocking::*;
    use serialization::{deserialize, serialize};

    use super::*;

    /// Tests use the legacy mainnet netid 8762 fee parameters.
    fn test_net_cfg() -> &'static dyn NetConfig { mm2_net_config::net_config_or_panic(8762) }

    #[test]
    fn recent_swap_status_json_skips_missing_swap() {
        let uuid = Uuid::new_v4();

        assert!(recent_swap_status_json(&uuid, Ok(None)).is_none());
    }

    #[test]
    fn recent_swap_status_json_skips_unloadable_swap() {
        let uuid = Uuid::new_v4();
        let load_result = MmError::err(SavedSwapError::ErrorDeserializing("missing field `data`".into()));

        assert!(recent_swap_status_json(&uuid, load_result).is_none());
    }

    #[test]
    fn test_dex_fee_amount() {
        let net_cfg = test_net_cfg();
        let dex_fee_threshold = MmNumber::from("0.0001");

        let base = "BTC";
        let rel = "ETH";
        let amount = 1.into();
        let actual_fee = dex_fee_amount(net_cfg, base, rel, &amount, &dex_fee_threshold);
        let expected_fee = amount / 777u64.into();
        assert_eq!(expected_fee, actual_fee);

        let base = "KMD";
        let rel = "ETH";
        let amount = 1.into();
        let actual_fee = dex_fee_amount(net_cfg, base, rel, &amount, &dex_fee_threshold);
        let expected_fee = amount * (9, 7770).into();
        assert_eq!(expected_fee, actual_fee);

        let base = "BTC";
        let rel = "KMD";
        let amount = 1.into();
        let actual_fee = dex_fee_amount(net_cfg, base, rel, &amount, &dex_fee_threshold);
        let expected_fee = amount * (9, 7770).into();
        assert_eq!(expected_fee, actual_fee);

        let base = "BTC";
        let rel = "KMD";
        let amount: MmNumber = "0.001".parse::<BigDecimal>().unwrap().into();
        let actual_fee = dex_fee_amount(net_cfg, base, rel, &amount, &dex_fee_threshold);
        assert_eq!(dex_fee_threshold, actual_fee);
    }

    #[test]
    fn test_serde_swap_negotiation_data() {
        let data = SwapNegotiationData::default();
        let bytes = serialize(&data);
        let deserialized = deserialize(bytes.as_slice()).unwrap();
        assert_eq!(data, deserialized);
    }

    #[test]
    fn test_lp_atomic_locktime() {
        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: true,
            taker_coin_confs: 2,
            taker_coin_nota: true,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime() * 4;
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: true,
            taker_coin_confs: 2,
            taker_coin_nota: false,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime() * 4;
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: false,
            taker_coin_confs: 2,
            taker_coin_nota: true,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime() * 4;
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: false,
            taker_coin_confs: 2,
            taker_coin_nota: false,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime();
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "BTC";
        let taker_coin = "DEX";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: false,
            taker_coin_confs: 2,
            taker_coin_nota: false,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime() * 4;
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "BTC";
        let my_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 2,
            maker_coin_nota: false,
            taker_coin_confs: 2,
            taker_coin_nota: false,
        };
        let other_conf_settings = SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        };
        let expected = get_payment_locktime() * 4;
        let version = AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        };
        let actual = lp_atomic_locktime(maker_coin, taker_coin, version);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let expected = get_payment_locktime();
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let expected = get_payment_locktime();
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let expected = get_payment_locktime();
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "DEX";
        let expected = get_payment_locktime();
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);

        let maker_coin = "BTC";
        let taker_coin = "DEX";
        let expected = get_payment_locktime() * 10;
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);

        let maker_coin = "KMD";
        let taker_coin = "BTC";
        let expected = get_payment_locktime() * 10;
        let actual = lp_atomic_locktime(maker_coin, taker_coin, AtomicLocktimeVersion::V1);
        assert_eq!(expected, actual);
    }

    #[test]
    fn check_negotiation_data_serde() {
        // old message format should be deserialized to NegotiationDataMsg::V1
        let v1 = NegotiationDataV1 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: [0; 20],
            persistent_pubkey: vec![1; 33],
        };

        let expected = NegotiationDataMsg::V1(NegotiationDataV1 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: [0; 20],
            persistent_pubkey: vec![1; 33],
        });

        let serialized = rmp_serde::to_vec(&v1).unwrap();

        let deserialized: NegotiationDataMsg = rmp_serde::from_read_ref(serialized.as_slice()).unwrap();

        assert_eq!(deserialized, expected);

        // new message format should be deserialized to old
        let v2 = NegotiationDataMsg::V2(NegotiationDataV2 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: vec![0; 20],
            persistent_pubkey: vec![1; 33],
            maker_coin_swap_contract: vec![1; 20],
            taker_coin_swap_contract: vec![1; 20],
        });

        let expected = NegotiationDataV1 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: [0; 20],
            persistent_pubkey: vec![1; 33],
        };

        let serialized = rmp_serde::to_vec(&v2).unwrap();

        let deserialized: NegotiationDataV1 = rmp_serde::from_read_ref(serialized.as_slice()).unwrap();

        assert_eq!(deserialized, expected);

        // new message format should be deserialized to new
        let v2 = NegotiationDataMsg::V2(NegotiationDataV2 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: vec![0; 20],
            persistent_pubkey: vec![1; 33],
            maker_coin_swap_contract: vec![1; 20],
            taker_coin_swap_contract: vec![1; 20],
        });

        let serialized = rmp_serde::to_vec(&v2).unwrap();

        let deserialized: NegotiationDataMsg = rmp_serde::from_read_ref(serialized.as_slice()).unwrap();

        assert_eq!(deserialized, v2);

        let v3 = NegotiationDataMsg::V3(NegotiationDataV3 {
            started_at: 0,
            payment_locktime: 0,
            secret_hash: vec![0; 20],
            maker_coin_swap_contract: vec![1; 20],
            taker_coin_swap_contract: vec![1; 20],
            maker_coin_htlc_pub: vec![1; 33],
            taker_coin_htlc_pub: vec![1; 33],
        });

        // v3 must be deserialized to v3, backward compatibility is not required
        let serialized = rmp_serde::to_vec(&v3).unwrap();

        let deserialized: NegotiationDataMsg = rmp_serde::from_read_ref(serialized.as_slice()).unwrap();

        assert_eq!(deserialized, v3);
    }

    #[test]
    fn test_dex_fee_no_fee_amounts() {
        let fee = DexFee::NoFee;
        assert_eq!(fee.total_spend_amount(), MmNumber::from(0));
        assert_eq!(fee.fee_amount(), MmNumber::from(0));
        assert_eq!(fee.burn_amount(), MmNumber::from(0));
    }

    #[test]
    fn test_dex_fee_standard_amounts() {
        let fee = DexFee::Standard(MmNumber::from("1.5"));
        assert_eq!(fee.total_spend_amount(), MmNumber::from("1.5"));
        assert_eq!(fee.fee_amount(), MmNumber::from("1.5"));
        assert_eq!(fee.burn_amount(), MmNumber::from(0));
    }

    #[test]
    fn test_dex_fee_with_burn_amounts() {
        let fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.75"),
            burn_amount: MmNumber::from("0.25"),
            burn_destination: DexFeeBurnDestination::KmdOpReturn,
        };
        assert_eq!(fee.total_spend_amount(), MmNumber::from("1.0"));
        assert_eq!(fee.fee_amount(), MmNumber::from("0.75"));
        assert_eq!(fee.burn_amount(), MmNumber::from("0.25"));
    }

    #[test]
    fn test_dex_fee_display() {
        let fee = DexFee::NoFee;
        assert_eq!(format!("{}", fee), "NoFee");

        let fee = DexFee::Standard(MmNumber::from("1.5"));
        let display = format!("{}", fee);
        assert!(display.starts_with("Standard("));

        let fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.75"),
            burn_amount: MmNumber::from("0.25"),
            burn_destination: DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: vec![0x02; 33],
            },
        };
        let display = format!("{}", fee);
        assert!(display.starts_with("WithBurn("));
    }

    /// Netid 8762 enables the KMD-only 75/25 OP_RETURN burn policy.
    #[test]
    fn should_configure_kmd_burn_on_netid_8762() {
        let net_cfg = mm2_net_config::net_config_or_panic(8762);
        assert!(net_cfg.burn_enabled());
        assert_eq!(MmNumber::from(net_cfg.dex_fee_share()), MmNumber::from((3, 4)));
        assert!(net_cfg.burn_addr_raw_pubkey().is_empty());
    }

    /// The v3/netid-6133 reference emits a single standard fee output.
    #[test]
    fn should_disable_burn_on_netid_6133() {
        let net_cfg = mm2_net_config::net_config_or_panic(6133);
        assert!(!net_cfg.burn_enabled());
        assert_eq!(MmNumber::from(net_cfg.dex_fee_share()), MmNumber::from(1));
    }

    /// Helper: create a TestCoin wrapped in MmCoinEnum with min_tx_amount mocked.
    fn mock_taker_coin(ticker: &'static str) -> MmCoinEnum {
        TestCoin::ticker.mock_safe(move |_| MockResult::Return(ticker));
        TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from_str("0.00001").unwrap()));
        TestCoin::should_burn_directly.mock_safe(|_| MockResult::Return(false));
        TestCoin::should_burn_dex_fee.mock_safe(|_| MockResult::Return(false));
        MmCoinEnum::Test(TestCoin::new(ticker))
    }

    /// Non-KMD takers on netid 8762 retain the legacy single-output fee form.
    #[test]
    fn should_compute_standard_fee_for_non_kmd_taker_on_8762() {
        let net_cfg = mm2_net_config::net_config_or_panic(8762);
        let taker_coin = mock_taker_coin("BTC");
        let trade_amount = MmNumber::from(1);

        let fee = compute_dex_fee(net_cfg, &taker_coin, "ETH", &trade_amount);
        match &fee {
            DexFee::Standard(amount) => {
                assert!(*amount > MmNumber::from(0), "fee should be positive");
                // For BTC/ETH (no KMD discount), rate is 1/777
                let expected = &trade_amount / &MmNumber::from(777);
                assert_eq!(*amount, expected);
            },
            other => panic!("expected DexFee::Standard on netid 8762, got {:?}", other),
        }
    }

    /// All takers on netid 6133 use the v3 single-output fee form.
    #[test]
    fn should_compute_standard_fee_on_6133() {
        let net_cfg = mm2_net_config::net_config_or_panic(6133);
        let taker_coin = mock_taker_coin("BTC");
        let trade_amount = MmNumber::from(1);

        let fee = compute_dex_fee(net_cfg, &taker_coin, "ETH", &trade_amount);
        match fee {
            DexFee::Standard(amount) => assert_eq!(amount, &trade_amount * &MmNumber::from((2, 100))),
            other => panic!("expected DexFee::Standard on netid 6133, got {:?}", other),
        }
    }

    /// Regression for issue #1: a v2.6.0-beta KMD taker on netid 8762 sends
    /// 75% to the fee address and 25% to a second OP_RETURN output.
    #[test]
    fn should_match_v2_6_0_beta_kmd_fee_split_on_8762() {
        let net_cfg = mm2_net_config::net_config_or_panic(8762);
        let taker_coin = mock_taker_coin("KMD");
        TestCoin::should_burn_directly.mock_safe(|_| MockResult::Return(true));
        let trade_amount = MmNumber::from("15.86");

        let fee = compute_dex_fee(net_cfg, &taker_coin, "CHTA", &trade_amount);
        let total = &trade_amount * &MmNumber::from((9, 7770));
        let expected_fee = &total * &MmNumber::from((3, 4));
        let expected_burn = &total - &expected_fee;
        match &fee {
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                burn_destination,
            } => {
                assert_eq!(*fee_amount, expected_fee);
                assert_eq!(*burn_amount, expected_burn);
                assert_eq!(*burn_destination, DexFeeBurnDestination::KmdOpReturn);
                assert_eq!(sat_from_big_decimal(&total.to_decimal(), 8).unwrap(), 1_837_065);
                assert_eq!(sat_from_big_decimal(&fee_amount.to_decimal(), 8).unwrap(), 1_377_799);
                assert_eq!(sat_from_big_decimal(&burn_amount.to_decimal(), 8).unwrap(), 459_266);
            },
            other => panic!("expected KMD OP_RETURN split on netid 8762, got {:?}", other),
        }
    }

    /// A product between coin dust and the old 1/10000 override must pass
    /// through unchanged; the reference implementations use only coin dust.
    #[test]
    fn should_not_apply_network_fee_floor_above_coin_dust() {
        let net_cfg = mm2_net_config::net_config_or_panic(8762);
        let taker_coin = mock_taker_coin("BTC");
        let trade_amount = MmNumber::from("0.02");
        let expected = &trade_amount / &MmNumber::from(777);
        assert!(expected > MmNumber::from("0.00001"));
        assert!(expected < MmNumber::from("0.0001"));

        let fee = compute_dex_fee(net_cfg, &taker_coin, "ETH", &trade_amount);
        match fee {
            DexFee::Standard(amount) => assert_eq!(amount, expected),
            other => panic!("expected DexFee::Standard fallback, got {:?}", other),
        }
    }

    /// compute_dex_fee total_spend_amount should equal dex_fee_amount_from_taker_coin
    /// (the underlying total before the burn split).
    #[test]
    fn test_compute_dex_fee_total_matches_raw_calculation() {
        for &netid in &[8762u16, 6133] {
            let net_cfg = mm2_net_config::net_config_or_panic(netid);
            let taker_coin = mock_taker_coin("DOGE");
            let trade_amount = MmNumber::from(100);

            let expected_total = dex_fee_amount_from_taker_coin(net_cfg, &taker_coin, "BTC", &trade_amount);
            let fee = compute_dex_fee(net_cfg, &taker_coin, "BTC", &trade_amount);
            assert_eq!(
                fee.total_spend_amount(),
                expected_total,
                "total_spend_amount mismatch on netid {}",
                netid
            );
        }
    }
}
