/// The module responsible to work with SQLite database
///
#[path = "database/my_orders.rs"]
pub mod my_orders;
#[path = "database/my_swaps.rs"] pub mod my_swaps;
#[path = "database/stats_nodes.rs"] pub mod stats_nodes;
#[path = "database/stats_swaps.rs"] pub mod stats_swaps;

use crate::CREATE_MY_SWAPS_TABLE;
use common::log::{debug, error, info};
use db_common::sqlite::rusqlite::{params_from_iter, Result as SqlResult};
use mm2_core::mm_ctx::MmArc;

use my_swaps::{fill_my_swaps_from_json_statements, mark_finished_swaps_from_json_statements};
use stats_swaps::create_and_fill_stats_swaps_from_json_statements;

const SELECT_MIGRATION: &str = "SELECT * FROM migration ORDER BY current_migration DESC LIMIT 1;";
const INSERT_MIGRATION: &str = "INSERT INTO migration (current_migration) VALUES (?1);";

const CREATE_MY_SWAPS_GLEEC_STATE_15_TABLE: &str = "CREATE TABLE my_swaps (
    id INTEGER NOT NULL PRIMARY KEY,
    my_coin VARCHAR(255) NOT NULL,
    other_coin VARCHAR(255) NOT NULL,
    uuid VARCHAR(255) NOT NULL UNIQUE,
    started_at INTEGER NOT NULL,
    is_finished BOOLEAN NOT NULL DEFAULT 0,
    events_json TEXT NOT NULL DEFAULT '[]',
    swap_type INTEGER NOT NULL DEFAULT 0,
    maker_volume TEXT,
    taker_volume TEXT,
    premium TEXT,
    dex_fee TEXT,
    secret BLOB,
    secret_hash BLOB,
    secret_hash_algo INTEGER,
    p2p_privkey BLOB,
    lock_duration INTEGER,
    maker_coin_confs INTEGER,
    maker_coin_nota BOOLEAN,
    taker_coin_confs INTEGER,
    taker_coin_nota BOOLEAN,
    other_p2p_pub BLOB,
    dex_fee_burn TEXT,
    swap_version INTEGER
);";

const CREATE_STATS_SWAPS_GLEEC_STATE_15_TABLE: &str = "CREATE TABLE stats_swaps (
    id INTEGER NOT NULL PRIMARY KEY,
    maker_coin VARCHAR(255) NOT NULL,
    taker_coin VARCHAR(255) NOT NULL,
    uuid VARCHAR(255) NOT NULL UNIQUE,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    maker_amount DECIMAL NOT NULL,
    taker_amount DECIMAL NOT NULL,
    is_success INTEGER NOT NULL,
    maker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '',
    maker_coin_platform VARCHAR(255) NOT NULL DEFAULT '',
    taker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '',
    taker_coin_platform VARCHAR(255) NOT NULL DEFAULT '',
    maker_coin_usd_price DECIMAL,
    taker_coin_usd_price DECIMAL,
    maker_pubkey VARCHAR(255),
    taker_pubkey VARCHAR(255),
    maker_gui VARCHAR(255),
    taker_gui VARCHAR(255),
    maker_version VARCHAR(255),
    taker_version VARCHAR(255)
);";

fn get_current_migration(ctx: &MmArc) -> SqlResult<i64> {
    let conn = ctx.sqlite_connection();
    conn.query_row(SELECT_MIGRATION, [], |row| row.get(0))
}

