//! # Purpose
//!
//! An [`alloy::transports::Transport`] implementation behind which the EVM coin family reaches its JSON-RPC
//! nodes. It realizes the multi-node access contract of CRD ch.35 §35.1.F (R35.1.8 -- R35.1.11, R35.1.14 --
//! R35.1.16):
//!
//! 1. A request is tried on one node at a time in the current order and ends with the first node that
//!    answers with a decodable, successful result. A connection failure, a non-2xx status, an undecodable body,
//!    a batch envelope answering a single request, a JSON-RPC error object and the per-node cutoff are all node
//!    failures that advance the request to the next node.
//! 2. The node that answers is rotated to the front of the order (sticky last-good node), the failed nodes
//!    follow in their previous relative order.
//! 3. Every node attempt is bounded by a wall-clock cutoff that covers the whole attempt.
//! 4. Every outbound request and inbound response fires the `RpcTransportEventHandler` callbacks so the
//!    metrics layer keeps counting bytes per coin.
//!
//! # Public exports
//!
//! - [`AlloyTransport`] — the cloneable, multi-URL, metrics-instrumented transport. Implements
//!   `tower::Service<RequestPacket>` and therefore blanket-implements [`alloy::transports::Transport`].
//! - [`NODE_CUTOFF_S`] / [`DIRECT_NODE_TIMEOUT_S`] — the two per-attempt bounds.
//!
//! # Invariants
//!
//! - **JSON-RPC wire bytes are unchanged.** Outgoing requests are serialised by alloy's own
//!   [`RequestPacket::Serialize`] impl. A successful response is handed on as the node sent it.
//! - **Concurrent requests.** The node order lives behind a short `std` lock that is never held across an
//!   `.await`. A request works on a snapshot of the order and, when a node answers, writes the rotated order
//!   back; with concurrent requests the last writer wins (CRD ch.35 D35.8 (iii)).
//! - **Node naming.** Nothing this module logs or returns names a node by more than scheme, host and port
//!   (R35.1.14): lower-layer texts are scrubbed of the node's URL before they are used.
//! - **Node log lines.** Every log line that names a node starts with `[<ticker>]` and carries
//!   ` (node <i> of <n>)` right after the host (R35.1.16); `<i>` is the position the coin's activation request
//!   gave the node, which each [`Node`] carries, so a single-node (direct) transport reports the node's place in
//!   the coin's list and a copy made by `with_own_order` reports the same places.
//! - **`Send + Sync + Clone + 'static`.** Required by the alloy [`Transport`] blanket impl. Internal state is
//!   `Arc`-shared so cloning the transport shares the node order.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use alloy::rpc::json_rpc::{ErrorPayload, RequestPacket, ResponsePacket, ResponsePayload};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
use common::executor::Timer;
use common::log::{debug, warn};
use futures::future::{select, Either};
use tower::Service;

use crate::node_url::{host_only, scrub_node_url, ListedNode, NodeSlot};
use crate::{RpcTransportEventHandler, RpcTransportEventHandlerShared};

/// Per-node bound of the failover path (CRD ch.35 R35.1.10): one attempt, from connecting to the decoded body,
/// is abandoned after this many seconds and the request moves on to the next node.
pub const NODE_CUTOFF_S: f64 = 10.;

/// Per-request bound of a provider that talks to one node directly, outside the failover path (the signed
/// transaction broadcast and the nonce reads of CRD ch.35 R35.1.12 use it): no 10 s cutoff, the lower-layer
/// bound of the reference.
pub const DIRECT_NODE_TIMEOUT_S: f64 = 30.;

/// Transport backing alloy's JSON-RPC pipeline for the EVM coin family.
///
/// Construct via [`AlloyTransport::with_event_handlers`]. Clones share the node order.
#[derive(Clone, Debug)]
pub struct AlloyTransport {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    nodes: Vec<Node>,
    /// Indices into `nodes`, in the order the next request starts with (CRD ch.35 R35.1.9). Never held across
    /// an `.await`.
    order: Mutex<Vec<usize>>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    sender: Arc<dyn NodeSender>,
    /// Seconds one node attempt may take.
    cutoff_s: f64,
}

#[derive(Clone)]
struct Node {
    /// The configured URL; it is a credential, so it is only ever used to send and to scrub.
    configured: String,
    uri: http::Uri,
    /// The host-only form of the node (CRD ch.35 R35.1.14): the only way the node is named.
    label: String,
    /// The coin's ticker and the node's position in the coin's list as the request gave it (R35.1.16).
    slot: NodeSlot,
}

/// The debug form names the node by its host-only form only (CRD ch.35 R35.1.17).
impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node").field("node", &self.label).finish()
    }
}

impl Node {
    fn scrub(&self, text: &str) -> String { scrub_node_url(text, &self.configured) }

