//! The per-ticker activation locks (CRD ch. 55 R55.1.12, ch. 43 D43.1F.2 (b)).
//!
//! Every activation of a ticker, on every family and path (task, one-shot and legacy), holds the lock of
//! its ticker from before it builds anything until it has registered the coin and finished its
//! post-registration steps. An activation that starts while another of the same ticker is running waits
//! for it and then re-checks whether the ticker is registered. Different tickers never wait for each other,
//! with one exception: the lock of a ticker is a shared/exclusive lock.
//!
//! * A ticker's own activation, and `disable_coin` of the ticker, take its lock **exclusively**.
//! * A token or layer-two activation also takes the lock of its **platform shared**, so that a
//!   `disable_coin` of the platform, or an activation of the platform, cannot run while the token is being
//!   registered, while several tokens of one platform still activate in parallel.
//!
//! The lock is written here, over `std` and wakers only, so that it works on every target without a runtime.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Shared,
    Exclusive,
}

#[derive(Default)]
struct LockState {
    readers: usize,
    writer: bool,
    wakers: Vec<Waker>,
}

/// The lock of one ticker.
#[derive(Default)]
struct TickerLock {
    state: Mutex<LockState>,
}

impl TickerLock {
    fn state(&self) -> std::sync::MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Resolves to the held lock once the ticker's lock can be taken in the mode.
struct Acquire {
    lock: Arc<TickerLock>,
    mode: Mode,
}

impl Future for Acquire {
    type Output = Held;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Held> {
        let lock = self.lock.clone();
        let mode = self.mode;
        let mut state = lock.state();
        let free = match mode {
            Mode::Shared => !state.writer,
            Mode::Exclusive => !state.writer && state.readers == 0,
        };
        if !free {
            // A waiter polled again with the same waker is not listed twice.
            if !state.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
                state.wakers.push(cx.waker().clone());
            }
            return Poll::Pending;
        }
        match mode {
            Mode::Shared => state.readers += 1,
            Mode::Exclusive => state.writer = true,
        }
        drop(state);
        Poll::Ready(Held { lock, mode })
    }
}

/// A lock of one ticker that is held; releasing it wakes the waiters.
struct Held {
    lock: Arc<TickerLock>,
    mode: Mode,
}

