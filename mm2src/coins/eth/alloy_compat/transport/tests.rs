//! Unit tests of the multi-node access contract (CRD ch.35 §35.1.F): T35.24 -- T35.27, T35.29, T35.30 and the
//! batch half of T35.33, at the level of the transport with stub nodes.

use super::test_support::*;
use super::*;
use crate::eth::alloy_compat::provider::provider_from_transport;
use crate::eth::alloy_compat::KdfProvider;
use alloy::providers::Provider;
use common::block_on;
use serde_json::Value as Json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const A: &str = "http://a.test";
const B: &str = "http://b.test";
const C: &str = "http://c.test";
const D: &str = "http://d.test";
/// Short enough for a test, long enough that a stub that answers at once is never cut off.
const CUTOFF: f64 = 0.2;

fn eth_call(provider: &KdfProvider) -> Result<String, TransportError> {
    block_on(provider.client().request::<_, String>("eth_call", ("0xdead",)))
}

fn host_of(url: &str) -> String { url.trim_start_matches("http://").to_owned() }

/// Every node named in `failing` answers with `fail`, the others with the result `"0x01"`.
fn failing_nodes(failing: &'static [&'static str], fail: fn(&Call) -> Reply) -> impl Fn(&Call) -> Reply {
    move |call| {
        if failing.iter().any(|f| host_of(f) == call.host) {
            fail(call)
        } else {
            Reply::Body(result_body(&call.request, r#""0x01""#))
        }
    }
}

#[test]
fn t35_24_failover_on_a_json_rpc_error_body() {
    for code in [-32000, 3] {
        let (provider, sender) = stub_provider(&[A, B], CUTOFF, move |call| {
            if call.host == "a.test" {
                Reply::Body(error_body(&call.request, code, "execution reverted"))
            } else {
                Reply::Body(result_body(&call.request, r#""0xabc""#))
            }
        });
        assert_eq!(eth_call(&provider).unwrap(), "0xabc", "code {code}");
        assert_eq!(sender.hosts(), ["a.test", "b.test"], "code {code}");
        // The answering node is first for the next request.
        sender.clear();
        assert_eq!(eth_call(&provider).unwrap(), "0xabc");
        assert_eq!(sender.hosts(), ["b.test"], "code {code}");
    }
}

#[test]
fn t35_24_every_other_failure_of_the_first_node_advances_to_the_second() {
    let failures: [(&str, fn(&Call) -> Reply); 5] = [
        ("HTTP 502", |_| Reply::Fail("HTTP 502 Bad Gateway".to_owned())),
        ("connection refused", |_| {
            Reply::Fail("Transport error: connection refused".to_owned())
        }),
        ("undecodable body", |_| Reply::Body("<html>oops</html>".to_owned())),
        ("batch envelope", |call| {
            Reply::Body(format!(
                r#"[{{"jsonrpc":"2.0","id":{},"result":"0xbad"}}]"#,
                call.request["id"]
            ))
        }),
        ("timeout", |_| Reply::Hang),
    ];
    for (name, fail) in failures {
        let (provider, sender) = stub_provider(&[A, B], CUTOFF, failing_nodes(&[A], fail));
        assert_eq!(eth_call(&provider).unwrap(), "0x01", "{name}");
        assert_eq!(sender.hosts(), ["a.test", "b.test"], "{name}");
    }
}

#[test]
fn a_success_passes_through_byte_for_byte() {
    // Odd spacing and an unusual number format survive: the result is handed on as the node sent it.
    let raw = r#"{ "a" : [1,  2.50 ,   "x"] }"#;
    let (transport, _) = stub_transport(&[A], CUTOFF, move |call| Reply::Body(result_body(&call.request, raw)));
    let provider = provider_from_transport(transport);
    let got: Box<serde_json::value::RawValue> = block_on(provider.client().request("eth_call", ("0xdead",))).unwrap();
    assert_eq!(got.get(), raw);
}

#[test]
fn event_handlers_fire_for_every_attempt() {
    #[derive(Default)]
    struct Counting {
        outgoing: AtomicUsize,
        incoming: AtomicUsize,
    }
    impl RpcTransportEventHandler for Counting {
        fn debug_info(&self) -> String { "counting".to_owned() }

        fn on_outgoing_request(&self, _: &[u8]) { self.outgoing.fetch_add(1, Ordering::SeqCst); }

        fn on_incoming_response(&self, _: &[u8]) { self.incoming.fetch_add(1, Ordering::SeqCst); }

        fn on_connected(&self, _: String) -> Result<(), String> { Ok(()) }
    }
    let counting = Arc::new(Counting::default());
    let shared: RpcTransportEventHandlerShared = counting.clone();
    // Node A answers a JSON-RPC error, node B fails at HTTP level, node C answers.
    let (transport, _) =
        stub_transport_with_handlers(&[A, B, C], CUTOFF, vec![shared], |call| match call.host.as_str() {
            "a.test" => Reply::Body(error_body(&call.request, 3, "revert")),
            "b.test" => Reply::Fail("HTTP 502".to_owned()),
            _ => Reply::Body(result_body(&call.request, r#""0x01""#)),
        });
    let provider = provider_from_transport(transport);
    eth_call(&provider).unwrap();
    assert_eq!(counting.outgoing.load(Ordering::SeqCst), 3, "one per attempt");
    assert_eq!(counting.incoming.load(Ordering::SeqCst), 2, "one per 2xx answer");
}

#[test]
fn t35_25_all_nodes_revert_the_third_nodes_error_names_it() {
    let (provider, sender) = stub_provider(&[A, B, C], CUTOFF, |call| {
        let message = format!("revert from {}", call.host);
        let code = match call.host.as_str() {
            "a.test" => -32000,
            "b.test" => 3,
            _ => 5,
        };
        Reply::Body(error_body(&call.request, code, &message))
    });
    let err = eth_call(&provider).unwrap_err();
    assert_eq!(sender.hosts(), ["a.test", "b.test", "c.test"]);
    // The error-response class, not the transport class: the coin layer sees a node's own answer.
    assert!(err.is_error_resp() && !err.is_transport_error(), "{err}");
    let TransportError::ErrorResp(payload) = &err else {
        panic!("expected an error response, got {err}")
    };
    assert_eq!(payload.code, 5);
    assert_eq!(payload.message, "revert from c.test (node http://c.test)");
}

#[test]
fn t35_25_two_reverts_and_a_cutoff_answer_with_the_second_nodes_error() {
    let (provider, _) = stub_provider(&[A, B, C], CUTOFF, |call| match call.host.as_str() {
        "c.test" => Reply::Hang,
        host => Reply::Body(error_body(&call.request, 3, &format!("revert from {host}"))),
    });
    let err = eth_call(&provider).unwrap_err();
    let TransportError::ErrorResp(payload) = &err else {
        panic!("expected an error response, got {err}")
    };
    assert_eq!(payload.message, "revert from b.test (node http://b.test)");
}

#[test]
fn t35_25_every_node_at_the_cutoff_is_a_generic_error_naming_no_node() {
    let (provider, sender) = stub_provider(&[A, B, C], 0.05, |_| Reply::Hang);
    let err = eth_call(&provider).unwrap_err();
    assert_eq!(sender.hosts().len(), 3);
    assert!(err.is_transport_error(), "{err}");
    let text = err.to_string();
    assert!(text.contains("unreachable"), "{text}");
    for host in ["a.test", "b.test", "c.test"] {
        assert!(!text.contains(host), "{text}");
    }
}

#[test]
fn t35_26_stickiness_and_a_fresh_instance_does_not_inherit_the_order() {
    let all_fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = all_fail.clone();
    let (transport, sender) = stub_transport(&[A, B, C, D], CUTOFF, move |call| {
        let failing = flag.load(Ordering::SeqCst) || matches!(call.host.as_str(), "a.test" | "b.test");
        if failing {
            Reply::Fail("HTTP 502".to_owned())
        } else {
            Reply::Body(result_body(&call.request, r#""0x01""#))
        }
    });
    let provider = provider_from_transport(transport.clone());
    eth_call(&provider).unwrap();
    assert_eq!(sender.hosts(), ["a.test", "b.test", "c.test"]);
    sender.clear();
    // The next request starts at C, then D, then A, then B.
    all_fail.store(true, Ordering::SeqCst);
    eth_call(&provider).unwrap_err();
    assert_eq!(sender.hosts(), ["c.test", "d.test", "a.test", "b.test"]);
    // A transport built for the same nodes (a re-activation) starts from its own, configured order.
    let (fresh, _) = stub_transport(&[A, B, C, D], CUTOFF, |_| Reply::Fail("HTTP 502".to_owned()));
    assert_eq!(fresh.order_labels(), [A, B, C, D]);
}

#[test]
fn t35_26_a_copy_starts_from_the_current_order_and_then_evolves_alone() {
    let (transport, _) = stub_transport(&[A, B, C], CUTOFF, |call| match call.host.as_str() {
        "a.test" => Reply::Fail("HTTP 502".to_owned()),
        _ => Reply::Body(result_body(&call.request, r#""0x01""#)),
    });
    let platform = provider_from_transport(transport.clone());
    eth_call(&platform).unwrap();
    assert_eq!(transport.order_labels(), [B, C, A]);

    let copy = transport.with_own_order();
    assert_eq!(
        copy.order_labels(),
        [B, C, A],
        "starts from the platform's then-current order"
    );
    let token = provider_from_transport(copy.clone());
    // A request on the copy that has to skip B moves C to the front of the copy only.
    let (skipping, _) = stub_transport(&[B, C, A], CUTOFF, |call| match call.host.as_str() {
        "b.test" => Reply::Fail("HTTP 502".to_owned()),
        _ => Reply::Body(result_body(&call.request, r#""0x01""#)),
    });
    let own = skipping.with_own_order();
    eth_call(&provider_from_transport(own.clone())).unwrap();
    assert_eq!(own.order_labels(), [C, A, B]);
    assert_eq!(skipping.order_labels(), [B, C, A], "the original order is untouched");
    drop(token);
}

#[test]
fn a_shared_provider_shares_the_order() {
    let (transport, _) = stub_transport(&[A, B], CUTOFF, |call| match call.host.as_str() {
        "a.test" => Reply::Fail("HTTP 502".to_owned()),
        _ => Reply::Body(result_body(&call.request, r#""0x01""#)),
    });
    let platform = provider_from_transport(transport.clone());
    let token = platform.clone();
    eth_call(&token).unwrap();
    assert_eq!(
        transport.order_labels(),
        [B, A],
        "a request through the clone moved the shared order"
    );
}

#[test]
fn t35_27_the_cutoff_moves_on_before_the_lower_layer_would() {
    // A node that never answers is given up at the cutoff, not at the 30 s lower-layer bound.
    let (provider, sender) = stub_provider(&[A, B], 0.1, failing_nodes(&[A], |_| Reply::Hang));
    let started = Instant::now();
    assert_eq!(eth_call(&provider).unwrap(), "0x01");
    let took = started.elapsed().as_secs_f64();
    assert!(took >= 0.1 && took < DIRECT_NODE_TIMEOUT_S, "took {took}");
    assert_eq!(sender.hosts(), ["a.test", "b.test"]);
}

#[test]
fn t35_27_with_four_hanging_nodes_the_request_fails_after_the_sum_of_the_cutoffs_and_not_earlier() {
    let (provider, sender) = stub_provider(&[A, B, C, D], 0.1, |_| Reply::Hang);
    let started = Instant::now();
    assert!(eth_call(&provider).is_err());
    assert_eq!(sender.hosts().len(), 4);
    assert!(
        started.elapsed().as_secs_f64() >= 0.4,
        "four cutoffs of 0.1 s in series"
    );
}

#[test]
fn the_production_bounds_are_ten_seconds_for_failover_and_thirty_for_a_direct_node() {
    assert_eq!(NODE_CUTOFF_S, 10.);
    assert_eq!(DIRECT_NODE_TIMEOUT_S, 30.);
    let failover = AlloyTransport::with_event_handlers(vec![A.to_owned(), B.to_owned()], vec![]).unwrap();
    assert_eq!(failover.inner.cutoff_s, NODE_CUTOFF_S);
    let direct = AlloyTransport::direct(A.to_owned(), vec![]).unwrap();
    assert_eq!(direct.inner.cutoff_s, DIRECT_NODE_TIMEOUT_S);
}

#[test]
fn t35_30_the_all_failed_text_lists_each_host_once_and_nothing_more() {
    // Two configured URLs on one host (different keys) and a second host.
    let urls = [
        "https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X",
        "https://user:secret@rpc.example.org:8545/v3/OTHERKEY?token=Y",
        "https://other.example.net/key/THIRDKEY",
    ];
    let (provider, _) = stub_provider(&urls, CUTOFF, |call| {
        Reply::Fail(format!("Transport '{}' error: HTTP 502 Bad Gateway", call.uri))
    });
    let err = eth_call(&provider).unwrap_err();
    assert!(err.is_transport_error(), "{err}");
    let text = err.to_string();
    assert_eq!(text.matches("https://rpc.example.org:8545").count(), 1, "{text}");
    assert_eq!(text.matches("https://other.example.net").count(), 1, "{text}");
    for secret in [
        "user", "secret", "/v3/", "APIKEY", "OTHERKEY", "THIRDKEY", "token=", "/key/",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // A transport-class outcome carries no node suffix (R35.1.15).
    assert!(!text.contains("(node "), "{text}");
}

#[test]
fn t35_28_nothing_secret_reaches_a_returned_text_or_a_log_line() {
    let capturing = install_log_capture();
    let url = "https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X";
    let secrets = ["user", "secret", "/v3/APIKEY", "APIKEY", "token=X"];

    // Lower-layer failures that embed the URL in several forms, including a DNS failure.
    let (provider, _) = stub_provider(&[url], CUTOFF, |call| {
        Reply::Fail(format!(
            "Transport '{0}' error: dns error: failed to lookup address information for {0}",
            call.uri
        ))
    });
    let err = eth_call(&provider).unwrap_err().to_string();
    assert!(err.contains("https://rpc.example.org:8545"), "{err}");
    for secret in secrets {
        assert!(!err.contains(secret), "{secret} in {err}");
    }

    // A node that answers a JSON-RPC error whose message mentions its own URL.
    let (provider, _) = stub_provider(&[url], CUTOFF, |call| {
        Reply::Body(error_body(
            &call.request,
            3,
            &format!("execution reverted at {}", call.uri),
        ))
    });
    let err = eth_call(&provider).unwrap_err().to_string();
    assert!(err.ends_with("(node https://rpc.example.org:8545)"), "{err}");
    for secret in secrets {
        assert!(!err.contains(secret), "{secret} in {err}");
    }

    let lines: Vec<String> = captured_log_lines()
        .into_iter()
        .filter(|line| line.contains("EVM RPC"))
        .collect();
    if capturing {
        assert!(
            lines.iter().any(|line| line
                == "WARN EVM RPC https://rpc.example.org:8545 answered eth_call with error 3: execution reverted at \
                    https://rpc.example.org:8545"),
            "{lines:?}"
        );
    }
    for line in &lines {
        for secret in secrets {
            assert!(!line.contains(secret), "{secret} in log line {line}");
        }
    }
}

#[test]
fn t35_29_the_node_suffix_is_on_a_json_rpc_outcome_only() {
    let (provider, _) = stub_provider(&[A, B], CUTOFF, |call| {
        if call.host == "b.test" {
            Reply::Body(error_body(&call.request, 3, "execution reverted"))
        } else {
            Reply::Fail("HTTP 502".to_owned())
        }
    });
    let json_rpc = eth_call(&provider).unwrap_err();
    assert!(
        json_rpc
            .to_string()
            .ends_with("execution reverted (node http://b.test)"),
        "{json_rpc}"
    );

    let (provider, _) = stub_provider(&[A, B], CUTOFF, |_| Reply::Fail("HTTP 502".to_owned()));
    let transport = eth_call(&provider).unwrap_err();
    assert!(!transport.to_string().contains("(node "), "{transport}");
}

fn batch_packet(provider: &KdfProvider) -> RequestPacket {
    let first = provider
        .client()
        .make_request("eth_blockNumber", ())
        .serialize()
        .unwrap();
    let second = provider.client().make_request("eth_chainId", ()).serialize().unwrap();
    RequestPacket::Batch(vec![first, second])
}

fn batch_body(request: &Json, error_in_second: bool) -> String {
    let ids: Vec<Json> = request.as_array().unwrap().iter().map(|r| r["id"].clone()).collect();
    let second = if error_in_second {
        format!(
            r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32000,"message":"boom"}}}}"#,
            ids[1]
        )
    } else {
        format!(r#"{{"jsonrpc":"2.0","id":{},"result":"0x2"}}"#, ids[1])
    };
    format!(r#"[{{"jsonrpc":"2.0","id":{},"result":"0x1"}},{}]"#, ids[0], second)
}

#[test]
fn t35_33_a_batch_with_one_error_element_fails_the_node_as_a_whole() {
    let (transport, sender) = stub_transport(&[A, B], CUTOFF, |call| {
        Reply::Body(batch_body(&call.request, call.host == "a.test"))
    });
    let provider = provider_from_transport(transport.clone());
    let packet = batch_packet(&provider);
    let answer = block_on(transport.clone().call(packet)).unwrap();
    assert!(matches!(answer, ResponsePacket::Batch(ref v) if v.len() == 2));
    assert_eq!(sender.hosts(), ["a.test", "b.test"]);
}

#[test]
fn t35_33_a_single_envelope_answering_a_batch_is_a_node_failure() {
    let (transport, sender) = stub_transport(&[A, B], CUTOFF, |call| {
        if call.host == "a.test" {
            Reply::Body(r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#.to_owned())
        } else {
            Reply::Body(batch_body(&call.request, false))
        }
    });
    let provider = provider_from_transport(transport.clone());
    let packet = batch_packet(&provider);
    block_on(transport.clone().call(packet)).unwrap();
    assert_eq!(sender.hosts(), ["a.test", "b.test"]);
}

#[test]
fn an_empty_node_list_is_an_error_not_a_panic() {
    let (transport, _) = stub_transport(&[], CUTOFF, |_| Reply::Hang);
    let provider = provider_from_transport(transport);
    assert!(eth_call(&provider).is_err());
}
