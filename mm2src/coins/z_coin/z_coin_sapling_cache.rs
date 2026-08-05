//! Sapling state cache storage abstraction for ZCoin.
//!
//! On native builds the cache is backed by SQLite; on WASM it is backed by
//! IndexedDB (R39.6.1).  Both implementations expose the same async interface
//! so the sync loop and witness-building code remain platform-agnostic.

use async_trait::async_trait;
use derive_more::Display;
use keys::hash::H256;
use mm2_err_handle::prelude::*;
use sapling::CommitmentTree;
use serialization::{serialize_list, Reader};
use zcash_primitives::merkle_tree::{read_commitment_tree, write_commitment_tree};

// ── Shared types ──────────────────────────────────────────────────────────────

/// One row of the sapling cache: the commitment tree snapshot and note
/// commitments (cmus) for a single block height.
pub(crate) struct SaplingBlockState {
    pub(crate) height: u32,
    pub(crate) prev_tree_state: CommitmentTree,
    pub(crate) cmus: Vec<H256>,
}

/// Errors that can be returned by the storage backend.
#[derive(Debug, Display)]
pub(crate) enum ZCoinSaplingCacheError {
    #[display(fmt = "Database initialisation error: {}", _0)]
    InitError(String),
    #[display(fmt = "Query error: {}", _0)]
    QueryError(String),
    #[display(fmt = "Serialisation error: {}", _0)]
    SerialisationError(String),
}

// ── Storage trait ─────────────────────────────────────────────────────────────

/// Platform-agnostic async interface to the sapling state cache.
///
/// The trait is `Send + Sync` on native (required for the tokio executor and
/// for the `Arc<dyn …>` field in `ZCoinFields`).  On WASM all types are
/// single-threaded so the same bounds hold trivially.
#[async_trait]
pub(crate) trait SaplingStateCacheOps: Send + Sync {
    /// Return the most recently stored block state, or `None` if the cache is
    /// empty.
    async fn query_latest_block(&self) -> MmResult<Option<SaplingBlockState>, ZCoinSaplingCacheError>;

    /// Return every cached block state at or after `height`, ordered by height
    /// ascending.
    async fn query_states_after_height(&self, height: u32) -> MmResult<Vec<SaplingBlockState>, ZCoinSaplingCacheError>;

    /// Persist a new block state.  Implementations must be idempotent (e.g.
    /// INSERT-OR-REPLACE) so that a crash-restart cannot produce duplicates.
    async fn insert_block_state(&self, state: SaplingBlockState) -> MmResult<(), ZCoinSaplingCacheError>;
}

// ── Shared serialisation helpers ─────────────────────────────────────────────

/// Serialise a Sapling `CommitmentTree` to raw bytes for storage.
pub(crate) fn tree_to_bytes(tree: &CommitmentTree) -> Result<Vec<u8>, ZCoinSaplingCacheError> {
    let mut buf = Vec::new();
    write_commitment_tree(tree, &mut buf).map_err(|e| ZCoinSaplingCacheError::SerialisationError(e.to_string()))?;
    Ok(buf)
}

/// Deserialise a Sapling `CommitmentTree` from raw bytes.
pub(crate) fn bytes_to_tree(bytes: &[u8]) -> Result<CommitmentTree, ZCoinSaplingCacheError> {
    read_commitment_tree(bytes).map_err(|e| ZCoinSaplingCacheError::SerialisationError(e.to_string()))
}

/// Serialise a `Vec<H256>` (note commitments) to a compact byte vector using
/// the mm2 `serialize_list` helper.
pub(crate) fn cmus_to_bytes(cmus: &[H256]) -> Vec<u8> { serialize_list(cmus).take() }

/// Deserialise a `Vec<H256>` from the compact byte vector produced by
/// `cmus_to_bytes`.
pub(crate) fn bytes_to_cmus(bytes: &[u8]) -> Result<Vec<H256>, ZCoinSaplingCacheError> {
    let mut reader = Reader::from_read(bytes);
    reader
        .read_list()
        .map_err(|e| ZCoinSaplingCacheError::SerialisationError(e.to_string()))
}

