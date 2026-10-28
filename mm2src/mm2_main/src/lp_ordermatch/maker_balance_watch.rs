//! The balance watch for the node's own maker orders (CRD ch.60 R60.4.5, R60.4.6).
//!
//! A coin's balance is polled only while the node has at least one maker order with that coin as base,
//! and only to keep those orders' maximum volume honest. The registry of running watches lives in
//! [`MakerOrdersContext`], next to the per-base-coin order counts that start and stop it: the last order
//! of a base coin leaving the store (cancelled, matched away, expired, or removed by disabling the coin)
//! ends that coin's watch in the same lock scope, and the watch task, which wakes only every
//! [`MAKER_BALANCE_WATCH_INTERVAL_SECS`], then finds its registration gone and ends without reading.
//! A new watch for the same coin gets a new generation, so an old task can never poll for it.
use super::*;
use coins::CoinsContext;
use std::future::Future;

/// The fixed pause between two polls of one watch (R60.4.6 item 1). The sleep comes first. It is not read from
/// the daemon configuration and there is no operator override.
pub(crate) const MAKER_BALANCE_WATCH_INTERVAL_SECS: f64 = 30.;

/// The registry of the running watches, one per base coin at most.
#[derive(Default)]
pub(crate) struct BalanceWatches {
    next_generation: u64,
    watches: HashMap<String, BalanceWatch>,
}

struct BalanceWatch {
    generation: u64,
    /// The last balance seen: the seed given at order creation, else the last successful poll. `None` until
    /// the first successful poll of an unseeded watch, so that poll counts as a change (R60.4.6 item 4).
    last_seen: Option<BigDecimal>,
}

/// What a poll that read `balance` has to do next.
#[derive(Debug, PartialEq)]
pub(crate) enum BalanceObservation {
    /// The watch is gone or superseded, or no maker order with the base coin is left: end the task.
    Stale,
    /// The balance equals the last value seen: nothing to recompute.
    Unchanged,
    /// The balance differs from the last value seen (or none was seen yet): recompute once.
    Changed,
}

impl BalanceWatches {
    /// Registers a watch unless one runs already; returns its generation when it was registered.
    fn start(&mut self, ticker: &str, seed: Option<BigDecimal>) -> Option<u64> {
        if self.watches.contains_key(ticker) {
            return None;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.watches.insert(ticker.to_owned(), BalanceWatch {
            generation,
            last_seen: seed,
        });
        Some(generation)
    }

    /// Ends the watch of `ticker`, if any.
    pub(super) fn stop(&mut self, ticker: &str) { self.watches.remove(ticker); }

    /// Ends the watch of `ticker` only if it is the given generation.
    fn stop_generation(&mut self, ticker: &str, generation: u64) {
        if self.is_current(ticker, generation) {
            self.stop(ticker);
        }
    }

    fn is_current(&self, ticker: &str, generation: u64) -> bool {
        self.watches.get(ticker).map(|w| w.generation) == Some(generation)
    }
}

impl MakerOrdersContext {
    /// Registers the watch of `ticker` if a maker order with that base coin exists and no watch runs for it.
    /// `seed` is the balance the order creation read, `None` for restored and converted orders (R60.4.6 items
    /// 3 and 4). Returns the generation of the new watch, whose task the caller has to spawn.
    pub(crate) fn begin_balance_watch(&mut self, ticker: &str, seed: Option<BigDecimal>) -> Option<u64> {
        if !self.coin_has_active_maker_orders(ticker) {
            return None;
        }
        self.balance_watches.start(ticker, seed)
    }

    /// Records `balance` as the last one seen by the watch `generation` of `ticker` and tells what to do.
    pub(crate) fn observe_balance(
        &mut self,
        ticker: &str,
        generation: u64,
        balance: &BigDecimal,
    ) -> BalanceObservation {
        if !self.coin_has_active_maker_orders(ticker) {
            self.balance_watches.stop_generation(ticker, generation);
            return BalanceObservation::Stale;
        }
        match self.balance_watches.watches.get_mut(ticker) {
            Some(watch) if watch.generation == generation => {
                if watch.last_seen.as_ref() == Some(balance) {
                    BalanceObservation::Unchanged
                } else {
                    watch.last_seen = Some(balance.clone());
                    BalanceObservation::Changed
                }
            },
            _ => BalanceObservation::Stale,
        }
    }

