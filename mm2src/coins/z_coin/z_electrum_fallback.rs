//! Electrum as the fallback for three swap-path questions of a Light-mode shielded coin
//! (CRD ch.39 R39.11.25, R39.11.1, R39.11.5).
//!
//! A Light-mode coin is served by lightwalletd alone, with one exception: when no admitted lightwalletd
//! endpoint can answer already-sent, the one-shot spend search or a spend-watch poll, the servers of the
//! activation's `electrum_servers` are asked. This module is that exception and nothing else of the coin
//! comes through it.
//!
//! * **Activation never connects.** The activation hands its server list to [`ElectrumFallback`], which
//!   only remembers it. The Electrum client is the shared one (chapter 38's pool, built by the shared
//!   builder) and is built on the first question that reaches it; an empty or absent list means there is
//!   no fallback and nothing is ever built.
//! * **A server is vetted before it answers** (R39.11.25(a), (b)): its block header at the fresh tip
//!   minus [`IDENTITY_DEPTH`] must hash (double SHA-256 of the full header) to what lightwalletd reports
//!   for that height, and its tip must not lag the fresh tip by more than [`MAX_TIP_LAG`] blocks. The
//!   identity result is cached per server for the admission re-evaluation interval; the lag is read for
//!   every question.
//! * **Reads** are the script history and unspent listing of the HTLC's P2SH script and the transactions
//!   the history names, decoded with the Zcash reader, never with the shared UTXO parser (R39.11.25(c)).
//! * **Failures are never negative answers** (R39.11.25(d)): a failed connection, read or decode, a vetting
//!   failure and an exhausted time budget each leave the question undetermined, and the error carries the
//!   named condition of every server asked.
//!
//! The reads sit behind [`ElectrumReads`] so the three questions can be driven without a network.

use super::z_lightwalletd::{p2sh_hash_of, txid_internal_to_display, DetectionLog, LightwalletdBackend, OutputRef,
                            ADMISSION_REEVALUATION_INTERVAL};
use super::z_tx_read::{read_tx, txid_display};
use crate::coin_conf;
use crate::node_url::{electrum_server_name, UNNAMED_SERVER};
use crate::utxo::rpc_clients::electrum_pool::round_budget;
use crate::utxo::rpc_clients::{electrum_script_hash, ElectrumClient, ElectrumPoolTimings, ElectrumRpcRequest};
use crate::utxo::utxo_builder::build_electrum_client;
use crate::utxo::ElectrumBuilderArgs;
use async_trait::async_trait;
use futures::compat::Future01CompatExt;
use futures::lock::Mutex as AsyncMutex;
use kdf_crypto::dhash256;
use keys::Address;
use mm2_core::mm_ctx::{MmArc, MmWeak};
use parking_lot::Mutex;
use rpc::v1::types::H256 as H256Json;
use script::Builder;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::Instant as TokioInstant;
use zcash_primitives::transaction::Transaction as ZTransaction;

/// How far below the fresh tip the block is whose hash proves a server follows this coin's chain
/// (R39.11.25(a)).
pub(crate) const IDENTITY_DEPTH: u64 = 10;

/// How far a server's tip may lag the fresh tip and still answer (R39.11.25(b)).
pub(crate) const MAX_TIP_LAG: u64 = 10;

/// One entry of a script's history, as an Electrum server lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryItem {
    /// The transaction identifier in display order, as the protocol carries it.
    pub(crate) txid: H256Json,
    /// The mined height; zero or negative for a transaction not yet mined.
    pub(crate) height: i64,
}

/// One entry of a script's unspent listing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnspentItem {
    /// The transaction identifier in display order.
    pub(crate) txid: H256Json,
    pub(crate) vout: u32,
}

