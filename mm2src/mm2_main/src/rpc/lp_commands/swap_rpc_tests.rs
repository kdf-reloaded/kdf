//! Tests of the typed cores of the swap-list RPC methods of group C (CRD ch.43 §43.1R; plan `mmrpc-v2-completion`,
//! phase 4): `all_swaps_uuids_by_filter` and `import_swaps`.
//!
//! * The legacy arms keep their answers. Every expected legacy body below is the body that the handler produced as it
//!   stood on dev at `c7116883e`, derived from its code and marked with the handler that produced it; the expected
//!   error texts are those of its `try_s!` sites. The `file:line] ` tokens in front of a legacy error text change with
//!   every edit of the source, so they are split off, checked to name the file of the handler, and the text behind
//!   them is compared whole ([`legacy_refusal`]).
//!   The expected bodies were compared with the answers of a release build in the lab row of PR #387.
//! * The wire forms of the error enum of `all_swaps_uuids_by_filter` are checked in `lp_swap/swap_rpc_errors.rs`
//!   (`import_swaps` has no error value).
//! * The two methods are registered on the versioned surface. Requests run through the whole server path
//!   (`process_single_request`: the legacy table, the fallback, the versioned dispatcher) on the test thread.
//! * No assertion message carries an error payload, a body or a `Display` text: only tokens, method names and labels.

use super::*;
use crate::mm2::database::init_and_migrate_db;
use crate::mm2::lp_swap::my_swaps_dir;
use crate::mm2::lp_swap::terminal_report_tests::{record, Outcome, Role};
use common::block_on;
use db_common::sqlite::rusqlite::Connection;
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The start time of every record of `record`.
const STARTED_AT: u64 = 1_699_990_000;

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

/// A legacy success: HTTP 200. Returns the body text exactly as the client receives it.
fn legacy_success(ctx: &MmArc, method: &str, params: Json) -> String {
    let answer = match run(ctx, legacy_request(method, params)) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: a legacy success was expected"),
    };
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

/// A versioned request. `params` is left out when `None`.
fn versioned(ctx: &MmArc, method: &str, params: Option<Json>, id: usize) -> (u16, Json) {
    let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": id });
    if let Some(params) = params {
        request["params"] = params;
    }
    let answer = match run(ctx, request) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: the versioned surface answers every request"),
    };
    let body = json::from_slice(answer.body()).unwrap_or_else(|_| panic!("the answer is JSON"));
    (answer.status().as_u16(), body)
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

/// A node with an in-memory swap index (every migration applied), as a started node has it, and a directory for the
/// swap records. The node removes its database directory when it is dropped.
struct SwapNode {
    ctx: MmArc,
    dbdir: std::path::PathBuf,
}

impl SwapNode {
    fn new() -> SwapNode { SwapNode::build(true) }

    /// A node whose directory for the swap records does not exist: no record can be stored.
    fn without_record_directory() -> SwapNode { SwapNode::build(false) }

    fn build(with_record_directory: bool) -> SwapNode {
        let dbdir = std::env::temp_dir().join(format!("kdf-swap-list-rpc-{}", Uuid::new_v4()));
        let conf = json!({ "rpc_password": "pw", "netid": 8762, "dbdir": dbdir.to_string_lossy() });
        let ctx = MmCtxBuilder::default().with_conf(conf).into_mm_arc();
        let _ = ctx
            .sqlite_connection
            .pin(Arc::new(Mutex::new(Connection::open_in_memory().unwrap())));
        block_on(init_and_migrate_db(&ctx)).unwrap();
        if with_record_directory {
            std::fs::create_dir_all(my_swaps_dir(&ctx)).unwrap();
        }
        SwapNode { ctx, dbdir }
    }
}

impl Drop for SwapNode {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
}

/// The JSON of a finished swap record of `role` for RIN (maker coin) and KMD (taker coin), started at [`STARTED_AT`].
/// The swap index lists a maker record under `my_coin` RIN and a taker record under `my_coin` KMD.
fn record_json(uuid: Uuid, role: Role) -> Json {
    json::to_value(record(uuid, role, "RIN", "KMD", Outcome::Success, false)).unwrap()
}

/// The `result` of a body, as text-sorted UUIDs of its `uuids` member.
fn sorted_uuids(result: &Json) -> Vec<String> {
    let mut uuids: Vec<String> = result["uuids"]
        .as_array()
        .expect("a list of UUIDs")
        .iter()
        .map(|uuid| uuid.as_str().expect("a UUID text").to_owned())
        .collect();
    uuids.sort();
    uuids
}

fn sorted(uuids: &[Uuid]) -> Vec<String> {
    let mut uuids: Vec<String> = uuids.iter().map(Uuid::to_string).collect();
    uuids.sort();
    uuids
}