    /// Whether the watch `generation` of `ticker` is still the registered one and a maker order with that
    /// base coin exists.
    pub(crate) fn balance_watch_is_live(&self, ticker: &str, generation: u64) -> bool {
        self.balance_watches.is_current(ticker, generation) && self.coin_has_active_maker_orders(ticker)
    }

    /// Whether a watch is registered for `ticker`.
    #[cfg(test)]
    pub(crate) fn has_balance_watch(&self, ticker: &str) -> bool { self.balance_watches.watches.contains_key(ticker) }

    /// The generation of the watch registered for `ticker`.
    #[cfg(test)]
    pub(crate) fn balance_watch_generation(&self, ticker: &str) -> Option<u64> {
        self.balance_watches.watches.get(ticker).map(|watch| watch.generation)
    }

    /// The last balance seen by the watch of `ticker`.
    #[cfg(test)]
    pub(crate) fn balance_watch_last_seen(&self, ticker: &str) -> Option<BigDecimal> {
        self.balance_watches
            .watches
            .get(ticker)
            .and_then(|watch| watch.last_seen.clone())
    }
}

/// Starts the watch of `ticker` and its task if a maker order with that base coin exists and no watch runs
/// for it. Call it after adding an own maker order to the store. `seed` is the balance the creation read,
/// `None` for restored and converted orders.
pub(crate) fn start_maker_balance_watch(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    ticker: &str,
    seed: Option<BigDecimal>,
) {
    let generation = match ordermatch_ctx.maker_orders_ctx.lock().begin_balance_watch(ticker, seed) {
        Some(generation) => generation,
        None => return,
    };
    spawn(run_maker_balance_watch(
        ctx.weak(),
        ticker.to_owned(),
        generation,
        Timer::sleep,
    ));
}

/// The task of one watch: sleep, poll, repeat, until the watch is stale or the context is gone. `sleep` is the
/// pause between polls; production passes [`Timer::sleep`], tests pass a clock they drive.
pub(crate) async fn run_maker_balance_watch<S, F>(ctx: MmWeak, ticker: String, generation: u64, sleep: S)
where
    S: Fn(f64) -> F,
    F: Future<Output = ()>,
{
    loop {
        sleep(MAKER_BALANCE_WATCH_INTERVAL_SECS).await;
        let ctx = match MmArc::from_weak(&ctx) {
            Some(ctx) => ctx,
            None => return,
        };
        if !poll_maker_balance(&ctx, &ticker, generation).await {
            return;
        }
    }
}

/// One poll: reads the balance of `ticker` and, only if it moved and a maker order with that base coin still
/// exists, runs the balance handlers once (R60.4.5 item 3). A failed read changes nothing (R60.4.6 item 2).
/// Returns `false` when the watch has ended.
async fn poll_maker_balance(ctx: &MmArc, ticker: &str, generation: u64) -> bool {
    let ordermatch_ctx = match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx,
        Err(e) => {
            log::error!("Balance watch of {}: no ordermatch context: {}", ticker, e);
            return true;
        },
    };
    if !ordermatch_ctx
        .maker_orders_ctx
        .lock()
        .balance_watch_is_live(ticker, generation)
    {
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .balance_watches
            .stop_generation(ticker, generation);
        return false;
    }
    // A coin that is not active (yet), such as the base coin of an order restored at start, costs no read; the
    // watch waits for its activation.
    let coin = match lp_coinfind(ctx, ticker).await {
        Ok(Some(coin)) => coin,
        Ok(None) => return true,
        Err(e) => {
            log::warn!("Balance watch of {}: !lp_coinfind: {}", ticker, e);
            return true;
        },
    };
    let balance = match coin.my_spendable_balance().compat().await {
        Ok(balance) => balance,
        Err(e) => {
            log::warn!("Balance watch of {}: !my_spendable_balance: {}", ticker, e);
            return true;
        },
    };
    let observation = ordermatch_ctx
        .maker_orders_ctx
        .lock()
        .observe_balance(ticker, generation, &balance);
    match observation {
        BalanceObservation::Stale => false,
        BalanceObservation::Unchanged => true,
        BalanceObservation::Changed => {
            match CoinsContext::from_ctx(ctx) {
                Ok(coins_ctx) => coins_ctx.balance_updated(&coin, &balance).await,
                Err(e) => log::error!("Balance watch of {}: no coins context: {}", ticker, e),
            }
            true
        },
    }
}
