//! The Electrum connection pool of a coin (CRD ch.38 R38.6.4b-R38.6.4k-R).
//!
//! The servers listed at activation form the pool for the coin's lifetime. The pool keeps a *maintained
//! set* of connections between a floor and a ceiling, prefers the servers ranked first (the order
//! drawn once at activation, or the listed order, see [`ServerOrder`]), suspends a
//! server that keeps failing for a growing time, routes every request through the maintained set in
//! priority order, retries over every server of the pool when the maintained set fails, and derives
//! the time each attempt may take from the call's overall budget (R38.6.4j).
//!
//! Every duration is held in [`ElectrumPoolTimings`]; production uses [`ElectrumPoolTimings::default`],
//! tests build the pool with millisecond values.

use super::electrum_rpc_client::{open_session, server_spec, ServerSpec, Session, SessionEnded};
use super::*;
use common::log::{debug, warn};
use futures::channel::mpsc as futures_mpsc;
use futures::future::{join_all, select as select_future, Either as EitherFuture};
use futures::stream::FuturesUnordered;
use mm2_core::mm_ctx::MmArc;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex as StdMutex, RwLock, Weak};

/// Every duration the pool uses (CRD ch.38 R38.6.4c, R38.6.4d, R38.6.4e, R38.6.4j).
#[derive(Clone, Debug)]
pub struct ElectrumPoolTimings {
    /// Bound on establishing a connection (resolving, connecting, TLS, version negotiation) when the
    /// server sets no `timeout_sec` (R38.6.4e item 1).
    pub establishment: Duration,
    /// Bound on a request over an established connection (R38.6.4e item 2).
    pub request: Duration,
    /// A connection silent for this long is closed (R38.6.4e item 3).
    pub idle_cutoff: Duration,
    /// How often `server.ping` goes over the maintained connections (R38.6.4e item 4).
    pub ping_interval: Duration,
    /// The regular cadence of maintenance passes (R38.6.4c item 1).
    pub maintenance_interval: Duration,
    /// The pause before a pass that ended below the floor is repeated (R38.6.4c item 4).
    pub short_repass: Duration,
    /// The first suspension of a server (R38.6.4d item 1).
    pub suspension_base: Duration,
    /// The longest suspension of a server (R38.6.4d item 1).
    pub suspension_cap: Duration,
    /// The default overall budget *B* of a call (R38.6.4j item 1).
    pub call_budget: Duration,
    /// The part of *B* no round may use up (R38.6.4j item 2).
    pub budget_reserve: Duration,
    /// The smallest budget of a round (R38.6.4j item 2).
    pub round_floor: Duration,
}

impl Default for ElectrumPoolTimings {
    fn default() -> Self {
        ElectrumPoolTimings {
            establishment: Duration::from_secs(20),
            request: Duration::from_secs(20),
            idle_cutoff: Duration::from_secs(60),
            ping_interval: Duration::from_secs(30),
            maintenance_interval: Duration::from_secs(5 * 60),
            short_repass: Duration::from_secs(1),
            suspension_base: Duration::from_secs(20),
            suspension_cap: Duration::from_secs(12 * 60 * 60),
            call_budget: Duration::from_secs(60),
            budget_reserve: Duration::from_secs(5),
            round_floor: Duration::from_secs(5),
        }
    }
}

/// The order in which the listed servers are ranked (CRD ch.38 R38.6.4b-R). The rank is the position in
/// that order and the only notion of priority the pool has.
#[derive(Clone, Debug, Default)]
pub enum ServerOrder {
    /// The listed order is the priority.
    #[default]
    Listed,
    /// A uniformly random order, drawn once when the servers are added.
    Random,
    /// Tests: the server at rank `i` is the listed server `order[i]`.
    #[cfg(test)]
    Fixed(Vec<usize>),
}

impl ServerOrder {
    /// A uniformly random order of `items`, drawn from `rng`.
    pub(super) fn shuffle_with<T, R: rand::Rng>(mut items: Vec<T>, rng: &mut R) -> Vec<T> {
        items.shuffle(rng);
        items
    }

    /// Puts `items`, given in the listed order, into the pool's order.
    pub(super) fn arrange<T>(&self, items: Vec<T>) -> Vec<T> {
        match self {
            ServerOrder::Listed => items,
            ServerOrder::Random => ServerOrder::shuffle_with(items, &mut common::small_rng()),
            #[cfg(test)]
            ServerOrder::Fixed(order) => {
                assert_eq!(order.len(), items.len(), "the injected order must cover every server");
                let mut listed: Vec<Option<T>> = items.into_iter().map(Some).collect();
                order
                    .iter()
                    .map(|index| listed[*index].take().expect("the injected order repeats a server"))
                    .collect()
            },
        }
    }
}

/// The order `conf` asks for, and the warning to log when its `electrum_server_order` is not recognized.
pub(super) fn parse_server_order(conf: &Json) -> (ServerOrder, Option<String>) {
    match &conf["electrum_server_order"] {
        Json::Null => (ServerOrder::Random, None),
        Json::String(value) if value == "random" => (ServerOrder::Random, None),
        Json::String(value) if value == "listed" => (ServerOrder::Listed, None),
        other => {
            let shown = match other {
                Json::String(value) => value.clone(),
                other => other.to_string(),
            };
            (
                ServerOrder::Random,
                Some(format!(
                    "electrum_server_order \"{shown}\" is not recognized; using \"random\""
                )),
            )
        },
    }
}

/// CRD ch.38 R38.6.4b-R: the `MM2.json` string `electrum_server_order`. `random` (the default, also used when
/// the key is absent) draws the order once at activation; `listed` keeps the listed order. Any other value
/// is taken as `random` and one warning is logged.
///
/// **Compatibility:** for behaviour matching GLEEC KDF, set `electrum_server_order` to `listed`.
pub(crate) fn electrum_server_order(ctx: &MmArc) -> ServerOrder {
    let (order, warning) = parse_server_order(&ctx.conf);
    if let Some(warning) = warning {
        warn!("{warning}");
    }
    order
}

