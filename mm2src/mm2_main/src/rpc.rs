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
#[cfg(feature = "unsafe-rpc-wire-dump")]
use coins::node_url::redact_urls_in_text;
use common::log::log_crate::Level;
#[cfg(feature = "unsafe-rpc-wire-dump")] use common::log::trace;
#[cfg(not(target_arch = "wasm32"))] use common::log::warn;
use common::log::{error, info};
use common::{err_to_rpc_json_string, err_tp_rpc_json, HttpStatusCode};
use derive_more::Display;
use dispatcher_legacy::LegacyOutcome;
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
#[path = "rpc/legacy_adapter.rs"] pub(crate) mod legacy_adapter;
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
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "rpc/lp_commands/wallet_rpc_tests.rs"]
mod wallet_rpc_tests;

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
        // A URL inside a string keeps its scheme, host and port; its userinfo, path, query and fragment are
        // marked (CRD ch.35, the rule after R35.1.17, D35.10). Nothing else of the value changes.
        Json::String(text) if text.contains("://") => *text = redact_urls_in_text(text),
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

/// The level an RPC refusal is logged at, by the HTTP status of the answer: a refusal the request caused (4xx) is a
/// warning, anything else is an error. The log text and the answer are not affected.
pub(crate) fn rpc_error_log_level(status: StatusCode) -> Level {
    // 401 and 403 stay errors: a wrong password, a ban or a non-local client is reported like a failure, not like a
    // mistyped request. So does 408: in this tree it is the status of timeouts, which are not the request's fault.
    // The status is taken as the handler declares it: a few handler errors map a node-side failure to 400 (for
    // example `EnableLightningError::RpcError`, `WalletConnectRpcError::InitializationError`) and therefore warn.
    if status.is_client_error()
        && status != StatusCode::UNAUTHORIZED
        && status != StatusCode::FORBIDDEN
        && status != StatusCode::REQUEST_TIMEOUT
    {
        Level::Warn
    } else {
        Level::Error
    }
}

/// Texts of the legacy surface, which has no status class: every entry is a list of fragments that must all occur in
/// the error text (the ERRL macros prefix `file:line] ` and handlers nest messages, so the match is a `contains`).
/// An entry is a refusal the request itself can cause. Authentication, rate-limit, ban and transport texts are not
/// listed on purpose: they stay errors.
#[cfg(not(target_arch = "wasm32"))]
const CLIENT_CAUSED_LEGACY_ERRORS: &[&[&str]] = &[
    // An unknown or an inactive coin.
    &["No such coin"],
    &["coin ", " is not found"],
    &["is not found or inactive"],
    &[" is not activated"],
    &[" are not activated"],
    // Not enough balance for the requested trade.
    &["Not enough ", " for swap"],
    &["IBC routing requires minimum balance"],
    // A volume or a price below its minimum, or a volume that is not valid.
    &["Max base vol "],
    &["Min base vol "],
    &["Max rel vol "],
    &["Base amount ", " is too low"],
    &["Rel amount ", " is too low"],
    &["Min volume ", " is too low"],
    &["Price ", " is too low"],
    &["The volume ", " is too low"],
    &["is below min base vol"],
    &["less than minimum transaction amount"],
    &["New volume ", " should be more than"],
    // A pair or a coin that cannot trade.
    &["Rel coin can not be same as base"],
    &["Base and rel must be different coins"],
    &["Trading is not supported for coin"],
    &[" is wallet only"],
];

/// `true` when the text of a legacy handler error is a refusal the request itself caused, so it is logged as a
/// warning (see [`CLIENT_CAUSED_LEGACY_ERRORS`]).
#[cfg(not(target_arch = "wasm32"))]
fn legacy_error_is_client_caused(text: &str) -> bool {
    CLIENT_CAUSED_LEGACY_ERRORS
        .iter()
        .any(|fragments| fragments.iter().all(|fragment| text.contains(fragment)))
}

fn response_from_dispatcher_error(
    error: MmError<DispatcherError>,
    version: MmRpcVersion,
    id: Option<usize>,
) -> Response<Vec<u8>> {
    common::log::log_crate::log!(
        rpc_error_log_level(error.get_inner().status_code()),
        "RPC dispatcher error: {}",
        error
    );
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
        let outcome = dispatcher_legacy::process_single_request(ctx.clone(), req, client, local_only)
            .await
            .map_err(|e| ERRL!("{}", e))?;
        return match outcome {
            LegacyOutcome::Answered(response) => Ok(response),
            // CRD ch.43 R43.1A.1a: the legacy checks passed and the legacy table has no such method. The request is
            // retried once on the versioned surface; the versioned surface never falls back to the legacy one.
            LegacyOutcome::NoLegacyMethod(req) => {
                Ok(process_legacy_fallback_request(ctx, req, client, local_only).await)
            },
        };
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

    Ok(process_versioned_request(ctx, req, client, local_only, version, id).await)
}

/// Runs a request through the versioned dispatcher and renders a dispatcher failure as the mmrpc error envelope.
async fn process_versioned_request(
    ctx: MmArc,
    req: Json,
    client: SocketAddr,
    local_only: bool,
    version: MmRpcVersion,
    id: Option<usize>,
) -> Response<Vec<u8>> {
    match dispatcher::process_single_request(ctx, req, client, local_only).await {
        Ok(response) => response,
        // return always serialized response
        Err(e) => response_from_dispatcher_error(e, version, id),
    }
}

