/******************************************************************************
 * Copyright © 2014-2018 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 ******************************************************************************/
//
//  rpc.rs
//
//  Copyright © 2014-2018 SuperNET. All rights reserved.
//

use crate::mm2::rpc::rate_limiter::RateLimitError;
#[cfg(feature = "unsafe-rpc-wire-dump")] use common::log::trace;
#[cfg(not(target_arch = "wasm32"))] use common::log::warn;
use common::log::{error, info};
use common::{err_to_rpc_json_string, err_tp_rpc_json, HttpStatusCode};
use derive_more::Display;
use futures::future::join_all;
use http::header::{HeaderValue, ACCESS_CONTROL_ALLOW_ORIGIN};
use http::request::Parts;
use http::{Method, Request, Response, StatusCode};
#[cfg(not(target_arch = "wasm32"))]
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
#[cfg(not(target_arch = "wasm32"))]
use hyper::body::{Bytes, Incoming};
use lazy_static::lazy_static;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_rpc::mm_protocol::{MmRpcBuilder, MmRpcResponse, MmRpcVersion};
use regex::Regex;
use serde::Serialize;
use serde_json::{self as json, Value as Json};
use std::borrow::Cow;
use std::net::SocketAddr;

#[cfg(target_arch = "wasm32")]
#[path = "rpc/lp_commands/connect_metamask.rs"]
pub mod connect_metamask;
#[path = "rpc/dispatcher/dispatcher.rs"] mod dispatcher;
#[path = "rpc/dispatcher/dispatcher_legacy.rs"]
mod dispatcher_legacy;
#[path = "rpc/lp_commands/lp_commands.rs"] pub mod lp_commands;
#[path = "rpc/lp_commands/lp_commands_legacy.rs"]
pub mod lp_commands_legacy;
#[path = "rpc/lp_commands/one_inch/mod.rs"] pub mod one_inch;
#[path = "rpc/rate_limiter.rs"] mod rate_limiter;
#[path = "rpc/lp_commands/send_asked_data.rs"]
pub mod send_asked_data;
#[path = "rpc/lp_commands/shared_db_id.rs"] pub mod shared_db_id;
#[path = "rpc/sse_handler.rs"] mod sse_handler;
#[path = "rpc/streaming_activations/mod.rs"]
pub mod streaming_activations;
#[cfg(not(target_arch = "wasm32"))]
#[path = "rpc/lp_commands/trezor.rs"]
pub mod trezor;
#[path = "rpc/lp_commands/wallet_connect.rs"]
pub mod wallet_connect;

/// Lists the RPC method not requiring the "userpass" authentication.  
/// None is also public to skip auth and display proper error in case of method is missing
const PUBLIC_METHODS: &[Option<&str>] = &[
    // Sorted alphanumerically (on the first letter) for readability.
    Some("fundvalue"),
    Some("getprice"),
    Some("getpeers"),
    Some("getcoins"),
    Some("help"),
    Some("metrics"),
    Some("notify"), // Manually checks the peer's public key.
    Some("orderbook"),
    Some("passphrase"), // Manually checks the "passphrase".
    Some("pricearray"),
    Some("psock"),
    Some("statsdisp"),
    Some("stats_swap_status"),
    Some("tradesarray"),
    Some("ticker"),
    None,
];

#[cfg(feature = "unsafe-rpc-wire-dump")]
const RPC_WIRE_DUMP_REDACTED_VALUE: &str = "__MM2_RPC_WIRE_DUMP_REDACTED_SECRET__";

#[cfg(feature = "unsafe-rpc-wire-dump")]
const RPC_WIRE_DUMP_MAX_CHARS: usize = 65_536;