/// How a coin's pool is set up (CRD ch.38 R38.6.4b).
#[derive(Clone, Debug)]
pub struct ElectrumPoolOptions {
    /// The floor of the maintained set; at least 1.
    pub min_connected: usize,
    /// The ceiling of the maintained set; the number of listed servers when unset.
    pub max_connected: Option<usize>,
    /// The client name offered in `server.version`. `None` skips the version negotiation.
    pub client_name: Option<String>,
    pub timings: ElectrumPoolTimings,
    /// How [`ElectrumClientImpl::add_servers`] ranks the listed servers.
    pub order: ServerOrder,
}

impl Default for ElectrumPoolOptions {
    fn default() -> Self {
        ElectrumPoolOptions {
            min_connected: 1,
            max_connected: None,
            client_name: None,
            timings: ElectrumPoolTimings::default(),
            order: ServerOrder::default(),
        }
    }
}

/// The time a failing server stays out of the maintenance passes (CRD ch.38 R38.6.4d item 1).
///
/// The state is a pure function of the failures it has seen and of the times they happened, so the
/// 12-hour cap is tested by feeding it timestamps instead of waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Suspension {
    /// Disconnections and failed attempts since the last successful connection.
    failures: u32,
    /// The time (seconds since the epoch) the suspension lapses.
    until: f64,
}

impl Suspension {
    /// Records a disconnection or a failed attempt at `now` and returns the length of the suspension in seconds.
    pub(super) fn on_failure(&mut self, now: f64, timings: &ElectrumPoolTimings) -> f64 {
        let doubled = timings.suspension_base.as_secs_f64() * 2f64.powi(self.failures.min(60) as i32);
        let secs = doubled.min(timings.suspension_cap.as_secs_f64());
        self.failures = self.failures.saturating_add(1);
        self.until = now + secs;
        secs
    }

    /// A successful connection resets the server: its next suspension is the base one again.
    pub(super) fn on_success(&mut self) { *self = Suspension::default(); }

    pub(super) fn is_suspended(&self, now: f64) -> bool { now < self.until }
}

/// The budget of one round of attempts (CRD ch.38 R38.6.4j item 2): the smaller of the per-attempt
/// bound `attempt_cap` (establishment plus request) and an equal share of what remains of the call's
/// budget after the reserve, for each of the `rounds_left` rounds still to make, never below the floor.
pub(super) fn round_budget(
    remaining: Duration,
    rounds_left: usize,
    attempt_cap: Duration,
    timings: &ElectrumPoolTimings,
) -> Duration {
    let rounds = rounds_left.max(1) as u32;
    let share = remaining.saturating_sub(timings.budget_reserve) / rounds;
    share.min(attempt_cap).max(timings.round_floor)
}

fn secs_left(deadline: f64) -> Duration {
    let left = deadline - now_float();
    if left > 0. {
        Duration::from_secs_f64(left)
    } else {
        Duration::ZERO
    }
}

/// What wakes the maintenance loop besides its timer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Wake {
    /// Run a maintenance pass at once.
    Pass,
    /// Place the script-hash subscriptions again (R38.6.4h); no pass is needed for that.
    Resubscribe,
}

/// Why an attempt to establish a connection failed (CRD ch.38 R38.6.4d items 1 and 3).
#[derive(Debug)]
enum EstablishError {
    /// Retrying cannot cure it: the server leaves the pool.
    Removed(String),
    /// The server is suspended and tried again later.
    Recoverable(String),
}

struct SlotState {
    removed: Option<String>,
    maintained: bool,
    session: Option<Arc<Session>>,
    suspension: Suspension,
}

/// One server of the pool. The position in the pool's order is the priority (`rank`, 0 is the highest).
pub(super) struct ServerSlot {
    rank: usize,
    addr: String,
    establish_timeout: Duration,
    spec: Result<ServerSpec, String>,
    /// Held while a connection of this server is being established, so two attempts never both connect.
    connect_lock: AsyncMutex<()>,
    state: StdMutex<SlotState>,
}

