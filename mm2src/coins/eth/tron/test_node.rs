//! A loopback stand-in for a TRON full node, for the unit tests of the TRON wallet path.
//!
//! The stub is a real HTTP/1.1 server on `127.0.0.1`, so the tests drive the real HTTP layer of the node client.
//! Each connection serves one request and is closed afterwards. A test supplies a handler that maps the request
//! (endpoint path and JSON body) to a status and a body, and can read back every request the node received.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What the stub node answers to one request.
pub(super) struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    /// HTTP 200 with `body`.
    pub(super) fn ok(body: &str) -> Self {
        Reply {
            status: 200,
            body: body.to_owned(),
        }
    }

    /// `status` with `body`.
    pub(super) fn status(status: u16, body: &str) -> Self {
        Reply {
            status,
            body: body.to_owned(),
        }
    }
}

type Handler = dyn Fn(&str, &str) -> Reply + Send + Sync;

/// A running stub node. Dropping it stops the accept loop.
pub(super) struct StubNode {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl StubNode {
    /// Starts a node on a free loopback port. `handler` gets the request path (for example
    /// `/wallet/getaccount`) and the request body, and returns the answer.
    pub(super) fn start(handler: impl Fn(&str, &str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("local address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);

        let (accept_stop, accept_requests) = (stop.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (handler, requests) = (handler.clone(), accept_requests.clone());
                std::thread::spawn(move || serve(stream, &*handler, &requests));
            }
        });
        StubNode { addr, stop, requests }
    }

    /// The node's base URL, as a caller would configure it.
    pub(super) fn url(&self) -> String { format!("http://{}", self.addr) }

    /// `(path, body)` of every request the node received, in arrival order.
    pub(super) fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The bodies of the requests to `path`, parsed as JSON, in arrival order.
    pub(super) fn bodies_for(&self, path: &str) -> Vec<serde_json::Value> {
        self.requests()
            .into_iter()
            .filter(|(p, _)| p == path)
            .map(|(_, body)| serde_json::from_str(&body).expect("the client sends JSON"))
            .collect()
    }
}

impl Drop for StubNode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so that it sees the flag and ends.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Reads one HTTP request from `stream`, records it, writes the handler's answer and closes the connection.
fn serve(mut stream: TcpStream, handler: &Handler, requests: &Mutex<Vec<(String, String)>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < head_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
    requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((path.clone(), body.clone()));

    let reply = handler(&path, &body);
    let response = format!(
        "HTTP/1.1 {} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}