    /// A log line that names this node in the R35.1.14 / R35.1.16 form.
    fn line(&self, before: &str, after: &str) -> String { self.slot.line(before, &self.label, after) }
}

/// The result of one HTTP exchange with one node: the body of a 2xx answer, or a description of the failure.
pub(crate) type NodeSendFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>;

/// Performs the HTTP exchange with a single node. Production uses [`HttpSender`]; unit tests inject a stub so
/// that the failover rules can be checked without a network.
pub(crate) trait NodeSender: fmt::Debug + Send + Sync + 'static {
    /// POSTs `body` to `uri`. A non-2xx status is a failure, not a body. The returned text may carry the
    /// URL: the caller scrubs it. `bound_s` is the attempt's own bound: everything the exchange starts must
    /// have ended by then (the caller's [`within_cutoff`] only stops waiting, it cannot stop spawned work).
    fn send<'a>(&'a self, uri: &'a http::Uri, body: &'a [u8], bound_s: f64) -> NodeSendFuture<'a>;
}

#[derive(Debug)]
struct HttpSender;

impl NodeSender for HttpSender {
    fn send<'a>(&'a self, uri: &'a http::Uri, body: &'a [u8], bound_s: f64) -> NodeSendFuture<'a> {
        Box::pin(send_http(uri, body, bound_s))
    }
}

impl AlloyTransport {
    /// Build a transport that fails over between `nodes` with the [`NODE_CUTOFF_S`] per-node cutoff and a list of
    /// metrics handlers. Returns `Err` if any URL cannot be parsed as an [`http::Uri`].
    pub fn with_event_handlers(
        nodes: Vec<ListedNode>,
        event_handlers: Vec<RpcTransportEventHandlerShared>,
    ) -> Result<Self, String> {
        Self::with_sender(nodes, event_handlers, Arc::new(HttpSender), NODE_CUTOFF_S)
    }

    /// Like [`Self::with_event_handlers`] for a single node that is reached outside the failover path, with the
    /// [`DIRECT_NODE_TIMEOUT_S`] bound instead of the cutoff.
    pub fn direct(node: ListedNode, event_handlers: Vec<RpcTransportEventHandlerShared>) -> Result<Self, String> {
        Self::with_sender(vec![node], event_handlers, Arc::new(HttpSender), DIRECT_NODE_TIMEOUT_S)
    }

