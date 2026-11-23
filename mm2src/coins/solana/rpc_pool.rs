//! # Purpose
//! The node pool of a Solana coin: it picks the node an operation runs on (CRD ch.40 R40.1.5, the maintainer's
//! decision of 2026-11-19). It serves every Solana coin, whichever activation surface built it, and every operation of
//! the coin. A token uses its platform's pool.
//!
//! # Design
//! - **Order.** The coin's nodes are put into one order when the pool is built: the listed order under
//!   [`SolanaNodeOrder::Listed`], a uniformly random permutation drawn once under [`SolanaNodeOrder::Random`] (the
//!   default; the MM2.json string `solana_node_order`). The order lives in the running coin only.
//! - **Health.** Before a node is used, `getHealth` is asked of it when it has not been checked in the last
//!   [`HEALTH_VALID_MS`], or when its set-aside has just ended. A failed check (error answer or transport failure)
//!   sets the node aside like a transport failure. A passed check is remembered for [`HEALTH_VALID_MS`].
//! - **Set-aside.** A node is set aside after a transport failure or a failed health check, for
//!   [`SET_ASIDE_FIRST_MS`], doubling on each consecutive failure up to [`SET_ASIDE_MAX_MS`]; a successful operation
//!   resets it. Nodes are tried in the pool's order and set-aside nodes are skipped, so a recovered higher-ranked
//!   node is used again as soon as its set-aside has ended and its health check has passed. When every node is set
//!   aside, each is tried once in order before the operation fails.
//! - **One operation, one node.** [`SolanaRpcPool::run`] runs an operation, which may send several requests, on one
//!   node. A transport failure inside it sets the node aside and reruns the whole operation on the next node, so an
//!   operation given to `run` must only read or only resend what is already built (CRD ch.40 R40.1.5): a signed
//!   transaction is never rebuilt or re-signed after it has been sent.
//! - A server-side `Rpc` or `Decode` error answers for the request, not for the node: it is returned as it is.
//!
//! The clock and the random source are injectable, so the behavior is tested without sleeping.

use crate::solana::rpc_client::{BlockSummary, ConfirmedTransaction, FullAccount, RpcError, RpcErrorKind,
                                SignatureInfo, SolanaRpcClient, TokenAccountsFilter};
use common::log::warn;
use common::now_ms;
use mm2_core::mm_ctx::MmArc;
use rand::seq::SliceRandom;
use serde_json::Value as Json;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use std::sync::{Arc, Mutex};

/// How long a passed health check is remembered.
pub const HEALTH_VALID_MS: u64 = 30_000;
/// The first set-aside time of a node.
pub const SET_ASIDE_FIRST_MS: u64 = 20_000;
/// The longest set-aside time of a node (12 hours).
pub const SET_ASIDE_MAX_MS: u64 = 12 * 60 * 60 * 1000;

/// The order in which a coin's nodes are ranked (the MM2.json string `solana_node_order`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolanaNodeOrder {
    /// The nodes in a uniformly random order drawn once when the coin is built. The default.
    Random,
    /// The listed order is kept.
    Listed,
}

/// The order `conf` asks for, and the warning to log when its `solana_node_order` is not recognized. An absent key
/// means `random`. Any other value, `mixed` and `measured` included, means `random`.
pub fn parse_node_order(conf: &Json) -> (SolanaNodeOrder, Option<String>) {
    match &conf["solana_node_order"] {
        Json::Null => (SolanaNodeOrder::Random, None),
        Json::String(value) if value == "random" => (SolanaNodeOrder::Random, None),
        Json::String(value) if value == "listed" => (SolanaNodeOrder::Listed, None),
        other => {
            let shown = match other {
                Json::String(value) => value.clone(),
                other => other.to_string(),
            };
            (
                SolanaNodeOrder::Random,
                Some(format!(
                    "solana_node_order \"{shown}\" is not recognized; using \"random\""
                )),
            )
        },
    }
}

/// The `MM2.json` string `solana_node_order`: `random` (the default) or `listed`. Any other value is taken as
/// `random` and one warning is logged.
///
/// **Compatibility:** for the node order of the reference, set `solana_node_order` to `listed`.
pub fn solana_node_order(ctx: &MmArc) -> SolanaNodeOrder {
    let (order, warning) = parse_node_order(&ctx.conf);
    if let Some(warning) = warning {
        warn!("{warning}");
    }
    order
}

/// The time source of the pool; tests inject their own.
pub trait PoolClock: Send + Sync + std::fmt::Debug {
    fn now_ms(&self) -> u64;
}

#[derive(Debug)]
struct SystemClock;

impl PoolClock for SystemClock {
    fn now_ms(&self) -> u64 { now_ms() }
}

