use async_trait::async_trait;
use common::executor::Timer;
use common::log::{error, warn};
use derive_more::Display;
use futures::{pin_mut, select, FutureExt};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use std::future::Future;
use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
pub use native_lock::SwapLock;
#[cfg(target_arch = "wasm32")] pub use wasm_lock::SwapLock;

pub type SwapLockResult<T> = Result<T, MmError<SwapLockError>>;

#[derive(Debug, Display)]
pub enum SwapLockError {
    #[display(fmt = "Error reading timestamp: {}", _0)]
    ErrorReadingTimestamp(String),
    #[display(fmt = "Error writing timestamp: {}", _0)]
    ErrorWritingTimestamp(String),
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    /// The run no longer holds the swap lock (CRD ch.44 R44.9A.13 item 3): another runner took it, it is gone, or
    /// refreshes kept failing past the lock's time limit.
    #[display(fmt = "Swap lock lost: {}", _0)]
    LockLost(String),
}

impl SwapLockError {
    pub fn is_lock_lost(&self) -> bool { matches!(self, SwapLockError::LockLost(_)) }
}

#[async_trait]
pub trait SwapLockOps: Sized {
    async fn lock(ctx: &MmArc, swap_uuid: Uuid, ttl_sec: f64) -> SwapLockResult<Option<Self>>;

    async fn touch(&self) -> SwapLockResult<()>;
}

/// Refreshes `lock` every `interval_sec` seconds until a refresh reports that the lock is lost, and returns the
/// reason (CRD ch.44 R44.9A.13 item 3). Any other error is logged and retried at the next tick; if refreshes keep
/// failing until the lock's time limit has passed since its last successful write, the lock reports itself lost.
/// A late refresh that still finds the lock's own value (for example after a host suspend) is not a loss.
/// The lock is dropped (and its file removed, if still ours) when this future is dropped.
pub async fn touch_until_lost<L: SwapLockOps>(lock: L, uuid: Uuid, interval_sec: f64) -> String {
    loop {
        match lock.touch().await {
            Ok(()) => (),
            Err(e) => match e.get_inner() {
                SwapLockError::LockLost(reason) => return reason.clone(),
                _ => warn!("Swap {} file lock error: {}", uuid, e),
            },
        }
        Timer::sleep(interval_sec).await;
    }
}

/// How [`run_while_lock_held`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// The swap future ran to its end.
    Finished,
    /// The swap lock was lost and the swap future was dropped.
    Lost,
}

/// Drives `swap_fut` while `lock` is refreshed. If the lock is lost the swap future is dropped at whatever await
/// it is suspended on: the run is stopped where it stands, like an abrupt stop, and starts no further step. An
/// error is logged, no event is written, and [`LockOutcome::Lost`] is returned so the caller can hand the swap to
/// the ordinary kick-start (CRD ch.51 R5 with ch.44 R44.9A.13 item 3).
pub async fn run_while_lock_held<L: SwapLockOps, F: Future<Output = ()>>(
    swap_fut: F,
    lock: L,
    uuid: Uuid,
    interval_sec: f64,
) -> LockOutcome {
    let swap_fut = swap_fut.fuse();
    let touch_loop = touch_until_lost(lock, uuid, interval_sec).fuse();
    pin_mut!(swap_fut, touch_loop);
    select! {
        _swap = swap_fut => LockOutcome::Finished,
        reason = touch_loop => {
            error!(
                "Swap {} run stopped because its swap lock was lost ({}); the swap is handed to the kick-start, \
                 which resumes it from its saved record if the lock is free after the wait (ch.51 R5)",
                uuid, reason
            );
            LockOutcome::Lost
        },
    }
}

