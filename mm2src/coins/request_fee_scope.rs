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
//!
//! # The short cache (R38.6.6j items 2-4, R38.6.6k, R57.2.6, R57.2.7)
//!
//! A scope also carries its node's [`FeeEstimateCache`]. A fee reading that an order-posting request obtained
//! can be reused by later order-posting requests for at most the lifetime of the asking coin (R38.6.6k): the
//! coin's own `fee_estimate_cache_secs` (0 to 600, fixed), else the top-level `MM2.json`
//! `fee_estimate_cache_secs` (0 to 30, fixed), else the adaptive default. The adaptive lifetime starts at the
//! coin's `avg_blocktime` (2 to 600 seconds, 15 without one) and doubles, up to 600 seconds, each time a fresh
//! reading equals the previous one; a differing reading resets it. A fixed 0 disables the cache for the coin.
//! The cache is reached only through a scope, so code outside a request neither reads from it nor writes to it,
//! and its adaptive state is not touched either. What is cached across requests: the dynamic-fee estimate of a
//! UTXO coin, the forced minimum relay fee of a UTXO coin and the resolved legacy gas price of an EVM chain.
//!
//! A reading is stored under a *key* coin and judged against the lifetime of the *asking* coin. They are the
//! same coin for a UTXO reading; for the gas price of an EVM token the key is its platform coin, so that a
//! platform and all its tokens share one reading and one adaptive state (R57.2.7).

use crate::CoinsContext;
use common::log::error;
use common::now_ms;
use futures::lock::Mutex as AsyncMutex;
use mm2_core::mm_ctx::MmArc;
use serde_json::Value as Json;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// What kind of reading is memoized. Together with the key coin's ticker it forms the key of a reading.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FeeReadKind {
    /// The fee estimate of a dynamic-fee UTXO coin (R38.6.6j item 1).
    UtxoFeeEstimate,
    /// The node's minimum relay fee of a UTXO coin that forces it (R38.6.6j item 1).
    UtxoRelayFee,
    /// The resolved legacy gas price of an EVM coin (R57.2.6).
    EvmGasPrice,
}

/// The default lifetime (R38.6.6j item 2, R38.6.6k item 4), in seconds.
pub const DEFAULT_FEE_ESTIMATE_CACHE_SECS: u64 = 15;
/// The largest value of the top-level `fee_estimate_cache_secs` (R38.6.6j item 2).
pub const MAX_FEE_ESTIMATE_CACHE_SECS: u64 = 30;
/// The largest value of a coin's own `fee_estimate_cache_secs` (R38.6.6k item 1).
pub const MAX_COIN_FEE_ESTIMATE_CACHE_SECS: u64 = 600;
/// The bounds of a lifetime taken from `avg_blocktime` (R38.6.6k item 3), in seconds.
const BLOCKTIME_LIFETIME_BOUNDS: (f64, f64) = (2., 600.);
const FEE_ESTIMATE_CACHE_KEY: &str = "fee_estimate_cache_secs";

/// Reads the top-level `MM2.json` integer `fee_estimate_cache_secs` (R38.6.6j item 2): `None` when the key is
/// absent, else 0 to 30.
///
/// # Errors
///
/// A value that is not an integer, or lies outside 0 to 30, is an error naming the key. The node start fails
/// with it.
pub fn explicit_fee_estimate_cache_secs(conf: &Json) -> Result<Option<u64>, String> {
    match conf.get(FEE_ESTIMATE_CACHE_KEY) {
        None => Ok(None),
        Some(value) => match value.as_u64() {
            Some(secs) if secs <= MAX_FEE_ESTIMATE_CACHE_SECS => Ok(Some(secs)),
            _ => Err(format!(
                "{FEE_ESTIMATE_CACHE_KEY} must be an integer from 0 to {MAX_FEE_ESTIMATE_CACHE_SECS}, got {value}"
            )),
        },
    }
}

/// The per-coin `fee_estimate_cache_secs` (R38.6.6k item 1) of the first of `entries` that carries the key. Pass
/// the coin's own configuration entry first and, for a token, its platform coin's entry second.
///
/// # Errors
///
/// A value that is not an integer from 0 to 600 is an error naming the key. The coin's activation fails with it.
pub fn per_coin_fee_estimate_cache_secs(entries: &[&Json]) -> Result<Option<u64>, String> {
    match entries.iter().find_map(|entry| entry.get(FEE_ESTIMATE_CACHE_KEY)) {
        None => Ok(None),
        Some(value) => match value.as_u64() {
            Some(secs) if secs <= MAX_COIN_FEE_ESTIMATE_CACHE_SECS => Ok(Some(secs)),
            _ => Err(format!(
                "{FEE_ESTIMATE_CACHE_KEY} must be an integer from 0 to {MAX_COIN_FEE_ESTIMATE_CACHE_SECS}, got {value}"
            )),
        },
    }
}

/// The start value of the adaptive default (R38.6.6k item 3), in seconds: the `avg_blocktime` of the first of
/// `entries` that has a positive number, in whole seconds clamped to 2 to 600, else 15. A value that is not a
/// positive number is ignored.
pub fn adaptive_start_secs(entries: &[&Json]) -> u64 {
    let blocktime = entries.iter().find_map(|entry| {
        entry
            .get("avg_blocktime")?
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.)
    });
    match blocktime {
        Some(blocktime) => {
            let (lowest, highest) = BLOCKTIME_LIFETIME_BOUNDS;
            blocktime.floor().clamp(lowest, highest) as u64
        },
        None => DEFAULT_FEE_ESTIMATE_CACHE_SECS,
    }
}

/// How long a reading lives for a coin (R38.6.6k).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeeLifetime {
    /// A fixed number of seconds: the coin's own `fee_estimate_cache_secs` or the node's explicit one. It does
    /// not adapt; 0 means the coin does not use the cache.
    Fixed(u64),
    /// The adaptive default: the lifetime is that of the cache entry, which starts at the start value and
    /// doubles while fresh readings keep returning the same value.
    Adaptive,
}

/// The kind of lifetime a coin has, by the precedence of R38.6.6k: the coin's own `fee_estimate_cache_secs`, else
/// the node's explicit top-level one, else the adaptive default. `entries` are the coin's configuration entry
/// and, for a token, its platform coin's entry after it.
///
/// # Errors
///
/// A per-coin value outside 0 to 600 (see [`per_coin_fee_estimate_cache_secs`]) or an invalid top-level value.
pub fn fee_estimate_lifetime(node_conf: &Json, entries: &[&Json]) -> Result<FeeLifetime, String> {
    if let Some(secs) = per_coin_fee_estimate_cache_secs(entries)? {
        return Ok(FeeLifetime::Fixed(secs));
    }
    if let Some(secs) = explicit_fee_estimate_cache_secs(node_conf)? {
        return Ok(FeeLifetime::Fixed(secs));
    }
    Ok(FeeLifetime::Adaptive)
}

