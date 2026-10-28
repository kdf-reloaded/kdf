//! A scripted Electrum server for tests: a real TCP listener on `127.0.0.1` that speaks just enough of
//! the Electrum line protocol to exercise the client's connection pool without a network.
//!
//! One [`StubServer`] is one server. Its behavior can be switched while the test runs, and it counts
//! the connections it accepted and the requests it received per method.

use common::executor::spawn;
use futures::channel::mpsc as futures_mpsc;
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    results: Mutex<HashMap<String, Json>>,
    requests: Mutex<HashMap<String, usize>>,
    last_requests: Mutex<HashMap<String, Json>>,
    accepted: AtomicUsize,
    live: Mutex<Vec<futures_mpsc::UnboundedSender<Control>>>,
    listener_stop: Mutex<Option<oneshot::Sender<()>>>,
}

pub struct StubServer {
    port: u16,
    state: Arc<StubState>,
    /// While the behavior is [`StubBehavior::Down`]: a socket bound to `port` that does not listen.
    /// Connecting to it is refused, and nobody else can bind the port until the stub is dropped
    /// or switched to another behavior.
    reserved: Mutex<Option<TcpSocket>>,
}

/// Binds `127.0.0.1:port` (`0`: any free port) without listening, and returns the socket and the port.
/// `SO_REUSEADDR` is left off on purpose: with it another bind of the port could succeed.
fn reserve_port(port: u16) -> std::io::Result<(TcpSocket, u16)> {
    let socket = TcpSocket::new_v4()?;
    socket.bind(([127, 0, 0, 1], port).into())?;
    let port = socket.local_addr()?.port();
    Ok((socket, port))
}

impl StubServer {
    /// Starts a stub with the given behavior on a free port of `127.0.0.1`.
    pub async fn start(behavior: StubBehavior) -> StubServer {
        let (port, listener, reserved) = if behavior == StubBehavior::Down {
            let (socket, port) = reserve_port(0).expect("reserve the port of a down stub");
            (port, None, Some(socket))
        } else {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a stub server");
            let port = listener.local_addr().expect("local address").port();
            (port, Some(listener), None)
        };
        let state = Arc::new(StubState {
            behavior: Mutex::new(behavior),
            version: Mutex::new("1.4".to_owned()),
            accept_delay: Mutex::new(Duration::ZERO),
            results: Mutex::new(HashMap::new()),
            requests: Mutex::new(HashMap::new()),
            last_requests: Mutex::new(HashMap::new()),
            accepted: AtomicUsize::new(0),
            live: Mutex::new(Vec::new()),
            listener_stop: Mutex::new(None),
        });
        let server = StubServer {
            port,
            state,
            reserved: Mutex::new(reserved),
        };
        if let Some(listener) = listener {
            server.serve(listener);
        }
        server
    }

    /// The address a client lists: `127.0.0.1:port`.
    pub fn addr(&self) -> String { format!("127.0.0.1:{}", self.port) }

    /// The entry of the `servers` list of an activation request.
    pub fn request(&self) -> super::ElectrumRpcRequest {
        super::ElectrumRpcRequest {
            url: self.addr(),
            protocol: Default::default(),
            disable_cert_verification: false,
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
                state.live.lock().unwrap().push(control_tx);
                spawn(serve_connection(state.clone(), stream, number, control_rx));
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
                if let Ok((socket, _)) = reserve_port(self.port) {
                    *self.reserved.lock().unwrap() = Some(socket);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } else if behavior != StubBehavior::Down && was == StubBehavior::Down {
            let reserved = self.reserved.lock().unwrap().take();
            let listener = match reserved {
                Some(socket) => socket.listen(1024).expect("listen on the reserved stub port"),
                None => TcpListener::bind(("127.0.0.1", self.port))
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

    /// The result `method` answers (the default is `null`).
    pub fn set_result(&self, method: &str, result: Json) {
        self.state.results.lock().unwrap().insert(method.to_owned(), result);
    }

    /// Closes every open connection; the behavior stays.
    pub fn kill_connections(&self) {
        for control in self.state.live.lock().unwrap().drain(..) {
            let _ = control.unbounded_send(Control::Close);
        }
    }

    /// Sends a notification (a request without an id) over every open connection.
    pub fn notify(&self, method: &str, params: Json) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        for control in self.state.live.lock().unwrap().iter() {
            let _ = control.unbounded_send(Control::Push(line.clone()));
        }
    }

    /// How many connections the stub accepted so far, refused ones included.
    pub fn accepted(&self) -> usize { self.state.accepted.load(Ordering::SeqCst) }

    /// How many connections are open now.
    pub fn open_connections(&self) -> usize {
        let mut live = self.state.live.lock().unwrap();
        live.retain(|control| !control.is_closed());
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

/// Answers one request; `None` stays unanswered.
fn answer(state: &StubState, behavior: StubBehavior, request: &Json) -> Option<Json> {
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    *state.requests.lock().unwrap().entry(method.clone()).or_default() += 1;
    state
        .last_requests
        .lock()
        .unwrap()
        .insert(method.clone(), request.clone());
    let id = request["id"].clone();
    let result = |result: Json| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let error = |message: &str| Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -1, "message": message}}));
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
        _ => result(
            state
                .results
                .lock()
                .unwrap()
                .get(&method)
                .cloned()
                .unwrap_or(Json::Null),
        ),
    }
}

async fn serve_connection(
    state: Arc<StubState>,
    stream: TcpStream,
    number: usize,
    mut control_rx: futures_mpsc::UnboundedReceiver<Control>,
) {
    let delay = *state.accept_delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    if let StubBehavior::RefuseAfter(allowed) = *state.behavior.lock().unwrap() {
        if number >= allowed {
            return;
        }
    }
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = futures::select! {
            line = lines.next_line().fuse() => match line {
                Ok(Some(line)) => line,
                _ => return,
            },
            control = control_rx.next() => match control {
                Some(Control::Push(line)) => {
                    if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
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
        let behavior = match *state.behavior.lock().unwrap() {
            StubBehavior::RefuseAfter(_) => StubBehavior::Answer,
            other => other,
        };
        let response = match &request {
            Json::Array(batch) => {
                let answers: Vec<Json> = batch.iter().filter_map(|item| answer(&state, behavior, item)).collect();
                if answers.is_empty() {
                    None
                } else {
                    Some(Json::Array(answers))
                }
            },
            single => answer(&state, behavior, single),
        };
        if let Some(response) = response {
            if write.write_all(format!("{response}\n").as_bytes()).await.is_err() {
                return;
            }
        }
    }
}
