#[cfg(test)] use super::electrum_pool::ServerStateSnapshot;
use super::electrum_pool::{ElectrumPoolOptions, ElectrumServersView, PoolShared};
use super::*;
use crate::utxo::header_layout_fallback::HeaderReadPolicy;
use crate::utxo::header_time_only::read_window_times;
#[cfg(not(target_arch = "wasm32"))]
use common::dns_resolver::connect_tcp;
use futures::channel::mpsc as futures_mpsc;
use script::Script;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex as StdMutex;

// Response/request data types and the `electrum_script_hash` helper live in
// the sibling `electrum_types` module (carved out via P13.5 follow-up to keep
// this file focused on the connection/transport layer). They are re-exported
// from `rpc_clients::*` so existing call sites are unaffected.

/// Electrum client configuration
#[allow(clippy::upper_case_acronyms)]
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Serialize)]
enum ElectrumConfig {
    TCP,
    SSL {
        dns_name: String,
        skip_validation: bool,
        /// The pinned keys, as SHA-256 digests of the `SubjectPublicKeyInfo` (CRD ch.38 R38.6.4n). When there are any, they
        /// decide and `skip_validation` has no further effect.
        spki_pins: Vec<[u8; 32]>,
    },
}

/// Electrum client configuration
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, Serialize)]
enum ElectrumConfig {
    WS,
    WSS,
}

/// What it takes to connect one listed server, parsed and checked once when the server joins the pool.
pub(crate) struct ServerSpec {
    /// `host:port`, the only form in which the server is named (CRD ch.38 R38.6.9a).
    addr: String,
    config: ElectrumConfig,
    #[cfg(not(target_arch = "wasm32"))]
    host: String,
    #[cfg(not(target_arch = "wasm32"))]
    port: u16,
    /// The WebSocket URL, scheme included.
    #[cfg(target_arch = "wasm32")]
    ws_url: String,
}

/// The debug form shows `host:port` only; the WebSocket URL of the browser build is not part of it.
impl fmt::Debug for ServerSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerSpec")
            .field("addr", &self.addr)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Splits `host:port` (an IPv6 literal is written `[::1]:port`) into the host and the port.
///
/// The address must be exactly a host and a port: a scheme, userinfo, path, query or fragment makes it unusable.
/// No error text echoes the address (CRD ch.38 R38.6.9a).
#[cfg(not(target_arch = "wasm32"))]
fn parse_host_port(addr: &str) -> Result<(String, u16), String> {
    const NOT_HOST_PORT: &str = "the address is not a host:port address";
    if electrum_host_port(addr).as_deref() != Some(addr) {
        return Err(NOT_HOST_PORT.to_owned());
    }
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| NOT_HOST_PORT.to_owned())?;
    let host = match host.strip_prefix('[').and_then(|host| host.strip_suffix(']')) {
        Some(ipv6) => ipv6,
        None if host.contains(':') => return Err(NOT_HOST_PORT.to_owned()),
        None => host,
    };
    if host.is_empty() {
        return Err("the address has no host".to_owned());
    }
    let port = port
        .parse::<u16>()
        .map_err(|e| format!("the address has no valid port: {}", e))?;
    Ok((host.to_owned(), port))
}

/// How many keys one server may pin (CRD ch.38 R38.6.4n item 1).
#[cfg(not(target_arch = "wasm32"))]
const MAX_SPKI_PINS: usize = 4;

/// The text of a malformed `spki_sha256` (CRD ch.38 R38.6.4n item 5). The configured value is not echoed.
#[cfg(not(target_arch = "wasm32"))]
const SPKI_MALFORMED: &str = "spki_sha256 is not a 64-digit hexadecimal SHA-256 digest";

/// The text of a `spki_sha256` on a server without TLS (CRD ch.38 R38.6.4n item 5).
#[cfg(not(target_arch = "wasm32"))]
const SPKI_NEEDS_SSL: &str = "spki_sha256 needs protocol SSL";

/// The text of a `spki_sha256` on a browser build (CRD ch.38 R38.6.4n item 6).
#[cfg(target_arch = "wasm32")]
const SPKI_IN_BROWSER: &str = "spki_sha256 is not supported in a browser";

/// Reads the value of `spki_sha256`: a string, or an array of 1 to 4 strings, each 64 hexadecimal digits in either case
/// with colons between byte pairs accepted and ignored (CRD ch.38 R38.6.4n items 1 and 5).
#[cfg(not(target_arch = "wasm32"))]
fn parse_spki_pins(value: &Json) -> Result<Vec<[u8; 32]>, String> {
    let entries: Vec<&Json> = match value {
        Json::Array(entries) if (1..=MAX_SPKI_PINS).contains(&entries.len()) => entries.iter().collect(),
        Json::Array(_) => return Err(SPKI_MALFORMED.to_owned()),
        single => vec![single],
    };
    entries.into_iter().map(parse_spki_pin).collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_spki_pin(value: &Json) -> Result<[u8; 32], String> {
    let text = value.as_str().ok_or_else(|| SPKI_MALFORMED.to_owned())?;
    let digits: String = text.chars().filter(|c| *c != ':').collect();
    let bytes = hex::decode(digits).map_err(|_| SPKI_MALFORMED.to_owned())?;
    <[u8; 32]>::try_from(bytes).map_err(|_| SPKI_MALFORMED.to_owned())
}

/// What the TLS settings of a listed server call for at activation (CRD ch.38 R38.6.4m item 6, R38.6.4n item 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TlsNotice {
    /// `disable_cert_verification` is set and no key is pinned: the connection is encrypted, the server is not
    /// authenticated.
    NotAuthenticated,
    /// The server's key is pinned by `spki_sha256`.
    Pinned,
}

impl ServerSpec {
    /// The notice a server of this spec gets when it joins the pool. A `TCP` server and a browser build get none.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn tls_notice(&self) -> Option<TlsNotice> {
        match &self.config {
            ElectrumConfig::SSL { spki_pins, .. } if !spki_pins.is_empty() => Some(TlsNotice::Pinned),
            ElectrumConfig::SSL {
                skip_validation: true, ..
            } => Some(TlsNotice::NotAuthenticated),
            ElectrumConfig::SSL { .. } | ElectrumConfig::TCP => None,
        }
    }

    /// A browser does the TLS of `WSS` and `disable_cert_verification` has no effect there (CRD ch.38 R38.6.4m item 9).
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn tls_notice(&self) -> Option<TlsNotice> { None }
}

/// Attempts to process the request (parse url, etc) and build up what is needed to connect the server.
/// An error means that retrying cannot cure it (CRD ch.38 R38.6.4d item 3). No error text echoes the configured
/// address (CRD ch.38 R38.6.9a).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn server_spec(req: &ElectrumRpcRequest) -> Result<ServerSpec, String> {
    let (host, port) = parse_host_port(&req.url)?;
    let config = match req.protocol {
        ElectrumProtocol::TCP => {
            // A pin that cannot be enforced must not look enforced; a malformed one is reported as such first.
            if let Some(value) = &req.spki_sha256 {
                parse_spki_pins(value)?;
                return Err(SPKI_NEEDS_SSL.to_owned());
            }
            ElectrumConfig::TCP
        },
        ElectrumProtocol::SSL => {
            let spki_pins = match &req.spki_sha256 {
                Some(value) => parse_spki_pins(value)?,
                None => Vec::new(),
            };
            let uri: Uri = try_s!(req.url.parse());
            let host = uri.host().ok_or(ERRL!("Couldn't retrieve a host from the address"))?;

            // check the dns name (IP literals are rejected here, as they always were)
            try_s!(DnsName::try_from(host));

            ElectrumConfig::SSL {
                dns_name: host.into(),
                skip_validation: req.disable_cert_verification,
                spki_pins,
            }
        },
        // Not a missing feature: 'ws'/'wss' are the browser/WASM transport, the same
        // way 'TCP'/'SSL' are rejected by the WASM client below.
        ElectrumProtocol::WS | ElectrumProtocol::WSS => {
            return ERR!("'ws' and 'wss' are browser-only Electrum protocols and cannot be used by a native node. Use 'TCP' or 'SSL'")
        },
    };

    Ok(ServerSpec {
        // The address is exactly `host:port` (checked above), so it names the server in logs.
        addr: req.url.clone(),
        config,
        host,
        port,
    })
}

/// Attempts to process the request (parse url, etc) and build up what is needed to connect the server.
/// An error means that retrying cannot cure it (CRD ch.38 R38.6.4d item 3). No error text echoes the configured
/// address (CRD ch.38 R38.6.9a).
#[cfg(target_arch = "wasm32")]
pub(crate) fn server_spec(req: &ElectrumRpcRequest) -> Result<ServerSpec, String> {
    // The browser does the TLS of `WSS` and cannot be told to pin (CRD ch.38 R38.6.4n item 6).
    if req.spki_sha256.is_some() {
        return Err(SPKI_IN_BROWSER.to_owned());
    }
    let uri: Uri = try_s!(req.url.parse());

    if uri.scheme().is_some() {
        return ERR!(
            "There has not to be a scheme in the url. \
            'ws://' scheme is used by default. \
            Consider using 'protocol: \"WSS\"' in the electrum request to switch to the 'wss://' scheme."
        );
    }

    let (config, ws_url) = match req.protocol {
        ElectrumProtocol::WS => (ElectrumConfig::WS, format!("ws://{}", req.url)),
        ElectrumProtocol::WSS => (ElectrumConfig::WSS, format!("wss://{}", req.url)),
        ElectrumProtocol::TCP | ElectrumProtocol::SSL => {
            return ERR!("'TCP' and 'SSL' are not supported in a browser. Please use 'WS' or 'WSS' protocols");
        },
    };

    Ok(ServerSpec {
        // Only the host and the port name the server; the WebSocket URL keeps the rest of the address.
        addr: electrum_host_port(&req.url).ok_or_else(|| "the address has no usable host".to_owned())?,
        config,
        ws_url,
    })
}

/// Tells the pool that the connection with the given id ended.
pub(crate) type SessionEnded = Arc<dyn Fn(u64) + Send + Sync>;

/// Tells the pool the height of a header notification that arrived over a connection (CRD ch.38 R38.6.4l item 2).
pub(crate) type HeaderNotified = Arc<dyn Fn(u64) + Send + Sync>;

/// No tip is known yet.
const NO_TIP: u64 = u64::MAX;
/// No moment is known yet.
const NO_TIME_MS: u64 = u64::MAX;

/// What a connection has learned about the chain tip: the height of the last header it carried, and whether
/// a header request was answered over it, which makes the server push a notification for each new tip
/// (CRD ch.38 R38.6.4l items 2 and 6, D38.6.4l.4), and when the first answer and the last notification came
/// (R38.6.4o item 4).
pub(crate) struct HeaderTrack {
    notifying: AtomicBool,
    tip: AtomicU64,
    /// When the first header answer came, in milliseconds since the epoch.
    answered_at_ms: AtomicU64,
    /// When the last header notification came, in milliseconds since the epoch.
    notified_at_ms: AtomicU64,
    on_notification: HeaderNotified,
}

impl HeaderTrack {
    fn new(on_notification: HeaderNotified) -> HeaderTrack {
        HeaderTrack {
            notifying: AtomicBool::new(false),
            tip: AtomicU64::new(NO_TIP),
            answered_at_ms: AtomicU64::new(NO_TIME_MS),
            notified_at_ms: AtomicU64::new(NO_TIME_MS),
            on_notification,
        }
    }

    /// The answer to a header request: the server now pushes the tips that follow. The first answer starts the
    /// silence a stuck connection is counted from; a later one (a block-count read over the connection) only moves
    /// the height.
    fn note_answer(&self, height: u64) {
        let first = self.tip.swap(height, AtomicOrdering::Relaxed) == NO_TIP;
        if first {
            self.answered_at_ms.store(now_ms(), AtomicOrdering::Relaxed);
        }
        self.notifying.store(true, AtomicOrdering::Relaxed);
    }

    /// A header notification: the connection's height moves, and the pool is told.
    fn note_notification(&self, height: u64) {
        self.note_answer(height);
        self.notified_at_ms.store(now_ms(), AtomicOrdering::Relaxed);
        (self.on_notification)(height);
    }

    /// When the first header answer came (seconds since the epoch).
    pub(crate) fn answered_at(&self) -> Option<f64> { Self::seconds(self.answered_at_ms.load(AtomicOrdering::Relaxed)) }

    /// When the last header notification came (seconds since the epoch).
    pub(crate) fn notified_at(&self) -> Option<f64> { Self::seconds(self.notified_at_ms.load(AtomicOrdering::Relaxed)) }

    fn seconds(ms: u64) -> Option<f64> { (ms != NO_TIME_MS).then(|| ms as f64 / 1000.) }

    /// The height of the last header the connection carried.
    pub(crate) fn tip(&self) -> Option<u64> {
        let tip = self.tip.load(AtomicOrdering::Relaxed);
        (tip != NO_TIP).then_some(tip)
    }

    /// Whether header notifications can arrive over the connection.
    pub(crate) fn is_notifying(&self) -> bool { self.notifying.load(AtomicOrdering::Relaxed) }
}

/// One established connection to an Electrum server: the sender of the writing task, the answers
/// waiting for a response, and the handle that ends the background tasks.
///
/// A session never reconnects; when it ends the pool decides what happens to its server.
pub(crate) struct Session {
    id: u64,
    addr: String,
    tx: StdMutex<mpsc::Sender<Vec<u8>>>,
    /// Responses are stored here
    responses: JsonRpcPendingRequestsShared,
    closed: Arc<AtomicBool>,
    /// The chain tip as this connection has seen it.
    headers: Arc<HeaderTrack>,
    /// Ends the background tasks of the session.
    shutdown_tx: StdMutex<Option<oneshot::Sender<()>>>,
}

impl Session {
    pub(crate) fn id(&self) -> u64 { self.id }

    pub(crate) fn addr(&self) -> &str { &self.addr }

    pub(crate) fn is_closed(&self) -> bool { self.closed.load(AtomicOrdering::Relaxed) }

    pub(crate) fn headers(&self) -> &HeaderTrack { &self.headers }

    /// Ends the session. The background tasks stop and the pool is told through [`SessionEnded`].
    pub(crate) fn close(&self) {
        self.closed.store(true, AtomicOrdering::Relaxed);
        let shutdown_tx = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(shutdown_tx) = shutdown_tx {
            // The tasks may have ended already.
            let _ = shutdown_tx.send(());
        }
    }

    /// Sends `request` and waits for its response for at most `timeout`.
    pub(crate) async fn request(
        &self,
        request: &JsonRpcRequestEnum,
        timeout: Duration,
    ) -> Result<JsonRpcResponseEnum, String> {
        if self.is_closed() {
            return Err("the connection is closed".to_owned());
        }
        let json = json::to_string(request).map_err(|e| e.to_string())?;
        // Electrum request and responses must end with \n
        // https://electrumx.readthedocs.io/en/latest/protocol-basics.html#message-stream
        #[cfg(not(target_arch = "wasm32"))]
        let json = format!("{}\n", json);

        let id = request.rpc_id();
        let (req_tx, resp_rx) = async_oneshot::channel();
        self.responses.lock().await.insert(id.clone(), req_tx);
        let tx = self.tx.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let exchange = async move {
            tx.send(json.into_bytes())
                .compat()
                .await
                .map_err(|_| "the connection is closed".to_owned())?;
            resp_rx
                .await
                .map_err(|_| "the connection ended before the server answered".to_owned())
        };
        let started = now_ms();
        let outcome = exchange.boxed().timeout(timeout).await;
        // Every request message of the session passes here: one debug line per message, with the method names
        // only (never params, script hashes, addresses or payloads).
        let result = match &outcome {
            Ok(Ok(_)) => now_ms().saturating_sub(started).to_string(),
            Ok(Err(_)) => "failed".to_owned(),
            Err(_) => "timeout".to_owned(),
        };
        common::log::debug!("electrum {} {} {}", self.addr(), request_methods(request), result);
        let error = match outcome {
            Ok(Ok(response)) => {
                self.note_header_answer(request, &response);
                return Ok(response);
            },
            Ok(Err(error)) => error,
            Err(_) => format!("no answer within {:.1} s", timeout.as_secs_f64()),
        };
        self.responses.lock().await.remove(&id);
        Err(error)
    }
}