// ── Native SQLite implementation ──────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use db_common::sqlite::rusqlite::types::Type;
    use db_common::sqlite::rusqlite::{params, Connection, Error as SqliteError, Row};
    use std::sync::{Arc, Mutex};

    impl From<SqliteError> for ZCoinSaplingCacheError {
        fn from(e: SqliteError) -> Self { ZCoinSaplingCacheError::QueryError(e.to_string()) }
    }

    fn init_db(sql: &Connection) -> Result<(), SqliteError> {
        const INIT_STMT: &str = "CREATE TABLE IF NOT EXISTS sapling_cache (
            height          INTEGER NOT NULL PRIMARY KEY,
            prev_tree_state BLOB    NOT NULL,
            cmus            BLOB    NOT NULL
        );";
        sql.execute(INIT_STMT, []).map(|_| ())
    }

    fn row_to_state(row: &Row<'_>) -> Result<SaplingBlockState, SqliteError> {
        let height: u32 = row.get(0)?;
        let tree_bytes: Vec<u8> = row.get(1)?;
        let cmu_bytes: Vec<u8> = row.get(2)?;

        let prev_tree_state = read_commitment_tree(tree_bytes.as_slice())
            .map_err(|e| SqliteError::FromSqlConversionFailure(1, Type::Blob, Box::new(e)))?;
        let mut reader = Reader::from_read(cmu_bytes.as_slice());
        let cmus = reader
            .read_list()
            .map_err(|e| SqliteError::FromSqlConversionFailure(2, Type::Blob, Box::new(e)))?;
        Ok(SaplingBlockState {
            height,
            prev_tree_state,
            cmus,
        })
    }

    /// SQLite-backed sapling state cache (native target only).
    pub(crate) struct ZCoinSqliteSaplingCache {
        pub(crate) sqlite: Arc<Mutex<Connection>>,
    }

    impl ZCoinSqliteSaplingCache {
        /// Open (or create) the SQLite database at `path` and initialise the
        /// schema.  This is a blocking call and must be called inside
        /// `tokio::task::block_in_place`.
        pub(crate) fn open(conn: Connection) -> Result<Self, ZCoinSaplingCacheError> {
            init_db(&conn)?;
            Ok(ZCoinSqliteSaplingCache {
                sqlite: Arc::new(Mutex::new(conn)),
            })
        }
    }

    #[async_trait]
    impl SaplingStateCacheOps for ZCoinSqliteSaplingCache {
        async fn query_latest_block(&self) -> MmResult<Option<SaplingBlockState>, ZCoinSaplingCacheError> {
            const STMT: &str = "SELECT height, prev_tree_state, cmus FROM sapling_cache ORDER BY height DESC LIMIT 1";
            let sqlite = self.sqlite.clone();
            tokio::task::block_in_place(move || {
                let conn = sqlite.lock().unwrap();
                match conn.query_row(STMT, [], |r| row_to_state(r)) {
                    Ok(state) => Ok(Some(state)),
                    Err(SqliteError::QueryReturnedNoRows) => Ok(None),
                    Err(e) => MmError::err(ZCoinSaplingCacheError::from(e)),
                }
            })
        }

        async fn query_states_after_height(
            &self,
            height: u32,
        ) -> MmResult<Vec<SaplingBlockState>, ZCoinSaplingCacheError> {
            const STMT: &str =
                "SELECT height, prev_tree_state, cmus FROM sapling_cache WHERE height >= ?1 ORDER BY height ASC";
            let sqlite = self.sqlite.clone();
            tokio::task::block_in_place(move || {
                let conn = sqlite.lock().unwrap();
                let mut stmt = conn.prepare(STMT).map_err(ZCoinSaplingCacheError::from)?;
                #[allow(clippy::needless_question_mark)]
                let rows: Result<Vec<_>, _> = stmt
                    .query_map(params![height], row_to_state)
                    .map_err(ZCoinSaplingCacheError::from)?
                    .collect();
                rows.map_err(|e| MmError::new(ZCoinSaplingCacheError::from(e)))
            })
        }

        async fn insert_block_state(&self, state: SaplingBlockState) -> MmResult<(), ZCoinSaplingCacheError> {
            const STMT: &str = "INSERT OR REPLACE INTO sapling_cache (height, prev_tree_state, cmus) \
                                 VALUES (?1, ?2, ?3)";
            let sqlite = self.sqlite.clone();
            let tree_bytes = tree_to_bytes(&state.prev_tree_state)?;
            let cmu_bytes = cmus_to_bytes(&state.cmus);
            tokio::task::block_in_place(move || {
                let conn = sqlite.lock().unwrap();
                conn.execute(STMT, params![state.height, tree_bytes, cmu_bytes])
                    .map(|_| ())
                    .map_err(|e| MmError::new(ZCoinSaplingCacheError::from(e)))
            })
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::ZCoinSqliteSaplingCache;

// ── WASM IndexedDB implementation ─────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use mm2_core::mm_ctx::MmArc;
    use mm2_db::indexed_db::{ConstructibleDb, DbIdentifier, DbInstance, DbTransactionError, DbUpgrader, IndexedDb,
                             IndexedDbBuilder, InitDbError, InitDbResult, OnUpgradeResult, SharedDb, TableSignature};
    use serde::{Deserialize, Serialize};

    const DB_NAME: &str = "z_coin_sapling_cache";
    const DB_VERSION: u32 = 1;
    const HEIGHT_INDEX: &str = "height";

    impl From<DbTransactionError> for ZCoinSaplingCacheError {
        fn from(e: DbTransactionError) -> Self { ZCoinSaplingCacheError::QueryError(e.to_string()) }
    }

    impl From<InitDbError> for ZCoinSaplingCacheError {
        fn from(e: InitDbError) -> Self { ZCoinSaplingCacheError::InitError(e.to_string()) }
    }

    /// Single row in the IndexedDB sapling_cache object store.
    /// Heights are stored as `u32` inside the record; the `height` field is
    /// also exposed as a non-unique index so cursors can filter by it.
    #[derive(Deserialize, Serialize)]
    struct SaplingCacheTable {
        /// The coin ticker — scopes the table to a single Z-coin instance.
        ticker: String,
        height: u32,
        /// Base64-encoded serialised Sapling `CommitmentTree`.
        prev_tree_state_b64: String,
        /// Base64-encoded compact-serialised `Vec<H256>`.
        cmus_b64: String,
    }

    impl TableSignature for SaplingCacheTable {
        fn table_name() -> &'static str { "sapling_cache" }

        fn on_upgrade_needed(upgrader: &DbUpgrader, old_version: u32, new_version: u32) -> OnUpgradeResult<()> {
            if let (0, 1) = (old_version, new_version) {
                let table = upgrader.create_table(Self::table_name())?;
                // Non-unique index on ticker so we can scope queries to a coin.
                table.create_index("ticker", false)?;
                // Non-unique index on height for ordered retrieval.
                table.create_index(HEIGHT_INDEX, false)?;
            }
            Ok(())
        }
    }

    pub(crate) struct ZCoinSaplingCacheDb {
        pub(crate) inner: IndexedDb,
    }

    #[async_trait]
    impl DbInstance for ZCoinSaplingCacheDb {
        fn db_name() -> &'static str { DB_NAME }

        async fn init(db_id: DbIdentifier) -> InitDbResult<Self> {
            let inner = IndexedDbBuilder::new(db_id)
                .with_version(DB_VERSION)
                .with_table::<SaplingCacheTable>()
                .build()
                .await?;
            Ok(ZCoinSaplingCacheDb { inner })
        }
    }

    /// IndexedDB-backed sapling state cache (WASM target only).
    pub(crate) struct ZCoinIdbSaplingCache {
        ticker: String,
        db: SharedDb<ZCoinSaplingCacheDb>,
    }

    impl ZCoinIdbSaplingCache {
        pub(crate) fn new(ticker: String, ctx: &MmArc) -> Self {
            ZCoinIdbSaplingCache {
                ticker,
                db: ConstructibleDb::new_shared(ctx),
            }
        }

        async fn lock_db(
            &self,
        ) -> MmResult<mm2_db::indexed_db::DbLocked<'_, ZCoinSaplingCacheDb>, ZCoinSaplingCacheError> {
            self.db.get_or_initialize().await.mm_err(ZCoinSaplingCacheError::from)
        }

        fn encode_state(state: &SaplingBlockState) -> Result<SaplingCacheTable, ZCoinSaplingCacheError> {
            let tree_bytes = tree_to_bytes(&state.prev_tree_state)?;
            let cmu_bytes = cmus_to_bytes(&state.cmus);
            Ok(SaplingCacheTable {
                ticker: String::new(), // filled in by caller
                height: state.height,
                prev_tree_state_b64: base64::encode(&tree_bytes),
                cmus_b64: base64::encode(&cmu_bytes),
            })
        }

        fn decode_row(row: SaplingCacheTable) -> Result<SaplingBlockState, ZCoinSaplingCacheError> {
            let tree_bytes = base64::decode(&row.prev_tree_state_b64)
                .map_err(|e| ZCoinSaplingCacheError::SerialisationError(e.to_string()))?;
            let cmu_bytes =
                base64::decode(&row.cmus_b64).map_err(|e| ZCoinSaplingCacheError::SerialisationError(e.to_string()))?;
            let prev_tree_state = bytes_to_tree(&tree_bytes)?;
            let cmus = bytes_to_cmus(&cmu_bytes)?;
            Ok(SaplingBlockState {
                height: row.height,
                prev_tree_state,
                cmus,
            })
        }
    }

    #[async_trait]
    impl SaplingStateCacheOps for ZCoinIdbSaplingCache {
        async fn query_latest_block(&self) -> MmResult<Option<SaplingBlockState>, ZCoinSaplingCacheError> {
            let locked = self.lock_db().await?;
            let tx = locked.inner.transaction().await.mm_err(ZCoinSaplingCacheError::from)?;
            let table = tx
                .table::<SaplingCacheTable>()
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;
            let rows = table
                .get_items("ticker", &self.ticker)
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;
            // Find the row with the highest height.
            let best = rows
                .into_iter()
                .max_by_key(|(_, r)| r.height)
                .map(|(_, r)| Self::decode_row(r))
                .transpose()?;
            Ok(best)
        }

        async fn query_states_after_height(
            &self,
            height: u32,
        ) -> MmResult<Vec<SaplingBlockState>, ZCoinSaplingCacheError> {
            let locked = self.lock_db().await?;
            let tx = locked.inner.transaction().await.mm_err(ZCoinSaplingCacheError::from)?;
            let table = tx
                .table::<SaplingCacheTable>()
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;
            let rows = table
                .get_items("ticker", &self.ticker)
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;
            let mut states: Vec<SaplingBlockState> = rows
                .into_iter()
                .filter(|(_, r)| r.height >= height)
                .map(|(_, r)| Self::decode_row(r))
                .collect::<Result<_, _>>()?;
            states.sort_by_key(|s| s.height);
            Ok(states)
        }

        async fn insert_block_state(&self, state: SaplingBlockState) -> MmResult<(), ZCoinSaplingCacheError> {
            let locked = self.lock_db().await?;
            let tx = locked.inner.transaction().await.mm_err(ZCoinSaplingCacheError::from)?;
            let table = tx
                .table::<SaplingCacheTable>()
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;

            // Upsert: replace existing row at this height if it exists.
            let existing = table
                .get_items("ticker", &self.ticker)
                .await
                .mm_err(ZCoinSaplingCacheError::from)?;
            let maybe_id = existing
                .into_iter()
                .find(|(_, r)| r.height == state.height)
                .map(|(id, _)| id);

            let mut row = Self::encode_state(&state)?;
            row.ticker = self.ticker.clone();

            match maybe_id {
                Some(item_id) => {
                    table
                        .replace_item(item_id, &row)
                        .await
                        .mm_err(ZCoinSaplingCacheError::from)?;
                },
                None => {
                    table.add_item(&row).await.mm_err(ZCoinSaplingCacheError::from)?;
                },
            }
            Ok(())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::ZCoinIdbSaplingCache;