impl ServerSlot {
    fn new(rank: usize, req: &ElectrumRpcRequest, timings: &ElectrumPoolTimings) -> ServerSlot {
        let spec = server_spec(req);
        let removed = spec.as_ref().err().cloned();
        ServerSlot {
            rank,
            addr: req.url.clone(),
            establish_timeout: req.request_timeout().unwrap_or(timings.establishment),
            spec,
            connect_lock: AsyncMutex::new(()),
            state: StdMutex::new(SlotState {
                removed,
                maintained: false,
                session: None,
                suspension: Suspension::default(),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_removed(&self) -> bool { self.state().removed.is_some() }

    fn removed_reason(&self) -> Option<String> { self.state().removed.clone() }

    fn is_maintained(&self) -> bool { self.state().maintained }

    fn is_suspended(&self, now: f64) -> bool { self.state().suspension.is_suspended(now) }

    /// The established connection, if the server has one that is still open.
    fn live_session(&self) -> Option<Arc<Session>> {
        self.state()
            .session
            .as_ref()
            .filter(|session| !session.is_closed())
            .cloned()
    }
}

/// The connections a call opened, to be released when the call ends if their servers are not maintained then.
#[derive(Default)]
pub(super) struct OpenedConnections {
    sessions: Vec<(Arc<ServerSlot>, u64)>,
    /// Set when the call ended; a connection that is established later is released at once.
    finished: bool,
}

pub(super) type OpenedByCall = Arc<StdMutex<OpenedConnections>>;

/// The failures of a call, one per server tried (CRD ch.38 R38.6.4i).
#[derive(Default)]
struct Failures(Vec<(String, String)>);

impl Failures {
    fn record(&mut self, addr: &str, error: String) {
        match self.0.iter_mut().find(|(known, _)| known == addr) {
            Some(entry) => entry.1 = error,
            None => self.0.push((addr.to_owned(), error)),
        }
    }

    fn describe(&self, ticker: &str, note: &str) -> String {
        self.describe_as(ticker, "the request failed on every Electrum server tried", note)
    }

    fn describe_as(&self, ticker: &str, headline: &str, note: &str) -> String {
        if self.0.is_empty() {
            return format!("{ticker}: no Electrum server could be tried{note}");
        }
        let servers: Vec<String> = self.0.iter().map(|(addr, error)| format!("{addr}: {error}")).collect();
        format!("{ticker}: {headline} ({}): {}{note}", self.0.len(), servers.join("; "))
    }
}

/// What a test sees of one server of the pool.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct ServerStateSnapshot {
    pub addr: String,
    pub rank: usize,
    pub maintained: bool,
    /// Not maintained, not removed: a candidate of the maintenance passes.
    pub candidate: bool,
    /// The server has an open connection, maintained or not.
    pub connected: bool,
    pub removed: bool,
    pub suspended_until: Option<f64>,
    pub suspension_level: u32,
}

pub(super) struct PoolShared {
    ticker: String,
    servers: RwLock<Vec<Arc<ServerSlot>>>,
    min_connected: usize,
    max_connected: Option<usize>,
    pub(super) timings: ElectrumPoolTimings,
    client_name: Option<String>,
    order: ServerOrder,
    protocol_version: OrdRange<f32>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    next_session_id: AtomicU64,
    next_request_id: AtomicU64,
    /// Counts the connections that ended; a change tells a subscriber its subscription may be lost.
    session_epoch: AtomicU64,
    resubscribe_pending: AtomicBool,
    trigger_tx: futures_mpsc::UnboundedSender<Wake>,
    trigger_rx: StdMutex<Option<futures_mpsc::UnboundedReceiver<Wake>>>,
}

impl PoolShared {
    pub(super) fn new(
        ticker: String,
        options: ElectrumPoolOptions,
        protocol_version: OrdRange<f32>,
        event_handlers: Vec<RpcTransportEventHandlerShared>,
    ) -> PoolShared {
        let (trigger_tx, trigger_rx) = futures_mpsc::unbounded();
        PoolShared {
            ticker,
            servers: RwLock::new(Vec::new()),
            min_connected: options.min_connected.max(1),
            max_connected: options.max_connected,
            timings: options.timings,
            client_name: options.client_name,
            order: options.order,
            protocol_version,
            event_handlers,
            next_session_id: AtomicU64::new(1),
            next_request_id: AtomicU64::new(0),
            session_epoch: AtomicU64::new(0),
            resubscribe_pending: AtomicBool::new(false),
            trigger_tx,
            trigger_rx: StdMutex::new(Some(trigger_rx)),
        }
    }

    // ---- composition -------------------------------------------------------------------------

    /// Adds `req` to the pool. A server that cannot be used by this build stays in the list, at its
    /// rank, as removed (R38.6.4d item 3), and the reason is returned.
    pub(super) fn add_server(&self, req: &ElectrumRpcRequest) -> Result<(), String> {
        let mut servers = self.servers.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        let slot = Arc::new(ServerSlot::new(servers.len(), req, &self.timings));
        let removed = slot.removed_reason();
        servers.push(slot);
        match removed {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Adds the servers, given in the listed order, ranked by the pool's order (R38.6.4b-R): the order is
    /// drawn here, once. Returns the servers this build cannot use, with the reason.
    pub(super) fn add_servers(&self, reqs: &[ElectrumRpcRequest]) -> Vec<(ElectrumRpcRequest, String)> {
        self.order
            .arrange(reqs.to_vec())
            .into_iter()
            .filter_map(|req| self.add_server(&req).err().map(|reason| (req, reason)))
            .collect()
    }

    pub(super) fn slots(&self) -> Vec<Arc<ServerSlot>> {
        self.servers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn slot_by_rank(&self, rank: usize) -> Option<Arc<ServerSlot>> {
        self.servers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(rank)
            .cloned()
    }

    fn slot_by_addr(&self, addr: &str) -> Option<Arc<ServerSlot>> {
        self.slots().into_iter().find(|slot| slot.addr == addr)
    }

    /// The servers not removed from the pool, in priority order.
    fn usable_slots(&self) -> Vec<Arc<ServerSlot>> {
        self.slots().into_iter().filter(|slot| !slot.is_removed()).collect()
    }

    pub(super) fn usable_count(&self) -> usize { self.usable_slots().len() }

    /// The ceiling of the maintained set; a ceiling above the list length behaves as the list length.
    pub(super) fn ceiling(&self) -> usize {
        let listed = self.slots().len().max(1);
        self.max_connected.unwrap_or(listed).clamp(1, listed)
    }

    pub(super) fn floor(&self) -> usize { self.min_connected }

    /// The maintained connections in priority order.
    fn maintained_slots(&self) -> Vec<Arc<ServerSlot>> {
        self.slots()
            .into_iter()
            .filter(|slot| {
                let state = slot.state();
                state.removed.is_none() && state.maintained && state.session.is_some()
            })
            .collect()
    }

    pub(super) fn maintained_count(&self) -> usize { self.maintained_slots().len() }

    pub(super) fn has_open_connection(&self) -> bool { self.slots().iter().any(|slot| slot.live_session().is_some()) }

    /// The id of the next request of the client; the negotiation and the ping draw from the same counter as the callers.
    pub(super) fn fresh_request_id(&self) -> String {
        self.next_request_id.fetch_add(1, AtomicOrdering::Relaxed).to_string()
    }

    /// Asks for a maintenance pass at once (R38.6.4c item 1).
    pub(super) fn trigger(&self) { let _ = self.trigger_tx.unbounded_send(Wake::Pass); }

    pub(super) fn session_epoch(&self) -> u64 { self.session_epoch.load(AtomicOrdering::Relaxed) }

    /// Asks for the script-hash subscriptions to be placed again over a maintained connection (R38.6.4h).
    pub(super) fn request_resubscribe(&self) {
        self.resubscribe_pending.store(true, AtomicOrdering::Relaxed);
        let _ = self.trigger_tx.unbounded_send(Wake::Resubscribe);
    }

    fn take_resubscribe_pending(&self) -> bool { self.resubscribe_pending.swap(false, AtomicOrdering::Relaxed) }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Vec<ServerStateSnapshot> {
        self.slots()
            .iter()
            .map(|slot| {
                let state = slot.state();
                let suspended = state.suspension.is_suspended(now_float());
                ServerStateSnapshot {
                    addr: slot.addr.clone(),
                    rank: slot.rank,
                    maintained: state.maintained,
                    candidate: !state.maintained && state.removed.is_none(),
                    connected: state.session.as_ref().map_or(false, |session| !session.is_closed()),
                    removed: state.removed.is_some(),
                    suspended_until: suspended.then_some(state.suspension.until),
                    suspension_level: state.suspension.failures,
                }
            })
            .collect()
    }

    // ---- establishing, ending and releasing connections --------------------------------------

    /// Establishes a connection to the server: resolves, connects, does the TLS handshake where
    /// configured and negotiates the protocol version, all within the server's establishment bound
    /// (R38.6.4e item 1). A connection whose negotiation did not succeed is never returned.
    async fn establish(self: &Arc<Self>, slot: &ServerSlot) -> Result<Arc<Session>, EstablishError> {
        let spec = slot
            .spec
            .as_ref()
            .map_err(|reason| EstablishError::Removed(reason.clone()))?;
        let session_id = self.next_session_id.fetch_add(1, AtomicOrdering::Relaxed);
        let weak = Arc::downgrade(self);
        let rank = slot.rank;
        let on_end: SessionEnded = Arc::new(move |ended_id| {
            if let Some(pool) = weak.upgrade() {
                pool.session_ended(rank, ended_id)
            }
        });
        let attempt = async {
            let session = open_session(
                session_id,
                spec,
                self.event_handlers.clone(),
                self.timings.idle_cutoff,
                on_end,
            )
            .await
            .map_err(EstablishError::Recoverable)?;
            let session = Arc::new(session);
            // The session is closed again unless the negotiation succeeds, also when this future is dropped.
            let guard = CloseOnDrop(Some(session.clone()));
            if let Some(client_name) = &self.client_name {
                self.negotiate_version(&session, client_name, slot.establish_timeout)
                    .await?;
            }
            guard.defuse();
            Ok(session)
        };
        match attempt.boxed().timeout(slot.establish_timeout).await {
            Ok(result) => result,
            Err(_) => Err(EstablishError::Recoverable(format!(
                "the connection was not established within {:.1} s",
                slot.establish_timeout.as_secs_f64()
            ))),
        }
    }

    /// `server.version` with the client's supported range (R38.6.4e item 1), over a connection being established.
    async fn negotiate_version(
        &self,
        session: &Session,
        client_name: &str,
        timeout: Duration,
    ) -> Result<(), EstablishError> {
        let versions: Vec<String> = self
            .protocol_version
            .flatten()
            .into_iter()
            .map(|version| format!("{}", version))
            .collect();
        let request = JsonRpcRequestEnum::Single(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: self.fresh_request_id(),
            method: "server.version".into(),
            params: vec![json::json!(client_name), json::json!(versions)],
        });
        let response = session
            .request(&request, timeout)
            .await
            .map_err(EstablishError::Recoverable)?;
        let single = match response {
            JsonRpcResponseEnum::Single(single) => single,
            JsonRpcResponseEnum::Batch(_) => {
                return Err(EstablishError::Recoverable(
                    "the answer to server.version could not be read: a batch answer".to_owned(),
                ))
            },
        };
        if !single.error.is_null() {
            return Err(EstablishError::Removed(format!(
                "the server rejected the protocol versions offered ({}..{}): {}",
                self.protocol_version.start(),
                self.protocol_version.end(),
                single.error
            )));
        }
        let version: ElectrumProtocolVersion = json::from_value(single.result).map_err(|e| {
            EstablishError::Recoverable(format!("the answer to server.version could not be read: {}", e))
        })?;
        let actual: f32 = version.protocol_version.parse().map_err(|e| {
            EstablishError::Recoverable(format!(
                "the protocol version {:?} the server answered could not be read: {}",
                version.protocol_version, e
            ))
        })?;
        if !self.protocol_version.contains(&actual) {
            return Err(EstablishError::Removed(format!(
                "the server answered the unsupported protocol version {}",
                actual
            )));
        }
        info!(
            "{}: using protocol version {} for Electrum {}",
            self.ticker,
            actual,
            session.addr()
        );
        Ok(())
    }

    /// The connection of `slot`, established first if there is none (R38.6.4e item 1, R38.6.4g). The
    /// flag tells whether this call established it; such a connection is registered in `opened`.
    ///
    /// The establishment runs as a task of its own, so that it needs no particular executor of the caller
    /// and goes on, and keeps the server's connection lock, when the caller gives up waiting for it.
    pub(super) async fn ensure_connected(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        opened: Option<&OpenedByCall>,
    ) -> Result<(Arc<Session>, bool), String> {
        if let Some(session) = slot.live_session() {
            return Ok((session, false));
        }
        let (result_tx, result_rx) = futures::channel::oneshot::channel();
        let (pool, slot, opened) = (self.clone(), slot.clone(), opened.cloned());
        spawn(async move {
            let result = pool.connect_server(&slot, opened.as_ref()).await;
            // The caller may have stopped waiting.
            let _ = result_tx.send(result);
        });
        result_rx
            .await
            .map_err(|_| "the connection attempt was abandoned".to_owned())?
    }

    async fn connect_server(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        opened: Option<&OpenedByCall>,
    ) -> Result<(Arc<Session>, bool), String> {
        let _connecting = slot.connect_lock.lock().await;
        if let Some(reason) = slot.removed_reason() {
            return Err(format!("the server was removed from the pool: {}", reason));
        }
        if let Some(session) = slot.live_session() {
            return Ok((session, false));
        }
        match self.establish(slot).await {
            Ok(session) => {
                {
                    let mut state = slot.state();
                    state.session = Some(session.clone());
                    state.suspension.on_success();
                }
                if session.is_closed() {
                    self.session_ended(slot.rank, session.id());
                    return Err("the connection ended right after it was established".to_owned());
                }
                if let Some(opened) = opened {
                    let mut opened = opened.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if opened.finished {
                        drop(opened);
                        self.release_one(slot, session.id());
                    } else {
                        opened.sessions.push((slot.clone(), session.id()));
                    }
                }
                Ok((session, true))
            },
            Err(EstablishError::Removed(reason)) => {
                warn!(
                    "{}: Electrum server {} is removed from the pool: {}",
                    self.ticker, slot.addr, reason
                );
                slot.state().removed = Some(reason.clone());
                Err(reason)
            },
            Err(EstablishError::Recoverable(error)) => {
                let secs = slot.state().suspension.on_failure(now_float(), &self.timings);
                debug!(
                    "{}: Electrum server {} could not be connected ({}), suspended for {:.0} s",
                    self.ticker, slot.addr, error, secs
                );
                Err(error)
            },
        }
    }

    /// A connection ended, whatever the reason (a dropped transport, the idle cutoff, a close by this
    /// client): the server leaves the maintained set and is suspended (R38.6.4d item 1), the
    /// subscriptions it carried are to be placed again (R38.6.4h), and a pass runs at once if the
    /// maintained set fell below the floor (R38.6.4c item 1). Ending the same connection twice is harmless.
    fn session_ended(&self, rank: usize, session_id: u64) {
        let Some(slot) = self.slot_by_rank(rank) else { return };
        {
            let mut state = slot.state();
            if state.session.as_ref().map(|session| session.id()) != Some(session_id) {
                return;
            }
            state.session = None;
            state.maintained = false;
            state.suspension.on_failure(now_float(), &self.timings);
        }
        self.session_epoch.fetch_add(1, AtomicOrdering::Relaxed);
        self.request_resubscribe();
        if self.maintained_count() < self.floor() {
            self.trigger();
        }
    }

    /// Closes the connection of a server that failed a request, which demotes and suspends it (R38.6.4f item 1).
    fn close_slot_session(&self, slot: &ServerSlot, session: &Session) {
        session.close();
        self.session_ended(slot.rank, session.id());
    }

    /// Releases the connections a call opened to servers that are not maintained (R38.6.4f item 2, R38.6.4g).
    fn release_opened(&self, opened: &OpenedByCall) {
        let sessions = {
            let mut opened = opened.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            opened.finished = true;
            std::mem::take(&mut opened.sessions)
        };
        for (slot, session_id) in sessions {
            self.release_one(&slot, session_id);
        }
    }

    #[cfg(test)]
    pub(super) fn release_opened_for_test(&self, opened: &OpenedByCall) { self.release_opened(opened); }

    /// Closes the connection with the id `session_id` of `slot` unless the server is maintained.
    fn release_one(&self, slot: &Arc<ServerSlot>, session_id: u64) {
        let session = {
            let state = slot.state();
            if state.maintained {
                return;
            }
            state.session.clone().filter(|session| session.id() == session_id)
        };
        if let Some(session) = session {
            self.close_slot_session(slot, &session);
        }
    }

    // ---- maintenance (R38.6.4c) ---------------------------------------------------------------

    /// Adds the connection of `slot` to the maintained set, demoting the lowest-ranked member if the set
    /// then exceeds the ceiling (R38.6.4c item 2).
    fn join_maintained(&self, slot: &Arc<ServerSlot>, session: &Session) {
        if session.is_closed() {
            return;
        }
        let was_empty = self.maintained_count() == 0;
        {
            let mut state = slot.state();
            if state.removed.is_some() || state.session.as_ref().map(|s| s.id()) != Some(session.id()) {
                return;
            }
            state.maintained = true;
        }
        if was_empty {
            self.resubscribe_pending.store(true, AtomicOrdering::Relaxed);
        }
        let maintained = self.maintained_slots();
        if maintained.len() > self.ceiling() {
            if let Some(lowest) = maintained.last() {
                let demoted = lowest.state().session.clone();
                lowest.state().maintained = false;
                if let Some(session) = demoted {
                    // The demoted server is no longer pinged nor asked; its connection is closed at once.
                    self.close_slot_session(lowest, &session);
                }
            }
        }
    }

    /// Offers the connection of `slot`, established by the activation probe, to the maintained set by the
    /// rule of R38.6.4c item 2: it joins while the set is below the ceiling or while it outranks the
    /// lowest-ranked member (which is then demoted); otherwise it is released at once (R38.6.4k-R item 3).
    fn offer_probed(&self, slot: &Arc<ServerSlot>, session: &Session) {
        let maintained = self.maintained_slots();
        let outranks_lowest = maintained.last().map_or(false, |lowest| slot.rank < lowest.rank);
        if maintained.len() < self.ceiling() || outranks_lowest {
            self.join_maintained(slot, session);
        } else {
            self.release_one(slot, session.id());
        }
    }

    /// The activation probe (CRD ch.38 R38.6.4k-R): connects every server of the pool in parallel, whatever
    /// the ceiling and the suspensions, and returns as soon as one establishment succeeds. It fails when no
    /// establishment succeeds before every attempt ended or the call budget *B* (R38.6.4j) is spent; the
    /// failure names the coin and, for each server, its address and failure (R38.6.4i). The probe goes on in
    /// the background until every attempt ended, offering each success to the maintained set.
    pub(super) async fn activation_probe(self: &Arc<Self>) -> Result<(), String> {
        let deadline = now_float() + self.timings.call_budget.as_secs_f64();
        let (first_tx, first_rx) = futures::channel::oneshot::channel();
        let pool = self.clone();
        spawn(async move { pool.run_probe(deadline, first_tx).await });
        first_rx
            .await
            .map_err(|_| format!("{}: the Electrum activation probe was abandoned", self.ticker))?
    }

    async fn run_probe(
        self: Arc<Self>,
        deadline: f64,
        first_tx: futures::channel::oneshot::Sender<Result<(), String>>,
    ) {
        let mut first_tx = Some(first_tx);
        let opened = OpenedByCall::default();
        let mut failures = Failures::default();
        let budget = self.timings.call_budget;
        let mut attempts: FuturesUnordered<_> = self
            .usable_slots()
            .into_iter()
            .map(|slot| {
                let (pool, opened) = (&self, &opened);
                async move {
                    // Each attempt is bounded by its establishment bound, which the establishment itself
                    // enforces, and by what remains of the call budget.
                    let result = match pool
                        .ensure_connected(&slot, Some(opened))
                        .boxed()
                        .timeout(secs_left(deadline))
                        .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(format!(
                            "no connection within the call's time budget of {:.0} s",
                            budget.as_secs_f64()
                        )),
                    };
                    (slot, result)
                }
            })
            .collect();
        while let Some((slot, result)) = attempts.next().await {
            match result {
                Ok((session, _)) => {
                    self.offer_probed(&slot, &session);
                    if let Some(first_tx) = first_tx.take() {
                        let _ = first_tx.send(Ok(()));
                    }
                },
                Err(e) => failures.record(&slot.addr, e),
            }
        }
        drop(attempts);
        // A connection that is established after this point is released at once.
        self.release_opened(&opened);
        if let Some(first_tx) = first_tx.take() {
            let _ = first_tx.send(Err(failures.describe_as(
                &self.ticker,
                "no Electrum server could be connected",
                "",
            )));
        }
    }

    /// One maintenance pass (R38.6.4c items 2 and 3).
    pub(super) async fn run_pass(self: &Arc<Self>) {
        let mut considered: HashSet<usize> = HashSet::new();
        loop {
            let now = now_float();
            let maintained: Vec<usize> = self.maintained_slots().iter().map(|slot| slot.rank).collect();
            let need = self.floor().saturating_sub(maintained.len());
            let candidates: Vec<Arc<ServerSlot>> = self
                .usable_slots()
                .into_iter()
                .filter(|slot| !slot.is_maintained() && !considered.contains(&slot.rank))
                .collect();
            let unsuspended: Vec<Arc<ServerSlot>> = candidates
                .iter()
                .filter(|slot| !slot.is_suspended(now))
                .cloned()
                .collect();
            // Suspended servers are skipped unless the pass would otherwise stay short of the floor (item 2).
            let eligible = if unsuspended.len() < need {
                candidates
            } else {
                unsuspended
            };
            let Some(next) = eligible.into_iter().min_by_key(|slot| slot.rank) else {
                break;
            };
            let outranks_lowest = maintained.iter().max().map_or(false, |lowest| next.rank < *lowest);
            if maintained.len() >= self.ceiling() && !outranks_lowest {
                // Neither fits under the ceiling nor outranks the lowest-ranked member; no lower-ranked can.
                break;
            }
            considered.insert(next.rank);
            match self.ensure_connected(&next, None).await {
                Ok((session, _)) => self.join_maintained(&next, &session),
                Err(e) => debug!("{}: maintenance could not connect {}: {}", self.ticker, next.addr, e),
            }
        }
    }

    /// How long to wait for the next pass (R38.6.4c items 1 and 4).
    pub(super) fn next_pass_delay(&self) -> Duration {
        let usable = self.usable_count();
        let maintained = self.maintained_count();
        let candidates_remain = usable > maintained;
        let floor_reachable = self.floor() <= usable;
        if maintained < self.floor() && candidates_remain && floor_reachable {
            self.timings.short_repass
        } else {
            self.timings.maintenance_interval
        }
    }

    /// Spawns the pool's background tasks: the maintenance loop, which runs its first pass at once, and
    /// optionally the ping loop. Both stop when the client is dropped.
    pub(super) fn start(self: &Arc<Self>, client: Weak<ElectrumClientImpl>, spawn_ping: bool) {
        let trigger_rx = self
            .trigger_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(trigger_rx) = trigger_rx {
            spawn(maintenance_loop(client, trigger_rx));
        }
        if spawn_ping {
            spawn(ping_loop(Arc::downgrade(self)));
        }
    }

    // ---- routing (R38.6.4f, R38.6.4g, R38.6.4j) -----------------------------------------------

    /// Sends `request` over `session`; a failure closes the connection (R38.6.4f item 1).
    async fn request_on(
        &self,
        slot: &ServerSlot,
        session: &Session,
        request: &JsonRpcRequestEnum,
        timeout: Duration,
    ) -> Result<JsonRpcResponseEnum, String> {
        match session.request(request, timeout).await {
            Ok(response) => Ok(response),
            Err(e) => {
                self.close_slot_session(slot, session);
                Err(e)
            },
        }
    }

    /// One attempt of a retry round: connects the server if needed and asks it, all within `round`.
    async fn attempt_with_connect(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        request: &JsonRpcRequestEnum,
        round: Duration,
        opened: &OpenedByCall,
    ) -> Result<JsonRpcResponseEnum, String> {
        let started = now_float();
        let (session, _) = match self.ensure_connected(slot, Some(opened)).boxed().timeout(round).await {
            Ok(connected) => connected?,
            Err(_) => {
                return Err(format!(
                    "no connection within the {:.1} s this round has",
                    round.as_secs_f64()
                ))
            },
        };
        let left = round.saturating_sub(Duration::from_secs_f64((now_float() - started).max(0.)));
        if left.is_zero() {
            return Err("no time left to ask after connecting".to_owned());
        }
        self.request_on(slot, &session, request, left.min(self.timings.request))
            .await
    }

    /// Routes an ordinary request (CRD ch.38 R38.6.4f items 1 and 2) within the call budget `budget` (R38.6.4j).
    pub(super) async fn route(
        self: &Arc<Self>,
        request: JsonRpcRequestEnum,
        budget: Duration,
    ) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
        if matches!(&request, JsonRpcRequestEnum::Single(single) if single.method == "server.ping") {
            return self.route_ping(&request).await;
        }

        let deadline = now_float() + budget.as_secs_f64();
        let opened = OpenedByCall::default();
        let mut failures = Failures::default();
        let result = self.route_inner(&request, deadline, &opened, &mut failures).await;
        self.release_opened(&opened);
        match result {
            Some(answer) => Ok(answer),
            None => {
                let note = if secs_left(deadline).is_zero() {
                    format!(" (the call's time budget of {:.0} s is used up)", budget.as_secs_f64())
                } else {
                    String::new()
                };
                Err(failures.describe(&self.ticker, &note))
            },
        }
    }

    async fn route_inner(
        self: &Arc<Self>,
        request: &JsonRpcRequestEnum,
        deadline: f64,
        opened: &OpenedByCall,
        failures: &mut Failures,
    ) -> Option<(JsonRpcRemoteAddr, JsonRpcResponseEnum)> {
        let timings = &self.timings;
        let width = self.ceiling();
        let retry_rounds = |servers: usize| (servers + width - 1) / width;
        let maintained = self.maintained_slots();
        let usable_at_start = self.usable_count();

        // Item 1: the maintained connections, one at a time in priority order.
        let mut tried: Vec<usize> = Vec::new();
        for (i, slot) in maintained.iter().enumerate() {
            let remaining = secs_left(deadline);
            if remaining.is_zero() {
                return None;
            }
            let Some(session) = slot.live_session() else { continue };
            let rounds_left = (maintained.len() - i) + retry_rounds(usable_at_start);
            let round = round_budget(
                remaining,
                rounds_left,
                slot.establish_timeout + timings.request,
                timings,
            );
            tried.push(slot.rank);
            let attempt_timeout = round.min(timings.request).min(remaining);
            match self.request_on(slot, &session, request, attempt_timeout).await {
                Ok(response) => return Some((JsonRpcRemoteAddr(slot.addr.clone()), response)),
                Err(e) => failures.record(&slot.addr, e),
            }
        }

        // Item 2: every server of the pool, `width` at a time; the servers not yet tried come first.
        let usable = self.usable_slots();
        let (untried, retried): (Vec<_>, Vec<_>) = usable.into_iter().partition(|slot| !tried.contains(&slot.rank));
        let order: Vec<Arc<ServerSlot>> = untried.into_iter().chain(retried).collect();
        let groups: Vec<&[Arc<ServerSlot>]> = order.chunks(width).collect();
        for (i, group) in groups.iter().enumerate() {
            let remaining = secs_left(deadline);
            if remaining.is_zero() {
                return None;
            }
            let cap = group
                .iter()
                .map(|slot| slot.establish_timeout)
                .max()
                .unwrap_or(timings.establishment)
                + timings.request;
            let round = round_budget(remaining, groups.len() - i, cap, timings).min(remaining);
            let mut attempts: FuturesUnordered<_> = group
                .iter()
                .map(|slot| async move {
                    let result = self.attempt_with_connect(slot, request, round, opened).await;
                    (slot.clone(), result)
                })
                .collect();
            while let Some((slot, result)) = attempts.next().await {
                match result {
                    Ok(response) => return Some((JsonRpcRemoteAddr(slot.addr.clone()), response)),
                    Err(e) => failures.record(&slot.addr, e),
                }
            }
        }
        None
    }

    /// `server.ping` goes over every maintained connection at once and waits for all of them; a
    /// connection whose ping fails is closed; a ping that fails everywhere is not retried (R38.6.4e
    /// item 4, R38.6.4f item 4).
    async fn route_ping(
        self: &Arc<Self>,
        request: &JsonRpcRequestEnum,
    ) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
        let maintained = self.maintained_slots();
        let pings = maintained.iter().map(|slot| async move {
            let result = match slot.live_session() {
                Some(session) => self.request_on(slot, &session, request, self.timings.request).await,
                None => Err("not connected".to_owned()),
            };
            (slot.addr.clone(), result)
        });
        let mut failures = Failures::default();
        let mut answer = None;
        for (addr, result) in join_all(pings).await {
            match result {
                Ok(response) if answer.is_none() => answer = Some((JsonRpcRemoteAddr(addr), response)),
                Ok(_) => (),
                Err(e) => failures.record(&addr, e),
            }
        }
        answer.ok_or_else(|| failures.describe(&self.ticker, ""))
    }

    /// Pings the maintained connections (R38.6.4e item 4).
    pub(super) async fn ping_maintained(self: &Arc<Self>) {
        let request = JsonRpcRequestEnum::Single(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: self.fresh_request_id(),
            method: "server.ping".into(),
            params: vec![],
        });
        if self.maintained_count() == 0 {
            return;
        }
        if let Err(e) = self.route_ping(&request).await {
            debug!("{}", e);
        }
    }

    /// Routes a request addressed to one named server (R38.6.4g). The version negotiation request never
    /// opens a connection; any other request opens the connection if it is closed and releases it
    /// afterwards when the server is not maintained.
    pub(super) async fn route_directed(
        self: &Arc<Self>,
        request: JsonRpcRequestEnum,
        to_addr: &str,
    ) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
        let Some(slot) = self.slot_by_addr(to_addr) else {
            return Err(format!("{}: unknown Electrum server {}", self.ticker, to_addr));
        };
        let failure = |error: String| format!("{}: Electrum server {}: {}", self.ticker, to_addr, error);
        if let Some(reason) = slot.removed_reason() {
            return Err(failure(format!("the server was removed from the pool: {}", reason)));
        }
        let negotiation = matches!(&request, JsonRpcRequestEnum::Single(single) if single.method == "server.version");
        if negotiation {
            let session = slot
                .live_session()
                .ok_or_else(|| failure("the server is not connected".to_owned()))?;
            return self
                .request_on(&slot, &session, &request, self.timings.request)
                .await
                .map(|response| (JsonRpcRemoteAddr(slot.addr.clone()), response))
                .map_err(failure);
        }
        let opened = OpenedByCall::default();
        let result = async {
            let (session, _) = self.ensure_connected(&slot, Some(&opened)).await?;
            self.request_on(&slot, &session, &request, self.timings.request).await
        }
        .await;
        self.release_opened(&opened);
        result
            .map(|response| (JsonRpcRemoteAddr(slot.addr.clone()), response))
            .map_err(failure)
    }
}

