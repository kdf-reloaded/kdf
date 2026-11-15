use super::{construct_event_closure, DbTransactionError, DbTransactionResult, InternalItem, ItemId,
            RequestHandlersGuard};
use crate::indexed_db::db_driver::cursor::IdbCursorBuilder;
use common::{deserialize_from_js, serialize_to_js, stringify_js_error};
use futures::channel::mpsc;
use futures::StreamExt;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbObjectStore, IdbRequest};

pub struct IdbObjectStoreImpl {
    pub(crate) object_store: IdbObjectStore,
    pub(crate) aborted: Arc<AtomicBool>,
}

impl IdbObjectStoreImpl {
    pub fn aborted(&self) -> bool { self.aborted.load(Ordering::Relaxed) }

    pub async fn add_item(&self, item: &Json) -> DbTransactionResult<ItemId> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        // The [`InternalItem::item`] is a flatten field, so if we add the item without the [`InternalItem::_item_id`] id,
        // it will be calculated automatically.
        let js_value = match serialize_to_js(item) {
            Ok(value) => value,
            Err(e) => return MmError::err(DbTransactionError::ErrorSerializingItem(e.to_string())),
        };
        let add_request = match self.object_store.add(&js_value) {
            Ok(request) => request,
            Err(e) => return MmError::err(DbTransactionError::ErrorUploadingItem(stringify_js_error(&e))),
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&add_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&add_request);
            return MmError::err(DbTransactionError::ErrorUploadingItem(error));
        }

        Self::item_id_from_completed_request(&add_request)
    }

    pub async fn get_items(&self, index_str: &str, index_value: Json) -> DbTransactionResult<Vec<(ItemId, Json)>> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let index = index_str.to_owned();
        let index_value_js =
            serialize_to_js(&index_value).map_to_mm(|e| DbTransactionError::ErrorSerializingIndex {
                index: index.clone(),
                description: e.to_string(),
            })?;

        let db_index = match self.object_store.index(index_str) {
            Ok(index) => index,
            Err(_) => return MmError::err(DbTransactionError::NoSuchIndex { index }),
        };
        let get_request = match db_index.get_all_with_key(&index_value_js) {
            Ok(request) => request,
            Err(e) => {
                return MmError::err(DbTransactionError::InvalidIndex {
                    index,
                    index_value,
                    description: stringify_js_error(&e),
                })
            },
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&get_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&get_request);
            return MmError::err(DbTransactionError::ErrorGettingItems(error));
        }

        Self::items_from_completed_request(&get_request)
    }

    pub async fn get_item_ids(&self, index_str: &str, index_value: Json) -> DbTransactionResult<Vec<ItemId>> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let index = index_str.to_owned();
        let index_value_js =
            serialize_to_js(&index_value).map_to_mm(|e| DbTransactionError::ErrorSerializingIndex {
                index: index.clone(),
                description: e.to_string(),
            })?;

        let db_index = match self.object_store.index(index_str) {
            Ok(index) => index,
            Err(_) => return MmError::err(DbTransactionError::NoSuchIndex { index }),
        };
        let get_request = match db_index.get_all_keys_with_key(&index_value_js) {
            Ok(request) => request,
            Err(e) => {
                return MmError::err(DbTransactionError::InvalidIndex {
                    index,
                    index_value,
                    description: stringify_js_error(&e),
                })
            },
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&get_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&get_request);
            return MmError::err(DbTransactionError::ErrorGettingItems(error));
        }

        Self::item_ids_from_completed_request(&get_request)
    }

    pub async fn get_all_items(&self) -> DbTransactionResult<Vec<(ItemId, Json)>> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let get_request = match self.object_store.get_all() {
            Ok(request) => request,
            Err(e) => return MmError::err(DbTransactionError::UnexpectedState(stringify_js_error(&e))),
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&get_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&get_request);
            return MmError::err(DbTransactionError::ErrorGettingItems(error));
        }

        Self::items_from_completed_request(&get_request)
    }

    pub async fn replace_item(&self, _item_id: ItemId, item: Json) -> DbTransactionResult<ItemId> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let item_with_key = InternalItem { _item_id, item };
        let js_value = match serialize_to_js(&item_with_key) {
            Ok(value) => value,
            Err(e) => return MmError::err(DbTransactionError::ErrorSerializingItem(e.to_string())),
        };
        let replace_request = match self.object_store.put(&js_value) {
            Ok(request) => request,
            Err(e) => return MmError::err(DbTransactionError::ErrorUploadingItem(stringify_js_error(&e))),
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&replace_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&replace_request);
            return MmError::err(DbTransactionError::ErrorUploadingItem(error));
        }

        Self::item_id_from_completed_request(&replace_request)
    }

    pub async fn delete_item(&self, item_id: ItemId) -> DbTransactionResult<()> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let item_id = JsValue::from(item_id);

        let delete_request = match self.object_store.delete(&item_id) {
            Ok(request) => request,
            Err(e) => return MmError::err(DbTransactionError::ErrorDeletingItems(stringify_js_error(&e))),
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&delete_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&delete_request);
            return MmError::err(DbTransactionError::ErrorDeletingItems(error));
        }

        Ok(())
    }

    pub async fn clear(&self) -> DbTransactionResult<()> {
        if self.aborted.load(Ordering::Relaxed) {
            return MmError::err(DbTransactionError::TransactionAborted);
        }

        let clear_request = match self.object_store.clear() {
            Ok(request) => request,
            Err(e) => return MmError::err(DbTransactionError::ErrorDeletingItems(stringify_js_error(&e))),
        };

        if let Err(_error_event) = Self::wait_for_request_complete(&clear_request).await {
            self.aborted.store(true, Ordering::Relaxed);
            let error = Self::error_from_failed_request(&clear_request);
            return MmError::err(DbTransactionError::ErrorDeletingItems(error));
        }

        Ok(())
    }

    pub fn cursor_builder(&self, index_str: &str) -> DbTransactionResult<IdbCursorBuilder> {
        let db_index = match self.object_store.index(index_str) {
            Ok(index) => index,
            Err(_) => {
                return MmError::err(DbTransactionError::NoSuchIndex {
                    index: index_str.to_owned(),
                })
            },
        };
        Ok(IdbCursorBuilder::new(db_index))
    }

    async fn wait_for_request_complete(request: &IdbRequest) -> Result<JsValue, JsValue> {
        let (tx, mut rx) = mpsc::channel(2);

        let onsuccess_closure = construct_event_closure(Ok, tx.clone());
        let onerror_closure = construct_event_closure(Err, tx.clone());

        request.set_onsuccess(Some(onsuccess_closure.as_ref().unchecked_ref()));
        request.set_onerror(Some(onerror_closure.as_ref().unchecked_ref()));
        // Declared after the closures, so the handlers are withdrawn before the closures are freed.
        let _handlers_guard = RequestHandlersGuard::new(request);

        rx.next().await.expect("The request event channel must not be closed")
    }

    fn item_id_from_completed_request(request: &IdbRequest) -> DbTransactionResult<ItemId> {
        let result_js_value = match request.result() {
            Ok(res) => res,
            Err(e) => return MmError::err(DbTransactionError::UnexpectedState(stringify_js_error(&e))),
        };

        deserialize_from_js(result_js_value).map_to_mm(|e| DbTransactionError::ErrorDeserializingItem(e.to_string()))
    }

    fn items_from_completed_request(request: &IdbRequest) -> DbTransactionResult<Vec<(ItemId, Json)>> {
        let result_js_value = match request.result() {
            Ok(res) => res,
            Err(e) => return MmError::err(DbTransactionError::UnexpectedState(stringify_js_error(&e))),
        };

        if result_js_value.is_null() || result_js_value.is_undefined() {
            return Ok(Vec::new());
        }

        let items: Vec<InternalItem> = match deserialize_from_js(result_js_value) {
            Ok(items) => items,
            Err(e) => return MmError::err(DbTransactionError::ErrorDeserializingItem(e.to_string())),
        };

        Ok(items.into_iter().map(|item| (item._item_id, item.item)).collect())
    }

    fn item_ids_from_completed_request(request: &IdbRequest) -> DbTransactionResult<Vec<ItemId>> {
        let result_js_value = match request.result() {
            Ok(res) => res,
            Err(e) => return MmError::err(DbTransactionError::UnexpectedState(stringify_js_error(&e))),
        };

        if result_js_value.is_null() || result_js_value.is_undefined() {
            return Ok(Vec::new());
        }

        deserialize_from_js(result_js_value).map_to_mm(|e| DbTransactionError::ErrorDeserializingItem(e.to_string()))
    }

    fn error_from_failed_request(request: &IdbRequest) -> String {
        match request.error() {
            Ok(Some(exception)) => format!("{:?}", exception),
            _ => "Unknown".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{IdbDatabaseBuilder, OnUpgradeNeededCb};
    use futures::FutureExt;
    use serde_json::json;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    /// Dropping the future that waits for a request must not abort the transaction the request belongs to.
    #[wasm_bindgen_test]
    async fn test_dropped_request_future_does_not_abort_transaction() {
        // A unique name, so a repeated run in the same browser profile starts from an empty database.
        let db_name = format!("TEST_DROPPED_REQUEST_FUTURE_{}", js_sys::Date::now() as u64);
        let on_upgrade_needed: OnUpgradeNeededCb = Box::new(|upgrader, _old_version, _new_version| {
            let table_upgrader = upgrader.create_table("items")?;
            table_upgrader.create_index("key", false)
        });

        let db = IdbDatabaseBuilder::new(&db_name)
            .with_version(1)
            .with_tables([("items".to_owned(), on_upgrade_needed)])
            .build()
            .await
            .expect("!IdbDatabaseBuilder::build");

        let transaction = db.transaction().expect("!IdbDatabaseImpl::transaction");
        let table = transaction.open_table("items").expect("!open_table");

        // Poll the request future once, so the request is issued and its handlers are installed, then drop it
        // before the request completes.
        let dropped_item = json!({ "key": "dropped" });
        assert!(table.add_item(&dropped_item).now_or_never().is_none());

        // The second write of the same transaction must still succeed.
        let kept_item = json!({ "key": "kept" });
        let kept_id = table
            .add_item(&kept_item)
            .await
            .expect("!add_item after a dropped request future");
        drop(table);

        transaction
            .wait_for_complete()
            .await
            .expect("The transaction must commit");

        let transaction = db.transaction().expect("!IdbDatabaseImpl::transaction");
        let table = transaction.open_table("items").expect("!open_table");
        let items = table.get_all_items().await.expect("!get_all_items");
        assert_eq!(items.len(), 2);
        assert!(items.contains(&(kept_id, kept_item)));
    }
}
