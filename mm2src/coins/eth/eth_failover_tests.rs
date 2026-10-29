//! CRD ch.35 §35.1.F at the level of the coin: nonce gathering and the signed-transaction broadcast
//! (R35.1.12, T35.31), the caller-supplied raw transaction (T35.32), the ERC-20 metadata reads and the node
//! order of tokens (T35.26, T35.33). Nodes are stubs; nothing here touches a network.

use super::eth_tests::{eth_coin_for_test, eth_coin_for_test_with_providers};
use super::*;
use crate::eth::alloy_compat::provider::provider_from_transport;
use crate::eth::alloy_compat::transport::test_support::*;
use crate::eth::alloy_compat::{AlloyTransport, KdfProvider};
use alloy::providers::Provider as _;
use common::block_on;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};

/// The per-request bound of a direct provider in production.
const DIRECT_BOUND_S: f64 = 30.;
const NODE_1: &str = "http://n1.test";
const NODE_2: &str = "http://n2.test";
const NODE_3: &str = "http://n3.test";
const TX_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

fn instance(url: &str, handler: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> (Web3Instance, StubSender) {
    let (provider, sender) = stub_provider(&[url], DIRECT_BOUND_S, handler);
    (
        Web3Instance {
            web3: provider,
            is_parity: false,
        },
        sender,
    )
}

fn nonce_reply(call: &Call, nonce: u64) -> Reply {
    Reply::Body(result_body(&call.request, &format!(r#""{nonce:#x}""#)))
}

fn rejection(call: &Call, message: &str) -> Reply { Reply::Body(error_body(&call.request, -32000, message)) }

fn my_address() -> Address {
    eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None)
        .1
        .my_address
}

/// The three nodes of T35.31: pending nonces 5, 7, 7; `send` decides what each of them answers to a
/// `eth_sendRawTransaction`.
fn three_nodes(send: [fn(&Call) -> Reply; 3]) -> (Vec<Web3Instance>, [StubSender; 3]) {
    let nonces = [5, 7, 7];
    let urls = [NODE_1, NODE_2, NODE_3];
    let mut instances = Vec::new();
    let mut senders = Vec::new();
    for ((url, nonce), send) in urls.into_iter().zip(nonces).zip(send) {
        let (node, sender) = instance(url, move |call| match call.method.as_str() {
            "eth_getTransactionCount" => nonce_reply(call, nonce),
            "eth_sendRawTransaction" => send(call),
            other => panic!("the stub node does not serve `{other}`"),
        });
        instances.push(node);
        senders.push(sender);
    }
    let [s1, s2, s3]: [StubSender; 3] = senders.try_into().ok().unwrap();
    (instances, [s1, s2, s3])
}

fn accept(call: &Call) -> Reply { Reply::Body(result_body(&call.request, &format!(r#""{TX_HASH}""#))) }

#[test]
fn t35_31_the_highest_pending_nonce_wins_and_its_nodes_are_remembered() {
    let (nodes, [s1, s2, s3]) = three_nodes([accept, accept, accept]);
    let pending = block_on(pending_nonce_from_nodes(my_address(), &nodes, 0.01)).unwrap();
    assert_eq!(pending.nonce, U256::from(7));
    assert_eq!(pending.nodes.len(), 2, "nodes two and three reported 7");
    for sender in [&s1, &s2, &s3] {
        assert_eq!(sender.hosts_for("eth_getTransactionCount").len(), 1, "one round");
    }
}

#[test]
fn t35_31_a_lagging_node_does_not_hold_the_send() {
    // Today's all-must-agree behavior would loop here; the highest nonce wins instead.
    let (nodes, _) = three_nodes([accept, accept, accept]);
    let nonce = get_addr_nonce(my_address(), nodes).wait().unwrap();
    assert_eq!(nonce, U256::from(7));
}

#[test]
fn t35_31_the_transaction_goes_to_the_highest_nonce_nodes_only_and_one_acceptance_is_enough() {
    let (nodes, [s1, s2, s3]) = three_nodes([accept, |call| rejection(call, "already known"), accept]);
    let pending = block_on(pending_nonce_from_nodes(my_address(), &nodes, 0.01)).unwrap();
    block_on(broadcast_signed_tx(&pending.nodes, &[1, 2, 3])).unwrap();
    assert!(s1.hosts_for("eth_sendRawTransaction").is_empty(), "node one is behind");
    assert_eq!(s2.hosts_for("eth_sendRawTransaction").len(), 1);
    assert_eq!(s3.hosts_for("eth_sendRawTransaction").len(), 1);
}

#[test]
fn t35_31_when_both_reject_the_send_fails_with_the_last_rejection() {
    let (nodes, _) = three_nodes([
        accept,
        |call| rejection(call, "already known"),
        |call| rejection(call, "replacement transaction underpriced"),
    ]);
    let pending = block_on(pending_nonce_from_nodes(my_address(), &nodes, 0.01)).unwrap();
    let err = block_on(broadcast_signed_tx(&pending.nodes, &[1, 2, 3])).unwrap_err();
    assert!(err.contains("replacement transaction underpriced"), "{err}");
    assert!(err.ends_with("(node http://n3.test)"), "{err}");
    assert!(!err.contains("already known"), "{err}");
}

#[test]
fn t35_31_the_broadcast_does_not_wait_for_a_node_that_never_answers() {
    // Node two hangs (only the 30 s bound would end it); node three accepts. The nodes are asked
    // concurrently and the first acceptance ends the broadcast.
    let (nodes, [_, s2, s3]) = three_nodes([accept, |_| Reply::Hang, accept]);
    let pending = block_on(pending_nonce_from_nodes(my_address(), &nodes, 0.01)).unwrap();
    let started = std::time::Instant::now();
    block_on(broadcast_signed_tx(&pending.nodes, &[1, 2, 3])).unwrap();
    assert!(started.elapsed().as_secs_f64() < DIRECT_BOUND_S / 2.);
    assert_eq!(s2.hosts_for("eth_sendRawTransaction").len(), 1, "asked, not skipped");
    assert_eq!(s3.hosts_for("eth_sendRawTransaction").len(), 1);
}

/// T35.34: a node of the nonce gathering that fails is named with the coin's ticker and its position in the
/// coin's list (R35.1.16), not "1 of 1".
#[test]
fn t35_34_a_failing_node_of_the_nonce_gathering_is_named_with_ticker_and_list_position() {
    let capturing = install_log_capture();
    let listed = crate::node_url::listed_nodes("T3534NONCE", vec![NODE_1.into(), NODE_2.into(), NODE_3.into()]);
    let mut instances = Vec::new();
    for (i, node) in listed.into_iter().enumerate() {
        let failing = i == 1;
        let (transport, _) = stub_transport_of_nodes(vec![node], DIRECT_BOUND_S, vec![], move |call| {
            if failing {
                Reply::Fail("connection refused".to_owned())
            } else {
                nonce_reply(call, 7)
            }
        });
        instances.push(Web3Instance {
            web3: provider_from_transport(transport),
            is_parity: false,
        });
    }
    let pending = block_on(pending_nonce_from_nodes(my_address(), &instances, 0.01)).unwrap();
    assert_eq!(pending.nodes.len(), 2);
    if capturing {
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|l| l.contains("[T3534NONCE] Error"))
            .collect();
        let wanted = format!(
            " when getting nonce for addr {:?} from node http://n2.test (node 2 of 3)",
            my_address()
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[T3534NONCE] Error ") && l.ends_with(&wanted)),
            "{lines:?}"
        );
    }
}

#[test]
fn t35_31_no_node_answering_the_nonce_is_six_rounds_then_failure() {
    let mut senders = Vec::new();
    let mut nodes = Vec::new();
    for url in [NODE_1, NODE_2] {
        let (node, sender) = instance(url, |_| Reply::Fail("connection refused".to_owned()));
        nodes.push(node);
        senders.push(sender);
    }
    let started = std::time::Instant::now();
    let err = block_on(pending_nonce_from_nodes(my_address(), &nodes, 0.02))
        .err()
        .unwrap();
    assert!(err.contains("Couldn't get nonce"), "{err}");
    for sender in &senders {
        assert_eq!(
            sender.hosts_for("eth_getTransactionCount").len(),
            6,
            "one request per round"
        );
    }
    // Five pauses between six rounds.
    assert!(started.elapsed().as_secs_f64() >= 5. * 0.02);
}

#[test]
fn t35_31_a_signed_swap_send_uses_the_highest_nonce_and_only_its_nodes() {
    // The whole path: nonce gathering, signing, broadcast, and the nonce-increase wait.
    let sent: Arc<Mutex<Vec<u64>>> = Arc::default();
    let node3_nonce = Arc::new(AtomicU64::new(7));
    let mut instances = Vec::new();
    let mut senders = Vec::new();
    for (url, start) in [(NODE_1, 5u64), (NODE_2, 7), (NODE_3, 7)] {
        let sent = sent.clone();
        let live = node3_nonce.clone();
        let (node, sender) = instance(url, move |call| match (call.host.as_str(), call.method.as_str()) {
            (_, "eth_getTransactionCount") if call.host == "n3.test" => nonce_reply(call, live.load(AtomicOrd::SeqCst)),
            (_, "eth_getTransactionCount") => nonce_reply(call, start),
            ("n2.test", "eth_sendRawTransaction") => rejection(call, "already known"),
            ("n3.test", "eth_sendRawTransaction") => {
                let raw = hex::decode(call.request["params"][0].as_str().unwrap().trim_start_matches("0x")).unwrap();
                let tx = signed_eth_tx_from_bytes(&raw).unwrap();
                sent.lock().unwrap().push(tx.nonce.as_u64());
                // The node now counts the transaction.
                live.store(8, AtomicOrd::SeqCst);
                accept(call)
            },
            other => panic!("the stub node must not receive {other:?}"),
        });
        instances.push(node);
        senders.push(sender);
    }
    let (main, _main_sender) = stub_provider(&["http://main.test"], 10., |call| match call.method.as_str() {
        "eth_gasPrice" => Reply::Body(result_body(&call.request, r#""0x3b9aca00""#)),
        "eth_feeHistory" => Reply::Body(result_body(
            &call.request,
            r#"{"oldestBlock":"0x1","baseFeePerGas":["0x3b9aca00","0x3b9aca00"],"gasUsedRatio":[0.5],"reward":[[]]}"#,
        )),
        other => panic!("the main provider must not serve `{other}` here"),
    });
    let (ctx, coin) = eth_coin_for_test_with_providers(
        EthCoinType::Eth,
        main,
        instances,
        None,
        mm2_net_config::SUPPORTED_NETIDS[0],
        18,
    );
    block_on(sign_and_send_transaction_impl(
        ctx,
        coin.clone(),
        1000000000000u64.into(),
        Action::Call(coin.my_address),
        vec![],
        21000.into(),
    ))
    .unwrap();
    assert_eq!(*sent.lock().unwrap(), vec![7], "signed with the highest pending nonce");
    assert!(
        senders[0].hosts_for("eth_sendRawTransaction").is_empty(),
        "the lagging node gets no copy"
    );
    assert_eq!(senders[1].hosts_for("eth_sendRawTransaction").len(), 1);
    assert_eq!(senders[2].hosts_for("eth_sendRawTransaction").len(), 1);
}

/// A coin whose failover provider is over nodes A and B (played by `handler`) and whose single-node instances
/// are one stub that must never be asked.
fn coin_over_failover_nodes(
    handler: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> (EthCoin, StubSender, StubSender) {
    let (web3, failover) = stub_provider(&["http://a.test", "http://b.test"], 10., handler);
    let (single, single_sender) = instance(NODE_1, |call| {
        panic!("the single-node instance was asked {}", call.method)
    });
    let (_ctx, coin) = eth_coin_for_test_with_providers(
        EthCoinType::Eth,
        web3,
        vec![single],
        None,
        mm2_net_config::SUPPORTED_NETIDS[0],
        18,
    );
    (coin, failover, single_sender)
}

#[test]
fn t35_32_a_caller_supplied_raw_transaction_goes_through_rotation_not_the_signed_broadcast() {
    let (coin, failover, single) = coin_over_failover_nodes(|call| match call.host.as_str() {
        "a.test" => rejection(call, "nonce too low"),
        _ => accept(call),
    });
    let hash = coin.send_raw_tx_bytes(&[1, 2, 3]).wait().unwrap();
    assert_eq!(hash, TX_HASH.trim_start_matches("0x"));
    assert_eq!(
        failover.hosts(),
        ["a.test", "b.test"],
        "first node's rejection moved the request on"
    );
    assert!(single.calls().is_empty(), "the R35.1.12 broadcast set was not used");

    // The hex form takes the same route.
    failover.clear();
    coin.send_raw_tx("0x010203").wait().unwrap();
    assert_eq!(failover.hosts(), ["b.test"], "the accepting node is first now");
}

#[test]
fn t35_32_when_every_node_rejects_the_caller_sees_the_last_rejection_naming_it() {
    let (coin, failover, _) = coin_over_failover_nodes(|call| {
        let message = format!("rejected by {}", call.host);
        rejection(call, &message)
    });
    let err = coin.send_raw_tx_bytes(&[1, 2, 3]).wait().unwrap_err();
    assert_eq!(failover.hosts(), ["a.test", "b.test"]);
    assert!(err.contains("rejected by b.test"), "{err}");
    assert!(err.ends_with("(node http://b.test)"), "{err}");
}

fn uint_word(value: u8) -> String { format!("0x{}{:02x}", "00".repeat(31), value) }

fn string_word(text: &str) -> String {
    let mut padded = hex::encode(text);
    padded.push_str(&"00".repeat(32 - text.len()));
    format!(
        "0x{}{}{}",
        uint_word(0x20).trim_start_matches("0x"),
        uint_word(text.len() as u8).trim_start_matches("0x"),
        padded
    )
}

fn token_node(call: &Call) -> Reply {
    if call.host == "a.test" {
        return rejection(call, "execution reverted");
    }
    let data = call.request["params"][0]["data"].as_str().unwrap();
    let answer = if data.starts_with("0x313ce567") {
        uint_word(6)
    } else {
        string_word("ABC")
    };
    Reply::Body(result_body(&call.request, &format!(r#""{answer}""#)))
}

fn order_of(provider: &KdfProvider) -> Vec<String> {
    provider
        .client()
        .transport()
        .as_any()
        .downcast_ref::<AlloyTransport>()
        .expect("a failover transport")
        .order_labels()
}

#[test]
fn t35_33_token_metadata_reads_fail_over() {
    let (web3, failover) = stub_provider(&["http://a.test", "http://b.test"], 10., token_node);
    let token = Address::from_slice(&[0xab; 20]);
    assert_eq!(block_on(get_token_decimals(&web3, token)).unwrap(), 6);
    assert_eq!(block_on(get_token_symbol(&web3, token)).unwrap(), "ABC");
    // The decimals read tried A, then B; the symbol read started at B.
    assert_eq!(failover.hosts(), ["a.test", "b.test", "b.test"]);
}

#[test]
fn t35_33_activating_a_token_reads_decimals_through_failover_and_the_token_shares_the_platform_order() {
    // D35.8 (i): a token shares its platform's live node order. Flipping `TOKEN_SHARES_PLATFORM_NODE_ORDER`
    // to `false` makes the last assertion fail by design: the token's order is then its own.
    let (web3, _failover) = stub_provider(&["http://a.test", "http://b.test"], 10., token_node);
    let platform_provider = web3.clone();
    let (_ctx, platform) = eth_coin_for_test_with_providers(
        EthCoinType::Eth,
        web3,
        vec![],
        None,
        mm2_net_config::SUPPORTED_NETIDS[0],
        18,
    );
    // `JST` has no `decimals` in the test coins file, so activation reads them on-chain.
    let token = block_on(platform.erc20_token_from_conf_or_contract(
        "JST".to_owned(),
        Address::from_slice(&[0xab; 20]),
        1,
        false,
    ))
    .unwrap();
    assert_eq!(token.decimals, 6);
    // The read failed over from A to B on the platform's order; the token sees it.
    assert_eq!(order_of(&platform_provider), ["http://b.test", "http://a.test"]);
    assert_eq!(order_of(&token.alloy_provider()), ["http://b.test", "http://a.test"]);
    // And a request through the token moves the platform's order.
    let token_provider = token.alloy_provider();
    block_on(get_token_symbol(&token_provider, Address::from_slice(&[0xab; 20]))).unwrap();
    assert_eq!(order_of(&platform_provider), ["http://b.test", "http://a.test"]);
}
