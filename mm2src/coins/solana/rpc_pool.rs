//! # Purpose
//! Thin failover layer that distributes JSON-RPC traffic across a list
//! of [`SolanaRpcClient`] endpoints with **lazy quarantine** on
//! transport failures.
//!
//! # Design
//! - The pool owns `Vec<SolanaRpcClient>` and a parallel
//!   `Vec<EndpointHealth>`. `EndpointHealth` stores a single
//!   `AtomicI64` — the unix-second timestamp until which the endpoint
//!   is considered unhealthy. A value `<= now` means "healthy".
//! - On every call the pool walks the endpoints in registration order
//!   and dispatches to the first healthy one. On
//!   [`RpcErrorKind::Transport`] (timeout, DNS, TCP, 5xx) the pool
//!   marks the endpoint unhealthy for [`QUARANTINE_TTL_SECS`] and
//!   retries the next endpoint. Server-side `Rpc` errors are returned
//!   to the caller unchanged — they reflect a request-level fault, not
//!   an endpoint fault.
//! - Health state is lock-free (`AtomicI64`); the hot path takes no
//!   mutexes, which keeps endpoint selection off any shared async lock.
//!
//! # Non-goals
//! - No background probing. Recovery is lazy: an endpoint becomes
//!   eligible again the moment its quarantine expires; the next call
//!   that lands on it does the actual probe via the underlying RPC.
//! - No latency-weighted or sticky-by-pubkey strategy. The pool API is
//!   small enough that a future strategy can replace the body of
//!   [`SolanaRpcPool::pick_healthy`] without touching call sites.

use crate::solana::rpc_client::{RpcError, RpcErrorKind, SolanaRpcClient, TokenAccountsFilter};
use common::now_ms;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Default quarantine window for an endpoint that returned a transport
/// error. Tuned to "long enough that we don't hammer a flaky endpoint,
/// short enough that a transient network blip does not park it for the
/// rest of the session". 30 s matches the GLEEC heartbeat cadence.
pub const QUARANTINE_TTL_SECS: i64 = 30;

/// Per-endpoint health record. `unhealthy_until <= now` means healthy.
#[derive(Debug, Default)]
struct EndpointHealth {
    unhealthy_until: AtomicI64,
}

impl EndpointHealth {
    fn is_healthy(&self) -> bool { self.unhealthy_until.load(Ordering::Relaxed) <= now_secs_i64() }

    fn quarantine(&self, ttl_secs: i64) {
        let until = now_secs_i64() + ttl_secs;
        self.unhealthy_until.store(until, Ordering::Relaxed);
    }

    fn clear(&self) { self.unhealthy_until.store(0, Ordering::Relaxed); }
}

/// Wall-clock seconds since the Unix epoch as `i64`. We use `i64` so
/// `quarantine(-1)` (test fixture) yields a value that is unambiguously
/// in the past on every platform.
fn now_secs_i64() -> i64 { (now_ms() / 1000) as i64 }

/// Failover-aware Solana RPC pool. Cloneable; cloning shares the
/// underlying clients and health state.
#[derive(Clone, Debug)]
pub struct SolanaRpcPool {
    inner: Arc<PoolInner>,
}

#[derive(Debug)]
struct PoolInner {
    clients: Vec<SolanaRpcClient>,
    health: Vec<EndpointHealth>,
    quarantine_ttl_secs: i64,
}

impl SolanaRpcPool {
    /// Build a pool from raw endpoint URLs with the default commitment
    /// level. Returns `None` if `urls` is empty (the activation layer
    /// is expected to surface a coin-init error in that case).
    pub fn from_urls<I, S>(urls: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_commitment(urls, CommitmentConfig::default())
    }