/// Calls `hand_over` with `uuid` exactly once if `outcome` is [`LockOutcome::Lost`], otherwise never. The caller
/// invokes it after the run's registry guard has been released, and passes a function that starts the ordinary
/// kick-start of this one swap, which takes the lock by the ch.51 R5 rule (wait one limit, retry once, abandon if
/// still held, no event) and otherwise resumes the swap from its saved record (R6, R7).
pub fn hand_over_if_lost(outcome: LockOutcome, uuid: Uuid, hand_over: impl FnOnce(Uuid)) {
    if outcome == LockOutcome::Lost {
        hand_over(uuid);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_lock {
    use super::*;
    use crate::mm2::lp_swap::my_swaps_dir;
    use mm2_io::file_lock::{FileLock, FileLockError};
    use std::path::PathBuf;

    impl From<FileLockError> for SwapLockError {
        fn from(e: FileLockError) -> Self {
            match e {
                FileLockError::ErrorReadingTimestamp { path, error } => {
                    SwapLockError::ErrorReadingTimestamp(format!("Path: {:?}, Error: {}", path, error))
                },
                FileLockError::ErrorWritingTimestamp { path, error }
                | FileLockError::ErrorCreatingLockFile { path, error } => {
                    SwapLockError::ErrorWritingTimestamp(format!("Path: {:?}, Error: {}", path, error))
                },
                FileLockError::LockLost { path, reason } => {
                    SwapLockError::LockLost(format!("Path: {:?}, Reason: {}", path, reason))
                },
            }
        }
    }

    pub struct SwapLock {
        file_lock: FileLock<PathBuf>,
    }

    #[async_trait]
    impl SwapLockOps for SwapLock {
        async fn lock(ctx: &MmArc, swap_uuid: Uuid, ttl_sec: f64) -> SwapLockResult<Option<SwapLock>> {
            let lock_path = my_swaps_dir(ctx).join(format!("{}.lock", swap_uuid));
            let file_lock = match FileLock::lock(lock_path, ttl_sec).mm_err(Into::into)? {
                Some(lock) => lock,
                None => return Ok(None),
            };
            Ok(Some(SwapLock { file_lock }))
        }

        async fn touch(&self) -> SwapLockResult<()> { Ok(self.file_lock.touch().mm_err(Into::into)?) }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_lock {
    use super::*;
    use crate::mm2::lp_swap::swap_wasm_db::{DbTransactionError, InitDbError, ItemId, SwapLockTable};
    use crate::mm2::lp_swap::SwapsContext;
    use common::executor::spawn;
    use common::log::{debug, error};
    use common::{now_float, now_ms};
    use std::sync::Mutex;

    /// What this lock last wrote (CRD ch.44 R44.9A.13 item 3), kept in memory only.
    struct LockState {
        /// The timestamp this lock last wrote to its row.
        timestamp: u64,
        /// Wall clock time (ms) of that write.
        written_at_ms: u64,
        /// Set once a refresh found the lock lost; a lost lock writes and removes nothing.
        lost: bool,
    }

    impl From<DbTransactionError> for SwapLockError {
        fn from(e: DbTransactionError) -> Self {
            match e {
                e @ DbTransactionError::NoSuchTable { .. }
                | e @ DbTransactionError::ErrorCreatingTransaction(_)
                | e @ DbTransactionError::ErrorOpeningTable { .. }
                | e @ DbTransactionError::ErrorSerializingIndex { .. }
                | e @ DbTransactionError::ErrorSerializingItem(_)
                | e @ DbTransactionError::MultipleItemsByUniqueIndex { .. }
                | e @ DbTransactionError::NoSuchIndex { .. }
                | e @ DbTransactionError::InvalidIndex { .. }
                | e @ DbTransactionError::UnexpectedState(_)
                | e @ DbTransactionError::TransactionAborted => SwapLockError::InternalError(e.to_string()),
                e @ DbTransactionError::ErrorDeserializingItem(_) | e @ DbTransactionError::ErrorGettingItems(_) => {
                    SwapLockError::ErrorReadingTimestamp(e.to_string())
                },
                e @ DbTransactionError::ErrorDeletingItems(_) | e @ DbTransactionError::ErrorUploadingItem(_) => {
                    SwapLockError::ErrorWritingTimestamp(e.to_string())
                },
            }
        }
    }

    impl From<InitDbError> for SwapLockError {
        fn from(e: InitDbError) -> Self { SwapLockError::InternalError(e.to_string()) }
    }

    pub struct SwapLock {
        ctx: MmArc,
        swap_uuid: Uuid,
        /// The identifier of the timestamp record in the `SwapLockTable`.
        pub(super) record_id: ItemId,
        ttl_sec: f64,
        state: Mutex<LockState>,
    }

    impl Drop for SwapLock {
        fn drop(&mut self) {
            let ctx = self.ctx.clone();
            let record_id = self.record_id;
            let swap_uuid = self.swap_uuid;
            let (timestamp, lost) = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (state.timestamp, state.lost)
            };
            let fut = async move {
                if lost {
                    // The row belongs to another runner now.
                    return;
                }
                if let Err(e) = Self::release(ctx, swap_uuid, record_id, timestamp).await {
                    error!("Error realising the SwapLock: {}", e);
                }
                debug!("SwapLock::drop] Finish");
            };
            spawn(fut);
        }
    }

    #[async_trait]
    impl SwapLockOps for SwapLock {
        async fn lock(ctx: &MmArc, uuid: Uuid, ttl_sec: f64) -> SwapLockResult<Option<Self>> {
            let swaps_ctx = SwapsContext::from_ctx(ctx).map_to_mm(SwapLockError::InternalError)?;
            let db = swaps_ctx.swap_db().await.map_mm_err()?;
            let transaction = db.transaction().await.map_mm_err()?;
            let table = transaction.table::<SwapLockTable>().await.map_mm_err()?;

            if let Some((item_id, SwapLockTable { timestamp, .. })) =
                table.get_item_by_unique_index("uuid", uuid).await.map_mm_err()?
            {
                let time_passed = now_float() - timestamp as f64;
                if time_passed <= ttl_sec {
                    return Ok(None);
                }
                // delete the timestamp from the table before the new timestamp is written
                table.delete_item(item_id).await.map_mm_err()?;
            }

            let item = SwapLockTable {
                uuid,
                timestamp: now_ms() / 1000,
            };
            let record_id = table.add_item(&item).await.map_mm_err()?;
            transaction.wait_for_complete().await.map_mm_err()?;

            Ok(Some(SwapLock {
                ctx: ctx.clone(),
                swap_uuid: uuid,
                record_id,
                ttl_sec,
                state: Mutex::new(LockState {
                    timestamp: item.timestamp,
                    written_at_ms: now_ms(),
                    lost: false,
                }),
            }))
        }

        async fn touch(&self) -> SwapLockResult<()> {
            let (last_timestamp, written_at_ms, was_lost) = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (state.timestamp, state.written_at_ms, state.lost)
            };
            if was_lost {
                return self.mark_lost("already marked lost".to_owned());
            }
            match self.refresh(last_timestamp, written_at_ms).await {
                // A take-over (a foreign or missing row) is lost at once; an I/O or database error is lost only if
                // the last successful write is older than the time limit, otherwise it is retried at the next tick.
                Err(e) if !matches!(e.get_inner(), SwapLockError::LockLost(_)) => {
                    match self.failed_too_long(written_at_ms) {
                        Some(reason) => self.mark_lost(reason),
                        None => Err(e),
                    }
                },
                other => other,
            }
        }
    }

    impl SwapLock {
        /// `Some(reason)` if a failed refresh finds the last successful write older than the time limit. A late refresh that still finds this lock's own row is not lost.
        fn failed_too_long(&self, written_at_ms: u64) -> Option<String> {
            let age = now_ms().saturating_sub(written_at_ms) as f64 / 1000.;
            (age > self.ttl_sec).then(|| {
                format!(
                    "refresh failed and the last successful write was {:.0} s ago, over the {} s limit",
                    age, self.ttl_sec
                )
            })
        }

        async fn refresh(&self, last_timestamp: u64, written_at_ms: u64) -> SwapLockResult<()> {
            let swaps_ctx = SwapsContext::from_ctx(&self.ctx).map_to_mm(SwapLockError::InternalError)?;
            let db = swaps_ctx.swap_db().await.map_mm_err()?;

            let transaction = db.transaction().await.map_mm_err()?;
            let table = transaction.table::<SwapLockTable>().await.map_mm_err()?;

            // The row must still be the one this lock wrote: same record, same timestamp. A take-over writes a
            // different row or timestamp; a late refresh that finds its own row is not lost (nobody took the lock).
            match table
                .get_item_by_unique_index("uuid", self.swap_uuid)
                .await
                .map_mm_err()?
            {
                Some((item_id, row)) if item_id == self.record_id && row.timestamp == last_timestamp => (),
                Some(_) => return self.mark_lost("the lock row was replaced by another runner".to_owned()),
                None => return self.mark_lost("the lock row is gone".to_owned()),
            }

            let item = SwapLockTable {
                uuid: self.swap_uuid,
                timestamp: now_ms() / 1000,
            };
            let replaced_record_id = table.replace_item(self.record_id, &item).await.map_mm_err()?;
            transaction.wait_for_complete().await.map_mm_err()?;

            if self.record_id != replaced_record_id {
                let error = format!("Expected {} record id, found {}", self.record_id, replaced_record_id);
                return MmError::err(SwapLockError::ErrorWritingTimestamp(error));
            }
            let late_by = now_ms().saturating_sub(written_at_ms) as f64 / 1000.;
            if late_by > self.ttl_sec {
                warn!(
                    "Swap lock of {} was refreshed {:.0} s after its last write, over the {} s limit; nobody took it",
                    self.swap_uuid, late_by, self.ttl_sec
                );
            }
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.timestamp = item.timestamp;
            state.written_at_ms = now_ms();
            Ok(())
        }

        fn mark_lost(&self, reason: String) -> SwapLockResult<()> {
            self.state.lock().unwrap_or_else(|p| p.into_inner()).lost = true;
            MmError::err(SwapLockError::LockLost(reason))
        }

        /// Deletes the lock row only if it is still the row this lock wrote (same record, same timestamp).
        async fn release(ctx: MmArc, swap_uuid: Uuid, record_id: ItemId, timestamp: u64) -> SwapLockResult<()> {
            let swaps_ctx = SwapsContext::from_ctx(&ctx).map_to_mm(SwapLockError::InternalError)?;
            let db = swaps_ctx.swap_db().await.map_mm_err()?;
            let transaction = db.transaction().await.map_mm_err()?;
            let table = transaction.table::<SwapLockTable>().await.map_mm_err()?;
            match table.get_item_by_unique_index("uuid", swap_uuid).await.map_mm_err()? {
                Some((item_id, row)) if item_id == record_id && row.timestamp == timestamp => {
                    table.delete_item(record_id).await.map_mm_err()?;
                    transaction.wait_for_complete().await.map_mm_err()?;
                },
                _ => (),
            }
            Ok(())
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::wasm_lock::*;
    use super::*;
    use crate::mm2::lp_swap::swap_wasm_db::SwapLockTable;
    use crate::mm2::lp_swap::SwapsContext;
    use common::executor::Timer;
    use common::new_uuid;
    use common::now_ms;
    use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
    use mm2_db::indexed_db::ItemId;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    async fn get_all_items(ctx: &MmArc) -> Vec<(ItemId, SwapLockTable)> {
        let swaps_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        let db = swaps_ctx.swap_db().await.expect("Error getting SwapDb");
        let transaction = db.transaction().await.expect("Error creating transaction");
        let table = transaction.table::<SwapLockTable>().await.expect("Error opening table");
        table.get_all_items().await.expect("Error getting items")
    }

    #[wasm_bindgen_test]
    async fn test_file_lock_should_create_file_and_record_timestamp_and_then_delete_on_drop() {
        let ctx = MmCtxBuilder::new().with_test_db_namespace().into_mm_arc();
        let uuid = new_uuid();

        let started_at = now_ms() / 1000;
        let swap_lock = SwapLock::lock(&ctx, uuid, 10.)
            .await
            .expect("!SwapLock::lock")
            .expect("SwapLock::lock must return a value");

        let items = get_all_items(&ctx).await;
        assert_eq!(items.len(), 1);
        let (record_id, lock_item) = items[0].clone();

        assert_eq!(record_id, swap_lock.record_id);
        assert_eq!(lock_item.uuid, uuid);
        assert!(started_at <= lock_item.timestamp);

        drop(swap_lock);

        Timer::sleep(1.).await;
        let actual = get_all_items(&ctx).await;
        assert!(
            actual.is_empty(),
            "SwapLockTable must be empty after the SwapLock is dropped"
        );
    }

    #[wasm_bindgen_test]
    async fn test_file_lock_should_return_none_if_lock_acquired() {
        let ctx = MmCtxBuilder::new().with_test_db_namespace().into_mm_arc();
        let uuid = new_uuid();
        let _lock = SwapLock::lock(&ctx, uuid, 10.)
            .await
            .expect("!SwapLock::lock")
            .expect("SwapLock::lock must return a value");
        let new_lock = SwapLock::lock(&ctx, uuid, 10.).await.expect("!SwapLock::lock");
        assert!(
            new_lock.is_none(),
            "SwapLock::lock must return None if the lock has already been acquired"
        );
    }

    #[wasm_bindgen_test]
    async fn test_file_lock_should_acquire_and_update_timestamp_if_ttl_expired() {
        let ctx = MmCtxBuilder::new().with_test_db_namespace().into_mm_arc();
        let uuid = new_uuid();

        let started_at = now_ms() / 1000;
        let first_lock = SwapLock::lock(&ctx, uuid, 1.)
            .await
            .expect("!SwapLock::lock")
            .expect("SwapLock::lock must return a value");

        let items = get_all_items(&ctx).await;
        assert_eq!(items.len(), 1);
        let (first_record_id, first_lock_item) = items[0].clone();

        assert_eq!(first_record_id, first_lock.record_id);
        assert_eq!(first_lock_item.uuid, uuid);
        assert!(started_at <= first_lock_item.timestamp);

        Timer::sleep(2.).await;

        let second_lock = SwapLock::lock(&ctx, uuid, 1.)
            .await
            .expect("!SwapLock::lock")
            .expect("SwapLock::lock must return a value after the last ttl is over");

        let items = get_all_items(&ctx).await;
        assert_eq!(items.len(), 1);
        let (second_record_id, second_lock_item) = items[0].clone();

        assert_eq!(second_record_id, second_lock.record_id);
        assert_ne!(first_record_id, second_record_id);
        assert_eq!(second_lock_item.uuid, uuid);
        assert!(first_lock_item.timestamp < second_lock_item.timestamp);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod lock_fencing_tests {
    use super::*;
    use common::{block_on, now_ms};
    use mm2_io::file_lock::FileLock;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A swap lock over a plain `FileLock`, so the loop can be tested without a context.
    struct TestLock(FileLock<PathBuf>);

    #[async_trait]
    impl SwapLockOps for TestLock {
        async fn lock(_ctx: &MmArc, _swap_uuid: Uuid, _ttl_sec: f64) -> SwapLockResult<Option<Self>> { Ok(None) }

        async fn touch(&self) -> SwapLockResult<()> { Ok(self.0.touch().mm_err(Into::into)?) }
    }

    fn lock_in_new_dir(tag: &str) -> (PathBuf, TestLock) {
        let dir = std::env::temp_dir().join(format!("swap-lock-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lock");
        let lock = FileLock::lock(path.clone(), 40.).unwrap().unwrap();
        (path, TestLock(lock))
    }

    #[test]
    fn touch_loop_ends_with_the_reason_when_the_file_is_replaced() {
        let (path, lock) = lock_in_new_dir("loop");
        let other_value = (now_ms() / 1000 + 100).to_string();
        std::fs::write(&path, &other_value).unwrap();
        let reason = block_on(touch_until_lost(lock, Uuid::nil(), 0.05));
        assert!(reason.contains("not the value this lock wrote"), "{}", reason);
        // The lost lock left the other owner's file.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), other_value);
    }

    /// The `select!` of `run_while_lock_held`: a stub swap future that writes a record every 20 ms stops writing
    /// once the lock file is replaced, and the run ends without the swap future finishing.
    #[test]
    fn run_stops_before_the_next_write_when_the_lock_is_lost() {
        let (path, lock) = lock_in_new_dir("run");
        let writes = Arc::new(AtomicUsize::new(0));
        let swap_writes = writes.clone();
        let swap_fut = async move {
            loop {
                Timer::sleep(0.02).await;
                swap_writes.fetch_add(1, Ordering::SeqCst);
            }
        };
        let replace_path = path.clone();
        let replacer = async move {
            Timer::sleep(0.3).await;
            std::fs::write(&replace_path, (now_ms() / 1000 + 100).to_string()).unwrap();
        };
        // The run ends only through the lost lock: the stub never finishes by itself.
        block_on(futures::future::join(
            run_while_lock_held(swap_fut, lock, Uuid::nil(), 0.05),
            replacer,
        ));
        let at_stop = writes.load(Ordering::SeqCst);
        assert!(at_stop > 0, "the swap future ran while the lock was held");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            writes.load(Ordering::SeqCst),
            at_stop,
            "no write after the lock was lost"
        );
        assert!(path.exists(), "the other owner's file stays");
    }

    #[test]
    fn run_ends_normally_when_the_swap_finishes_and_removes_its_lock_file() {
        let (path, lock) = lock_in_new_dir("finish");
        let outcome = block_on(run_while_lock_held(async {}, lock, Uuid::nil(), 30.));
        assert_eq!(outcome, LockOutcome::Finished);
        assert!(!path.exists(), "the run's own lock file is removed on exit");
    }

    /// The hand-over runs once, with the uuid, after a lost lock; never after a normal finish.
    #[test]
    fn hand_over_runs_exactly_once_after_a_lost_lock_and_never_after_a_finish() {
        let uuid = Uuid::new_v4();
        let calls = std::cell::RefCell::new(Vec::new());
        hand_over_if_lost(LockOutcome::Finished, uuid, |u| calls.borrow_mut().push(u));
        assert!(calls.borrow().is_empty());

        let (path, lock) = lock_in_new_dir("handover");
        std::fs::write(&path, (now_ms() / 1000 + 100).to_string()).unwrap();
        let outcome = block_on(run_while_lock_held(std::future::pending::<()>(), lock, uuid, 0.05));
        assert_eq!(outcome, LockOutcome::Lost);
        hand_over_if_lost(outcome, uuid, |u| calls.borrow_mut().push(u));
        assert_eq!(*calls.borrow(), vec![uuid]);
    }

    /// What the hand-over's kick-start does with the lock (here with `FileLock::lock`, the file layer of the R5
    /// acquisition): a free lock (file removed) is taken again with a new value; a lock that another runner holds
    /// with a current value is not taken. The wait and the retry of R5 live in the run functions and are not run here.
    #[test]
    fn hand_over_takes_a_free_lock_and_leaves_a_held_one() {
        // Free: the file vanished (lost as missing), then the hand-over takes it.
        let (path, lock) = lock_in_new_dir("handover-free");
        std::fs::remove_file(&path).unwrap();
        let outcome = block_on(run_while_lock_held(
            std::future::pending::<()>(),
            lock,
            Uuid::nil(),
            0.05,
        ));
        assert_eq!(outcome, LockOutcome::Lost);
        let mut taken = None;
        hand_over_if_lost(outcome, Uuid::nil(), |_| {
            taken = FileLock::lock(path.clone(), 40.).unwrap()
        });
        assert!(taken.is_some(), "a free lock is taken again");
        assert!(path.exists());
        drop(taken);

        // Held: another runner replaced the content with a current time. It is one second ahead of ours, because
        // a value equal to our own (same second) is not told apart from ours; a value in the future counts as held
        // (CRD ch.44 R44.9A.8).
        let (path, lock) = lock_in_new_dir("handover-held");
        let other = (now_ms() / 1000 + 1).to_string();
        std::fs::write(&path, &other).unwrap();
        let outcome = block_on(run_while_lock_held(
            std::future::pending::<()>(),
            lock,
            Uuid::nil(),
            0.05,
        ));
        assert_eq!(outcome, LockOutcome::Lost);
        let mut taken = None;
        hand_over_if_lost(outcome, Uuid::nil(), |_| {
            taken = FileLock::lock(path.clone(), 40.).unwrap()
        });
        assert!(taken.is_none(), "a lock held with a current value is not taken");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), other);
    }
}