/// Closes the session unless defused; used while a connection is being established.
struct CloseOnDrop(Option<Arc<Session>>);

impl CloseOnDrop {
    fn defuse(mut self) { self.0 = None; }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.close();
        }
    }
}

async fn maintenance_loop(client: Weak<ElectrumClientImpl>, mut trigger_rx: futures_mpsc::UnboundedReceiver<Wake>) {
    // The first pass runs at activation, at once.
    let mut next_pass_at = 0.;
    loop {
        let wait = (next_pass_at - now_float()).max(0.);
        let mut pass_due = match select_future(Box::pin(Timer::sleep(wait)), Box::pin(trigger_rx.next())).await {
            EitherFuture::Left(_) => true,
            EitherFuture::Right((None, _)) => return,
            EitherFuture::Right((Some(wake), _)) => wake == Wake::Pass,
        };
        while let Ok(wake) = trigger_rx.try_recv() {
            pass_due |= wake == Wake::Pass;
        }
        let Some(client) = client.upgrade() else { return };
        let pool = client.pool.clone();
        if pass_due {
            pool.run_pass().await;
            next_pass_at = now_float() + pool.next_pass_delay().as_secs_f64();
        }
        // The subscriptions are placed over a maintained connection (R38.6.4h); while there is none,
        // the request stays pending until a pass has made one.
        if pool.maintained_count() > 0 && pool.take_resubscribe_pending() {
            let client = ElectrumClient(client);
            if !client.resubscribe_watched_scripthashes().await {
                pool.resubscribe_pending.store(true, AtomicOrdering::Relaxed);
                next_pass_at = next_pass_at.min(now_float() + pool.timings.short_repass.as_secs_f64());
            }
        }
    }
}