impl Session {
    /// An answer to a lone header request (`blockchain.headers.subscribe`) tells the connection's height.
    fn note_header_answer(&self, request: &JsonRpcRequestEnum, response: &JsonRpcResponseEnum) {
        if let (JsonRpcRequestEnum::Single(request), JsonRpcResponseEnum::Single(response)) = (request, response) {
            if request.method == BLOCKCHAIN_HEADERS_SUB_ID && response.error.is_null() {
                if let Some(height) = response.result["height"].as_u64() {
                    self.headers.note_answer(height);
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) { self.close(); }
}
#[derive(Debug)]
struct ConcurrentRequestState<V> {
    is_running: bool,
    subscribers: Vec<RpcReqSub<V>>,
}

impl<V> ConcurrentRequestState<V> {
    fn new() -> Self {
        ConcurrentRequestState {
            is_running: false,
            subscribers: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ConcurrentRequestMap<K, V> {
    inner: AsyncMutex<HashMap<K, ConcurrentRequestState<V>>>,
}

impl<K, V> Default for ConcurrentRequestMap<K, V> {
    fn default() -> Self {
        ConcurrentRequestMap {
            inner: AsyncMutex::new(HashMap::new()),
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash, V: Clone> ConcurrentRequestMap<K, V> {
    pub fn new() -> ConcurrentRequestMap<K, V> { ConcurrentRequestMap::default() }

    pub(crate) async fn wrap_request(&self, request_arg: K, request_fut: RpcRes<V>) -> Result<V, JsonRpcError> {
        let mut map = self.inner.lock().await;
        let state = map
            .entry(request_arg.clone())
            .or_insert_with(ConcurrentRequestState::new);
        if state.is_running {
            let (tx, rx) = async_oneshot::channel();
            state.subscribers.push(tx);
            // drop here to avoid holding the lock during await
            drop(map);
            rx.await.unwrap()
        } else {
            // drop here to avoid holding the lock during await
            drop(map);
            let request_res = request_fut.compat().await;
            let mut map = self.inner.lock().await;
            let state = map.get_mut(&request_arg).unwrap();
            for sub in state.subscribers.drain(..) {
                if sub.send(request_res.clone()).is_err() {
                    warn!("subscriber is dropped");
                }
            }
            state.is_running = false;
            request_res
        }
    }
}

#[derive(Debug)]
pub struct ElectrumClientImpl {
    coin_ticker: String,
    /// The servers listed at activation and the connections to them (CRD ch.38 R38.6.4b).
    pub(super) pool: Arc<PoolShared>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    protocol_version: OrdRange<f32>,
    get_balance_concurrent_map: ConcurrentRequestMap<String, ElectrumBalance>,
    list_unspent_concurrent_map: ConcurrentRequestMap<String, Vec<ElectrumUnspent>>,
    /// Concurrent-request map of the R38.6.3a balance batch, keyed by the P2PKH and P2PK script hashes.
    get_balance_with_p2pk_concurrent_map:
        ConcurrentRequestMap<(String, String), (ElectrumBalance, Vec<ElectrumUnspent>)>,
    /// Concurrent-request map of the R38.6.3a unspent-list batch, keyed by the P2PKH and P2PK script hashes.
    list_unspent_with_p2pk_concurrent_map:
        ConcurrentRequestMap<(String, String), (Vec<ElectrumUnspent>, Vec<ElectrumUnspent>)>,
    /// Script hashes this client has asked its servers to watch (R38.6.5).
    ///
    /// Held so every subscription can be placed again over another connection
    /// (R38.6.4h): a subscription belongs to one TCP session, so a dropped
    /// connection silently loses it. Nothing may assume a subscription
    /// survived.
    watched_script_hashes: AsyncMutex<HashSet<String>>,
}

impl fmt::Debug for PoolShared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "PoolShared") }
}

/// Drops the duplicates the server can return in an unspent list, keeping the first occurrence.
fn dedup_unspents(unspents: Vec<ElectrumUnspent>) -> Vec<ElectrumUnspent> {
    unspents
        .into_iter()
        .unique_by(|unspent| (unspent.tx_hash, unspent.tx_pos))
        .collect()
}

async fn electrum_request_multi(
    client: ElectrumClient,
    request: JsonRpcRequestEnum,
) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
    let budget = client.pool.timings.call_budget;
    client.pool.route(request, budget).await
}

async fn electrum_request_to(
    client: ElectrumClient,
    request: JsonRpcRequestEnum,
    to_addr: String,
) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
    client.pool.route_directed(request, &to_addr).await
}

impl ElectrumClientImpl {
    /// Replaces the pool settings. To be called before the first server is added.
    pub fn configure_pool(&mut self, options: ElectrumPoolOptions) {
        self.pool = Arc::new(PoolShared::new(
            self.coin_ticker.clone(),
            options,
            self.protocol_version.clone(),
            self.event_handlers.clone(),
        ));
    }

    /// Adds a server to the pool. The position in the call order is the server's priority; see [`Self::add_servers`]. A server
    /// this build cannot use stays in the pool as removed and the reason is returned.
    pub fn add_server(&self, req: &ElectrumRpcRequest) -> Result<(), String> { self.pool.add_server(req) }

    /// Adds the servers, given in the listed order, ranked by the pool's order: drawn once at random, or
    /// the listed order (CRD ch.38 R38.6.4b-R). Returns the servers this build cannot use, with the reason.
    pub fn add_servers(&self, reqs: &[ElectrumRpcRequest]) -> Vec<(String, String)> { self.pool.add_servers(reqs) }

    /// The activation probe (CRD ch.38 R38.6.4k-R): connects every server of the pool at once and
    /// returns when the first connection is established, or fails with the failure of each server.
    /// To be called after the servers are added and before [`Self::start_pool`].
    pub async fn activation_probe(self: &Arc<Self>) -> Result<(), String> { self.pool.activation_probe().await }

    /// Starts the background maintenance of the pool: the first pass connects the servers in the
    /// background and later passes keep the maintained set (CRD ch.38 R38.6.4c). Nothing waits for it.
    /// With `spawn_ping` the maintained connections are pinged on a regular cadence.
    pub fn start_pool(self: &Arc<Self>, spawn_ping: bool) { self.pool.start(Arc::downgrade(self), spawn_ping); }

    /// Check if one of the connections is open.
    pub async fn is_connected(&self) -> bool { self.pool.has_open_connection() }

    /// The number of servers in the pool, those removed from it excluded.
    pub async fn count_connections(&self) -> usize { self.pool.usable_count() }

    /// The size of the maintained set.
    pub async fn count_connected(&self) -> usize { self.pool.maintained_count() }

    /// The addresses of the servers in the pool, in priority order (CRD ch.38 R38.6.4b), those removed
    /// from it excluded. These are the names a directed request (R38.6.4g) is addressed to.
    pub fn server_addrs(&self) -> Vec<String> { self.pool.usable_addrs() }

    /// Get available protocol versions.
    pub fn protocol_version(&self) -> &OrdRange<f32> { &self.protocol_version }

    /// The pool as the `electrum_servers` request reports it (CRD ch.38 R38.6.4o item 10). It sends nothing to any
    /// server and changes nothing.
    pub fn servers_view(&self) -> ElectrumServersView { self.pool.servers_view() }

    /// What a test sees of each server of the pool.
    #[cfg(test)]
    pub(crate) fn pool_snapshot(&self) -> Vec<ServerStateSnapshot> { self.pool.snapshot() }
}

#[derive(Clone, Debug)]
pub struct ElectrumClient(pub Arc<ElectrumClientImpl>);
impl Deref for ElectrumClient {
    type Target = ElectrumClientImpl;
    fn deref(&self) -> &ElectrumClientImpl { &*self.0 }
}

pub(super) const BLOCKCHAIN_HEADERS_SUB_ID: &str = "blockchain.headers.subscribe";
pub(super) const BLOCKCHAIN_SCRIPTHASH_SUB_ID: &str = "blockchain.scripthash.subscribe";
pub(super) const BLOCKCHAIN_CONTRACT_EVENT_SUB_ID: &str = "blockchain.contract.event.subscribe";

/// Registry key for a Qtum contract-event subscription.
///
/// A QRC20 balance lives in contract storage rather than in the address's UTXO
/// set, so a token transfer need not change the QTUM address's script hash and
/// cannot be observed through a script-hash subscription. Contract events are
/// therefore watched per (address, contract, topic) triple rather than per
/// address, and share the same registry by carrying a distinct key.
pub fn contract_event_key(address_hash160: &str, contract_addr: &str, topic: &str) -> String {
    format!("contract:{address_hash160}:{contract_addr}:{topic}")
}

// Script hash -> the party that asked for it to be watched.
//
// Keyed globally rather than per client because a script hash already
// identifies exactly one address of one coin, and because the notification
// arrives deep inside a connection read loop that holds no client handle.
// Threading one through every connection would buy nothing the key does not
// already give us.
lazy_static! {
    static ref SCRIPTHASH_WATCHERS: std::sync::Mutex<HashMap<String, Vec<futures_mpsc::UnboundedSender<String>>>> =
        std::sync::Mutex::new(HashMap::new());
}

/// Register `sender` to be woken whenever `script_hash` changes status.
///
/// The receiver is a wake signal, not a data feed: it carries the script hash
/// only so the consumer knows which address to re-read. Balances are always
/// re-read authoritatively, never inferred from the notification.
///
/// More than one consumer may care about the same hash -- the balance streamer
/// and the transaction-history loop both watch a coin's address -- so watchers
/// accumulate rather than replace. Storing one sender per hash would let
/// whichever registered last silently starve the other.
pub fn watch_scripthash(script_hash: String, sender: futures_mpsc::UnboundedSender<String>) {
    SCRIPTHASH_WATCHERS
        .lock()
        .expect("scripthash watcher registry poisoned")
        .entry(script_hash)
        .or_default()
        .push(sender);
}

/// Drop any watcher of `script_hash` whose receiver is gone, removing the entry
/// once none remain. Safe to call for an unregistered hash.
///
/// Consumers signal departure by dropping their receiver; this is the sweep
/// that reclaims the registration. It deliberately does not drop live watchers,
/// because one consumer going away must not silence the others.
pub fn unwatch_scripthash(script_hash: &str) {
    let mut watchers = match SCRIPTHASH_WATCHERS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(senders) = watchers.get_mut(script_hash) {
        senders.retain(|sender| !sender.is_closed());
        if senders.is_empty() {
            watchers.remove(script_hash);
        }
    }
}

/// Deliver a status change to whoever registered for it.
///
/// A closed receiver means the consumer went away, so the registration is
/// dropped rather than retried.
fn notify_scripthash_change(script_hash: &str) {
    let mut watchers = match SCRIPTHASH_WATCHERS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(senders) = watchers.get_mut(script_hash) {
        // Departed consumers are pruned as a side effect, so a registration
        // cannot outlive its receiver for the life of the process.
        senders.retain(|sender| sender.unbounded_send(script_hash.to_owned()).is_ok());
        if senders.is_empty() {
            watchers.remove(script_hash);
        }
    }
}

impl UtxoJsonRpcClientInfo for ElectrumClient {
    fn coin_name(&self) -> &str { self.coin_ticker.as_str() }
}

impl JsonRpcClient for ElectrumClient {
    fn version(&self) -> &'static str { "2.0" }

    fn next_id(&self) -> String { self.pool.fresh_request_id() }

    fn client_info(&self) -> String { UtxoJsonRpcClientInfo::client_info(self) }

    fn transport(&self, request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        Box::new(electrum_request_multi(self.clone(), request).boxed().compat())
    }
}

impl JsonRpcBatchClient for ElectrumClient {}

impl JsonRpcMultiClient for ElectrumClient {
    fn transport_exact(&self, to_addr: String, request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        Box::new(electrum_request_to(self.clone(), request, to_addr).boxed().compat())
    }
}

// A separate block so the header fetches can be mocked in tests (`mockable` cannot take the async
// methods of the block below).
#[cfg_attr(test, mockable)]
impl ElectrumClient {
    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-block-header
    pub fn blockchain_block_header(&self, height: u64) -> RpcRes<BytesJson> {
        rpc_func!(self, "blockchain.block.header", height)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-block-headers
    pub fn blockchain_block_headers(&self, start_height: u64, count: NonZeroU64) -> RpcRes<ElectrumBlockHeadersRes> {
        rpc_func!(self, "blockchain.block.headers", start_height, count)
    }
}

/// Read-only requests addressed to one named server (CRD ch.38 R38.6.4g): the request opens the server's
/// connection if it is closed and releases it afterwards when the server is not maintained, and it is
/// answered by that server or fails naming it. Used where the answer of a particular server is the
/// point (chain-identity checks of the shielded-coin fallback, CRD ch.39 R39.11.25).
impl ElectrumClient {
    /// `blockchain.block.header` of `server`.
    pub fn blockchain_block_header_from(&self, server: &str, height: u64) -> RpcRes<BytesJson> {
        rpc_func_from!(self, server, "blockchain.block.header", height)
    }

    /// `blockchain.headers.subscribe` of `server`: its tip.
    pub fn blockchain_headers_subscribe_from(&self, server: &str) -> RpcRes<ElectrumBlockHeader> {
        rpc_func_from!(self, server, "blockchain.headers.subscribe")
    }

    /// `blockchain.scripthash.get_history` of `server`.
    pub fn scripthash_get_history_from(&self, server: &str, hash: &str) -> RpcRes<Vec<ElectrumTxHistoryItem>> {
        rpc_func_from!(self, server, "blockchain.scripthash.get_history", hash)
    }

    /// `blockchain.scripthash.listunspent` of `server`, as the server returns it.
    pub fn scripthash_list_unspent_from(&self, server: &str, hash: &str) -> RpcRes<Vec<ElectrumUnspent>> {
        rpc_func_from!(self, server, "blockchain.scripthash.listunspent", hash)
    }

    /// `blockchain.transaction.get` of `server`, the raw transaction bytes.
    pub fn transaction_get_from(&self, server: &str, txid: &H256Json) -> RpcRes<BytesJson> {
        let verbose = false;
        rpc_func_from!(self, server, "blockchain.transaction.get", txid, verbose)
    }

    /// `blockchain.transaction.get` of `server` with the verbose flag: the transaction with its confirmations.
    pub fn transaction_get_verbose_from(&self, server: &str, txid: &H256Json) -> RpcRes<RpcTransaction> {
        let verbose = true;
        rpc_func_from!(self, server, "blockchain.transaction.get", txid, verbose)
    }

    /// The wait for the confirmations of a transaction (CRD ch.38 R38.6.4o item 6). It chooses one server when it
    /// starts (see [`PoolShared::choose_wait_server`]) and sends every transaction request and block-count request
    /// to it as a directed request (R38.6.4g), so that the answer that ends the wait always comes from the server
    /// being polled and never from whichever server a spread read reached. When a request to the server fails in
    /// transport, the wait chooses again, leaving out the servers that failed during this wait while another choice
    /// exists, and goes on with the new server. An error answer of the server is an answer, not a failure of the
    /// server. No request is added: the wait asks as often as it did.
    pub(crate) async fn wait_for_confirmations_on_one_server(
        &self,
        tx_hash: H256Json,
        expiry_height: u32,
        confirmations: u32,
        requires_notarization: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Result<(), String> {
        let mut failed: Vec<String> = Vec::new();
        let mut server = self.pool.choose_wait_server(&failed);
        loop {
            if now_ms() / 1000 > wait_until {
                return ERR!(
                    "Waited too long until {} for transaction {:?} to be confirmed {} times",
                    wait_until,
                    tx_hash,
                    confirmations
                );
            }
            let Some(addr) = server.clone() else {
                error!("No Electrum server to poll for the transaction {:?}, retrying", tx_hash);
                Timer::sleep(check_every as f64).await;
                server = self.pool.choose_wait_server(&failed);
                continue;
            };

            match self.transaction_get_verbose_from(&addr, &tx_hash).compat().await {
                Ok(t) => {
                    let tx_confirmations = if requires_notarization {
                        t.confirmations
                    } else {
                        t.rawconfirmations.unwrap_or(t.confirmations)
                    };
                    if tx_confirmations >= confirmations {
                        return Ok(());
                    } else {
                        info!(
                            "Waiting for tx {:?} confirmations, now {}, required {}, requires_notarization {}",
                            tx_hash, tx_confirmations, confirmations, requires_notarization
                        )
                    }
                },
                Err(e) => {
                    if e.error.is_transport() {
                        failed.push(addr);
                        server = self.pool.choose_wait_server(&failed);
                        // Another server is asked at once; with none left to choose, the wait goes on as before.
                        if server.as_ref().map_or(false, |next| !failed.contains(next)) {
                            continue;
                        }
                    }
                    if expiry_height > 0 {
                        let Some(addr) = server.clone() else {
                            Timer::sleep(check_every as f64).await;
                            continue;
                        };
                        match self.blockchain_headers_subscribe_from(&addr).compat().await {
                            Ok(header) => {
                                let block = header.block_height();
                                if block > expiry_height as u64 {
                                    return ERR!("The transaction {:?} has expired, current block {}", tx_hash, block);
                                }
                            },
                            Err(e) => {
                                error!("Error {} getting block number, retrying in 10 seconds", e);
                                if e.error.is_transport() {
                                    failed.push(addr);
                                    server = self.pool.choose_wait_server(&failed);
                                }
                                Timer::sleep(check_every as f64).await;
                                continue;
                            },
                        }
                    }
                    error!(
                        "Error {:?} getting the transaction {:?}, retrying in 10 seconds",
                        e, tx_hash
                    )
                },
            }

            Timer::sleep(check_every as f64).await;
        }
    }
}

impl ElectrumClient {
    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#server-ping
    pub fn server_ping(&self) -> RpcRes<()> { rpc_func!(self, "server.ping") }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#server-version
    pub fn server_version(
        &self,
        server_address: &str,
        client_name: &str,
        version: &OrdRange<f32>,
    ) -> RpcRes<ElectrumProtocolVersion> {
        let protocol_version: Vec<String> = version.flatten().into_iter().map(|v| format!("{}", v)).collect();
        rpc_func_from!(self, server_address, "server.version", client_name, protocol_version)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
    /// It can return duplicates sometimes: https://github.com/artemii235/SuperNET/issues/269
    /// We should remove them to build valid transactions
    pub fn scripthash_list_unspent(&self, hash: &str) -> RpcRes<Vec<ElectrumUnspent>> {
        let request_fut = Box::new(rpc_func!(self, "blockchain.scripthash.listunspent", hash).and_then(
            move |unspents: Vec<ElectrumUnspent>| {
                let mut map: HashMap<(H256Json, u32), bool> = HashMap::new();
                let unspents = unspents
                    .into_iter()
                    .filter(|unspent| match map.entry((unspent.tx_hash, unspent.tx_pos)) {
                        Entry::Occupied(_) => false,
                        Entry::Vacant(e) => {
                            e.insert(true);
                            true
                        },
                    })
                    .collect();
                Ok(unspents)
            },
        ));
        let arc = self.clone();
        let hash = hash.to_owned();
        let fut = async move { arc.list_unspent_concurrent_map.wrap_request(hash, request_fut).await };
        Box::new(fut.boxed().compat())
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
    /// It can return duplicates sometimes: https://github.com/artemii235/SuperNET/issues/269
    /// We should remove them to build valid transactions.
    /// Please note the function returns `ScriptHashUnspents` elements in the same order in which they were requested.
    pub fn scripthash_list_unspent_batch(&self, hashes: Vec<ElectrumScriptHash>) -> RpcRes<Vec<ScriptHashUnspents>> {
        let requests = hashes
            .iter()
            .map(|hash| rpc_req!(self, "blockchain.scripthash.listunspent", hash));
        Box::new(self.batch_rpc(requests).map(move |unspents: Vec<ScriptHashUnspents>| {
            unspents
                .into_iter()
                .map(|hash_unspents| {
                    hash_unspents
                        .into_iter()
                        .unique_by(|unspent| (unspent.tx_hash, unspent.tx_pos))
                        .collect::<Vec<_>>()
                })
                .collect()
        }))
    }

    /// CRD ch.38 R38.6.3a: the P2PKH balance of `p2pkh_hash` and the P2PK unspents of `p2pk_hash` in ONE
    /// Electrum JSON-RPC batch, that is one request message for the server.
    /// The unspents are de-duplicated exactly as [`ElectrumClient::scripthash_list_unspent`] does.
    pub fn scripthash_get_balance_and_list_unspent(
        &self,
        p2pkh_hash: &str,
        p2pk_hash: &str,
    ) -> RpcRes<(ElectrumBalance, Vec<ElectrumUnspent>)> {
        let arc = self.clone();
        let key = (p2pkh_hash.to_owned(), p2pk_hash.to_owned());
        let request = self.p2pk_batch(
            rpc_req!(self, "blockchain.scripthash.get_balance", p2pkh_hash),
            rpc_req!(self, "blockchain.scripthash.listunspent", p2pk_hash),
            |balance, p2pk_unspents| (balance, dedup_unspents(p2pk_unspents)),
        );
        let fut = async move {
            arc.get_balance_with_p2pk_concurrent_map
                .wrap_request(key, request)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    /// CRD ch.38 R38.6.3a: the unspents of `p2pkh_hash` and those of `p2pk_hash` in ONE Electrum JSON-RPC
    /// batch, that is one request message for the server. Returns `(p2pkh_unspents, p2pk_unspents)`, each
    /// de-duplicated exactly as [`ElectrumClient::scripthash_list_unspent`] does.
    pub fn scripthash_list_unspent_with_p2pk(
        &self,
        p2pkh_hash: &str,
        p2pk_hash: &str,
    ) -> RpcRes<(Vec<ElectrumUnspent>, Vec<ElectrumUnspent>)> {
        let arc = self.clone();
        let key = (p2pkh_hash.to_owned(), p2pk_hash.to_owned());
        let request = self.p2pk_batch(
            rpc_req!(self, "blockchain.scripthash.listunspent", p2pkh_hash),
            rpc_req!(self, "blockchain.scripthash.listunspent", p2pk_hash),
            |p2pkh_unspents: Vec<ElectrumUnspent>, p2pk_unspents: Vec<ElectrumUnspent>| {
                (dedup_unspents(p2pkh_unspents), dedup_unspents(p2pk_unspents))
            },
        );
        let fut = async move {
            arc.list_unspent_with_p2pk_concurrent_map
                .wrap_request(key, request)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    /// Sends the two requests as one JSON-RPC batch and decodes the two responses, in the order requested.
    fn p2pk_batch<A, B, R>(
        &self,
        p2pkh_request: JsonRpcRequest,
        p2pk_request: JsonRpcRequest,
        combine: impl FnOnce(A, B) -> R + Send + 'static,
    ) -> RpcRes<R>
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        B: serde::de::DeserializeOwned + Send + 'static,
        R: Send + 'static,
    {
        let client_info = JsonRpcClient::client_info(self);
        let context_request = p2pkh_request.clone();
        let fut = self
            .batch_rpc::<_, Json>([p2pkh_request, p2pk_request])
            .compat()
            .map(move |result| {
                let parse_error = |e: String| JsonRpcError {
                    client_info: client_info.clone(),
                    request: JsonRpcRequestEnum::Single(context_request.clone()),
                    error: JsonRpcErrorType::Parse(JsonRpcRemoteAddr::default(), e),
                };
                let mut items = result?.into_iter();
                let (first, second) = match (items.next(), items.next()) {
                    (Some(first), Some(second)) => (first, second),
                    _ => return Err(parse_error("expected two responses in the batch".to_owned())),
                };
                let first = json::from_value(first).map_err(|e| parse_error(e.to_string()))?;
                let second = json::from_value(second).map_err(|e| parse_error(e.to_string()))?;
                Ok(combine(first, second))
            });
        Box::new(fut.boxed().compat())
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
    pub fn scripthash_get_history(&self, hash: &str) -> RpcRes<Vec<ElectrumTxHistoryItem>> {
        rpc_func!(self, "blockchain.scripthash.get_history", hash)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-gethistory
    pub fn scripthash_get_balance(&self, hash: &str) -> RpcRes<ElectrumBalance> {
        let arc = self.clone();
        let hash = hash.to_owned();
        let fut = async move {
            let request = rpc_func!(arc, "blockchain.scripthash.get_balance", &hash);
            arc.get_balance_concurrent_map.wrap_request(hash, request).await
        };
        Box::new(fut.boxed().compat())
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-gethistory
    /// Requests balances in a batch and returns them in the same order they were requested.
    pub fn scripthash_get_balances<I>(&self, hashes: I) -> RpcRes<Vec<ElectrumBalance>>
    where
        I: IntoIterator<Item = String>,
    {
        let requests = hashes
            .into_iter()
            .map(|hash| rpc_req!(self, "blockchain.scripthash.get_balance", &hash));
        self.batch_rpc(requests)
    }

    /// Ask the servers to notify us when `script_hash` changes status.
    ///
    /// The hash is remembered so it can be re-subscribed on a newly connected
    /// server (R38.6.5): a subscription lives with one session, so a reconnect
    /// or a server swap drops it silently.
    ///
    /// A failure here is not fatal. The caller keeps its existing polling, so
    /// the effect of a lost subscription is added latency rather than a stale
    /// balance.
    pub async fn subscribe_scripthash(&self, script_hash: String) -> Result<(), String> {
        let epoch = self.pool.session_epoch();
        let res: Result<Json, _> = rpc_func!(self, BLOCKCHAIN_SCRIPTHASH_SUB_ID, &script_hash)
            .compat()
            .await;
        res.map_err(|e| ERRL!("{}", e))?;
        self.watched_script_hashes.lock().await.insert(script_hash);
        // The connection that carried the subscription may have ended before the hash was remembered
        // (a call that had to open a connection of its own releases it when it is done).
        if self.pool.session_epoch() != epoch {
            self.pool.request_resubscribe();
        }
        Ok(())
    }

    /// Place every remembered subscription again over a maintained connection (R38.6.4h). Failures are
    /// logged and skipped so one unusable server cannot stall the rest; `false` tells that some
    /// subscription could not be placed and the attempt is to be repeated.
    pub async fn resubscribe_watched_scripthashes(&self) -> bool {
        let hashes: Vec<String> = self.watched_script_hashes.lock().await.iter().cloned().collect();
        let mut all_placed = true;
        for script_hash in hashes {
            let res: Result<Json, _> = rpc_func!(self, BLOCKCHAIN_SCRIPTHASH_SUB_ID, &script_hash)
                .compat()
                .await;
            if let Err(e) = res {
                common::log::debug!("Could not re-subscribe script hash {}: {}", script_hash, e);
                all_placed = false;
            }
        }
        all_placed
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-headers-subscribe
    pub fn blockchain_headers_subscribe(&self) -> RpcRes<ElectrumBlockHeader> {
        rpc_func!(self, "blockchain.headers.subscribe")
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-broadcast
    pub fn blockchain_transaction_broadcast(&self, tx: BytesJson) -> RpcRes<H256Json> {
        rpc_func!(self, "blockchain.transaction.broadcast", tx)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-estimatefee
    /// It is recommended to set n_blocks as low as possible.
    /// However, in some cases, n_blocks = 1 leads to an unreasonably high fee estimation.
    /// https://github.com/KomodoPlatform/atomicDEX-API/issues/656#issuecomment-743759659
    pub fn estimate_fee(&self, mode: &Option<EstimateFeeMode>, n_blocks: u32) -> UtxoRpcFut<f64> {
        match mode {
            Some(m) => {
                Box::new(rpc_func!(self, "blockchain.estimatefee", n_blocks, m).map_to_mm_fut(UtxoRpcError::from))
            },
            None => Box::new(rpc_func!(self, "blockchain.estimatefee", n_blocks).map_to_mm_fut(UtxoRpcError::from)),
        }
    }

    pub fn retrieve_last_headers(
        &self,
        blocks_limit_to_check: NonZeroU64,
        block_height: u64,
        policy: HeaderReadPolicy,
    ) -> UtxoRpcFut<(HashMap<u64, BlockHeader>, Vec<BlockHeader>)> {
        let (from, count) = {
            let from = if block_height < blocks_limit_to_check.get() {
                0
            } else {
                block_height - blocks_limit_to_check.get()
            };
            (from, blocks_limit_to_check)
        };
        Box::new(
            self.blockchain_block_headers(from, count)
                .map_to_mm_fut(UtxoRpcError::from)
                .and_then(move |headers| {
                    let (block_registry, block_headers) = {
                        if headers.count == 0 {
                            return MmError::err(UtxoRpcError::Internal("No headers available".to_string()));
                        }
                        let count = usize::try_from(headers.count)
                            .map_to_mm(|e| UtxoRpcError::InvalidResponse(e.to_string()))?;
                        let block_headers = match policy.read_list(&headers.hex.0, count) {
                            Ok(headers) => headers,
                            Err(e) => {
                                return MmError::err(UtxoRpcError::InvalidResponse(e.reason(|e| format!("{:?}", e))))
                            },
                        };
                        let mut block_registry: HashMap<u64, BlockHeader> = HashMap::new();
                        let mut starting_height = from;
                        for block_header in &block_headers {
                            block_registry.insert(starting_height, block_header.clone());
                            starting_height += 1;
                        }
                        (block_registry, block_headers)
                    };
                    Ok((block_registry, block_headers))
                }),
        )
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get-merkle
    pub fn blockchain_transaction_get_merkle(&self, txid: H256Json, height: u64) -> RpcRes<TxMerkleBranch> {
        rpc_func!(self, "blockchain.transaction.get_merkle", txid, height)
    }

    /// Lists unspent outputs locked by the given `script`.
    ///
    /// `list_unspent` only queries the P2PKH script of an address, so non-standard outputs such as
    /// pay-to-pubkey (P2PK) are never discovered through it. This helper queries an arbitrary
    /// scriptPubKey (whose full form is known only where the wallet pubkey is available).
    pub fn list_unspent_for_script(&self, script: &Script) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let script_hash = electrum_script_hash(script);
        Box::new(
            self.scripthash_list_unspent(&hex::encode(script_hash))
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |unspents| {
                    unspents
                        .iter()
                        .map(|unspent| UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.tx_hash.reversed().into(),
                                index: unspent.tx_pos,
                            },
                            value: unspent.value,
                            height: unspent.height,
                        })
                        .collect()
                }),
        )
    }
}

#[cfg_attr(test, mockable)]
impl ElectrumClient {
    /// CRD ch.38 R38.6.3a: the balance of the P2PKH script of `address`, and the unspents locked by
    /// `p2pk_script` (the P2PK lookup R38.6.3 adds to a legacy-address balance), read in ONE Electrum
    /// request message. Returns `(p2pkh_balance, p2pk_unspents)`.
    pub fn display_balance_with_p2pk(
        &self,
        address: &Address,
        decimals: u8,
        p2pk_script: &Script,
    ) -> UtxoRpcFut<(BigDecimal, Vec<UnspentInfo>)> {
        let p2pkh_hash = hex::encode(electrum_script_hash(&output_script(address, ScriptType::P2PKH)));
        let p2pk_hash = hex::encode(electrum_script_hash(p2pk_script));
        Box::new(
            self.scripthash_get_balance_and_list_unspent(&p2pkh_hash, &p2pk_hash)
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |(balance, p2pk_unspents)| {
                    (balance.to_big_decimal(decimals), unspents_to_infos(&p2pk_unspents))
                }),
        )
    }

    /// CRD ch.38 R38.6.3a: the unspents of the P2PKH script of `address` followed by those locked by
    /// `p2pk_script`, read in ONE Electrum request message.
    pub fn list_unspent_with_p2pk(&self, address: &Address, p2pk_script: &Script) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let p2pkh_hash = hex::encode(electrum_script_hash(&output_script(address, ScriptType::P2PKH)));
        let p2pk_hash = hex::encode(electrum_script_hash(p2pk_script));
        Box::new(
            self.scripthash_list_unspent_with_p2pk(&p2pkh_hash, &p2pk_hash)
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |(p2pkh_unspents, p2pk_unspents)| {
                    let mut unspents = unspents_to_infos(&p2pkh_unspents);
                    unspents.extend(unspents_to_infos(&p2pk_unspents));
                    unspents
                }),
        )
    }
}

fn unspents_to_infos(unspents: &[ElectrumUnspent]) -> Vec<UnspentInfo> {
    unspents
        .iter()
        .map(|unspent| UnspentInfo {
            outpoint: OutPoint {
                hash: unspent.tx_hash.reversed().into(),
                index: unspent.tx_pos,
            },
            value: unspent.value,
            height: unspent.height,
        })
        .collect()
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoRpcClientOps for ElectrumClient {
    fn list_unspent(&self, address: &Address, _decimals: u8) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let script = output_script(address, ScriptType::P2PKH);
        let script_hash = electrum_script_hash(&script);
        Box::new(
            self.scripthash_list_unspent(&hex::encode(script_hash))
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |unspents| {
                    unspents
                        .iter()
                        .map(|unspent| UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.tx_hash.reversed().into(),
                                index: unspent.tx_pos,
                            },
                            value: unspent.value,
                            height: unspent.height,
                        })
                        .collect()
                }),
        )
    }

    fn list_unspent_group(&self, addresses: Vec<Address>, _decimals: u8) -> UtxoRpcFut<UnspentMap> {
        let script_hashes = addresses
            .iter()
            .map(|addr| {
                let script = output_script(addr, ScriptType::P2PKH);
                let script_hash = electrum_script_hash(&script);
                hex::encode(script_hash)
            })
            .collect();

        let this = self.clone();
        let fut = async move {
            let unspents = this.scripthash_list_unspent_batch(script_hashes).compat().await?;

            let unspent_map = addresses
                .into_iter()
                // `scripthash_list_unspent_batch` returns `ScriptHashUnspents` elements in the same order in which they were requested.
                // So we can zip `addresses` and `unspents` into one iterator.
                .zip(unspents)
                // Map `(Address, Vec<ElectrumUnspent>)` pairs into `(Address, Vec<UnspentInfo>)`.
                .map(|(address, electrum_unspents)| (address, electrum_unspents.collect_into()))
                .collect();
            Ok(unspent_map)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_transaction(&self, tx: &UtxoTx) -> UtxoRpcFut<H256Json> {
        let bytes = if tx.has_witness() {
            BytesJson::from(serialize_with_flags(tx, SERIALIZE_TRANSACTION_WITNESS))
        } else {
            BytesJson::from(serialize(tx))
        };
        Box::new(
            self.blockchain_transaction_broadcast(bytes)
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    fn send_raw_transaction(&self, tx: BytesJson) -> UtxoRpcFut<H256Json> {
        Box::new(
            self.blockchain_transaction_broadcast(tx)
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// returns transaction bytes by default
    fn get_transaction_bytes(&self, txid: &H256Json) -> UtxoRpcFut<BytesJson> {
        let verbose = false;
        Box::new(rpc_func!(self, "blockchain.transaction.get", txid, verbose).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// returns verbose transaction by default
    fn get_verbose_transaction(&self, txid: &H256Json) -> UtxoRpcFut<RpcTransaction> {
        let verbose = true;
        Box::new(rpc_func!(self, "blockchain.transaction.get", txid, verbose).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// Returns verbose transactions in a batch.
    fn get_verbose_transactions(&self, tx_ids: &[H256Json]) -> UtxoRpcFut<Vec<RpcTransaction>> {
        let verbose = true;
        let requests = tx_ids
            .iter()
            .map(|txid| rpc_req!(self, "blockchain.transaction.get", txid, verbose));
        Box::new(self.batch_rpc(requests).map_to_mm_fut(UtxoRpcError::from))
    }

    fn get_block_count(&self) -> UtxoRpcFut<u64> {
        Box::new(
            self.blockchain_headers_subscribe()
                .map(|r| r.block_height())
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    fn display_balance(&self, address: Address, decimals: u8) -> RpcRes<BigDecimal> {
        let hash = electrum_script_hash(&output_script(&address, ScriptType::P2PKH));
        let hash_str = hex::encode(hash);
        Box::new(
            self.scripthash_get_balance(&hash_str)
                .map(move |electrum_balance| electrum_balance.to_big_decimal(decimals)),
        )
    }

    fn display_balances(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<Vec<(Address, BigDecimal)>> {
        let this = self.clone();
        let fut = async move {
            let hashes = addresses.iter().map(|address| {
                let hash = electrum_script_hash(&output_script(address, ScriptType::P2PKH));
                hex::encode(hash)
            });

            let electrum_balances = this.scripthash_get_balances(hashes).compat().await?;
            let balances = electrum_balances
                .into_iter()
                // `scripthash_get_balances` returns `ElectrumBalance` elements in the same order in which they were requested.
                // So we can zip `addresses` and the balances into one iterator.
                .zip(addresses)
                .map(|(electrum_balance, address)| (address, electrum_balance.to_big_decimal(decimals)))
                .collect();
            Ok(balances)
        };

        Box::new(fut.boxed().compat())
    }

    fn estimate_fee_sat(
        &self,
        decimals: u8,
        _fee_method: &EstimateFeeMethod,
        mode: &Option<EstimateFeeMode>,
        n_blocks: u32,
    ) -> UtxoRpcFut<u64> {
        Box::new(self.estimate_fee(mode, n_blocks).map(move |fee| {
            if fee > 0.00001 {
                (fee * 10.0_f64.powf(decimals as f64)) as u64
            } else {
                1000
            }
        }))
    }

    fn get_relay_fee(&self) -> RpcRes<BigDecimal> { rpc_func!(self, "blockchain.relayfee") }

    fn find_output_spend(
        &self,
        tx_hash: H256,
        script_pubkey: &[u8],
        vout: usize,
        _from_block: BlockHashOrHeight,
    ) -> Box<dyn Future<Item = Option<SpentOutputInfo>, Error = String> + Send> {
        let selfi = self.clone();
        let script_hash = hex::encode(electrum_script_hash(script_pubkey));
        let fut = async move {
            let history = try_s!(selfi.scripthash_get_history(&script_hash).compat().await);

            if history.len() < 2 {
                return Ok(None);
            }

            for item in history.iter() {
                let transaction = try_s!(selfi.get_transaction_bytes(&item.tx_hash).compat().await);

                let maybe_spend_tx: UtxoTx = try_s!(deserialize(transaction.as_slice()).map_err(|e| ERRL!("{:?}", e)));

                for (index, input) in maybe_spend_tx.inputs.iter().enumerate() {
                    if input.previous_output.hash == tx_hash && input.previous_output.index == vout as u32 {
                        return Ok(Some(SpentOutputInfo {
                            spending_tx: maybe_spend_tx,
                            input_index: index,
                            spent_in_block: BlockHashOrHeight::Height(item.height),
                        }));
                    }
                }
            }
            Ok(None)
        };
        Box::new(fut.boxed().compat())
    }

    fn get_median_time_past(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        coin_variant: CoinVariant,
    ) -> UtxoRpcFut<u32> {
        self.get_median_time_past_with_policy(starting_block, count, HeaderReadPolicy::with_layout(coin_variant))
    }

    fn get_median_time_past_with_policy(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        policy: HeaderReadPolicy,
    ) -> UtxoRpcFut<u32> {
        let from = if starting_block <= count.get() {
            0
        } else {
            starting_block - count.get() + 1
        };
        let selfi = self.clone();
        let fut = async move {
            let res = selfi
                .blockchain_block_headers(from, count)
                .compat()
                .await
                .map_to_mm(UtxoRpcError::from)?;
            if res.count == 0 {
                return MmError::err(UtxoRpcError::InvalidResponse("Server returned zero count".to_owned()));
            }
            let count = usize::try_from(res.count).map_to_mm(|e| UtxoRpcError::InvalidResponse(e.to_string()))?;
            // Step 1 is the layout-aware read of the policy; steps 2 to 4 (CRD ch.37 R37.5.10a) run only when
            // it fails.
            let mut timestamps = read_window_times(
                &policy,
                &selfi.coin_ticker,
                &res.hex.0,
                count,
                from,
                now_ms() / 1000,
                |height| {
                    let client = selfi.clone();
                    async move {
                        client
                            .blockchain_block_header(height)
                            .compat()
                            .await
                            .map(|bytes| bytes.0)
                            .map_to_mm(UtxoRpcError::from)
                    }
                },
            )
            .await?;
            // can unwrap because count is non zero
            Ok(median(timestamps.as_mut_slice()).unwrap())
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>> {
        self.get_block_timestamp_with_policy(height, &HeaderReadPolicy::standard())
            .await
    }

    async fn get_block_timestamp_with_policy(
        &self,
        height: u64,
        policy: &HeaderReadPolicy,
    ) -> Result<u64, MmError<UtxoRpcError>> {
        let header_bytes = self.blockchain_block_header(height).compat().await?;
        let header = policy
            .read_header(&header_bytes.0)
            .map_to_mm(|e| UtxoRpcError::InvalidResponse(e.reason(|e| format!("{:?}", e))))?;
        Ok(header.time as u64)
    }
}

#[cfg_attr(test, mockable)]
impl ElectrumClientImpl {
    pub fn new(coin_ticker: String, event_handlers: Vec<RpcTransportEventHandlerShared>) -> ElectrumClientImpl {
        let protocol_version = OrdRange::new(1.2, 1.4).unwrap();
        ElectrumClientImpl {
            pool: Arc::new(PoolShared::new(
                coin_ticker.clone(),
                ElectrumPoolOptions::default(),
                protocol_version.clone(),
                event_handlers.clone(),
            )),
            coin_ticker,
            event_handlers,
            protocol_version,
            get_balance_concurrent_map: ConcurrentRequestMap::new(),
            list_unspent_concurrent_map: ConcurrentRequestMap::new(),
            get_balance_with_p2pk_concurrent_map: ConcurrentRequestMap::new(),
            list_unspent_with_p2pk_concurrent_map: ConcurrentRequestMap::new(),
            watched_script_hashes: AsyncMutex::new(HashSet::new()),
        }
    }

    #[cfg(test)]
    pub fn with_protocol_version(
        coin_ticker: String,
        event_handlers: Vec<RpcTransportEventHandlerShared>,
        protocol_version: OrdRange<f32>,
    ) -> ElectrumClientImpl {
        let mut client = ElectrumClientImpl {
            protocol_version,
            ..ElectrumClientImpl::new(coin_ticker, event_handlers)
        };
        // The pool negotiates the protocol version, so it must know the range.
        client.configure_pool(ElectrumPoolOptions::default());
        client
    }
}

/// Helper function casting mpsc::Receiver as Stream.
fn rx_to_stream(rx: mpsc::Receiver<Vec<u8>>) -> impl Stream<Item = Vec<u8>, Error = io::Error> {
    rx.map_err(|_| panic!("errors not possible on rx"))
}

async fn electrum_process_json(raw_json: Json, arc: &JsonRpcPendingRequestsShared, headers: Option<&HeaderTrack>) {
    // detect if we got standard JSONRPC response or subscription response as JSONRPC request
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ElectrumRpcResponseEnum {
        /// The subscription response as JSONRPC request. It is tried first: it needs a `method`, which a
        /// response never has, while every field of `JsonRpcResponse` has a default, so a notification
        /// would match the single response, be looked up under an empty id, and be dropped.
        SubscriptionNotification(JsonRpcRequest),
        /// The standard JSONRPC single response.
        SingleResponse(JsonRpcResponse),
        /// The batch of standard JSONRPC responses.
        BatchResponses(JsonRpcBatchResponse),
    }

    let response: ElectrumRpcResponseEnum = match json::from_value(raw_json) {
        Ok(res) => res,
        Err(e) => {
            error!("{}", e);
            return;
        },
    };

    let response = match response {
        ElectrumRpcResponseEnum::SingleResponse(single) => JsonRpcResponseEnum::Single(single),
        ElectrumRpcResponseEnum::BatchResponses(batch) => JsonRpcResponseEnum::Batch(batch),
        ElectrumRpcResponseEnum::SubscriptionNotification(req) => {
            // A script-hash notification is an event, not the answer to a
            // pending request, so it is dispatched here and never reaches the
            // request-matching path below.
            if req.method == BLOCKCHAIN_SCRIPTHASH_SUB_ID {
                if let Some(script_hash) = req.params.first().and_then(|p| p.as_str()) {
                    notify_scripthash_change(script_hash);
                }
                return;
            }
            // A contract-event notification echoes the subscription's own
            // arguments, so the registry key is rebuilt from them exactly as it
            // was built when subscribing.
            if req.method == BLOCKCHAIN_CONTRACT_EVENT_SUB_ID {
                let arg = |i: usize| req.params.get(i).and_then(|p| p.as_str());
                if let (Some(address), Some(contract), Some(topic)) = (arg(0), arg(1), arg(2)) {
                    notify_scripthash_change(&contract_event_key(address, contract, topic));
                } else {
                    common::log::debug!(
                        "Contract-event notification with unexpected parameters: {:?}",
                        req.params
                    );
                }
                return;
            }
            let id = match req.method.as_ref() {
                BLOCKCHAIN_HEADERS_SUB_ID => {
                    if let (Some(headers), Some(height)) =
                        (headers, req.params.first().and_then(|p| p["height"].as_u64()))
                    {
                        headers.note_notification(height);
                    }
                    BLOCKCHAIN_HEADERS_SUB_ID
                },
                // Unknown subscription kinds are ignored rather than treated as
                // errors: a server is free to send notifications we never asked
                // for, and dropping them must not disturb the connection.
                _ => {
                    common::log::debug!("Ignoring unrecognised subscription notification {:?}", req.method);
                    return;
                },
            };
            JsonRpcResponseEnum::Single(JsonRpcResponse {
                id: id.into(),
                jsonrpc: "2.0".into(),
                result: req.params[0].clone(),
                error: Json::Null,
            })
        },
    };

    // the corresponding sender may not exist, receiver may be dropped
    // these situations are not considered as errors so we just silently skip them
    let mut pending = arc.lock().await;
    if let Some(tx) = pending.remove(&response.rpc_id()) {
        tx.send(response).ok();
    }
}

#[cfg(test)]
pub(super) async fn electrum_process_json_for_test(raw_json: Json, arc: &JsonRpcPendingRequestsShared) {
    electrum_process_json(raw_json, arc, None).await
}

async fn electrum_process_chunk(chunk: &[u8], arc: &JsonRpcPendingRequestsShared, headers: &HeaderTrack) {
    // we should split the received chunk because we can get several responses in 1 chunk.
    let split = chunk.split(|item| *item == b'\n');
    for chunk in split {
        // split returns empty slice if it ends with separator which is our case
        if !chunk.is_empty() {
            let raw_json: Json = match json::from_slice(chunk) {
                Ok(json) => json,
                Err(e) => {
                    error!("{}", e);
                    return;
                },
            };
            electrum_process_json(raw_json, arc, Some(headers)).await
        }
    }
}

/// The enum wrapping possible variants of underlying Streams
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::large_enum_variant)]
enum ElectrumStream {
    Tcp(TcpStream),
    Tls(TlsStream<TcpStream>),
}

#[cfg(not(target_arch = "wasm32"))]
impl AsRef<TcpStream> for ElectrumStream {
    fn as_ref(&self) -> &TcpStream {
        match self {
            ElectrumStream::Tcp(stream) => stream,
            ElectrumStream::Tls(stream) => stream.get_ref().0,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AsyncRead for ElectrumStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncRead::poll_read(Pin::new(stream), cx, buf),
            ElectrumStream::Tls(stream) => AsyncRead::poll_read(Pin::new(stream), cx, buf),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AsyncWrite for ElectrumStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_write(Pin::new(stream), cx, buf),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_write(Pin::new(stream), cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_flush(Pin::new(stream), cx),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_flush(Pin::new(stream), cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_shutdown(Pin::new(stream), cx),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_shutdown(Pin::new(stream), cx),
        }
    }
}

/// Closes a session that has been silent for `cutoff` (CRD ch.38 R38.6.4e item 3), no earlier.
async fn idle_cutoff_loop(last_chunk: Arc<AtomicU64>, cutoff: Duration) {
    let cutoff_ms = cutoff.as_millis() as u64;
    loop {
        let idle_ms = now_ms().saturating_sub(last_chunk.load(AtomicOrdering::Relaxed));
        if idle_ms >= cutoff_ms {
            warn!(
                "Didn't receive any data for {} ms. Shutting down the connection.",
                idle_ms
            );
            break;
        }
        Timer::sleep((cutoff_ms - idle_ms) as f64 / 1000.).await;
    }
}

/// Builds the Electrum TLS client configuration.
///
/// The `ring` provider is passed explicitly (`builder_with_provider`) rather than read from the
/// process-global default, which would panic at the first connection if two providers were ever
/// compiled in. `tls12` is enabled on the `rustls` dependency and the default protocol versions
/// are TLS 1.3 and TLS 1.2: Electrum servers in the wild include TLS 1.2-only deployments.
#[cfg(not(target_arch = "wasm32"))]
fn rustls_client_config(unsafe_conf: bool) -> Arc<ClientConfig> {
    let cert_store = RootCertStore {
        roots: TLS_SERVER_ROOTS.to_vec(),
    };
    build_rustls_client_config(unsafe_conf, cert_store)
}

#[cfg(not(target_arch = "wasm32"))]
fn build_rustls_client_config(unsafe_conf: bool, cert_store: RootCertStore) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls_ring::default_provider());

    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions");

    let tls_config = if unsafe_conf {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification::new(provider)))
            .with_no_client_auth()
    } else {
        builder.with_root_certificates(cert_store).with_no_client_auth()
    };
    Arc::new(tls_config)
}

/// Builds the TLS client configuration of a server whose key is pinned (CRD ch.38 R38.6.4n item 2). The flag it returns
/// is set when the presented key matched none of the pins. The configuration belongs to one connection, so the flag does
/// too.
#[cfg(not(target_arch = "wasm32"))]
fn pinned_rustls_client_config(pins: Vec<[u8; 32]>) -> (Arc<ClientConfig>, Arc<AtomicBool>) {
    let provider = Arc::new(rustls_ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions");
    let mismatch = Arc::new(AtomicBool::new(false));
    let tls_config = builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedKeyVerification::new(provider, pins, mismatch.clone())))
        .with_no_client_auth();
    (Arc::new(tls_config), mismatch)
}

#[cfg(not(target_arch = "wasm32"))]
lazy_static! {
    static ref SAFE_TLS_CONFIG: Arc<ClientConfig> = rustls_client_config(false);
    static ref UNSAFE_TLS_CONFIG: Arc<ClientConfig> = rustls_client_config(true);
}

/// Connects the server and starts the tasks that move the bytes of the session. Resolving the
/// address, connecting and the TLS handshake happen here; the caller bounds the time they take
/// (CRD ch.38 R38.6.4e item 1).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn open_session(
    id: u64,
    spec: &ServerSpec,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    idle_cutoff: Duration,
    on_end: SessionEnded,
    on_header: HeaderNotified,
) -> Result<Session, String> {
    open_session_with_tls(id, spec, event_handlers, idle_cutoff, on_end, on_header, None).await
}

/// [`open_session`], with the TLS client configuration of a validating session replaceable. Only the unit tests replace
/// it (`Some`), to trust a certificate of their own; the node passes `None` and gets the bundled roots.
#[cfg(not(target_arch = "wasm32"))]
async fn open_session_with_tls(
    id: u64,
    spec: &ServerSpec,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    idle_cutoff: Duration,
    on_end: SessionEnded,
    on_header: HeaderNotified,
    validating_tls_config: Option<Arc<ClientConfig>>,
) -> Result<Session, String> {
    let addr = spec.addr.clone();
    let connect_f = match &spec.config {
        ElectrumConfig::TCP => {
            // The name goes through the node's resolver (CRD ch.45 R45.6B.9 item 4); every address it resolves to is
            // tried in turn. A failed lookup fails this establishment like a failed connect, inside the same bound.
            Either::Left(connect_tcp(spec.host.as_str(), spec.port).map_ok(ElectrumStream::Tcp))
        },
        ElectrumConfig::SSL {
            dns_name,
            skip_validation,
            spki_pins,
        } => {
            // With a pin, the pin decides whatever the flag says (CRD ch.38 R38.6.4n item 4).
            let (tls_connector, mismatch) = if !spki_pins.is_empty() {
                let (config, mismatch) = pinned_rustls_client_config(spki_pins.clone());
                (TlsConnector::from(config), Some(mismatch))
            } else if *skip_validation {
                (TlsConnector::from(UNSAFE_TLS_CONFIG.clone()), None)
            } else {
                (
                    TlsConnector::from(validating_tls_config.unwrap_or_else(|| SAFE_TLS_CONFIG.clone())),
                    None,
                )
            };
            let dns_name = dns_name.clone();

            Either::Right(
                // `dns_name` (the server name of the TLS handshake) stays the host of the server, not an address.
                connect_tcp(spec.host.as_str(), spec.port).and_then(move |stream| {
                    // Can use `unwrap` cause `dns_name` is pre-checked.
                    let dns = ServerName::try_from(dns_name).map_err(|e| fomat!([e])).unwrap();
                    tls_connector
                        .connect(dns, stream)
                        .map_ok(ElectrumStream::Tls)
                        .map_err(move |e| match &mismatch {
                            // A key that matches no pin is reported in the words of R38.6.4n item 5, not in the TLS
                            // library's.
                            Some(mismatch) if mismatch.load(AtomicOrdering::SeqCst) => {
                                io::Error::new(io::ErrorKind::InvalidData, SPKI_MISMATCH)
                            },
                            _ => e,
                        })
                }),
            )
        },
    };

    let stream = connect_f.await.map_err(|e| e.to_string())?;
    stream.as_ref().set_nodelay(true).map_err(|e| e.to_string())?;
    info!("Electrum client connected to {}", addr);
    event_handlers.on_connected(addr.clone())?;

    let last_chunk = Arc::new(AtomicU64::new(now_ms()));
    let closed = Arc::new(AtomicBool::new(false));
    let responses: JsonRpcPendingRequestsShared = Arc::new(AsyncMutex::new(JsonRpcPendingRequests::default()));
    let (tx, rx) = mpsc::channel(0);
    let rx = rx_to_stream(rx).inspect({
        let event_handlers = event_handlers.clone();
        move |data| {
            // measure the length of each sent packet
            event_handlers.on_outgoing_request(data);
        }
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let headers = Arc::new(HeaderTrack::new(on_header));

    let (read, mut write) = tokio::io::split(stream);
    let recv_f = {
        let addr = addr.clone();
        let responses = responses.clone();
        let last_chunk = last_chunk.clone();
        let headers = headers.clone();
        async move {
            let mut buffer = String::with_capacity(1024);
            let mut buf_reader = BufReader::new(read);
            loop {
                match buf_reader.read_line(&mut buffer).await {
                    Ok(c) => {
                        if c == 0 {
                            info!("EOF from {}", addr);
                            break;
                        }
                    },
                    Err(e) => {
                        error!("Error on read {} from {}", e, addr);
                        break;
                    },
                };
                // measure the length of each incoming packet
                event_handlers.on_incoming_response(buffer.as_bytes());
                last_chunk.store(now_ms(), AtomicOrdering::Relaxed);

                electrum_process_chunk(buffer.as_bytes(), &responses, &headers).await;
                buffer.clear();
            }
        }
    };

    let send_f = {
        let addr = addr.clone();
        let mut rx = rx.compat();
        async move {
            while let Some(Ok(bytes)) = rx.next().await {
                if let Err(e) = write.write_all(&bytes).await {
                    error!("Write error {} to {}", e, addr);
                    break;
                }
            }
        }
    };

    spawn(session_task(
        id,
        addr.clone(),
        recv_f,
        send_f,
        last_chunk,
        idle_cutoff,
        shutdown_rx,
        responses.clone(),
        closed.clone(),
        on_end,
    ));

    Ok(Session {
        id,
        addr,
        tx: StdMutex::new(tx),
        responses,
        closed,
        headers,
        shutdown_tx: StdMutex::new(Some(shutdown_tx)),
    })
}

/// Connects the server and starts the tasks that move the messages of the session. The caller bounds
/// the time this takes (CRD ch.38 R38.6.4e item 1).
#[cfg(target_arch = "wasm32")]
pub(crate) async fn open_session(
    id: u64,
    spec: &ServerSpec,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    idle_cutoff: Duration,
    on_end: SessionEnded,
    on_header: HeaderNotified,
) -> Result<Session, String> {
    use mm2_net::wasm_ws::ws_transport;
    use std::sync::atomic::AtomicUsize;

    lazy_static! {
        static ref CONN_IDX: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    }

    let addr = spec.addr.clone();
    let conn_idx = CONN_IDX.fetch_add(1, AtomicOrdering::Relaxed);
    let (mut transport_tx, mut transport_rx) = ws_transport(conn_idx, &spec.ws_url)
        .await
        .map_err(|e| format!("{:?}", e))?;

    info!("Electrum client connected to {}", addr);
    event_handlers.on_connected(addr.clone())?;

    let last_chunk = Arc::new(AtomicU64::new(now_ms()));
    let closed = Arc::new(AtomicBool::new(false));
    let responses: JsonRpcPendingRequestsShared = Arc::new(AsyncMutex::new(JsonRpcPendingRequests::default()));
    let (outgoing_tx, outgoing_rx) = mpsc::channel(0);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let headers = Arc::new(HeaderTrack::new(on_header));

    let incoming_fut = {
        let addr = addr.clone();
        let responses = responses.clone();
        let last_chunk = last_chunk.clone();
        let event_handlers = event_handlers.clone();
        let headers = headers.clone();
        async move {
            while let Some(incoming_res) = transport_rx.next().await {
                last_chunk.store(now_ms(), AtomicOrdering::Relaxed);
                match incoming_res {
                    Ok(incoming_json) => {
                        // measure the length of each incoming packet
                        let incoming_str = incoming_json.to_string();
                        event_handlers.on_incoming_response(incoming_str.as_bytes());

                        electrum_process_json(incoming_json, &responses, Some(&headers)).await;
                    },
                    Err(e) => {
                        error!("{} error: {:?}", addr, e);
                    },
                }
            }
        }
    };

    let outgoing_fut = {
        let addr = addr.clone();
        let mut outgoing_rx = rx_to_stream(outgoing_rx).compat();
        async move {
            while let Some(Ok(data)) = outgoing_rx.next().await {
                let raw_json: Json = match json::from_slice(&data) {
                    Ok(js) => js,
                    Err(e) => {
                        error!("Error {} deserializing the outgoing data: {:?}", e, data);
                        continue;
                    },
                };
                // measure the length of each sent packet
                event_handlers.on_outgoing_request(&data);

                if let Err(e) = transport_tx.send(raw_json).await {
                    error!("Error sending to {}: {:?}", addr, e);
                    break;
                }
            }
        }
    };

    spawn(session_task(
        id,
        addr.clone(),
        incoming_fut,
        outgoing_fut,
        last_chunk,
        idle_cutoff,
        shutdown_rx,
        responses.clone(),
        closed.clone(),
        on_end,
    ));

    Ok(Session {
        id,
        addr,
        tx: StdMutex::new(outgoing_tx),
        responses,
        closed,
        headers,
        shutdown_tx: StdMutex::new(Some(shutdown_tx)),
    })
}

/// The method names of a request message, comma-separated for a batch. Only names: never params, script hashes,
/// addresses or payloads.
fn request_methods(request: &JsonRpcRequestEnum) -> String {
    match request {
        JsonRpcRequestEnum::Single(single) => single.method.clone(),
        JsonRpcRequestEnum::Batch(batch) => batch.methods().join(","),
    }
}

/// Runs until the session ends: the read side or the write side stops, the idle cutoff strikes, or
/// the session is closed. Then it fails the requests still waiting and tells the pool.
#[allow(clippy::too_many_arguments)]
async fn session_task(
    id: u64,
    addr: String,
    read_side: impl std::future::Future<Output = ()> + Send + 'static,
    write_side: impl std::future::Future<Output = ()> + Send + 'static,
    last_chunk: Arc<AtomicU64>,
    idle_cutoff: Duration,
    shutdown_rx: oneshot::Receiver<()>,
    responses: JsonRpcPendingRequestsShared,
    closed: Arc<AtomicBool>,
    on_end: SessionEnded,
) {
    let mut idle_f = Box::pin(idle_cutoff_loop(last_chunk, idle_cutoff)).fuse();
    let mut read_f = Box::pin(read_side).fuse();
    let mut write_f = Box::pin(write_side).fuse();
    let mut shutdown_f = shutdown_rx.compat().fuse();
    select! {
        _idle = idle_f => (),
        _read = read_f => (),
        _write = write_f => (),
        _shutdown = shutdown_f => (),
    }
    info!("{} connection dropped", addr);
    closed.store(true, AtomicOrdering::Relaxed);
    // The requests still waiting get no answer anymore: dropping their senders fails them at once.
    responses.lock().await.clear();
    on_end(id);
}

pub(crate) fn address_balance_from_unspent_map(
    address: &Address,
    unspent_map: &UnspentMap,
    decimals: u8,
) -> BigDecimal {
    let unspents = match unspent_map.get(address) {
        Some(unspents) => unspents,
        // If `balances` doesn't contain `address`, there are no unspents related to the address.
        // Consider the balance of that address equal to 0.
        None => return BigDecimal::from(0),
    };
    unspents.iter().fold(BigDecimal::from(0), |sum, unspent| {
        sum + big_decimal_from_sat_unsigned(unspent.value, decimals)
    })
}

#[cfg(test)]
mod connection_error_tests {
    use super::{contract_event_key, futures_mpsc, notify_scripthash_change, unwatch_scripthash, watch_scripthash};

    /// A registered watcher is woken with the hash that changed, so the
    /// consumer knows which address to re-read.
    #[test]
    fn scripthash_notification_reaches_its_watcher() {
        let (tx, mut rx) = futures_mpsc::unbounded();
        watch_scripthash("aabb".to_owned(), tx);

        notify_scripthash_change("aabb");

        assert_eq!(rx.try_recv().unwrap(), "aabb".to_owned());
        unwatch_scripthash("aabb");
    }

    /// Two consumers watching the same address must both be woken.
    ///
    /// The balance streamer and the transaction-history loop both watch a
    /// coin's address, so a registry holding one sender per hash would let
    /// whichever registered second silently starve the first -- a failure that
    /// looks exactly like "that subsystem just never updates".
    #[test]
    fn multiple_watchers_of_one_hash_are_all_woken() {
        let (tx_a, mut rx_a) = futures_mpsc::unbounded();
        let (tx_b, mut rx_b) = futures_mpsc::unbounded();
        watch_scripthash("shared".to_owned(), tx_a);
        watch_scripthash("shared".to_owned(), tx_b);

        notify_scripthash_change("shared");

        assert_eq!(rx_a.try_recv().unwrap(), "shared".to_owned(), "first watcher");
        assert_eq!(rx_b.try_recv().unwrap(), "shared".to_owned(), "second watcher");
        unwatch_scripthash("shared");
    }

    /// One consumer departing must not silence the other.
    #[test]
    fn departed_watcher_does_not_silence_its_peer() {
        let (tx_gone, rx_gone) = futures_mpsc::unbounded();
        let (tx_live, mut rx_live) = futures_mpsc::unbounded();
        watch_scripthash("peer".to_owned(), tx_gone);
        watch_scripthash("peer".to_owned(), tx_live);
        drop(rx_gone);

        unwatch_scripthash("peer");
        notify_scripthash_change("peer");

        assert_eq!(
            rx_live.try_recv().unwrap(),
            "peer".to_owned(),
            "the surviving watcher must still be notified"
        );
        unwatch_scripthash("peer");
    }

    /// A notification for a hash nobody registered must be a no-op rather than
    /// an error: servers may push subscriptions we never asked for, and that
    /// must not disturb the connection.
    #[test]
    fn unwatched_scripthash_notification_is_ignored() {
        notify_scripthash_change("never-registered");

        let (tx, mut rx) = futures_mpsc::unbounded();
        watch_scripthash("ccdd".to_owned(), tx);
        notify_scripthash_change("some-other-hash");
        assert!(rx.try_recv().is_err(), "an unrelated hash must not wake this watcher");
        unwatch_scripthash("ccdd");
    }

    /// If the consumer is gone the registration is dropped, so a departed
    /// streamer cannot leak an entry for the life of the process.
    #[test]
    fn dropped_receiver_deregisters_its_watch() {
        let (tx, rx) = futures_mpsc::unbounded();
        watch_scripthash("eeff".to_owned(), tx);
        drop(rx);

        notify_scripthash_change("eeff");

        let (tx2, mut rx2) = futures_mpsc::unbounded();
        watch_scripthash("eeff".to_owned(), tx2);
        notify_scripthash_change("eeff");
        assert_eq!(rx2.try_recv().unwrap(), "eeff".to_owned());
        unwatch_scripthash("eeff");
    }

    /// Unwatching reclaims a departed watcher's registration.
    ///
    /// It prunes by receiver liveness rather than by key, because several
    /// consumers may share a hash and one leaving must not silence the rest.
    #[test]
    fn unwatch_reclaims_a_departed_watcher() {
        let (tx, rx) = futures_mpsc::unbounded();
        watch_scripthash("1122".to_owned(), tx);
        drop(rx);

        unwatch_scripthash("1122");

        // With the entry reclaimed, a fresh watcher starts clean and is the
        // only recipient.
        let (tx2, mut rx2) = futures_mpsc::unbounded();
        watch_scripthash("1122".to_owned(), tx2);
        notify_scripthash_change("1122");
        assert_eq!(rx2.try_recv().unwrap(), "1122".to_owned());
        assert!(rx2.try_recv().is_err(), "exactly one delivery, not a duplicate");
        unwatch_scripthash("1122");
    }

    /// Contract-event keys are distinct from script-hash keys for the same
    /// address, so a QRC20 token and its platform coin do not collide.
    ///
    /// A QRC20 balance lives in contract storage; if both keyed on the address
    /// alone, a token notification would wake the platform coin's watcher and
    /// vice versa, and one would displace the other at registration.
    #[test]
    fn contract_event_and_scripthash_keys_do_not_collide() {
        let address = "abcd";
        let event_key = contract_event_key(address, "contract1", "topic1");
        assert_ne!(
            event_key, address,
            "a contract-event key must not equal the bare address"
        );

        let (tx_addr, mut rx_addr) = futures_mpsc::unbounded();
        let (tx_event, mut rx_event) = futures_mpsc::unbounded();
        watch_scripthash(address.to_owned(), tx_addr);
        watch_scripthash(event_key.clone(), tx_event);

        notify_scripthash_change(&event_key);

        assert_eq!(rx_event.try_recv().unwrap(), event_key.clone(), "token watcher woken");
        assert!(
            rx_addr.try_recv().is_err(),
            "the platform coin's watcher must not be woken"
        );

        unwatch_scripthash(address);
        unwatch_scripthash(&event_key);
    }

    /// Different tokens held at the same address must key differently, or one
    /// token's transfer would be reported as another's.
    #[test]
    fn contract_event_keys_differ_per_token() {
        let a = contract_event_key("addr", "token_a", "topic");
        let b = contract_event_key("addr", "token_b", "topic");
        assert_ne!(a, b);
    }

    /// Stress the watcher registry far past any realistic wallet, to answer
    /// whether a subscription cap is needed.
    ///
    /// Realistic worst case today is one subscription per activated
    /// Electrum-backed coin, because the balance streamer watches a single
    /// address per coin. This registers three orders of magnitude more and
    /// asserts both correctness and that the work stays trivial, so the
    /// no-cap decision rests on a measurement rather than an assumption.
    #[test]
    fn watcher_registry_handles_far_more_than_any_realistic_wallet() {
        use std::time::Instant;

        const COUNT: usize = 10_000;

        let mut receivers = Vec::with_capacity(COUNT);
        let hashes: Vec<String> = (0..COUNT).map(|i| format!("stress{i:059}")).collect();

        let registered = Instant::now();
        for hash in &hashes {
            let (tx, rx) = futures_mpsc::unbounded();
            watch_scripthash(hash.clone(), tx);
            receivers.push(rx);
        }
        let register_elapsed = registered.elapsed();

        let notified = Instant::now();
        for hash in &hashes {
            notify_scripthash_change(hash);
        }
        let notify_elapsed = notified.elapsed();

        // Every watcher must receive exactly its own hash: a registry that
        // collapsed or cross-wired entries under load would be worse than one
        // that simply refused the work.
        for (rx, hash) in receivers.iter_mut().zip(hashes.iter()) {
            assert_eq!(rx.try_recv().unwrap(), hash.clone());
        }

        for hash in &hashes {
            unwatch_scripthash(hash);
        }

        // Generous bounds: the point is to catch a pathological cost such as a
        // per-notification scan of the whole registry, not to benchmark.
        assert!(
            register_elapsed.as_millis() < 2_000,
            "registering {COUNT} watchers took {register_elapsed:?}"
        );
        assert!(
            notify_elapsed.as_millis() < 2_000,
            "notifying {COUNT} watchers took {notify_elapsed:?}"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tls_tests {
    use super::*;
    use rustls::version::{TLS12, TLS13};
    use rustls::{ProtocolVersion, ServerConfig, SupportedProtocolVersion};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    struct TestServer {
        port: u16,
        cert: CertificateDer<'static>,
    }

    /// A one-shot loopback TLS server restricted to `versions` that echoes a 4-byte payload.
    async fn spawn_server(versions: &[&'static SupportedProtocolVersion], names: &[&str]) -> TestServer {
        let certified = rcgen::generate_simple_self_signed(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
            .expect("self-signed certificate");
        let cert = CertificateDer::from(certified.cert.der().to_vec());
        spawn_server_with_cert(versions, cert, certified.key_pair.serialize_der()).await
    }

    /// [`spawn_server`] with the certificate and the private key (PKCS#8 DER) given.
    async fn spawn_server_with_cert(
        versions: &[&'static SupportedProtocolVersion],
        cert: CertificateDer<'static>,
        key_der: Vec<u8>,
    ) -> TestServer {
        let key = rustls::pki_types::PrivateKeyDer::try_from(key_der).expect("key");
        let provider = Arc::new(rustls_ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .expect("server certificate");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let mut buf = [0u8; 4];
                    if tls.read_exact(&mut buf).await.is_ok() {
                        let _ = tls.write_all(&buf).await;
                        let _ = tls.flush().await;
                    }
                }
            }
        });
        TestServer { port, cert }
    }

    /// Connects with `config`, sends a 4-byte payload and returns the negotiated version.
    async fn connect(
        config: Arc<ClientConfig>,
        server: &TestServer,
        name: &'static str,
    ) -> std::io::Result<ProtocolVersion> {
        let tcp = TcpStream::connect(("127.0.0.1", server.port)).await?;
        let mut tls = TlsConnector::from(config)
            .connect(ServerName::try_from(name).unwrap(), tcp)
            .await?;
        tls.write_all(b"ping").await?;
        tls.flush().await?;
        let mut buf = [0u8; 4];
        tls.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"ping");
        Ok(tls.get_ref().1.protocol_version().expect("negotiated version"))
    }

    fn trusting(cert: &CertificateDer<'static>) -> Arc<ClientConfig> {
        let mut store = RootCertStore::empty();
        store.add(cert.clone()).expect("trust anchor");
        build_rustls_client_config(false, store)
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn validating_config_completes_handshake_on_tls12_only_and_tls13() {
        run(async {
            let tls12 = spawn_server(&[&TLS12], &["localhost"]).await;
            assert_eq!(
                connect(trusting(&tls12.cert), &tls12, "localhost").await.unwrap(),
                ProtocolVersion::TLSv1_2
            );
            let tls13 = spawn_server(&[&TLS13], &["localhost"]).await;
            assert_eq!(
                connect(trusting(&tls13.cert), &tls13, "localhost").await.unwrap(),
                ProtocolVersion::TLSv1_3
            );
        });
    }

    #[test]
    fn validating_config_rejects_untrusted_and_misnamed_certificates() {
        run(async {
            let server = spawn_server(&[&TLS12, &TLS13], &["localhost"]).await;
            // Only the bundled webpki roots: a self-signed certificate is not trusted.
            let roots_only = rustls_client_config(false);
            assert!(connect(roots_only, &server, "localhost").await.is_err());
            // Trusted certificate, wrong server name.
            assert!(connect(trusting(&server.cert), &server, "example.test").await.is_err());
        });
    }

    #[test]
    fn skip_validation_accepts_self_signed_and_misnamed_certificates_on_both_versions() {
        run(async {
            let config = rustls_client_config(true);
            let tls12 = spawn_server(&[&TLS12], &["localhost"]).await;
            assert_eq!(
                connect(config.clone(), &tls12, "localhost").await.unwrap(),
                ProtocolVersion::TLSv1_2
            );
            // Name mismatch is also tolerated, as it was with rustls 0.20.
            assert!(connect(config.clone(), &tls12, "example.test").await.is_ok());
            let tls13 = spawn_server(&[&TLS13], &["localhost"]).await;
            assert_eq!(
                connect(config, &tls13, "localhost").await.unwrap(),
                ProtocolVersion::TLSv1_3
            );
        });
    }

    #[test]
    fn host_precheck_accepts_hostnames_and_rejects_ip_literals() {
        assert!(DnsName::try_from("electrum.example.org").is_ok());
        assert!(DnsName::try_from("localhost").is_ok());
        assert!(DnsName::try_from("127.0.0.1").is_err());
        assert!(DnsName::try_from("").is_err());
    }

    // ---- CRD ch.45 T45.19: the server names go through the node's resolver ----

    use common::dns_resolver::{gauges, install_test_map};
    use std::net::{IpAddr, Ipv4Addr};

    fn spec_of(url: String, protocol: ElectrumProtocol) -> ServerSpec {
        server_spec(&request_of(url, protocol)).expect("a server spec")
    }

    fn request_of(url: String, protocol: ElectrumProtocol) -> ElectrumRpcRequest {
        ElectrumRpcRequest {
            url,
            protocol,
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        }
    }

    fn no_op_callbacks() -> (SessionEnded, HeaderNotified) { (Arc::new(|_| {}), Arc::new(|_| {})) }

    #[test]
    fn a_tcp_server_name_is_resolved_through_the_installed_resolver() {
        let guard = install_test_map(&[("electrum.test", &[IpAddr::V4(Ipv4Addr::LOCALHOST)])]);
        run(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let port = listener.local_addr().unwrap().port();
            let accepted = tokio::spawn(async move { listener.accept().await.map(|(stream, _)| stream) });
            let spec = spec_of(format!("electrum.test:{}", port), ElectrumProtocol::TCP);
            let (on_end, on_header) = no_op_callbacks();
            let session = open_session(1, &spec, vec![], Duration::from_secs(60), on_end, on_header)
                .await
                .expect("the session opens");
            accepted
                .await
                .unwrap()
                .expect("the loopback server accepted the connection");
            drop(session);
        });
        // The gauges are process-wide and the tests that do not hold the test lock look up names beside this one: the
        // name counts exactly, the gauge at least.
        assert_eq!(guard.lookups_of("electrum.test"), 1);
        assert!(gauges().unwrap().lookups >= 1);
    }

    #[test]
    fn a_failed_lookup_is_a_recoverable_establishment_failure_that_names_no_server() {
        // The test resolver knows the name and has no address for it.
        let guard = install_test_map(&[("empty.test", &[])]);
        run(async {
            let spec = spec_of("empty.test:50001".to_owned(), ElectrumProtocol::TCP);
            let (on_end, on_header) = no_op_callbacks();
            let error = open_session(1, &spec, vec![], Duration::from_secs(60), on_end, on_header)
                .await
                .err()
                .expect("the lookup fails");
            assert!(!error.is_empty());
            assert!(!error.contains("empty.test"), "the error names no server: {}", error);
        });
        assert_eq!(guard.failures_of("empty.test"), 1);
        assert!(gauges().unwrap().failures >= 1);
    }

    #[test]
    fn a_tls_server_is_checked_against_its_name_and_not_its_address() {
        let guard = install_test_map(&[
            ("electrum.test", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
            ("other.test", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
        ]);
        run(async {
            // Two servers, each with a certificate for `electrum.test`: the test server serves one connection at a time and
            // keeps the first open, so the second dial needs a server of its own.
            let server = spawn_server(&[&TLS12, &TLS13], &["electrum.test"]).await;
            let second = spawn_server(&[&TLS12, &TLS13], &["electrum.test"]).await;
            let (on_end, on_header) = no_op_callbacks();

            // The certificate is for `electrum.test`: the session to that name opens.
            let spec = spec_of(format!("electrum.test:{}", server.port), ElectrumProtocol::SSL);
            let opened = open_session_with_tls(
                1,
                &spec,
                vec![],
                Duration::from_secs(60),
                on_end.clone(),
                on_header.clone(),
                Some(trusting(&server.cert)),
            )
            .await;
            assert!(opened.is_ok(), "{:?}", opened.err());

            // `other.test` resolves to the same address, but the certificate is not for that name.
            let spec = spec_of(format!("other.test:{}", second.port), ElectrumProtocol::SSL);
            let refused = open_session_with_tls(
                2,
                &spec,
                vec![],
                Duration::from_secs(60),
                on_end,
                on_header,
                Some(trusting(&second.cert)),
            )
            .await;
            assert!(
                refused.is_err(),
                "the certificate check must use the name, not the address"
            );
        });
        assert_eq!(guard.lookups_of("electrum.test"), 1);
        assert_eq!(guard.lookups_of("other.test"), 1);
        assert!(gauges().unwrap().lookups >= 2);
    }

    // ---- CRD ch.38 T38.6.4m (c) and (e), T38.6.4n: the key pin and the warning ----

    use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
    use rcgen::{date_time_ymd, CertificateParams, KeyPair};

    /// Static DER fixtures for T38.6.4m (c). No private key is kept: the refusals under test happen on the client, in
    /// the check of the certificate and the handshake signature, and a `rustls` server cannot sign with a 1024-bit RSA
    /// key anyway. Generated once with OpenSSL 3.0 (key files discarded; both certificates are valid until 2126):
    ///
    /// ```text
    /// # version 1 certificate (an X.509 version 1 certificate carries no extensions), ECDSA P-256 key
    /// openssl ecparam -name prime256v1 -genkey -noout -out ec.key
    /// openssl req -new -key ec.key -subj "/CN=v1.electrum.test" -out v1.csr
    /// openssl x509 -req -in v1.csr -signkey ec.key -days 36500 -out v1.pem
    /// openssl x509 -in v1.pem -outform der -out v1-cert.der
    /// # version 3 certificate with an RSA key of 1024 bits
    /// openssl genrsa -out rsa1024.key 1024
    /// openssl req -x509 -new -key rsa1024.key -sha256 -days 36500 -subj "/CN=rsa1024.electrum.test" \
    ///     -addext "subjectAltName=DNS:rsa1024.electrum.test" -out rsa1024.pem
    /// openssl x509 -in rsa1024.pem -outform der -out rsa1024-cert.der
    /// # the pins below, the way R38.6.4n item 7 describes it
    /// openssl x509 -in <cert>.pem -pubkey -noout | openssl pkey -pubin -outform der | sha256sum
    /// ```
    const V1_CERT: &[u8] = include_bytes!("../test-fixtures/electrum-tls/v1-cert.der");
    const V1_CERT_SPKI_SHA256: &str = "8d620cd46b1e975750f8f5cf64b53ef9c46ad3b2a6f6808de4d1b5c1ba7d764c";
    const RSA1024_CERT: &[u8] = include_bytes!("../test-fixtures/electrum-tls/rsa1024-cert.der");
    const RSA1024_CERT_SPKI_SHA256: &str = "f48938b51c39c2053a415123a906423a0bbf27c11dc6c8ed5282e710668d0ecf";

    const PIN_HOST: &str = "pin.test";

    fn localhost_map() -> common::dns_resolver::TestResolverGuard {
        install_test_map(&[
            (PIN_HOST, &[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
            ("other.test", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
        ])
    }

    /// A self-signed certificate for `names` over `key`; `tweak` changes the parameters (validity, subject).
    fn certificate_for(
        key: &KeyPair,
        names: &[&str],
        tweak: impl FnOnce(&mut CertificateParams),
    ) -> CertificateDer<'static> {
        let mut params = CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
            .expect("certificate parameters");
        tweak(&mut params);
        CertificateDer::from(params.self_signed(key).expect("self-signed certificate").der().to_vec())
    }

    /// The pin of `key`: SHA-256 of its DER `SubjectPublicKeyInfo`, in lower-case hexadecimal.
    fn pin_of(key: &KeyPair) -> String { hex::encode(Sha256::digest(&key.public_key_der())) }

    fn colon_upper(pin: &str) -> String {
        pin.as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap().to_uppercase())
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Opens a session to a fresh loopback server (restricted to `version`, serving `cert` with `key`) the way the pool
    /// does, for a server entry with `pin` and `flag`. The error is the establishment failure.
    async fn dial(
        version: &'static SupportedProtocolVersion,
        key: &KeyPair,
        cert: CertificateDer<'static>,
        pin: Option<Json>,
        flag: bool,
    ) -> Result<(), String> {
        let server = spawn_server_with_cert(&[version], cert, key.serialize_der()).await;
        let request = ElectrumRpcRequest {
            disable_cert_verification: flag,
            spki_sha256: pin,
            ..request_of(format!("{}:{}", PIN_HOST, server.port), ElectrumProtocol::SSL)
        };
        let spec = server_spec(&request).expect("a server spec");
        let (on_end, on_header) = no_op_callbacks();
        open_session(1, &spec, vec![], Duration::from_secs(60), on_end, on_header)
            .await
            .map(drop)
    }

    #[test]
    fn t38_6_4n_a_matching_pin_is_accepted_on_tls12_and_tls13_and_in_every_form() {
        let _guard = localhost_map();
        run(async {
            for version in [&TLS12, &TLS13] {
                let key = KeyPair::generate().unwrap();
                let other = pin_of(&KeyPair::generate().unwrap());
                let pin = pin_of(&key);
                for value in [
                    json::json!(pin),
                    json::json!(pin.to_uppercase()),
                    json::json!(colon_upper(&pin)),
                    // A second pin of an array matches; the first is another key's.
                    json::json!([other, pin]),
                    json::json!([pin, other, other, other]),
                ] {
                    let cert = certificate_for(&key, &[PIN_HOST], |_| {});
                    let outcome = dial(version, &key, cert, Some(value.clone()), false).await;
                    assert!(outcome.is_ok(), "{:?} {:?}", value, outcome);
                }
            }
        });
    }

    #[test]
    fn t38_6_4n_a_mismatch_is_refused_with_the_text_of_the_rule() {
        let _guard = localhost_map();
        run(async {
            for version in [&TLS12, &TLS13] {
                let key = KeyPair::generate().unwrap();
                let cert = certificate_for(&key, &[PIN_HOST], |_| {});
                let wrong = Some(json::json!([pin_of(&KeyPair::generate().unwrap())]));
                let outcome = dial(version, &key, cert, wrong, false).await;
                assert_eq!(outcome, Err("the server's key does not match spki_sha256".to_owned()));
            }
        });
    }

    #[test]
    fn t38_6_4n_a_self_signed_expired_or_misnamed_certificate_with_the_pinned_key_is_accepted() {
        let _guard = localhost_map();
        run(async {
            for version in [&TLS12, &TLS13] {
                let key = KeyPair::generate().unwrap();
                let pin = Some(json::json!(pin_of(&key)));
                // Self-signed: without the pin the default verification refuses it (T38.6.4m (a)).
                let cert = certificate_for(&key, &[PIN_HOST], |_| {});
                assert!(dial(version, &key, cert.clone(), None, false).await.is_err());
                assert!(dial(version, &key, cert, pin.clone(), false).await.is_ok());
                // Expired.
                let cert = certificate_for(&key, &[PIN_HOST], |params| {
                    params.not_before = date_time_ymd(2001, 1, 1);
                    params.not_after = date_time_ymd(2002, 1, 1);
                });
                assert!(dial(version, &key, cert, pin.clone(), false).await.is_ok());
                // A name that is not the server's.
                let cert = certificate_for(&key, &["unrelated.test"], |_| {});
                assert!(dial(version, &key, cert, pin, false).await.is_ok());
            }
        });
    }

    #[test]
    fn t38_6_4n_another_key_under_the_same_subject_is_refused() {
        let _guard = localhost_map();
        run(async {
            for version in [&TLS12, &TLS13] {
                let pinned = KeyPair::generate().unwrap();
                let impostor = KeyPair::generate().unwrap();
                // The same names and the same subject, another key: even a valid-looking certificate does not pass.
                let cert = certificate_for(&impostor, &[PIN_HOST], |_| {});
                let outcome = dial(version, &impostor, cert, Some(json::json!(pin_of(&pinned))), false).await;
                assert_eq!(outcome, Err(SPKI_MISMATCH.to_owned()));
            }
        });
    }

    #[test]
    fn t38_6_4n_the_flag_together_with_a_pin_behaves_as_the_pin_alone() {
        let _guard = localhost_map();
        run(async {
            let key = KeyPair::generate().unwrap();
            let other = KeyPair::generate().unwrap();
            let cert = certificate_for(&key, &[PIN_HOST], |_| {});
            // The flag alone accepts this self-signed certificate.
            assert!(dial(&TLS13, &key, cert.clone(), None, true).await.is_ok());
            // With a pin of another key the flag no longer helps.
            let outcome = dial(&TLS13, &key, cert.clone(), Some(json::json!(pin_of(&other))), true).await;
            assert_eq!(outcome, Err(SPKI_MISMATCH.to_owned()));
            // With the right pin the connection is accepted, as with the pin alone.
            assert!(dial(&TLS13, &key, cert, Some(json::json!(pin_of(&key))), true)
                .await
                .is_ok());
        });
    }

    #[test]
    fn t38_6_4m_c_a_version_1_certificate_and_an_rsa_1024_certificate_are_refused_under_the_flag() {
        use rustls::internal::msgs::codec::{Codec, Reader};
        fn signature(scheme: u16) -> DigitallySignedStruct {
            let mut bytes = scheme.to_be_bytes().to_vec();
            bytes.extend_from_slice(&128u16.to_be_bytes());
            bytes.extend(std::iter::repeat(0u8).take(128));
            DigitallySignedStruct::read(&mut Reader::init(&bytes)).expect("a signature structure")
        }
        const ECDSA_P256_SHA256: u16 = 0x0403;
        const RSA_PKCS1_SHA256: u16 = 0x0401;
        const RSA_PSS_SHA256: u16 = 0x0804;

        let provider = Arc::new(rustls_ring::default_provider());
        let flagged = NoCertificateVerification::new(provider);
        let name = ServerName::try_from("x.test").unwrap();
        let v1 = CertificateDer::from(V1_CERT.to_vec());
        let rsa = CertificateDer::from(RSA1024_CERT.to_vec());
        let message = b"handshake transcript";
        for cert in [&v1, &rsa] {
            // The chain, the name and the dates are skipped under the flag ...
            assert!(flagged
                .verify_server_cert(cert, &[], &name, &[], UnixTime::now())
                .is_ok());
        }
        // ... the check of the certificate and of the key behind the handshake signature stays.
        for scheme in [ECDSA_P256_SHA256, RSA_PSS_SHA256] {
            assert!(flagged
                .verify_tls12_signature(message, &v1, &signature(scheme))
                .is_err());
            assert!(flagged
                .verify_tls13_signature(message, &v1, &signature(scheme))
                .is_err());
        }
        for scheme in [RSA_PKCS1_SHA256, RSA_PSS_SHA256] {
            assert!(flagged
                .verify_tls12_signature(message, &rsa, &signature(scheme))
                .is_err());
        }
        assert!(flagged
            .verify_tls13_signature(message, &rsa, &signature(RSA_PSS_SHA256))
            .is_err());
    }

    #[test]
    fn t38_6_4n_a_pin_does_not_let_a_version_1_or_an_rsa_1024_certificate_through() {
        use rustls::internal::msgs::codec::{Codec, Reader};
        let provider = Arc::new(rustls_ring::default_provider());
        let name = ServerName::try_from("x.test").unwrap();
        let v1 = CertificateDer::from(V1_CERT.to_vec());
        let rsa = CertificateDer::from(RSA1024_CERT.to_vec());
        let pins = |hexes: &[&str]| -> Vec<[u8; 32]> {
            hexes
                .iter()
                .map(|h| <[u8; 32]>::try_from(hex::decode(h).unwrap()).unwrap())
                .collect()
        };

        // The digest the verifier computes is the one of the `openssl` pipeline of R38.6.4n item 7: it matches the
        // value recorded from that pipeline for the RSA fixture.
        let flag = Arc::new(AtomicBool::new(false));
        let pinned = PinnedKeyVerification::new(provider.clone(), pins(&[RSA1024_CERT_SPKI_SHA256]), flag.clone());
        assert!(pinned
            .verify_server_cert(&rsa, &[], &name, &[], UnixTime::now())
            .is_ok());
        assert!(!flag.load(AtomicOrdering::SeqCst));
        // The pinned key must still prove itself with an accepted scheme; a 1024-bit RSA key cannot.
        let mut bytes = 0x0804u16.to_be_bytes().to_vec();
        bytes.extend_from_slice(&128u16.to_be_bytes());
        bytes.extend(std::iter::repeat(0u8).take(128));
        let dss = DigitallySignedStruct::read(&mut Reader::init(&bytes)).expect("a signature structure");
        assert!(pinned.verify_tls13_signature(b"transcript", &rsa, &dss).is_err());
        assert!(pinned.verify_tls12_signature(b"transcript", &rsa, &dss).is_err());

        // A version 1 certificate does not parse as the certificate of R38.6.4m item 5, with its pin or without it; that is
        // not a key mismatch.
        let flag = Arc::new(AtomicBool::new(false));
        let pinned = PinnedKeyVerification::new(provider, pins(&[V1_CERT_SPKI_SHA256]), flag.clone());
        assert!(pinned
            .verify_server_cert(&v1, &[], &name, &[], UnixTime::now())
            .is_err());
        assert!(!flag.load(AtomicOrdering::SeqCst));

        // A key that matches none of the pins sets the flag.
        let pinned = PinnedKeyVerification::new(
            Arc::new(rustls_ring::default_provider()),
            pins(&[V1_CERT_SPKI_SHA256]),
            flag.clone(),
        );
        assert!(pinned
            .verify_server_cert(&rsa, &[], &name, &[], UnixTime::now())
            .is_err());
        assert!(flag.load(AtomicOrdering::SeqCst));
    }

    #[test]
    fn t38_6_4n_pins_are_read_in_every_accepted_form_and_refused_in_every_malformed_one() {
        let pin = "0123456789abcdef".repeat(4);
        let expected = <[u8; 32]>::try_from(hex::decode(&pin).unwrap()).unwrap();
        for accepted in [
            json::json!(pin),
            json::json!(pin.to_uppercase()),
            json::json!(colon_upper(&pin)),
            json::json!(colon_upper(&pin).to_lowercase()),
        ] {
            assert_eq!(parse_spki_pins(&accepted), Ok(vec![expected]), "{}", accepted);
        }
        // An array of 1 to 4.
        for count in 1..=4 {
            let array = Json::Array(vec![json::json!(pin); count]);
            assert_eq!(parse_spki_pins(&array).map(|pins| pins.len()), Ok(count));
        }
        let malformed = Err("spki_sha256 is not a 64-digit hexadecimal SHA-256 digest".to_owned());
        for bad in [
            json::json!(&pin[..63]),
            json::json!(format!("{}0", pin)),
            json::json!(format!("{}g", &pin[..63])),
            json::json!(format!("{}é", &pin[..62])),
            json::json!(""),
            json::json!(":::"),
            json::json!([]),
            Json::Array(vec![json::json!(pin); 5]),
            json::json!([1]),
            json::json!([pin, 7]),
            json::json!([[pin]]),
            json::json!(7),
            json::json!(true),
            json::json!({ "pin": pin }),
            Json::Null,
        ] {
            assert_eq!(parse_spki_pins(&bad), malformed, "{}", bad);
        }
    }

    #[test]
    fn t38_6_4n_a_server_entry_with_a_pin_is_classified_when_it_is_read() {
        let pin = pin_of(&KeyPair::generate().unwrap());
        let entry = |protocol, flag: bool, pin: Option<Json>| ElectrumRpcRequest {
            disable_cert_verification: flag,
            spki_sha256: pin,
            ..request_of("pin.test:50002".to_owned(), protocol)
        };
        let notice = |request: ElectrumRpcRequest| server_spec(&request).expect("a server spec").tls_notice();
        let ssl = ElectrumProtocol::SSL;

        // R38.6.4m item 6 and R38.6.4n item 4.
        assert_eq!(notice(entry(ssl.clone(), false, None)), None);
        assert_eq!(
            notice(entry(ssl.clone(), true, None)),
            Some(TlsNotice::NotAuthenticated)
        );
        assert_eq!(
            notice(entry(ssl.clone(), false, Some(json::json!(pin)))),
            Some(TlsNotice::Pinned)
        );
        assert_eq!(
            notice(entry(ssl.clone(), true, Some(json::json!(pin)))),
            Some(TlsNotice::Pinned)
        );
        assert_eq!(notice(entry(ElectrumProtocol::TCP, true, None)), None);

        // R38.6.4n item 5: removal-class failures, and the configured value is never echoed.
        let marker = "secret-marker";
        let malformed = server_spec(&entry(ssl.clone(), false, Some(json::json!(marker))))
            .expect_err("a malformed pin removes the server");
        assert_eq!(malformed, "spki_sha256 is not a 64-digit hexadecimal SHA-256 digest");
        let on_tcp = server_spec(&entry(ElectrumProtocol::TCP, false, Some(json::json!(pin))))
            .expect_err("a pin on TCP removes the server");
        assert_eq!(on_tcp, "spki_sha256 needs protocol SSL");
        let malformed_on_tcp = server_spec(&entry(ElectrumProtocol::TCP, false, Some(json::json!(marker))))
            .expect_err("a malformed pin on TCP removes the server");
        assert_eq!(malformed_on_tcp, malformed);
        assert!(!format!("{:?}", entry(ssl, false, Some(json::json!(marker)))).contains(marker));
    }

    #[test]
    fn t38_6_4n_the_key_is_read_from_both_requests_as_a_string_or_an_array() {
        let pin = pin_of(&KeyPair::generate().unwrap());
        let read = |value: Json| -> ElectrumRpcRequest { json::from_value(value).expect("a server entry") };
        let single = read(json::json!({ "url": "a.test:1", "protocol": "SSL", "spki_sha256": pin }));
        assert_eq!(single.spki_sha256, Some(json::json!(pin)));
        let array = read(json::json!({ "url": "a.test:1", "protocol": "SSL", "spki_sha256": [pin, pin] }));
        assert_eq!(array.spki_sha256, Some(json::json!([pin, pin])));
        // Without the key, or with a null, the entry behaves exactly as before.
        assert!(read(json::json!({ "url": "a.test:1", "protocol": "SSL" }))
            .spki_sha256
            .is_none());
        assert!(
            read(json::json!({ "url": "a.test:1", "protocol": "SSL", "spki_sha256": null }))
                .spki_sha256
                .is_none()
        );
        // A malformed value does not fail the request: it removes this server when it joins the pool.
        let malformed = read(json::json!({ "url": "a.test:1", "protocol": "SSL", "spki_sha256": 7 }));
        assert!(server_spec(&malformed).is_err());
        // The key survives a round trip and an absent key stays absent.
        assert!(json::to_value(&single).unwrap().get("spki_sha256").is_some());
        assert!(json::to_value(read(json::json!({ "url": "a.test:1" })))
            .unwrap()
            .get("spki_sha256")
            .is_none());
    }

    #[test]
    fn t38_6_4n_malformed_pins_and_a_pin_on_tcp_remove_the_server_and_nothing_connects() {
        let _guard = localhost_map();
        common::block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let port = listener.local_addr().unwrap().port();
            let pin = pin_of(&KeyPair::generate().unwrap());
            let malformed_tls = ElectrumRpcRequest {
                spki_sha256: Some(json::json!("not-a-digest")),
                ..request_of(format!("{}:{}", PIN_HOST, port), ElectrumProtocol::SSL)
            };
            let pinned_tcp = ElectrumRpcRequest {
                spki_sha256: Some(json::json!(pin)),
                ..request_of(format!("127.0.0.1:{}", port), ElectrumProtocol::TCP)
            };
            let client = ElectrumClientImpl::new("PINREMOVED".to_owned(), vec![]);
            let mut removed = client.add_servers(&[malformed_tls, pinned_tcp]);
            removed.sort();
            assert_eq!(removed, vec![
                (
                    format!("127.0.0.1:{}", port),
                    "spki_sha256 needs protocol SSL".to_owned()
                ),
                (
                    format!("{}:{}", PIN_HOST, port),
                    "spki_sha256 is not a 64-digit hexadecimal SHA-256 digest".to_owned()
                ),
            ]);
            assert_eq!(client.count_connections().await, 0);
            let client = Arc::new(client);
            let _ = client.activation_probe().await;
            let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
            assert!(accepted.is_err(), "a removed server is never connected");
        });
    }

    #[test]
    fn t38_6_4n_a_mismatch_suspends_the_server_and_is_logged_at_warn_every_time() {
        let capturing = install_log_capture();
        let _guard = localhost_map();
        common::block_on(async {
            let key = KeyPair::generate().unwrap();
            let cert = certificate_for(&key, &[PIN_HOST], |_| {});
            let server = spawn_server_with_cert(&[&TLS13], cert, key.serialize_der()).await;
            let wrong = pin_of(&KeyPair::generate().unwrap());
            let request = ElectrumRpcRequest {
                spki_sha256: Some(json::json!(wrong)),
                ..request_of(format!("{}:{}", PIN_HOST, server.port), ElectrumProtocol::SSL)
            };
            let client = ElectrumClientImpl::new("PINMISMATCH".to_owned(), vec![]);
            assert!(client.add_servers(&[request]).is_empty());
            let client = Arc::new(client);
            for _ in 0..2 {
                let error = client
                    .activation_probe()
                    .await
                    .expect_err("the only server has another key");
                assert!(error.contains(SPKI_MISMATCH), "{}", error);
            }
            // Suspended, not removed.
            let snapshot = client.pool_snapshot();
            assert_eq!(snapshot.len(), 1);
            assert!(!snapshot[0].removed);
            assert!(snapshot[0].suspended_until.is_some());
            assert_eq!(client.count_connections().await, 1);

            if capturing {
                let warns: Vec<String> = captured_log_lines()
                    .into_iter()
                    .filter(|line| line.starts_with("WARN ") && line.contains("PINMISMATCH"))
                    .collect();
                let expected = format!(
                    "WARN PINMISMATCH: Electrum server {}:{}: the server's key does not match spki_sha256",
                    PIN_HOST, server.port
                );
                assert_eq!(warns, vec![expected.clone(), expected]);
            }
        });
    }

    #[test]
    fn t38_6_4m_e_the_warning_is_logged_once_per_activation_for_host_and_port_only() {
        let capturing = install_log_capture();
        let ticker = "WARNONCE";
        let pin = pin_of(&KeyPair::generate().unwrap());
        let entry = |url: &str, protocol, flag: bool, pin: Option<Json>| ElectrumRpcRequest {
            disable_cert_verification: flag,
            spki_sha256: pin,
            ..request_of(url.to_owned(), protocol)
        };
        let client = ElectrumClientImpl::new(ticker.to_owned(), vec![]);
        let removed = client.add_servers(&[
            entry("flagged.test:50002", ElectrumProtocol::SSL, true, None),
            entry("flagged-tcp.test:50001", ElectrumProtocol::TCP, true, None),
            entry(
                "flagged-pinned.test:50002",
                ElectrumProtocol::SSL,
                true,
                Some(json::json!(pin)),
            ),
            entry(
                "pinned.test:50002",
                ElectrumProtocol::SSL,
                false,
                Some(json::json!([pin, pin])),
            ),
            entry("plain.test:50002", ElectrumProtocol::SSL, false, None),
            // Removed (an IP literal has no host name): never connected, so no line.
            entry("127.0.0.1:50002", ElectrumProtocol::SSL, true, None),
        ]);
        assert_eq!(removed.len(), 1);
        if !capturing {
            return;
        }
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|line| line.contains(ticker))
            .collect();
        // T38.6.4m (e): exactly one WARN, naming `host:port` and nothing else; none for TCP, a pinned or a plain server.
        let warns: Vec<&String> = lines.iter().filter(|line| line.starts_with("WARN ")).collect();
        assert_eq!(warns, vec![&format!(
            "WARN {}: Electrum server flagged.test:50002 has disable_cert_verification set: the connection is \
             encrypted, but the server is not authenticated",
            ticker
        )]);
        // T38.6.4n: one INFO line for each pinned server, and only for those.
        let infos: Vec<&String> = lines.iter().filter(|line| line.starts_with("INFO ")).collect();
        assert_eq!(infos.len(), 2, "{:?}", lines);
        for name in ["flagged-pinned.test:50002", "pinned.test:50002"] {
            let expected = format!("INFO {}: Electrum server {} is pinned by spki_sha256", ticker, name);
            assert_eq!(
                infos.iter().filter(|line| ***line == expected).count(),
                1,
                "{:?}",
                infos
            );
        }
    }
}

/// A client with the default pool settings but the tip checks off (CRD ch.38 R38.6.4o item 11): with them on, a
/// connection sends a header request when it joins, which the tests that count request messages do not want to see.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn client_without_tip_checks() -> ElectrumClientImpl {
    let mut client = ElectrumClientImpl::new("MOCK".to_owned(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        tip_checks: false,
        ..Default::default()
    });
    client
}

/// An Electrum client connected to a loopback stub server (see `electrum_stub_server`), for tests that count request
/// messages. `script` sets the stub's canned answers before the client connects; the stub records every message
/// the client writes, and `StubServer::request_messages` returns those that are not the connection's own traffic.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn electrum_client_over_stub(
    script: impl FnOnce(&super::electrum_stub_server::StubServer),
) -> (ElectrumClient, super::electrum_stub_server::StubServer) {
    use super::electrum_stub_server::{StubBehavior, StubServer};

    common::block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        script(&server);
        let client = client_without_tip_checks();
        client
            .add_server(&server.request())
            .expect("the stub is a usable server");
        let client = Arc::new(client);
        client
            .activation_probe()
            .await
            .expect("the stub accepts the connection");
        (ElectrumClient(client), server)
    })
}

/// An Electrum client over several loopback stub servers, ranked in the given order, with the default pool settings
/// (read spreading on). Returns once every server is in the maintained set.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) async fn electrum_client_over_stubs(servers: &[&super::electrum_stub_server::StubServer]) -> ElectrumClient {
    let client = client_without_tip_checks();
    for server in servers {
        client.add_server(&server.request()).expect("a stub is a usable server");
    }
    let client = Arc::new(client);
    client
        .activation_probe()
        .await
        .expect("the stubs accept the connection");
    let started = std::time::Instant::now();
    while client.count_connected().await < servers.len() {
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the stubs did not all join the maintained set"
        );
        common::executor::Timer::sleep(0.01).await;
    }
    ElectrumClient(client)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod p2pk_batch_tests {
    use super::*;
    use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
    use crate::utxo::rpc_clients::UtxoRpcClientOps;
    use common::block_on;
    use serde_json::json;

    const P2PKH_HASH: &str = "aa";
    const P2PK_HASH: &str = "bb";

    fn script_balance(server: &StubServer) {
        server.set_scripthash_result(
            "blockchain.scripthash.get_balance",
            P2PKH_HASH,
            json!({"confirmed": 500, "unconfirmed": 20}),
        );
    }

    fn script_unspents(server: &StubServer) {
        server.set_scripthash_result(
            "blockchain.scripthash.listunspent",
            P2PKH_HASH,
            json!([
                {"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": 300},
                // The server can repeat an unspent; it is dropped as in the single-hash read.
                {"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": 300},
            ]),
        );
        server.set_scripthash_result(
            "blockchain.scripthash.listunspent",
            P2PK_HASH,
            json!([
                {"tx_hash": "02".repeat(32), "tx_pos": 1, "height": 11, "value": 70}
            ]),
        );
    }

    fn script(server: &StubServer) {
        script_balance(server);
        script_unspents(server);
    }

    #[test]
    fn balance_and_p2pk_lookups_travel_in_one_request_message() {
        let (client, server) = electrum_client_over_stub(script);
        let (balance, p2pk_unspents) = block_on(
            client
                .scripthash_get_balance_and_list_unspent(P2PKH_HASH, P2PK_HASH)
                .compat(),
        )
        .unwrap();

        assert_eq!((balance.confirmed, balance.unconfirmed), (500, 20));
        assert_eq!(p2pk_unspents.len(), 1);
        assert_eq!(p2pk_unspents[0].value, 70);

        let messages = server.request_messages();
        assert_eq!(messages.len(), 1, "one request message, not one per lookup");
        let batch = messages[0].as_array().expect("a JSON-RPC batch");
        let lookups: Vec<_> = batch
            .iter()
            .map(|r| (r["method"].as_str().unwrap(), r["params"][0].as_str().unwrap()))
            .collect();
        assert_eq!(lookups, vec![
            ("blockchain.scripthash.get_balance", P2PKH_HASH),
            ("blockchain.scripthash.listunspent", P2PK_HASH),
        ]);
    }

    #[test]
    fn unspent_lists_travel_in_one_request_message_and_keep_their_order() {
        let (client, server) = electrum_client_over_stub(script);
        let (p2pkh, p2pk) = block_on(client.scripthash_list_unspent_with_p2pk(P2PKH_HASH, P2PK_HASH).compat()).unwrap();

        assert_eq!(
            p2pkh.iter().map(|u| u.value).collect::<Vec<_>>(),
            vec![300],
            "duplicates dropped"
        );
        assert_eq!(p2pk.iter().map(|u| u.value).collect::<Vec<_>>(), vec![70]);
        let messages = server.request_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_single_hash_read_is_still_one_plain_request() {
        let (client, server) = electrum_client_over_stub(script);
        let balance = block_on(client.scripthash_get_balance(P2PKH_HASH).compat()).unwrap();
        assert_eq!(balance.confirmed, 500);
        let messages = server.request_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_object(), "not a batch");
    }

    /// One debug line per request message, naming the server and the methods and nothing else.
    #[test]
    fn every_request_message_is_logged_once_without_its_payload() {
        use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
        if !install_log_capture() {
            // Another logger owns the process, so there is nothing to read the lines from.
            return;
        }
        let (client, server) = electrum_client_over_stub(script);
        let addr = server.addr();

        block_on(
            client
                .scripthash_get_balance_and_list_unspent(P2PKH_HASH, P2PK_HASH)
                .compat(),
        )
        .unwrap();
        block_on(client.scripthash_get_balance(P2PKH_HASH).compat()).unwrap();
        assert_eq!(server.request_messages().len(), 2);

        // The client also logs its connection events with the address; the request lines are the debug ones.
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|line| line.starts_with("DEBUG electrum ") && line.contains(&addr))
            .collect();
        assert_eq!(lines.len(), 2, "one line per message: {lines:?}");
        let fields = |line: &str| -> Vec<String> {
            // "DEBUG electrum <addr> <methods> <ms>"
            line.split_whitespace().map(str::to_owned).collect()
        };
        let batch = fields(&lines[0]);
        assert_eq!(batch.len(), 5, "{lines:?}");
        assert_eq!(&batch[..3], &["DEBUG", "electrum", addr.as_str()]);
        assert_eq!(
            batch[3],
            "blockchain.scripthash.get_balance,blockchain.scripthash.listunspent"
        );
        assert!(batch[4].parse::<u64>().is_ok(), "{lines:?}");
        let single = fields(&lines[1]);
        assert_eq!(single.len(), 5, "{lines:?}");
        assert_eq!(single[3], "blockchain.scripthash.get_balance");
        assert!(single[4].parse::<u64>().is_ok(), "{lines:?}");
        // The hashes (params) are not logged.
        assert!(lines
            .iter()
            .all(|l| !l.contains(P2PKH_HASH) || l.contains("scripthash")));
        assert!(lines.iter().all(|l| !l.contains(P2PK_HASH) || l.contains("scripthash")));
    }

    #[test]
    fn a_failing_lookup_fails_the_whole_read() {
        let (client, _server) = electrum_client_over_stub(|server| {
            script_unspents(server);
            // A balance answer that is not a balance.
            server.set_result("blockchain.scripthash.get_balance", json!("not a balance"));
        });
        let res = block_on(
            client
                .scripthash_get_balance_and_list_unspent(P2PKH_HASH, P2PK_HASH)
                .compat(),
        );
        assert!(res.is_err());
    }
}
