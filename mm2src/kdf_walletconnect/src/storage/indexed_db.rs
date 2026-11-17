//! Browser (IndexedDB) session storage.
//!
//! Mirrors the native SQLite backend: one object store, one row per topic,
//! carrying the opaque JSON payload and the expiry. The same [`WcStorageOps`]
//! trait, the same row shape and the same load/expire/update/delete lifecycle
//! apply on both backends.

use super::{StoredSession, WcStorageOps, WC_CLIENT_KEY_TABLE, WC_SESSION_TABLE};
use crate::error::WalletConnectError;
use async_trait::async_trait;
use mm2_core::mm_ctx::MmArc;
use mm2_db::indexed_db::{ConstructibleDb, DbIdentifier, DbInstance, DbLocked, DbTransactionError, DbUpgrader,
                         IndexedDb, IndexedDbBuilder, InitDbError, InitDbResult, OnUpgradeResult, SharedDb,
                         TableSignature};
use mm2_err_handle::mm_error::MmError;
use serde::{Deserialize, Serialize};

const DB_NAME: &str = "wc_session";
const DB_VERSION: u32 = 2;
/// Unique index used to fetch, replace and delete a row by its topic.
const TOPIC_INDEX: &str = "topic";
/// Unique index of the single relay client key record.
const CLIENT_KEY_INDEX: &str = "id";
/// The fixed identifier of the single relay client key record.
const CLIENT_KEY_ID: &str = "relay_client_key";

type WcSessionDbLocked<'a> = DbLocked<'a, WcSessionDb>;

impl From<MmError<DbTransactionError>> for WalletConnectError {
    fn from(e: MmError<DbTransactionError>) -> Self { WalletConnectError::Storage(e.to_string()) }
}

impl From<MmError<InitDbError>> for WalletConnectError {
    fn from(e: MmError<InitDbError>) -> Self { WalletConnectError::Storage(e.to_string()) }
}

/// One persisted session row, shaped identically to [`StoredSession`].
#[derive(Clone, Debug, Deserialize, Serialize)]
struct WcSessionTable {
    topic: String,
    data: String,
    expiry: i64,
}

impl From<StoredSession> for WcSessionTable {
    fn from(s: StoredSession) -> Self {
        WcSessionTable {
            topic: s.topic,
            data: s.data,
            expiry: s.expiry,
        }
    }
}

impl From<WcSessionTable> for StoredSession {
    fn from(t: WcSessionTable) -> Self {
        StoredSession {
            topic: t.topic,
            data: t.data,
            expiry: t.expiry,
        }
    }
}

impl TableSignature for WcSessionTable {
    fn table_name() -> &'static str { WC_SESSION_TABLE }

    fn on_upgrade_needed(upgrader: &DbUpgrader, old_version: u32, new_version: u32) -> OnUpgradeResult<()> {
        if old_version == 0 && new_version >= 1 {
            let table = upgrader.create_table(Self::table_name())?;
            table.create_index(TOPIC_INDEX, true)?;
        }
        Ok(())
    }
}

/// The relay client key record: one row with a fixed id, carrying the secret key value.
#[derive(Clone, Deserialize, Serialize)]
struct WcClientKeyTable {
    id: String,
    value: String,
}

impl TableSignature for WcClientKeyTable {
    fn table_name() -> &'static str { WC_CLIENT_KEY_TABLE }

    fn on_upgrade_needed(upgrader: &DbUpgrader, old_version: u32, new_version: u32) -> OnUpgradeResult<()> {
        if old_version < 2 && new_version >= 2 {
            let table = upgrader.create_table(Self::table_name())?;
            table.create_index(CLIENT_KEY_INDEX, true)?;
        }
        Ok(())
    }
}

/// The IndexedDB database holding the single session object store.
pub struct WcSessionDb {
    inner: IndexedDb,
}

#[async_trait]
impl DbInstance for WcSessionDb {
    fn db_name() -> &'static str { DB_NAME }

    async fn init(db_id: DbIdentifier) -> InitDbResult<Self> {
        let inner = IndexedDbBuilder::new(db_id)
            .with_version(DB_VERSION)
            .with_table::<WcSessionTable>()
            .with_table::<WcClientKeyTable>()
            .build()
            .await?;
        Ok(WcSessionDb { inner })
    }
}

/// IndexedDB-backed session store (wasm).
pub struct IndexedDbSessionStorage {
    db: SharedDb<WcSessionDb>,
}

impl IndexedDbSessionStorage {
    /// Binds the store to the database namespace of `ctx`.
    pub fn new(ctx: &MmArc) -> Self {
        IndexedDbSessionStorage {
            db: ConstructibleDb::new_shared(ctx),
        }
    }

    async fn lock(&self) -> Result<WcSessionDbLocked<'_>, WalletConnectError> {
        self.db.get_or_initialize().await.map_err(WalletConnectError::from)
    }
}

#[async_trait]
impl WcStorageOps for IndexedDbSessionStorage {
    async fn init(&self) -> Result<(), WalletConnectError> {
        // Opening the database runs the upgrade handler that creates the store.
        self.lock().await?;
        Ok(())
    }

    async fn save_session(&self, session: StoredSession) -> Result<(), WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcSessionTable>().await?;
        let row = WcSessionTable::from(session);
        table
            .replace_item_by_unique_index(TOPIC_INDEX, row.topic.clone(), &row)
            .await?;
        Ok(())
    }

    async fn get_session(&self, topic: &str) -> Result<Option<StoredSession>, WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcSessionTable>().await?;
        let found = table.get_item_by_unique_index(TOPIC_INDEX, topic).await?;
        Ok(found.map(|(_id, row)| StoredSession::from(row)))
    }

    async fn get_all_sessions(&self) -> Result<Vec<StoredSession>, WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcSessionTable>().await?;
        let rows = table.get_all_items().await?;
        Ok(rows.into_iter().map(|(_id, row)| StoredSession::from(row)).collect())
    }

    async fn delete_session(&self, topic: &str) -> Result<(), WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcSessionTable>().await?;
        table.delete_item_by_unique_index(TOPIC_INDEX, topic).await?;
        Ok(())
    }

    async fn load_client_key(&self) -> Result<Option<String>, WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcClientKeyTable>().await?;
        let found = table.get_item_by_unique_index(CLIENT_KEY_INDEX, CLIENT_KEY_ID).await?;
        Ok(found.map(|(_id, row)| row.value))
    }

    async fn save_client_key(&self, key: String) -> Result<(), WalletConnectError> {
        let locked = self.lock().await?;
        let transaction = locked.inner.transaction().await?;
        let table = transaction.table::<WcClientKeyTable>().await?;
        let row = WcClientKeyTable {
            id: CLIENT_KEY_ID.to_string(),
            value: key,
        };
        table
            .replace_item_by_unique_index(CLIENT_KEY_INDEX, CLIENT_KEY_ID, &row)
            .await?;
        Ok(())
    }
}
