//! Unit tests of the TRON node client (CRD ch.21 §21.9, R-N3 to R-N9), against loopback stub nodes: failover on
//! bad answers (T-N1 to T-N3), the per-attempt cutoff (T-N4), concurrent requests (T-N5), the node order (T-N6),
//! the all-failed error (T-N7), the broadcast rules (T-N8) and the request headers (R-N9).

use super::api::{AccountListing, TronApiClient, TronApiError};
use super::nodes::{parse_nodes, TronApiKey, TronNode};
use super::test_node::{Reply, StubNode};
use super::{tron_coin_from_conf_and_request, Network, TronAddress};
use crate::closed_port::closed_url;
use crate::CoinProtocol;
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CHAIN_PARAMETERS: &str =
    r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420}]}"#;
const ACCOUNT: &str = r#"{"balance":5000000,"create_time":1700000000000}"#;
const TXID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
/// A cutoff short enough for a test; the production value is 10 s.
const TEST_CUTOFF_S: f64 = 0.4;

/// A node that answers every request with `status` and `body`.
fn fixed(status: u16, body: &'static str) -> StubNode { StubNode::start(move |_, _| Reply::status(status, body)) }

/// A node that accepts a request and does not answer it for a long time.
fn silent() -> StubNode {
    StubNode::start(|_, _| {
        std::thread::sleep(Duration::from_secs(4));
        Reply::ok("{}")
    })
}

fn client_over(nodes: &[&StubNode], cutoff_s: f64) -> TronApiClient {
    TronApiClient::new_with_cutoff_s(
        "TRX",
        nodes.iter().map(|n| TronNode::plain(n.url())).collect(),
        cutoff_s,
    )
}

fn chain_parameters(client: &TronApiClient) -> Result<usize, TronApiError> {
    block_on(client.get_chain_parameters()).map(|answer| answer.chain_parameter.len())
}

fn address() -> TronAddress { TronAddress::from_base58("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").expect("a valid address") }

fn labels(nodes: &[&StubNode]) -> Vec<String> { nodes.iter().map(|n| n.url()).collect() }

#[test]
fn t_n1_a_429_with_a_non_json_body_moves_on_to_the_next_node() {
    let (limited, healthy) = (fixed(429, "Too Many Requests"), fixed(200, CHAIN_PARAMETERS));
    let client = client_over(&[&limited, &healthy], 5.);
    assert_eq!(chain_parameters(&client).expect("the second node answers"), 2);
    assert_eq!(limited.requests().len(), 1);
    assert_eq!(healthy.requests().len(), 1);
}

#[test]
fn t_n2_a_429_with_an_error_body_is_never_an_absent_account() {
    let (limited, healthy) = (fixed(429, r#"{"Error":"rate limit"}"#), fixed(200, ACCOUNT));
    let client = client_over(&[&limited, &healthy], 5.);
    let account = block_on(client.get_account(&address()))
        .expect("the second node answers")
        .expect("an existing account");
    assert_eq!(account.balance, 5_000_000);
    assert_eq!(limited.requests().len(), 1);

    // With the second node also limited the request fails; it never reads as "no account".
    let both_limited = client_over(&[&limited, &fixed(429, "{}")], 5.);
    assert!(block_on(both_limited.get_account(&address())).is_err());
}

#[test]
fn t_n3_a_200_with_a_non_json_body_moves_on_to_the_next_node() {
    let (broken, healthy) = (fixed(200, "<html>not json</html>"), fixed(200, CHAIN_PARAMETERS));
    let client = client_over(&[&broken, &healthy], 5.);
    assert_eq!(chain_parameters(&client).expect("the second node answers"), 2);
    // A body of the wrong shape is a bad answer too.
    let (wrong_shape, healthy) = (fixed(200, r#"{"chainParameter":"x"}"#), fixed(200, CHAIN_PARAMETERS));
    let client = client_over(&[&wrong_shape, &healthy], 5.);
    assert_eq!(chain_parameters(&client).expect("the second node answers"), 2);
}

#[test]
fn t_n4_a_node_that_never_answers_costs_the_cutoff_and_then_the_next_node_answers() {
    let (stuck, healthy) = (silent(), fixed(200, CHAIN_PARAMETERS));
    let client = client_over(&[&stuck, &healthy], TEST_CUTOFF_S);
    let started = Instant::now();
    assert_eq!(chain_parameters(&client).expect("the second node answers"), 2);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(350),
        "the request did not wait for the cutoff: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_millis(1500), "the request took {elapsed:?}");
    assert_eq!(client.order_labels(), labels(&[&healthy, &stuck]));
}

#[test]
fn t_n5_a_request_stuck_on_a_slow_node_does_not_hold_up_another_request() {
    // The node answers its first request after 2 s and every later one at once.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let node = StubNode::start(move |_, _| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        Reply::ok(CHAIN_PARAMETERS)
    });
    let client = client_over(&[&node], 10.);
    let (first, (second, second_elapsed)) = block_on(async {
        let first = async {
            let started = Instant::now();
            let result = client.get_chain_parameters().await;
            (result, started.elapsed())
        };
        let second = async {
            common::executor::Timer::sleep(0.4).await;
            let started = Instant::now();
            let result = client.get_chain_parameters().await;
            (result, started.elapsed())
        };
        let ((first, first_elapsed), second) = futures::join!(first, second);
        // The first request really was stuck, and the second finished well before it would have.
        assert!(first_elapsed >= Duration::from_millis(1800), "{first_elapsed:?}");
        (first, second)
    });
    first.expect("the slow request answers");
    second.expect("the second request answers");
    assert!(
        second_elapsed < Duration::from_millis(1000),
        "the second request waited for the first: {second_elapsed:?}"
    );
}

#[test]
fn t_n6_the_answering_node_becomes_first_and_the_order_is_shared_by_clones() {
    let (bad_a, bad_b, good) = (
        fixed(500, "{}"),
        fixed(502, "bad gateway"),
        fixed(200, CHAIN_PARAMETERS),
    );
    let client = client_over(&[&bad_a, &bad_b, &good], 5.);
    // The configured order is kept until a node answers.
    assert_eq!(client.order_labels(), labels(&[&bad_a, &bad_b, &good]));
    chain_parameters(&client).expect("the third node answers");
    // The answering node first, the failed ones at the tail in their relative order.
    assert_eq!(client.order_labels(), labels(&[&good, &bad_a, &bad_b]));

    // A clone shares the order: its next request goes straight to the good node.
    let clone = client.clone();
    assert_eq!(clone.order_labels(), client.order_labels());
    chain_parameters(&clone).expect("the first node answers");
    assert_eq!(good.requests().len(), 2);
    assert_eq!(bad_a.requests().len(), 1);
    assert_eq!(bad_b.requests().len(), 1);

    // A node that answers from the middle of the order: the nodes before it go to the tail.
    let (bad, mid, last) = (
        fixed(500, "{}"),
        fixed(200, CHAIN_PARAMETERS),
        fixed(200, CHAIN_PARAMETERS),
    );
    let client = client_over(&[&bad, &mid, &last], 5.);
    chain_parameters(&client).expect("the second node answers");
    assert_eq!(client.order_labels(), labels(&[&mid, &last, &bad]));
}

#[test]
fn t_n7_all_nodes_failed_names_the_last_non_cutoff_failure_and_no_credential() {
    // Credentials in the userinfo, the path and the query of the configured URLs.
    let (a, b) = (fixed(429, "Too Many Requests"), fixed(200, "junk"));
    let urls = [
        a.url().replacen("http://", "http://user:SECRETPW@", 1) + "/KEYPATH-A?token=QUERYSECRET",
        b.url() + "/KEYPATH-B",
    ];
    let client = TronApiClient::new_with_cutoff_s("TRX", urls.iter().cloned().map(TronNode::plain).collect(), 5.);
    let err = block_on(client.get_chain_parameters()).expect_err("every node fails");
    // The last failure that is not a cutoff is node B's undecodable body.
    assert!(matches!(err, TronApiError::InvalidResponse(_)), "{err:?}");
    let text = format!("{err} {err:?}");
    assert!(text.contains(&a.url()) && text.contains(&b.url()), "{text}");
    assert!(text.contains("429"), "{text}");
    assert_eq!(
        text.matches(&a.url()).count(),
        2,
        "a host is listed once (Display and Debug): {text}"
    );
    for secret in ["SECRETPW", "KEYPATH", "QUERYSECRET", "token="] {
        assert!(!text.contains(secret), "{text} renders a credential part");
    }

    // A transport-class last failure is an all-nodes-failed error.
    let (dead, limited) = (fixed(500, "{}"), fixed(503, "{}"));
    let err = block_on(client_over(&[&dead, &limited], 5.).get_chain_parameters()).expect_err("every node fails");
    assert!(matches!(err, TronApiError::AllNodesFailed(_)), "{err:?}");

    // A cutoff before a non-cutoff failure: the non-cutoff failure is named, the cutoff host is listed too.
    let (stuck, limited) = (silent(), fixed(429, "{}"));
    let err = block_on(client_over(&[&stuck, &limited], TEST_CUTOFF_S).get_chain_parameters()).expect_err("fails");
    let text = err.to_string();
    assert!(matches!(err, TronApiError::AllNodesFailed(_)), "{err:?}");
    assert!(
        text.contains(&stuck.url()) && text.contains("no answer within"),
        "{text}"
    );

    // Every node hit the cutoff: a timeout-class "nodes unreachable" error.
    let (stuck_a, stuck_b) = (silent(), silent());
    let started = Instant::now();
    let err = block_on(client_over(&[&stuck_a, &stuck_b], TEST_CUTOFF_S).get_chain_parameters()).expect_err("fails");
    assert!(matches!(err, TronApiError::Timeout(_)), "{err:?}");
    assert!(err.to_string().contains("unreachable"), "{err}");
    assert!(started.elapsed() < Duration::from_millis(2500));
}

#[test]
fn t_n8_a_broadcast_fails_over_on_a_node_failure_and_stops_on_a_rejection() {
    let accepted = format!(r#"{{"result":true,"txid":"{TXID}"}}"#);
    let accepted: &'static str = Box::leak(accepted.into_boxed_str());

    // A transport failure, then a bad body, then an acceptance.
    let (junk, good) = (fixed(200, "junk"), fixed(200, accepted));
    let client = TronApiClient::new_with_cutoff_s(
        "TRX",
        vec![
            TronNode::plain(closed_url()),
            TronNode::plain(junk.url()),
            TronNode::plain(good.url()),
        ],
        5.,
    );
    let answer = block_on(client.broadcast_hex("0a02abcd")).expect("the third node accepts");
    assert_eq!(answer.txid.as_deref(), Some(TXID));

    // A well-formed rejection ends the request with that node's code and message; no further node is asked.
    let (rejecting, never_asked) = (
        fixed(200, r#"{"result":false,"code":"SIGERROR","message":"6261"}"#),
        fixed(200, accepted),
    );
    let client = client_over(&[&rejecting, &never_asked], 5.);
    match block_on(client.broadcast_hex("0a02abcd")) {
        Err(TronApiError::RemoteError { code, message }) => {
            assert_eq!(code, "SIGERROR");
            assert_eq!(message, "6261");
        },
        other => panic!("expected the node's rejection, got {other:?}"),
    }
    assert_eq!(rejecting.requests().len(), 1);
    assert!(
        never_asked.requests().is_empty(),
        "a further node was asked after a rejection"
    );
}

/// The header `name` of every request `node` received.
fn seen(node: &StubNode, name: &str) -> Vec<Option<String>> { node.header_values(name) }

#[test]
fn headers_of_a_node_entry_reach_that_node_only() {
    let (first, second) = (fixed(500, "{}"), fixed(200, CHAIN_PARAMETERS));
    let urls = json!([
        {"url": first.url(), "headers": {"X-Provider-Key": "FIRST-VALUE"}},
        {"url": second.url(), "headers": {"X-Other-Key": "SECOND-VALUE"}},
    ]);
    let client = TronApiClient::new_with_cutoff_s("TRX", parse_nodes(&urls, None).expect("nodes"), 5.);
    chain_parameters(&client).expect("the second node answers");
    assert_eq!(seen(&first, "x-provider-key"), vec![Some("FIRST-VALUE".to_owned())]);
    assert_eq!(seen(&first, "x-other-key"), vec![None]);
    assert_eq!(seen(&second, "x-other-key"), vec![Some("SECOND-VALUE".to_owned())]);
    assert_eq!(seen(&second, "x-provider-key"), vec![None]);
    // The body is still sent as JSON.
    assert_eq!(seen(&first, "content-type"), vec![Some("application/json".to_owned())]);
}

#[test]
fn the_api_key_is_not_sent_to_a_host_that_is_not_trongrid() {
    // The stubs listen on the loopback, which is not a TronGrid host (the host rule itself is tested in `nodes`).
    let node = fixed(200, CHAIN_PARAMETERS);
    let key = TronApiKey::from_conf(&json!({"tron_api_key": "KEY-SECRET"}))
        .expect("valid")
        .expect("set");
    let client =
        TronApiClient::new_with_cutoff_s("TRX", parse_nodes(&json!([node.url()]), Some(&key)).expect("nodes"), 5.);
    chain_parameters(&client).expect("answers");
    assert_eq!(seen(&node, "tron-pro-api-key"), vec![None]);
}

/// The query string of a node URL (a key in the query) stays at the end of the request URL, after the endpoint path
/// (CRD ch.21 R-N9).
#[test]
fn a_query_string_key_of_a_node_url_stays_at_the_end_of_the_request_url() {
    let node = StubNode::start(|path, _| {
        if path == "/x/wallet/getchainparameters?key=1" {
            Reply::ok(CHAIN_PARAMETERS)
        } else {
            Reply::status(404, "{}")
        }
    });
    let nodes = parse_nodes(&json!([format!("{}/x?key=1", node.url())]), None).expect("nodes");
    let client = TronApiClient::new_with_cutoff_s("TRX", nodes, 5.);
    chain_parameters(&client).expect("the node answers the request with the query kept");
    assert_eq!(node.requests()[0].0, "/x/wallet/getchainparameters?key=1");
}

fn activate(conf: serde_json::Value, urls: serde_json::Value) -> Result<(), String> {
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let mut key = [0u8; 32];
    key[31] = 1;
    block_on(tron_coin_from_conf_and_request(
        &ctx,
        "TRX",
        &json!({}),
        &json!({ "urls": urls }),
        &key,
        CoinProtocol::TRX { network: Network::Nile },
    ))
    .map(|_| ())
}

#[test]
fn activation_accepts_both_entry_forms_and_refuses_bad_headers_without_echoing_them() {
    let node = fixed(200, CHAIN_PARAMETERS);
    activate(
        json!({"tron_api_key": "KEY-SECRET"}),
        json!([node.url(), {"url": node.url(), "headers": {"X-A": "v"}}]),
    )
    .expect("both entry forms activate");

    for (conf, urls) in [
        (json!({}), json!([{"url": node.url(), "headers": {"bad name": "v"}}])),
        (
            json!({}),
            json!([{"url": node.url(), "headers": {"X-A": "SECRET\nVALUE"}}]),
        ),
        (json!({}), json!([{"url": node.url(), "headers": {"X-A": 7}}])),
        (json!({"tron_api_key": "KEY\nSECRET"}), json!([node.url()])),
        (json!({"tron_api_key": 12}), json!([node.url()])),
    ] {
        let err = activate(conf, urls).expect_err("refused");
        for secret in ["SECRET", "VALUE", "bad name", "KEY\n"] {
            assert!(!err.contains(secret), "{err} echoes a configured string");
        }
    }
}

#[test]
fn the_client_debug_form_renders_no_node_and_no_header() {
    let urls = json!([{"url": "https://user:URLSECRET@node.example/KEYPATH", "headers": {"X-A": "HEADERSECRET"}}]);
    let client = TronApiClient::new("TRX", parse_nodes(&urls, None).expect("nodes"));
    let text = format!("{client:?}");
    for secret in ["URLSECRET", "KEYPATH", "HEADERSECRET", "node.example"] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[test]
fn a_wrongly_shaped_getaccount_answer_fails_over_and_is_never_an_absent_account() {
    let (proxy, healthy) = (fixed(200, r#"{"message":"rate limited"}"#), fixed(200, ACCOUNT));
    let client = client_over(&[&proxy, &healthy], 5.);
    let account = block_on(client.get_account(&address()))
        .expect("the second node answers")
        .expect("an existing account");
    assert_eq!(account.balance, 5_000_000);
    assert_eq!(proxy.requests().len(), 1);

    // Alone, the wrongly shaped answer is an invalid-response failure, never `Ok(None)`.
    let alone = client_over(&[&proxy], 5.);
    let err = block_on(alone.get_account(&address())).expect_err("not an answer");
    assert!(matches!(err, TronApiError::InvalidResponse(_)), "{err:?}");

    // Only a 2xx empty object is an absent account.
    let empty = fixed(200, "{}");
    let absent = client_over(&[&empty], 5.);
    assert!(block_on(absent.get_account(&address())).expect("answers").is_none());
}

#[test]
fn an_answer_that_is_not_an_answer_to_the_endpoint_fails_over_on_every_checked_endpoint() {
    let proxy = || fixed(200, r#"{"message":"rate limited"}"#);

    // getnowblock: an unusable block id.
    let bad_block = fixed(
        200,
        r#"{"blockID":"zz","block_header":{"raw_data":{"number":1,"timestamp":1}}}"#,
    );
    let id = "0000000003456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let good_block: &'static str = Box::leak(
        format!(r#"{{"blockID":"{id}","block_header":{{"raw_data":{{"number":1,"timestamp":1}}}}}}"#).into_boxed_str(),
    );
    let healthy = fixed(200, good_block);
    let client = client_over(&[&bad_block, &healthy], 5.);
    block_on(client.get_now_block()).expect("the second node answers");
    assert_eq!(bad_block.requests().len(), 1);

    // triggerconstantcontract: a body without a result fails over; a node's own failure object is an answer.
    let (owner, contract) = (address(), address());
    let healthy = fixed(200, r#"{"constant_result":["00"],"energy_used":1}"#);
    let (proxy_node, bare) = (proxy(), fixed(200, "{}"));
    let client = client_over(&[&proxy_node, &bare, &healthy], 5.);
    block_on(client.trigger_constant_contract(&owner, &contract, "f()", "")).expect("the third node answers");
    let (failing, never_asked) = (
        fixed(200, r#"{"result":{"code":"CONTRACT_VALIDATE_ERROR","message":"6e6f"}}"#),
        fixed(200, r#"{"constant_result":["00"]}"#),
    );
    let client = client_over(&[&failing, &never_asked], 5.);
    block_on(client.trigger_constant_contract(&owner, &contract, "f()", "")).expect("an answer");
    assert!(never_asked.requests().is_empty());

    // broadcasthex: a body without a result flag fails over.
    let accepted: &'static str = Box::leak(format!(r#"{{"result":true,"txid":"{TXID}"}}"#).into_boxed_str());
    let (proxy_node, healthy) = (proxy(), fixed(200, accepted));
    let client = client_over(&[&proxy_node, &healthy], 5.);
    let answer = block_on(client.broadcast_hex("0a02abcd")).expect("the second node accepts");
    assert_eq!(answer.txid.as_deref(), Some(TXID));
}

#[test]
fn the_built_order_is_a_permutation_of_the_configured_list() {
    let nodes: Vec<StubNode> = (0..8).map(|_| fixed(200, CHAIN_PARAMETERS)).collect();
    let configured: Vec<String> = nodes.iter().map(StubNode::url).collect();
    let client = TronApiClient::new("TRX", configured.iter().cloned().map(TronNode::plain).collect());
    let mut built = client.order_labels();
    assert_eq!(built.len(), configured.len());
    built.sort();
    let mut expected = configured;
    expected.sort();
    assert_eq!(built, expected, "the order is not a permutation of the configured list");
}

#[test]
fn a_transient_code_moves_on_and_any_other_code_is_final() {
    let accepted: &'static str = Box::leak(format!(r#"{{"result":true,"txid":"{TXID}"}}"#).into_boxed_str());
    for code in [
        "SERVER_BUSY",
        "NO_CONNECTION",
        "NOT_ENOUGH_EFFECTIVE_CONNECTION",
        "BLOCK_UNSOLIDIFIED",
    ] {
        let busy_body: &'static str =
            Box::leak(format!(r#"{{"result":false,"code":"{code}","message":"6e6f"}}"#).into_boxed_str());
        let (busy, healthy) = (fixed(200, busy_body), fixed(200, accepted));
        let client = client_over(&[&busy, &healthy], 5.);
        let answer = block_on(client.broadcast_hex("0a02abcd")).expect("the second node accepts");
        assert_eq!(answer.txid.as_deref(), Some(TXID), "{code}");
    }

    // Any other code ends the request at that node.
    let (rejecting, never_asked) = (
        fixed(
            200,
            r#"{"result":false,"code":"CONTRACT_VALIDATE_ERROR","message":"6e6f"}"#,
        ),
        fixed(200, accepted),
    );
    let client = client_over(&[&rejecting, &never_asked], 5.);
    match block_on(client.broadcast_hex("0a02abcd")) {
        Err(TronApiError::RemoteError { code, .. }) => assert_eq!(code, "CONTRACT_VALIDATE_ERROR"),
        other => panic!("expected the node's rejection, got {other:?}"),
    }
    assert!(never_asked.requests().is_empty());

    // Other endpoints and error-object forms: a `result` object and an `error` object carrying a transient code.
    for body in [
        r#"{"result":{"code":"BLOCK_UNSOLIDIFIED","message":"x"}}"#,
        r#"{"error":{"code":"NO_CONNECTION","message":"x"}}"#,
    ] {
        let (busy, healthy) = (fixed(200, body), fixed(200, CHAIN_PARAMETERS));
        let client = client_over(&[&busy, &healthy], 5.);
        assert_eq!(chain_parameters(&client).expect("the second node answers"), 2);
        assert_eq!(busy.requests().len(), 1);
    }
    let (busy, healthy) = (
        fixed(200, r#"{"result":{"code":"SERVER_BUSY","message":"x"}}"#),
        fixed(200, r#"{"constant_result":["00"]}"#),
    );
    let client = client_over(&[&busy, &healthy], 5.);
    block_on(client.trigger_constant_contract(&address(), &address(), "f()", "")).expect("the second node answers");
}

/// CRD ch.21 R21.10B.13 and its finding: a dry run that reverts ends the request at the node that answered it. The
/// answer forms are those a public mainnet node gave on 2026-11-20.
#[test]
fn a_reverting_dry_run_is_final_and_asks_no_other_node() {
    let word = "0000000000000000000000000000000000000000000000000000000000000001";
    let succeeded: &'static str = Box::leak(
        format!(
            r#"{{"result":{{"result":true}},"energy_used":64285,"energy_penalty":49635,"constant_result":["{word}"],"logs":[{{"data":"{word}"}}],"transaction":{{"ret":[{{}}]}}}}"#
        )
        .into_boxed_str(),
    );
    let reverted = r#"{"result":{"result":true,"message":"REVERT opcode executed"},"energy_used":8624,"energy_penalty":6640,"constant_result":[""],"transaction":{"ret":[{"ret":"FAILED"}]}}"#;
    let dry_run = |client: &TronApiClient| {
        block_on(client.trigger_constant_dry_run(&address(), &address(), "transfer(address,uint256)", ""))
    };

    let (reverting, never_asked) = (fixed(200, reverted), fixed(200, succeeded));
    let client = client_over(&[&reverting, &never_asked], 5.);
    match dry_run(&client) {
        Err(TronApiError::ContractRevert(reason)) => assert_eq!(reason, "REVERT opcode executed"),
        other => panic!("expected the contract's revert, got {other:?}"),
    }
    assert_eq!(reverting.requests().len(), 1);
    assert!(never_asked.requests().is_empty());
    // The node order is unchanged: the reverting node answered, it did not fail.
    assert_eq!(client.order_labels(), labels(&[&reverting, &never_asked]));

    // A node failure before it still moves on, and the revert of the next node is final.
    let (down, reverting, never_asked) = (fixed(500, "down"), fixed(200, reverted), fixed(200, succeeded));
    let client = client_over(&[&down, &reverting, &never_asked], 5.);
    assert!(matches!(dry_run(&client), Err(TronApiError::ContractRevert(_))));
    assert!(never_asked.requests().is_empty());

    // A successful dry run is an answer with its energy figure.
    let healthy = fixed(200, succeeded);
    let client = client_over(&[&healthy], 5.);
    assert_eq!(dry_run(&client).expect("an answer").energy_used, Some(64_285));
}

#[test]
fn endpoint_checks_move_on_to_the_next_node() {
    let id = "0000000003456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let block = |number: &str, timestamp: i64| -> &'static str {
        Box::leak(
            format!(
                r#"{{"blockID":"{id}","block_header":{{"raw_data":{{"number":{number},"timestamp":{timestamp}}}}}}}"#
            )
            .into_boxed_str(),
        )
    };
    for bad in [block("-1", 1), block("1", 0), block("1", -5)] {
        let (bad_node, healthy) = (fixed(200, bad), fixed(200, block("1", 1)));
        let client = client_over(&[&bad_node, &healthy], 5.);
        block_on(client.get_now_block()).expect("the second node answers");
        assert_eq!(bad_node.requests().len(), 1, "{bad}");
    }

    for bad in [
        r#"{"chainParameter":[{"key":"getTransactionFee","value":0},{"key":"getEnergyFee","value":420}]}"#,
        r#"{"chainParameter":[{"key":"getTransactionFee","value":1000}]}"#,
        r#"{"chainParameter":[]}"#,
    ] {
        let bad_node = fixed(200, bad);
        let full = fixed(
            200,
            r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420}]}"#,
        );
        let client = client_over(&[&bad_node, &full], 5.);
        assert_eq!(chain_parameters(&client).expect("the second node answers"), 2, "{bad}");
        assert_eq!(bad_node.requests().len(), 1, "{bad}");
    }

    for bad in [r#"{"constant_result":[""]}"#, r#"{"constant_result":["zz"]}"#] {
        let good_word = format!(r#"{{"constant_result":["{:064x}"]}}"#, 7);
        let (bad_node, healthy) = (fixed(200, bad), StubNode::start(move |_, _| Reply::ok(&good_word)));
        let client = client_over(&[&bad_node, &healthy], 5.);
        let answer = block_on(client.trigger_constant_read(&address(), &address(), "balanceOf(address)", ""))
            .expect("the second node answers");
        assert_eq!(answer.constant_result.map(|r| r.len()), Some(1));
        assert_eq!(bad_node.requests().len(), 1, "{bad}");
    }
}

#[test]
fn a_read_without_a_constant_result_moves_on_to_the_next_node() {
    for bad in [r#"{"result":{"result":true}}"#, r#"{"constant_result":[]}"#] {
        let good_word = format!(r#"{{"constant_result":["{:064x}"]}}"#, 7);
        let (bad_node, healthy) = (fixed(200, bad), StubNode::start(move |_, _| Reply::ok(&good_word)));
        let client = client_over(&[&bad_node, &healthy], 5.);
        let answer = block_on(client.trigger_constant_read(&address(), &address(), "balanceOf(address)", ""))
            .expect("the second node answers");
        assert_eq!(answer.constant_result.map(|r| r.len()), Some(1));
        assert_eq!(bad_node.requests().len(), 1, "{bad}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The account listings: GET requests of the keyed TronGrid nodes only (CRD ch.21 §21.10C)
// ---------------------------------------------------------------------------------------------------------------

const LISTING: &str = r#"{"data":[],"success":true,"meta":{"at":1,"page_size":0}}"#;

/// A client over `nodes` in which `history[i]` says whether node `i` counts as a keyed TronGrid node. A loopback
/// stub can never be one by the production rule, which `nodes.rs` tests on its own.
fn history_client_over(nodes: &[&StubNode], history: &[bool], cutoff_s: f64) -> TronApiClient {
    TronApiClient::new_with_history_nodes(
        "TRX",
        nodes.iter().map(|n| TronNode::plain(n.url())).collect(),
        history,
        cutoff_s,
    )
}

fn listing(client: &TronApiClient) -> Result<usize, TronApiError> {
    block_on(client.get_account_listing(&address(), AccountListing::Transactions, None)).map(|page| page.data.len())
}

/// The GET walk fails over like the POST walk (R-N4): a status outside 2xx, a body that is not JSON, a body of the
/// wrong shape, a listing not marked a success and a transient code each move on, and the answering node becomes
/// the first node of the order (R-N5).
#[test]
fn a_listing_fails_over_on_every_bad_answer_and_the_answering_node_becomes_first() {
    for (status, body) in [
        (429, r#"{"success":false,"error":"rate limit","statusCode":429}"#),
        (403, r#"{"success":false,"error":"forbidden","statusCode":403}"#),
        (503, r#"{"success":false}"#),
        (301, ""),
        (200, "<html>not json</html>"),
        (200, r#"{"success":true}"#),
        (200, r#"{"success":false,"data":[],"meta":{}}"#),
        (200, r#"{"Error":"class java.lang.NullPointerException"}"#),
        (200, r#"{"result":false,"code":"SERVER_BUSY","message":"busy"}"#),
    ] {
        let (bad, good) = (fixed(status, body), fixed(200, LISTING));
        let client = history_client_over(&[&bad, &good], &[true, true], 5.);
        assert_eq!(listing(&client).expect("the second node answers"), 0, "{status} {body}");
        assert_eq!(bad.requests().len(), 1, "{status} {body}");
        assert_eq!(client.order_labels(), labels(&[&good, &bad]), "{status} {body}");
        // The next request starts at the node that answered.
        listing(&client).expect("answers");
        assert_eq!(bad.requests().len(), 1, "{status} {body}");
    }
}

/// The cutoff of R-N3 bounds a GET attempt too: a silent node costs the cutoff and the next node answers; when
/// every node is silent the error is the timeout class.
#[test]
fn a_listing_attempt_is_bounded_by_the_cutoff() {
    let (slow, good) = (silent(), fixed(200, LISTING));
    let client = history_client_over(&[&slow, &good], &[true, true], TEST_CUTOFF_S);
    let started = Instant::now();
    listing(&client).expect("the second node answers");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs_f64(TEST_CUTOFF_S),
        "the cutoff was not waited"
    );
    assert!(elapsed < Duration::from_secs(3), "the silent node was not abandoned");

    let (first, second) = (silent(), silent());
    let client = history_client_over(&[&first, &second], &[true, true], TEST_CUTOFF_S);
    assert!(matches!(listing(&client), Err(TronApiError::Timeout(_))));
}

/// Only the keyed TronGrid nodes are asked for a listing: a node that is not one gets no request, even when it is
/// first in the order and even when the keyed node fails.
#[test]
fn a_listing_is_asked_of_keyed_trongrid_nodes_only() {
    let (plain, keyed) = (fixed(200, LISTING), fixed(200, LISTING));
    let client = history_client_over(&[&plain, &keyed], &[false, true], 5.);
    assert!(client.has_history_nodes());
    listing(&client).expect("the keyed node answers");
    assert!(
        plain.requests().is_empty(),
        "a node without a key was asked for a listing"
    );
    assert_eq!(keyed.requests().len(), 1);
    // The other requests still go to every node, in the order: a POST reaches the node without a key.
    let (first, second) = (fixed(200, CHAIN_PARAMETERS), fixed(200, CHAIN_PARAMETERS));
    let client = history_client_over(&[&first, &second], &[false, true], 5.);
    chain_parameters(&client).expect("answers");
    assert_eq!(first.requests().len(), 1);
    assert!(second.requests().is_empty());

    let (plain, failing) = (fixed(200, LISTING), fixed(500, "{}"));
    let client = history_client_over(&[&plain, &failing], &[false, true], 5.);
    assert!(listing(&client).is_err());
    assert!(
        plain.requests().is_empty(),
        "a node without a key was asked after the keyed node failed"
    );
}

/// Without a keyed TronGrid node no request is made at all.
#[test]
fn a_listing_without_a_keyed_node_makes_no_request() {
    let node = fixed(200, LISTING);
    let client = client_over(&[&node], 5.);
    assert!(!client.has_history_nodes());
    assert!(matches!(listing(&client), Err(TronApiError::AllNodesFailed(_))));
    assert!(node.requests().is_empty());
}

/// The request of each listing: the documented path and parameters, confirmed rows only, the token filter and the
/// cursor; the node's own headers travel with it and the query of the node URL stays at the end.
#[test]
fn a_listing_request_has_the_documented_path_and_parameters() {
    let node = fixed(200, LISTING);
    let urls = json!([{ "url": format!("{}/x?key=1", node.url()), "headers": { "TRON-PRO-API-KEY": "KEY-VALUE" } }]);
    let client = TronApiClient::new_with_history_nodes("TRX", parse_nodes(&urls, None).expect("nodes"), &[true], 5.);
    let owner = address();
    let base58 = owner.to_base58();

    block_on(client.get_account_listing(&owner, AccountListing::Transactions, None)).expect("answers");
    block_on(client.get_account_listing(&owner, AccountListing::Trc20 { contract: None }, None)).expect("answers");
    block_on(client.get_account_listing(
        &owner,
        AccountListing::Trc20 { contract: Some(&owner) },
        Some("Cursor_1-x"),
    ))
    .expect("answers");

    let asked: Vec<String> = node.requests().into_iter().map(|(path, _)| path).collect();
    assert_eq!(asked, [
        format!("/x/v1/accounts/{base58}/transactions?only_confirmed=true&limit=200&key=1"),
        format!("/x/v1/accounts/{base58}/transactions/trc20?only_confirmed=true&limit=200&key=1"),
        format!(
            "/x/v1/accounts/{base58}/transactions/trc20?only_confirmed=true&limit=200&contract_address={base58}&\
             fingerprint=Cursor_1-x&key=1"
        ),
    ]);
    assert_eq!(seen(&node, "tron-pro-api-key"), vec![Some("KEY-VALUE".to_owned()); 3]);

    // A cursor that could change the request URL is refused before any request.
    let before = node.requests().len();
    for cursor in ["a&b=c", "a b", "a/b", "a?b", ""] {
        let refused = block_on(client.get_account_listing(&owner, AccountListing::Transactions, Some(cursor)));
        assert!(matches!(refused, Err(TronApiError::InvalidResponse(_))), "{cursor}");
    }
    assert_eq!(node.requests().len(), before);
}

/// No error of a listing renders a part of a node URL beyond its host or a header value (R-N1, R-N8, R-N9): for a
/// node that answers badly and for a node that cannot be reached.
#[test]
fn a_failed_listing_names_no_key() {
    let failing = fixed(500, r#"{"success":false,"error":"down"}"#);
    let urls = json!([
        { "url": format!("{}/APIKEY?token=QUERYKEY", failing.url()), "headers": { "TRON-PRO-API-KEY": "HEADERKEY" } },
        { "url": "http://user:secret@127.0.0.1:1/OTHERKEY", "headers": { "TRON-PRO-API-KEY": "SECONDKEY" } },
    ]);
    let client =
        TronApiClient::new_with_history_nodes("TRX", parse_nodes(&urls, None).expect("nodes"), &[true, true], 5.);
    let error = listing(&client).expect_err("every node fails");
    let texts = [error.to_string(), format!("{error:?}"), format!("{client:?}")];
    for text in &texts {
        for secret in [
            "APIKEY",
            "QUERYKEY",
            "HEADERKEY",
            "user",
            "secret",
            "OTHERKEY",
            "SECONDKEY",
        ] {
            assert!(!text.contains(secret), "a credential part is rendered");
        }
    }
    // The key reached the node it belongs to.
    assert_eq!(seen(&failing, "tron-pro-api-key"), vec![Some("HEADERKEY".to_owned())]);
}

/// The receipt read of the history: a node that does not know the transaction (the empty object) is a bad answer,
/// so the next node is asked.
#[test]
fn a_receipt_the_node_does_not_know_moves_on_to_the_next_node() {
    let (unknowing, knowing) = (
        fixed(200, "{}"),
        fixed(200, r#"{"id":"ab","blockNumber":7,"receipt":{"net_usage":267}}"#),
    );
    let client = client_over(&[&unknowing, &knowing], 5.);
    let receipt = block_on(client.get_confirmed_transaction_info(TXID)).expect("the second node answers");
    assert_eq!(receipt.block_number, Some(7));
    assert_eq!(receipt.receipt.and_then(|inner| inner.net_usage), Some(267));

    let client = client_over(&[&unknowing], 5.);
    assert!(block_on(client.get_confirmed_transaction_info(TXID)).is_err());
}