async fn ping_loop(pool: Weak<PoolShared>) {
    loop {
        let interval = match pool.upgrade() {
            Some(pool) => pool.timings.ping_interval,
            None => return,
        };
        Timer::sleep(interval.as_secs_f64()).await;
        match pool.upgrade() {
            Some(pool) => pool.ping_maintained().await,
            None => return,
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn secs(s: u64) -> Duration { Duration::from_secs(s) }

    /// R38.6.4j item 2: the shares of *B* − 5 s never add up to more than that, and no round gets less than 5 s.
    #[test]
    fn round_budget_table_for_one_to_eight_rounds() {
        let timings = ElectrumPoolTimings::default();
        let cap = timings.establishment + timings.request;
        for rounds in 1..=8usize {
            let round = round_budget(timings.call_budget, rounds, cap, &timings);
            assert!(round >= timings.round_floor, "{rounds} rounds");
            assert!(round <= cap, "{rounds} rounds");
            let total = round * rounds as u32;
            assert!(
                total <= timings.call_budget - timings.budget_reserve,
                "{rounds} rounds use {total:?}"
            );
        }
        // With one round to make, the reference figures apply unchanged.
        assert_eq!(
            round_budget(timings.call_budget, 1, cap, &timings),
            secs(40).min(secs(55))
        );
        // Table values for B = 60 s: (60 - 5) / n, capped at 40 s.
        let expected_ms = [40_000, 27_500, 18_333, 13_750, 11_000, 9_166, 7_857, 6_875];
        for (rounds, expected) in (1..=8usize).zip(expected_ms) {
            let round = round_budget(timings.call_budget, rounds, cap, &timings);
            assert_eq!(round.as_millis(), expected, "{rounds} rounds");
        }
    }

    #[test]
    fn round_budget_floor_and_tight_budgets() {
        let timings = ElectrumPoolTimings::default();
        let cap = secs(40);
        // Many rounds cannot push a round below the floor.
        assert_eq!(round_budget(secs(60), 40, cap, &timings), timings.round_floor);
        // A budget smaller than the reserve still gives the floor.
        assert_eq!(round_budget(secs(3), 2, cap, &timings), timings.round_floor);
        // A caller's tighter budget of 20 s over four rounds.
        assert_eq!(round_budget(secs(20), 4, cap, &timings), timings.round_floor);
        // No rounds left is treated as one.
        assert_eq!(round_budget(secs(60), 0, cap, &timings), secs(40));
    }

    /// The figures of the chapter are pinned (R38.6.4c, R38.6.4d, R38.6.4e, R38.6.4j).
    #[test]
    fn production_timings_are_the_chapter_figures() {
        let t = ElectrumPoolTimings::default();
        assert_eq!(t.establishment, secs(20));
        assert_eq!(t.request, secs(20));
        assert_eq!(t.idle_cutoff, secs(60));
        assert_eq!(t.ping_interval, secs(30));
        assert_eq!(t.maintenance_interval, secs(300));
        assert_eq!(t.short_repass, secs(1));
        assert_eq!(t.suspension_base, secs(20));
        assert_eq!(t.suspension_cap, secs(12 * 3600));
        assert_eq!(t.call_budget, secs(60));
        assert_eq!(t.budget_reserve, secs(5));
        assert_eq!(t.round_floor, secs(5));
        let options = ElectrumPoolOptions::default();
        assert_eq!(options.min_connected, 1);
        assert_eq!(options.max_connected, None);
    }

    /// T38.6.4.4: 20 s, 40 s, 80 s ... up to the cap of 12 hours; a success resets the doubling.
    #[test]
    fn suspension_doubles_up_to_the_cap_and_a_success_resets_it() {
        let timings = ElectrumPoolTimings::default();
        let mut suspension = Suspension::default();
        let mut now = 1_000_000.;
        assert!(!suspension.is_suspended(now));
        let mut lengths = Vec::new();
        for _ in 0..20 {
            let secs = suspension.on_failure(now, &timings);
            assert!(suspension.is_suspended(now));
            assert!(suspension.is_suspended(now + secs - 0.5));
            assert!(!suspension.is_suspended(now + secs));
            lengths.push(secs as u64);
            // The clock stub jumps to the moment the suspension lapses.
            now += secs;
        }
        assert_eq!(&lengths[..5], &[20, 40, 80, 160, 320]);
        assert!(lengths.iter().all(|secs| *secs <= 12 * 3600));
        assert_eq!(*lengths.last().unwrap(), 12 * 3600, "the cap is reached and held");
        assert!(lengths.windows(2).all(|pair| pair[0] <= pair[1]));

        suspension.on_success();
        assert!(!suspension.is_suspended(now));
        assert_eq!(
            suspension.on_failure(now, &timings) as u64,
            20,
            "the next suspension is again 20 s"
        );
    }

    /// A huge number of failures does not overflow the doubling.
    #[test]
    fn suspension_survives_a_very_long_failure_streak() {
        let timings = ElectrumPoolTimings::default();
        let mut suspension = Suspension::default();
        for _ in 0..10_000 {
            suspension.on_failure(0., &timings);
        }
        assert_eq!(suspension.on_failure(0., &timings) as u64, 12 * 3600);
    }
}
