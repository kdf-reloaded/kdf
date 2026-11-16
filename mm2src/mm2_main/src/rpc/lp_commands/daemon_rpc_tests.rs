//! Tests of the typed cores of the daemon and network RPC methods of group E (CRD ch.43 §43.1R; plan
//! `mmrpc-v2-completion`, phase 4): `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`,
//! `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `ban_pubkey`,
//! `list_banned_pubkeys` and `unban_pubkeys`.
//!
//! * The legacy arms keep their answers. Every expected legacy body below is the body that the handler produced as it
//!   stood on dev at `c7116883e`, derived from its code and marked with the handler that produced it; the expected
//!   error texts are those of its `ERR!`/`try_s!` sites. The `file:line] ` tokens in front of a legacy error text change
//!   with every edit of the source, so they are split off, checked to name the file of the handler, and the text behind
//!   them is compared whole ([`legacy_refusal`]).
//!   The expected bodies were compared with the answers of a release build in the lab row of PR #387.
//! * The wire forms of the error enums are checked in `daemon_rpc_errors.rs`.
//! * The twelve methods are registered on the versioned surface. Requests run through the whole server path
//!   (`process_single_request`: the legacy table, the fallback, the versioned dispatcher) on the test thread.
//! * No assertion message carries an error payload, a body or a `Display` text: only tokens, method names and labels.

use super::*;
use crate::mm2::lp_network::P2PContext;
use crate::mm2::MmVersionResult;
use common::block_on;
use futures::channel::mpsc;
use futures::StreamExt;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_metrics::MetricsOps;
use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
use serde_json::json;
use std::collections::HashMap;

/// `actual` is `expected`; the message names `what` only (the values are bodies or texts).
fn same<T: PartialEq>(actual: &T, expected: &T, what: &str) {
    assert!(actual == expected, "{what}: the values differ");
}

/// Splits the `file:line] ` tokens that `try_s!` and `ERR!` put in front of a legacy error text.
fn split_location(error: &str) -> (Vec<String>, String) {
    let token = Regex::new(r"^([A-Za-z0-9_]+):\d+\] ").unwrap();
    let mut files = Vec::new();
    let mut rest = error;
    while let Some(found) = token.captures(rest) {
        files.push(found[1].to_owned());
        rest = &rest[found[0].len()..];
    }
    (files, rest.to_owned())
}

fn client() -> SocketAddr { SocketAddr::from(([127, 0, 0, 1], 4242)) }

/// Runs `request` through the whole server path on the test thread.
fn run(ctx: &MmArc, request: Json) -> Result<Response<Vec<u8>>, String> {
    block_on(process_single_request(ctx.clone(), request, client()))
}

/// A legacy request: no `mmrpc` key, the members inline next to `method`, the server password.
fn legacy_request(method: &str, params: Json) -> Json {
    let mut request = json!({ "userpass": "pw", "method": method });
    for (key, value) in params.as_object().expect("the inline parameters are an object") {
        request[key.as_str()] = value.clone();
    }
    request
}

/// A legacy answer that is a response: returns it whole (status, headers, body).
fn legacy_response(ctx: &MmArc, method: &str, params: Json) -> Response<Vec<u8>> {
    match run(ctx, legacy_request(method, params)) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: a legacy response was expected"),
    }
}

/// A legacy success: HTTP 200. Returns the body text exactly as the client receives it.
fn legacy_success(ctx: &MmArc, method: &str, params: Json) -> String {
    let answer = legacy_response(ctx, method, params);
    assert_eq!(answer.status().as_u16(), 200, "{method}: legacy success status");
    String::from_utf8(answer.into_body()).unwrap()
}