/// The coin entries that decide for `ticker`: the coin's entry of the `coins` list of `node_conf`, followed by
/// the entry of its platform coin (`protocol.protocol_data.platform`) when it names one.
fn entries_of(node_conf: &Json, ticker: &str) -> (Json, Json) {
    let entry_of = |ticker: &str| -> Json {
        node_conf["coins"]
            .as_array()
            .and_then(|coins| coins.iter().find(|coin| coin["coin"].as_str() == Some(ticker)))
            .cloned()
            .unwrap_or(Json::Null)
    };
    let own = entry_of(ticker);
    let platform = own["protocol"]["protocol_data"]["platform"]
        .as_str()
        .map_or(Json::Null, entry_of);
    (own, platform)
}

/// [`fee_estimate_lifetime`] for the coin `ticker` of the node whose `MM2.json` is `node_conf`.
pub fn fee_estimate_lifetime_of(node_conf: &Json, ticker: &str) -> Result<FeeLifetime, String> {
    let (own, platform) = entries_of(node_conf, ticker);
    fee_estimate_lifetime(node_conf, &[&own, &platform])
}

/// [`adaptive_start_secs`] for the coin `ticker` of the node whose `MM2.json` is `node_conf`.
pub fn adaptive_start_secs_of(node_conf: &Json, ticker: &str) -> u64 {
    let (own, platform) = entries_of(node_conf, ticker);
    adaptive_start_secs(&[&own, &platform])
}

struct CachedReading {
    /// The last fresh reading of the entry: the value a coin reuses, and the one the next fresh reading is
    /// compared with.
    value: Box<dyn Any + Send + Sync>,
    /// When it was obtained, in milliseconds of the cache's clock.
    obtained_ms: u64,
    /// The lifetime of the adaptive default for this entry, in seconds (R38.6.6k item 3).
    adaptive_secs: u64,
}

/// The fee estimates that order-posting requests of one node obtained, per kind and key coin (R38.6.6j item 2).
///
/// It lives in the node's [`CoinsContext`]. A reading is reusable by a coin while it is younger than that coin's
/// lifetime (R38.6.6k): the fixed one the coin has, or, on the adaptive default, the entry's own. A coin with a
/// fixed lifetime of 0 neither reads nor writes. A coin that is disabled or registered again loses its entries,
/// so a new instance of a coin never reads what the previous one cached (a request still running on the old
/// instance can store one more reading after that, which the lifetime bounds).
///
/// **The adaptive state** (R38.6.6k item 3) is kept in the entry, so it is per key coin: per coin, and per
/// chain for the shared gas price. A fresh reading that equals the entry's previous one doubles the entry's
/// lifetime up to 600 s, but only when the previous reading was at least that lifetime old, that is when this
/// really is a fetch that no young enough reading prevented; a differing reading resets the lifetime to the
/// start value of the key coin. A coin with a fixed lifetime that fetched stores its reading in the same way:
/// the entry then stays the latest truth about the value for the coins on the adaptive default, a changed value
/// resets the lifetime at once, and an unchanged one never counts as more evidence than the time that really
/// passed between the two readings.
pub struct FeeEstimateCache {
    lifetime_of: Box<dyn Fn(&str) -> FeeLifetime + Send + Sync>,
    start_secs_of: Box<dyn Fn(&str) -> u64 + Send + Sync>,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    readings: Mutex<HashMap<(FeeReadKind, String), CachedReading>>,
}

impl FeeEstimateCache {
    /// The cache of the node `ctx`, on the wall clock. A coin's lifetime is resolved from the node's
    /// configuration when it is asked for (R38.6.6k), so it follows nothing but that configuration.
    pub fn for_node(ctx: &MmArc) -> FeeEstimateCache {
        let (lifetime_node, start_node) = (ctx.weak(), ctx.weak());
        FeeEstimateCache::with_lifetimes(
            move |ticker| {
                let Some(ctx) = MmArc::from_weak(&lifetime_node) else {
                    return FeeLifetime::Fixed(0);
                };
                // Activation refused an invalid value, so this is not reached with one; if it ever is, the
                // coin simply does not use the cache.
                fee_estimate_lifetime_of(&ctx.conf, ticker).unwrap_or_else(|e| {
                    error!("{ticker}: no fee estimate lifetime, reading afresh: {e}");
                    FeeLifetime::Fixed(0)
                })
            },
            move |ticker| match MmArc::from_weak(&start_node) {
                Some(ctx) => adaptive_start_secs_of(&ctx.conf, ticker),
                None => DEFAULT_FEE_ESTIMATE_CACHE_SECS,
            },
            now_ms,
        )
    }

    /// A cache in which every coin has the fixed lifetime `secs`, on the given clock in milliseconds.
    pub fn with_clock(secs: u64, clock: impl Fn() -> u64 + Send + Sync + 'static) -> FeeEstimateCache {
        FeeEstimateCache::with_lifetimes(move |_| FeeLifetime::Fixed(secs), |_| 0, clock)
    }

    /// A cache in which every coin is on the adaptive default with the start value `start_secs`.
    pub fn adaptive_with_clock(start_secs: u64, clock: impl Fn() -> u64 + Send + Sync + 'static) -> FeeEstimateCache {
        FeeEstimateCache::with_lifetimes(|_| FeeLifetime::Adaptive, move |_| start_secs, clock)
    }

    /// A cache on the given clock, in milliseconds, whose coins have the lifetimes `lifetime_of` gives and whose
    /// entries start at `start_secs_of(key coin)`. A clock that steps back makes the readings older than any
    /// lifetime, never younger.
    pub fn with_lifetimes(
        lifetime_of: impl Fn(&str) -> FeeLifetime + Send + Sync + 'static,
        start_secs_of: impl Fn(&str) -> u64 + Send + Sync + 'static,
        clock: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> FeeEstimateCache {
        FeeEstimateCache {
            lifetime_of: Box::new(lifetime_of),
            start_secs_of: Box::new(start_secs_of),
            clock: Box::new(clock),
            readings: Mutex::new(HashMap::new()),
        }
    }

    fn readings(&self) -> std::sync::MutexGuard<'_, HashMap<(FeeReadKind, String), CachedReading>> {
        self.readings.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The reading `(kind, key)` if it is younger than the lifetime of the `asking` coin.
    fn lookup<T: Clone + 'static>(&self, kind: FeeReadKind, key: &str, asking: &str) -> Option<T> {
        let lifetime = (self.lifetime_of)(asking);
        if lifetime == FeeLifetime::Fixed(0) {
            return None;
        }
        let now = (self.clock)();
        let readings = self.readings();
        let reading = readings.get(&(kind, key.to_owned()))?;
        let lifetime_secs = match lifetime {
            FeeLifetime::Fixed(secs) => secs,
            FeeLifetime::Adaptive => reading.adaptive_secs,
        };
        let age = now.checked_sub(reading.obtained_ms)?;
        if age >= lifetime_secs.saturating_mul(1000) {
            return None;
        }
        reading.value.downcast_ref::<T>().cloned()
    }

