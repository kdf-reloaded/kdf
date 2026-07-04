use crate::utxo::rpc_clients::ElectrumBlockHeader;
use crate::utxo::utxo_block_header_storage::{BlockHeaderStorageError, BlockHeaderStorageOps};
use crate::CoinsContext;
use async_trait::async_trait;
use chain::{BlockHeader, BlockHeaderBits};
use mm2_core::mm_ctx::MmArc;
use mm2_db::indexed_db::{DbIdentifier, DbInstance, DbLocked, DbTransactionError, DbUpgrader, IndexedDb,
                         IndexedDbBuilder, InitDbError, InitDbResult, OnUpgradeResult, SharedDb, TableSignature,
                         WeakDb};
use mm2_err_handle::prelude::*;
use primitives::hash::H256;
use serialization::deserialize;
use std::collections::HashMap;

const DB_NAME: &str = "block_headers_storage";
const DB_VERSION: u32 = 1;
const TICKER_INDEX: &str = "ticker";

pub type BlockHeaderStorageDbLocked<'a> = DbLocked<'a, BlockHeaderStorageDb>;

impl From<DbTransactionError> for BlockHeaderStorageError {
    fn from(e: DbTransactionError) -> Self {
        BlockHeaderStorageError::QueryError {
            query: "indexed_db transaction".to_owned(),
            reason: e.to_string(),
        }
    }
}

impl From<InitDbError> for BlockHeaderStorageError {
    fn from(e: InitDbError) -> Self {
        BlockHeaderStorageError::InitializationError {
            ticker: DB_NAME.to_owned(),
            reason: e.to_string(),
        }
    }
}

/// A single stored block header. One record per `(ticker, block_height)` pair; the `ticker`
/// index is used to scope every query to a coin. The serialized header `hex` is the source of
/// truth — hash and difficulty bits are derived by decoding it, keeping this backend behaviourally
/// identical to the native SQLite backend.
#[derive(Deserialize, Serialize)]
pub struct BlockHeaderStorageTable {
    /// Coin ticker. Non-unique index used to scope queries to a single coin.
    ticker: String,
    block_height: u64,
    hex: String,
}

impl TableSignature for BlockHeaderStorageTable {
    fn table_name() -> &'static str { "block_header_storage" }

    fn on_upgrade_needed(upgrader: &DbUpgrader, old_version: u32, new_version: u32) -> OnUpgradeResult<()> {
        if let (0, 1) = (old_version, new_version) {
            let table = upgrader.create_table(Self::table_name())?;
            table.create_index(TICKER_INDEX, false)?;
        }
        Ok(())
    }
}

pub struct BlockHeaderStorageDb {
    pub(crate) inner: IndexedDb,
}

#[async_trait]
impl DbInstance for BlockHeaderStorageDb {
    fn db_name() -> &'static str { DB_NAME }

    async fn init(db_id: DbIdentifier) -> InitDbResult<Self> {
        let inner = IndexedDbBuilder::new(db_id)
            .with_version(DB_VERSION)
            .with_table::<BlockHeaderStorageTable>()
            .build()
            .await?;
        Ok(BlockHeaderStorageDb { inner })
    }
}

/// The wrapper over the [`CoinsContext::block_headers_storage_db`] weak pointer.
#[derive(Debug)]
pub struct IndexedDBBlockHeadersStorage {
    db: WeakDb<BlockHeaderStorageDb>,
}

impl IndexedDBBlockHeadersStorage {
    pub fn new(ctx: &MmArc) -> Result<Self, MmError<BlockHeaderStorageError>> {
        let coins_ctx =
            CoinsContext::from_ctx(ctx).map_to_mm(|reason| BlockHeaderStorageError::InitializationError {
                ticker: DB_NAME.to_owned(),
                reason,
            })?;
        Ok(IndexedDBBlockHeadersStorage {
            db: SharedDb::downgrade(&coins_ctx.block_headers_storage_db),
        })
    }

    fn get_shared_db(&self) -> Result<SharedDb<BlockHeaderStorageDb>, MmError<BlockHeaderStorageError>> {
        self.db
            .upgrade()
            .or_mm_err(|| BlockHeaderStorageError::InitializationError {
                ticker: DB_NAME.to_owned(),
                reason: "'IndexedDBBlockHeadersStorage::db' doesn't exist".to_owned(),
            })
    }

    async fn lock_db(
        db: &SharedDb<BlockHeaderStorageDb>,
    ) -> Result<BlockHeaderStorageDbLocked<'_>, MmError<BlockHeaderStorageError>> {
        db.get_or_initialize().await.mm_err(BlockHeaderStorageError::from)
    }

    /// Fetches every stored header for `for_coin`. The stored set is bounded by the configured
    /// retention limit, so loading it into memory for the scan-based lookups is acceptable.
    async fn fetch_all_headers(
        &self,
        for_coin: &str,
    ) -> Result<Vec<(u64, BlockHeader)>, MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        let rows = table.get_items(TICKER_INDEX, for_coin).await.map_mm_err()?;
        let mut headers = Vec::with_capacity(rows.len());
        for (_item_id, row) in rows {
            headers.push((row.block_height, decode_header(for_coin, &row.hex)?));
        }
        Ok(headers)
    }

    /// Inserts the given `(height, hex)` headers, overwriting any header already stored at the same
    /// height (last-writer-wins per height).
    async fn upsert_headers(
        &self,
        for_coin: &str,
        headers: Vec<(u64, String)>,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        // Map existing heights to their item ids so we can overwrite in place.
        let mut existing: HashMap<u64, _> = table
            .get_items(TICKER_INDEX, for_coin)
            .await
            .map_mm_err()?
            .into_iter()
            .map(|(item_id, row)| (row.block_height, item_id))
            .collect();

        for (block_height, hex) in headers {
            let row = BlockHeaderStorageTable {
                ticker: for_coin.to_owned(),
                block_height,
                hex,
            };
            match existing.get(&block_height) {
                Some(item_id) => {
                    table.replace_item(*item_id, &row).await.map_mm_err()?;
                },
                None => {
                    let item_id = table.add_item(&row).await.map_mm_err()?;
                    existing.insert(block_height, item_id);
                },
            }
        }
        Ok(())
    }
}