#[cfg(feature = "unsafe-rpc-wire-dump")]
const RPC_WIRE_DUMP_SECRET_KEY_MARKERS: &[&str] = &[
    "userpass",
    "passphrase",
    "seed",
    "mnemonic",
    "private",
    "priv",
    "xprv",
    "wif",
    "secret",
    "password",
    "api_key",
    "token",
];

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn rpc_wire_dump_enabled() -> bool { matches!(std::env::var("MM2_RPC_WIRE_DUMP").ok().as_deref(), Some("1")) }

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn rpc_wire_dump_secrets_enabled() -> bool {
    matches!(std::env::var("MM2_RPC_WIRE_DUMP_SECRETS").ok().as_deref(), Some("1"))
}

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn rpc_wire_dump_key_is_sensitive(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    RPC_WIRE_DUMP_SECRET_KEY_MARKERS
        .iter()
        .any(|marker| lowered == *marker || lowered.contains(marker))
}

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn redact_rpc_wire_dump_json(value: &mut Json) {
    match value {
        Json::Object(map) => {
            for (key, nested_value) in map.iter_mut() {
                if rpc_wire_dump_key_is_sensitive(key) {
                    *nested_value = Json::String(RPC_WIRE_DUMP_REDACTED_VALUE.to_owned());
                } else {
                    redact_rpc_wire_dump_json(nested_value);
                }
            }
        },
        Json::Array(items) => {
            for item in items {
                redact_rpc_wire_dump_json(item);
            }
        },
        _ => {},
    }
}

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn truncate_rpc_wire_dump(mut s: String) -> String {
    if s.len() > RPC_WIRE_DUMP_MAX_CHARS {
        s.truncate(RPC_WIRE_DUMP_MAX_CHARS);
        s.push_str("...<rpc-wire-dump-truncated>");
    }
    s
}

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn format_rpc_wire_dump_payload(payload: &[u8], include_secrets: bool) -> String {
    if let Ok(mut json_value) = json::from_slice::<Json>(payload) {
        if !include_secrets {
            redact_rpc_wire_dump_json(&mut json_value);
        }

        if let Ok(rendered) = json::to_string(&json_value) {
            return truncate_rpc_wire_dump(rendered);
        }
    }

    truncate_rpc_wire_dump(String::from_utf8_lossy(payload).into_owned())
}

pub type DispatcherResult<T> = Result<T, MmError<DispatcherError>>;

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum DispatcherError {
    #[display(fmt = "Your ip is banned.")]
    Banned,
    #[display(fmt = "No such method")]
    NoSuchMethod,
    #[display(fmt = "Error parsing request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Selected method can be called from localhost only!")]
    LocalHostOnly,
    #[display(fmt = "Userpass is not set!")]
    UserpassIsNotSet,
    #[display(fmt = "Userpass is invalid! - {}", _0)]
    UserpassIsInvalid(RateLimitError),
    #[display(fmt = "Error parsing mmrpc version: {}", _0)]
    InvalidMmRpcVersion(String),
}

impl HttpStatusCode for DispatcherError {
    fn status_code(&self) -> StatusCode {
        match self {
            DispatcherError::NoSuchMethod
            | DispatcherError::InvalidRequest(_)
            | DispatcherError::InvalidMmRpcVersion(_) => StatusCode::BAD_REQUEST,
            DispatcherError::LocalHostOnly
            | DispatcherError::UserpassIsNotSet
            | DispatcherError::UserpassIsInvalid(_)
            | DispatcherError::Banned => StatusCode::FORBIDDEN,
        }
    }
}

impl From<serde_json::Error> for DispatcherError {
    fn from(e: serde_json::Error) -> Self { DispatcherError::InvalidRequest(e.to_string()) }
}

#[allow(unused_macros)]
macro_rules! unwrap_or_err_response {
    ($e:expr, $($args:tt)*) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => return rpc_err_response(500, &ERRL!("{}", err)),
        }
    };
}

async fn process_json_batch_requests(ctx: MmArc, requests: &[Json], client: SocketAddr) -> Result<Json, String> {
    let mut futures = Vec::with_capacity(requests.len());
    for request in requests {
        futures.push(process_single_request(ctx.clone(), request.clone(), client));
    }
    let results = join_all(futures).await;
    let responses: Vec<_> = results
        .into_iter()
        .map(|resp| match resp {
            Ok(r) => match json::from_slice(r.body()) {
                Ok(j) => j,
                Err(e) => {
                    error!("Response {:?} is not a valid JSON, error: {}", r, e);
                    Json::Null
                },
            },
            Err(e) => err_tp_rpc_json(e),
        })
        .collect();
    Ok(Json::Array(responses))
}

