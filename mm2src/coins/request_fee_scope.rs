//! Request-scoped reuse of fee readings (CRD ch.38 R38.6.6j item 1, ch.57 R57.2.6 item 1, ch.60 R60.6.7
//! item 8).
//!
//! An order-posting request (`setprice`, `update_maker_order`, `buy`, `sell`, `trade_preimage`,
//! `max_maker_vol`, `max_taker_vol` and the maker balance-update handler) enters a [`RequestFeeScope`] with
//! [`with_request_fee_scope`]. Inside it, the readings that cost a node round trip are taken at most once
//! per coin: the dynamic-fee estimate of a UTXO coin, the forced minimum relay fee and the resolved legacy
//! gas price of an EVM coin. Every consumer of the request then sees the same value.
//!
//! Code outside a scope is not affected: [`memoize`] runs its fetch straight away. Swap execution (payments,
//! spends, refunds, version-two preimages, watcher spends) never runs inside a scope, so it always reads
//! fresh (R38.6.6j item 3). Two properties keep it so:
//!
//! - the scope is carried by the polling of the future handed to [`with_request_fee_scope`], not by a
//!   task or a thread, so a task that is spawned from inside a request does not inherit it;
//! - nothing outside the request handlers above enters one.
//!
//! The scope is a plain thread-local installed around every poll of the wrapped future, which behaves the
//! same on native and `wasm32` targets (no executor or `tokio` task-local is needed).

use futures::lock::Mutex as AsyncMutex;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// What kind of reading is memoized. Together with the coin ticker it forms the key of a reading.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FeeReadKind {
    /// The fee estimate of a dynamic-fee UTXO coin (R38.6.6j item 1).
    UtxoFeeEstimate,
    /// The node's minimum relay fee of a UTXO coin that forces it (R38.6.6j item 1).
    UtxoRelayFee,
    /// The resolved legacy gas price of an EVM coin (R57.2.6).
    EvmGasPrice,
}

type Slot = Arc<AsyncMutex<Option<Box<dyn Any + Send + Sync>>>>;

/// The readings taken so far by one request.
#[derive(Default)]
pub struct RequestFeeScope {
    slots: Mutex<HashMap<(FeeReadKind, String), Slot>>,
}

impl RequestFeeScope {
    fn slot(&self, kind: FeeReadKind, ticker: &str) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        slots.entry((kind, ticker.to_owned())).or_default().clone()
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<RequestFeeScope>>> = const { RefCell::new(None) };
}

fn current_scope() -> Option<Arc<RequestFeeScope>> { CURRENT.with(|current| current.borrow().clone()) }

/// Whether the code being polled runs inside an order-posting request, that is whether [`memoize`] would
/// reuse a reading. Meant for tests that check where a scope is entered.
pub fn is_active() -> bool { current_scope().is_some() }

/// Puts a scope in place for the duration of one poll and restores the previous one afterwards, also when
/// the poll panics.
struct EnterGuard {
    previous: Option<Arc<RequestFeeScope>>,
}

impl EnterGuard {
    fn enter(scope: Arc<RequestFeeScope>) -> EnterGuard {
        let previous = CURRENT.with(|current| current.borrow_mut().replace(scope));
        EnterGuard { previous }
    }
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

struct Scoped<F> {
    scope: Arc<RequestFeeScope>,
    future: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = EnterGuard::enter(self.scope.clone());
        self.future.as_mut().poll(cx)
    }
}

/// Runs `future` as one order-posting request: every fee reading it makes through [`memoize`] is shared by
/// all of its consumers. A call made while a scope is already in place joins that scope, so a request that
/// is itself made of order-posting calls reads each fee once.
pub async fn with_request_fee_scope<F: Future>(future: F) -> F::Output {
    if current_scope().is_some() {
        return future.await;
    }
    Scoped {
        scope: Arc::new(RequestFeeScope::default()),
        future: Box::pin(future),
    }
    .await
}