#[async_trait]
impl BlockHeaderStorageOps for IndexedDBBlockHeadersStorage {
    async fn init(&self, _for_coin: &str) -> Result<(), MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        // Locking initializes the database (and creates the table) if it hasn't been already.
        Self::lock_db(&shared_db).await?;
        Ok(())
    }

    async fn is_initialized_for(&self, _for_coin: &str) -> Result<bool, MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        Self::lock_db(&shared_db).await?;
        Ok(true)
    }

    async fn add_electrum_block_headers_to_storage(
        &self,
        for_coin: &str,
        headers: Vec<ElectrumBlockHeader>,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let headers = headers
            .into_iter()
            .map(|header| {
                let block_height = header.block_height();
                let hex = match header {
                    ElectrumBlockHeader::V12(h) => h.as_hex(),
                    ElectrumBlockHeader::V14(h) => format!("{:02x}", h.hex),
                };
                (block_height, hex)
            })
            .collect();
        self.upsert_headers(for_coin, headers).await
    }

    async fn add_block_headers_to_storage(
        &self,
        for_coin: &str,
        headers: HashMap<u64, BlockHeader>,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let headers = headers
            .into_iter()
            .map(|(height, header)| (height, hex::encode(header.raw())))
            .collect();
        self.upsert_headers(for_coin, headers).await
    }

    async fn get_block_header(
        &self,
        for_coin: &str,
        height: u64,
    ) -> Result<Option<BlockHeader>, MmError<BlockHeaderStorageError>> {
        match self.get_block_header_raw(for_coin, height).await? {
            Some(hex) => Ok(Some(decode_header(for_coin, &hex)?)),
            None => Ok(None),
        }
    }

    async fn get_block_header_raw(
        &self,
        for_coin: &str,
        height: u64,
    ) -> Result<Option<String>, MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        let row = table
            .get_items(TICKER_INDEX, for_coin)
            .await
            .map_mm_err()?
            .into_iter()
            .find_map(|(_item_id, row)| (row.block_height == height).then(|| row.hex));
        Ok(row)
    }

    async fn get_block_headers_count(&self, for_coin: &str) -> Result<u64, MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        let ids = table.get_item_ids(TICKER_INDEX, for_coin).await.map_mm_err()?;
        Ok(ids.len() as u64)
    }

    async fn get_last_block_height(&self, for_coin: &str) -> Result<Option<u64>, MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        let highest = table
            .get_items(TICKER_INDEX, for_coin)
            .await
            .map_mm_err()?
            .into_iter()
            .map(|(_item_id, row)| row.block_height)
            .max();
        Ok(highest)
    }

    async fn get_block_height_by_hash(
        &self,
        for_coin: &str,
        hash: H256,
    ) -> Result<Option<u64>, MmError<BlockHeaderStorageError>> {
        for (height, header) in self.fetch_all_headers(for_coin).await? {
            if header.hash() == hash {
                return Ok(Some(height));
            }
        }
        Ok(None)
    }

    async fn get_last_block_header_with_non_max_bits(
        &self,
        for_coin: &str,
        max_bits: u32,
    ) -> Result<Option<BlockHeader>, MmError<BlockHeaderStorageError>> {
        let mut headers = self.fetch_all_headers(for_coin).await?;
        // Highest height first.
        headers.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        for (_height, header) in headers {
            if block_header_bits_u32(&header) != max_bits {
                return Ok(Some(header));
            }
        }
        Ok(None)
    }

    async fn remove_block_headers_from_to_height(
        &self,
        for_coin: &str,
        from_height: u64,
        to_height: u64,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let shared_db = self.get_shared_db()?;
        let locked_db = Self::lock_db(&shared_db).await?;
        let transaction = locked_db.inner.transaction().await.map_mm_err()?;
        let table = transaction.table::<BlockHeaderStorageTable>().await.map_mm_err()?;

        let to_delete: Vec<_> = table
            .get_items(TICKER_INDEX, for_coin)
            .await
            .map_mm_err()?
            .into_iter()
            .filter(|(_item_id, row)| row.block_height >= from_height && row.block_height <= to_height)
            .map(|(item_id, _row)| item_id)
            .collect();
        for item_id in to_delete {
            table.delete_item(item_id).await.map_mm_err()?;
        }
        Ok(())
    }
}

fn decode_header(for_coin: &str, hex: &str) -> Result<BlockHeader, MmError<BlockHeaderStorageError>> {
    let bytes = hex::decode(hex).map_to_mm(|e| BlockHeaderStorageError::DecodeError {
        ticker: for_coin.to_owned(),
        reason: e.to_string(),
    })?;
    deserialize(bytes.as_slice()).map_to_mm(|e| BlockHeaderStorageError::DecodeError {
        ticker: for_coin.to_owned(),
        reason: e.to_string(),
    })
}

/// Returns the compact difficulty bits of a header as a plain `u32`.
fn block_header_bits_u32(header: &BlockHeader) -> u32 {
    match header.bits {
        BlockHeaderBits::Compact(compact) => u32::from(compact),
        BlockHeaderBits::U32(bits) => bits,
    }
}