    /// Build a pool from raw endpoint URLs with a caller-supplied
    /// commitment.
    pub fn with_commitment<I, S>(urls: I, commitment: CommitmentConfig) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let clients: Vec<SolanaRpcClient> = urls
            .into_iter()
            .map(|u| SolanaRpcClient::with_commitment(u, commitment))
            .collect();
        if clients.is_empty() {
            return None;
        }
        let health = clients.iter().map(|_| EndpointHealth::default()).collect();
        Some(Self {
            inner: Arc::new(PoolInner {
                clients,
                health,
                quarantine_ttl_secs: QUARANTINE_TTL_SECS,
            }),
        })
    }

    /// Build a pool from already-constructed clients. Used by tests
    /// and by code paths that need per-endpoint commitment overrides.
    pub fn from_clients(clients: Vec<SolanaRpcClient>) -> Option<Self> {
        if clients.is_empty() {
            return None;
        }
        let health = clients.iter().map(|_| EndpointHealth::default()).collect();
        Some(Self {
            inner: Arc::new(PoolInner {
                clients,
                health,
                quarantine_ttl_secs: QUARANTINE_TTL_SECS,
            }),
        })
    }

    /// Number of endpoints in the pool.
    pub fn len(&self) -> usize { self.inner.clients.len() }

    /// Snapshot of endpoint URLs in registration order.
    pub fn urls(&self) -> Vec<&str> { self.inner.clients.iter().map(|c| c.url()).collect() }

    /// Returns `true` if **every** endpoint is currently quarantined.
    /// Useful for diagnostics; the call paths themselves do not branch
    /// on this — they just attempt the call and surface the last error
    /// if every endpoint failed.
    pub fn all_quarantined(&self) -> bool { self.inner.health.iter().all(|h| !h.is_healthy()) }

    /// Manually clear the quarantine on every endpoint (operator
    /// escape hatch; not used by the hot path).
    pub fn clear_quarantines(&self) { self.inner.health.iter().for_each(EndpointHealth::clear); }

    fn pick_healthy(&self) -> Vec<usize> {
        // Healthy endpoints first, then quarantined ones as a last
        // resort so a fully-quarantined pool still tries to make
        // progress (and updates health state when it does).
        let mut healthy = Vec::with_capacity(self.inner.clients.len());
        let mut stale = Vec::new();
        for (i, h) in self.inner.health.iter().enumerate() {
            if h.is_healthy() {
                healthy.push(i);
            } else {
                stale.push(i);
            }
        }
        healthy.extend(stale);
        healthy
    }

    /// Run an async RPC operation against the pool. The closure is
    /// invoked once per candidate endpoint; the first non-transport
    /// outcome (success **or** server `Rpc` error) is returned.
    /// Transport failures quarantine the endpoint and trigger a retry
    /// against the next candidate.
    async fn dispatch<T, F, Fut>(&self, op: F) -> Result<T, RpcError>
    where
        F: Fn(SolanaRpcClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, RpcError>>,
    {
        let order = self.pick_healthy();
        let mut last_err: Option<RpcError> = None;
        for idx in order {
            let client = self.inner.clients[idx].clone();
            match op(client).await {
                Ok(v) => {
                    self.inner.health[idx].clear();
                    return Ok(v);
                },
                Err(e) => match e.kind {
                    RpcErrorKind::Transport(_) => {
                        self.inner.health[idx].quarantine(self.inner.quarantine_ttl_secs);
                        last_err = Some(e);
                        continue;
                    },
                    RpcErrorKind::Decode(_) | RpcErrorKind::Rpc(_) => return Err(e),
                },
            }
        }
        Err(last_err.unwrap_or_else(|| RpcError::transport("pool", "no endpoints configured")))
    }

    // ──────────────────────────────────────────────────────────────
    // Same surface as SolanaRpcClient. Each method is a one-line
    // delegate; we intentionally do not macro-generate them so the
    // signatures are greppable and rust-analyzer "go to definition"
    // works without cross-macro indirection.
    // ──────────────────────────────────────────────────────────────

    pub async fn get_health(&self) -> Result<(), RpcError> {
        self.dispatch(|c| async move { c.get_health().await }).await
    }

    pub async fn get_balance(&self, address: &Pubkey) -> Result<u64, RpcError> {
        let address = *address;
        self.dispatch(move |c| async move { c.get_balance(&address).await })
            .await
    }

    pub async fn get_block_height(&self) -> Result<u64, RpcError> {
        self.dispatch(|c| async move { c.get_block_height().await }).await
    }

    pub async fn get_latest_blockhash(&self) -> Result<Hash, RpcError> {
        self.dispatch(|c| async move { c.get_latest_blockhash().await }).await
    }

    pub async fn get_fee_for_message(&self, message: &Message) -> Result<u64, RpcError> {
        let msg = message.clone();
        self.dispatch(move |c| {
            let msg = msg.clone();
            async move { c.get_fee_for_message(&msg).await }
        })
        .await
    }

    pub async fn get_minimum_balance_for_rent_exemption(&self, data_len: usize) -> Result<u64, RpcError> {
        self.dispatch(move |c| async move { c.get_minimum_balance_for_rent_exemption(data_len).await })
            .await
    }

    pub async fn send_transaction(&self, tx: &Transaction) -> Result<Signature, RpcError> {
        let tx = tx.clone();
        self.dispatch(move |c| {
            let tx = tx.clone();
            async move { c.send_transaction(&tx).await }
        })
        .await
    }

    pub async fn get_token_accounts_by_owner(
        &self,
        owner: &Pubkey,
        filter: TokenAccountsFilter,
    ) -> Result<Vec<crate::solana::rpc_client::KeyedTokenAccount>, RpcError> {
        let owner = *owner;
        self.dispatch(move |c| {
            let filter = filter.clone();
            async move { c.get_token_accounts_by_owner(&owner, filter).await }
        })
        .await
    }

    pub async fn get_token_account_balance(
        &self,
        account: &Pubkey,
    ) -> Result<crate::solana::rpc_client::TokenAmount, RpcError> {
        let account = *account;
        self.dispatch(move |c| async move { c.get_token_account_balance(&account).await })
            .await
    }

    pub async fn get_account_exists(&self, address: &Pubkey) -> Result<bool, RpcError> {
        let address = *address;
        self.dispatch(move |c| async move { c.get_account_exists(&address).await })
            .await
    }

    pub async fn get_account_owner(&self, address: &Pubkey) -> Result<Option<Pubkey>, RpcError> {
        let address = *address;
        self.dispatch(move |c| async move { c.get_account_owner(&address).await })
            .await
    }
}

