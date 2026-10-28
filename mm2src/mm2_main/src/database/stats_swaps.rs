use crate::mm2::lp_swap::{MakerSavedSwap, MakerSwapEvent, SavedSwap, SavedSwapIo, TakerSavedSwap, TakerSwapEvent};
use common::log::{debug, error};
use db_common::sqlite::rusqlite::{params_from_iter, Connection, OptionalExtension};
use mm2_core::mm_ctx::MmArc;
use rpc::v1::types::H264 as H264Json;
use std::collections::HashSet;

const CREATE_STATS_SWAPS_TABLE: &str = "CREATE TABLE IF NOT EXISTS stats_swaps (
    id INTEGER NOT NULL PRIMARY KEY,
    maker_coin VARCHAR(255) NOT NULL,
    taker_coin VARCHAR(255) NOT NULL,
    uuid VARCHAR(255) NOT NULL UNIQUE,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    maker_amount DECIMAL NOT NULL,
    taker_amount DECIMAL NOT NULL,
    is_success INTEGER NOT NULL
);";

const INSERT_STATS_SWAP_ON_INIT: &str = "INSERT INTO stats_swaps (
    maker_coin,
    taker_coin,
    uuid,
    started_at,
    finished_at,
    maker_amount,
    taker_amount,
    is_success
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

// The public-key columns are not inserted: they are set afterwards from the record's own role only (CRD ch.51
// R50A item 5), so an unknown key stays NULL.
const INSERT_STATS_SWAP: &str = "INSERT INTO stats_swaps (
    maker_coin,
    maker_coin_ticker,
    maker_coin_platform,
    taker_coin,
    taker_coin_ticker,
    taker_coin_platform,
    uuid,
    started_at,
    finished_at,
    maker_amount,
    taker_amount,
    is_success,
    maker_coin_usd_price,
    taker_coin_usd_price,
    maker_gui,
    taker_gui,
    maker_version,
    taker_version
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

pub const ADD_FIAT_SNAPSHOT_COLUMNS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_usd_price DECIMAL;",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_usd_price DECIMAL;",
];

pub const ADD_PUBKEY_COLUMNS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_pubkey VARCHAR(255);",
    "ALTER TABLE stats_swaps ADD COLUMN taker_pubkey VARCHAR(255);",
];

pub const ADD_GUI_AND_VERSION_COLUMNS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_gui VARCHAR(255);",
    "ALTER TABLE stats_swaps ADD COLUMN taker_gui VARCHAR(255);",
    "ALTER TABLE stats_swaps ADD COLUMN maker_version VARCHAR(255);",
    "ALTER TABLE stats_swaps ADD COLUMN taker_version VARCHAR(255);",
];

const ADD_SPLIT_TICKERS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_platform VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_platform VARCHAR(255) NOT NULL DEFAULT '';",
    "UPDATE stats_swaps SET maker_coin_ticker = CASE instr(maker_coin, '-') \
        WHEN 0 THEN maker_coin \
        ELSE substr(maker_coin, 0, instr(maker_coin, '-')) \
        END;",
    "UPDATE stats_swaps SET maker_coin_platform = CASE instr(maker_coin, '-') \
        WHEN 0 THEN '' \
        ELSE substr(maker_coin, instr(maker_coin, '-') + 1) \
        END;",
    "UPDATE stats_swaps SET taker_coin_ticker = CASE instr(taker_coin, '-') \
        WHEN 0 THEN taker_coin \
        ELSE substr(taker_coin, 0, instr(taker_coin, '-')) \
        END;",
    "UPDATE stats_swaps SET taker_coin_platform = CASE instr(taker_coin, '-') \
        WHEN 0 THEN '' \
        ELSE substr(taker_coin, instr(taker_coin, '-') + 1) \
        END;",
];

pub const ADD_STARTED_AT_INDEX: &str = "CREATE INDEX timestamp_index ON stats_swaps (started_at);";

const SELECT_ID_BY_UUID: &str = "SELECT id FROM stats_swaps WHERE uuid = ?1";
const UPDATE_MAKER_PUBKEY_BY_UUID: &str = "UPDATE stats_swaps SET maker_pubkey = ?1 WHERE uuid = ?2";
const UPDATE_TAKER_PUBKEY_BY_UUID: &str = "UPDATE stats_swaps SET taker_pubkey = ?1 WHERE uuid = ?2";

#[derive(Clone, Debug)]
pub struct FiatPriceSnapshot {
    pub maker_coin_usd_price: String,
    pub taker_coin_usd_price: String,
}