/// A legacy refusal: the legacy surface answers it as `{"error": <text>}` with HTTP 500, which is what an `Err(String)`
/// of the handler becomes. Returns the text without its `file:line] ` tokens; the innermost token names `handler_file`
/// (the file name without its extension).
fn legacy_refusal(ctx: &MmArc, method: &str, params: Json, handler_file: &str) -> String {
    let error = match run(ctx, legacy_request(method, params)) {
        Ok(_) => panic!("{method}: a legacy refusal was expected"),
        Err(error) => error,
    };
    let (files, rest) = split_location(&error);
    assert!(
        files.last().map(String::as_str) == Some(handler_file),
        "{method}: the legacy error names its handler file"
    );
    rest
}

/// Sends a versioned request `request` and returns the status and the JSON body of the answer.
fn send_versioned(ctx: &MmArc, method: &str, request: Json) -> (u16, Json) {
    let answer = match run(ctx, request) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: the versioned surface answers every request"),
    };
    let body = json::from_slice(answer.body()).unwrap_or_else(|_| panic!("the answer is JSON"));
    (answer.status().as_u16(), body)
}

/// A versioned request. `params` is left out when `None`.
fn versioned(ctx: &MmArc, method: &str, params: Option<Json>, id: usize) -> (u16, Json) {
    let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": id });
    if let Some(params) = params {
        request["params"] = params;
    }
    send_versioned(ctx, method, request)
}

/// A versioned request of a public method: no `userpass` member. `params` is left out when `None`.
fn versioned_public(ctx: &MmArc, method: &str, params: Option<Json>, id: usize) -> (u16, Json) {
    let mut request = json!({ "mmrpc": "2.0", "method": method, "id": id });
    if let Some(params) = params {
        request["params"] = params;
    }
    send_versioned(ctx, method, request)
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

/// The typed refusal: HTTP status, `error_type` token, the JSON type of `error_data` (`None`: the key is absent) and
/// the echoed `id`, in the mmrpc 2.0 error envelope.
fn assert_typed_refusal(answer: &(u16, Json), status: u16, token: &str, data: Option<&str>, id: usize) {
    let (code, body) = answer;
    assert_eq!(*code, status, "{token}: status");
    assert_eq!(body["mmrpc"], "2.0", "{token}: mmrpc version");
    assert_eq!(body["error_type"], token, "{token}: error_type");
    assert_eq!(body.get("id"), Some(&json!(id)), "{token}: id");
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

/// The forms `params` takes for a method without members: absent, `null`, `{}` and an object with an unknown member.
fn parameterless_forms(first_id: usize) -> [(Option<Json>, usize); 4] {
    [
        (None, first_id),
        (Some(Json::Null), first_id + 1),
        (Some(json!({})), first_id + 2),
        (Some(json!({ "unknown_member": 1 })), first_id + 3),
    ]
}

/// A node without a P2P context, a peer identifier, metrics or bans.
fn bare_node() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({ "rpc_password": "pw" }))
        .into_mm_arc()
}

/// A node whose P2P command channel is answered by a task with fixed peer, gossip and relay data.
fn node_with_p2p_answers() -> MmArc {
    let ctx = bare_node();
    let (tx, mut rx) = mpsc::channel(16);
    P2PContext::new(tx).store_to_mm_arc(&ctx);
    common::executor::spawn(async move {
        while let Some(command) = rx.next().await {
            let map_of = |key: &str, values: &[&str]| -> HashMap<String, Vec<String>> {
                HashMap::from([(key.to_owned(), values.iter().map(|v| (*v).to_owned()).collect())])
            };
            match command {
                AdexBehaviourCmd::GetPeersInfo { result_tx } => {
                    let _ = result_tx.send(map_of("peer-a", &["/ip4/1.2.3.4/tcp/1"]));
                },
                AdexBehaviourCmd::GetGossipMesh { result_tx } => {
                    let _ = result_tx.send(map_of("topic-a", &["peer-a"]));
                },
                AdexBehaviourCmd::GetGossipPeerTopics { result_tx } => {
                    let _ = result_tx.send(map_of("peer-a", &["topic-a"]));
                },
                AdexBehaviourCmd::GetGossipTopicPeers { result_tx } => {
                    let _ = result_tx.send(map_of("topic-a", &["peer-a", "peer-b"]));
                },
                AdexBehaviourCmd::GetRelayMesh { result_tx } => {
                    let _ = result_tx.send(vec!["relay-a".to_owned(), "relay-b".to_owned()]);
                },
                _ => {},
            }
        }
    });
    ctx
}