/// Takes the reading `(kind, ticker)` from `fetch` at most once per request scope.
///
/// Outside a scope this is `fetch.await`. Inside one, concurrent callers wait for the first fetch, and later
/// callers reuse its value. A failed fetch is not remembered: the next caller fetches again.
pub async fn memoize<T, E, F>(kind: FeeReadKind, ticker: &str, fetch: F) -> Result<T, E>
where
    T: Clone + Send + Sync + 'static,
    F: Future<Output = Result<T, E>>,
{
    let scope = match current_scope() {
        Some(scope) => scope,
        None => return fetch.await,
    };
    let slot = scope.slot(kind, ticker);
    let mut reading = slot.lock().await;
    if let Some(value) = reading.as_ref().and_then(|boxed| boxed.downcast_ref::<T>()) {
        return Ok(value.clone());
    }
    let value = fetch.await?;
    *reading = Some(Box::new(value.clone()));
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use futures::future::join;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn counted(counter: &AtomicUsize, value: u64) -> Result<u64, String> {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(value)
    }

    #[test]
    fn outside_a_scope_every_call_reads_fresh() {
        let reads = AtomicUsize::new(0);
        block_on(async {
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 1))
                .await
                .unwrap();
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 2))
                .await
                .unwrap();
        });
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn inside_a_scope_a_reading_is_taken_once_per_kind_and_coin() {
        let reads = AtomicUsize::new(0);
        let values = block_on(with_request_fee_scope(async {
            let first = memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 7))
                .await
                .unwrap();
            let second = memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 8))
                .await
                .unwrap();
            // A different kind and a different coin are separate readings.
            let relay = memoize(FeeReadKind::UtxoRelayFee, "DOGE", counted(&reads, 9))
                .await
                .unwrap();
            let other = memoize(FeeReadKind::UtxoFeeEstimate, "LTC", counted(&reads, 10))
                .await
                .unwrap();
            (first, second, relay, other)
        }));
        assert_eq!(values, (7, 7, 9, 10));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn concurrent_consumers_share_the_first_fetch() {
        let reads = AtomicUsize::new(0);
        let (a, b) = block_on(with_request_fee_scope(join(
            memoize(FeeReadKind::EvmGasPrice, "ETH", counted(&reads, 5)),
            memoize(FeeReadKind::EvmGasPrice, "ETH", counted(&reads, 6)),
        )));
        assert_eq!((a.unwrap(), b.unwrap()), (5, 5));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_fetch_is_not_remembered() {
        let reads = AtomicUsize::new(0);
        block_on(with_request_fee_scope(async {
            let failed: Result<u64, String> = memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", async {
                reads.fetch_add(1, Ordering::SeqCst);
                Err("down".to_owned())
            })
            .await;
            assert!(failed.is_err());
            assert_eq!(
                memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 3)).await,
                Ok(3)
            );
        }));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_nested_scope_joins_the_outer_one_and_the_scope_ends_with_the_request() {
        let reads = AtomicUsize::new(0);
        block_on(with_request_fee_scope(async {
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 1))
                .await
                .unwrap();
            with_request_fee_scope(async {
                memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 2))
                    .await
                    .unwrap();
            })
            .await;
        }));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // The next request starts with nothing remembered, and code outside any request reads fresh.
        block_on(with_request_fee_scope(async {
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 3))
                .await
                .unwrap();
        }));
        block_on(memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 4))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_spawned_task_does_not_inherit_the_scope() {
        let reads = Arc::new(AtomicUsize::new(0));
        let reads_in_task = reads.clone();
        let task = block_on(with_request_fee_scope(async {
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 1))
                .await
                .unwrap();
            // The future is created in the scope, but polled on another thread outside any scope.
            std::thread::spawn(move || {
                block_on(async move {
                    memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads_in_task, 2))
                        .await
                        .unwrap()
                })
            })
        }));
        assert_eq!(task.join().unwrap(), 2);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }
}