/// Returns SQL statements to initially fill stats_swaps table using existing DB with JSON files
pub async fn create_and_fill_stats_swaps_from_json_statements(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    let maker_swaps = SavedSwap::load_all_from_maker_stats_db(ctx).await.unwrap_or_default();
    let taker_swaps = SavedSwap::load_all_from_taker_stats_db(ctx).await.unwrap_or_default();

    let mut result = vec![(CREATE_STATS_SWAPS_TABLE, vec![])];
    let mut inserted_maker_uuids = HashSet::with_capacity(maker_swaps.len());

    for maker_swap in maker_swaps {
        if let Some(sql_with_params) = insert_stats_maker_swap_sql_init(&maker_swap) {
            inserted_maker_uuids.insert(maker_swap.uuid);
            result.push(sql_with_params);
        }
    }
    for taker_swap in taker_swaps {
        if inserted_maker_uuids.contains(&taker_swap.uuid) {
            continue;
        }
        if let Some(sql_with_params) = insert_stats_taker_swap_sql_init(&taker_swap) {
            result.push(sql_with_params);
        }
    }
    result
}

fn split_coin(coin: &str) -> (String, String) {
    let mut split = coin.split('-');
    let ticker = split.next().expect("split returns empty string at least").into();
    let platform = split.next().map_or("".into(), |platform| platform.into());
    (ticker, platform)
}

fn insert_stats_maker_swap_sql(
    swap: &MakerSavedSwap,
    fiat_snapshot: Option<&FiatPriceSnapshot>,
) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let (maker_coin_ticker, maker_coin_platform) = split_coin(&swap_data.maker_coin);
    let (taker_coin_ticker, taker_coin_platform) = split_coin(&swap_data.taker_coin);

    let (maker_coin_usd_price, taker_coin_usd_price) = fiat_snapshot
        .map(|snapshot| {
            (
                snapshot.maker_coin_usd_price.clone(),
                snapshot.taker_coin_usd_price.clone(),
            )
        })
        .unwrap_or_default();

    let params = vec![
        swap_data.maker_coin.clone(),
        maker_coin_ticker,
        maker_coin_platform,
        swap_data.taker_coin.clone(),
        taker_coin_ticker,
        taker_coin_platform,
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
        maker_coin_usd_price,
        taker_coin_usd_price,
        swap.gui.clone().unwrap_or_default(),
        String::new(),
        swap.mm_version.clone().unwrap_or_default(),
        String::new(),
    ];
    Some((INSERT_STATS_SWAP, params))
}

fn insert_stats_maker_swap_sql_init(swap: &MakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let params = vec![
        swap_data.maker_coin.clone(),
        swap_data.taker_coin.clone(),
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
    ];
    Some((INSERT_STATS_SWAP_ON_INIT, params))
}

fn insert_stats_taker_swap_sql(
    swap: &TakerSavedSwap,
    fiat_snapshot: Option<&FiatPriceSnapshot>,
) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let (maker_coin_ticker, maker_coin_platform) = split_coin(&swap_data.maker_coin);
    let (taker_coin_ticker, taker_coin_platform) = split_coin(&swap_data.taker_coin);

    let (maker_coin_usd_price, taker_coin_usd_price) = fiat_snapshot
        .map(|snapshot| {
            (
                snapshot.maker_coin_usd_price.clone(),
                snapshot.taker_coin_usd_price.clone(),
            )
        })
        .unwrap_or_default();

    let params = vec![
        swap_data.maker_coin.clone(),
        maker_coin_ticker,
        maker_coin_platform,
        swap_data.taker_coin.clone(),
        taker_coin_ticker,
        taker_coin_platform,
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
        maker_coin_usd_price,
        taker_coin_usd_price,
        String::new(),
        swap.gui.clone().unwrap_or_default(),
        String::new(),
        swap.mm_version.clone().unwrap_or_default(),
    ];
    Some((INSERT_STATS_SWAP, params))
}

fn insert_stats_taker_swap_sql_init(swap: &TakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let params = vec![
        swap_data.maker_coin.clone(),
        swap_data.taker_coin.clone(),
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
    ];
    Some((INSERT_STATS_SWAP_ON_INIT, params))
}