// ----- `version`, `stop`, `metrics` -----

/// The body of the dev `lp_commands_legacy::version`: `MmVersionResult::new().to_json()` as the whole body, HTTP 200,
/// `application/json`. The versioned `result` is that whole object (a bare legacy body becomes the value of `result`).
#[test]
fn version_answers_the_legacy_body_and_the_versioned_result() {
    let ctx = bare_node();
    let expected = MmVersionResult::new().to_json();

    let answer = legacy_response(&ctx, "version", json!({}));
    assert_eq!(answer.status().as_u16(), 200, "legacy status");
    assert_eq!(
        answer.headers().get("content-type").map(|v| v.as_bytes()),
        Some(&b"application/json"[..]),
        "legacy content type"
    );
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &expected.to_string(),
        "the legacy body",
    );

    for (params, id) in parameterless_forms(301) {
        same(
            &versioned_result(versioned(&ctx, "version", params, id), id),
            &expected,
            "the versioned result",
        );
    }
}

/// The body of the dev `lp_commands_legacy::stop`: `{"result": "success"}`. The shutdown starts after the answer.
#[test]
fn stop_answers_the_legacy_body_and_the_versioned_result_and_then_stops_the_node() {
    for versioned_form in [false, true] {
        let ctx = bare_node();
        if versioned_form {
            same(
                &versioned_result(versioned(&ctx, "stop", None, 311), 311),
                &json!("success"),
                "the versioned result",
            );
        } else {
            same(
                &legacy_success(&ctx, "stop", json!({})),
                &r#"{"result":"success"}"#.to_owned(),
                "the legacy body",
            );
        }
        // The shutdown is scheduled a moment after the answer.
        let mut stopped = false;
        for _ in 0..100 {
            if ctx.is_stopping() {
                stopped = true;
                break;
            }
            block_on(common::executor::Timer::sleep(0.05));
        }
        assert!(stopped, "the node stops after the answer");
    }
}

/// A node whose metrics are not initialized: the dev handler answers `rpc_err_response(500, ..)`, the body
/// `{"error": <text>}` with HTTP 500; the versioned form is `Internal` (string, 500). `metrics` is public: it needs no
/// password on either surface.
#[test]
fn metrics_answers_a_node_without_metrics_with_its_text() {
    let ctx = bare_node();
    let answer = legacy_response(&ctx, "metrics", json!({}));
    assert_eq!(answer.status().as_u16(), 500, "legacy status");
    let body: Json = json::from_slice(answer.body()).unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|text| text.ends_with("metrics system is not initialized yet")),
        "the legacy text"
    );

    // Without the password.
    let answer = run(&ctx, json!({ "method": "metrics" })).unwrap_or_else(|_| panic!("a legacy response"));
    assert_eq!(answer.status().as_u16(), 500, "legacy status without the password");

    let answer = versioned(&ctx, "metrics", None, 321);
    assert_typed_refusal(&answer, 500, "Internal", Some("string"), 321);
    assert!(
        answer.1["error_data"]
            .as_str()
            .is_some_and(|text| text.ends_with("metrics system is not initialized yet")),
        "the versioned text"
    );
    assert_typed_refusal(
        &versioned_public(&ctx, "metrics", None, 322),
        500,
        "Internal",
        Some("string"),
        322,
    );
}