/// The reads the fallback makes of an Electrum server, each addressed to one named server (CRD ch.38
/// R38.6.4g). A trait so the three questions can be scripted without a network; the production
/// implementation is [`ElectrumClientReads`], over the shared Electrum client. Every error is text a
/// condition can be built from.
#[async_trait]
pub(crate) trait ElectrumReads: Send + Sync + fmt::Debug + 'static {
    /// The servers to ask, in the order of the client's pool (R38.6.4b). Builds the client when this is
    /// the first use. An empty list means there is no fallback; an error is the client failing to build,
    /// and names each listed server with its failure.
    async fn servers(&self) -> Result<Vec<String>, String>;
    /// `blockchain.headers.subscribe`: the server's tip height.
    async fn tip_height(&self, server: &str) -> Result<u64, String>;
    /// `blockchain.block.header`: the raw, full header bytes at `height`.
    async fn block_header(&self, server: &str, height: u64) -> Result<Vec<u8>, String>;
    /// `blockchain.scripthash.get_history`; `script_hash` is the standard Electrum script hash in hex.
    async fn history(&self, server: &str, script_hash: &str) -> Result<Vec<HistoryItem>, String>;
    /// `blockchain.scripthash.listunspent`.
    async fn unspent(&self, server: &str, script_hash: &str) -> Result<Vec<UnspentItem>, String>;
    /// `blockchain.transaction.get`: the raw transaction bytes.
    async fn transaction(&self, server: &str, txid: &H256Json) -> Result<Vec<u8>, String>;
    /// The only form in which a log line, error text or status message names `server`: `host:port`, or a
    /// placeholder when the address cannot be reduced to one, never the configured string (CRD ch.38
    /// R38.6.9a, ch.35 R35.1.17).
    fn server_name(&self, server: &str) -> String {
        electrum_server_name(server).unwrap_or_else(|| UNNAMED_SERVER.to_owned())
    }
}

/// The Electrum reads over the shared client. The client is built, with chapter 38's policies, when the
/// first read needs it, and not before: constructing this connects nothing.
pub(crate) struct ElectrumClientReads {
    ctx: MmWeak,
    ticker: String,
    servers: Vec<ElectrumRpcRequest>,
    client: AsyncMutex<Option<ElectrumClient>>,
}

impl fmt::Debug for ElectrumClientReads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElectrumClientReads")
            .field("ticker", &self.ticker)
            .field("servers", &self.servers.len())
            .finish()
    }
}

impl ElectrumClientReads {
    pub(crate) fn new(ctx: MmWeak, ticker: &str, servers: Vec<ElectrumRpcRequest>) -> Self {
        ElectrumClientReads {
            ctx,
            ticker: ticker.to_owned(),
            servers,
            client: AsyncMutex::new(None),
        }
    }

    /// Whether the client has been built, that is, whether anything has ever connected.
    #[cfg(test)]
    pub(crate) fn is_built(&self) -> bool { self.client.try_lock().map_or(true, |client| client.is_some()) }

    /// The shared client, built on first use with the shared builder. A failed build is not remembered:
    /// the next question tries again.
    async fn client(&self) -> Result<ElectrumClient, String> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }
        let ctx = MmArc::from_weak(&self.ctx).ok_or_else(|| "the node is shutting down".to_owned())?;
        // The connection-count members of the activation are accepted and ignored (R39.11.5).
        let client = build_electrum_client(
            &ctx,
            &self.ticker,
            &coin_conf(&ctx, &self.ticker),
            ElectrumBuilderArgs::default(),
            self.servers.clone(),
            None,
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
        *guard = Some(client.clone());
        Ok(client)
    }
}