/// What the pool remembers about one node.
#[derive(Debug, Default)]
struct NodeState {
    /// A passed health check is valid until this time; `0`: the node must be checked before it is used.
    health_valid_until_ms: u64,
    /// The node is set aside until this time.
    set_aside_until_ms: u64,
    /// Consecutive failures; the next set-aside time follows from it.
    failures: u32,
}

/// The set-aside time after `failures` earlier consecutive failures: [`SET_ASIDE_FIRST_MS`], doubling, at most
/// [`SET_ASIDE_MAX_MS`].
fn set_aside_ms(failures: u32) -> u64 {
    SET_ASIDE_FIRST_MS
        .checked_shl(failures.min(40))
        .map_or(SET_ASIDE_MAX_MS, |ms| ms.min(SET_ASIDE_MAX_MS))
}

/// Failover-aware Solana RPC pool. Cloneable; cloning shares the clients and the node memory.
#[derive(Clone, Debug)]
pub struct SolanaRpcPool {
    inner: Arc<PoolInner>,
}

#[derive(Debug)]
struct PoolInner {
    /// In the pool's order.
    clients: Vec<SolanaRpcClient>,
    states: Vec<Mutex<NodeState>>,
    clock: Arc<dyn PoolClock>,
}

impl SolanaRpcPool {
    /// Build a pool from raw endpoint URLs with the default commitment level, in the listed order. Returns `None` if
    /// `urls` is empty (the activation layer is expected to surface a coin-init error in that case).
    pub fn from_urls<I, S>(urls: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_commitment(urls, CommitmentConfig::default())
    }