/// Stores a received or self-published terminal swap record in `stats_swaps` (CRD ch.51 R50A item 5).
///
/// A record whose swap identifier is not yet present creates the row; a record whose identifier is present
/// creates nothing. In both cases the public-key column of the record's own role is then set from the record.
pub fn add_swap_to_index(conn: &Connection, swap: &SavedSwap, fiat_snapshot: Option<&FiatPriceSnapshot>) {
    let params = vec![swap.uuid().to_string()];
    let query_row = conn.query_row(SELECT_ID_BY_UUID, params_from_iter(params.iter()), |row| {
        row.get::<_, i64>(0)
    });
    match query_row.optional() {
        // swap is not indexed yet, go ahead
        Ok(None) => {
            let sql_with_params = match swap {
                SavedSwap::Maker(maker) => insert_stats_maker_swap_sql(maker, fiat_snapshot),
                SavedSwap::Taker(taker) => insert_stats_taker_swap_sql(taker, fiat_snapshot),
            };

            let (sql, params) = match sql_with_params {
                Some(tuple) => tuple,
                None => return,
            };

            debug!("Executing query {} with params {:?}", sql, params);
            if let Err(e) = conn.execute(sql, params_from_iter(params.iter())) {
                error!("Error {} on query {} with params {:?}", e, sql, params);
                return;
            };
        },
        // swap is already indexed, only the own-role key column is updated below
        Ok(Some(_)) => (),
        Err(e) => {
            error!("Error {} on query {} with params {:?}", e, SELECT_ID_BY_UUID, params);
            return;
        },
    };

    let update = match swap {
        SavedSwap::Maker(maker) => maker_pubkey_update_statement(maker),
        SavedSwap::Taker(taker) => taker_pubkey_update_statement(taker),
    };
    if let Some((sql, params)) = update {
        debug!("Executing query {} with params {:?}", sql, params);
        if let Err(e) = conn.execute(sql, params_from_iter(params.iter())) {
            error!("Error {} on query {} with params {:?}", e, sql, params);
        };
    }
}

pub fn add_and_split_tickers() -> Vec<(&'static str, Vec<String>)> {
    ADD_SPLIT_TICKERS.iter().map(|sql| (*sql, vec![])).collect()
}

pub fn add_fiat_snapshot_columns() -> Vec<(&'static str, Vec<String>)> {
    ADD_FIAT_SNAPSHOT_COLUMNS.iter().map(|sql| (*sql, vec![])).collect()
}

pub fn add_pubkey_columns() -> Vec<(&'static str, Vec<String>)> {
    ADD_PUBKEY_COLUMNS.iter().map(|sql| (*sql, vec![])).collect()
}

pub fn add_gui_and_version_columns() -> Vec<(&'static str, Vec<String>)> {
    ADD_GUI_AND_VERSION_COLUMNS.iter().map(|sql| (*sql, vec![])).collect()
}

/// The state-15 backfill (CRD ch.44 R44.6.3): sets, for every historical record, the public-key column of the
/// record's own role only, by the same rule as a live report (CRD ch.51 R50A item 5).
pub async fn backfill_pubkey_statements(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    let maker_swaps = SavedSwap::load_all_from_maker_stats_db(ctx).await.unwrap_or_default();
    let taker_swaps = SavedSwap::load_all_from_taker_stats_db(ctx).await.unwrap_or_default();

    maker_swaps
        .iter()
        .filter_map(maker_pubkey_update_statement)
        .chain(taker_swaps.iter().filter_map(taker_pubkey_update_statement))
        .collect()
}

fn maker_pubkey_update_statement(swap: &MakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let maker_pubkey = maker_own_pubkey(swap)?;
    Some((UPDATE_MAKER_PUBKEY_BY_UUID, vec![maker_pubkey, swap.uuid.to_string()]))
}

fn taker_pubkey_update_statement(swap: &TakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let taker_pubkey = taker_own_pubkey(swap)?;
    Some((UPDATE_TAKER_PUBKEY_BY_UUID, vec![taker_pubkey, swap.uuid.to_string()]))
}

/// The maker's persistent public key from the start event of a maker record. `None` when the record has no start
/// event or the key is all-zero, i.e. redacted by a privacy-coin author (CRD ch.51 R50A item 3).
fn maker_own_pubkey(swap: &MakerSavedSwap) -> Option<String> {
    swap.events.first().and_then(|event| match &event.event {
        MakerSwapEvent::Started(data) if data.my_persistent_pub != H264Json::default() => {
            Some(format!("{:02x}", data.my_persistent_pub))
        },
        _ => None,
    })
}

/// The taker's persistent public key from the start event of a taker record; see [`maker_own_pubkey`].
fn taker_own_pubkey(swap: &TakerSavedSwap) -> Option<String> {
    swap.events.first().and_then(|event| match &event.event {
        TakerSwapEvent::Started(data) if data.my_persistent_pub != H264Json::default() => {
            Some(format!("{:02x}", data.my_persistent_pub))
        },
        _ => None,
    })
}

#[test]
fn test_split_coin() {
    let input = "";
    let expected = ("".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "DOC";
    let expected = ("DOC".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "DOC-BEP20";
    let expected = ("DOC".into(), "BEP20".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "DOC-";
    let expected = ("DOC".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);
}
