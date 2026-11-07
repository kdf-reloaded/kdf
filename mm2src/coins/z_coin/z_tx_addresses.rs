//! The node's own record of the addresses a shielded transaction names (CRD ch.39 R39.8.9d2 (a)).
//!
//! The wallet database cannot say who a withdrawal paid: the output carries no outgoing viewing
//! information the wallet can open, and the compact blocks the scan reads hold nothing from which a
//! recipient could be recovered. The node therefore keeps, in its own file next to the wallet
//! database, the address the request named, keyed by the transaction id.
//!
//! The file is separate from the wallet database on purpose. The wallet-database crate never opens
//! it, so its migrations and schema checks cannot meet it. A rebuild of the wallet database (a
//! rescan, a divergence, the legacy-schema rename of R39.8.0ad) renames only that file, so the
//! entries survive and serve the rows the rescan finds again.
//!
//! One transaction can hold several addresses, each on a side, `from` or `to`. Only a withdrawal
//! writes today, with one `to` entry. The transparent addresses of R39.8.9c and R39.8.9d and the
//! shielded recipient of a swap's fee can be added later as more rows of the same table, with no
//! change to the schema.
//!
//! An entry never touches note selection or the in-flight exclusion of §39.8.0.7: it holds a
//! transaction id, an expiry height and a string, and nothing the wallet database does reads it.

use db_common::sqlite::rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tx_addresses (
    txid BLOB NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('from', 'to')),
    address TEXT NOT NULL,
    expiry_height INTEGER NOT NULL,
    PRIMARY KEY (txid, side, address)
) WITHOUT ROWID;";

/// Which side of a transaction an address is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddressSide {
    /// Not written yet: the later change that records the transparent inputs of R39.8.9c uses it. The
    /// reader and the schema already carry it.
    #[allow(dead_code)]
    From,
    To,
}

impl AddressSide {
    fn as_str(self) -> &'static str {
        match self {
            AddressSide::From => "from",
            AddressSide::To => "to",
        }
    }
}

/// The addresses recorded for one transaction, in the form they were recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TxAddresses {
    pub(crate) from: Vec<String>,
    pub(crate) to: Vec<String>,
}

/// The store: a handle on the file, opened for each call. A file that does not exist is an empty store.
#[derive(Clone, Debug)]
pub(crate) struct TxAddressStore {
    path: PathBuf,
}

impl TxAddressStore {
    pub(crate) fn new(path: PathBuf) -> Self { TxAddressStore { path } }

    pub(crate) fn path(&self) -> &Path { &self.path }

    fn open(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.path).map_err(|e| e.to_string())?;
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(conn)
    }

    /// Records `address` on `side` of the transaction `txid`, which expires at `expiry_height`. The
    /// `txid` is in the internal byte order of `transactions.txid` of the wallet database, so that the
    /// two join without a conversion. The same entry recorded twice is one entry.
    pub(crate) fn record(
        &self,
        txid: &[u8; 32],
        expiry_height: u32,
        side: AddressSide,
        address: &str,
    ) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "INSERT OR REPLACE INTO tx_addresses (txid, side, address, expiry_height) VALUES (?1, ?2, ?3, ?4)",
            params![&txid[..], side.as_str(), address, expiry_height],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Every entry, by transaction id in the internal byte order. A missing file is an empty store.
    pub(crate) fn load(&self) -> Result<HashMap<Vec<u8>, TxAddresses>, String> {
        let mut by_txid: HashMap<Vec<u8>, TxAddresses> = HashMap::new();
        if !self.path.exists() {
            return Ok(by_txid);
        }
        let conn = self.open()?;
        let mut stmt = conn
            .prepare("SELECT txid, side, address FROM tx_addresses ORDER BY txid, side, address")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (txid, side, address) = row.map_err(|e| e.to_string())?;
            let entry = by_txid.entry(txid).or_default();
            match side.as_str() {
                "from" => entry.from.push(address),
                _ => entry.to.push(address),
            }
        }
        Ok(by_txid)
    }

    /// Removes the entries of the transactions whose expiry height is below `scanned_height` and for
    /// which `is_mined` answers no (R39.8.9d2 (c)). An expiry height of 0 means the transaction never
    /// expires, so its entries stay. Returns the number of entries removed.
    ///
    /// `scanned_height` is the highest block the wallet database has scanned, not the chain tip: a
    /// wallet database that was rebuilt holds no mined transaction until the rescan has passed them, and
    /// the rescan has passed a transaction's block once it has scanned beyond the transaction's expiry
    /// height.
    pub(crate) fn prune(
        &self,
        scanned_height: u64,
        mut is_mined: impl FnMut(&[u8]) -> Result<bool, String>,
    ) -> Result<usize, String> {
        if !self.path.exists() {
            return Ok(0);
        }
        let scanned_height = i64::try_from(scanned_height).map_err(|e| e.to_string())?;
        let conn = self.open()?;
        let expired: Vec<Vec<u8>> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT txid FROM tx_addresses WHERE expiry_height != 0 AND expiry_height < ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![scanned_height], |row| row.get::<_, Vec<u8>>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let mut removed = 0;
        for txid in expired {
            if is_mined(&txid)? {
                continue;
            }
            removed += conn
                .execute("DELETE FROM tx_addresses WHERE txid = ?1", params![txid])
                .map_err(|e| e.to_string())?;
        }
        Ok(removed)
    }
}