    /// Keeps a reading that the `asking` coin just fetched, unless its lifetime is fixed at 0, and updates the
    /// adaptive state of the entry (see the type's documentation).
    fn store<T: Clone + PartialEq + Send + Sync + 'static>(
        &self,
        kind: FeeReadKind,
        key: &str,
        asking: &str,
        value: &T,
    ) {
        if (self.lifetime_of)(asking) == FeeLifetime::Fixed(0) {
            return;
        }
        let start_secs = (self.start_secs_of)(key);
        let now = (self.clock)();
        let mut readings = self.readings();
        match readings.get_mut(&(kind, key.to_owned())) {
            Some(reading) => {
                let unchanged = reading.value.downcast_ref::<T>() == Some(value);
                if !unchanged {
                    reading.adaptive_secs = start_secs;
                } else if now
                    .checked_sub(reading.obtained_ms)
                    .is_some_and(|age| age >= reading.adaptive_secs.saturating_mul(1000))
                {
                    reading.adaptive_secs = reading
                        .adaptive_secs
                        .saturating_mul(2)
                        .min(MAX_COIN_FEE_ESTIMATE_CACHE_SECS);
                }
                reading.value = Box::new(value.clone());
                reading.obtained_ms = now;
            },
            None => {
                readings.insert((kind, key.to_owned()), CachedReading {
                    value: Box::new(value.clone()),
                    obtained_ms: now,
                    adaptive_secs: start_secs,
                });
            },
        }
    }

    /// Drops the reading `(kind, key)`, so the next request that wants it reads afresh.
    pub fn discard(&self, kind: FeeReadKind, key: &str) { self.readings().remove(&(kind, key.to_owned())); }

    /// Drops every entry keyed by the coin `ticker`.
    pub fn forget_coin(&self, ticker: &str) { self.readings().retain(|(_, cached), _| cached != ticker); }
}

type Slot = Arc<AsyncMutex<Option<Box<dyn Any + Send + Sync>>>>;