#[async_trait]
impl ElectrumReads for ElectrumClientReads {
    async fn servers(&self) -> Result<Vec<String>, String> {
        if self.servers.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.client().await?.server_addrs())
    }

    async fn tip_height(&self, server: &str) -> Result<u64, String> {
        let header = self
            .client()
            .await?
            .blockchain_headers_subscribe_from(server)
            .compat()
            .await
            .map_err(|e| e.to_string())?;
        Ok(header.block_height())
    }

    async fn block_header(&self, server: &str, height: u64) -> Result<Vec<u8>, String> {
        self.client()
            .await?
            .blockchain_block_header_from(server, height)
            .compat()
            .await
            .map(|bytes| bytes.0)
            .map_err(|e| e.to_string())
    }

    async fn history(&self, server: &str, script_hash: &str) -> Result<Vec<HistoryItem>, String> {
        self.client()
            .await?
            .scripthash_get_history_from(server, script_hash)
            .compat()
            .await
            .map(|items| {
                items
                    .into_iter()
                    .map(|item| HistoryItem {
                        txid: item.tx_hash,
                        height: item.height,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    async fn unspent(&self, server: &str, script_hash: &str) -> Result<Vec<UnspentItem>, String> {
        self.client()
            .await?
            .scripthash_list_unspent_from(server, script_hash)
            .compat()
            .await
            .map(|items| {
                items
                    .into_iter()
                    .map(|item| UnspentItem {
                        txid: item.tx_hash,
                        vout: item.tx_pos,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    async fn transaction(&self, server: &str, txid: &H256Json) -> Result<Vec<u8>, String> {
        self.client()
            .await?
            .transaction_get_from(server, txid)
            .compat()
            .await
            .map(|bytes| bytes.0)
            .map_err(|e| e.to_string())
    }
}

/// Why the fallback gave no answer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FallbackError {
    /// The activation listed no Electrum server: there is no fallback, and nothing was asked.
    NotConfigured,
    /// Every server that could be asked failed to answer; one named condition per server, plus the
    /// conditions that stopped the question before any server could be asked.
    Undetermined(Vec<String>),
}

impl FallbackError {
    /// The error a question returns when lightwalletd gave no answer (`lightwalletd`, its own text) and
    /// the fallback did not either: both sources' conditions, named (R39.11.25(d), R39.11.20(c)).
    pub(crate) fn appended_to(&self, lightwalletd: String) -> String {
        match self {
            FallbackError::NotConfigured => lightwalletd,
            FallbackError::Undetermined(conditions) => format!(
                "{}; the Electrum fallback of the activation's electrum_servers could not answer either: {}",
                lightwalletd,
                conditions.join("; ")
            ),
        }
    }

    /// Whether the fallback has conditions to report on the sync-error channel (R39.8.0au).
    pub(crate) fn has_conditions(&self) -> bool { matches!(self, FallbackError::Undetermined(c) if !c.is_empty()) }
}

/// The already-sent answer of a server that was vetted and answered.
#[derive(Debug)]
pub(crate) enum SentAnswer {
    /// The payment transaction: it has an output of exactly the HTLC script.
    Sent(ZTransaction),
    /// The server's script history is complete and empty.
    NotSent,
}

/// The spend answer of a server that was vetted and answered.
#[derive(Debug)]
pub(crate) enum SpendAnswer {
    /// The transaction that spends the HTLC output, and the input of it that does.
    Spent { tx: ZTransaction, input_index: usize },
    /// The server's unspent listing for the script lists the HTLC output, and no history transaction
    /// spends it.
    NotSpent,
}

/// What is remembered of one server between questions.
#[derive(Default)]
struct ServerRecord {
    /// When the chain-identity check last passed; `None` after it failed or before it ran.
    identity_passed_at: Option<Instant>,
    /// The outcome of the last vetting, to log a first use and a change of outcome (R39.11.25(e)).
    last_vetted_ok: Option<bool>,
}

/// The Electrum fallback of one shielded coin.
pub(crate) struct ElectrumFallback {
    ticker: String,
    reads: Arc<dyn ElectrumReads>,
    timings: ElectrumPoolTimings,
    /// The overall budget of one question (CRD ch.38 R38.6.4j item 1).
    budget: Duration,
    servers: Mutex<HashMap<String, ServerRecord>>,
}

impl fmt::Debug for ElectrumFallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElectrumFallback")
            .field("ticker", &self.ticker)
            .field("reads", &self.reads)
            .finish()
    }
}

/// Lets the three questions keep the progress record of R39.5.5 meaningful while they run.
struct Progress<'a> {
    lightwalletd: &'a LightwalletdBackend,
    hash: [u8; 20],
}

impl Progress<'_> {
    fn set(&self, what: String) { self.lightwalletd.note_progress(self.hash, what); }
}

/// Runs `fut` unless `deadline` passes first; the budget running out is a named condition.
async fn bounded<T>(
    deadline: TokioInstant,
    what: &str,
    fut: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout_at(deadline, fut).await {
        Ok(result) => result,
        Err(_) => Err(format!("the time budget for this question was used up during {}", what)),
    }
}

/// The server walk of one question: the servers in the client's order, vetted one at a time as the
/// question reaches them, so a server that is never reached is never asked for anything.
struct Walk<'a> {
    fallback: &'a ElectrumFallback,
    lightwalletd: &'a LightwalletdBackend,
    servers: Vec<String>,
    next: usize,
    fresh_tip: u64,
    /// The question's overall deadline (R38.6.4j item 1).
    deadline: TokioInstant,
    /// What stopped each server so far.
    conditions: Vec<String>,
}

impl Walk<'_> {
    /// The next server that passes vetting, with the deadline of its turn, or `None` when the servers
    /// are used up or the budget is.
    async fn next_usable(&mut self) -> Option<(String, TokioInstant)> {
        while self.next < self.servers.len() {
            let now = TokioInstant::now();
            let remaining = self.deadline.saturating_duration_since(now);
            let left = self.servers.len() - self.next;
            if remaining.is_zero() {
                let unasked: Vec<String> = self.servers[self.next..]
                    .iter()
                    .map(|server| self.fallback.name(server))
                    .collect();
                self.conditions.push(format!(
                    "the time budget of {:?} for this question was used up before {} could be asked",
                    self.fallback.budget,
                    unasked.join(", ")
                ));
                self.next = self.servers.len();
                return None;
            }
            let server = self.servers[self.next].clone();
            self.next += 1;
            let timings = &self.fallback.timings;
            let turn = round_budget(remaining, left, timings.establishment + timings.request, timings).min(remaining);
            let turn_deadline = now + turn;
            match self
                .fallback
                .vet(self.lightwalletd, &server, self.fresh_tip, turn_deadline)
                .await
            {
                Ok(()) => return Some((server, turn_deadline)),
                Err(condition) => self
                    .conditions
                    .push(format!("{}: {}", self.fallback.name(&server), condition)),
            }
        }
        None
    }
}

impl ElectrumFallback {
    pub(crate) fn new(ticker: &str, reads: Arc<dyn ElectrumReads>) -> Self {
        let timings = ElectrumPoolTimings::default();
        ElectrumFallback {
            ticker: ticker.to_owned(),
            reads,
            budget: timings.call_budget,
            timings,
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// The fallback of an activation that listed `servers`. Nothing connects here (R39.11.5): the shared
    /// client is built by the first question that needs it.
    pub(crate) fn for_activation(ctx: MmWeak, ticker: &str, servers: Vec<ElectrumRpcRequest>) -> Self {
        ElectrumFallback::new(ticker, Arc::new(ElectrumClientReads::new(ctx, ticker, servers)))
    }

    #[cfg(test)]
    pub(crate) fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Records the outcome of a vetting and logs it: one `info` line at a server's first use, and a line
    /// when the outcome changes; an unchanged outcome is only a `debug` line (R39.11.25(e)).
    /// How a text names `server` (see [`ElectrumReads::server_name`]).
    fn name(&self, server: &str) -> String { self.reads.server_name(server) }

    fn announce(&self, server: &str, result: &Result<(), String>, identity: &str) {
        let ok = result.is_ok();
        let previous = {
            let mut servers = self.servers.lock();
            let record = servers.entry(server.to_owned()).or_default();
            record.last_vetted_ok.replace(ok)
        };
        let line = match result {
            Ok(()) => format!(
                "Electrum fallback of {}: server {} passed its chain-identity and tip check ({})",
                self.ticker,
                self.name(server),
                identity
            ),
            Err(reason) => format!(
                "Electrum fallback of {}: server {} is not used: {}",
                self.ticker,
                self.name(server),
                reason
            ),
        };
        match previous {
            None => common::log::info!("{}", line),
            Some(was_ok) if was_ok != ok && ok => common::log::info!("{}", line),
            Some(was_ok) if was_ok != ok => common::log::warn!("{}", line),
            Some(_) => common::log::debug!("{}", line),
        }
    }

    fn identity_is_fresh(&self, server: &str) -> bool {
        self.servers
            .lock()
            .get(server)
            .and_then(|record| record.identity_passed_at)
            .is_some_and(|at| at.elapsed() < ADMISSION_REEVALUATION_INTERVAL)
    }

    /// Makes the identity check of `server` look `by` older than it is.
    #[cfg(test)]
    pub(crate) fn age_identity(&self, server: &str, by: Duration) {
        if let Some(record) = self.servers.lock().get_mut(server) {
            record.identity_passed_at = record.identity_passed_at.and_then(|at| at.checked_sub(by));
        }
    }

    fn set_identity(&self, server: &str, passed_at: Option<Instant>) {
        self.servers
            .lock()
            .entry(server.to_owned())
            .or_default()
            .identity_passed_at = passed_at;
    }

    /// Vets `server` (R39.11.25(a), (b)): its tip may lag the fresh tip by at most [`MAX_TIP_LAG`] blocks,
    /// and the double SHA-256 of its full header at `fresh_tip - IDENTITY_DEPTH` must equal the hash
    /// lightwalletd reports for that height, byte for byte. The tip is read for every question; a
    /// passed identity check is kept for the admission re-evaluation interval. The `Err` is the named
    /// reason the server is not used.
    async fn vet(
        &self,
        lightwalletd: &LightwalletdBackend,
        server: &str,
        fresh_tip: u64,
        deadline: TokioInstant,
    ) -> Result<(), String> {
        let (result, identity) = self.vet_checks(lightwalletd, server, fresh_tip, deadline).await;
        self.announce(server, &result, &identity);
        result
    }

    async fn vet_checks(
        &self,
        lightwalletd: &LightwalletdBackend,
        server: &str,
        fresh_tip: u64,
        deadline: TokioInstant,
    ) -> (Result<(), String>, String) {
        let none = String::new();
        let server_tip = match bounded(deadline, "its tip read", self.reads.tip_height(server)).await {
            Ok(tip) => tip,
            Err(e) => return (Err(format!("its tip could not be read: {}", e)), none),
        };
        if server_tip.saturating_add(MAX_TIP_LAG) < fresh_tip {
            return (
                Err(format!(
                    "its tip {} is {} blocks below the fresh tip {}, more than the {} allowed",
                    server_tip,
                    fresh_tip - server_tip,
                    fresh_tip,
                    MAX_TIP_LAG
                )),
                none,
            );
        }
        if self.identity_is_fresh(server) {
            return (Ok(()), "identity check passed earlier".to_owned());
        }
        let height = fresh_tip.saturating_sub(IDENTITY_DEPTH);
        let header = match bounded(deadline, "its header read", self.reads.block_header(server, height)).await {
            Ok(header) if header.is_empty() => {
                self.set_identity(server, None);
                return (Err(format!("its header of block {} is empty", height)), none);
            },
            Ok(header) => header,
            Err(e) => {
                self.set_identity(server, None);
                return (
                    Err(format!("it could not serve the header of block {}: {}", height, e)),
                    none,
                );
            },
        };
        let server_hash = dhash256(&header);
        let expected = match bounded(deadline, "lightwalletd's block hash read", async {
            lightwalletd.block_hash(height).await.map_err(|e| e.to_string())
        })
        .await
        {
            Ok(expected) => expected,
            Err(e) => {
                return (
                    Err(format!(
                        "the chain identity could not be checked: lightwalletd's hash of block {} could not be \
                         read to compare with ({})",
                        height, e
                    )),
                    none,
                )
            },
        };
        let shown = |bytes: &[u8]| {
            let mut bytes = bytes.to_vec();
            bytes.reverse();
            hex::encode(bytes)
        };
        if server_hash.as_slice() != expected.as_slice() {
            self.set_identity(server, None);
            return (
                Err(format!(
                    "it is not on this coin's chain: the hash of its header of block {} is {} and lightwalletd \
                     reports {}",
                    height,
                    shown(server_hash.as_slice()),
                    shown(&expected)
                )),
                none,
            );
        }
        self.set_identity(server, Some(Instant::now()));
        (
            Ok(()),
            format!("block {} hash {} matches lightwalletd", height, shown(&expected)),
        )
    }

    /// Starts a question: the servers in the client's order and the fresh tip they are vetted against.
    async fn walk<'a>(&'a self, lightwalletd: &'a LightwalletdBackend) -> Result<Walk<'a>, FallbackError> {
        let deadline = TokioInstant::now() + self.budget;
        let servers = match bounded(deadline, "building the Electrum client", self.reads.servers()).await {
            Ok(servers) if servers.is_empty() => return Err(FallbackError::NotConfigured),
            Ok(servers) => servers,
            Err(e) => return Err(FallbackError::Undetermined(vec![format!("no Electrum client: {}", e)])),
        };
        let fresh_tip = bounded(deadline, "the fresh tip read", async {
            lightwalletd.confirmation_tip().await.map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| {
            FallbackError::Undetermined(vec![format!(
                "the fresh tip could not be read to vet the Electrum servers: {}",
                e
            )])
        })?;
        Ok(Walk {
            fallback: self,
            lightwalletd,
            servers,
            next: 0,
            fresh_tip,
            deadline,
            conditions: Vec::new(),
        })
    }

    /// The first Electrum server that passes vetting (R39.11.25(a), (b)) and then answers one script-history
    /// read, of `probe_script`, or the conditions that stopped each. This is the capability "can any Electrum
    /// server be used for this coin's address questions" of the taker refusal (R39.11.28). The content of
    /// the history is irrelevant: an empty one is an answer.
    pub(crate) async fn first_usable_server(
        &self,
        lightwalletd: &LightwalletdBackend,
        probe_script: &[u8],
    ) -> Result<String, FallbackError> {
        let mut walk = self.walk(lightwalletd).await?;
        let script_hash = hex::encode(electrum_script_hash(probe_script));
        while let Some((server, deadline)) = walk.next_usable().await {
            match bounded(
                deadline,
                "the script history read",
                self.reads.history(&server, &script_hash),
            )
            .await
            {
                Ok(_) => return Ok(server),
                Err(condition) => walk.conditions.push(format!("{}: {}", server, condition)),
            }
        }
        Err(FallbackError::Undetermined(walk.conditions))
    }

    /// The taker refusal's capability check (R39.11.28): at least one source can answer the swap's address
    /// questions, that is, an admitted lightwalletd endpoint answers a transparent-address read of
    /// `own_address` without the no-address-index rejection (marking an endpoint that answers with it), or
    /// else an Electrum server passes R39.11.25 (a) and (b) and answers a script-history read of the same
    /// address. lightwalletd is asked first; Electrum only when it cannot answer. `Ok` names the source;
    /// `Err` is the refusal text, naming the coin, every lightwalletd endpoint with its condition and every
    /// Electrum server with its condition.
    pub(crate) async fn ensure_watch_source(
        &self,
        lightwalletd: &LightwalletdBackend,
        own_address: &Address,
    ) -> Result<String, String> {
        let lightwalletd_conditions = match lightwalletd.probe_address_read(own_address).await {
            Ok(server) => return Ok(format!("lightwalletd {}", server)),
            Err(conditions) => conditions,
        };
        let own_script = Builder::build_p2pkh(&own_address.hash).to_bytes();
        let electrum_conditions = match self.first_usable_server(lightwalletd, &own_script).await {
            Ok(server) => return Ok(format!("Electrum {}", server)),
            Err(FallbackError::NotConfigured) => "no server is listed in the activation's electrum_servers".to_owned(),
            Err(FallbackError::Undetermined(conditions)) => conditions.join("; "),
        };
        Err(format!(
            "{}: the taker could not watch its payment for the maker's spend, because no source can answer an \
             address read. lightwalletd endpoints: {}. Electrum servers: {}",
            self.ticker,
            lightwalletd_conditions.join("; "),
            electrum_conditions
        ))
    }

    /// Already-sent (R39.11.18) from the Electrum servers: "sent" with a history transaction that has an
    /// output of exactly `script_pubkey`, "not sent" with a complete, empty history, anything else
    /// undetermined (R39.11.25(c), (d)).
    pub(crate) async fn already_sent(
        &self,
        lightwalletd: &LightwalletdBackend,
        script_pubkey: &[u8],
        log: &DetectionLog<'_>,
    ) -> Result<SentAnswer, FallbackError> {
        let Some(hash) = p2sh_hash_of(script_pubkey) else {
            return Err(FallbackError::Undetermined(vec![
                "the HTLC script-pubkey is not pay-to-script-hash".to_owned(),
            ]));
        };
        let progress = Progress { lightwalletd, hash };
        let result = self.already_sent_of(lightwalletd, script_pubkey, &progress, log).await;
        lightwalletd.end_progress(&hash);
        result
    }

    async fn already_sent_of(
        &self,
        lightwalletd: &LightwalletdBackend,
        script_pubkey: &[u8],
        progress: &Progress<'_>,
        log: &DetectionLog<'_>,
    ) -> Result<SentAnswer, FallbackError> {
        let mut walk = self.walk(lightwalletd).await?;
        log.line(format!(
            "HTLC already-sent detection for {}: no lightwalletd endpoint answered; asking the Electrum servers \
             ({} listed, fresh tip {})",
            log.coin,
            walk.servers.len(),
            walk.fresh_tip
        ));
        let script_hash = hex::encode(electrum_script_hash(script_pubkey));
        while let Some((server, deadline)) = walk.next_usable().await {
            match self
                .sent_on(&server, script_pubkey, &script_hash, deadline, progress)
                .await
            {
                Ok(answer) => {
                    log.line(format!(
                        "HTLC already-sent detection for {}: Electrum {} answered: {}",
                        log.coin,
                        self.name(&server),
                        match &answer {
                            SentAnswer::Sent(tx) => format!("sent, payment transaction {:?}", txid_display(tx)),
                            SentAnswer::NotSent => "not sent (its script history is complete and empty)".to_owned(),
                        }
                    ));
                    return Ok(answer);
                },
                Err(condition) => walk.conditions.push(format!("{}: {}", self.name(&server), condition)),
            }
        }
        log.line(format!(
            "HTLC already-sent detection for {}: no Electrum server answered: {}",
            log.coin,
            walk.conditions.join("; ")
        ));
        Err(FallbackError::Undetermined(walk.conditions))
    }

    async fn sent_on(
        &self,
        server: &str,
        script_pubkey: &[u8],
        script_hash: &str,
        deadline: TokioInstant,
        progress: &Progress<'_>,
    ) -> Result<SentAnswer, String> {
        progress.set(format!("Electrum {}, script history", self.name(server)));
        let history = bounded(
            deadline,
            "the script history read",
            self.reads.history(server, script_hash),
        )
        .await?;
        if history.is_empty() {
            return Ok(SentAnswer::NotSent);
        }
        let mut problems = Vec::new();
        for item in &history {
            progress.set(format!("Electrum {}, transaction {:?}", self.name(server), item.txid));
            match self.read_transaction(server, item, deadline).await {
                Ok(tx) if pays_script(&tx, script_pubkey) => return Ok(SentAnswer::Sent(tx)),
                Ok(_) => {},
                Err(problem) => problems.push(problem),
            }
        }
        Err(if problems.is_empty() {
            format!(
                "its script history names {} transaction(s) and none has an output of exactly the HTLC script",
                history.len()
            )
        } else {
            problems.join("; ")
        })
    }

    /// The transaction a history item names, decoded with the Zcash reader, and checked to be the one asked
    /// for: a server that answers with another transaction is no evidence.
    async fn read_transaction(
        &self,
        server: &str,
        item: &HistoryItem,
        deadline: TokioInstant,
    ) -> Result<ZTransaction, String> {
        let raw = bounded(
            deadline,
            "a transaction read",
            self.reads.transaction(server, &item.txid),
        )
        .await
        .map_err(|e| format!("transaction {:?} could not be fetched: {}", item.txid, e))?;
        let tx = read_tx(&raw).map_err(|e| format!("transaction {:?}: {}", item.txid, e))?;
        if txid_display(&tx) != item.txid {
            return Err(format!(
                "transaction {:?} was answered with transaction {:?}",
                item.txid,
                txid_display(&tx)
            ));
        }
        Ok(tx)
    }

    /// The spend of `output` (R39.11.21, R39.11.22) from the Electrum servers: the history transaction that
    /// spends the HTLC outpoint, or "not spent" only when the unspent listing lists the outpoint and no
    /// history transaction spends it; anything else undetermined (R39.11.25(c), (d)).
    pub(crate) async fn spend(
        &self,
        lightwalletd: &LightwalletdBackend,
        output: &OutputRef,
        log: &DetectionLog<'_>,
    ) -> Result<SpendAnswer, FallbackError> {
        let Some(hash) = p2sh_hash_of(&output.script_pubkey) else {
            return Err(FallbackError::Undetermined(vec![
                "the watched output's script-pubkey is not pay-to-script-hash".to_owned(),
            ]));
        };
        let progress = Progress { lightwalletd, hash };
        let result = self.spend_of(lightwalletd, output, &progress, log).await;
        lightwalletd.end_progress(&hash);
        result
    }

    async fn spend_of(
        &self,
        lightwalletd: &LightwalletdBackend,
        output: &OutputRef,
        progress: &Progress<'_>,
        log: &DetectionLog<'_>,
    ) -> Result<SpendAnswer, FallbackError> {
        let mut walk = self.walk(lightwalletd).await?;
        log.line(format!(
            "HTLC spend detection for {}: no lightwalletd endpoint answered; asking the Electrum servers ({} \
             listed, fresh tip {})",
            log.coin,
            walk.servers.len(),
            walk.fresh_tip
        ));
        let script_hash = hex::encode(electrum_script_hash(&output.script_pubkey));
        while let Some((server, deadline)) = walk.next_usable().await {
            match self.spend_on(&server, output, &script_hash, deadline, progress).await {
                Ok(answer) => {
                    log.line(format!(
                        "HTLC spend detection for {}: Electrum {} answered: {}",
                        log.coin,
                        self.name(&server),
                        match &answer {
                            SpendAnswer::Spent { tx, input_index } => {
                                format!("spent by {:?}, input {}", txid_display(tx), input_index)
                            },
                            SpendAnswer::NotSpent => "not spent (the unspent listing has the output)".to_owned(),
                        }
                    ));
                    return Ok(answer);
                },
                Err(condition) => walk.conditions.push(format!("{}: {}", self.name(&server), condition)),
            }
        }
        log.line(format!(
            "HTLC spend detection for {}: no Electrum server answered: {}",
            log.coin,
            walk.conditions.join("; ")
        ));
        Err(FallbackError::Undetermined(walk.conditions))
    }

    async fn spend_on(
        &self,
        server: &str,
        output: &OutputRef,
        script_hash: &str,
        deadline: TokioInstant,
        progress: &Progress<'_>,
    ) -> Result<SpendAnswer, String> {
        progress.set(format!("Electrum {}, script history", self.name(server)));
        let history = bounded(
            deadline,
            "the script history read",
            self.reads.history(server, script_hash),
        )
        .await?;
        let funding = txid_internal_to_display(output.txid);
        let mut problems = Vec::new();
        for item in history.iter().filter(|item| item.txid != funding) {
            progress.set(format!("Electrum {}, transaction {:?}", self.name(server), item.txid));
            match self.read_transaction(server, item, deadline).await {
                Ok(tx) => {
                    let spender = tx.transparent_bundle().and_then(|bundle| {
                        bundle.vin.iter().position(|input| {
                            *input.prevout().hash() == output.txid && input.prevout().n() == output.vout
                        })
                    });
                    if let Some(input_index) = spender {
                        return Ok(SpendAnswer::Spent { tx, input_index });
                    }
                },
                Err(problem) => problems.push(problem),
            }
        }
        // No spender was found; a history transaction that could not be read might have been one.
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        progress.set(format!("Electrum {}, unspent listing", self.name(server)));
        let unspent = bounded(
            deadline,
            "the unspent listing read",
            self.reads.unspent(server, script_hash),
        )
        .await?;
        if unspent
            .iter()
            .any(|entry| entry.txid == funding && entry.vout == output.vout)
        {
            return Ok(SpendAnswer::NotSpent);
        }
        Err(format!(
            "the HTLC output {:?}:{} is not in its unspent listing ({} entries) and none of its {} history \
             transaction(s) spends it",
            funding,
            output.vout,
            unspent.len(),
            history.len()
        ))
    }
}

/// Whether `tx` has a transparent output of exactly `script_pubkey`.
fn pays_script(tx: &ZTransaction, script_pubkey: &[u8]) -> bool {
    tx.transparent_bundle().is_some_and(|bundle| {
        bundle
            .vout
            .iter()
            .any(|out| out.script_pubkey().0 .0.as_slice() == script_pubkey)
    })
}

#[cfg(test)] pub(crate) mod tests;
