//! A scripted Electrum server for tests: a real TCP listener on `127.0.0.1` that speaks just enough of
//! the Electrum line protocol to exercise the client's connection pool without a network.
//!
//! One [`StubServer`] is one server. Its behavior can be switched while the test runs, and it counts
//! the connections it accepted and the requests it received per method, and records every message it
//! received.

use common::executor::spawn;
use futures::channel::mpsc as futures_mpsc;
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

/// What a stub does with the connections and requests it gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StubBehavior {
    /// Answers `server.version` (in range unless told otherwise), `server.ping` with `null` and every
    /// other method with its canned result.
    Answer,
    /// Accepts the connection and never answers anything.
    Silent,
    /// Answers `server.version` and `server.ping`, and never any other request.
    BlackHoleRequests,
    /// Serves the first `n` connections like [`StubBehavior::Answer`] and closes the later ones at once.
    /// `RefuseAfter(0)` refuses every connection after accepting it.
    RefuseAfter(usize),
    /// Answers `server.version` with a JSON-RPC error: the offered protocol versions are rejected.
    BadVersion,
    /// Answers `server.version` and `server.ping`, and every other request with a JSON-RPC error.
    JsonRpcError,
    /// Connecting is refused. The port stays reserved by a bound socket that never listens, so the
    /// operating system cannot hand it to another server while this stub lives.
    Down,
}

/// One answer of the stub keyed by the parameters of a request.
struct ParamRule {
    method: String,
    /// The `params` array of the request (`prefix == false`), or the leading elements of it (`prefix == true`).
    params: Json,
    prefix: bool,
    /// `Ok` is the result to answer, `Err` the JSON-RPC error object.
    outcome: Result<Json, Json>,
}

enum Control {
    Push(String),
    Close,
}

struct StubState {
    behavior: Mutex<StubBehavior>,
    /// The protocol version `server.version` answers.
    version: Mutex<String>,
    /// How long the stub waits before it serves an accepted connection.
    accept_delay: Mutex<Duration>,
    /// While set, an accepted connection is not served (see [`StubServer::hold_new_connections`]).
    hold_new_connections: AtomicBool,
    results: Mutex<HashMap<String, Json>>,
    /// Methods that answer a JSON-RPC error with the given message instead of a result; they win over `results`.
    errors: Mutex<HashMap<String, String>>,
    /// Canned results of the script-hash methods, per `(method, script hash)`; they win over `results`.
    scripthash_results: Mutex<HashMap<(String, String), Json>>,
    /// Methods that answer the given JSON-RPC error object (`{"code": .., "message": ..}`) instead of a result; they
    /// win over `results`.
    error_objects: Mutex<HashMap<String, Json>>,
    /// Canned answers keyed by the parameters of the request (see [`StubServer::set_params_result`]); they win over
    /// `scripthash_results` and `results`.
    param_rules: Mutex<Vec<ParamRule>>,
    /// Every ordinary request (not `server.version` or `server.ping`) as `(method, params)`, in the order it was
    /// answered, the elements of a batch one by one.
    request_log: Mutex<Vec<(String, Json)>>,
    /// Every message (a request, or a batch of them) the stub received, in the order it arrived.
    messages: Mutex<Vec<Json>>,
    requests: Mutex<HashMap<String, usize>>,
    last_requests: Mutex<HashMap<String, Json>>,
    accepted: AtomicUsize,
    /// The tip `blockchain.headers.subscribe` answers; without one it answers its canned result.
    tip: Mutex<Option<u64>>,
    /// The header bytes (hex) the tip's header carries in `blockchain.headers.subscribe` answers and notifications.
    header_hex: Mutex<String>,
    /// The header (hex) `blockchain.block.header` answers per height; a height without one gets the default.
    block_headers: Mutex<HashMap<u64, String>>,
    /// The chain the stub serves, header (hex) per height (see [`StubServer::set_chain_header`]).
    chain: Mutex<HashMap<u64, String>>,
    default_block_header: Mutex<Option<String>>,
    /// How long the stub waits before it answers a method (CRD ch.38 R38.6.4l tests).
    reply_delays: Mutex<HashMap<String, Duration>>,
    /// The open connections, with whether each has been answered a `blockchain.headers.subscribe`.
    live: Mutex<Vec<(futures_mpsc::UnboundedSender<Control>, Arc<AtomicBool>)>>,
    listener_stop: Mutex<Option<oneshot::Sender<()>>>,
}