/// The readings taken so far by one request, and the node's cache they may come from.
#[derive(Default)]
pub struct RequestFeeScope {
    slots: Mutex<HashMap<(FeeReadKind, String), Slot>>,
    cache: Option<Arc<FeeEstimateCache>>,
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

/// Runs `future` as one order-posting request of the node `ctx`: every fee reading it makes through
/// [`memoize`] is shared by all of its consumers, and the fee estimates may come from, and go to, the node's
/// short cache. A call made while a scope is already in place joins that scope, so a request that is itself
/// made of order-posting calls reads each fee once.
pub async fn with_request_fee_scope<F: Future>(ctx: &MmArc, future: F) -> F::Output {
    if current_scope().is_some() {
        return future.await;
    }
    // The cache only saves node round trips, so a request whose node context cannot be had reads fresh.
    let cache = match CoinsContext::from_ctx(ctx) {
        Ok(coins_ctx) => Some(coins_ctx.fee_estimate_cache.clone()),
        Err(e) => {
            error!("The fee estimate cache is not available, reading fee estimates afresh: {e}");
            None
        },
    };
    run_in_scope(cache, future).await
}

async fn run_in_scope<F: Future>(cache: Option<Arc<FeeEstimateCache>>, future: F) -> F::Output {
    if current_scope().is_some() {
        return future.await;
    }
    Scoped {
        scope: Arc::new(RequestFeeScope {
            slots: Mutex::default(),
            cache,
        }),
        future: Box::pin(future),
    }
    .await
}

/// Takes the reading `(kind, ticker)` from `fetch` at most once per request scope, for the coin `ticker` itself;
/// see [`memoize_shared`].
pub async fn memoize<T, E, F>(kind: FeeReadKind, ticker: &str, fetch: F) -> Result<T, E>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    F: Future<Output = Result<T, E>>,
{
    memoize_shared(kind, ticker, ticker, fetch).await
}

/// Takes the reading `(kind, key)` from `fetch` at most once per request scope, on behalf of the coin `asking`.
///
/// `key` names the coin the reading belongs to; the readings of several coins that read the same thing share one
/// key (the platform coin of an EVM chain for its gas price, R57.2.7). `asking` is the coin that wants the value:
/// whether a cached reading is young enough is judged against its lifetime (R38.6.6k).
///
/// Outside a scope this is `fetch.await`: it neither reads the node's cache nor writes to it. Inside one,
/// concurrent callers wait for the first fetch, and later callers reuse its value. The first caller of a fee
/// estimate takes a young enough reading of an earlier request from the node's cache instead of fetching, and a
/// value it fetched goes to the cache. A failed fetch is not remembered: the next caller fetches again.
pub async fn memoize_shared<T, E, F>(kind: FeeReadKind, key: &str, asking: &str, fetch: F) -> Result<T, E>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    F: Future<Output = Result<T, E>>,
{
    let scope = match current_scope() {
        Some(scope) => scope,
        None => return fetch.await,
    };
    let slot = scope.slot(kind, key);
    let mut reading = slot.lock().await;
    if let Some(value) = reading.as_ref().and_then(|boxed| boxed.downcast_ref::<T>()) {
        return Ok(value.clone());
    }
    if let Some(value) = scope
        .cache
        .as_ref()
        .and_then(|cache| cache.lookup::<T>(kind, key, asking))
    {
        *reading = Some(Box::new(value.clone()));
        return Ok(value);
    }
    let value = fetch.await?;
    if let Some(cache) = &scope.cache {
        cache.store(kind, key, asking, &value);
    }
    *reading = Some(Box::new(value.clone()));
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use futures::future::join;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// One order-posting request without the node's cache (only the sharing within the request).
    fn scoped<F: Future>(future: F) -> impl Future<Output = F::Output> { run_in_scope(None, future) }

    /// A cache on a clock the test sets, in milliseconds.
    fn cache_on(ttl_secs: u64) -> (Arc<FeeEstimateCache>, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let cache = FeeEstimateCache::with_clock(ttl_secs, move || clock.load(Ordering::SeqCst));
        (Arc::new(cache), now)
    }

    /// One order-posting request of the node whose cache is `cache`: takes the reading and returns its value.
    fn request(cache: &Arc<FeeEstimateCache>, kind: FeeReadKind, ticker: &str, reads: &AtomicUsize, value: u64) -> u64 {
        block_on(run_in_scope(Some(cache.clone()), async {
            memoize(kind, ticker, counted(reads, value)).await.unwrap()
        }))
    }

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
        let values = block_on(scoped(async {
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
        let (a, b) = block_on(scoped(join(
            memoize(FeeReadKind::EvmGasPrice, "ETH", counted(&reads, 5)),
            memoize(FeeReadKind::EvmGasPrice, "ETH", counted(&reads, 6)),
        )));
        assert_eq!((a.unwrap(), b.unwrap()), (5, 5));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_fetch_is_not_remembered() {
        let reads = AtomicUsize::new(0);
        block_on(scoped(async {
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
        block_on(scoped(async {
            memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 1))
                .await
                .unwrap();
            scoped(async {
                memoize(FeeReadKind::UtxoFeeEstimate, "DOGE", counted(&reads, 2))
                    .await
                    .unwrap();
            })
            .await;
        }));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // The next request starts with nothing remembered, and code outside any request reads fresh.
        block_on(scoped(async {
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
        let task = block_on(scoped(async {
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

    const ESTIMATE: FeeReadKind = FeeReadKind::UtxoFeeEstimate;

    #[test]
    fn a_later_request_reuses_an_estimate_within_the_ttl_measured_from_when_it_was_obtained() {
        // R38.6.6j item 2 for the UTXO estimate and R57.2.6 for the gas price.
        for kind in [ESTIMATE, FeeReadKind::EvmGasPrice] {
            let reads = AtomicUsize::new(0);
            let (cache, now) = cache_on(15);
            assert_eq!(request(&cache, kind, "DOGE", &reads, 1), 1);
            now.fetch_add(14_999, Ordering::SeqCst);
            assert_eq!(request(&cache, kind, "DOGE", &reads, 2), 1, "{kind:?} reused");
            assert_eq!(reads.load(Ordering::SeqCst), 1);

            // The age counts from the fetch, not from the last use: at 15 s the reading is stale.
            now.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request(&cache, kind, "DOGE", &reads, 3), 3, "{kind:?} fetched again");
            assert_eq!(reads.load(Ordering::SeqCst), 2);
            // and the new reading starts its own TTL.
            now.fetch_add(14_000, Ordering::SeqCst);
            assert_eq!(request(&cache, kind, "DOGE", &reads, 4), 3);
            assert_eq!(reads.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn a_cache_of_zero_seconds_shares_within_a_request_only() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(0);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 1), 1);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 2), 2);
        assert_eq!(reads.load(Ordering::SeqCst), 2, "one fetch per request");

        // Within one request the single reading is still shared (item 1).
        let within = block_on(run_in_scope(Some(cache.clone()), async {
            let first = memoize(ESTIMATE, "DOGE", counted(&reads, 5)).await.unwrap();
            let second = memoize(ESTIMATE, "DOGE", counted(&reads, 6)).await.unwrap();
            (first, second)
        }));
        assert_eq!(within, (5, 5));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn outside_a_request_the_cache_is_neither_read_nor_written() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(15);
        // Not written: two reads outside a request fetch twice, and a request after them still fetches.
        block_on(memoize(ESTIMATE, "DOGE", counted(&reads, 1))).unwrap();
        block_on(memoize(ESTIMATE, "DOGE", counted(&reads, 2))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 3), 3);
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        // Not read: with a young reading in the cache, a read outside a request still fetches.
        assert_eq!(
            block_on(memoize(ESTIMATE, "DOGE", counted(&reads, 4))),
            Ok::<u64, String>(4)
        );
        assert_eq!(reads.load(Ordering::SeqCst), 4);
        // and what it fetched did not replace the cached reading.
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 5), 3);
    }

    #[test]
    fn a_failed_fetch_is_not_cached() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(15);
        let failed: Result<u64, String> = block_on(run_in_scope(Some(cache.clone()), async {
            memoize(ESTIMATE, "DOGE", async {
                reads.fetch_add(1, Ordering::SeqCst);
                Err("down".to_owned())
            })
            .await
        }));
        assert!(failed.is_err());
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 8), 8);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn coins_and_kinds_have_separate_entries() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(15);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 1), 1);
        assert_eq!(request(&cache, ESTIMATE, "LTC", &reads, 2), 2);
        assert_eq!(request(&cache, FeeReadKind::EvmGasPrice, "DOGE", &reads, 3), 3);
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 9), 1);
        assert_eq!(request(&cache, ESTIMATE, "LTC", &reads, 9), 2);
        assert_eq!(request(&cache, FeeReadKind::EvmGasPrice, "DOGE", &reads, 9), 3);
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn the_forced_relay_fee_is_shared_within_a_request_and_carried_to_the_next() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(15);
        let relay = FeeReadKind::UtxoRelayFee;
        let within = block_on(run_in_scope(Some(cache.clone()), async {
            let first = memoize(relay, "DOGE", counted(&reads, 1)).await.unwrap();
            let second = memoize(relay, "DOGE", counted(&reads, 2)).await.unwrap();
            (first, second)
        }));
        assert_eq!(within, (1, 1));
        // Maintainer decision of 2026-10-28: the relay fee is cached across requests like the estimate.
        assert_eq!(request(&cache, relay, "DOGE", &reads, 3), 1);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_clock_that_steps_back_expires_the_reading() {
        let reads = AtomicUsize::new(0);
        let (cache, now) = cache_on(15);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 1), 1);
        now.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 2), 2);
    }

    #[test]
    fn forgetting_a_coin_drops_its_readings_only() {
        let reads = AtomicUsize::new(0);
        let (cache, _now) = cache_on(15);
        request(&cache, ESTIMATE, "DOGE", &reads, 1);
        request(&cache, ESTIMATE, "LTC", &reads, 2);
        cache.forget_coin("DOGE");
        assert_eq!(request(&cache, ESTIMATE, "DOGE", &reads, 3), 3);
        assert_eq!(request(&cache, ESTIMATE, "LTC", &reads, 4), 2);
    }

    #[test]
    fn the_top_level_fee_estimate_cache_secs_is_absent_or_zero_to_thirty() {
        assert_eq!(explicit_fee_estimate_cache_secs(&json!({})), Ok(None));
        assert_eq!(explicit_fee_estimate_cache_secs(&Json::Null), Ok(None));
        for secs in [0, 1, 15, 30] {
            assert_eq!(
                explicit_fee_estimate_cache_secs(&json!({"fee_estimate_cache_secs": secs})),
                Ok(Some(secs))
            );
        }
    }

    #[test]
    fn the_top_level_fee_estimate_cache_secs_outside_the_range_or_not_an_integer_names_the_key() {
        for bad in [
            json!(31),
            json!(-1),
            json!("15"),
            json!(15.5),
            json!(15.0),
            json!(null),
            json!(true),
        ] {
            let err = explicit_fee_estimate_cache_secs(&json!({ "fee_estimate_cache_secs": bad })).unwrap_err();
            assert!(err.contains("fee_estimate_cache_secs"), "{bad}: {err}");
        }
    }

    // ---- R38.6.6k: the lifetime of a coin ------------------------------------------------------------------

    /// The first lifetime of a coin: the fixed one, or the start value of the adaptive default.
    fn initial_secs(node_conf: &Json, entries: &[&Json]) -> Result<u64, String> {
        Ok(match fee_estimate_lifetime(node_conf, entries)? {
            FeeLifetime::Fixed(secs) => secs,
            FeeLifetime::Adaptive => adaptive_start_secs(entries),
        })
    }

    fn initial_secs_of(node_conf: &Json, ticker: &str) -> Result<u64, String> {
        Ok(match fee_estimate_lifetime_of(node_conf, ticker)? {
            FeeLifetime::Fixed(secs) => secs,
            FeeLifetime::Adaptive => adaptive_start_secs_of(node_conf, ticker),
        })
    }

    fn lifetime(node: Json, own: Json) -> u64 { initial_secs(&node, &[&own]).unwrap() }

    #[test]
    fn the_lifetime_is_the_coins_own_key_then_the_node_setting_then_the_block_time_then_15() {
        let node_5 = json!({"fee_estimate_cache_secs": 5});
        // Per-coin beats global beats block time.
        assert_eq!(
            lifetime(
                node_5.clone(),
                json!({"fee_estimate_cache_secs": 100, "avg_blocktime": 60})
            ),
            100
        );
        assert_eq!(lifetime(node_5.clone(), json!({"avg_blocktime": 60})), 5);
        // Global beats block time beats 15.
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 60})), 60);
        assert_eq!(lifetime(json!({}), json!({})), 15);
        // A per-coin 0 disables that coin whatever the node says; a top-level 0 disables a coin with a block
        // time and no override.
        assert_eq!(lifetime(node_5, json!({"fee_estimate_cache_secs": 0})), 0);
        assert_eq!(
            lifetime(json!({"fee_estimate_cache_secs": 0}), json!({"avg_blocktime": 60})),
            0
        );
        assert_eq!(
            lifetime(
                json!({"fee_estimate_cache_secs": 0}),
                json!({"avg_blocktime": 60, "fee_estimate_cache_secs": 600})
            ),
            600
        );
    }

    #[test]
    fn the_block_time_lifetime_is_clamped_and_a_bad_block_time_is_ignored() {
        let from_blocktime = |value: Json| lifetime(json!({}), json!({ "avg_blocktime": value }));
        assert_eq!(from_blocktime(json!(1)), 2);
        assert_eq!(from_blocktime(json!(2)), 2);
        assert_eq!(from_blocktime(json!(600)), 600);
        assert_eq!(from_blocktime(json!(601)), 600);
        assert_eq!(from_blocktime(json!(86_400)), 600);
        assert_eq!(from_blocktime(json!(7.9)), 7, "whole seconds");
        assert_eq!(from_blocktime(json!(0.5)), 2);
        for ignored in [json!(0), json!(-3), json!("60"), json!(null), json!(true)] {
            assert_eq!(from_blocktime(ignored.clone()), 15, "{ignored} is ignored");
        }
    }

    #[test]
    fn a_per_coin_value_outside_zero_to_six_hundred_is_an_error_naming_the_key() {
        for secs in [0, 1, 600] {
            assert_eq!(
                per_coin_fee_estimate_cache_secs(&[&json!({"fee_estimate_cache_secs": secs})]),
                Ok(Some(secs))
            );
        }
        for bad in [json!(601), json!(-1), json!("5"), json!(1.5), json!(null)] {
            let err = per_coin_fee_estimate_cache_secs(&[&json!({ "fee_estimate_cache_secs": bad })]).unwrap_err();
            assert!(err.contains("fee_estimate_cache_secs"), "{bad}: {err}");
        }
        // A node that sets a per-coin value only 30 may set at the top level: the two ranges differ.
        assert!(explicit_fee_estimate_cache_secs(&json!({"fee_estimate_cache_secs": 31})).is_err());
    }

    #[test]
    fn a_token_falls_back_to_its_platforms_entry_for_both_keys() {
        let node = json!({});
        let silent = json!({});
        let platform = json!({"fee_estimate_cache_secs": 200, "avg_blocktime": 30});
        // The per-coin key: absent in the token's entry, so the platform's.
        assert_eq!(initial_secs(&node, &[&silent, &platform]), Ok(200));
        // The token's own entry wins when it has the key.
        let own = json!({"fee_estimate_cache_secs": 7});
        assert_eq!(initial_secs(&node, &[&own, &platform]), Ok(7));
        // The block time: absent in the token's entry, so the platform's.
        let platform_blocktime = json!({"avg_blocktime": 30});
        assert_eq!(initial_secs(&node, &[&silent, &platform_blocktime]), Ok(30));
        assert_eq!(
            initial_secs(&node, &[&json!({"avg_blocktime": 40}), &platform_blocktime]),
            Ok(40)
        );
        // A block time of the token that cannot be used is ignored, as if absent.
        assert_eq!(
            initial_secs(&node, &[&json!({"avg_blocktime": 0}), &platform_blocktime]),
            Ok(30)
        );
        // An unusable per-coin value of the platform reaches the token.
        let err = initial_secs(&node, &[&silent, &json!({"fee_estimate_cache_secs": 601})]).unwrap_err();
        assert!(err.contains("fee_estimate_cache_secs"), "{err}");
    }

    #[test]
    fn a_coins_lifetime_is_found_in_the_nodes_coins_list_with_the_platform_of_a_token() {
        let node = json!({"coins": [
            {"coin": "ETH", "protocol": {"type": "ETH"}, "avg_blocktime": 12},
            {"coin": "JST", "protocol": {"type": "ERC20", "protocol_data": {"platform": "ETH"}}},
            {"coin": "OWN", "protocol": {"type": "ERC20", "protocol_data": {"platform": "ETH"}},
             "fee_estimate_cache_secs": 3},
            {"coin": "DOGE", "avg_blocktime": 60},
        ]});
        assert_eq!(initial_secs_of(&node, "ETH"), Ok(12));
        assert_eq!(initial_secs_of(&node, "JST"), Ok(12));
        assert_eq!(initial_secs_of(&node, "OWN"), Ok(3));
        assert_eq!(initial_secs_of(&node, "DOGE"), Ok(60));
        assert_eq!(initial_secs_of(&node, "UNLISTED"), Ok(15));
        let global = json!({"fee_estimate_cache_secs": 0, "coins": node["coins"].clone()});
        assert_eq!(initial_secs_of(&global, "DOGE"), Ok(0));
        assert_eq!(initial_secs_of(&global, "OWN"), Ok(3));
    }

    /// Age is judged against the lifetime of the coin that asks, and a coin with lifetime 0 neither reads nor
    /// writes.
    #[test]
    fn a_reading_is_judged_against_the_lifetime_of_the_asking_coin() {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let cache = Arc::new(FeeEstimateCache::with_lifetimes(
            |ticker| match ticker {
                "SHORT" => FeeLifetime::Fixed(5),
                "LONG" => FeeLifetime::Fixed(100),
                _ => FeeLifetime::Fixed(0),
            },
            |_| 0,
            move || clock.load(Ordering::SeqCst),
        ));
        let reads = AtomicUsize::new(0);
        let ask = |key: &str, asking: &str, value: u64| {
            block_on(run_in_scope(Some(cache.clone()), async {
                memoize_shared(ESTIMATE, key, asking, counted(&reads, value))
                    .await
                    .unwrap()
            }))
        };
        assert_eq!(ask("CHAIN", "LONG", 1), 1);
        now.fetch_add(10_000, Ordering::SeqCst);
        assert_eq!(ask("CHAIN", "SHORT", 2), 2, "10 s old is too old for a 5 s coin");
        // The short coin fetched, so the reading is now its own, 0 s old.
        assert_eq!(ask("CHAIN", "LONG", 3), 2);
        now.fetch_add(6_000, Ordering::SeqCst);
        assert_eq!(ask("CHAIN", "LONG", 4), 2, "6 s old is young for a 100 s coin");
        assert_eq!(ask("CHAIN", "SHORT", 5), 5, "and old for a 5 s coin");
        // A coin with lifetime 0 does not read the young reading and does not replace it.
        assert_eq!(ask("CHAIN", "OFF", 6), 6);
        assert_eq!(ask("CHAIN", "LONG", 7), 5);
        assert_eq!(reads.load(Ordering::SeqCst), 4);
    }

    /// R57.2.7: the platform and its tokens share one reading, within a request and across requests; a
    /// coin of another chain has its own.
    #[test]
    fn one_reading_per_chain_for_a_platform_and_its_tokens() {
        let reads = AtomicUsize::new(0);
        let (cache, now) = cache_on(15);
        let gas = FeeReadKind::EvmGasPrice;
        // Within one request: the platform and two tokens, keyed by the platform.
        let within = block_on(run_in_scope(Some(cache.clone()), async {
            let platform = memoize_shared(gas, "ETH", "ETH", counted(&reads, 1)).await.unwrap();
            let first = memoize_shared(gas, "ETH", "JST", counted(&reads, 2)).await.unwrap();
            let second = memoize_shared(gas, "ETH", "USDT", counted(&reads, 3)).await.unwrap();
            let other_chain = memoize_shared(gas, "BNB", "BNB", counted(&reads, 4)).await.unwrap();
            (platform, first, second, other_chain)
        }));
        assert_eq!(within, (1, 1, 1, 4));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        // Across requests, while young: a token asks first this time.
        now.fetch_add(14_000, Ordering::SeqCst);
        let across = block_on(run_in_scope(Some(cache.clone()), async {
            let token = memoize_shared(gas, "ETH", "JST", counted(&reads, 5)).await.unwrap();
            let platform = memoize_shared(gas, "ETH", "ETH", counted(&reads, 6)).await.unwrap();
            let other_chain = memoize_shared(gas, "BNB", "BNB", counted(&reads, 7)).await.unwrap();
            (token, platform, other_chain)
        }));
        assert_eq!(across, (1, 1, 4));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        // Old for everyone: one new resolution for the chain.
        now.fetch_add(2_000, Ordering::SeqCst);
        let after = block_on(run_in_scope(Some(cache.clone()), async {
            let token = memoize_shared(gas, "ETH", "USDT", counted(&reads, 8)).await.unwrap();
            let platform = memoize_shared(gas, "ETH", "ETH", counted(&reads, 9)).await.unwrap();
            (token, platform)
        }));
        assert_eq!(after, (8, 8));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        // Outside a request each coin resolves its own, always.
        block_on(memoize_shared(gas, "ETH", "JST", counted(&reads, 10))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 4);
    }

    // ---- R38.6.6k item 3: the adaptive default ------------------------------------------------------------

    /// A cache whose coins are all on the adaptive default, and a request that fetches `value` unless a young
    /// reading exists. Returns the cache, the clock, and a function that makes one request after advancing the
    /// clock by `wait_secs`; it answers whether the request fetched.
    fn adaptive_cache(start_secs: u64) -> (Arc<FeeEstimateCache>, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let cache = FeeEstimateCache::adaptive_with_clock(start_secs, move || clock.load(Ordering::SeqCst));
        (Arc::new(cache), now)
    }

    /// One request of the coin `ticker` after `wait_secs`, fetching `value`; true if it fetched.
    fn fetched(
        cache: &Arc<FeeEstimateCache>,
        now: &AtomicU64,
        kind: FeeReadKind,
        ticker: &str,
        wait_secs: u64,
        value: u64,
    ) -> bool {
        now.fetch_add(wait_secs * 1000, Ordering::SeqCst);
        let reads = AtomicUsize::new(0);
        request(cache, kind, ticker, &reads, value);
        reads.load(Ordering::SeqCst) == 1
    }

    #[test]
    fn the_adaptive_lifetime_doubles_while_fresh_readings_repeat_up_to_600() {
        for kind in [ESTIMATE, FeeReadKind::EvmGasPrice, FeeReadKind::UtxoRelayFee] {
            let (cache, now) = adaptive_cache(15);
            assert!(
                fetched(&cache, &now, kind, "DOGE", 0, 7),
                "the first reading is fetched"
            );
            // The lifetimes 15, 30, 60, 120, 240, 480, 600: a reading is reused for a second less than its
            // lifetime, and is fetched again at the lifetime.
            for lifetime in [15, 30, 60, 120, 240, 480, 600, 600, 600] {
                assert!(
                    !fetched(&cache, &now, kind, "DOGE", lifetime - 1, 7),
                    "{kind:?}: still young one second before {lifetime} s"
                );
                assert!(
                    fetched(&cache, &now, kind, "DOGE", 1, 7),
                    "{kind:?}: fetched again at {lifetime} s"
                );
            }
        }
    }

    #[test]
    fn a_differing_fresh_reading_resets_the_adaptive_lifetime_to_the_start_value() {
        let (cache, now) = adaptive_cache(15);
        fetched(&cache, &now, ESTIMATE, "DOGE", 0, 7);
        for lifetime in [15, 30, 60] {
            assert!(fetched(&cache, &now, ESTIMATE, "DOGE", lifetime, 7));
        }
        // The lifetime is now 120 s. A reading that differs resets it to 15 s.
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 120, 8));
        assert!(!fetched(&cache, &now, ESTIMATE, "DOGE", 14, 8));
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 1, 8), "15 s again, not 240");
        // and it doubles again from the start value: 30 s next.
        assert!(!fetched(&cache, &now, ESTIMATE, "DOGE", 29, 8));
        assert!(
            fetched(&cache, &now, ESTIMATE, "DOGE", 1, 9),
            "a differing reading at 30 s resets again"
        );
        assert!(!fetched(&cache, &now, ESTIMATE, "DOGE", 14, 9));
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 1, 9));
    }

    #[test]
    fn a_failed_fetch_changes_neither_the_adaptive_lifetime_nor_the_previous_reading() {
        let (cache, now) = adaptive_cache(15);
        fetched(&cache, &now, ESTIMATE, "DOGE", 0, 7);
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 15, 7)); // lifetime 30 s
        now.fetch_add(30_000, Ordering::SeqCst);
        let failed: Result<u64, String> = block_on(run_in_scope(Some(cache.clone()), async {
            memoize(ESTIMATE, "DOGE", async { Err("down".to_owned()) }).await
        }));
        assert!(failed.is_err());
        // The failure did not reset the lifetime (a reset would be 15 s): the next fresh reading, equal to
        // the previous one, doubles it to 60 s.
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 0, 7));
        assert!(!fetched(&cache, &now, ESTIMATE, "DOGE", 59, 7));
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 1, 7));
        // Nor did it replace the previous reading: a differing one after a failure still resets.
        now.fetch_add(120_000, Ordering::SeqCst);
        let failed: Result<u64, String> = block_on(run_in_scope(Some(cache.clone()), async {
            memoize(ESTIMATE, "DOGE", async { Err("down".to_owned()) }).await
        }));
        assert!(failed.is_err());
        assert!(fetched(&cache, &now, ESTIMATE, "DOGE", 0, 8));
        assert!(!fetched(&cache, &now, ESTIMATE, "DOGE", 14, 8));
    }

    #[test]
    fn the_start_value_comes_from_the_block_time_clamped_or_is_15() {
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 600})), 600);
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 1})), 2);
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 601})), 600);
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 60})), 60);
        assert_eq!(lifetime(json!({}), json!({})), 15);
        assert_eq!(lifetime(json!({}), json!({"avg_blocktime": 0})), 15);
        // The entry starts there and doubles from there, and the cap holds whatever the start.
        let node = json!({"coins": [{"coin": "FAST", "avg_blocktime": 1}, {"coin": "SLOW", "avg_blocktime": 400}]});
        let start = |ticker: &str| adaptive_start_secs_of(&node, ticker);
        assert_eq!((start("FAST"), start("SLOW"), start("NONE")), (2, 400, 15));
        let (cache, now) = adaptive_cache(400);
        fetched(&cache, &now, ESTIMATE, "SLOW", 0, 7);
        assert!(!fetched(&cache, &now, ESTIMATE, "SLOW", 399, 7));
        assert!(fetched(&cache, &now, ESTIMATE, "SLOW", 1, 7));
        assert!(!fetched(&cache, &now, ESTIMATE, "SLOW", 599, 7), "800 is capped to 600");
        assert!(fetched(&cache, &now, ESTIMATE, "SLOW", 1, 7));
        assert!(!fetched(&cache, &now, ESTIMATE, "SLOW", 599, 7));
    }

    #[test]
    fn a_fixed_lifetime_does_not_adapt() {
        // A per-coin key and an explicit global value are fixed; a global 0 disables an `avg_blocktime` coin.
        let node = json!({"coins": [
            {"coin": "PER", "avg_blocktime": 60, "fee_estimate_cache_secs": 20},
            {"coin": "BLOCK", "avg_blocktime": 60},
        ]});
        let global = json!({"fee_estimate_cache_secs": 10, "coins": node["coins"].clone()});
        let zero = json!({"fee_estimate_cache_secs": 0, "coins": node["coins"].clone()});
        assert_eq!(fee_estimate_lifetime_of(&node, "PER"), Ok(FeeLifetime::Fixed(20)));
        assert_eq!(fee_estimate_lifetime_of(&node, "BLOCK"), Ok(FeeLifetime::Adaptive));
        assert_eq!(fee_estimate_lifetime_of(&global, "BLOCK"), Ok(FeeLifetime::Fixed(10)));
        assert_eq!(fee_estimate_lifetime_of(&global, "PER"), Ok(FeeLifetime::Fixed(20)));
        assert_eq!(fee_estimate_lifetime_of(&zero, "BLOCK"), Ok(FeeLifetime::Fixed(0)));

        // A fixed coin keeps its lifetime however often its readings repeat.
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let cache = Arc::new(FeeEstimateCache::with_clock(10, move || clock.load(Ordering::SeqCst)));
        fetched(&cache, &now, ESTIMATE, "PER", 0, 7);
        for _ in 0..4 {
            assert!(!fetched(&cache, &now, ESTIMATE, "PER", 9, 7));
            assert!(
                fetched(&cache, &now, ESTIMATE, "PER", 1, 7),
                "10 s every time, never 20, 40, ..."
            );
        }
        // A global 0 disables the coin: it never reuses and never stores.
        let (cache, now) = {
            let now = Arc::new(AtomicU64::new(1_000_000));
            let clock = now.clone();
            (
                Arc::new(FeeEstimateCache::with_clock(0, move || clock.load(Ordering::SeqCst))),
                now,
            )
        };
        assert!(fetched(&cache, &now, ESTIMATE, "BLOCK", 0, 7));
        assert!(fetched(&cache, &now, ESTIMATE, "BLOCK", 1, 7));
    }

    /// A coin with a fixed lifetime that fetches keeps the entry truthful for the coins on the adaptive default:
    /// a changed value resets their lifetime at once, and an unchanged one counts as evidence only for the time
    /// that really passed.
    #[test]
    fn a_fixed_coins_fetch_updates_the_adaptive_state_soundly() {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let cache = Arc::new(FeeEstimateCache::with_lifetimes(
            |ticker| {
                if ticker == "FIXED" {
                    FeeLifetime::Fixed(2)
                } else {
                    FeeLifetime::Adaptive
                }
            },
            |_| 15,
            move || clock.load(Ordering::SeqCst),
        ));
        // Both coins read the gas price of one chain, so they share its entry.
        let reads = AtomicUsize::new(0);
        let fetched = |asking: &str, wait_secs: u64, value: u64| {
            now.fetch_add(wait_secs * 1000, Ordering::SeqCst);
            let before = reads.load(Ordering::SeqCst);
            block_on(run_in_scope(Some(cache.clone()), async {
                memoize_shared(FeeReadKind::EvmGasPrice, "CHAIN", asking, counted(&reads, value))
                    .await
                    .unwrap()
            }));
            reads.load(Ordering::SeqCst) > before
        };
        assert!(fetched("ADAPT", 0, 7));
        assert!(fetched("ADAPT", 15, 7)); // 30 s
                                          // A fixed coin fetches an equal value after 2 s: that is not 30 s of evidence, so no doubling.
        assert!(fetched("FIXED", 2, 7));
        assert!(
            !fetched("ADAPT", 29, 7),
            "still 30 s, counted from the fixed coin's reading"
        );
        assert!(fetched("ADAPT", 1, 7), "and 60 s now: this fetch waited the 30 s out");
        // A fixed coin that sees a different value resets the adaptive lifetime at once.
        assert!(fetched("FIXED", 2, 9));
        assert!(!fetched("ADAPT", 14, 9));
        assert!(fetched("ADAPT", 1, 9), "15 s again");
    }

    /// R38.6.6k item 3 with R57.2.7: the adaptive state of the shared gas price belongs to the chain's entry.
    #[test]
    fn the_adaptive_state_of_the_gas_price_is_per_chain() {
        let gas = FeeReadKind::EvmGasPrice;
        let (cache, now) = adaptive_cache(15);
        let reads = AtomicUsize::new(0);
        let ask = |key: &str, asking: &str, value: u64, wait_secs: u64| {
            now.fetch_add(wait_secs * 1000, Ordering::SeqCst);
            block_on(run_in_scope(Some(cache.clone()), async {
                memoize_shared(gas, key, asking, counted(&reads, value)).await.unwrap()
            }))
        };
        ask("ETH", "ETH", 5, 0);
        // Tokens asking after the platform's lifetime fetch one reading for the chain; it doubles the chain's
        // lifetime whoever asks.
        ask("ETH", "JST", 5, 15);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        ask("ETH", "USDT", 6, 29);
        ask("ETH", "ETH", 6, 0);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "30 s for the chain: young at 29 s for every coin"
        );
        ask("ETH", "USDT", 5, 1);
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        // Another chain has its own state: still at its start value.
        ask("BNB", "BNB", 9, 0);
        ask("BNB", "BNB", 9, 15);
        ask("BNB", "BNB", 9, 29);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            5,
            "BNB: 15 s then 30 s, independent of ETH"
        );
    }

    /// The node's own scope: the TTL comes from `MM2.json`, and the cache is the node's.
    mod node_scope {
        use super::*;
        use crate::{CoinsContext, MmCoinEnum, TestCoin};
        use mm2_core::mm_ctx::MmCtxBuilder;
        use serde_json::json;

        fn node(conf: Json) -> MmArc { MmCtxBuilder::new().with_conf(conf).into_mm_arc() }

        fn request_of(ctx: &MmArc, reads: &AtomicUsize, value: u64) -> u64 {
            block_on(with_request_fee_scope(ctx, async {
                memoize(ESTIMATE, "DYN", counted(reads, value)).await.unwrap()
            }))
        }

        #[test]
        fn the_default_cache_serves_the_next_request_and_zero_disables_it() {
            let reads = AtomicUsize::new(0);
            let ctx = node(json!({}));
            assert_eq!(request_of(&ctx, &reads, 1), 1);
            assert_eq!(request_of(&ctx, &reads, 2), 1);
            assert_eq!(reads.load(Ordering::SeqCst), 1);

            let reads = AtomicUsize::new(0);
            let ctx = node(json!({"fee_estimate_cache_secs": 0}));
            assert_eq!(request_of(&ctx, &reads, 1), 1);
            assert_eq!(request_of(&ctx, &reads, 2), 2);
            assert_eq!(reads.load(Ordering::SeqCst), 2);
        }

        #[test]
        fn the_cache_is_per_node() {
            let reads = AtomicUsize::new(0);
            let (a, b) = (node(json!({})), node(json!({})));
            request_of(&a, &reads, 1);
            assert_eq!(request_of(&b, &reads, 2), 2);
        }

        #[test]
        fn registering_or_disabling_a_coin_drops_what_the_previous_instance_cached() {
            let reads = AtomicUsize::new(0);
            let ctx = node(json!({}));
            let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
            assert_eq!(request_of(&ctx, &reads, 1), 1);

            // Disabled: the next request reads afresh.
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new("DYN")))).unwrap();
            assert_eq!(request_of(&ctx, &reads, 2), 2, "dropped when the coin is registered");
            assert_eq!(request_of(&ctx, &reads, 3), 2);
            block_on(crate::disable_coin(&ctx, "DYN")).unwrap();
            assert_eq!(request_of(&ctx, &reads, 4), 4, "dropped when the coin is disabled");
        }

        #[test]
        fn a_node_with_an_invalid_cache_setting_has_no_coins_context() {
            let ctx = node(json!({"fee_estimate_cache_secs": 31}));
            match CoinsContext::from_ctx(&ctx) {
                Err(e) => assert!(e.contains("fee_estimate_cache_secs"), "{e}"),
                Ok(_) => panic!("the invalid setting must be refused"),
            }
        }
    }
}
