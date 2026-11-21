//! The registry of the recently-spent records that QTUM and its QRC20 tokens share (CRD ch.38 D38.6.4Q.4 status
//! note, items 1 to 5).
//!
//! One record per platform and address script. The registry owns none of them: it keeps a weak reference per key,
//! so a record lives as long as any instance holds it and ends with the last holder. The record's own lock is the
//! lock of the address.
//!
//! Lock order: the record's lock is a lock of the send path. It is never taken while the activation locks of the
//! instance, the build lock of the Electrum client registry or a map of either registry are held. The map here is a
//! plain mutex, touched only when an instance is built, and never held across an await.

use super::RecentlySpentOutPoints;
use futures::lock::Mutex as AsyncMutex;
use keys::bytes::Bytes;
use std::sync::{Arc, Mutex, PoisonError, Weak};

type SharedRecord = Arc<AsyncMutex<RecentlySpentOutPoints>>;

/// The shared recently-spent records of a node, by weak reference.
#[derive(Default)]
pub struct SpentRecordRegistry {
    records: Mutex<Vec<(SpentRecordKey, Weak<AsyncMutex<RecentlySpentOutPoints>>)>>,
}

/// The platform (a token's `protocol.protocol_data.platform`, QTUM's own ticker) and the script of the address the
/// record is bound to.
#[derive(Clone, Debug, PartialEq)]
struct SpentRecordKey {
    platform: String,
    script: Bytes,
}

impl SpentRecordRegistry {
    /// The live record of the key, or a new one bound to `script`. The entries nobody holds are dropped.
    pub(crate) fn get_or_insert(&self, platform: &str, script: Bytes) -> SharedRecord {
        let key = SpentRecordKey {
            platform: platform.to_owned(),
            script,
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.retain(|(_, record)| record.strong_count() > 0);
        if let Some(record) = records
            .iter()
            .find(|(known, _)| known == &key)
            .and_then(|(_, record)| record.upgrade())
        {
            return record;
        }
        let record = Arc::new(AsyncMutex::new(RecentlySpentOutPoints::new(key.script.clone())));
        records.push((key, Arc::downgrade(&record)));
        record
    }

    /// How many records are alive; the entries nobody holds are dropped first.
    #[cfg(test)]
    pub(crate) fn live_records(&self) -> usize {
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.retain(|(_, record)| record.strong_count() > 0);
        records.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(byte: u8) -> Bytes { Bytes::from(vec![byte; 25]) }

    #[test]
    fn one_record_per_platform_and_script() {
        let registry = SpentRecordRegistry::default();
        let qtum = registry.get_or_insert("QTUM", script(1));
        let token = registry.get_or_insert("QTUM", script(1));
        assert!(Arc::ptr_eq(&qtum, &token), "same platform and script: one record");

        let other_address = registry.get_or_insert("QTUM", script(2));
        assert!(!Arc::ptr_eq(&qtum, &other_address), "another script: another record");

        let other_platform = registry.get_or_insert("tQTUM", script(1));
        assert!(!Arc::ptr_eq(&qtum, &other_platform), "another platform: another record");

        let empty = registry.get_or_insert("QTUM", Bytes::new());
        assert!(
            !Arc::ptr_eq(&qtum, &empty),
            "the empty (Trezor) script keeps its own record"
        );
        assert_eq!(registry.live_records(), 4);
    }

    /// D38.6.4Q.4 status note, item 1, under an HD login: QTUM's enabled address and the tokens' internal-key address are
    /// different scripts, so two records; a QTUM activated with `priv_key_policy` `IguanaPrivKey` builds its record
    /// for the internal-key address too, so it shares the tokens' record. (The builder gives every instance the script
    /// of the address it activates; this pins what the registry does with those scripts.)
    #[test]
    fn hd_enabled_address_and_internal_key_address_get_two_records_and_an_iguana_qtum_shares_the_tokens() {
        let registry = SpentRecordRegistry::default();
        let (hd_enabled_address, internal_key_address) = (script(10), script(20));
        let qtum_hd = registry.get_or_insert("QTUM", hd_enabled_address);
        let token_a = registry.get_or_insert("QTUM", internal_key_address.clone());
        let token_b = registry.get_or_insert("QTUM", internal_key_address.clone());
        let qtum_iguana = registry.get_or_insert("QTUM", internal_key_address);
        assert!(!Arc::ptr_eq(&qtum_hd, &token_a));
        assert!(Arc::ptr_eq(&token_a, &token_b));
        assert!(Arc::ptr_eq(&token_a, &qtum_iguana));
        assert_eq!(registry.live_records(), 2);
    }

    #[test]
    fn the_last_holder_ends_the_record() {
        let registry = SpentRecordRegistry::default();
        let first = registry.get_or_insert("QTUM", script(1));
        let second = registry.get_or_insert("QTUM", script(1));
        drop(first);
        assert_eq!(registry.live_records(), 1, "one holder is left");
        drop(second);
        assert_eq!(registry.live_records(), 0, "the last holder ended the record");
        // Enabling again starts empty and bound to the script.
        let fresh = registry.get_or_insert("QTUM", script(1));
        assert_eq!(registry.live_records(), 1);
        drop(fresh);
    }
}