/// An initialized registry without records: the dev handler's body is the bare `{"metrics": []}` (HTTP 200,
/// `application/json`); the versioned `result` is that object.
#[test]
fn metrics_answers_the_legacy_body_and_the_versioned_result() {
    let ctx = bare_node();
    ctx.metrics.init().unwrap();

    let answer = legacy_response(&ctx, "metrics", json!({}));
    assert_eq!(answer.status().as_u16(), 200, "legacy status");
    assert_eq!(
        answer.headers().get("content-type").map(|v| v.as_bytes()),
        Some(&b"application/json"[..]),
        "legacy content type"
    );
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"metrics":[]}"#.to_owned(),
        "the legacy body",
    );

    for (params, id) in parameterless_forms(331) {
        same(
            &versioned_result(versioned(&ctx, "metrics", params, id), id),
            &json!({ "metrics": [] }),
            "the versioned result",
        );
    }
    same(
        &versioned_result(versioned_public(&ctx, "metrics", None, 335), 335),
        &json!({ "metrics": [] }),
        "the versioned result without the password",
    );
}

// ----- `get_my_peer_id` and the peer and gossip queries -----

/// The dev `get_my_peer_id` answers `{"result": "<peer id>"}`, and `ERR!("Peer ID is not initialized")` without one.
#[test]
fn get_my_peer_id_answers_the_legacy_body_and_the_versioned_result() {
    let ctx = bare_node();
    same(
        &legacy_refusal(&ctx, "get_my_peer_id", json!({}), "lp_commands_legacy"),
        &"Peer ID is not initialized".to_owned(),
        "the not-initialized text",
    );
    let answer = versioned(&ctx, "get_my_peer_id", None, 341);
    assert_typed_refusal(&answer, 500, "Internal", Some("string"), 341);
    assert!(
        answer.1["error_data"] == json!("Peer ID is not initialized"),
        "the versioned text"
    );

    ctx.peer_id.pin("12D3KooWTestPeer".to_owned()).unwrap();
    same(
        &legacy_success(&ctx, "get_my_peer_id", json!({})),
        &r#"{"result":"12D3KooWTestPeer"}"#.to_owned(),
        "the legacy body",
    );
    for (params, id) in parameterless_forms(342) {
        same(
            &versioned_result(versioned(&ctx, "get_my_peer_id", params, id), id),
            &json!("12D3KooWTestPeer"),
            "the versioned result",
        );
    }
}

/// The five queries of the P2P layer answer `{"result": <map or list>}` on the legacy surface (the dev handlers of
/// `lp_commands_legacy`) and the content of `result` on the versioned one.
#[test]
fn peer_and_gossip_queries_answer_the_legacy_body_and_the_versioned_result() {
    let ctx = node_with_p2p_answers();
    let cases = [
        (
            "get_directly_connected_peers",
            r#"{"result":{"peer-a":["/ip4/1.2.3.4/tcp/1"]}}"#,
            json!({ "peer-a": ["/ip4/1.2.3.4/tcp/1"] }),
        ),
        (
            "get_gossip_mesh",
            r#"{"result":{"topic-a":["peer-a"]}}"#,
            json!({ "topic-a": ["peer-a"] }),
        ),
        (
            "get_gossip_peer_topics",
            r#"{"result":{"peer-a":["topic-a"]}}"#,
            json!({ "peer-a": ["topic-a"] }),
        ),
        (
            "get_gossip_topic_peers",
            r#"{"result":{"topic-a":["peer-a","peer-b"]}}"#,
            json!({ "topic-a": ["peer-a", "peer-b"] }),
        ),
        (
            "get_relay_mesh",
            r#"{"result":["relay-a","relay-b"]}"#,
            json!(["relay-a", "relay-b"]),
        ),
    ];
    for (index, (method, legacy_body, result)) in cases.into_iter().enumerate() {
        same(
            &legacy_success(&ctx, method, json!({})),
            &legacy_body.to_owned(),
            "the legacy body",
        );
        for (params, id) in parameterless_forms(351 + index * 10) {
            same(
                &versioned_result(versioned(&ctx, method, params, id), id),
                &result,
                "the versioned result",
            );
        }
    }
}

// ----- `ban_pubkey`, `list_banned_pubkeys`, `unban_pubkeys` -----