#[cfg(target_arch = "wasm32")]
async fn process_json_request(ctx: MmArc, req_json: Json, client: SocketAddr) -> Result<Json, String> {
    if let Some(requests) = req_json.as_array() {
        return process_json_batch_requests(ctx, &requests, client)
            .await
            .map_err(|e| ERRL!("{}", e));
    }

    let r = try_s!(process_single_request(ctx, req_json, client).await);
    json::from_slice(r.body()).map_err(|e| ERRL!("Response {:?} is not a valid JSON, error: {}", r, e))
}

#[cfg(not(target_arch = "wasm32"))]
async fn process_json_request(ctx: MmArc, req_json: Json, client: SocketAddr) -> Result<Response<Vec<u8>>, String> {
    if let Some(requests) = req_json.as_array() {
        let response = try_s!(process_json_batch_requests(ctx, requests, client).await);
        let res = try_s!(json::to_vec(&response));
        return Ok(try_s!(Response::builder().body(res)));
    }

    process_single_request(ctx, req_json, client).await
}

fn response_from_dispatcher_error(
    error: MmError<DispatcherError>,
    version: MmRpcVersion,
    id: Option<usize>,
) -> Response<Vec<u8>> {
    error!("RPC dispatcher error: {}", error);
    let response: MmRpcResponse<(), _> = MmRpcBuilder::err(error).version(version).id(id).build();
    response.serialize_http_response()
}

pub fn escape_answer<'a, S: Into<Cow<'a, str>>>(input: S) -> Cow<'a, str> {
    lazy_static! {
        static ref REGEX: Regex = Regex::new("[<>&]").unwrap();
    }

    let input = input.into();
    let mut last_match = 0;

    if REGEX.is_match(&input) {
        let matches = REGEX.find_iter(&input);
        let mut output = String::with_capacity(input.len());
        for mat in matches {
            let (begin, end) = (mat.start(), mat.end());
            output.push_str(&input[last_match..begin]);
            match &input[begin..end] {
                "<" => output.push_str("&lt;"),
                ">" => output.push_str("&gt;"),
                "&" => output.push_str("&amp;"),
                _ => unreachable!(),
            }
            last_match = end;
        }
        output.push_str(&input[last_match..]);
        Cow::Owned(output)
    } else {
        input
    }
}

async fn process_single_request(ctx: MmArc, req: Json, client: SocketAddr) -> Result<Response<Vec<u8>>, String> {
    let local_only = ctx.conf["rpc_local_only"].as_bool().unwrap_or(true);
    if req["mmrpc"].is_null() {
        return dispatcher_legacy::process_single_request(ctx, req, client, local_only)
            .await
            .map_err(|e| ERRL!("{}", e));
    }

    let id = req["id"].as_u64().map(|id| id as usize);
    let version: MmRpcVersion = match json::from_value(req["mmrpc"].clone()) {
        Ok(v) => v,
        Err(e) => {
            let error = MmError::new(DispatcherError::InvalidMmRpcVersion(e.to_string()));
            // use the latest `MmRpcVersion` if the version is not recognized
            return Ok(response_from_dispatcher_error(error, MmRpcVersion::V2, id));
        },
    };

    match dispatcher::process_single_request(ctx, req, client, local_only).await {
        Ok(response) => Ok(response),
        Err(e) => {
            // return always serialized response
            Ok(response_from_dispatcher_error(e, version, id))
        },
    }
}

/// Response body of the RPC server: buffered for the JSON-RPC answers, streaming for the SSE endpoint.
#[cfg(not(target_arch = "wasm32"))]
type RpcBody = UnsyncBoxBody<Bytes, std::convert::Infallible>;

