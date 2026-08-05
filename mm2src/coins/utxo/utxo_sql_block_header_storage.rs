use crate::utxo::rpc_clients::ElectrumBlockHeader;
use crate::utxo::utxo_block_header_storage::{BlockHeaderStorageError, BlockHeaderStorageOps};
use async_trait::async_trait;
use chain::{BlockHeader, BlockHeaderBits};
use common::async_blocking;
use db_common::{sqlite::rusqlite::Error as SqlError,
                sqlite::rusqlite::{Connection, Row, ToSql},
                sqlite::string_from_row,
                sqlite::validate_table_name,
                sqlite::CHECK_TABLE_EXISTS_SQL};
use mm2_err_handle::prelude::*;
use primitives::hash::H256;
use serialization::deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn block_headers_cache_table(ticker: &str) -> String { ticker.to_owned() + "_block_headers_cache" }

fn get_table_name_and_validate(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = block_headers_cache_table(for_coin);
    validate_table_name(&table_name).map_err(|e| BlockHeaderStorageError::CantRetrieveTableError {
        ticker: for_coin.to_string(),
        reason: e.to_string(),
    })?;
    Ok(table_name)
}

fn create_block_header_cache_table_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {} (
            block_height INTEGER NOT NULL UNIQUE,
            hex TEXT NOT NULL
        );",
        table_name
    );

    Ok(sql)
}

fn insert_block_header_in_cache_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    // Always update the block headers with new values just in case a chain reorganization occurs.
    let sql = format!(
        "INSERT OR REPLACE INTO {} (block_height, hex) VALUES (?1, ?2);",
        table_name
    );
    Ok(sql)
}

fn get_block_header_by_height(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!("SELECT hex FROM {} WHERE block_height=?1;", table_name);

    Ok(sql)
}

fn get_block_headers_count_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!("SELECT COUNT(block_height) FROM {};", table_name);

    Ok(sql)
}

fn get_last_block_height_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!("SELECT MAX(block_height) FROM {};", table_name);

    Ok(sql)
}

fn get_all_headers_descending_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!(
        "SELECT block_height, hex FROM {} ORDER BY block_height DESC;",
        table_name
    );

    Ok(sql)
}

fn remove_headers_from_to_height_sql(for_coin: &str) -> Result<String, MmError<BlockHeaderStorageError>> {
    let table_name = get_table_name_and_validate(for_coin)?;
    let sql = format!(
        "DELETE FROM {} WHERE block_height >= ?1 AND block_height <= ?2;",
        table_name
    );

    Ok(sql)
}

/// Returns the compact difficulty bits of a header as a plain `u32`.
fn block_header_bits_u32(header: &BlockHeader) -> u32 {
    match header.bits {
        BlockHeaderBits::Compact(compact) => u32::from(compact),
        BlockHeaderBits::U32(bits) => bits,
    }
}

#[derive(Clone, Debug)]
pub struct SqliteBlockHeadersStorage(pub Arc<Mutex<Connection>>);