pub struct StubServer {
    /// The loopback address the stub listens on (`127.0.0.1` unless started with [`StubServer::start_on`]).
    host: String,
    port: u16,
    state: Arc<StubState>,
    /// While the behavior is [`StubBehavior::Down`]: a socket bound to `port` that does not listen.
    /// Connecting to it is refused, and nobody else can bind the port until the stub is dropped
    /// or switched to another behavior.
    reserved: Mutex<Option<TcpSocket>>,
}

/// Binds `127.0.0.1:port` (`0`: any free port) without listening, and returns the socket and the port.
/// `SO_REUSEADDR` is left off on purpose: with it another bind of the port could succeed.
fn reserve_port(host: &str, port: u16) -> std::io::Result<(TcpSocket, u16)> {
    let socket = TcpSocket::new_v4()?;
    let ip: std::net::Ipv4Addr = host.parse().expect("an IPv4 host");
    socket.bind((ip, port).into())?;
    let port = socket.local_addr()?.port();
    Ok((socket, port))
}

impl StubServer {
    /// Starts a stub with the given behavior on a free port of `127.0.0.1`.
    pub async fn start(behavior: StubBehavior) -> StubServer { StubServer::start_on("127.0.0.1", behavior).await }

    /// Starts a stub on `host`, a loopback address (`127.0.0.2` and so on): two stubs on two addresses are two
    /// distinct hosts for a policy that counts hosts (CRD ch.51 R100).
    pub async fn start_on(host: &str, behavior: StubBehavior) -> StubServer {
        let (port, listener, reserved) = if behavior == StubBehavior::Down {
            let (socket, port) = reserve_port(host, 0).expect("reserve the port of a down stub");
            (port, None, Some(socket))
        } else {
            let listener = TcpListener::bind((host, 0)).await.expect("bind a stub server");
            let port = listener.local_addr().expect("local address").port();
            (port, Some(listener), None)
        };
        let state = Arc::new(StubState {
            behavior: Mutex::new(behavior),
            version: Mutex::new("1.4".to_owned()),
            accept_delay: Mutex::new(Duration::ZERO),
            hold_new_connections: AtomicBool::new(false),
            results: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            scripthash_results: Mutex::new(HashMap::new()),
            error_objects: Mutex::new(HashMap::new()),
            param_rules: Mutex::new(Vec::new()),
            request_log: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
            requests: Mutex::new(HashMap::new()),
            last_requests: Mutex::new(HashMap::new()),
            accepted: AtomicUsize::new(0),
            tip: Mutex::new(None),
            header_hex: Mutex::new("00".to_owned()),
            block_headers: Mutex::new(HashMap::new()),
            chain: Mutex::new(HashMap::new()),
            default_block_header: Mutex::new(None),
            reply_delays: Mutex::new(HashMap::new()),
            live: Mutex::new(Vec::new()),
            listener_stop: Mutex::new(None),
        });
        let server = StubServer {
            host: host.to_owned(),
            port,
            state,
            reserved: Mutex::new(reserved),
        };
        if let Some(listener) = listener {
            server.serve(listener);
        }
        server
    }

    /// The address a client lists: `host:port`, the host `127.0.0.1` unless started with [`StubServer::start_on`].
    pub fn addr(&self) -> String { format!("{}:{}", self.host, self.port) }

    /// The entry of the `servers` list of an activation request.
    pub fn request(&self) -> super::ElectrumRpcRequest {
        super::ElectrumRpcRequest {
            url: self.addr(),
            protocol: Default::default(),
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        }
    }