fn table_has_column(ctx: &MmArc, table: &str, column: &str) -> SqlResult<bool> {
    let conn = ctx.sqlite_connection();
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn init_and_migrate_db(ctx: &MmArc) -> SqlResult<()> {
    info!("Checking the current SQLite migration");
    match get_current_migration(ctx) {
        Ok(current_migration) => {
            if current_migration >= 1 {
                info!(
                    "Current migration is {}, skipping the init, trying to migrate",
                    current_migration
                );
                migrate_sqlite_database(ctx, current_migration).await?;
                return Ok(());
            }
        },
        Err(e) => {
            debug!("Error '{}' on getting current migration. The database is either empty or corrupted, trying to clean it first", e);
            clean_db(ctx);
        },
    };

    info!("Trying to initialize the SQLite database");

    init_db(ctx)?;
    migrate_sqlite_database(ctx, 1).await?;
    info!("SQLite database initialization is successful");
    Ok(())
}

fn init_db(ctx: &MmArc) -> SqlResult<()> {
    let conn = ctx.sqlite_connection();
    let init_batch = concat!(
        "BEGIN;
        CREATE TABLE IF NOT EXISTS migration (current_migration INTEGER NOT_NULL UNIQUE);
        INSERT INTO migration (current_migration) VALUES (1);",
        CREATE_MY_SWAPS_TABLE!(),
        "COMMIT;"
    );
    conn.execute_batch(init_batch)
}

fn clean_db(ctx: &MmArc) {
    let conn = ctx.sqlite_connection();
    if let Err(e) = conn.execute_batch(
        "DROP TABLE migration;
                    DROP TABLE my_swaps;",
    ) {
        error!("Error {} on SQLite database cleanup", e);
    }
}

async fn migration_1(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> { fill_my_swaps_from_json_statements(ctx).await }

async fn migration_2(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    create_and_fill_stats_swaps_from_json_statements(ctx).await
}

fn migration_3() -> Vec<(&'static str, Vec<String>)> { vec![(stats_swaps::ADD_STARTED_AT_INDEX, vec![])] }

fn migration_4() -> Vec<(&'static str, Vec<String>)> { stats_swaps::add_and_split_tickers() }

fn migration_5() -> Vec<(&'static str, Vec<String>)> { vec![(my_orders::CREATE_MY_ORDERS_TABLE, vec![])] }

fn migration_6() -> Vec<(&'static str, Vec<String>)> {
    vec![
        (stats_nodes::CREATE_NODES_TABLE, vec![]),
        (stats_nodes::CREATE_STATS_NODES_TABLE, vec![]),
    ]
}

fn migration_7() -> Vec<(&'static str, Vec<String>)> { stats_swaps::add_fiat_snapshot_columns() }

fn migration_8() -> Vec<(&'static str, Vec<String>)> { stats_swaps::add_pubkey_columns() }

fn migration_9() -> Vec<(&'static str, Vec<String>)> {
    vec![
        (
            "ALTER TABLE my_swaps ADD COLUMN is_finished BOOLEAN NOT NULL DEFAULT 0;",
            vec![],
        ),
        (
            "ALTER TABLE my_swaps ADD COLUMN events_json TEXT NOT NULL DEFAULT '[]';",
            vec![],
        ),
        (
            "ALTER TABLE my_swaps ADD COLUMN swap_type INTEGER NOT NULL DEFAULT 0;",
            vec![],
        ),
        ("ALTER TABLE my_swaps ADD COLUMN maker_volume TEXT;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN taker_volume TEXT;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN premium TEXT;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN dex_fee TEXT;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN secret BLOB;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN secret_hash BLOB;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN secret_hash_algo INTEGER;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN p2p_privkey BLOB;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN lock_duration INTEGER;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN maker_coin_confs INTEGER;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN maker_coin_nota BOOLEAN;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN taker_coin_confs INTEGER;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN taker_coin_nota BOOLEAN;", vec![]),
    ]
}

async fn migration_10(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    mark_finished_swaps_from_json_statements(ctx).await
}

fn migration_11() -> Vec<(&'static str, Vec<String>)> { stats_swaps::add_gui_and_version_columns() }

fn migration_12() -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("ALTER TABLE my_swaps ADD COLUMN other_p2p_pub BLOB;", vec![]),
        ("ALTER TABLE my_swaps ADD COLUMN dex_fee_burn TEXT;", vec![]),
    ]
}

fn migration_13() -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("ALTER TABLE my_swaps ADD COLUMN swap_version INTEGER;", vec![]),
        (
            "UPDATE my_swaps SET swap_version = 1 WHERE swap_version IS NULL;",
            vec![],
        ),
    ]
}

async fn migration_14(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    stats_swaps::backfill_pubkey_statements(ctx).await
}

fn is_legacy_reloaded_schema(ctx: &MmArc, current_migration: i64) -> SqlResult<bool> {
    if !matches!(current_migration, 8 | 9) {
        return Ok(false);
    }
    table_has_column(ctx, "my_swaps", "swap_type")
}

fn legacy_price_expr(stats_has_price: bool, my_swaps_has_price: bool, column: &str) -> String {
    let stats_price = format!("NULLIF(s.{}, '')", column);
    let my_swaps_price = format!("NULLIF(m.{}, '')", column);
    match (stats_has_price, my_swaps_has_price) {
        (true, true) => format!("COALESCE({}, {})", stats_price, my_swaps_price),
        (true, false) => stats_price,
        (false, true) => my_swaps_price,
        (false, false) => "NULL".to_owned(),
    }
}

fn rebuild_legacy_reloaded_stats_swaps_sql(maker_price_expr: &str, taker_price_expr: &str) -> String {
    format!(
        "ALTER TABLE stats_swaps RENAME TO stats_swaps_reloaded_legacy;
        {create_stats_swaps}
        INSERT INTO stats_swaps (
            id,
            maker_coin,
            taker_coin,
            uuid,
            started_at,
            finished_at,
            maker_amount,
            taker_amount,
            is_success,
            maker_coin_ticker,
            maker_coin_platform,
            taker_coin_ticker,
            taker_coin_platform,
            maker_coin_usd_price,
            taker_coin_usd_price,
            maker_pubkey,
            taker_pubkey,
            maker_gui,
            taker_gui,
            maker_version,
            taker_version
        )
        SELECT
            s.id,
            s.maker_coin,
            s.taker_coin,
            s.uuid,
            s.started_at,
            s.finished_at,
            s.maker_amount,
            s.taker_amount,
            s.is_success,
            s.maker_coin_ticker,
            s.maker_coin_platform,
            s.taker_coin_ticker,
            s.taker_coin_platform,
            {maker_price_expr},
            {taker_price_expr},
            NULL,
            NULL,
            NULL,
            NULL,
            NULL,
            NULL
        FROM stats_swaps_reloaded_legacy s
        LEFT JOIN my_swaps m ON m.uuid = s.uuid;
        DROP TABLE stats_swaps_reloaded_legacy;
        {started_at_index}",
        create_stats_swaps = CREATE_STATS_SWAPS_GLEEC_STATE_15_TABLE,
        maker_price_expr = maker_price_expr,
        taker_price_expr = taker_price_expr,
        started_at_index = stats_swaps::ADD_STARTED_AT_INDEX,
    )
}

fn rebuild_legacy_reloaded_my_swaps_sql() -> String {
    format!(
        "ALTER TABLE my_swaps RENAME TO my_swaps_reloaded_legacy;
        {create_my_swaps}
        INSERT INTO my_swaps (
            id,
            my_coin,
            other_coin,
            uuid,
            started_at,
            is_finished,
            events_json,
            swap_type,
            maker_volume,
            taker_volume,
            premium,
            dex_fee,
            secret,
            secret_hash,
            secret_hash_algo,
            p2p_privkey,
            lock_duration,
            maker_coin_confs,
            maker_coin_nota,
            taker_coin_confs,
            taker_coin_nota,
            other_p2p_pub,
            dex_fee_burn,
            swap_version
        )
        SELECT
            id,
            my_coin,
            other_coin,
            uuid,
            started_at,
            COALESCE(is_finished, 0),
            COALESCE(events_json, '[]'),
            COALESCE(swap_type, 0),
            maker_volume,
            taker_volume,
            premium,
            dex_fee,
            secret,
            secret_hash,
            secret_hash_algo,
            p2p_privkey,
            lock_duration,
            maker_coin_confs,
            maker_coin_nota,
            taker_coin_confs,
            taker_coin_nota,
            other_p2p_pub,
            dex_fee_burn,
            CASE WHEN swap_version IS NULL OR swap_version = 0 THEN 1 ELSE swap_version END
        FROM my_swaps_reloaded_legacy;
        DROP TABLE my_swaps_reloaded_legacy;",
        create_my_swaps = CREATE_MY_SWAPS_GLEEC_STATE_15_TABLE,
    )
}

async fn repair_legacy_reloaded_schema(ctx: &MmArc, current_migration: i64) -> SqlResult<Option<i64>> {
    if !is_legacy_reloaded_schema(ctx, current_migration)? {
        return Ok(None);
    }

    info!(
        "Detected legacy RELOADED SQLite migration {}, repairing to GLEEC-compatible migration 15",
        current_migration
    );

    let stats_has_price = table_has_column(ctx, "stats_swaps", "maker_coin_usd_price")?
        && table_has_column(ctx, "stats_swaps", "taker_coin_usd_price")?;
    let my_swaps_has_price = table_has_column(ctx, "my_swaps", "maker_coin_usd_price")?
        && table_has_column(ctx, "my_swaps", "taker_coin_usd_price")?;
    let maker_price_expr = legacy_price_expr(stats_has_price, my_swaps_has_price, "maker_coin_usd_price");
    let taker_price_expr = legacy_price_expr(stats_has_price, my_swaps_has_price, "taker_coin_usd_price");
    let rebuild_stats_swaps = rebuild_legacy_reloaded_stats_swaps_sql(&maker_price_expr, &taker_price_expr);
    let rebuild_my_swaps = rebuild_legacy_reloaded_my_swaps_sql();

    let mark_finished_statements = mark_finished_swaps_from_json_statements(ctx).await;
    let backfill_pubkey_statements = stats_swaps::backfill_pubkey_statements(ctx).await;

    let conn = ctx.sqlite_connection();
    let transaction = conn.unchecked_transaction()?;
    transaction.execute_batch(&rebuild_stats_swaps)?;
    transaction.execute_batch(&rebuild_my_swaps)?;

    for (statement, params) in mark_finished_statements {
        debug!("Executing SQL statement {:?} with params {:?}", statement, params);
        transaction.execute(statement, params_from_iter(params.iter()))?;
    }
    for (statement, params) in backfill_pubkey_statements {
        debug!("Executing SQL statement {:?} with params {:?}", statement, params);
        transaction.execute(statement, params_from_iter(params.iter()))?;
    }
    for migration in (current_migration + 1)..=15 {
        transaction.execute(INSERT_MIGRATION, [migration])?;
    }
    transaction.commit()?;

    info!("Legacy RELOADED SQLite repair complete, migrated to 15");
    Ok(Some(15))
}

async fn statements_for_migration(ctx: &MmArc, current_migration: i64) -> Option<Vec<(&'static str, Vec<String>)>> {
    match current_migration {
        1 => Some(migration_1(ctx).await),
        2 => Some(migration_2(ctx).await),
        3 => Some(migration_3()),
        4 => Some(migration_4()),
        5 => Some(migration_5()),
        6 => Some(migration_6()),
        7 => Some(migration_7()),
        8 => Some(migration_8()),
        9 => Some(migration_9()),
        10 => Some(migration_10(ctx).await),
        11 => Some(migration_11()),
        12 => Some(migration_12()),
        13 => Some(migration_13()),
        14 => Some(migration_14(ctx).await),
        _ => None,
    }
}

pub async fn migrate_sqlite_database(ctx: &MmArc, mut current_migration: i64) -> SqlResult<()> {
    info!("migrate_sqlite_database, current migration {}", current_migration);
    if let Some(repaired_migration) = repair_legacy_reloaded_schema(ctx, current_migration).await? {
        current_migration = repaired_migration;
    }
    while let Some(statements_with_params) = statements_for_migration(ctx, current_migration).await {
        // `statements_for_migration` locks the [`MmCtx::sqlite_connection`] mutex,
        // so we can't create a transaction outside of this loop.
        let conn = ctx.sqlite_connection();
        let transaction = conn.unchecked_transaction()?;
        for (statement, params) in statements_with_params {
            debug!("Executing SQL statement {:?} with params {:?}", statement, params);
            transaction.execute(statement, params_from_iter(params.iter()))?;
        }
        current_migration += 1;
        transaction.execute(INSERT_MIGRATION, [current_migration])?;
        transaction.commit()?;
    }
    info!("migrate_sqlite_database complete, migrated to {}", current_migration);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use db_common::sqlite::rusqlite::Connection;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use std::sync::{Arc, Mutex};

    fn setup_test_ctx() -> MmArc {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let conn = Connection::open_in_memory().unwrap();
        let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(conn)));
        ctx
    }

    fn table_column_specs(ctx: &MmArc, table: &str) -> Vec<(String, String)> {
        let conn = ctx.sqlite_connection();
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
        stmt.query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<SqlResult<Vec<(String, String)>>>()
            .unwrap()
    }

    fn migration_rows(ctx: &MmArc) -> Vec<i64> {
        let conn = ctx.sqlite_connection();
        let mut stmt = conn
            .prepare("SELECT current_migration FROM migration ORDER BY current_migration")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<i64>>>()
            .unwrap()
    }

    fn seed_legacy_reloaded_schema(ctx: &MmArc, current_migration: i64) {
        let conn = ctx.sqlite_connection();
        conn.execute_batch(
            "CREATE TABLE migration (current_migration INTEGER NOT_NULL UNIQUE);
            CREATE TABLE my_swaps (
                id INTEGER NOT NULL PRIMARY KEY,
                my_coin VARCHAR(255) NOT NULL,
                other_coin VARCHAR(255) NOT NULL,
                uuid VARCHAR(255) NOT NULL UNIQUE,
                started_at INTEGER NOT NULL
            );
            CREATE TABLE stats_swaps (
                id INTEGER NOT NULL PRIMARY KEY,
                maker_coin VARCHAR(255) NOT NULL,
                taker_coin VARCHAR(255) NOT NULL,
                uuid VARCHAR(255) NOT NULL UNIQUE,
                started_at INTEGER NOT NULL,
                finished_at INTEGER NOT NULL,
                maker_amount DECIMAL NOT NULL,
                taker_amount DECIMAL NOT NULL,
                is_success INTEGER NOT NULL
            );",
        )
        .unwrap();

        for migration in 1..=current_migration {
            conn.execute(INSERT_MIGRATION, [migration]).unwrap();
        }

        for statement in stats_swaps::add_and_split_tickers() {
            conn.execute(statement.0, []).unwrap();
        }
        conn.execute(stats_swaps::ADD_STARTED_AT_INDEX, []).unwrap();
        conn.execute(my_orders::CREATE_MY_ORDERS_TABLE, []).unwrap();
        conn.execute(stats_nodes::CREATE_NODES_TABLE, []).unwrap();
        conn.execute(stats_nodes::CREATE_STATS_NODES_TABLE, []).unwrap();

        for statement in [
            "ALTER TABLE my_swaps ADD COLUMN swap_type INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN is_finished INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN events_json TEXT NOT NULL DEFAULT '[]';",
            "ALTER TABLE my_swaps ADD COLUMN maker_volume TEXT NOT NULL DEFAULT '';",
            "ALTER TABLE my_swaps ADD COLUMN taker_volume TEXT NOT NULL DEFAULT '';",
            "ALTER TABLE my_swaps ADD COLUMN premium TEXT NOT NULL DEFAULT '';",
            "ALTER TABLE my_swaps ADD COLUMN dex_fee TEXT NOT NULL DEFAULT '';",
            "ALTER TABLE my_swaps ADD COLUMN dex_fee_burn TEXT NOT NULL DEFAULT '';",
            "ALTER TABLE my_swaps ADD COLUMN secret BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000';",
            "ALTER TABLE my_swaps ADD COLUMN secret_hash BLOB NOT NULL DEFAULT X'';",
            "ALTER TABLE my_swaps ADD COLUMN secret_hash_algo INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN p2p_privkey BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000';",
            "ALTER TABLE my_swaps ADD COLUMN lock_duration INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN maker_coin_confs INTEGER NOT NULL DEFAULT 1;",
            "ALTER TABLE my_swaps ADD COLUMN maker_coin_nota INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN taker_coin_confs INTEGER NOT NULL DEFAULT 1;",
            "ALTER TABLE my_swaps ADD COLUMN taker_coin_nota INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE my_swaps ADD COLUMN other_p2p_pub BLOB NOT NULL DEFAULT X'';",
            "ALTER TABLE my_swaps ADD COLUMN swap_version INTEGER NOT NULL DEFAULT 0;",
        ] {
            conn.execute(statement, []).unwrap();
        }

        if current_migration == 9 {
            for statement in [
                "ALTER TABLE my_swaps ADD COLUMN maker_coin_usd_price TEXT NOT NULL DEFAULT '';",
                "ALTER TABLE my_swaps ADD COLUMN taker_coin_usd_price TEXT NOT NULL DEFAULT '';",
                "ALTER TABLE stats_swaps ADD COLUMN maker_coin_usd_price VARCHAR(255) NOT NULL DEFAULT '';",
                "ALTER TABLE stats_swaps ADD COLUMN taker_coin_usd_price VARCHAR(255) NOT NULL DEFAULT '';",
            ] {
                conn.execute(statement, []).unwrap();
            }
        }

        let price_columns = if current_migration == 9 {
            ", maker_coin_usd_price, taker_coin_usd_price"
        } else {
            ""
        };
        let my_swap_prices = if current_migration == 9 { ", '10.5', ''" } else { "" };
        let stats_prices = if current_migration == 9 { ", '', '20.25'" } else { "" };
        conn.execute_batch(&format!(
            "INSERT INTO my_swaps (
                id, my_coin, other_coin, uuid, started_at, swap_type, is_finished, events_json,
                maker_volume, taker_volume, premium, dex_fee, dex_fee_burn, secret, secret_hash,
                secret_hash_algo, p2p_privkey, lock_duration, maker_coin_confs, maker_coin_nota,
                taker_coin_confs, taker_coin_nota, other_p2p_pub, swap_version{price_columns}
            ) VALUES (
                1, 'RICK', 'MORTY', 'legacy-swap', 1000, 2, 0, '[{{\"type\":\"Started\"}}]',
                '1.23', '4.56', '0.1', '0.02', '0.01', X'01', X'02',
                3, X'03', 7800, 2, 1, 3, 0, X'04', 0{my_swap_prices}
            );
            INSERT INTO stats_swaps (
                id, maker_coin, taker_coin, uuid, started_at, finished_at, maker_amount,
                taker_amount, is_success, maker_coin_ticker, maker_coin_platform,
                taker_coin_ticker, taker_coin_platform{price_columns}
            ) VALUES (
                1, 'RICK', 'MORTY', 'legacy-swap', 1000, 2000, 1.23,
                4.56, 1, 'RICK', '', 'MORTY', ''{stats_prices}
            );",
        ))
        .unwrap();
    }

    fn assert_gleec_state_15_columns(ctx: &MmArc) {
        let my_swaps = table_column_specs(ctx, "my_swaps");
        assert_eq!(my_swaps, vec![
            ("id".into(), "INTEGER".into()),
            ("my_coin".into(), "VARCHAR(255)".into()),
            ("other_coin".into(), "VARCHAR(255)".into()),
            ("uuid".into(), "VARCHAR(255)".into()),
            ("started_at".into(), "INTEGER".into()),
            ("is_finished".into(), "BOOLEAN".into()),
            ("events_json".into(), "TEXT".into()),
            ("swap_type".into(), "INTEGER".into()),
            ("maker_volume".into(), "TEXT".into()),
            ("taker_volume".into(), "TEXT".into()),
            ("premium".into(), "TEXT".into()),
            ("dex_fee".into(), "TEXT".into()),
            ("secret".into(), "BLOB".into()),
            ("secret_hash".into(), "BLOB".into()),
            ("secret_hash_algo".into(), "INTEGER".into()),
            ("p2p_privkey".into(), "BLOB".into()),
            ("lock_duration".into(), "INTEGER".into()),
            ("maker_coin_confs".into(), "INTEGER".into()),
            ("maker_coin_nota".into(), "BOOLEAN".into()),
            ("taker_coin_confs".into(), "INTEGER".into()),
            ("taker_coin_nota".into(), "BOOLEAN".into()),
            ("other_p2p_pub".into(), "BLOB".into()),
            ("dex_fee_burn".into(), "TEXT".into()),
            ("swap_version".into(), "INTEGER".into()),
        ]);

        let stats_swaps = table_column_specs(ctx, "stats_swaps");
        assert_eq!(stats_swaps, vec![
            ("id".into(), "INTEGER".into()),
            ("maker_coin".into(), "VARCHAR(255)".into()),
            ("taker_coin".into(), "VARCHAR(255)".into()),
            ("uuid".into(), "VARCHAR(255)".into()),
            ("started_at".into(), "INTEGER".into()),
            ("finished_at".into(), "INTEGER".into()),
            ("maker_amount".into(), "DECIMAL".into()),
            ("taker_amount".into(), "DECIMAL".into()),
            ("is_success".into(), "INTEGER".into()),
            ("maker_coin_ticker".into(), "VARCHAR(255)".into()),
            ("maker_coin_platform".into(), "VARCHAR(255)".into()),
            ("taker_coin_ticker".into(), "VARCHAR(255)".into()),
            ("taker_coin_platform".into(), "VARCHAR(255)".into()),
            ("maker_coin_usd_price".into(), "DECIMAL".into()),
            ("taker_coin_usd_price".into(), "DECIMAL".into()),
            ("maker_pubkey".into(), "VARCHAR(255)".into()),
            ("taker_pubkey".into(), "VARCHAR(255)".into()),
            ("maker_gui".into(), "VARCHAR(255)".into()),
            ("taker_gui".into(), "VARCHAR(255)".into()),
            ("maker_version".into(), "VARCHAR(255)".into()),
            ("taker_version".into(), "VARCHAR(255)".into()),
        ]);
    }

    #[test]
    fn fresh_database_migrates_to_contiguous_state_15() {
        let ctx = setup_test_ctx();
        block_on(init_and_migrate_db(&ctx)).unwrap();

        assert_eq!(migration_rows(&ctx), (1..=15).collect::<Vec<i64>>());
    }

    #[test]
    fn fresh_database_uses_gleec_state_15_columns() {
        let ctx = setup_test_ctx();
        block_on(init_and_migrate_db(&ctx)).unwrap();

        assert_gleec_state_15_columns(&ctx);
    }

    #[test]
    fn legacy_reloaded_state_8_repairs_to_gleec_state_15() {
        let ctx = setup_test_ctx();
        seed_legacy_reloaded_schema(&ctx, 8);

        block_on(migrate_sqlite_database(&ctx, 8)).unwrap();

        assert_eq!(migration_rows(&ctx), (1..=15).collect::<Vec<i64>>());
        assert_gleec_state_15_columns(&ctx);
        assert!(!table_has_column(&ctx, "my_swaps", "maker_coin_usd_price").unwrap());
        assert!(!table_has_column(&ctx, "my_swaps", "taker_coin_usd_price").unwrap());
    }

    #[test]
    fn legacy_reloaded_state_9_repairs_to_gleec_state_15() {
        let ctx = setup_test_ctx();
        seed_legacy_reloaded_schema(&ctx, 9);

        block_on(migrate_sqlite_database(&ctx, 9)).unwrap();

        assert_eq!(migration_rows(&ctx), (1..=15).collect::<Vec<i64>>());
        assert_gleec_state_15_columns(&ctx);
        assert!(!table_has_column(&ctx, "my_swaps", "maker_coin_usd_price").unwrap());
        assert!(!table_has_column(&ctx, "my_swaps", "taker_coin_usd_price").unwrap());

        let conn = ctx.sqlite_connection();
        let (swap_type, maker_volume, dex_fee_burn, swap_version): (i64, String, String, i64) = conn
            .query_row(
                "SELECT swap_type, maker_volume, dex_fee_burn, swap_version FROM my_swaps WHERE uuid = 'legacy-swap'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (swap_type, maker_volume, dex_fee_burn, swap_version),
            (2, "1.23".into(), "0.01".into(), 1,)
        );

        let (maker_price, taker_price): (String, String) = conn
            .query_row(
                "SELECT CAST(maker_coin_usd_price AS TEXT), CAST(taker_coin_usd_price AS TEXT) \
                 FROM stats_swaps WHERE uuid = 'legacy-swap'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((maker_price, taker_price), ("10.5".into(), "20.25".into()));
    }
}