/// A public key of 32 equal bytes: its hex text reads the same in either byte order.
fn pubkey(byte: u8) -> String { format!("{byte:02x}").repeat(32) }

/// The dev `ban_pubkey` answers `{"result": "success"}`; `list_banned_pubkeys` answers `{"result": {<pubkey>:
/// {"type": "Manual", "reason": ..}}}`; a key that is banned already is refused with `ERR!("Pubkey is banned
/// already")`, which is `InvalidParam` (string, 400) on the versioned surface.
#[test]
fn ban_pubkey_and_list_banned_pubkeys_answer_the_legacy_bodies_and_the_versioned_results() {
    let ctx = bare_node();
    let (first, second) = (pubkey(1), pubkey(2));

    same(
        &legacy_success(&ctx, "list_banned_pubkeys", json!({})),
        &r#"{"result":{}}"#.to_owned(),
        "the empty legacy list",
    );
    same(
        &legacy_success(&ctx, "ban_pubkey", json!({ "pubkey": first, "reason": "legacy" })),
        &r#"{"result":"success"}"#.to_owned(),
        "the legacy ban body",
    );
    same(
        &legacy_refusal(
            &ctx,
            "ban_pubkey",
            json!({ "pubkey": first, "reason": "again" }),
            "pubkey_banning",
        ),
        &"Pubkey is banned already".to_owned(),
        "the banned-already text",
    );
    assert_typed_refusal(
        &versioned(
            &ctx,
            "ban_pubkey",
            Some(json!({ "pubkey": first, "reason": "again" })),
            361,
        ),
        400,
        "InvalidParam",
        Some("string"),
        361,
    );

    // The versioned ban takes the same members inside `params`, the duration included, and ignores an unknown member.
    same(
        &versioned_result(
            versioned(
                &ctx,
                "ban_pubkey",
                Some(json!({ "pubkey": second, "reason": "versioned", "duration_min": 5, "unknown_member": true })),
                362,
            ),
            362,
        ),
        &json!("success"),
        "the versioned ban result",
    );

    let expected = json!({
        first.clone(): { "type": "Manual", "reason": "legacy" },
        second.clone(): { "type": "Manual", "reason": "versioned" },
    });
    let legacy: Json = json::from_str(&legacy_success(&ctx, "list_banned_pubkeys", json!({}))).unwrap();
    same(&legacy["result"], &expected, "the legacy list");
    for (params, id) in parameterless_forms(363) {
        same(
            &versioned_result(versioned(&ctx, "list_banned_pubkeys", params, id), id),
            &expected,
            "the versioned list",
        );
    }
}

/// A request that cannot be decoded is refused before the core runs: the legacy handler with the text of the decoder,
/// the versioned dispatcher with `InvalidRequest` (string, 400). That includes a public key that is not 32 hex bytes.
#[test]
fn ban_pubkey_and_unban_pubkeys_refuse_a_request_that_cannot_be_decoded() {
    let ctx = bare_node();
    same(
        &legacy_refusal(&ctx, "ban_pubkey", json!({ "pubkey": pubkey(1) }), "pubkey_banning"),
        &"missing field `reason`".to_owned(),
        "the missing-reason text",
    );
    assert!(
        !legacy_refusal(
            &ctx,
            "ban_pubkey",
            json!({ "pubkey": "not hex", "reason": "r" }),
            "pubkey_banning"
        )
        .is_empty(),
        "the malformed-key text"
    );
    assert!(
        !legacy_refusal(&ctx, "unban_pubkeys", json!({}), "pubkey_banning").is_empty(),
        "the missing-unban_by text"
    );

    let cases = [
        ("ban_pubkey", Some(json!({ "pubkey": pubkey(1) }))),
        ("ban_pubkey", Some(json!({ "pubkey": "not hex", "reason": "r" }))),
        ("ban_pubkey", None),
        ("unban_pubkeys", Some(json!({}))),
        ("unban_pubkeys", Some(json!({ "unban_by": { "type": "Some" } }))),
        ("unban_pubkeys", None),
    ];
    for (index, (method, params)) in cases.into_iter().enumerate() {
        let id = 380 + index;
        assert_typed_refusal(
            &versioned(&ctx, method, params, id),
            400,
            "InvalidRequest",
            Some("string"),
            id,
        );
    }
}