    fn serve(&self, listener: TcpListener) {
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        *self.state.listener_stop.lock().unwrap() = Some(stop_tx);
        let state = self.state.clone();
        spawn(async move {
            loop {
                let accepted = futures::select! {
                    accepted = listener.accept().fuse() => accepted,
                    _ = stop_rx => return,
                };
                let Ok((stream, _)) = accepted else { return };
                let number = state.accepted.fetch_add(1, Ordering::SeqCst);
                let (control_tx, control_rx) = futures_mpsc::unbounded();
                let subscribed = Arc::new(AtomicBool::new(false));
                state.live.lock().unwrap().push((control_tx, subscribed.clone()));
                spawn(serve_connection(state.clone(), stream, number, control_rx, subscribed));
            }
        });
    }

    /// Switches the behavior. Switching to [`StubBehavior::Down`] closes every open connection and
    /// stops listening, keeping the port reserved by a socket that does not listen (when the system
    /// lets the port be bound again at once; otherwise the port is just closed). Switching away from
    /// it makes the reserved socket listen on the same port.
    pub async fn set_behavior(&self, behavior: StubBehavior) {
        let was = std::mem::replace(&mut *self.state.behavior.lock().unwrap(), behavior);
        if behavior == StubBehavior::Down && was != StubBehavior::Down {
            if let Some(stop) = self.state.listener_stop.lock().unwrap().take() {
                let _ = stop.send(());
            }
            self.kill_connections();
            // Let the accept loop drop the listener before the caller relies on the refusal.
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            // The listener is gone; take the port over with a socket that never listens.
            for _ in 0..50 {
                if let Ok((socket, _)) = reserve_port(&self.host, self.port) {
                    *self.reserved.lock().unwrap() = Some(socket);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } else if behavior != StubBehavior::Down && was == StubBehavior::Down {
            let reserved = self.reserved.lock().unwrap().take();
            let listener = match reserved {
                Some(socket) => socket.listen(1024).expect("listen on the reserved stub port"),
                None => TcpListener::bind((self.host.as_str(), self.port))
                    .await
                    .expect("bind the stub port again"),
            };
            self.serve(listener);
        }
    }

    /// The protocol version `server.version` answers (default `1.4`).
    pub fn set_version(&self, version: &str) { *self.state.version.lock().unwrap() = version.to_owned(); }

    /// Makes the stub wait before it serves an accepted connection.
    pub fn set_accept_delay(&self, delay: Duration) { *self.state.accept_delay.lock().unwrap() = delay; }

    /// While `hold` is set, a connection the stub accepts is not served: nothing is read from it and nothing is
    /// answered, so a client cannot finish opening a session. When the hold is lifted, the connections that wait are
    /// served as if they had just been accepted. The connections that are open already are not affected. A test uses
    /// this to keep a client from getting a new session until a step of its own has ended, without a fixed wait.
    pub fn hold_new_connections(&self, hold: bool) { self.state.hold_new_connections.store(hold, Ordering::SeqCst); }

    /// The result `method` answers (the default is `null`).
    pub fn set_result(&self, method: &str, result: Json) {
        self.state.results.lock().unwrap().insert(method.to_owned(), result);
    }

    /// Makes `method` answer a JSON-RPC error with `message` (the other methods keep their results).
    pub fn set_error(&self, method: &str, message: &str) {
        self.state
            .errors
            .lock()
            .unwrap()
            .insert(method.to_owned(), message.to_owned());
    }

    /// Makes `method` answer its canned result again (a message of [`Self::set_error`] and an object of
    /// [`Self::set_error_object`] are both forgotten).
    pub fn clear_error(&self, method: &str) {
        self.state.errors.lock().unwrap().remove(method);
        self.state.error_objects.lock().unwrap().remove(method);
    }

    /// Makes `method` answer the JSON-RPC error object `error` (for example `{"code": 1, "message": "history too large"}`)
    /// instead of a result. [`Self::clear_error`] undoes it.
    pub fn set_error_object(&self, method: &str, error: Json) {
        self.state
            .error_objects
            .lock()
            .unwrap()
            .insert(method.to_owned(), error);
    }

    /// Makes `method` answer as a server that does not know it: the JSON-RPC error `-32601`, `unknown method`.
    pub fn set_unknown_method_error(&self, method: &str) {
        self.set_error_object(method, json!({"code": -32601, "message": "unknown method"}));
    }

    /// The result `method` answers when the `params` array of the request equals `params` exactly. It wins over
    /// [`Self::set_result`] and [`Self::set_scripthash_result`]; a later call for the same method and parameters
    /// replaces an earlier one.
    pub fn set_params_result(&self, method: &str, params: Json, result: Json) {
        self.set_param_rule(method, params, false, Ok(result));
    }

    /// Like [`Self::set_params_result`], but the rule matches every request whose `params` array starts with the
    /// elements of `params_prefix`. A rule that matches the whole array wins over one that matches a prefix.
    pub fn set_params_prefix_result(&self, method: &str, params_prefix: Json, result: Json) {
        self.set_param_rule(method, params_prefix, true, Ok(result));
    }

    /// Makes `method` answer the JSON-RPC error object `error` when the `params` array of the request equals `params`
    /// exactly. It wins over [`Self::set_params_result`] of the same parameters.
    pub fn set_params_error(&self, method: &str, params: Json, error: Json) {
        self.set_param_rule(method, params, false, Err(error));
    }

    /// Forgets every rule of [`Self::set_params_result`], [`Self::set_params_prefix_result`] and
    /// [`Self::set_params_error`] of `method`.
    pub fn clear_params_rules(&self, method: &str) {
        self.state
            .param_rules
            .lock()
            .unwrap()
            .retain(|rule| rule.method != method);
    }

    fn set_param_rule(&self, method: &str, params: Json, prefix: bool, outcome: Result<Json, Json>) {
        let mut rules = self.state.param_rules.lock().unwrap();
        rules.retain(|rule| !(rule.method == method && rule.prefix == prefix && rule.params == params));
        rules.push(ParamRule {
            method: method.to_owned(),
            params,
            prefix,
            outcome,
        });
    }

    /// The `params` of every ordinary request of `method` (every method but `server.version` and `server.ping`) the
    /// stub received, in the order the requests were answered.
    pub fn request_params(&self, method: &str) -> Vec<Json> {
        self.state
            .request_log
            .lock()
            .unwrap()
            .iter()
            .filter(|(logged, _)| logged == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// The method names of every ordinary request the stub received, in the order the requests were answered.
    pub fn request_methods_in_order(&self) -> Vec<String> {
        self.state
            .request_log
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    /// Every ordinary request as `(method, params)`, in the order the requests were answered.
    pub fn request_log(&self) -> Vec<(String, Json)> { self.state.request_log.lock().unwrap().clone() }

    /// The result `method` (`blockchain.scripthash.get_balance`, `blockchain.scripthash.listunspent`, ...)
    /// answers for the script hash `scripthash`, the first parameter of the request. It wins over
    /// [`Self::set_result`]; a batch element is answered by the same rule as a lone request.
    pub fn set_scripthash_result(&self, method: &str, scripthash: &str, result: Json) {
        self.state
            .scripthash_results
            .lock()
            .unwrap()
            .insert((method.to_owned(), scripthash.to_owned()), result);
    }

    /// Closes every open connection; the behavior stays.
    pub fn kill_connections(&self) {
        for (control, _) in self.state.live.lock().unwrap().drain(..) {
            let _ = control.unbounded_send(Control::Close);
        }
    }

    /// Sends a notification (a request without an id) over every open connection.
    pub fn notify(&self, method: &str, params: Json) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        for (control, _) in self.state.live.lock().unwrap().iter() {
            let _ = control.unbounded_send(Control::Push(line.clone()));
        }
    }

    /// Makes the stub wait `delay` before it answers `method`; the delay is the latency a client measures.
    /// A delayed answer does not hold up the requests that follow it on the connection.
    pub fn set_reply_delay(&self, method: &str, delay: Duration) {
        self.state.reply_delays.lock().unwrap().insert(method.to_owned(), delay);
    }

    /// Sets the chain tip `blockchain.headers.subscribe` answers and pushes it as a header notification over
    /// every connection that has asked for headers.
    pub fn set_tip(&self, height: u64) {
        *self.state.tip.lock().unwrap() = Some(height);
        self.push_header(height);
    }

    /// Sets the header (hex) `blockchain.block.header` answers for `height` (CRD ch.38 R38.6.4o item 12).
    pub fn set_block_header(&self, height: u64, hex: &str) {
        self.state.block_headers.lock().unwrap().insert(height, hex.to_owned());
    }

    /// Sets the header (hex) of `height` in the chain the stub serves. A header of the chain is what a header
    /// notification or answer of that height carries, and what `blockchain.block.headers` (`[start, count]`) serves,
    /// concatenated and with its `count`, up to the first height without one (CRD ch.38 §38.5C amendment of
    /// 2026-11-18). A height without one carries the bytes of [`Self::set_header_hex`].
    pub fn set_chain_header(&self, height: u64, hex: &str) {
        self.state.chain.lock().unwrap().insert(height, hex.to_owned());
    }

    /// Sets the header (hex) `blockchain.block.header` answers for every height that has none of its own.
    pub fn set_default_block_header(&self, hex: &str) {
        *self.state.default_block_header.lock().unwrap() = Some(hex.to_owned());
    }

    /// Sets the header bytes (hex) every header the stub answers or pushes carries, so that two stubs at one height can
    /// answer the same header or different ones (CRD ch.38 R38.6.4o item 7). The default is `00`.
    pub fn set_header_hex(&self, hex: &str) { *self.state.header_hex.lock().unwrap() = hex.to_owned(); }

    /// Pushes a header notification with `height` over every connection that has asked for headers, without
    /// changing the tip the stub answers.
    pub fn push_header(&self, height: u64) {
        let line = json!({
            "jsonrpc": "2.0",
            "method": "blockchain.headers.subscribe",
            "params": [header_json(height, &tip_header_hex(&self.state, height))],
        })
        .to_string();
        for (control, subscribed) in self.state.live.lock().unwrap().iter() {
            if subscribed.load(Ordering::SeqCst) {
                let _ = control.unbounded_send(Control::Push(line.clone()));
            }
        }
    }

    /// How many open connections have been answered a `blockchain.headers.subscribe`.
    pub fn header_subscriptions(&self) -> usize {
        let mut live = self.state.live.lock().unwrap();
        live.retain(|(control, _)| !control.is_closed());
        live.iter()
            .filter(|(_, subscribed)| subscribed.load(Ordering::SeqCst))
            .count()
    }

    /// How many connections the stub accepted so far, refused ones included.
    pub fn accepted(&self) -> usize { self.state.accepted.load(Ordering::SeqCst) }

    /// How many connections are open now.
    pub fn open_connections(&self) -> usize {
        let mut live = self.state.live.lock().unwrap();
        live.retain(|(control, _)| !control.is_closed());
        live.len()
    }

    /// How many requests of `method` the stub received so far.
    pub fn requests(&self, method: &str) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .get(method)
            .copied()
            .unwrap_or_default()
    }

    /// The last request of `method` as the stub received it.
    pub fn last_request(&self, method: &str) -> Option<Json> {
        self.state.last_requests.lock().unwrap().get(method).cloned()
    }

    /// Every message the stub received, a batch as one entry.
    pub fn messages(&self) -> Vec<Json> { self.state.messages.lock().unwrap().clone() }

    /// The messages the stub received, but those made only of `server.version` and `server.ping` requests
    /// (the connection's own traffic): what a test counts when it counts the messages a read cost.
    pub fn request_messages(&self) -> Vec<Json> {
        let is_own_traffic =
            |request: &Json| matches!(request["method"].as_str(), Some("server.version" | "server.ping"));
        self.messages()
            .into_iter()
            .filter(|message| match message {
                Json::Array(batch) => !batch.iter().all(is_own_traffic),
                single => !is_own_traffic(single),
            })
            .collect()
    }

    /// How many requests of every method but `server.version` and `server.ping` the stub received.
    pub fn ordinary_requests(&self) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method.as_str() != "server.version" && method.as_str() != "server.ping")
            .map(|(_, count)| *count)
            .sum()
    }
}

/// The header bytes (hex) a tip at `height` carries: its header in the chain the stub serves, else the stub's header bytes.
fn tip_header_hex(state: &StubState, height: u64) -> String {
    state
        .chain
        .lock()
        .unwrap()
        .get(&height)
        .cloned()
        .unwrap_or_else(|| state.header_hex.lock().unwrap().clone())
}

/// The header of a tip at `height` with the bytes `hex`, as `blockchain.headers.subscribe` answers and notifies it.
fn header_json(height: u64, hex: &str) -> Json { json!({"height": height, "hex": hex}) }

/// The outcome of the rule that fits `method` and `params`: a rule that matches the whole `params` array wins over one
/// that matches a prefix, and a later rule over an earlier one.
fn find_param_rule(state: &StubState, method: &str, params: &Json) -> Option<Result<Json, Json>> {
    let rules = state.param_rules.lock().unwrap();
    let fits = |rule: &&ParamRule| rule.method == method && !rule.prefix && rule.params == *params;
    let fits_prefix = |rule: &&ParamRule| {
        rule.method == method
            && rule.prefix
            && match (rule.params.as_array(), params.as_array()) {
                (Some(prefix), Some(params)) => params.starts_with(prefix),
                _ => false,
            }
    };
    rules
        .iter()
        .rev()
        .find(fits)
        .or_else(|| rules.iter().rev().find(fits_prefix))
        .map(|rule| rule.outcome.clone())
}

/// Answers one request; `None` stays unanswered. `subscribed` is set when the connection is answered a header
/// subscription.
fn answer(state: &StubState, behavior: StubBehavior, request: &Json, subscribed: &AtomicBool) -> Option<Json> {
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    *state.requests.lock().unwrap().entry(method.clone()).or_default() += 1;
    state
        .last_requests
        .lock()
        .unwrap()
        .insert(method.clone(), request.clone());
    if !matches!(method.as_str(), "server.version" | "server.ping") {
        state
            .request_log
            .lock()
            .unwrap()
            .push((method.clone(), request["params"].clone()));
    }
    let id = request["id"].clone();
    let result = |result: Json| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let error = |message: &str| Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -1, "message": message}}));
    let error_object = |error: Json| Some(json!({"jsonrpc": "2.0", "id": id, "error": error}));
    let rule = find_param_rule(state, &method, &request["params"]);
    match (behavior, method.as_str()) {
        (StubBehavior::Silent, _) | (StubBehavior::Down, _) => None,
        (StubBehavior::BadVersion, "server.version") => error("unsupported protocol version"),
        (_, "server.version") => {
            let version = state.version.lock().unwrap().clone();
            result(json!(["StubElectrum 0.1", version]))
        },
        (_, "server.ping") => result(Json::Null),
        (StubBehavior::BlackHoleRequests, _) => None,
        (StubBehavior::JsonRpcError, _) => error("stub error"),
        _ if state.errors.lock().unwrap().contains_key(&method) => {
            let message = state.errors.lock().unwrap().get(&method).cloned().unwrap_or_default();
            error(&message)
        },
        _ if state.error_objects.lock().unwrap().contains_key(&method) => {
            let object = state
                .error_objects
                .lock()
                .unwrap()
                .get(&method)
                .cloned()
                .unwrap_or_default();
            error_object(object)
        },
        _ if rule.is_some() => match rule.unwrap_or(Ok(Json::Null)) {
            Ok(canned) => result(canned),
            Err(object) => error_object(object),
        },
        (_, "blockchain.block.header")
            if request["params"][0].as_u64().map_or(false, |height| {
                state.block_headers.lock().unwrap().contains_key(&height)
                    || state.default_block_header.lock().unwrap().is_some()
            }) =>
        {
            let height = request["params"][0].as_u64().unwrap_or_default();
            let hex = state
                .block_headers
                .lock()
                .unwrap()
                .get(&height)
                .cloned()
                .or_else(|| state.default_block_header.lock().unwrap().clone())
                .unwrap_or_default();
            result(json!(hex))
        },
        (_, "blockchain.headers.subscribe") if state.tip.lock().unwrap().is_some() => {
            subscribed.store(true, Ordering::SeqCst);
            let height = state.tip.lock().unwrap().unwrap_or_default();
            let hex = tip_header_hex(state, height);
            result(header_json(height, &hex))
        },
        (_, "blockchain.block.headers")
            if request["params"][0]
                .as_u64()
                .map_or(false, |start| state.chain.lock().unwrap().contains_key(&start)) =>
        {
            let start = request["params"][0].as_u64().unwrap_or_default();
            let wanted = request["params"][1].as_u64().unwrap_or_default();
            let chain = state.chain.lock().unwrap();
            let mut hex = String::new();
            let mut count = 0u64;
            while count < wanted {
                let Some(header) = chain.get(&(start + count)) else {
                    break;
                };
                hex.push_str(header);
                count += 1;
            }
            result(json!({"count": count, "hex": hex, "max": 2016}))
        },
        _ => {
            let scripthash = request["params"][0].as_str().unwrap_or_default().to_owned();
            let canned = state
                .scripthash_results
                .lock()
                .unwrap()
                .get(&(method.clone(), scripthash))
                .cloned();
            result(canned.unwrap_or_else(|| {
                state
                    .results
                    .lock()
                    .unwrap()
                    .get(&method)
                    .cloned()
                    .unwrap_or(Json::Null)
            }))
        },
    }
}

