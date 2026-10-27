//! Deterministic stand-ins for EVM nodes: a [`StubSender`] replaces the HTTP exchange of the transport so the
//! failover rules of CRD ch.35 §35.1.F can be checked without a network.

use super::*;
use crate::eth::alloy_compat::provider::provider_from_transport;
use crate::eth::alloy_compat::KdfProvider;
use serde_json::{json, Value as Json};
use std::sync::atomic::{AtomicBool, Ordering};

/// What a stub node does with one request.
pub(crate) enum Reply {
    /// HTTP 200 with this body.
    Body(String),
    /// The exchange fails (connection refused, HTTP 502, ...); the text is the lower layer's message.
    Fail(String),
    /// The node never answers; only the cutoff ends the attempt.
    Hang,
}

/// One request as a stub node saw it.
pub(crate) struct Call {
    /// The full configured URI, as the HTTP layer would have been given it.
    pub uri: String,
    /// The host of the URI.
    pub host: String,
    pub method: String,
    pub request: Json,
}

type Handler = dyn Fn(&Call) -> Reply + Send + Sync;

#[derive(Clone)]
pub(crate) struct StubSender {
    handler: Arc<Handler>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl fmt::Debug for StubSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "StubSender") }
}

impl StubSender {
    pub(crate) fn new(handler: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> Self {
        StubSender {
            handler: Arc::new(handler),
            calls: Arc::default(),
        }
    }

    /// `(host, method)` of every request that reached a node, in arrival order.
    pub(crate) fn calls(&self) -> Vec<(String, String)> { self.calls.lock().unwrap().clone() }

    /// The hosts that received a request, in arrival order.
    pub(crate) fn hosts(&self) -> Vec<String> { self.calls().into_iter().map(|(host, _)| host).collect() }

    pub(crate) fn hosts_for(&self, method: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|(_, m)| m == method)
            .map(|(host, _)| host)
            .collect()
    }

    pub(crate) fn clear(&self) { self.calls.lock().unwrap().clear() }
}

impl NodeSender for StubSender {
    fn send<'a>(&'a self, uri: &'a http::Uri, body: &'a [u8], _bound_s: f64) -> NodeSendFuture<'a> {
        let request: Json = serde_json::from_slice(body).unwrap();
        let method = match &request {
            Json::Array(_) => "batch".to_owned(),
            single => single["method"].as_str().unwrap().to_owned(),
        };
        let call = Call {
            uri: uri.to_string(),
            host: uri.host().unwrap().to_owned(),
            method,
            request,
        };
        self.calls
            .lock()
            .unwrap()
            .push((call.host.clone(), call.method.clone()));
        let reply = (self.handler)(&call);
        Box::pin(async move {
            match reply {
                Reply::Body(body) => Ok(body.into_bytes()),
                Reply::Fail(text) => Err(text),
                Reply::Hang => futures::future::pending().await,
            }
        })
    }
}

/// A JSON-RPC success body for `request` whose `result` text is exactly `result_text`.
pub(crate) fn result_body(request: &Json, result_text: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{},"result":{}}}"#, request["id"], result_text)
}

/// A JSON-RPC error body for `request`.
pub(crate) fn error_body(request: &Json, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": request["id"], "error": {"code": code, "message": message}}).to_string()
}

/// A transport over `urls` whose nodes are played by `handler`. `cutoff_s` is the per-node cutoff.
pub(crate) fn stub_transport(
    urls: &[&str],
    cutoff_s: f64,
    handler: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> (AlloyTransport, StubSender) {
    stub_transport_with_handlers(urls, cutoff_s, vec![], handler)
}

pub(crate) fn stub_transport_with_handlers(
    urls: &[&str],
    cutoff_s: f64,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    handler: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> (AlloyTransport, StubSender) {
    let sender = StubSender::new(handler);
    let transport = AlloyTransport::with_sender(
        urls.iter().map(|u| (*u).to_owned()).collect(),
        event_handlers,
        Arc::new(sender.clone()),
        cutoff_s,
    )
    .unwrap();
    (transport, sender)
}

/// Like [`stub_transport`], wrapped into the provider the coin code uses.
pub(crate) fn stub_provider(
    urls: &[&str],
    cutoff_s: f64,
    handler: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> (KdfProvider, StubSender) {
    let (transport, sender) = stub_transport(urls, cutoff_s, handler);
    (provider_from_transport(transport), sender)
}

/// Collects every log line emitted while it is installed, so a test can look for a leaked credential.
struct CapturingLogger;

static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALLED: AtomicBool = AtomicBool::new(false);
static LOGGER: CapturingLogger = CapturingLogger;

impl common::log::log_crate::Log for CapturingLogger {
    fn enabled(&self, _: &common::log::log_crate::Metadata) -> bool { true }

    fn log(&self, record: &common::log::log_crate::Record) {
        CAPTURED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{} {}", record.level(), record.args()));
    }

    fn flush(&self) {}
}

/// Collects the lines of the `log!` macro, which does not go through the `log` crate.
struct CapturingCallback;

impl common::log::LogCallback for CapturingCallback {
    fn callback(&mut self, _level: common::log::LogLevel, line: String) {
        CAPTURED.lock().unwrap_or_else(PoisonError::into_inner).push(line);
    }
}

/// Installs the capturing logger and the `log!` capture. `false` when another logger already owns the process, in which case the
/// captured lines are empty and a test must not assert that a line is present.
pub(crate) fn install_log_capture() -> bool {
    use common::log::log_crate::{set_logger, set_max_level, LevelFilter};
    if set_logger(&LOGGER).is_ok() {
        INSTALLED.store(true, Ordering::SeqCst);
    }
    set_max_level(LevelFilter::Trace);
    common::log::register_callback(CapturingCallback);
    INSTALLED.load(Ordering::SeqCst)
}

/// Every line captured so far.
pub(crate) fn captured_log_lines() -> Vec<String> { CAPTURED.lock().unwrap_or_else(PoisonError::into_inner).clone() }