/// CRD ch.43 R43.1A.1b-R43.1A.1c: the legacy-to-versioned fallback. The legacy body is converted by setting its
/// top-level `mmrpc` key to `"2.0"` and nothing else, then goes through the same versioned path as a request that
/// carried the key from the start: strict decoding, the versioned local-only, ban and authentication checks, and the
/// mmrpc 2.0 envelope for every outcome. `id` is echoed when it is a non-negative integer and is `null` otherwise.
/// This function never goes back to the legacy dispatcher.
async fn process_legacy_fallback_request(
    ctx: MmArc,
    mut req: Json,
    client: SocketAddr,
    local_only: bool,
) -> Response<Vec<u8>> {
    let id = req["id"].as_u64().map(|id| id as usize);
    // A body that is not an object stays as it is and is refused by the strict decoding as `InvalidRequest`.
    if let Json::Object(body) = &mut req {
        body.insert("mmrpc".to_owned(), Json::String("2.0".to_owned()));
    }
    process_versioned_request(ctx, req, client, local_only, MmRpcVersion::V2, id).await
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
                    let text = fomat!((err));
                    if legacy_error_is_client_caused(&text) {
                        warn!("RPC error response: {}", text);
                    } else {
                        error!("RPC error response: {}", text);
                    }
                    let ebody = err_to_rpc_json_string(&text);
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

    /// T35.35 / D35.10: in the redacted mode, a URL keeps its scheme, host and port and shows a marker for each
    /// part it had; nothing else of the dump changes. With the secrets switch on, the dump is the input.
    #[test]
    fn t35_35_redacted_dump_keeps_the_host_of_a_url_and_marks_the_rest() {
        const REMOVED_PARTS: [&str; 6] = ["alice", "pw1", "KEYA", "KEYB", "tok1", "tok2"];
        let rin = br#"{"userpass":"abc","method":"electrum","coin":"RIN","mm2":1,"servers":[
            {"url":"electrum.example.org:50002","protocol":"SSL"},
            {"url":"wss://alice:pw1@electrum.example.org:443/ws/KEYA?token=tok1","protocol":"WSS"},
            {"url":"wss://electrum2.example.org:443/","protocol":"WSS"}]}"#;
        let evm = br#"{"method":"enable_eth_with_tokens","mmrpc":"2.0","params":{"ticker":"ETH","tx_history":true,
            "nodes":[{"url":"https://alice:pw1@rpc.example.org:8545/v3/KEYA?x=tok1"},
                     {"url":"https://rpc2.example.org/v3/KEYB"},
                     {"url":"https://rpc3.example.org:8545/"}]},
            "error":"error sending request for url (https://rpc.example.org:8545/v3/KEYB?token=tok2): dns error"}"#;

        let rin_dump = format_rpc_wire_dump_payload(rin, false);
        let redacted_rin: Json = json::from_str(&rin_dump).unwrap();
        assert_eq!(redacted_rin["coin"], "RIN");
        assert_eq!(redacted_rin["method"], "electrum");
        assert_eq!(redacted_rin["mm2"], 1);
        assert_eq!(redacted_rin["userpass"], RPC_WIRE_DUMP_REDACTED_VALUE);
        assert_eq!(redacted_rin["servers"][0]["url"], "electrum.example.org:50002");
        assert_eq!(redacted_rin["servers"][0]["protocol"], "SSL");
        assert_eq!(
            redacted_rin["servers"][1]["url"],
            "wss://<redacted>@electrum.example.org:443/<redacted>?<redacted>"
        );
        assert_eq!(redacted_rin["servers"][2]["url"], "wss://electrum2.example.org:443/");

        let evm_dump = format_rpc_wire_dump_payload(evm, false);
        let redacted_evm: Json = json::from_str(&evm_dump).unwrap();
        assert_eq!(redacted_evm["method"], "enable_eth_with_tokens");
        assert_eq!(redacted_evm["params"]["ticker"], "ETH");
        assert_eq!(redacted_evm["params"]["tx_history"], true);
        let nodes = &redacted_evm["params"]["nodes"];
        assert_eq!(
            nodes[0]["url"],
            "https://<redacted>@rpc.example.org:8545/<redacted>?<redacted>"
        );
        assert_eq!(nodes[1]["url"], "https://rpc2.example.org/<redacted>");
        assert_eq!(nodes[2]["url"], "https://rpc3.example.org:8545/");
        assert_eq!(
            redacted_evm["error"],
            "error sending request for url (https://rpc.example.org:8545/<redacted>?<redacted>): dns error"
        );

        for dumped in [&rin_dump, &evm_dump] {
            assert!(
                REMOVED_PARTS.iter().all(|part| !dumped.contains(part)),
                "a removed URL part is still in the redacted dump"
            );
        }

        for payload in [&rin[..], &evm[..]] {
            let input: Json = json::from_slice(payload).unwrap();
            let full: Json = json::from_str(&format_rpc_wire_dump_payload(payload, true)).unwrap();
            assert_eq!(full, input);
        }
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

    fn start_server() -> (MmArc, SocketAddr) { start_server_with_coins(json!([])) }

    /// Like [`start_server`], with `coins` as the coins configuration and `pw` as the RPC password.
    fn start_server_with_coins(coins: Json) -> (MmArc, SocketAddr) {
        // Reserve a free port, then hand it to the server (`rpcport: 0` would hide the chosen port).
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let ctx = MmCtxBuilder::default()
            .with_conf(json!({
                "rpcip": "127.0.0.1",
                "rpcport": port.max(1024),
                "rpccors": "http://example.test",
                "rpc_password": "pw",
                "coins": coins,
            }))
            .into_mm_arc();
        spawn_rpc(ctx.ffi_handle().unwrap()).unwrap();
        (ctx, SocketAddr::from(([127, 0, 0, 1], port.max(1024))))
    }

    /// POSTs `request` and returns the status line and the JSON body exactly as a client receives them.
    fn post(addr: SocketAddr, request: &Json) -> (String, Json) {
        let body = request.to_string();
        let mut stream = connect(addr);
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.lines().next().unwrap_or_default().to_owned();
        let (_, payload) = response.split_once("\r\n\r\n").expect("a complete response");
        let payload = serde_json::from_str(payload).unwrap_or_else(|e| panic!("{e}: {response}"));
        (status, payload)
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

    /// Tendermint swaps are postponed for this beta (`coins::TENDERMINT_SWAPS_AVAILABLE`): over the wire, a real
    /// Tendermint platform coin and a real Tendermint token are refused by the routes that start a new trade
    /// (`setprice`, `buy`, `sell` and the v2 `start_swap`), on either side of the pair, with the usual
    /// wallet-only answer.
    #[test]
    fn tendermint_coins_are_refused_by_the_new_trade_routes() {
        use coins::tendermint::{tendermint_coin_from_conf_and_request, TendermintToken};
        use coins::{CoinsContext, MmCoinEnum, TestCoin};

        let coins = json!([
            { "coin": "IRIS", "avg_blocktime": 5, "mm2": 1, "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uiris", "decimals": 6, "account_prefix": "iaa", "chain_id": "irishub-1" } } },
            { "coin": "IRIS-NYAN", "mm2": 1, "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                "platform": "IRIS", "decimals": 6, "denom": "unyan" } } },
            { "coin": "TRADE", "mm2": 1 },
        ]);
        let (ctx, addr) = start_server_with_coins(coins.clone());
        crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "tendermint trade refusal test passphrase")
            .expect("initialize the crypto context");
        let priv_key = *ctx.secp256k1_key_pair().private().secret;
        let node = serde_json::from_value(json!({ "url": "http://127.0.0.1:1" })).unwrap();
        let platform = common::block_on(tendermint_coin_from_conf_and_request(
            &ctx,
            "IRIS".to_owned(),
            &coins[0],
            vec![node],
            &priv_key,
            false,
        ))
        .unwrap_or_else(|e| panic!("cannot build the platform: {e}"));
        let token = TendermintToken::from_protocol("IRIS-NYAN".to_owned(), platform.clone(), 6, "unyan").unwrap();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for coin in [
            MmCoinEnum::from(platform),
            MmCoinEnum::from(token),
            MmCoinEnum::Test(TestCoin::new("TRADE")),
        ] {
            common::block_on(coins_ctx.add_coin(coin)).unwrap();
        }

        for tendermint in ["IRIS", "IRIS-NYAN"] {
            for (base, rel, side) in [(tendermint, "TRADE", "Base"), ("TRADE", tendermint, "Rel")] {
                let wallet_only_error = format!("{side} coin {tendermint} is wallet only");

                // The legacy routes answer `{"error": ...}` with HTTP 500.
                for method in ["setprice", "buy", "sell"] {
                    let request = json!({
                        "userpass": "pw", "method": method, "base": base, "rel": rel, "price": 1, "volume": 1
                    });
                    let (status, body) = post(addr, &request);
                    assert!(status.contains("500"), "{method} {base}/{rel}: {status}");
                    let error = body["error"].as_str().unwrap_or_else(|| panic!("{method}: {body}"));
                    assert!(error.ends_with(&wallet_only_error), "{method} {base}/{rel}: {body}");
                }

                // `start_swap` is the v2 front of the same three.
                for method in ["set_price", "buy", "sell"] {
                    let request = json!({
                        "mmrpc": "2.0", "userpass": "pw", "method": "start_swap", "id": 3,
                        "params": {
                            "base": base, "rel": rel, "base_coin_amount": "1", "rel_coin_amount": "1",
                            "method": { method: {} },
                        },
                    });
                    let (status, body) = post(addr, &request);
                    assert!(
                        status.contains("500"),
                        "start_swap {method} {base}/{rel}: {status} {body}"
                    );
                    assert_eq!(body["error_type"], "Internal", "{body}");
                    assert!(
                        body["error_data"]
                            .as_str()
                            .unwrap_or_default()
                            .ends_with(&wallet_only_error),
                        "start_swap {method} {base}/{rel}: {body}"
                    );
                }
            }
        }
    }

    /// Methods that only the legacy method table knows (CRD ch.43 R43.1A.4): `get_trade_fee`, which stays legacy-only
    /// by decision (D8 of `docs/plans/mmrpc-v2-completion.md`), and `import_swaps`, a method of a later group of that
    /// plan. The order methods are not among them: since 0.5.0 `setprice`, `update_maker_order`, `cancel_order`,
    /// `cancel_all_orders`, `my_orders` and `order_status` (phase 1) and `buy`, `sell`, `orderbook_depth` and
    /// `orders_history_by_filter` (phase 2) also have a versioned form, a Reloaded extension that the reference does
    /// not have (tested in `versioned_order_methods_*` and `versioned_trading_methods_*` below), and so do the ten
    /// wallet and coin methods of phase 3 (tested in `wallet_rpc_tests`). A method that gets a versioned form
    /// leaves this list.
    const LEGACY_ONLY_METHODS: [&str; 2] = ["get_trade_fee", "import_swaps"];

    fn assert_http_status(status_line: &str, code: u16) {
        assert!(
            status_line.starts_with(&format!("HTTP/1.1 {code}")),
            "expected HTTP {code}, got {status_line}"
        );
    }

    /// Sends a legacy request (no `mmrpc` key, `params` inline next to `method`, the server password) and returns
    /// the status line and the JSON body, exactly as [`post`] does.
    fn post_legacy(addr: SocketAddr, method: &str, params: Json) -> (String, Json) {
        let mut request = json!({ "userpass": "pw", "method": method });
        for (key, value) in params.as_object().expect("the inline parameters are an object") {
            request[key.as_str()] = value.clone();
        }
        post(addr, &request)
    }

    /// CRD ch.43 R43.1A.1, R43.1A.3, R43.1A.4: a versioned request for a method that only the legacy table knows is
    /// answered like any unknown versioned method: the mmrpc error envelope, `NoSuchMethod`, no `error_data`,
    /// HTTP 400, `mmrpc` and `id` echoed (`id` is `null` when the request has none). There is no versioned alias.
    #[test]
    fn versioned_requests_for_legacy_only_methods_answer_no_such_method() {
        let (_ctx, addr) = start_server();
        for method in LEGACY_ONLY_METHODS {
            for id in [Some(7), None] {
                let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "params": {} });
                if let Some(id) = id {
                    request["id"] = json!(id);
                }
                let (status, body) = post(addr, &request);
                assert_http_status(&status, 400);
                assert_eq!(body["mmrpc"], "2.0", "{method}: {body}");
                assert_eq!(body["error_type"], "NoSuchMethod", "{method}: {body}");
                assert!(body.get("error_data").is_none(), "{method}: {body}");
                assert_eq!(
                    body.get("id"),
                    Some(&id.map_or(Json::Null, |id| json!(id))),
                    "{method}: {body}"
                );
            }
        }
    }

    /// CRD ch.43 R43.1A.3: the authentication runs before the method lookup, so an unauthenticated versioned call of
    /// a legacy-only (hence unknown) protected method answers 403, not 400.
    #[test]
    fn versioned_request_for_a_legacy_only_method_is_authenticated_before_the_lookup() {
        let (_ctx, addr) = start_server();
        let request = json!({ "mmrpc": "2.0", "method": "import_swaps", "params": {}, "id": 5 });
        let (status, body) = post(addr, &request);
        assert_http_status(&status, 403);
        assert_eq!(body["mmrpc"], "2.0", "{body}");
        assert_eq!(body["error_type"], "UserpassIsNotSet", "{body}");
        assert!(body.get("error_data").is_none(), "{body}");
        assert_eq!(body["id"], 5, "{body}");
    }

    /// CRD ch.43 R43.1A.2: a legacy-shaped body (parameters inline) with an added `"mmrpc": "2.0"` is not routed to
    /// the legacy handler: it is undecodable under the versioned envelope and answers `InvalidRequest` (string
    /// payload, HTTP 400) before any method lookup. One order method and one other method.
    #[test]
    fn legacy_shaped_body_with_an_mmrpc_key_answers_invalid_request() {
        let (_ctx, addr) = start_server();
        let uuid = "2f9afe84-7a89-4194-8947-45fba563118f";
        let cases = [
            (
                "setprice",
                json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 }),
            ),
            ("update_maker_order", json!({ "uuid": uuid })),
            ("cancel_order", json!({ "uuid": uuid })),
            ("cancel_all_orders", json!({ "cancel_by": { "type": "All" } })),
            ("order_status", json!({ "uuid": uuid })),
            ("my_orders", json!({ "unknown": 1 })),
            ("buy", json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 })),
            ("sell", json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 })),
            ("orderbook_depth", json!({ "pairs": [["RIN", "KMD"]] })),
            ("orders_history_by_filter", json!({ "include_details": true })),
            ("my_balance", json!({ "coin": "RIN" })),
        ];
        for (method, inline) in cases {
            let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": 3 });
            for (key, value) in inline.as_object().unwrap() {
                request[key.as_str()] = value.clone();
            }
            let (status, body) = post(addr, &request);
            assert_http_status(&status, 400);
            assert_eq!(body["mmrpc"], "2.0", "{method}: {body}");
            assert_eq!(body["error_type"], "InvalidRequest", "{method}: {body}");
            assert!(body["error_data"].is_string(), "{method}: {body}");
            assert_eq!(body["id"], 3, "{method}: {body}");
        }
    }

    /// The answer must be the mmrpc 2.0 error envelope with this `error_type`, HTTP status and `id`.
    fn assert_mmrpc_error(answer: &(String, Json), code: u16, error_type: &str, id: Json) {
        let (status, body) = answer;
        assert_http_status(status, code);
        // No message below carries the body: it is an error payload.
        assert_eq!(body["mmrpc"], "2.0", "{error_type}: mmrpc version");
        assert_eq!(body["error_type"], error_type, "{error_type}: error_type");
        assert_eq!(body.get("id"), Some(&id), "{error_type}: id");
    }

    /// The answer must be the legacy error body: HTTP 500, exactly one member `error`, no mmrpc envelope.
    fn assert_legacy_error_body(answer: &(String, Json)) {
        let (status, body) = answer;
        assert_http_status(status, 500);
        assert_eq!(
            body.as_object().map(|object| object.len()),
            Some(1),
            "the legacy error body has one member"
        );
        assert!(body["error"].is_string(), "the legacy error member is a string");
    }

    /// CRD ch.43 R43.1A.1a-R43.1A.1c: a legacy request (no `mmrpc`) shaped like a versioned one minus the key, for a
    /// method only the versioned table knows, is answered in the mmrpc 2.0 envelope with the result, HTTP 200 and
    /// the `id` echoed (`null` when absent).
    #[test]
    fn legacy_body_shaped_as_a_versioned_request_is_answered_through_the_fallback() {
        let (ctx, addr) = start_server();
        crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy fallback test passphrase")
            .expect("initialize the crypto context");
        for (request, id) in [
            (
                json!({ "userpass": "pw", "method": "get_public_key", "id": 11 }),
                json!(11),
            ),
            (
                json!({ "userpass": "pw", "method": "get_public_key", "params": {} }),
                Json::Null,
            ),
        ] {
            let (status, body) = post(addr, &request);
            assert_http_status(&status, 200);
            assert_eq!(body["mmrpc"], "2.0", "{body}");
            assert!(body["result"]["public_key"].is_string(), "{body}");
            assert_eq!(body.get("id"), Some(&id), "{body}");
        }
    }

    /// CRD ch.43 R43.1A.1b: an inline member next to `method` makes the converted request undecodable: the answer is
    /// `InvalidRequest` (string payload, HTTP 400) in the mmrpc 2.0 envelope, before any method lookup.
    #[test]
    fn legacy_body_with_inline_parameters_for_a_versioned_only_method_answers_invalid_request() {
        let (_ctx, addr) = start_server();
        let answer = post_legacy(addr, "get_public_key", json!({ "coin": "RIN", "id": 12 }));
        assert_mmrpc_error(&answer, 400, "InvalidRequest", json!(12));
        assert!(answer.1["error_data"].is_string(), "{}", answer.1);
    }

    /// CRD ch.43 R43.1A.1c: a legacy request for a name neither table knows, with the right password, answers
    /// `NoSuchMethod` (no `error_data`, HTTP 400) in the mmrpc 2.0 envelope with the `id` echoed or `null`.
    #[test]
    fn legacy_request_for_a_name_no_table_knows_answers_no_such_method_in_the_2_0_envelope() {
        let (_ctx, addr) = start_server();
        for id in [Some(9), None] {
            let mut request = json!({ "userpass": "pw", "method": "no_such_method_in_any_table" });
            if let Some(id) = id {
                request["id"] = json!(id);
            }
            let answer = post(addr, &request);
            assert_mmrpc_error(&answer, 400, "NoSuchMethod", id.map_or(Json::Null, |id| json!(id)));
            assert!(answer.1.get("error_data").is_none(), "{}", answer.1);
        }
    }

    /// CRD ch.43 (the endpoint never panics the daemon in response to a request), maintainer decision of 2026-11-10:
    /// without the `sim-panic-rpc` feature (every build that ships) the legacy `sim_panic` method does not exist. An
    /// authenticated legacy request for it is answered like any unknown name: `NoSuchMethod` in the mmrpc 2.0
    /// envelope (no `error_data`), HTTP 400. A versioned request for it answers the same.
    #[cfg(not(feature = "sim-panic-rpc"))]
    #[test]
    fn sim_panic_is_not_routed_without_the_sim_panic_rpc_feature() {
        let (_ctx, addr) = start_server();
        let legacy = post_legacy(addr, "sim_panic", json!({}));
        assert_mmrpc_error(&legacy, 400, "NoSuchMethod", Json::Null);
        assert!(legacy.1.get("error_data").is_none(), "{}", legacy.1);

        let versioned = post(
            addr,
            &json!({ "mmrpc": "2.0", "userpass": "pw", "method": "sim_panic", "params": {}, "id": 2 }),
        );
        assert_mmrpc_error(&versioned, 400, "NoSuchMethod", json!(2));
        assert!(versioned.1.get("error_data").is_none(), "{}", versioned.1);
    }

    /// CRD ch.43, maintainer decision of 2026-11-10: with the test-only `sim-panic-rpc` feature the legacy `sim_panic`
    /// method is routed. A request without `mode` lists the supported modes. Never send `"mode": "simple"` here: it
    /// panics the process.
    #[cfg(feature = "sim-panic-rpc")]
    #[test]
    fn sim_panic_without_a_mode_lists_the_modes_with_the_sim_panic_rpc_feature() {
        let (_ctx, addr) = start_server();
        let (status, body) = post_legacy(addr, "sim_panic", json!({}));
        assert_http_status(&status, 200);
        assert_eq!(body, json!({ "modes": ["simple"] }));
    }

    /// CRD ch.43 R43.1A.1d: an unknown name is protected at the legacy layer, so without or with a wrong `userpass`
    /// it is refused with the legacy error body (HTTP 500) before the fallback.
    #[test]
    fn legacy_request_for_an_unknown_name_without_a_valid_password_keeps_the_legacy_body() {
        let (_ctx, addr) = start_server();
        let without = post(addr, &json!({ "method": "no_such_method_in_any_table", "id": 1 }));
        assert_legacy_error_body(&without);
        let wrong = post(
            addr,
            &json!({ "userpass": "wrong", "method": "no_such_method_in_any_table", "id": 1 }),
        );
        assert_legacy_error_body(&wrong);
        // The same applies to a name only the versioned table knows.
        let versioned_only = post(addr, &json!({ "method": "get_public_key", "id": 1 }));
        assert_legacy_error_body(&versioned_only);
    }

    /// CRD ch.43 R43.1A.1a, R43.1A.1d: a missing or non-string `method` is public at the legacy layer, reaches the
    /// fallback without a password and answers `InvalidRequest` (HTTP 400) in the mmrpc 2.0 envelope. A name of the
    /// public set that neither table serves answers `NoSuchMethod` (HTTP 400) without a password.
    #[test]
    fn legacy_request_without_a_usable_method_or_with_a_public_unserved_name_needs_no_password() {
        let (_ctx, addr) = start_server();
        let missing = post(addr, &json!({ "id": 4 }));
        assert_mmrpc_error(&missing, 400, "InvalidRequest", json!(4));
        assert!(missing.1["error_data"].is_string(), "{}", missing.1);
        let not_a_string = post(addr, &json!({ "method": 5, "id": 4 }));
        assert_mmrpc_error(&not_a_string, 400, "InvalidRequest", json!(4));
        let public_unserved = post(addr, &json!({ "method": "getprice", "id": 4 }));
        assert_mmrpc_error(&public_unserved, 400, "NoSuchMethod", json!(4));
        assert!(public_unserved.1.get("error_data").is_none(), "{}", public_unserved.1);
    }

    /// CRD ch.43 R43.1A.1c: an `id` that is not a non-negative integer is reported as `null`, and the converted
    /// request is undecodable (`InvalidRequest`, HTTP 400).
    #[test]
    fn legacy_request_with_a_string_id_answers_invalid_request_with_a_null_id() {
        let (_ctx, addr) = start_server();
        let answer = post(
            addr,
            &json!({ "userpass": "pw", "method": "no_such_method_in_any_table", "id": "abc" }),
        );
        assert_mmrpc_error(&answer, 400, "InvalidRequest", Json::Null);
        assert!(answer.1["error_data"].is_string(), "{}", answer.1);
    }

    /// CRD ch.43 R43.1A.1e: the legacy layer authenticates `userpass` only. A protected unknown name sent with
    /// `rpc_pass` is refused with the legacy error body before the fallback; the alias is honored only where no
    /// password is needed (a public name no table serves), where the converted request decodes and answers
    /// `NoSuchMethod`.
    #[test]
    fn legacy_request_with_rpc_pass_is_authenticated_by_userpass_only() {
        let (_ctx, addr) = start_server();
        let protected = post(
            addr,
            &json!({ "rpc_pass": "pw", "method": "no_such_method_in_any_table", "id": 6 }),
        );
        assert_legacy_error_body(&protected);
        let public = post(addr, &json!({ "rpc_pass": "pw", "method": "getprice", "id": 6 }));
        assert_mmrpc_error(&public, 400, "NoSuchMethod", json!(6));
    }

    /// CRD ch.43 R43.1A.1a: a legacy method that the legacy table knows keeps its legacy answer: a success is the
    /// legacy body, and a failing handler is the legacy error body (HTTP 500); neither is re-dispatched.
    #[test]
    fn known_legacy_methods_keep_their_legacy_answers() {
        let (_ctx, addr) = start_server();
        let (status, body) = post_legacy(addr, "version", json!({}));
        assert_http_status(&status, 200);
        assert!(body["result"].is_string(), "{body}");
        assert!(body.get("mmrpc").is_none(), "{body}");

        let failure = post_legacy(addr, "my_balance", json!({ "coin": "NO_SUCH_COIN" }));
        assert_legacy_error_body(&failure);
        let error = failure.1["error"].as_str().unwrap();
        assert!(error.contains("No such coin: NO_SUCH_COIN"), "{}", failure.1);
    }

    /// The error text of a legacy handler answer without the `file:line] ` tokens in front of it. `ERR!` and `try_s!` put
    /// one there at the handler and at every layer above it (the dispatcher, the server), and the line numbers change
    /// with every edit of the source; one of the tokens must name the order-matching handler file.
    fn legacy_error_text_without_location(answer: &(String, Json)) -> String {
        legacy_error_text_without_location_of(answer, "ordermatch_trading")
    }

    /// Like [`legacy_error_text_without_location`] for a handler in the file `handler_file` (without its extension).
    fn legacy_error_text_without_location_of(answer: &(String, Json), handler_file: &str) -> String {
        assert_legacy_error_body(answer);
        let error = answer.1["error"].as_str().unwrap();
        let leading = Regex::new(r"^(?:[A-Za-z0-9_]+:\d+\] )+").unwrap();
        let prefix = leading.find(error).map(|found| found.as_str()).unwrap_or_default();
        assert!(
            prefix.contains(&format!("{handler_file}:")),
            "the text keeps the prefix of its handler file"
        );
        error[prefix.len()..].to_owned()
    }

    /// The six order-management methods that moved onto typed cores (plan `mmrpc-v2-completion`, phase 1) answer on the
    /// legacy surface exactly as the handlers of dev did, on a node with no orders: each body is the one the dev
    /// handler built (`my_orders`: `MyOrdersResponse`; `cancel_all_orders_rpc`: `json!({"result": ..})`;
    /// `cancel_order_rpc`: HTTP 404 and `json!({"error": ..})`), and each refusal is the text of its `ERR!`/`try_s!`
    /// site, as the legacy error body (HTTP 500, one member `error`).
    #[test]
    fn legacy_order_methods_keep_their_answers_on_a_node_without_orders() {
        let (_ctx, addr) = start_server();
        let uuid = "2f9afe84-7a89-4194-8947-45fba563118f";

        let (status, body) = post_legacy(addr, "my_orders", json!({}));
        assert_http_status(&status, 200);
        assert_eq!(body.to_string(), r#"{"result":{"maker_orders":{},"taker_orders":{}}}"#);

        let (status, body) = post_legacy(addr, "cancel_all_orders", json!({ "cancel_by": { "type": "All" } }));
        assert_http_status(&status, 200);
        assert_eq!(
            body.to_string(),
            r#"{"result":{"cancelled":[],"currently_matching":[]}}"#
        );

        let (status, body) = post_legacy(addr, "cancel_order", json!({ "uuid": uuid }));
        assert_http_status(&status, 404);
        assert_eq!(
            body.to_string(),
            format!(r#"{{"error":"Order with uuid {uuid} is not found"}}"#)
        );

        let answer = post_legacy(addr, "order_status", json!({ "uuid": uuid }));
        assert_eq!(
            legacy_error_text_without_location(&answer),
            format!("Order with uuid {uuid} is not found")
        );

        let answer = post_legacy(addr, "update_maker_order", json!({ "uuid": uuid }));
        assert_eq!(
            legacy_error_text_without_location(&answer),
            format!("There is no order with UUID {uuid}")
        );

        let answer = post_legacy(
            addr,
            "setprice",
            json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 }),
        );
        assert_eq!(
            legacy_error_text_without_location(&answer),
            "Base coin RIN is not found"
        );

        // A body that does not decode keeps the text of the decoder, one prefix in front.
        let answer = post_legacy(addr, "setprice", json!({ "base": "RIN" }));
        assert_eq!(legacy_error_text_without_location(&answer), "missing field `rel`");
    }

    // ----- The group-A order methods on the versioned surface (plan `mmrpc-v2-completion`, phase 1) -----
    //
    // A Reloaded extension since 0.5.0: the reference has no versioned form of these six methods. `params` carries the
    // legacy members unchanged, unknown members of `params` are ignored (the envelope stays strict), `result` is the
    // content of the legacy `result` (the bare object for `order_status`) and the errors are the typed enums of the
    // cores.

    /// POSTs a versioned request over HTTP. `params` is left out when `None`.
    fn post_v2(addr: SocketAddr, method: &str, params: Option<Json>, id: usize) -> (String, Json) {
        let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": id });
        if let Some(params) = params {
            request["params"] = params;
        }
        post(addr, &request)
    }

    /// The status code of an HTTP status line.
    fn code_of(status_line: &str) -> u16 { status_line.split_whitespace().nth(1).unwrap().parse().unwrap() }

    /// Runs `request` through the whole server path (the legacy table, the fallback, the versioned dispatcher) on the
    /// test thread, so that the coin mocks of the test apply, and returns the HTTP status and the JSON body.
    fn call(ctx: &MmArc, request: Json) -> (u16, Json) {
        let client = SocketAddr::from(([127, 0, 0, 1], 4242));
        let response = common::block_on(process_single_request(ctx.clone(), request, client)).expect("an answer");
        let body = json::from_slice(response.body()).unwrap_or_else(|_| panic!("the answer is JSON"));
        (response.status().as_u16(), body)
    }

    fn call_v2(ctx: &MmArc, method: &str, params: Json, id: usize) -> (u16, Json) {
        call(
            ctx,
            json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "params": params, "id": id }),
        )
    }

    fn call_legacy(ctx: &MmArc, method: &str, params: Json) -> (u16, Json) {
        let mut request = json!({ "userpass": "pw", "method": method });
        for (key, value) in params.as_object().expect("the inline parameters are an object") {
            request[key.as_str()] = value.clone();
        }
        call(ctx, request)
    }

    /// A versioned success: HTTP 200, the 2.0 envelope with the `id` echoed and no error member. Returns `result`.
    fn versioned_result(answer: (u16, Json), id: usize) -> Json {
        let (status, body) = answer;
        // No message below carries the body: a refused request would put an error payload into it.
        assert_eq!(status, 200, "versioned success: status");
        assert_eq!(body["mmrpc"], "2.0", "versioned success: mmrpc version");
        assert_eq!(body["id"], id, "versioned success: id");
        assert!(
            body.get("error").is_none() && body.get("error_type").is_none(),
            "versioned success: no error member"
        );
        body["result"].clone()
    }

    /// `actual` and `expected` are objects with the same members and the same value for each, member by member.
    fn assert_same_members(actual: &Json, expected: &Json, what: &str) {
        let object = |value: &Json| {
            value
                .as_object()
                .unwrap_or_else(|| panic!("{what}: not an object"))
                .clone()
        };
        let (actual, expected) = (object(actual), object(expected));
        let keys = |object: &serde_json::Map<String, Json>| {
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&actual), keys(&expected), "{what}: the members differ");
        for (key, value) in &expected {
            assert_eq!(&actual[key], value, "{what}: member {key}");
        }
    }

    /// The typed refusal: HTTP status, `error_type` token, the JSON type of `error_data` (`None`: the key is absent) and
    /// the echoed `id`, in the mmrpc 2.0 error envelope.
    fn assert_typed_refusal(answer: &(String, Json), status: u16, token: &str, data: Option<&str>, id: usize) {
        assert_mmrpc_error(answer, status, token, json!(id));
        let body = &answer.1;
        match data {
            Some("object") => assert!(body["error_data"].is_object(), "{token}: error_data kind"),
            Some("string") => assert!(body["error_data"].is_string(), "{token}: error_data kind"),
            Some(other) => panic!("unknown kind of error_data: {other}"),
            None => assert!(body.get("error_data").is_none(), "{token}: error_data kind"),
        }
        assert!(
            body["error"].is_string() && body.get("result").is_none(),
            "{token}: error envelope"
        );
    }

    /// Each of the six methods answers a versioned success in the 2.0 envelope with `id` echoed, and `result` has the
    /// members of the content that the legacy body carries under `result` (for `order_status`: of the legacy body).
    /// Every request carries an unknown member inside `params`, which is ignored. The maker order is compared with the
    /// object the dev handler rendered, `MakerOrderForRpc` of the order in the container.
    #[test]
    fn versioned_order_methods_answer_the_legacy_result_in_the_2_0_envelope() {
        use crate::mm2::lp_ordermatch::order_rpc_tests::Node;
        use crate::mm2::lp_ordermatch::MakerOrderForRpc;

        let node = Node::new();
        let ctx = &node.ctx;
        let rendered =
            |order: &crate::mm2::lp_ordermatch::MakerOrder| json::to_value(MakerOrderForRpc::from(order)).unwrap();

        // `setprice`: the maker order object (the dev `set_price` answered `{"result": MakerOrderForRpc}`).
        let result = versioned_result(
            call_v2(
                ctx,
                "setprice",
                json!({ "base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "min_volume": "0.1",
                        "save_in_history": false, "unknown_member": true }),
                11,
            ),
            11,
        );
        let uuid: uuid::Uuid = result["uuid"].as_str().unwrap().parse().unwrap();
        assert_same_members(&result, &rendered(&node.order(uuid)), "setprice");
        // Out of the way of the `cancel_all_orders` check below.
        assert_eq!(call_legacy(ctx, "cancel_order", json!({ "uuid": uuid })).0, 200);

        // `update_maker_order`: the updated maker order object.
        let order = node.add_order(true, true);
        let result = versioned_result(
            call_v2(
                ctx,
                "update_maker_order",
                json!({ "uuid": order.uuid, "new_price": "0.6", "unknown_member": 1 }),
                12,
            ),
            12,
        );
        assert_same_members(&result, &rendered(&node.order(order.uuid)), "update_maker_order");
        assert_eq!(result["price"], "0.6");

        // `order_status`: the legacy object unchanged under `result`, for an active order and for a history order.
        let result = versioned_result(
            call_v2(
                ctx,
                "order_status",
                json!({ "uuid": order.uuid, "unknown_member": 1 }),
                13,
            ),
            13,
        );
        let legacy = call_legacy(ctx, "order_status", json!({ "uuid": order.uuid }));
        assert_eq!(legacy.0, 200);
        assert_same_members(&result, &legacy.1, "order_status (active)");
        assert_eq!(result["type"], "Maker");

        // `cancel_order`: the string "success" (the legacy body is `{"result": "success"}`).
        let result = versioned_result(
            call_v2(
                ctx,
                "cancel_order",
                json!({ "uuid": order.uuid, "unknown_member": 1 }),
                14,
            ),
            14,
        );
        assert_eq!(result, json!("success"));
        let other = node.add_order(true, true);
        let legacy = call_legacy(ctx, "cancel_order", json!({ "uuid": other.uuid }));
        assert_eq!(legacy.1["result"], result);

        let result = versioned_result(call_v2(ctx, "order_status", json!({ "uuid": order.uuid }), 15), 15);
        let legacy = call_legacy(ctx, "order_status", json!({ "uuid": order.uuid }));
        assert_same_members(&result, &legacy.1, "order_status (history)");
        assert_eq!(result["cancellation_reason"], "Cancelled");

        // `cancel_all_orders`: `{cancelled, currently_matching}`, as the legacy `result` carries it.
        let first = node.add_order(true, true);
        let result = versioned_result(
            call_v2(
                ctx,
                "cancel_all_orders",
                json!({ "cancel_by": { "type": "All" }, "unknown_member": 1 }),
                16,
            ),
            16,
        );
        assert_same_members(
            &result,
            &json!({ "cancelled": [first.uuid], "currently_matching": [] }),
            "cancel_all_orders",
        );
        let second = node.add_order(true, true);
        let legacy = call_legacy(ctx, "cancel_all_orders", json!({ "cancel_by": { "type": "All" } }));
        assert_same_members(
            &legacy.1["result"],
            &json!({ "cancelled": [second.uuid], "currently_matching": [] }),
            "cancel_all_orders (legacy)",
        );

        // `my_orders`: `{maker_orders, taker_orders}`; `params` absent, `null`, `{}` and an object with members.
        let order = node.add_order(false, false);
        let legacy = call_legacy(ctx, "my_orders", json!({}));
        for (id, params) in [
            (17, None),
            (18, Some(Json::Null)),
            (19, Some(json!({}))),
            (20, Some(json!({ "unknown_member": 1 }))),
        ] {
            let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": "my_orders", "id": id });
            if let Some(params) = params {
                request["params"] = params;
            }
            let result = versioned_result(call(ctx, request), id);
            assert_same_members(&result, &legacy.1["result"], "my_orders");
            assert!(result["maker_orders"][order.uuid.to_string()].is_object(), "{result}");
        }
    }

    /// The typed refusals a node without coins and orders reaches through the dispatcher: the token, the `error_data`
    /// kind, the HTTP status and the `id` of each. An unknown member inside `params` is ignored here too: the request
    /// is decoded and refused for its content. `my_orders` succeeds on an empty node with every spelling of "no
    /// members". Variants that need a live coin are covered by the unit tests of the cores.
    #[test]
    fn versioned_order_methods_answer_their_typed_refusals_on_a_node_without_coins() {
        let (_ctx, addr) = start_server();
        let uuid = "2f9afe84-7a89-4194-8947-45fba563118f";

        for (id, method) in [(21, "cancel_order"), (22, "order_status"), (23, "update_maker_order")] {
            let answer = post_v2(addr, method, Some(json!({ "uuid": uuid, "unknown_member": 1 })), id);
            assert_typed_refusal(&answer, 400, "UUIDNotFound", Some("object"), id);
            assert_eq!(answer.1["error_data"]["uuid"], uuid, "{method}: error_data uuid");
        }

        let answer = post_v2(
            addr,
            "setprice",
            Some(json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1, "unknown_member": 1 })),
            24,
        );
        assert_typed_refusal(&answer, 400, "NoSuchCoin", Some("object"), 24);
        assert_eq!(answer.1["error_data"]["coin"], "RIN", "setprice: error_data coin");

        // Params that do not decode answer `InvalidRequest` (string payload, HTTP 400), as for every versioned method.
        let answer = post_v2(addr, "setprice", Some(json!({ "base": "RIN" })), 25);
        assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), 25);
        let answer = post_v2(addr, "cancel_all_orders", Some(json!({})), 26);
        assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), 26);
        let answer = post_v2(addr, "cancel_order", None, 27);
        assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), 27);

        // `my_orders` and `cancel_all_orders` succeed on a node with no orders.
        for (id, params) in [
            (28, None),
            (29, Some(Json::Null)),
            (30, Some(json!({}))),
            (31, Some(json!({ "unknown_member": 1 }))),
        ] {
            let (status, body) = post_v2(addr, "my_orders", params, id);
            let result = versioned_result((code_of(&status), body), id);
            assert_eq!(result, json!({ "maker_orders": {}, "taker_orders": {} }));
        }
        let (status, body) = post_v2(
            addr,
            "cancel_all_orders",
            Some(json!({ "cancel_by": { "type": "All" }, "unknown_member": 1 })),
            32,
        );
        let result = versioned_result((code_of(&status), body), 32);
        assert_eq!(result, json!({ "cancelled": [], "currently_matching": [] }));
    }

    /// The refusal of the rel coin needs a registered base coin, so it runs on a node with coins.
    #[test]
    fn versioned_setprice_refuses_an_unknown_rel_coin() {
        use crate::mm2::lp_ordermatch::order_rpc_tests::Node;

        let node = Node::new();
        let (status, body) = call_v2(
            &node.ctx,
            "setprice",
            json!({ "base": "DYN", "rel": "NOPE", "price": 1, "volume": 1 }),
            33,
        );
        assert_typed_refusal(
            &(format!("HTTP/1.1 {status}"), body.clone()),
            400,
            "NoSuchCoin",
            Some("object"),
            33,
        );
        assert_eq!(body["error_data"]["coin"], "NOPE", "setprice: error_data coin");
    }

    /// The legacy form of each of the six methods keeps answering its legacy body next to the versioned one (the
    /// recorded bodies are those of `legacy_order_methods_keep_their_answers_on_a_node_without_orders`).
    #[test]
    fn legacy_order_methods_still_answer_their_legacy_bodies_beside_the_versioned_forms() {
        let (_ctx, addr) = start_server();
        let (status, body) = post_legacy(addr, "my_orders", json!({}));
        assert_http_status(&status, 200);
        assert_eq!(body.to_string(), r#"{"result":{"maker_orders":{},"taker_orders":{}}}"#);
        let (status, body) = post_legacy(addr, "cancel_all_orders", json!({ "cancel_by": { "type": "All" } }));
        assert_http_status(&status, 200);
        assert_eq!(
            body.to_string(),
            r#"{"result":{"cancelled":[],"currently_matching":[]}}"#
        );
        let uuid = "2f9afe84-7a89-4194-8947-45fba563118f";
        let (status, body) = post_legacy(addr, "cancel_order", json!({ "uuid": uuid }));
        assert_http_status(&status, 404);
        assert_eq!(
            body.to_string(),
            format!(r#"{{"error":"Order with uuid {uuid} is not found"}}"#)
        );
        for (method, params) in [
            ("order_status", json!({ "uuid": uuid })),
            ("update_maker_order", json!({ "uuid": uuid })),
            (
                "setprice",
                json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 }),
            ),
        ] {
            assert_legacy_error_body(&post_legacy(addr, method, params));
        }
    }

    // ----- The group-B trading methods (plan `mmrpc-v2-completion`, phase 2) -----
    //
    // `buy`, `sell`, `orderbook_depth` and `orders_history_by_filter` follow the rules of group A: a Reloaded extension
    // since 0.5.0, `params` carries the legacy members unchanged, unknown members of `params` are ignored (the envelope
    // stays strict), `result` is the content of the legacy `result`, and the errors are the typed enums of the cores.
    // The legacy bodies recorded below are those the handlers of dev (`0f309c779`) answered.

    /// The group-B methods answer on the legacy surface as the dev handlers did, on a node without coins: each refusal
    /// is the text of its `ERR!`/`try_s!` site, as the legacy error body (HTTP 500, one member `error`). The messages
    /// of the assertions name the method only: the texts are error payloads.
    #[test]
    fn legacy_trading_methods_keep_their_answers_on_a_node_without_coins() {
        let (_ctx, addr) = start_server_with_coins(json!([{ "coin": "WALLET", "wallet_only": true }]));
        let refusal = |answer: &(String, Json), handler_file: &str, text: &str, method: &str| {
            assert!(
                legacy_error_text_without_location_of(answer, handler_file) == text,
                "{method}: the text of the refusal"
            );
        };
        let pair = json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1 });

        // dev `buy_impl` looks the rel coin up first, `sell_impl` the base coin.
        let answer = post_legacy(addr, "buy", pair.clone());
        refusal(
            &answer,
            "ordermatch_trading",
            "Rel coin is not found or inactive",
            "buy",
        );
        let answer = post_legacy(addr, "sell", pair);
        refusal(
            &answer,
            "ordermatch_trading",
            "Base coin is not found or inactive",
            "sell",
        );
        for method in ["buy", "sell"] {
            let same = json!({ "base": "RIN", "rel": "RIN", "price": 1, "volume": 1 });
            let answer = post_legacy(addr, method, same);
            refusal(
                &answer,
                "ordermatch_trading",
                "Base and rel must be different coins",
                method,
            );
            // A body that does not decode keeps the text of the decoder, one prefix in front.
            let answer = post_legacy(addr, method, json!({ "base": "RIN" }));
            refusal(&answer, "ordermatch_trading", "missing field `rel`", method);
        }

        // dev `orderbook_depth_rpc`: `ERR!("Pairs {:?} have wallet only coins", ..)` and the decoder's text.
        let answer = post_legacy(
            addr,
            "orderbook_depth",
            json!({ "pairs": [["WALLET", "KMD"], ["KMD", "RIN"]] }),
        );
        refusal(
            &answer,
            "orderbook_depth",
            r#"Pairs [("WALLET", "KMD")] have wallet only coins"#,
            "orderbook_depth",
        );
        let answer = post_legacy(addr, "orderbook_depth", json!({}));
        refusal(&answer, "orderbook_depth", "missing field `pairs`", "orderbook_depth");

        // dev `orders_history_by_filter`: a filter that does not decode keeps the text of the decoder.
        let answer = post_legacy(
            addr,
            "orders_history_by_filter",
            json!({ "from_timestamp": "yesterday" }),
        );
        assert!(
            legacy_error_text_without_location(&answer).starts_with("invalid type"),
            "orders_history_by_filter: the text of the decoder"
        );
    }

    /// Each of the four methods answers a versioned success in the 2.0 envelope with `id` echoed, and `result` has the
    /// members of the content that the legacy body carries under `result`. Every request carries an unknown member inside
    /// `params`, which is ignored. The taker order is compared with the object the dev handler rendered, a
    /// `LpautobuyResult` of the order the node keeps; the legacy form answers next to it.
    #[test]
    fn versioned_trading_methods_answer_the_legacy_result_in_the_2_0_envelope() {
        use crate::mm2::lp_ordermatch::order_rpc_tests::{dev_taker_order_result, Node};

        let node = Node::with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }));
        let ctx = &node.ctx;

        // `buy` and `sell`: the taker order object (the dev handlers answered `{"result": LpautobuyResult}`).
        for (id, method) in [(41, "buy"), (42, "sell")] {
            let result = versioned_result(
                call_v2(
                    ctx,
                    method,
                    json!({ "base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "min_volume": "0.1",
                            "save_in_history": false, "unknown_member": true }),
                    id,
                ),
                id,
            );
            let uuid: uuid::Uuid = result["uuid"].as_str().unwrap().parse().unwrap();
            assert_same_members(&result, &dev_taker_order_result(&node.taker_order(uuid)), method);

            let legacy = call_legacy(
                ctx,
                method,
                json!({ "base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "min_volume": "0.1",
                        "save_in_history": false }),
            );
            assert_eq!(legacy.0, 200);
            let legacy_uuid: uuid::Uuid = legacy.1["result"]["uuid"].as_str().unwrap().parse().unwrap();
            assert_same_members(
                &legacy.1["result"],
                &dev_taker_order_result(&node.taker_order(legacy_uuid)),
                method,
            );
        }

        // `orderbook_depth`: the list of pairs with their depth, as the legacy `result` carries it.
        node.subscribe_to_orderbook("DYN", "KMD");
        let result = versioned_result(
            call_v2(
                ctx,
                "orderbook_depth",
                json!({ "pairs": [["DYN", "KMD"]], "unknown_member": 1 }),
                43,
            ),
            43,
        );
        let legacy = call_legacy(ctx, "orderbook_depth", json!({ "pairs": [["DYN", "KMD"]] }));
        assert_eq!(legacy.0, 200);
        assert_eq!(result, legacy.1["result"]);
        assert_eq!(
            result,
            json!([{ "pair": ["DYN", "KMD"], "depth": { "asks": 0, "bids": 0 } }])
        );

        // `orders_history_by_filter`: `{orders, details, found_records, warnings}`, as the legacy `result` carries it.
        let order = node.add_order(true, true);
        for (id, params) in [
            (44, json!({})),
            (45, json!({ "include_details": true, "unknown_member": 1 })),
            (46, json!({ "base": "DYN", "unknown_member": 1 })),
            (47, json!({ "base": "NOPE" })),
        ] {
            let result = versioned_result(call_v2(ctx, "orders_history_by_filter", params.clone(), id), id);
            let legacy = call_legacy(ctx, "orders_history_by_filter", params.clone());
            assert_eq!(legacy.0, 200);
            assert_same_members(&result, &legacy.1["result"], "orders_history_by_filter");
            let found = if params["base"] == "NOPE" { 0 } else { 1 };
            assert_eq!(result["found_records"], found);
            if found == 1 {
                assert_eq!(result["orders"][0]["uuid"], order.uuid.to_string());
            }
            let details = if params["include_details"] == true { 1 } else { 0 };
            assert_eq!(result["details"].as_array().map(|details| details.len()), Some(details));
        }
    }

    /// The typed refusals a node without coins and orders reaches through the dispatcher: the token, the `error_data`
    /// kind, the HTTP status and the `id` of each. An unknown member inside `params` is ignored here too: the request
    /// is decoded and refused for its content. Variants that need a live coin or a storage are covered by the unit tests
    /// of the cores.
    #[test]
    fn versioned_trading_methods_answer_their_typed_refusals_on_a_node_without_coins() {
        let (_ctx, addr) = start_server_with_coins(json!([{ "coin": "WALLET", "wallet_only": true }]));

        // `buy` looks the rel coin up first, `sell` the base coin.
        let params = json!({ "base": "RIN", "rel": "KMD", "price": 1, "volume": 1, "unknown_member": 1 });
        let answer = post_v2(addr, "buy", Some(params.clone()), 51);
        assert_typed_refusal(&answer, 400, "NoSuchCoin", Some("object"), 51);
        assert!(answer.1["error_data"]["coin"] == "KMD", "buy: error_data coin");
        let answer = post_v2(addr, "sell", Some(params), 52);
        assert_typed_refusal(&answer, 400, "NoSuchCoin", Some("object"), 52);
        assert!(answer.1["error_data"]["coin"] == "RIN", "sell: error_data coin");

        for (id, method) in [(53, "buy"), (54, "sell")] {
            let same = json!({ "base": "RIN", "rel": "RIN", "price": 1, "volume": 1, "unknown_member": 1 });
            let answer = post_v2(addr, method, Some(same), id);
            assert_typed_refusal(&answer, 400, "BaseEqualRel", None, id);
            // Params that do not decode answer `InvalidRequest` (string payload, HTTP 400), as for every versioned method.
            let answer = post_v2(addr, method, Some(json!({ "base": "RIN" })), id + 10);
            assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), id + 10);
            let answer = post_v2(addr, method, None, id + 20);
            assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), id + 20);
        }

        let answer = post_v2(
            addr,
            "orderbook_depth",
            Some(json!({ "pairs": [["WALLET", "KMD"], ["KMD", "RIN"]], "unknown_member": 1 })),
            55,
        );
        assert_typed_refusal(&answer, 400, "CoinIsWalletOnly", Some("string"), 55);
        assert!(
            answer.1["error_data"] == r#"Pairs [("WALLET", "KMD")] have wallet only coins"#,
            "orderbook_depth: error_data text"
        );
        let answer = post_v2(addr, "orderbook_depth", Some(json!({})), 56);
        assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), 56);

        // A filter that does not decode answers `InvalidRequest` before the history is read.
        let answer = post_v2(
            addr,
            "orders_history_by_filter",
            Some(json!({ "from_timestamp": "yesterday" })),
            57,
        );
        assert_typed_refusal(&answer, 400, "InvalidRequest", Some("string"), 57);
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

    // ----- The level RPC refusals are logged at (client-caused refusals warn, genuine failures stay errors) -----

    /// Every refusal text the legacy surface can send for a request that is itself at fault, as the handlers word it.
    const CLIENT_CAUSED_TEXTS: &[&str] = &[
        "No such coin: RIN",
        "No such coin RIN",
        "Base coin RIN is not found",
        "Rel coin LTC-segwit is not found",
        "Rel coin is not found or inactive",
        "Base coin is not found or inactive",
        "Coin KMDCL is not activated",
        "Base coin RIN and/or rel coin KMD are not activated",
        "Not enough FLR for swap: available 0, required at least 1, locked by swaps None",
        "Not enough base coin ETH balance for swap: available 0, required at least 1, locked by swaps None",
        "IBC routing requires minimum balance on HTLC coin ATOM: required 1, current 0",
        "Max base vol 0.5 is below min base vol: 1",
        "Max base vol 0.00000001 is too low, required: 0.0001",
        "Min base vol 0.00000001 is too low, required: 0.0001",
        "Max rel vol 0.00000001 is too low, required: 0.0001",
        "Base amount 0.00000001 is too low, required: 0.0001",
        "Rel amount 0.00000001 is too low, required: 0.0001",
        "Min volume 0.00000001 is too low, required: 0.0001",
        "Price 0.000000001 is too low, required: 0.00000001",
        "Price is too low, minimum is 0.00000001",
        "The volume 0.00000001 of the RIN coin is too low, required at least 0.0001: a payment of that size could not be spent or refunded",
        "The volume 0.00000001 of the RIN coin less than minimum transaction amount 0.0001",
        "New volume 0 should be more than zero",
        "New volume 1 should be more than reserved amount for order matches 2",
        "Rel coin can not be same as base",
        "Base and rel must be different coins",
        "Trading is not supported for coin LN",
        "Base coin RIN is wallet only",
        "Rel coin RIN is wallet only",
    ];

    /// Texts of refusals that are not the request's fault, or that no list may soften: they stay errors.
    const GENUINE_FAILURE_TEXTS: &[&str] = &[
        "Transport error: all EVM nodes failed: connection refused",
        "Userpass is invalid!",
        "Userpass is not set!",
        "Your ip is banned.",
        "Internal error: database is locked",
        "Only POST requests are supported!",
        "Invalid input",
        "expected value at line 1 column 1",
        "Cannot check the RIN coin spend safety: no fee estimate",
        "Taker payment is not found, swap is not recoverable",
        "No such mode: simple",
        "",
    ];

    /// An error text as the ERRL macros nest it: every layer adds its `file:line] ` prefix.
    fn nested(text: &str) -> String { format!("lp_ordermatch:2611] lp_commands_legacy:84] {text}") }

    #[test]
    fn legacy_refusals_the_request_caused_are_classified_as_client_caused() {
        for text in CLIENT_CAUSED_TEXTS {
            assert!(legacy_error_is_client_caused(text), "{text}");
            assert!(
                legacy_error_is_client_caused(&format!("lp_ordermatch:2611] {text}")),
                "{text}"
            );
            assert!(legacy_error_is_client_caused(&nested(text)), "{text}");
            assert!(
                legacy_error_is_client_caused(&format!("{} (retry)", nested(text))),
                "{text}"
            );
        }
    }

    #[test]
    fn legacy_failures_of_the_node_are_not_classified_as_client_caused() {
        for text in GENUINE_FAILURE_TEXTS {
            assert!(!legacy_error_is_client_caused(text), "{text:?}");
            assert!(!legacy_error_is_client_caused(&nested(text)), "{text:?}");
        }
    }

    #[test]
    fn a_fragment_of_a_pair_alone_does_not_classify_a_text() {
        // Each of these carries only one half of a fragment pair.
        for text in [
            "The coin RIN is fine",
            "Rel amount 5 was accepted",
            "Min volume 5 was accepted",
            "Price 5 was accepted",
            "Not enough peers",
            "x is too low",
            "the swap is not recoverable",
        ] {
            assert!(!legacy_error_is_client_caused(text), "{text}");
        }
    }

    #[test]
    fn rpc_error_log_level_warns_for_client_errors_and_errors_otherwise() {
        for code in [400, 404, 409, 422, 429, 499] {
            let status = StatusCode::from_u16(code).unwrap();
            assert_eq!(rpc_error_log_level(status), Level::Warn, "{code}");
        }
        // Authentication, access and timeout refusals are reported like failures.
        for code in [401, 403, 408, 500, 501, 502, 503, 504] {
            let status = StatusCode::from_u16(code).unwrap();
            assert_eq!(rpc_error_log_level(status), Level::Error, "{code}");
        }
        // The dispatcher's own statuses.
        for error in [
            DispatcherError::NoSuchMethod,
            DispatcherError::InvalidRequest("x".to_owned()),
            DispatcherError::InvalidMmRpcVersion("x".to_owned()),
        ] {
            assert_eq!(rpc_error_log_level(error.status_code()), Level::Warn);
        }
        for error in [
            DispatcherError::Banned,
            DispatcherError::LocalHostOnly,
            DispatcherError::UserpassIsNotSet,
        ] {
            assert_eq!(rpc_error_log_level(error.status_code()), Level::Error);
        }
    }

    mod capture {
        use common::log::log_crate::{set_logger, set_max_level, Level, LevelFilter, Log, Metadata, Record};
        use std::sync::Mutex;

        static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

        struct Capture;

        impl Log for Capture {
            fn enabled(&self, metadata: &Metadata) -> bool { metadata.level() <= Level::Warn }

            fn log(&self, record: &Record) {
                if self.enabled(record.metadata()) {
                    LINES.lock().unwrap_or_else(|e| e.into_inner()).push(format!(
                        "{} {}",
                        record.level(),
                        record.args()
                    ));
                }
            }

            fn flush(&self) {}
        }

        static CAPTURE: Capture = Capture;

        /// Installs the capture of the warn and error lines. `false` when another logger already owns the process, in
        /// which case a test must not assert that a line is present.
        pub(super) fn installed() -> bool {
            let installed = set_logger(&CAPTURE).is_ok();
            if installed {
                set_max_level(LevelFilter::Warn);
            }
            // `set_logger` fails the second time too: the capture is ours when a line can be seen.
            common::log::log_crate::warn!("rpc log level capture probe");
            installed
                || LINES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .any(|l| l.contains("rpc log level capture probe"))
        }

        /// The captured lines (`"{level} {text}"`) that contain `needle`.
        pub(super) fn lines_with(needle: &str) -> Vec<String> {
            LINES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|l| l.contains(needle))
                .cloned()
                .collect()
        }
    }

    /// Sends `body` with the request line `method / HTTP/1.1` and returns the status line and the JSON body, as
    /// [`post`] does for a POST.
    fn send_with_method(addr: SocketAddr, method: &str, body: &str) -> (String, Json) {
        let mut stream = connect(addr);
        write!(
            stream,
            "{method} / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.lines().next().unwrap_or_default().to_owned();
        let (_, payload) = response.split_once("\r\n\r\n").expect("a complete response");
        let payload = serde_json::from_str(payload).unwrap_or_else(|e| panic!("{e}: {response}"));
        (status, payload)
    }

    /// A legacy request that the request itself caused to fail (an unknown coin): the answer is unchanged (the legacy
    /// error body, HTTP 500) and the line is logged at WARN, not at ERROR.
    #[test]
    fn legacy_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged() {
        let capturing = capture::installed();
        let (_ctx, addr) = start_server();
        let failure = post_legacy(addr, "my_balance", json!({ "coin": "NO_SUCH_COIN_LOG_LEVEL_A" }));
        assert_legacy_error_body(&failure);
        let error = failure.1["error"].as_str().unwrap();
        assert!(
            error.contains("No such coin: NO_SUCH_COIN_LOG_LEVEL_A"),
            "{}",
            failure.1
        );

        if !capturing {
            eprintln!("legacy_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged: another logger owns the process, line assertions skipped");
            return;
        }
        let lines = capture::lines_with("No such coin: NO_SUCH_COIN_LOG_LEVEL_A");
        assert!(!lines.is_empty(), "the refusal is logged");
        for line in &lines {
            assert!(line.starts_with("WARN RPC error response: "), "{line}");
        }
    }

    /// A versioned request whose handler answers a 4xx: the mmrpc error is unchanged and the line is logged at WARN.
    #[test]
    fn versioned_handler_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged() {
        let capturing = capture::installed();
        let (_ctx, addr) = start_server();
        let request = json!({
            "mmrpc": "2.0",
            "userpass": "pw",
            "method": "min_trading_vol",
            "params": { "coin": "NO_SUCH_COIN_LOG_LEVEL_B" },
            "id": 8,
        });
        let answer = post(addr, &request);
        assert_mmrpc_error(&answer, 400, "NoSuchCoin", json!(8));
        assert_eq!(
            answer.1["error_data"]["coin"], "NO_SUCH_COIN_LOG_LEVEL_B",
            "{}",
            answer.1
        );

        if !capturing {
            eprintln!("versioned_handler_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged: another logger owns the process, line assertions skipped");
            return;
        }
        let lines = capture::lines_with("No such coin: NO_SUCH_COIN_LOG_LEVEL_B");
        assert!(!lines.is_empty(), "the refusal is logged");
        for line in &lines {
            assert!(line.starts_with("WARN RPC error response: "), "{line}");
        }
    }

    /// A request the dispatcher itself refuses with a 4xx (an unknown method): the mmrpc error is unchanged and the
    /// line is logged at WARN. Other tests produce the same text, which is also a 400, so every such line is checked.
    #[test]
    fn dispatcher_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged() {
        let capturing = capture::installed();
        let (_ctx, addr) = start_server();
        let answer = post(
            addr,
            &json!({ "mmrpc": "2.0", "userpass": "pw", "method": "no_such_method_in_any_table", "params": {}, "id": 9 }),
        );
        assert_mmrpc_error(&answer, 400, "NoSuchMethod", json!(9));
        assert!(answer.1.get("error_data").is_none(), "{}", answer.1);

        if !capturing {
            eprintln!("dispatcher_refusal_the_request_caused_is_logged_at_warn_and_answered_unchanged: another logger owns the process, line assertions skipped");
            return;
        }
        let lines = capture::lines_with("No such method");
        assert!(!lines.is_empty(), "the refusal is logged");
        for line in &lines {
            assert!(line.starts_with("WARN RPC dispatcher error: "), "{line}");
        }
    }

    /// A legacy refusal whose text is not listed stays an error: a GET request is answered by the legacy error body
    /// (HTTP 500) and the line is logged at ERROR, because its text is not in the table. (No cheaper genuine 5xx path
    /// exists on the stub server.)
    #[test]
    fn legacy_failure_that_is_not_client_caused_is_logged_at_error_and_answered_unchanged() {
        let capturing = capture::installed();
        let (_ctx, addr) = start_server();
        let failure = send_with_method(addr, "GET", "{}");
        assert_legacy_error_body(&failure);
        let error = failure.1["error"].as_str().unwrap();
        assert!(error.contains("Only POST requests are supported!"), "{}", failure.1);

        if !capturing {
            eprintln!("legacy_failure_that_is_not_client_caused_is_logged_at_error_and_answered_unchanged: another logger owns the process, line assertions skipped");
            return;
        }
        let lines = capture::lines_with("Only POST requests are supported!");
        assert!(!lines.is_empty(), "the failure is logged");
        for line in &lines {
            assert!(line.starts_with("ERROR RPC error response: "), "{line}");
        }
    }
}