impl Drop for Held {
    fn drop(&mut self) {
        let wakers = {
            let mut state = self.lock.state();
            match self.mode {
                Mode::Shared => state.readers -= 1,
                Mode::Exclusive => state.writer = false,
            }
            std::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

/// The activation locks of the tickers that are being activated.
///
/// An entry lives only as long as somebody holds or awaits its lock, so the map does not grow with the
/// number of tickers ever activated.
#[derive(Default)]
pub struct ActivationLocks {
    by_ticker: Mutex<HashMap<String, Weak<TickerLock>>>,
}

/// Holds the activation locks of one or more tickers until it is dropped.
#[must_use = "the activation lock is released when the guard is dropped"]
pub struct ActivationGuard {
    _held: Vec<Held>,
}

impl ActivationLocks {
    fn lock_for(&self, ticker: &str) -> Result<Arc<TickerLock>, String> {
        let mut by_ticker = self
            .by_ticker
            .lock()
            .map_err(|error| format!("The activation lock registry is unavailable: {}", error))?;
        by_ticker.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = by_ticker.get(ticker).and_then(Weak::upgrade) {
            return Ok(lock);
        }

        let lock = Arc::new(TickerLock::default());
        by_ticker.insert(ticker.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }

    /// Waits for the activation locks of all `tickers`, each taken exclusively, and returns a guard that holds
    /// them.
    ///
    /// The locks are acquired in the sorted order of the tickers, and a ticker named twice is locked once,
    /// so two activations that share several tickers cannot deadlock each other.
    pub async fn lock(&self, tickers: &[&str]) -> Result<ActivationGuard, String> {
        self.lock_with_shared(&[], tickers).await
    }

    /// Like [`Self::lock`], with the locks of the `shared` tickers taken shared: the lock of a platform that a
    /// token or layer-two activation needs to stay registered while it works. A ticker named in both lists is taken
    /// exclusively.
    ///
    /// All locks are acquired in one sorted order of the tickers, whatever their mode (the platform and its
    /// tokens have no fixed order of their own), so no two callers can deadlock each other.
    pub async fn lock_with_shared(&self, shared: &[&str], exclusive: &[&str]) -> Result<ActivationGuard, String> {
        let mut wanted: Vec<(&str, Mode)> = shared.iter().map(|t| (*t, Mode::Shared)).collect();
        wanted.extend(exclusive.iter().map(|t| (*t, Mode::Exclusive)));
        // Sorted by ticker, an exclusive request first, so that dedup keeps it.
        wanted.sort_unstable_by(|a, b| {
            a.0.cmp(b.0)
                .then((b.1 == Mode::Exclusive).cmp(&(a.1 == Mode::Exclusive)))
        });
        wanted.dedup_by(|later, kept| later.0 == kept.0);

        let mut held = Vec::with_capacity(wanted.len());
        for (ticker, mode) in wanted {
            let lock = self.lock_for(ticker)?;
            held.push(Acquire { lock, mode }.await);
        }
        Ok(ActivationGuard { _held: held })
    }

    #[cfg(test)]
    fn waiters(&self, ticker: &str) -> usize {
        let lock = self.lock_for(ticker).unwrap();
        let waiters = lock.state().wakers.len();
        waiters
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

    #[test]
    fn a_shared_lock_lets_other_shared_locks_in_and_keeps_an_exclusive_one_out() {
        let locks = ActivationLocks::default();
        let first = block_on(locks.lock_with_shared(&["PLAT"], &["T1"])).unwrap();
        // Another token of the platform, shared on the platform, does not wait.
        let second = locks
            .lock_with_shared(&["PLAT"], &["T2"])
            .now_or_never()
            .expect("shared locks coexist")
            .unwrap();
        // An exclusive lock of the platform (its own activation, or `disable_coin`) waits for both.
        assert!(locks.lock(&["PLAT"]).now_or_never().is_none());
        drop(first);
        assert!(locks.lock(&["PLAT"]).now_or_never().is_none());
        drop(second);
        assert!(locks.lock(&["PLAT"]).now_or_never().is_some());
    }

    #[test]
    fn an_exclusive_lock_keeps_a_shared_one_out_and_the_same_ticker_stays_exclusive() {
        let locks = ActivationLocks::default();
        let platform = block_on(locks.lock(&["PLAT"])).unwrap();
        assert!(locks.lock_with_shared(&["PLAT"], &["T1"]).now_or_never().is_none());
        drop(platform);
        let token = block_on(locks.lock_with_shared(&["PLAT"], &["T1"])).unwrap();
        // Two activations of one token still wait for each other.
        assert!(locks.lock_with_shared(&["PLAT"], &["T1"]).now_or_never().is_none());
        drop(token);
        assert!(locks.lock_with_shared(&["PLAT"], &["T1"]).now_or_never().is_some());
    }

    #[test]
    fn a_ticker_named_shared_and_exclusive_is_exclusive() {
        let locks = ActivationLocks::default();
        let guard = block_on(locks.lock_with_shared(&["A"], &["A"])).unwrap();
        assert!(locks.lock_with_shared(&["A"], &[]).now_or_never().is_none());
        drop(guard);
    }

    #[test]
    fn a_waiter_polled_again_with_the_same_waker_is_listed_once() {
        use futures::task::noop_waker;
        let locks = ActivationLocks::default();
        let _held = block_on(locks.lock(&["A"])).unwrap();
        let mut waiter = Box::pin(locks.lock(&["A"]));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(waiter.as_mut().poll(&mut cx).is_pending());
        assert!(waiter.as_mut().poll(&mut cx).is_pending());
        assert_eq!(locks.waiters("A"), 1);
    }
}