/// The dev `unban_pubkeys` answers `{"result": {"still_banned", "unbanned", "were_not_banned"}}`, the members in this
/// order. A listed key that is not banned is in `were_not_banned`; an empty list unbans nothing and is a success.
#[test]
fn unban_pubkeys_answers_the_legacy_body_and_the_versioned_result() {
    let ctx = bare_node();
    let (first, second, third) = (pubkey(1), pubkey(2), pubkey(3));
    for key in [&first, &second] {
        same(
            &legacy_success(&ctx, "ban_pubkey", json!({ "pubkey": key, "reason": "r" })),
            &r#"{"result":"success"}"#.to_owned(),
            "the ban body",
        );
    }
    let manual = json!({ "type": "Manual", "reason": "r" });

    same(
        &legacy_success(
            &ctx,
            "unban_pubkeys",
            json!({ "unban_by": { "type": "Few", "data": [first, third] } }),
        ),
        &json::to_string(&json!({
            "result": {
                "still_banned": { second.clone(): manual.clone() },
                "unbanned": { first.clone(): manual.clone() },
                "were_not_banned": [third],
            }
        }))
        .unwrap(),
        "the legacy body",
    );

    // An empty list unbans nothing.
    same(
        &versioned_result(
            versioned(
                &ctx,
                "unban_pubkeys",
                Some(json!({ "unban_by": { "type": "Few", "data": [] }, "unknown_member": 1 })),
                391,
            ),
            391,
        ),
        &json!({ "still_banned": { second.clone(): manual.clone() }, "unbanned": {}, "were_not_banned": [] }),
        "the versioned result of an empty list",
    );

    same(
        &versioned_result(
            versioned(
                &ctx,
                "unban_pubkeys",
                Some(json!({ "unban_by": { "type": "All" } })),
                392,
            ),
            392,
        ),
        &json!({ "still_banned": {}, "unbanned": { second.clone(): manual }, "were_not_banned": [] }),
        "the versioned result of All",
    );
    same(
        &legacy_success(&ctx, "list_banned_pubkeys", json!({})),
        &r#"{"result":{}}"#.to_owned(),
        "the list after the unban",
    );
}

// ----- The envelope -----

/// CRD ch.43 R43.1A.2: a legacy-shaped body (parameters inline) with an added `"mmrpc": "2.0"` is undecodable under the
/// versioned envelope and answers `InvalidRequest` (string payload, HTTP 400) before any method lookup, for the methods
/// of the group that take members.
#[test]
fn legacy_shaped_body_with_an_mmrpc_key_answers_invalid_request_for_the_methods_with_members() {
    let ctx = bare_node();
    let cases = [
        ("ban_pubkey", json!({ "pubkey": pubkey(1), "reason": "r" })),
        ("unban_pubkeys", json!({ "unban_by": { "type": "All" } })),
        ("version", json!({ "unknown": 1 })),
        ("get_relay_mesh", json!({ "unknown": 1 })),
    ];
    for (method, inline) in cases {
        let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": 3 });
        for (key, value) in inline.as_object().unwrap() {
            request[key.as_str()] = value.clone();
        }
        let answer = match run(&ctx, request) {
            Ok(answer) => answer,
            Err(_) => panic!("{method}: the versioned surface answers every request"),
        };
        let body: Json = json::from_slice(answer.body()).unwrap();
        assert_eq!(answer.status().as_u16(), 400, "{method}: status");
        assert_eq!(body["error_type"], "InvalidRequest", "{method}: error_type");
        assert!(body["error_data"].is_string(), "{method}: error_data kind");
        assert_eq!(body["id"], 3, "{method}: id");
    }
}