/// The body of the dev `swap_rpc::all_swaps_uuids_by_filter`: `{"result": {"uuids", "my_coin", "other_coin",
/// "from_timestamp", "to_timestamp", "found_records"}}`, the members in this order.
fn listing_body(uuids: &[Uuid], filter: [Json; 4]) -> String {
    let [my_coin, other_coin, from_timestamp, to_timestamp] = filter;
    json::to_string(&json!({
        "result": {
            "uuids": uuids,
            "my_coin": my_coin,
            "other_coin": other_coin,
            "from_timestamp": from_timestamp,
            "to_timestamp": to_timestamp,
            "found_records": uuids.len(),
        }
    }))
    .unwrap()
}

// ----- `import_swaps` -----

/// The body of the dev `swap_rpc::import_swaps`: `{"result": {"imported": [<uuids, in request order>], "skipped":
/// {<uuid>: <text>}}}`. The imported swaps are listed in the swap index afterwards.
#[test]
fn import_swaps_answers_the_legacy_body_and_the_versioned_result() {
    let node = SwapNode::new();
    let ctx = &node.ctx;
    let (maker_id, taker_id, third_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    same(
        &legacy_success(
            ctx,
            "import_swaps",
            json!({ "swaps": [record_json(maker_id, Role::Maker), record_json(taker_id, Role::Taker)] }),
        ),
        &json::to_string(&json!({ "result": { "imported": [maker_id, taker_id], "skipped": {} } })).unwrap(),
        "the legacy body",
    );

    // Both swaps are in the swap index, each under the coin of its own side.
    let listed = legacy_success(ctx, "all_swaps_uuids_by_filter", json!({}));
    let listed: Json = json::from_str(&listed).unwrap();
    same(
        &sorted_uuids(&listed["result"]),
        &sorted(&[maker_id, taker_id]),
        "the listing after the import",
    );

    // The versioned form takes the same member inside `params`, ignores an unknown member, and answers the content of
    // the legacy `result`.
    let result = versioned_result(
        versioned(
            ctx,
            "import_swaps",
            Some(json!({ "swaps": [record_json(third_id, Role::Maker)], "unknown_member": true })),
            201,
        ),
        201,
    );
    same(
        &result,
        &json!({ "imported": [third_id], "skipped": {} }),
        "the versioned result",
    );

    // An empty list imports nothing and is not a refusal, on both surfaces.
    same(
        &legacy_success(ctx, "import_swaps", json!({ "swaps": [] })),
        &r#"{"result":{"imported":[],"skipped":{}}}"#.to_owned(),
        "the legacy body of an empty list",
    );
    same(
        &versioned_result(versioned(ctx, "import_swaps", Some(json!({ "swaps": [] })), 202), 202),
        &json!({ "imported": [], "skipped": {} }),
        "the versioned result of an empty list",
    );

    // A record that is imported again is imported again: the record is written over, the failed insertion into the
    // swap index (the UUID is there) is logged, and the answer lists the swap.
    same(
        &legacy_success(
            ctx,
            "import_swaps",
            json!({ "swaps": [record_json(maker_id, Role::Maker)] }),
        ),
        &json::to_string(&json!({ "result": { "imported": [maker_id], "skipped": {} } })).unwrap(),
        "the legacy body of a second import",
    );
    let listed: Json = json::from_str(&legacy_success(ctx, "all_swaps_uuids_by_filter", json!({}))).unwrap();
    same(
        &sorted_uuids(&listed["result"]),
        &sorted(&[maker_id, taker_id, third_id]),
        "the listing after the second import",
    );
}

/// A record that cannot be stored is an entry of `skipped` with the text of its failure, inside a success, on both
/// surfaces: it is not an error of the call (CRD ch.43 R43.1B.3). The text carries the `file:line] ` token of the
/// handler file that built it. The swap is not in the swap index.
#[test]
fn import_swaps_answers_a_record_that_cannot_be_stored_in_skipped() {
    let node = SwapNode::without_record_directory();
    let ctx = &node.ctx;
    let (legacy_id, versioned_id) = (Uuid::new_v4(), Uuid::new_v4());

    let body: Json = json::from_str(&legacy_success(
        ctx,
        "import_swaps",
        json!({ "swaps": [record_json(legacy_id, Role::Maker)] }),
    ))
    .unwrap();
    same(&body["result"]["imported"], &json!([]), "the legacy imported list");
    let skipped = body["result"]["skipped"]
        .as_object()
        .expect("an object of skipped swaps");
    assert_eq!(skipped.len(), 1, "the legacy skipped swaps");
    let text = skipped[&legacy_id.to_string()].as_str().expect("a text");
    assert!(
        split_location(text).0.first().map(String::as_str) == Some("swap_rpc"),
        "the skipped text names the handler file"
    );

    let result = versioned_result(
        versioned(
            ctx,
            "import_swaps",
            Some(json!({ "swaps": [record_json(versioned_id, Role::Taker)] })),
            211,
        ),
        211,
    );
    same(&result["imported"], &json!([]), "the versioned imported list");
    let skipped = result["skipped"].as_object().expect("an object of skipped swaps");
    assert_eq!(skipped.len(), 1, "the versioned skipped swaps");
    assert!(
        skipped[&versioned_id.to_string()].is_string(),
        "the versioned skipped text"
    );

    let listed: Json = json::from_str(&legacy_success(ctx, "all_swaps_uuids_by_filter", json!({}))).unwrap();
    same(
        &listed["result"]["found_records"],
        &json!(0),
        "the swap index stays empty",
    );
}

/// A request without a list of swaps is refused: the legacy handler reads the `swaps` member of the body with the
/// text of the decoder, the versioned dispatcher answers `InvalidRequest` (string, 400) before the core runs.
#[test]
fn import_swaps_refuses_a_request_without_a_list_of_swaps() {
    let node = SwapNode::new();
    let ctx = &node.ctx;

    same(
        &legacy_refusal(ctx, "import_swaps", json!({}), "swap_rpc"),
        &"invalid type: null, expected a sequence".to_owned(),
        "the missing-list text",
    );
    same(
        &legacy_refusal(ctx, "import_swaps", json!({ "swaps": "x" }), "swap_rpc"),
        &r#"invalid type: string "x", expected a sequence"#.to_owned(),
        "the wrong-type text",
    );

    for (params, id) in [
        (None, 221),
        (Some(json!({})), 222),
        (Some(json!({ "swaps": "x" })), 223),
    ] {
        assert_typed_refusal(
            &versioned(ctx, "import_swaps", params, id),
            400,
            "InvalidRequest",
            Some("string"),
            id,
        );
    }
}

// ----- `all_swaps_uuids_by_filter` -----

/// Two swaps in the index (a maker swap RIN/KMD and a taker swap RIN/KMD, both started at [`STARTED_AT`]) are listed
/// by every member of the filter. The legacy body is the one of the dev `swap_rpc::all_swaps_uuids_by_filter`; the
/// versioned `result` is the content of its `result`.
#[test]
fn all_swaps_uuids_by_filter_answers_the_legacy_body_and_the_versioned_result() {
    let node = SwapNode::new();
    let ctx = &node.ctx;
    let (maker_id, taker_id) = (Uuid::new_v4(), Uuid::new_v4());
    same(
        &legacy_success(
            ctx,
            "import_swaps",
            json!({ "swaps": [record_json(maker_id, Role::Maker), record_json(taker_id, Role::Taker)] }),
        ),
        &json::to_string(&json!({ "result": { "imported": [maker_id, taker_id], "skipped": {} } })).unwrap(),
        "the import that fills the index",
    );

    // (filter members, the swaps that the filter lists). The maker swap has `my_coin` RIN, the taker swap KMD.
    let cases: Vec<(Json, [Json; 4], Vec<Uuid>)> = vec![
        (
            json!({ "my_coin": "RIN" }),
            [json!("RIN"), Json::Null, Json::Null, Json::Null],
            vec![maker_id],
        ),
        (
            json!({ "other_coin": "RIN" }),
            [Json::Null, json!("RIN"), Json::Null, Json::Null],
            vec![taker_id],
        ),
        (
            json!({ "my_coin": "RIN", "other_coin": "KMD" }),
            [json!("RIN"), json!("KMD"), Json::Null, Json::Null],
            vec![maker_id],
        ),
        (
            json!({ "my_coin": "BTC" }),
            [json!("BTC"), Json::Null, Json::Null, Json::Null],
            vec![],
        ),
        // `from_timestamp` includes the start time, `to_timestamp` excludes it.
        (
            json!({ "from_timestamp": STARTED_AT + 1 }),
            [Json::Null, Json::Null, json!(STARTED_AT + 1), Json::Null],
            vec![],
        ),
        (
            json!({ "to_timestamp": STARTED_AT }),
            [Json::Null, Json::Null, Json::Null, json!(STARTED_AT)],
            vec![],
        ),
        (
            json!({ "my_coin": "KMD", "from_timestamp": STARTED_AT, "to_timestamp": STARTED_AT + 1 }),
            [json!("KMD"), Json::Null, json!(STARTED_AT), json!(STARTED_AT + 1)],
            vec![taker_id],
        ),
        // The native swap index answers a range that ends before it starts with an empty list.
        (
            json!({ "from_timestamp": 10, "to_timestamp": 5 }),
            [Json::Null, Json::Null, json!(10), json!(5)],
            vec![],
        ),
    ];
    for (index, (params, filter, expected)) in cases.into_iter().enumerate() {
        let id = 230 + index;
        let legacy = legacy_success(ctx, "all_swaps_uuids_by_filter", params.clone());
        same(&legacy, &listing_body(&expected, filter), "the legacy body");
        let legacy: Json = json::from_str(&legacy).unwrap();
        let result = versioned_result(versioned(ctx, "all_swaps_uuids_by_filter", Some(params), id), id);
        same(&result, &legacy["result"], "the versioned result");
    }

    // Without a filter both swaps are listed, in the order of the index (newest first; the two share a start time).
    let legacy: Json = json::from_str(&legacy_success(ctx, "all_swaps_uuids_by_filter", json!({}))).unwrap();
    same(
        &sorted_uuids(&legacy["result"]),
        &sorted(&[maker_id, taker_id]),
        "the unfiltered legacy listing",
    );
    same(
        &legacy["result"]["found_records"],
        &json!(2),
        "the unfiltered legacy count",
    );
    for member in ["my_coin", "other_coin", "from_timestamp", "to_timestamp"] {
        assert!(legacy["result"][member].is_null(), "the unfiltered {member}");
    }
}

/// `params` of the versioned form: absent, `null`, `{}` and an object with an unknown member all list every swap.
#[test]
fn all_swaps_uuids_by_filter_takes_every_form_of_params() {
    let node = SwapNode::new();
    let ctx = &node.ctx;
    let maker_id = Uuid::new_v4();
    legacy_success(
        ctx,
        "import_swaps",
        json!({ "swaps": [record_json(maker_id, Role::Maker)] }),
    );
    for (params, id) in [
        (None, 241),
        (Some(Json::Null), 242),
        (Some(json!({})), 243),
        (Some(json!({ "unknown_member": 1 })), 244),
    ] {
        let result = versioned_result(versioned(ctx, "all_swaps_uuids_by_filter", params, id), id);
        same(&result["uuids"], &json!([maker_id]), "the versioned list");
        same(&result["found_records"], &json!(1), "the versioned count");
    }
}

/// A member of the wrong type is refused: the legacy handler decodes the whole body with the text of the decoder, the
/// versioned dispatcher answers `InvalidRequest` (string, 400).
#[test]
fn all_swaps_uuids_by_filter_refuses_a_malformed_filter() {
    let node = SwapNode::new();
    let ctx = &node.ctx;
    same(
        &legacy_refusal(
            ctx,
            "all_swaps_uuids_by_filter",
            json!({ "from_timestamp": "x" }),
            "swap_rpc",
        ),
        &r#"invalid type: string "x", expected u64"#.to_owned(),
        "the wrong-type text",
    );
    assert_typed_refusal(
        &versioned(
            ctx,
            "all_swaps_uuids_by_filter",
            Some(json!({ "from_timestamp": "x" })),
            251,
        ),
        400,
        "InvalidRequest",
        Some("string"),
        251,
    );
}

/// A failure of the swap index is `DbError` (string, 500), with the text of the storage error; the legacy text is that
/// text behind the `file:line] ` token of the handler.
#[test]
fn all_swaps_uuids_by_filter_answers_a_storage_failure() {
    let node = SwapNode::new();
    let ctx = &node.ctx;
    ctx.sqlite_connection().execute_batch("DROP TABLE my_swaps;").unwrap();

    let text = legacy_refusal(ctx, "all_swaps_uuids_by_filter", json!({}), "swap_rpc");
    assert!(text.starts_with("Unknown SQL error: "), "the storage text");

    let answer = versioned(ctx, "all_swaps_uuids_by_filter", Some(json!({})), 261);
    assert_typed_refusal(&answer, 500, "DbError", Some("string"), 261);
    assert!(
        answer.1["error_data"]
            .as_str()
            .unwrap()
            .starts_with("Unknown SQL error: "),
        "the versioned storage text"
    );
}

// ----- The envelope -----

/// CRD ch.43 R43.1A.2: a legacy-shaped body (parameters inline) with an added `"mmrpc": "2.0"` is undecodable under the
/// versioned envelope and answers `InvalidRequest` (string payload, HTTP 400) before any method lookup, for both
/// methods of the group.
#[test]
fn legacy_shaped_body_with_an_mmrpc_key_answers_invalid_request_for_both_methods() {
    let node = SwapNode::new();
    let cases = [
        ("all_swaps_uuids_by_filter", json!({ "my_coin": "RIN" })),
        ("import_swaps", json!({ "swaps": [] })),
    ];
    for (method, inline) in cases {
        let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": 3 });
        for (key, value) in inline.as_object().unwrap() {
            request[key.as_str()] = value.clone();
        }
        let answer = match run(&node.ctx, request) {
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