#[cfg(not(target_arch = "wasm32"))]
fn full_body(data: impl Into<Bytes>) -> RpcBody { Full::new(data.into()).boxed_unsync() }

#[cfg(not(target_arch = "wasm32"))]
async fn rpc_service(req: Request<Incoming>, ctx_h: u32, client: SocketAddr) -> Response<RpcBody> {
    /// Unwraps a result or propagates its error 500 response with the specified headers (if they are present).
    macro_rules! try_sf {
        ($value: expr $(, $header_key:expr => $header_val:expr)*) => {
            match $value {
                Ok(ok) => ok,
                Err(err) => {
                    error!("RPC error response: {}", err);
                    let ebody = err_to_rpc_json_string(&fomat!((err)));
                    // generate a `Response` with the headers specified in `$header_key` and `$header_val`
                    let response = Response::builder().status(500) $(.header($header_key, $header_val))* .body(full_body(ebody)).unwrap();
                    return response;
                },
            }
        };
    }

    async fn process_rpc_request(
        ctx: MmArc,
        req: Parts,
        req_json: Json,
        client: SocketAddr,
    ) -> Result<Response<Vec<u8>>, String> {
        if req.method != Method::POST {
            return ERR!("Only POST requests are supported!");
        }

        process_json_request(ctx, req_json, client).await
    }

    let ctx = try_sf!(MmArc::from_ffi_handle(ctx_h));

    // SSE endpoint: GET /event-stream?id=<client_id>
    // Check the path before consuming the body for JSON parsing.
    if req.uri().path() == sse_handler::SSE_ENDPOINT {
        let (parts, _body) = req.into_parts();
        return sse_handler::handle_sse(parts, ctx_h).await;
    }

    // https://github.com/artemii235/SuperNET/issues/219
    let rpc_cors = match ctx.conf["rpccors"].as_str() {
        Some(s) => try_sf!(HeaderValue::from_str(s)),
        None => HeaderValue::from_static("http://localhost:3000"),
    };

    // Convert the native Hyper stream into a portable stream of `Bytes`.
    let (req, req_body) = req.into_parts();
    let req_bytes = try_sf!(req_body.collect().await.map(|collected| collected.to_bytes()), ACCESS_CONTROL_ALLOW_ORIGIN => rpc_cors);
    let req_str = String::from_utf8_lossy(req_bytes.as_ref());
    let is_invalid_input = req_str.chars().any(|c| c == '<' || c == '>' || c == '&');
    if is_invalid_input {
        return Response::builder()
            .status(500)
            .header("Content-Type", "application/json")
            .body(full_body(err_to_rpc_json_string("Invalid input")))
            .unwrap();
    }
    let req_json: Json = try_sf!(json::from_slice(&req_bytes), ACCESS_CONTROL_ALLOW_ORIGIN => rpc_cors);

    #[cfg(feature = "unsafe-rpc-wire-dump")]
    let wire_dump_enabled = rpc_wire_dump_enabled();
    #[cfg(feature = "unsafe-rpc-wire-dump")]
    let wire_dump_include_secrets = wire_dump_enabled && rpc_wire_dump_secrets_enabled();
    #[cfg(feature = "unsafe-rpc-wire-dump")]
    if wire_dump_enabled {
        let request_dump = format_rpc_wire_dump_payload(req_bytes.as_ref(), wire_dump_include_secrets);
        trace!("[unsafe-rpc-wire-dump] request from {}: {}", client, request_dump);
    }

    let res = try_sf!(process_rpc_request(ctx, req, req_json, client).await, ACCESS_CONTROL_ALLOW_ORIGIN => rpc_cors);
    let (mut parts, body) = res.into_parts();
    parts.headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, rpc_cors);

    #[cfg(feature = "unsafe-rpc-wire-dump")]
    if wire_dump_enabled {
        let response_dump = format_rpc_wire_dump_payload(&body, wire_dump_include_secrets);
        trace!("[unsafe-rpc-wire-dump] response to {}: {}", client, response_dump);
    }

    let body_escaped = match std::str::from_utf8(&body) {
        Ok(body_utf8) => {
            let escaped = escape_answer(body_utf8);
            escaped.as_bytes().to_vec()
        },
        Err(_) => {
            return Response::builder()
                .status(500)
                .header("Content-Type", "application/json")
                .body(full_body(err_to_rpc_json_string("Non UTF-8 output")))
                .unwrap();
        },
    };
    Response::from_parts(parts, full_body(body_escaped))
}