    /// Build a pool from raw endpoint URLs with a caller-supplied commitment, in the listed order.
    pub fn with_commitment<I, S>(urls: I, commitment: CommitmentConfig) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_parts(
            urls.into_iter().map(Into::into).collect(),
            Some(commitment),
            SolanaNodeOrder::Listed,
            Arc::new(SystemClock),
            &mut common::small_rng(),
        )
    }

    /// Build a pool from raw endpoint URLs whose clients send no commitment level, so the node's default applies
    /// (the compatibility activation surface, CRD ch.40 R40.6B.4), in the listed order. Returns `None` if `urls` is
    /// empty.
    pub fn without_commitment<I, S>(urls: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_parts(
            urls.into_iter().map(Into::into).collect(),
            None,
            SolanaNodeOrder::Listed,
            Arc::new(SystemClock),
            &mut common::small_rng(),
        )
    }

    /// The pool of an activated coin: `commitment` (`None`: no commitment level is sent) and the node `order`.
    pub fn with_order(urls: Vec<String>, commitment: Option<CommitmentConfig>, order: SolanaNodeOrder) -> Option<Self> {
        Self::with_parts(urls, commitment, order, Arc::new(SystemClock), &mut common::small_rng())
    }

    /// Build a pool from every part, the clock and the random source included. Returns `None` if `urls` is empty.
    pub fn with_parts<R: rand::Rng>(
        urls: Vec<String>,
        commitment: Option<CommitmentConfig>,
        order: SolanaNodeOrder,
        clock: Arc<dyn PoolClock>,
        rng: &mut R,
    ) -> Option<Self> {
        let clients: Vec<SolanaRpcClient> = urls
            .into_iter()
            .map(|url| match commitment {
                Some(commitment) => SolanaRpcClient::with_commitment(url, commitment),
                None => SolanaRpcClient::without_commitment(url),
            })
            .collect();
        Self::from_clients_ordered(clients, order, clock, rng)
    }

    /// Build a pool from already-constructed clients, in the listed order.
    pub fn from_clients(clients: Vec<SolanaRpcClient>) -> Option<Self> {
        Self::from_clients_ordered(
            clients,
            SolanaNodeOrder::Listed,
            Arc::new(SystemClock),
            &mut common::small_rng(),
        )
    }

    fn from_clients_ordered<R: rand::Rng>(
        mut clients: Vec<SolanaRpcClient>,
        order: SolanaNodeOrder,
        clock: Arc<dyn PoolClock>,
        rng: &mut R,
    ) -> Option<Self> {
        if clients.is_empty() {
            return None;
        }
        if order == SolanaNodeOrder::Random {
            clients.shuffle(rng);
        }
        let states = clients.iter().map(|_| Mutex::new(NodeState::default())).collect();
        Some(Self {
            inner: Arc::new(PoolInner { clients, states, clock }),
        })
    }

    /// Number of endpoints in the pool.
    pub fn len(&self) -> usize { self.inner.clients.len() }

    /// Snapshot of endpoint URLs in the pool's order.
    pub fn urls(&self) -> Vec<&str> { self.inner.clients.iter().map(|c| c.url()).collect() }

    fn state(&self, idx: usize) -> std::sync::MutexGuard<'_, NodeState> {
        self.inner.states[idx].lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_set_aside(&self, idx: usize) -> bool { self.state(idx).set_aside_until_ms > self.inner.clock.now_ms() }

    fn needs_health_check(&self, idx: usize) -> bool {
        self.state(idx).health_valid_until_ms <= self.inner.clock.now_ms()
    }

    fn health_passed(&self, idx: usize) {
        let until = self.inner.clock.now_ms().saturating_add(HEALTH_VALID_MS);
        self.state(idx).health_valid_until_ms = until;
    }

    /// Sets the node aside after a failure; the next health check is due when the set-aside ends. A failure seen while
    /// the node is already set aside (several operations running at once met the same fault, or the node was tried
    /// once because every node was set aside) keeps the current set-aside and does not count: only a failure after
    /// the set-aside has ended, or on a node that was not set aside, counts and doubles the time.
    fn set_aside(&self, idx: usize) {
        let now = self.inner.clock.now_ms();
        let mut state = self.state(idx);
        if state.set_aside_until_ms > now {
            state.health_valid_until_ms = 0;
            return;
        }
        state.set_aside_until_ms = now.saturating_add(set_aside_ms(state.failures));
        state.failures = state.failures.saturating_add(1);
        state.health_valid_until_ms = 0;
    }

    /// Tests: reports a failure of the node at `idx` as an operation would.
    #[cfg(test)]
    pub(crate) fn report_failure_for_test(&self, idx: usize) { self.set_aside(idx); }

    /// Tests: how long the node at `idx` stays set aside from now, in milliseconds.
    #[cfg(test)]
    pub(crate) fn set_aside_remaining_ms_for_test(&self, idx: usize) -> u64 {
        let now = self.inner.clock.now_ms();
        self.state(idx).set_aside_until_ms.saturating_sub(now)
    }

    /// The node answered: the back-off starts over.
    fn answered(&self, idx: usize) {
        let mut state = self.state(idx);
        state.failures = 0;
        state.set_aside_until_ms = 0;
    }

    /// Runs `op` on one node. The nodes are tried in the pool's order, set-aside nodes skipped; when every node is
    /// set aside each is tried once. A node is health-checked first when its check is stale. A transport failure of
    /// the health check or of `op` sets the node aside and `op` runs again, from its start, on the next node; any
    /// other outcome is returned as it is. `op` may send several requests, all to the one node it is given.
    ///
    /// `op` is rerun after a transport failure, so it must only read, or resend an already built and signed
    /// transaction unchanged. When no node serves it, the error is the last failure.
    pub async fn run<T, F, Fut>(&self, op: F) -> Result<T, RpcError>
    where
        F: Fn(SolanaRpcClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, RpcError>>,
    {
        let mut candidates: Vec<usize> = (0..self.inner.clients.len())
            .filter(|idx| !self.is_set_aside(*idx))
            .collect();
        if candidates.is_empty() {
            candidates = (0..self.inner.clients.len()).collect();
        }
        let mut last_err: Option<RpcError> = None;
        for idx in candidates {
            let client = self.inner.clients[idx].clone();
            if self.needs_health_check(idx) {
                match client.get_health().await {
                    Ok(()) => self.health_passed(idx),
                    Err(e) => {
                        self.set_aside(idx);
                        last_err = Some(e);
                        continue;
                    },
                }
            }
            match op(client).await {
                Ok(value) => {
                    self.answered(idx);
                    return Ok(value);
                },
                Err(e) => match e.kind {
                    RpcErrorKind::Transport(_) => {
                        self.set_aside(idx);
                        last_err = Some(e);
                    },
                    RpcErrorKind::Decode(_) | RpcErrorKind::Rpc(_) => {
                        self.answered(idx);
                        return Err(e);
                    },
                },
            }
        }
        Err(last_err.unwrap_or_else(|| RpcError::transport("pool", "no endpoints configured")))
    }

    /// Whether `url` is the URL of one of the pool's nodes, as it was given.
    pub fn has_node(&self, url: &str) -> bool { self.inner.clients.iter().any(|c| c.url() == url) }

    /// The reads behind a refund step or a secret step (CRD ch.40 R40.6D.16, chapter 51 R100): `op` runs, from its
    /// start, on two distinct nodes of the pool, in the pool's order with set-aside nodes skipped; a node that does
    /// not answer is passed over for the next. The result is the two answers, which the caller compares.
    ///
    /// With `evidence_node` (the one node the activation named) `op` runs on that node alone and its answer is
    /// both members of the pair; when it does not answer, no other node takes its place.
    ///
    /// # Errors
    ///
    /// The text of the reason when fewer than two nodes answered (the read is undetermined).
    pub async fn read_for_evidence<T, F, Fut>(&self, evidence_node: Option<&str>, op: F) -> Result<(T, T), String>
    where
        T: Clone,
        F: Fn(SolanaRpcClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, RpcError>>,
    {
        if let Some(url) = evidence_node {
            let Some(client) = self.inner.clients.iter().find(|c| c.url() == url) else {
                return Err("the named evidence node is not one of the coin's nodes".to_owned());
            };
            return match op(client.clone()).await {
                Ok(answer) => Ok((answer.clone(), answer)),
                Err(_) => Err("the named evidence node did not answer".to_owned()),
            };
        }
        let mut candidates: Vec<usize> = (0..self.inner.clients.len())
            .filter(|idx| !self.is_set_aside(*idx))
            .collect();
        if candidates.is_empty() {
            candidates = (0..self.inner.clients.len()).collect();
        }
        let mut answers: Vec<T> = Vec::with_capacity(2);
        for idx in candidates {
            let client = self.inner.clients[idx].clone();
            if self.needs_health_check(idx) {
                match client.get_health().await {
                    Ok(()) => self.health_passed(idx),
                    Err(_) => {
                        self.set_aside(idx);
                        continue;
                    },
                }
            }
            match op(client).await {
                Ok(answer) => {
                    self.answered(idx);
                    answers.push(answer);
                    if answers.len() == 2 {
                        break;
                    }
                },
                Err(e) => {
                    if matches!(e.kind, RpcErrorKind::Transport(_)) {
                        self.set_aside(idx);
                    } else {
                        self.answered(idx);
                    }
                },
            }
        }
        match <[T; 2]>::try_from(answers) {
            Ok([first, second]) => Ok((first, second)),
            Err(_) => Err("fewer than two nodes answered".to_owned()),
        }
    }

    // One-request operations. An operation of several requests uses `run` itself.

    pub async fn get_health(&self) -> Result<(), RpcError> { self.run(|c| async move { c.get_health().await }).await }

    pub async fn get_balance(&self, address: &Pubkey) -> Result<u64, RpcError> {
        let address = *address;
        self.run(move |c| async move { c.get_balance(&address).await }).await
    }

    pub async fn get_block_height(&self) -> Result<u64, RpcError> {
        self.run(|c| async move { c.get_block_height().await }).await
    }

    pub async fn get_latest_blockhash(&self) -> Result<Hash, RpcError> {
        self.run(|c| async move { c.get_latest_blockhash().await }).await
    }

    pub async fn get_fee_for_message(&self, message: &Message) -> Result<u64, RpcError> {
        let msg = message.clone();
        self.run(move |c| {
            let msg = msg.clone();
            async move { c.get_fee_for_message(&msg).await }
        })
        .await
    }

    pub async fn get_minimum_balance_for_rent_exemption(&self, data_len: usize) -> Result<u64, RpcError> {
        self.run(move |c| async move { c.get_minimum_balance_for_rent_exemption(data_len).await })
            .await
    }

    /// Sends the signed transaction `tx`. A failure is retried on the next node with the same signed bytes; the
    /// transaction is never rebuilt or re-signed here.
    pub async fn send_transaction(&self, tx: &Transaction) -> Result<Signature, RpcError> {
        let tx = tx.clone();
        self.run(move |c| {
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
        self.run(move |c| {
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
        self.run(move |c| async move { c.get_token_account_balance(&account).await })
            .await
    }

    pub async fn get_account_exists(&self, address: &Pubkey) -> Result<bool, RpcError> {
        let address = *address;
        self.run(move |c| async move { c.get_account_exists(&address).await })
            .await
    }

    pub async fn get_account_owner(&self, address: &Pubkey) -> Result<Option<Pubkey>, RpcError> {
        let address = *address;
        self.run(move |c| async move { c.get_account_owner(&address).await })
            .await
    }

    pub async fn get_signatures_for_address(
        &self,
        address: &Pubkey,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SignatureInfo>, RpcError> {
        let address = *address;
        let before = before.map(str::to_owned);
        self.run(move |c| {
            let before = before.clone();
            async move { c.get_signatures_for_address(&address, before.as_deref(), limit).await }
        })
        .await
    }

    pub async fn get_transaction_base64(&self, signature: &str) -> Result<Option<ConfirmedTransaction>, RpcError> {
        let signature = signature.to_owned();
        self.run(move |c| {
            let signature = signature.clone();
            async move { c.get_transaction_base64(&signature).await }
        })
        .await
    }

    /// `getAccountInfo` of `address` at `finalized` from one node (the whole account).
    pub async fn get_account_info_finalized(&self, address: &Pubkey) -> Result<Option<FullAccount>, RpcError> {
        let address = *address;
        self.run(move |c| async move { c.get_account_info_finalized(&address).await })
            .await
    }

    pub async fn get_block_summary(&self, slot: u64) -> Result<Option<BlockSummary>, RpcError> {
        self.run(move |c| async move { c.get_block_summary(slot).await }).await
    }
}