fn query_single_row<T, P, F>(
    conn: &Connection,
    query: &str,
    params: P,
    map_fn: F,
) -> Result<Option<T>, MmError<BlockHeaderStorageError>>
where
    P: IntoIterator,
    P::Item: ToSql,
    F: FnOnce(&Row<'_>) -> Result<T, SqlError>,
{
    db_common::sqlite::query_single_row(conn, query, params, map_fn).map_err(|e| {
        MmError::new(BlockHeaderStorageError::QueryError {
            query: query.to_string(),
            reason: e.to_string(),
        })
    })
}

struct SqlBlockHeader {
    block_height: String,
    block_hex: String,
}

impl From<ElectrumBlockHeader> for SqlBlockHeader {
    fn from(header: ElectrumBlockHeader) -> Self {
        match header {
            ElectrumBlockHeader::V12(h) => {
                let block_hex = h.as_hex();
                let block_height = h.block_height.to_string();
                SqlBlockHeader {
                    block_height,
                    block_hex,
                }
            },
            ElectrumBlockHeader::V14(h) => {
                let block_hex = format!("{:02x}", h.hex);
                let block_height = h.height.to_string();
                SqlBlockHeader {
                    block_height,
                    block_hex,
                }
            },
        }
    }
}
async fn common_headers_insert(
    for_coin: &str,
    storage: SqliteBlockHeadersStorage,
    headers: Vec<SqlBlockHeader>,
) -> Result<(), MmError<BlockHeaderStorageError>> {
    let for_coin = for_coin.to_owned();
    let mut conn = storage.0.lock().unwrap();
    let sql_transaction = conn
        .transaction()
        .map_err(|e| BlockHeaderStorageError::AddToStorageError {
            ticker: for_coin.to_string(),
            reason: e.to_string(),
        })?;
    for header in headers {
        let block_cache_params = [&header.block_height, &header.block_hex];
        sql_transaction
            .execute(&insert_block_header_in_cache_sql(&for_coin)?, block_cache_params)
            .map_err(|e| BlockHeaderStorageError::AddToStorageError {
                ticker: for_coin.to_string(),
                reason: e.to_string(),
            })?;
    }
    sql_transaction
        .commit()
        .map_err(|e| BlockHeaderStorageError::AddToStorageError {
            ticker: for_coin.to_string(),
            reason: e.to_string(),
        })?;
    Ok(())
}

impl SqliteBlockHeadersStorage {
    /// Fetches every stored header for `for_coin`, decoded, ordered by descending height.
    /// Used by the scan-based lookups (`get_block_height_by_hash`,
    /// `get_last_block_header_with_non_max_bits`); the stored set is bounded by the
    /// configured retention limit.
    async fn all_headers_descending(
        &self,
        for_coin: &str,
    ) -> Result<Vec<(u64, BlockHeader)>, MmError<BlockHeaderStorageError>> {
        let sql = get_all_headers_descending_sql(for_coin)?;
        let selfi = self.clone();
        let ticker = for_coin.to_owned();
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            let mut stmt = conn.prepare(&sql).map_err(|e| {
                MmError::new(BlockHeaderStorageError::QueryError {
                    query: sql.clone(),
                    reason: e.to_string(),
                })
            })?;
            let rows = stmt
                .query_map([], |row| {
                    let height: i64 = row.get(0)?;
                    let hex: String = row.get(1)?;
                    Ok((height, hex))
                })
                .map_err(|e| {
                    MmError::new(BlockHeaderStorageError::QueryError {
                        query: sql.clone(),
                        reason: e.to_string(),
                    })
                })?;
            let mut headers = Vec::new();
            for row in rows {
                let (height, hex) = row.map_err(|e| {
                    MmError::new(BlockHeaderStorageError::QueryError {
                        query: sql.clone(),
                        reason: e.to_string(),
                    })
                })?;
                let bytes = hex::decode(&hex).map_err(|e| {
                    MmError::new(BlockHeaderStorageError::DecodeError {
                        ticker: ticker.clone(),
                        reason: e.to_string(),
                    })
                })?;
                let header: BlockHeader = deserialize(bytes.as_slice()).map_err(|e| {
                    MmError::new(BlockHeaderStorageError::DecodeError {
                        ticker: ticker.clone(),
                        reason: e.to_string(),
                    })
                })?;
                headers.push((height.max(0) as u64, header));
            }
            Ok(headers)
        })
        .await
    }
}