    pub(crate) fn with_sender(
        nodes: Vec<ListedNode>,
        event_handlers: Vec<RpcTransportEventHandlerShared>,
        sender: Arc<dyn NodeSender>,
        cutoff_s: f64,
    ) -> Result<Self, String> {
        let nodes = nodes
            .into_iter()
            .map(|ListedNode { url: configured, slot }| {
                // The parse error of `http::Uri` does not echo its input, so it is safe to return.
                let uri = configured.parse::<http::Uri>().map_err(|e| e.to_string())?;
                let label = host_only(&configured);
                Ok(Node {
                    configured,
                    uri,
                    label,
                    slot,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let order = Mutex::new((0..nodes.len()).collect());
        Ok(Self {
            inner: Arc::new(Inner {
                nodes,
                order,
                event_handlers,
                sender,
                cutoff_s,
            }),
        })
    }

    /// A transport over the same nodes that starts from this transport's *current* order and evolves it on its
    /// own from then on (CRD ch.35 R35.1.7: a token that starts from a copy of its platform's order).
    pub(crate) fn with_own_order(&self) -> Self {
        let nodes = self.inner.nodes.clone();
        let order = Mutex::new(self.inner.current_order());
        Self {
            inner: Arc::new(Inner {
                nodes,
                order,
                event_handlers: self.inner.event_handlers.clone(),
                sender: self.inner.sender.clone(),
                cutoff_s: self.inner.cutoff_s,
            }),
        }
    }

    /// `[<ticker>] <before><host> (node <i> of <n>)<after>` for the first node of this transport; the line of a
    /// provider that talks to one node on its own (CRD ch.35 R35.1.12, R35.1.16). `None` for a transport without
    /// nodes.
    pub(crate) fn first_node_line(&self, before: &str, after: &str) -> Option<String> {
        self.inner.nodes.first().map(|node| node.line(before, after))
    }

    /// The host-only names of the nodes in the order the next request starts with.
    #[cfg(test)]
    pub(crate) fn order_labels(&self) -> Vec<String> {
        let inner = &self.inner;
        inner
            .current_order()
            .into_iter()
            .map(|i| inner.nodes[i].label.clone())
            .collect()
    }
}

impl Inner {
    fn current_order(&self) -> Vec<usize> { self.order.lock().unwrap_or_else(PoisonError::into_inner).clone() }

    fn set_order(&self, order: Vec<usize>) { *self.order.lock().unwrap_or_else(PoisonError::into_inner) = order; }
}

impl Service<RequestPacket> for AlloyTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let inner = self.inner.clone();
        Box::pin(send_packet(inner, req))
    }
}

/// Why one node attempt did not produce an answer.
#[derive(Debug)]
enum NodeFailure {
    /// The per-node cutoff elapsed.
    Cutoff,
    /// The connection, the status, the body or its framing was unusable. The text is already scrubbed.
    Transport(String),
    /// The node answered 2xx with a JSON-RPC error object.
    JsonRpc(ErrorPayload),
}

/// What a failure is called in the all-failed text.
fn describe(failure: &NodeFailure, cutoff_s: f64) -> String {
    match failure {
        NodeFailure::Cutoff => format!("no answer within {cutoff_s} s"),
        NodeFailure::Transport(text) => text.clone(),
        NodeFailure::JsonRpc(payload) => format!("error code {}: {}", payload.code, payload.message),
    }
}

/// The method label used in logs: the method of a single request, the methods of a batch joined by commas.
fn method_label(req: &RequestPacket) -> String { req.method_names().collect::<Vec<_>>().join(",") }

/// The warn line of a node that answered with a JSON-RPC error object (CRD ch.35 R35.1.15, R35.1.16).
fn answered_with_error_line(node: &Node, method: &str, payload: &ErrorPayload) -> String {
    node.line(
        "EVM RPC ",
        &format!(
            " answered {method} with error {}: {}",
            payload.code,
            node.scrub(&payload.message)
        ),
    )
}

/// The debug line of a node attempt that failed in any other way (CRD ch.35 R35.1.8, R35.1.10, R35.1.16).
fn attempt_failed_line(node: &Node, method: &str, failure: &NodeFailure, cutoff_s: f64) -> String {
    node.line(
        "EVM RPC ",
        &format!(" failed {method}: {}", describe(failure, cutoff_s)),
    )
}

/// Serialise the [`RequestPacket`], try the nodes in the current order (CRD ch.35 R35.1.8), fire metrics
/// callbacks, rotate the answering node to the front (R35.1.9) and return its answer, or the all-failed error
/// (R35.1.11).
async fn send_packet(inner: Arc<Inner>, req: RequestPacket) -> Result<ResponsePacket, TransportError> {
    let method = method_label(&req);
    let is_batch = matches!(req, RequestPacket::Batch(_));
    let body = req.serialize().map_err(TransportError::ser_err)?;
    let body_bytes = body.get().as_bytes().to_vec();

    let order = inner.current_order();
    let mut failures: Vec<(usize, NodeFailure)> = Vec::new();
    for (position, &index) in order.iter().enumerate() {
        let node = &inner.nodes[index];
        inner.event_handlers.on_outgoing_request(&body_bytes);

        match attempt(&inner, node, is_batch, &body_bytes).await {
            Ok(packet) => {
                // The answering node first, the nodes after it behind it, the nodes that failed before it last
                // in their previous relative order.
                let mut rotated = Vec::with_capacity(order.len());
                rotated.push(index);
                rotated.extend_from_slice(&order[position + 1..]);
                rotated.extend_from_slice(&order[..position]);
                inner.set_order(rotated);
                return Ok(packet);
            },
            Err(failure) => {
                if let NodeFailure::JsonRpc(payload) = &failure {
                    warn!("{}", answered_with_error_line(node, &method, payload));
                } else {
                    debug!("{}", attempt_failed_line(node, &method, &failure, inner.cutoff_s));
                }
                failures.push((index, failure));
            },
        }
    }

    Err(all_failed(&inner, failures))
}

/// One attempt on one node, bounded by the cutoff, classified per CRD ch.35 R35.1.8.
async fn attempt(inner: &Inner, node: &Node, is_batch: bool, body: &[u8]) -> Result<ResponsePacket, NodeFailure> {
    let sent = within_cutoff(inner.cutoff_s, inner.sender.send(&node.uri, body, inner.cutoff_s)).await;
    let response_bytes = match sent {
        None => return Err(NodeFailure::Cutoff),
        Some(Err(text)) => return Err(NodeFailure::Transport(node.scrub(&text))),
        Some(Ok(bytes)) => bytes,
    };
    inner.event_handlers.on_incoming_response(&response_bytes);
    decode_answer(&response_bytes, is_batch)
}

/// Decodes the body of a 2xx answer. A JSON-RPC error object, an undecodable body and a framing that does not
/// match the request (a batch for a single request or the other way round) are node failures; for a batch, an
/// error in any element fails the node as a whole (CRD ch.35 D35.8 (ii)).
fn decode_answer(bytes: &[u8], is_batch: bool) -> Result<ResponsePacket, NodeFailure> {
    let packet = serde_json::from_slice::<ResponsePacket>(bytes)
        .map_err(|_| NodeFailure::Transport("undecodable JSON-RPC response body".to_owned()))?;
    match (&packet, is_batch) {
        (ResponsePacket::Single(response), false) => match &response.payload {
            ResponsePayload::Failure(payload) => Err(NodeFailure::JsonRpc(payload.clone())),
            ResponsePayload::Success(_) => Ok(packet),
        },
        (ResponsePacket::Batch(responses), true) => {
            let failed = responses.iter().find_map(|response| match &response.payload {
                ResponsePayload::Failure(payload) => Some(payload.clone()),
                ResponsePayload::Success(_) => None,
            });
            match failed {
                Some(payload) => Err(NodeFailure::JsonRpc(payload)),
                None => Ok(packet),
            }
        },
        (ResponsePacket::Batch(_), false) => Err(NodeFailure::Transport(
            "a batch response answered a single request".to_owned(),
        )),
        (ResponsePacket::Single(_), true) => Err(NodeFailure::Transport(
            "a single response answered a batch request".to_owned(),
        )),
    }
}

/// The error of a request that every node failed (CRD ch.35 R35.1.11).
fn all_failed(inner: &Inner, failures: Vec<(usize, NodeFailure)>) -> TransportError {
    let last_non_cutoff = failures
        .iter()
        .rev()
        .find(|(_, failure)| !matches!(failure, NodeFailure::Cutoff));
    match last_non_cutoff {
        // The responding node's own error, naming that node (R35.1.15).
        Some((index, NodeFailure::JsonRpc(payload))) => {
            let node = &inner.nodes[*index];
            TransportError::ErrorResp(ErrorPayload {
                code: payload.code,
                message: format!("{} (node {})", node.scrub(&payload.message), node.label).into(),
                data: payload.data.clone(),
            })
        },
        // A transport-class failure: every host tried, once, with its failure (R35.1.14).
        Some(_) => {
            let mut tried: Vec<(&str, String)> = Vec::new();
            for (index, failure) in &failures {
                let label = inner.nodes[*index].label.as_str();
                // The host leads its entry; a lower layer that repeats it inside its own text would name it twice.
                let text = describe(failure, inner.cutoff_s).replace(label, "the node");
                match tried.iter_mut().find(|(seen, _)| *seen == label) {
                    Some(entry) => entry.1 = text,
                    None => tried.push((label, text)),
                }
            }
            let listing = tried
                .into_iter()
                .map(|(label, text)| format!("{label}: {text}"))
                .collect::<Vec<_>>()
                .join("; ");
            TransportErrorKind::custom_str(&format!("all EVM nodes failed: {listing}"))
        },
        // Every node hit the cutoff, or there is no node: a generic error that names no node.
        None => TransportErrorKind::custom_str(&format!(
            "EVM nodes unreachable: no node answered within {} s",
            inner.cutoff_s
        )),
    }
}

/// Runs `fut` for at most `seconds`; `None` means the cutoff elapsed first. The same select-with-timer on the
/// native and the WASM target.
async fn within_cutoff<F: Future>(seconds: f64, fut: F) -> Option<F::Output> {
    let fut = Box::pin(fut);
    match select(fut, Timer::sleep(seconds)).await {
        Either::Left((output, _)) => Some(output),
        Either::Right(_) => None,
    }
}

/// How long past the attempt's bound the spawned HTTP exchange may live. The caller's [`within_cutoff`] must win
/// the race so that a timed-out node is reported as a cutoff; the exchange's own bound only has to end the spawned
/// task soon after, so it is slightly later.
#[cfg(not(target_arch = "wasm32"))]
const SPAWNED_EXCHANGE_GRACE_S: f64 = 1.;

#[cfg(not(target_arch = "wasm32"))]
async fn send_http(uri: &http::Uri, body: &[u8], bound_s: f64) -> Result<Vec<u8>, String> {
    use http::header::HeaderValue;
    use mm2_net::transport::slurp_req_bounded;

    let mut req = http::Request::new(body.to_vec());
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = uri.clone();
    req.headers_mut()
        .insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let (status, _headers, response_bytes) = slurp_req_bounded(req, bound_s + SPAWNED_EXCHANGE_GRACE_S)
        .await
        .map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    Ok(response_bytes)
}

#[cfg(target_arch = "wasm32")]
async fn send_http(uri: &http::Uri, body: &[u8], _bound_s: f64) -> Result<Vec<u8>, String> {
    use mm2_net::wasm_http::FetchRequest;

    let body_string = String::from_utf8(body.to_vec()).map_err(|e| e.to_string())?;
    let (status, response_str) = FetchRequest::post(&uri.to_string())
        .cors()
        .body_utf8(body_string)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .request_str()
        .await
        .map_err(|e| format!("{e:?}"))?;

    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    Ok(response_str.into_bytes())
}

#[cfg(test)] pub(crate) mod test_support;
#[cfg(test)] mod tests;
