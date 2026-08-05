//! Native SQLite session storage.

use super::{StoredSession, WcStorageOps, WC_SESSION_TABLE};
use crate::error::WalletConnectError;
use async_trait::async_trait;
use db_common::async_sql_conn::{AsyncConnError, AsyncConnection};
use db_common::sqlite::rusqlite::params;

impl From<AsyncConnError> for WalletConnectError {
    fn from(e: AsyncConnError) -> Self { WalletConnectError::Storage(e.to_string()) }
}

/// SQLite-backed session store.
pub struct SqliteSessionStorage {
    conn: AsyncConnection,
}

impl SqliteSessionStorage {
    /// Wraps an open async SQLite connection.
    pub fn new(conn: AsyncConnection) -> Self { SqliteSessionStorage { conn } }
}

#[async_trait]
impl WcStorageOps for SqliteSessionStorage {
    async fn init(&self) -> Result<(), WalletConnectError> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {WC_SESSION_TABLE} (
                topic   CHAR(32) PRIMARY KEY,
                data    TEXT     NOT NULL,
                expiry  BIGINT   NOT NULL
            );"
        );
        self.conn
            .call(move |conn| {
                conn.execute(&sql, [])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn save_session(&self, session: StoredSession) -> Result<(), WalletConnectError> {
        let sql = format!("INSERT OR REPLACE INTO {WC_SESSION_TABLE} (topic, data, expiry) VALUES (?1, ?2, ?3);");
        self.conn
            .call(move |conn| {
                conn.execute(&sql, params![session.topic, session.data, session.expiry])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn get_session(&self, topic: &str) -> Result<Option<StoredSession>, WalletConnectError> {
        let topic = topic.to_string();
        let sql = format!("SELECT topic, data, expiry FROM {WC_SESSION_TABLE} WHERE topic = ?1;");
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query_map(params![topic], |row| {
                    Ok(StoredSession {
                        topic: row.get(0)?,
                        data: row.get(1)?,
                        expiry: row.get(2)?,
                    })
                })?;
                match rows.next() {
                    Some(row) => Ok(Some(row?)),
                    None => Ok(None),
                }
            })
            .await?;
        Ok(row)
    }

    async fn get_all_sessions(&self) -> Result<Vec<StoredSession>, WalletConnectError> {
        let sql = format!("SELECT topic, data, expiry FROM {WC_SESSION_TABLE};");
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let mapped = stmt.query_map([], |row| {
                    Ok(StoredSession {
                        topic: row.get(0)?,
                        data: row.get(1)?,
                        expiry: row.get(2)?,
                    })
                })?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row?);
                }
                Ok(out)
            })
            .await?;
        Ok(rows)
    }

    async fn delete_session(&self, topic: &str) -> Result<(), WalletConnectError> {
        let topic = topic.to_string();
        let sql = format!("DELETE FROM {WC_SESSION_TABLE} WHERE topic = ?1;");
        self.conn
            .call(move |conn| {
                conn.execute(&sql, params![topic])?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}