// ────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_reject_empty_endpoint_list() {
        let none: Option<SolanaRpcPool> = SolanaRpcPool::from_urls(Vec::<String>::new());
        assert!(none.is_none());
    }

    #[test]
    fn should_preserve_endpoint_order() {
        let pool = SolanaRpcPool::from_urls(vec!["http://a", "http://b", "http://c"]).unwrap();
        assert_eq!(pool.urls(), vec!["http://a", "http://b", "http://c"]);
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn should_mark_endpoint_healthy_initially() {
        let pool = SolanaRpcPool::from_urls(vec!["http://a"]).unwrap();
        assert!(!pool.all_quarantined());
    }

    #[test]
    fn should_quarantine_endpoint_for_ttl_then_recover() {
        let h = EndpointHealth::default();
        assert!(h.is_healthy());
        h.quarantine(QUARANTINE_TTL_SECS);
        assert!(!h.is_healthy());
        h.clear();
        assert!(h.is_healthy());
    }

    #[test]
    fn should_treat_negative_ttl_as_immediate_recovery() {
        let h = EndpointHealth::default();
        h.quarantine(-1);
        assert!(h.is_healthy());
    }

    #[test]
    fn should_pick_healthy_endpoints_before_quarantined_ones() {
        let pool = SolanaRpcPool::from_urls(vec!["http://a", "http://b", "http://c"]).unwrap();
        // Quarantine the first endpoint and expect it to be tried last.
        pool.inner.health[0].quarantine(QUARANTINE_TTL_SECS);
        let order = pool.pick_healthy();
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn should_report_all_quarantined_when_every_endpoint_is_parked() {
        let pool = SolanaRpcPool::from_urls(vec!["http://a", "http://b"]).unwrap();
        pool.inner.health.iter().for_each(|h| h.quarantine(QUARANTINE_TTL_SECS));
        assert!(pool.all_quarantined());
    }

    #[test]
    fn should_clear_quarantines_on_demand() {
        let pool = SolanaRpcPool::from_urls(vec!["http://a", "http://b"]).unwrap();
        pool.inner.health.iter().for_each(|h| h.quarantine(QUARANTINE_TTL_SECS));
        pool.clear_quarantines();
        assert!(!pool.all_quarantined());
    }
}