#[cfg(all(test, feature = "unsafe-rpc-wire-dump"))]
mod wire_dump_tests {
    use super::*;

    #[test]
    fn redact_rpc_wire_dump_json_replaces_known_secret_keys() {
        let mut payload = json::json!({
            "userpass": "abc",
            "params": {
                "passphrase": "seed phrase",
                "nested": { "private_key": "deadbeef" },
                "safe": "ok"
            }
        });

        redact_rpc_wire_dump_json(&mut payload);

        assert_eq!(payload["userpass"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(payload["params"]["passphrase"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(payload["params"]["nested"]["private_key"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(payload["params"]["safe"], "ok");
    }

    #[test]
    fn format_rpc_wire_dump_payload_keeps_json_valid_while_redacting() {
        let payload = br#"{"userpass":"abc","params":{"token":"123","amount":"1"}}"#;
        let dumped = format_rpc_wire_dump_payload(payload, false);
        let parsed: Json = json::from_str(&dumped).expect("expected valid JSON");

        assert_eq!(parsed["userpass"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(parsed["params"]["token"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(parsed["params"]["amount"], "1");
    }

    #[test]
    fn format_rpc_wire_dump_payload_keeps_secrets_when_override_enabled() {
        let payload = br#"{"userpass":"abc","params":{"token":"123"}}"#;
        let dumped = format_rpc_wire_dump_payload(payload, true);
        let parsed: Json = json::from_str(&dumped).expect("expected valid JSON");

        assert_eq!(parsed["userpass"], "abc");
        assert_eq!(parsed["params"]["token"], "123");
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_rpc(ctx_h: u32) -> Result<(), String> {
    use common::wio::CORE;
    use futures::future::{select, Either};
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto;
    use hyper_util::server::graceful::GracefulShutdown;
    use std::convert::Infallible;

    // NB: hyper 1 has no `Server`, so the accept loop is ours. That is also what gives us the remote IP
    // address of each connection, which the RPC handlers need.

    let ctx = MmArc::from_ffi_handle(ctx_h)?;

    let rpc_ip_port = ctx.rpc_ip_port()?;
    // By entering the context, we tie `tokio::spawn` to this executor.
    let _runtime_guard = CORE.0.enter();

    // A bind failure (port already in use, permission denied, ...) is a launch-time
    // configuration error, not a process invariant violation: CRD ch.45 R45.7.1
    // requires a clean, structured error and an orderly non-zero exit here, never a
    // panic. This function previously panicked on this path, which aborts rather
    // than unwinds now that the function is a plain Rust fn (its former `extern "C"`
    // qualifier made a panic here undefined behaviour / an abrupt SIGABRT, since it
    // has always had exactly one caller in this crate, not a real FFI boundary).
    let listener = common::wio::bind_listener(rpc_ip_port).map_err(|e| format!("Can't bind on {rpc_ip_port}: {e}"))?;
    // The actual bound address: identical to `rpc_ip_port` unless port 0 was
    // requested (R45.5.1: "bind any free port"), in which case the OS chose one.
    let bound_addr = listener
        .local_addr()
        .map_err(|e| format!("Can't get the bound address: {e}"))?;

    let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
    let mut shutdown_tx = Some(shutdown_tx);
    ctx.on_stop(Box::new(move || {
        if let Some(shutdown_tx) = shutdown_tx.take() {
            info!("on_stop] firing shutdown_tx!");
            if shutdown_tx.send(()).is_err() {
                warn!("on_stop] shutdown_tx already closed");
            }
            Ok(())
        } else {
            ERR!("on_stop callback called twice!")
        }
    }));

    let server = async move {
        // Serves HTTP/1 and HTTP/2 on the same port, as the `hyper` 0.14 server did.
        let mut builder = auto::Builder::new(TokioExecutor::new());
        // https://github.com/hyperium/hyper/issues/1764
        builder.http1().half_close(false);
        let graceful = GracefulShutdown::new();
        let mut shutdown = Box::pin(shutdown_rx);
        loop {
            let (stream, remote_addr) = match select(Box::pin(listener.accept()), shutdown).await {
                Either::Left((Ok(accepted), pending_shutdown)) => {
                    shutdown = pending_shutdown;
                    accepted
                },
                Either::Left((Err(err), pending_shutdown)) => {
                    // Back off so that a persistent failure (e.g. out of descriptors) does not spin.
                    error!("{}", err);
                    shutdown = pending_shutdown;
                    common::executor::Timer::sleep(1.).await;
                    continue;
                },
                Either::Right(_) => break,
            };
            let service = service_fn(move |req: Request<Incoming>| async move {
                Ok::<_, Infallible>(rpc_service(req, ctx_h, remote_addr).await)
            });
            let conn = graceful.watch(
                builder
                    .clone()
                    .serve_connection(TokioIo::new(stream), service)
                    .into_owned(),
            );
            CORE.0.spawn(async move {
                if let Err(err) = conn.await {
                    error!("{}", err);
                }
            });
        }
        drop(listener);
        // Like `with_graceful_shutdown`, wait for the connections in flight (SSE streams included).
        graceful.shutdown().await;
    };

    CORE.0.spawn({
        log_tag!(
            ctx,
            "😉";
            fmt = ">>>>>>>>>> DEX stats {}:{} DEX stats API enabled at unixtime.{}  <<<<<<<<<",
            bound_addr.ip(),
            bound_addr.port(),
            gstuff::now_ms() / 1000
        );
        let _ = ctx.rpc_started.pin(true);
        server
    });
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_rpc(ctx_h: u32) {
    use futures::StreamExt;
    use mm2_rpc::wasm_rpc;
    use std::sync::Mutex;

    let ctx = MmArc::from_ffi_handle(ctx_h).expect("No context");
    if ctx.wasm_rpc.is_some() {
        error!("RPC is initialized already");
        return;
    }

    let client: SocketAddr = "127.0.0.1:1"
        .parse()
        .expect("'127.0.0.1:1' must be valid socket address");

    let (request_tx, mut request_rx) = wasm_rpc::channel();
    let ctx_weak = ctx.weak();
    let fut = async move {
        while let Some((request_json, response_tx)) = request_rx.next().await {
            let ctx = match MmArc::from_weak(&ctx_weak) {
                Some(ctx) => ctx,
                None => break,
            };

            let response = process_json_request(ctx, request_json, client).await;
            if let Err(e) = response_tx.send(response) {
                error!("Response is not processed: {:?}", e);
            }
        }
    };
    common::executor::spawn(fut);

    // even if the [`MmCtx::wasm_rpc`] is initialized already, the spawned future above will be shutdown
    if let Err(e) = ctx.wasm_rpc.pin(request_tx) {
        error!("'MmCtx::wasm_rpc' is initialized already: {}", e);
        return;
    };
    if let Err(e) = ctx.rpc_started.pin(true) {
        error!("'MmCtx::rpc_started' is set already: {}", e);
        return;
    }

    log_tag!(
        ctx,
        "😉";
        fmt = ">>>>>>>>>> DEX stats API enabled at unixtime.{}  <<<<<<<<<",
        common::now_ms() / 1000
    );
}

/// Loopback tests of the native RPC server on `hyper` 1: the accept loop, the buffered JSON-RPC
/// answers and the streaming SSE endpoint, driven over a real TCP socket.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod server_tests {
    use super::*;
    use async_trait::async_trait;
    use common::executor::Timer;
    use common::wio::CORE;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, NoDataIn, StreamerId};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    /// Emits a probe event every 50 ms until it is shut down.
    struct ProbeStreamer;

    #[async_trait]
    impl EventStreamer for ProbeStreamer {
        type DataInType = NoDataIn;

        fn streamer_id(&self) -> StreamerId { StreamerId::Heartbeat }

        async fn handle(
            self,
            broadcaster: Broadcaster,
            ready_tx: oneshot::Sender<Result<(), String>>,
            shutdown_rx: oneshot::Receiver<()>,
            _data_rx: mpsc::UnboundedReceiver<NoDataIn>,
        ) {
            let _ = ready_tx.send(Ok(()));
            let mut shutdown = core::pin::pin!(shutdown_rx);
            loop {
                let sleep = core::pin::pin!(Timer::sleep(0.05));
                match futures::future::select(sleep, &mut shutdown).await {
                    futures::future::Either::Left(_) => {
                        broadcaster.broadcast(Event::new(StreamerId::Heartbeat, json!({ "probe": 1 })))
                    },
                    futures::future::Either::Right(_) => break,
                }
            }
        }
    }

    fn start_server() -> (MmArc, SocketAddr) {
        // Reserve a free port, then hand it to the server (`rpcport: 0` would hide the chosen port).
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let ctx = MmCtxBuilder::default()
            .with_conf(json!({ "rpcip": "127.0.0.1", "rpcport": port.max(1024), "rpccors": "http://example.test" }))
            .into_mm_arc();
        spawn_rpc(ctx.ffi_handle().unwrap()).unwrap();
        (ctx, SocketAddr::from(([127, 0, 0, 1], port.max(1024))))
    }

    fn connect(addr: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream
    }

    /// Reads from `stream` until `needle` shows up in what has been received so far (at least one read).
    fn read_until(stream: &mut TcpStream, received: &mut String, needle: &str) {
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).expect("read timed out or failed");
            assert!(n > 0, "connection closed before {needle:?}; got {received:?}");
            received.push_str(&String::from_utf8_lossy(&buf[..n]));
            if received.contains(needle) {
                return;
            }
        }
    }

    #[test]
    fn rpc_rejects_markup_in_the_body_with_a_json_error() {
        let (_ctx, addr) = start_server();
        let body = "<script>";
        let mut stream = connect(addr);
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 500"), "{response}");
        assert!(response.contains("Invalid input"), "{response}");
    }

    #[test]
    fn rpc_answers_malformed_json_with_500_and_the_configured_cors_origin() {
        let (_ctx, addr) = start_server();
        let body = "{not json";
        let mut stream = connect(addr);
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 500"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("access-control-allow-origin: http://example.test"),
            "{response}"
        );
    }

    /// The SSE answer must stay open and deliver events as they happen, not only once the body ends.
    #[test]
    fn sse_endpoint_streams_events_over_an_open_connection() {
        let (ctx, addr) = start_server();
        let mut stream = connect(addr);
        write!(stream, "GET /event-stream?id=7 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut received = String::new();
        read_until(&mut stream, &mut received, "\r\n\r\n");
        let head = received.to_ascii_lowercase();
        assert!(head.starts_with("http/1.1 200"), "{received}");
        assert!(head.contains("content-type: text/event-stream"), "{received}");
        assert!(head.contains("cache-control: no-cache"), "{received}");

        // The client is registered once the handler ran; subscribe it to the probe streamer.
        CORE.0
            .block_on(ctx.event_stream_manager.add(7, ProbeStreamer))
            .expect("the probe streamer starts");

        // Two distinct frames, received while the connection is still open.
        while received.matches("data: {").count() < 2 {
            read_until(&mut stream, &mut received, "");
        }
        assert!(received.contains(r#""origin":"HEARTBEAT""#), "{received}");
        assert!(received.contains(r#""payload":{"probe":1}"#), "{received}");
        assert!(received.contains(r#""error":false"#), "{received}");
    }
}