async fn serve_connection(
    state: Arc<StubState>,
    stream: TcpStream,
    number: usize,
    mut control_rx: futures_mpsc::UnboundedReceiver<Control>,
    subscribed: Arc<AtomicBool>,
) {
    let delay = *state.accept_delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    while state.hold_new_connections.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if let StubBehavior::RefuseAfter(allowed) = *state.behavior.lock().unwrap() {
        if number >= allowed {
            return;
        }
    }
    let (read, write) = tokio::io::split(stream);
    // A delayed answer is written by a task of its own, so the writer is shared.
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = futures::select! {
            line = lines.next_line().fuse() => match line {
                Ok(Some(line)) => line,
                _ => return,
            },
            control = control_rx.next() => match control {
                Some(Control::Push(line)) => {
                    if write.lock().await.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        return;
                    }
                    continue;
                },
                Some(Control::Close) | None => return,
            },
        };
        let Ok(request) = serde_json::from_str::<Json>(&line) else {
            continue;
        };
        state.messages.lock().unwrap().push(request.clone());
        let behavior = match *state.behavior.lock().unwrap() {
            StubBehavior::RefuseAfter(_) => StubBehavior::Answer,
            other => other,
        };
        let methods: Vec<String> = match &request {
            Json::Array(batch) => batch
                .iter()
                .map(|item| item["method"].as_str().unwrap_or_default().to_owned())
                .collect(),
            single => vec![single["method"].as_str().unwrap_or_default().to_owned()],
        };
        let response = match &request {
            Json::Array(batch) => {
                let answers: Vec<Json> = batch
                    .iter()
                    .filter_map(|item| answer(&state, behavior, item, &subscribed))
                    .collect();
                if answers.is_empty() {
                    None
                } else {
                    Some(Json::Array(answers))
                }
            },
            single => answer(&state, behavior, single, &subscribed),
        };
        let Some(response) = response else { continue };
        let delay = {
            let delays = state.reply_delays.lock().unwrap();
            methods.iter().filter_map(|method| delays.get(method)).max().copied()
        };
        match delay {
            Some(delay) if !delay.is_zero() => {
                let write = write.clone();
                spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = write.lock().await.write_all(format!("{response}\n").as_bytes()).await;
                });
            },
            _ => {
                if write
                    .lock()
                    .await
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;

    /// Sends `requests` (one line each) over one connection and returns the answers in the order they arrive.
    async fn ask(server: &StubServer, requests: &[Json]) -> Vec<Json> {
        let stream = TcpStream::connect(server.addr()).await.expect("connect to the stub");
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();
        let mut answers = Vec::new();
        for request in requests {
            write
                .write_all(format!("{request}\n").as_bytes())
                .await
                .expect("write a request");
            let line = lines.next_line().await.expect("read an answer").expect("an answer");
            answers.push(serde_json::from_str(&line).expect("an answer is JSON"));
        }
        answers
    }

    fn call(id: u64, method: &str, params: Json) -> Json {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[test]
    fn params_results_match_exactly_or_by_prefix_and_the_exact_rule_wins() {
        block_on(async {
            let server = StubServer::start(StubBehavior::Answer).await;
            server.set_result("m", json!("plain"));
            server.set_params_prefix_result("m", json!(["a"]), json!("prefix"));
            server.set_params_result("m", json!(["a", "b"]), json!("exact"));
            let answers = ask(&server, &[
                call(1, "m", json!(["a", "b"])),
                call(2, "m", json!(["a", "c"])),
                call(3, "m", json!(["z"])),
                call(4, "m", json!(["a", "b", "c"])),
            ])
            .await;
            let results: Vec<_> = answers.iter().map(|a| a["result"].clone()).collect();
            assert_eq!(results, [
                json!("exact"),
                json!("prefix"),
                json!("plain"),
                json!("prefix")
            ]);
            // A later rule for the same parameters replaces the earlier one.
            server.set_params_result("m", json!(["a", "b"]), json!("replaced"));
            let answers = ask(&server, &[call(5, "m", json!(["a", "b"]))]).await;
            assert_eq!(answers[0]["result"], json!("replaced"));
        });
    }

    #[test]
    fn params_errors_and_error_objects_answer_the_exact_error_and_clear_error_undoes_them() {
        block_on(async {
            let server = StubServer::start(StubBehavior::Answer).await;
            server.set_result("m", json!("plain"));
            server.set_params_result("m", json!(["a"]), json!("result"));
            server.set_params_error("m", json!(["a"]), json!({"code": 7, "message": "only this one"}));
            server.set_error_object("n", json!({"code": 1, "message": "history too large"}));
            server.set_unknown_method_error("u");
            let answers = ask(&server, &[
                call(1, "m", json!(["a"])),
                call(2, "m", json!(["b"])),
                call(3, "n", json!([])),
                call(4, "u", json!([])),
            ])
            .await;
            assert_eq!(answers[0]["error"], json!({"code": 7, "message": "only this one"}));
            assert_eq!(answers[1]["result"], json!("plain"));
            assert_eq!(answers[2]["error"], json!({"code": 1, "message": "history too large"}));
            assert_eq!(
                answers[3]["error"],
                json!({"code": -32601, "message": "unknown method"})
            );

            server.clear_error("n");
            server.clear_error("u");
            server.clear_params_rules("m");
            let answers = ask(&server, &[
                call(5, "n", json!([])),
                call(6, "u", json!([])),
                call(7, "m", json!(["a"])),
            ])
            .await;
            assert_eq!(answers[0]["result"], Json::Null);
            assert_eq!(answers[1]["result"], Json::Null);
            assert_eq!(answers[2]["result"], json!("plain"));
        });
    }

    #[test]
    fn the_request_log_keeps_the_order_and_the_params_of_ordinary_requests_only() {
        block_on(async {
            let server = StubServer::start(StubBehavior::Answer).await;
            let _ = ask(&server, &[
                call(1, "server.version", json!(["x", "1.4"])),
                call(2, "a", json!([1])),
                call(3, "server.ping", json!([])),
                call(4, "b", json!([2])),
                call(5, "a", json!([3])),
            ])
            .await;
            assert_eq!(server.request_methods_in_order(), ["a", "b", "a"]);
            assert_eq!(server.request_params("a"), [json!([1]), json!([3])]);
            assert_eq!(server.request_params("b"), [json!([2])]);
            assert!(server.request_params("server.ping").is_empty());
            assert_eq!(server.request_log().len(), 3);
        });
    }
}
