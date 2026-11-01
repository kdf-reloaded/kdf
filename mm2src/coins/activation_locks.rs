//! The per-ticker activation locks (CRD ch. 55 R55.1.12).
//!
//! Every activation of a ticker, on every family and path (task, one-shot and legacy), holds the lock of
//! its ticker from before it builds anything until it has registered the coin and finished its
//! post-registration steps. An activation that starts while another of the same ticker is running waits
//! for it and then re-checks whether the ticker is registered. Different tickers never wait for each other.

use futures::lock::{Mutex as AsyncMutex, OwnedMutexGuard};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// The activation locks of the tickers that are being activated.
///
/// An entry lives only as long as somebody holds or awaits its lock, so the map does not grow with the
/// number of tickers ever activated.
#[derive(Default)]
pub struct ActivationLocks {
    by_ticker: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

/// Holds the activation locks of one or more tickers until it is dropped.
#[must_use = "the activation lock is released when the guard is dropped"]
pub struct ActivationGuard {
    _held: Vec<OwnedMutexGuard<()>>,
}

impl ActivationLocks {
    fn lock_for(&self, ticker: &str) -> Result<Arc<AsyncMutex<()>>, String> {
        let mut by_ticker = self
            .by_ticker
            .lock()
            .map_err(|error| format!("The activation lock registry is unavailable: {}", error))?;
        by_ticker.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = by_ticker.get(ticker).and_then(Weak::upgrade) {
            return Ok(lock);
        }

        let lock = Arc::new(AsyncMutex::new(()));
        by_ticker.insert(ticker.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }

    /// Waits for the activation locks of all `tickers` and returns a guard that holds them.
    ///
    /// The locks are acquired in the sorted order of the tickers, and a ticker named twice is locked once,
    /// so two activations that share several tickers cannot deadlock each other.
    pub async fn lock(&self, tickers: &[&str]) -> Result<ActivationGuard, String> {
        let mut tickers = tickers.to_vec();
        tickers.sort_unstable();
        tickers.dedup();

        let mut held = Vec::with_capacity(tickers.len());
        for ticker in tickers {
            held.push(self.lock_for(ticker)?.lock_owned().await);
        }
        Ok(ActivationGuard { _held: held })
    }

    #[cfg(test)]
    fn tracked_tickers(&self) -> usize {
        let mut by_ticker = self.by_ticker.lock().unwrap();
        by_ticker.retain(|_, lock| lock.strong_count() > 0);
        by_ticker.len()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use common::block_on;
    use futures::future::FutureExt;

    #[test]
    fn locks_are_shared_only_by_ticker() {
        let locks = ActivationLocks::default();
        let first = block_on(locks.lock(&["ARRR"])).unwrap();
        assert!(locks.lock(&["ARRR"]).now_or_never().is_none());
        assert!(locks.lock(&["ZEC"]).now_or_never().is_some());
        drop(first);
        assert!(locks.lock(&["ARRR"]).now_or_never().is_some());
    }

    #[test]
    fn a_multi_ticker_guard_holds_every_ticker_and_tolerates_a_repeated_one() {
        let locks = ActivationLocks::default();
        let guard = block_on(locks.lock(&["USDT", "ETH", "USDT", "DAI"])).unwrap();
        for ticker in ["ETH", "USDT", "DAI"] {
            assert!(locks.lock(&[ticker]).now_or_never().is_none(), "{} is not held", ticker);
        }
        assert!(locks.lock(&["BTC"]).now_or_never().is_some());
        drop(guard);
        assert!(locks.lock(&["ETH", "USDT", "DAI"]).now_or_never().is_some());
    }

    #[test]
    fn a_waiter_that_is_dropped_leaves_the_lock_to_the_holder_and_the_map_is_cleaned() {
        let locks = ActivationLocks::default();
        let first = block_on(locks.lock(&["ARRR"])).unwrap();
        // The waiter is polled once, so it is queued, and then dropped like a cancelled task.
        let mut waiter = Box::pin(locks.lock(&["ARRR"]));
        assert!(waiter.as_mut().now_or_never().is_none());
        drop(waiter);
        assert_eq!(locks.tracked_tickers(), 1);
        assert!(locks.lock(&["ARRR"]).now_or_never().is_none());
        drop(first);
        assert!(locks.lock(&["ARRR"]).now_or_never().is_some());
        assert_eq!(locks.tracked_tickers(), 0);
    }
}