#[async_trait]
impl BlockHeaderStorageOps for SqliteBlockHeadersStorage {
    async fn init(&self, for_coin: &str) -> Result<(), MmError<BlockHeaderStorageError>> {
        let ticker = for_coin.to_owned();
        let selfi = self.clone();
        let sql_cache = create_block_header_cache_table_sql(for_coin)?;
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            conn.execute(&sql_cache, [])
                .map(|_| ())
                .map_err(|e| BlockHeaderStorageError::InitializationError {
                    ticker,
                    reason: e.to_string(),
                })?;
            Ok(())
        })
        .await
    }

    async fn is_initialized_for(&self, for_coin: &str) -> Result<bool, MmError<BlockHeaderStorageError>> {
        let block_headers_cache_table = get_table_name_and_validate(for_coin)?;
        let selfi = self.clone();
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            let cache_initialized = query_single_row(
                &conn,
                CHECK_TABLE_EXISTS_SQL,
                [block_headers_cache_table],
                string_from_row,
            )?;
            Ok(cache_initialized.is_some())
        })
        .await
    }

    async fn add_electrum_block_headers_to_storage(
        &self,
        for_coin: &str,
        headers: Vec<ElectrumBlockHeader>,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let headers_for_sql = headers.into_iter().map(Into::into).collect();
        common_headers_insert(for_coin, self.clone(), headers_for_sql).await
    }

    async fn add_block_headers_to_storage(
        &self,
        for_coin: &str,
        headers: HashMap<u64, BlockHeader>,
    ) -> Result<(), MmError<BlockHeaderStorageError>> {
        let headers_for_sql = headers
            .into_iter()
            .map(|(height, header)| SqlBlockHeader {
                block_height: height.to_string(),
                block_hex: hex::encode(header.raw()),
            })
            .collect();
        common_headers_insert(for_coin, self.clone(), headers_for_sql).await
    }

    async fn get_block_header(
        &self,
        for_coin: &str,
        height: u64,
    ) -> Result<Option<BlockHeader>, MmError<BlockHeaderStorageError>> {
        if let Some(header_raw) = self.get_block_header_raw(for_coin, height).await? {
            let header_bytes = hex::decode(header_raw).map_err(|e| BlockHeaderStorageError::DecodeError {
                ticker: for_coin.to_string(),
                reason: e.to_string(),
            })?;
            let header: BlockHeader =
                deserialize(header_bytes.as_slice()).map_err(|e| BlockHeaderStorageError::DecodeError {
                    ticker: for_coin.to_string(),
                    reason: e.to_string(),
                })?;
            return Ok(Some(header));
        }
        Ok(None)
    }

    async fn get_block_header_raw(
        &self,
        for_coin: &str,
        height: u64,
    ) -> Result<Option<String>, MmError<BlockHeaderStorageError>> {
        let params = [height.to_string()];
        let sql = get_block_header_by_height(for_coin)?;
        let selfi = self.clone();

        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            query_single_row(&conn, &sql, params, string_from_row)
        })
        .await
        .map_err(|e| {
            MmError::new(BlockHeaderStorageError::GetFromStorageError {
                ticker: for_coin.to_string(),
                reason: e.into_inner().to_string(),
            })
        })
    }

    async fn get_block_headers_count(&self, for_coin: &str) -> Result<u64, MmError<BlockHeaderStorageError>> {
        let sql = get_block_headers_count_sql(for_coin)?;
        let selfi = self.clone();
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).map_err(|e| {
                MmError::new(BlockHeaderStorageError::QueryError {
                    query: sql.clone(),
                    reason: e.to_string(),
                })
            })?;
            Ok(count.max(0) as u64)
        })
        .await
    }

    async fn get_last_block_height(&self, for_coin: &str) -> Result<Option<u64>, MmError<BlockHeaderStorageError>> {
        let sql = get_last_block_height_sql(for_coin)?;
        let selfi = self.clone();
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            // `MAX(block_height)` over an empty table yields SQL NULL -> deserialized as `None`.
            let height: Option<i64> = conn.query_row(&sql, [], |row| row.get(0)).map_err(|e| {
                MmError::new(BlockHeaderStorageError::QueryError {
                    query: sql.clone(),
                    reason: e.to_string(),
                })
            })?;
            Ok(height.map(|h| h.max(0) as u64))
        })
        .await
    }

    async fn get_block_height_by_hash(
        &self,
        for_coin: &str,
        hash: H256,
    ) -> Result<Option<u64>, MmError<BlockHeaderStorageError>> {
        for (height, header) in self.all_headers_descending(for_coin).await? {
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
        for (_height, header) in self.all_headers_descending(for_coin).await? {
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
        let sql = remove_headers_from_to_height_sql(for_coin)?;
        let selfi = self.clone();
        let ticker = for_coin.to_owned();
        async_blocking(move || {
            let conn = selfi.0.lock().unwrap();
            conn.execute(&sql, [from_height.to_string(), to_height.to_string()])
                .map(|_| ())
                .map_err(|e| {
                    MmError::new(BlockHeaderStorageError::DeleteFromStorageError {
                        ticker,
                        reason: e.to_string(),
                    })
                })
        })
        .await
    }
}

#[cfg(test)]
impl SqliteBlockHeadersStorage {
    pub fn in_memory() -> Self {
        SqliteBlockHeadersStorage(Arc::new(Mutex::new(Connection::open_in_memory().unwrap())))
    }

    fn is_table_empty(&self, table_name: &str) -> bool {
        validate_table_name(table_name).unwrap();
        let sql = "SELECT COUNT(block_height) FROM ".to_owned() + table_name + ";";
        let conn = self.0.lock().unwrap();
        let rows_count: u32 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
        rows_count == 0
    }
}

#[cfg(test)]
mod sql_block_headers_storage_tests {
    use super::*;
    use crate::utxo::rpc_clients::ElectrumBlockHeaderV14;
    use common::block_on;
    use primitives::hash::H256;

    #[test]
    fn test_init_collection() {
        let for_coin = "init_collection";
        let storage = SqliteBlockHeadersStorage::in_memory();
        let initialized = block_on(storage.is_initialized_for(for_coin)).unwrap();
        assert!(!initialized);

        block_on(storage.init(for_coin)).unwrap();
        // repetitive init must not fail
        block_on(storage.init(for_coin)).unwrap();

        let initialized = block_on(storage.is_initialized_for(for_coin)).unwrap();
        assert!(initialized);
    }

    #[test]
    fn test_add_block_headers() {
        let for_coin = "insert";
        let storage = SqliteBlockHeadersStorage::in_memory();
        let table = block_headers_cache_table(for_coin);
        block_on(storage.init(for_coin)).unwrap();

        let initialized = block_on(storage.is_initialized_for(for_coin)).unwrap();
        assert!(initialized);

        let block_header = ElectrumBlockHeaderV14 {
            height: 520481,
            hex: "0000002076d41d3e4b0bfd4c0d3b30aa69fdff3ed35d85829efd04000000000000000000b386498b583390959d9bac72346986e3015e83ac0b54bc7747a11a494ac35c94bb3ce65a53fb45177f7e311c".into(),
        }.into();
        let headers = vec![ElectrumBlockHeader::V14(block_header)];
        block_on(storage.add_electrum_block_headers_to_storage(for_coin, headers)).unwrap();
        assert!(!storage.is_table_empty(&table));
    }

    #[test]
    fn test_get_block_header() {
        let for_coin = "get";
        let storage = SqliteBlockHeadersStorage::in_memory();
        let table = block_headers_cache_table(for_coin);
        block_on(storage.init(for_coin)).unwrap();

        let initialized = block_on(storage.is_initialized_for(for_coin)).unwrap();
        assert!(initialized);

        let block_header = ElectrumBlockHeaderV14 {
            height: 520481,
            hex: "0000002076d41d3e4b0bfd4c0d3b30aa69fdff3ed35d85829efd04000000000000000000b386498b583390959d9bac72346986e3015e83ac0b54bc7747a11a494ac35c94bb3ce65a53fb45177f7e311c".into(),
        }.into();
        let headers = vec![ElectrumBlockHeader::V14(block_header)];
        block_on(storage.add_electrum_block_headers_to_storage(for_coin, headers)).unwrap();
        assert!(!storage.is_table_empty(&table));

        let hex = block_on(storage.get_block_header_raw(for_coin, 520481))
            .unwrap()
            .unwrap();
        assert_eq!(hex, "0000002076d41d3e4b0bfd4c0d3b30aa69fdff3ed35d85829efd04000000000000000000b386498b583390959d9bac72346986e3015e83ac0b54bc7747a11a494ac35c94bb3ce65a53fb45177f7e311c".to_string());

        let block_header = block_on(storage.get_block_header(for_coin, 520481)).unwrap().unwrap();
        assert_eq!(
            block_header.hash(),
            H256::from_reversed_str("0000000000000000002e31d0714a5ab23100945ff87ba2d856cd566a3c9344ec")
        )
    }

    #[test]
    fn test_block_header_storage_contract_ops() {
        let for_coin = "contract";
        let storage = SqliteBlockHeadersStorage::in_memory();
        block_on(storage.init(for_coin)).unwrap();

        let hex = "0000002076d41d3e4b0bfd4c0d3b30aa69fdff3ed35d85829efd04000000000000000000b386498b583390959d9bac72346986e3015e83ac0b54bc7747a11a494ac35c94bb3ce65a53fb45177f7e311c";
        let bytes = hex::decode(hex).unwrap();
        let header: BlockHeader = deserialize(bytes.as_slice()).unwrap();
        let bits = block_header_bits_u32(&header);

        // Empty store.
        assert_eq!(block_on(storage.get_block_headers_count(for_coin)).unwrap(), 0);
        assert_eq!(block_on(storage.get_last_block_height(for_coin)).unwrap(), None);

        // Insert the same header payload at heights 1, 2 and 3.
        let mut registry = HashMap::new();
        registry.insert(1u64, header.clone());
        registry.insert(2u64, header.clone());
        registry.insert(3u64, header.clone());
        block_on(storage.add_block_headers_to_storage(for_coin, registry)).unwrap();

        assert_eq!(block_on(storage.get_block_headers_count(for_coin)).unwrap(), 3);
        assert_eq!(block_on(storage.get_last_block_height(for_coin)).unwrap(), Some(3));

        // Height-by-hash returns the highest matching height (descending scan).
        assert_eq!(
            block_on(storage.get_block_height_by_hash(for_coin, header.hash())).unwrap(),
            Some(3)
        );

        // All stored headers share `bits`.
        assert!(
            block_on(storage.get_last_block_header_with_non_max_bits(for_coin, bits))
                .unwrap()
                .is_none()
        );
        assert!(
            block_on(storage.get_last_block_header_with_non_max_bits(for_coin, bits.wrapping_add(1)))
                .unwrap()
                .is_some()
        );

        // Inclusive range removal [2, 3].
        block_on(storage.remove_block_headers_from_to_height(for_coin, 2, 3)).unwrap();
        assert_eq!(block_on(storage.get_block_headers_count(for_coin)).unwrap(), 1);
        assert_eq!(block_on(storage.get_last_block_height(for_coin)).unwrap(), Some(1));

        // Remove the remaining header.
        block_on(storage.remove_block_headers_from_to_height(for_coin, 0, 100)).unwrap();
        assert_eq!(block_on(storage.get_block_headers_count(for_coin)).unwrap(), 0);
        assert_eq!(block_on(storage.get_last_block_height(for_coin)).unwrap(), None);
    }
}
