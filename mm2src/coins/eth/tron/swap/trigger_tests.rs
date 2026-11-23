//! Tests of sending a contract call and of its dry run (CRD ch.21 §21.9, §21.11C), against a stub node on the
//! loopback interface: the real coin code and the real HTTP layer run, only the node's answers are canned.

use super::contract_abi::{decode_payments_output, encode_call, encode_payments_query, PaymentRecord, PaymentState,
                          TronSwapCall};
use super::ids_tests::{addr, secret_hash, ID, LOCK_TIME};
use super::trigger::{dry_run_energy, read_swap_head, send_trigger, SWAP_HEAD_TIME_TOLERANCE_SECS};
use super::tx::TronSwapTx;
use super::TronSwapError;
use crate::eth::tron::api::{TronApiClient, TronApiError};
use crate::eth::tron::fee::TronFeeError;
use crate::eth::tron::nodes::TronNode;
use crate::eth::tron::test_node::{Reply, StubNode};
use crate::eth::tron::wallet_path_tests::{trx_coin, NOW_BLOCK_BODY};
use crate::eth::tron::{tron_coin_from_conf_and_request, Network, TronAddress};
use crate::eth::{EthCoin, EthCoinType};
use crate::CoinProtocol;
use common::block_on;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};

/// The address of the test key (the secret key 1), which signs for the test coin.
const SENDER_WIRE_HEX: &str = "417e5f4552091a69125d5dfcb7b8c2659029395bdf";
const CALL_VALUE: i64 = 5_000_000;
const FEE_LIMIT_SUN: u64 = 30_000_000;

fn swap_contract() -> TronAddress { addr(0x10) }

fn payment_data() -> Vec<u8> {
    encode_call(&TronSwapCall::EthPayment {
        id: ID,
        receiver: addr(0x30),
        secret_hash: secret_hash(),
        lock_time: u64::from(LOCK_TIME),
    })
    .unwrap()
}

/// The id of the transaction a `broadcasthex` request body carries.
fn txid_of_broadcast(body: &str) -> String {
    let request: Json = serde_json::from_str(body).expect("the client sends JSON");
    let bytes = hex::decode(request["transaction"].as_str().expect("a hex string")).expect("hex");
    format!(
        "{:x}",
        TronSwapTx::from_bytes(&bytes).expect("a signed transaction").txid()
    )
}

/// The local time, in milliseconds.
fn local_ms() -> i64 { common::now_ms() as i64 }

/// A node with the head block of [`NOW_BLOCK_BODY`] at the local time, whose `broadcasthex` answers what
/// `broadcast` makes of the request body.
fn node(broadcast: impl Fn(&str) -> String + Send + Sync + 'static) -> StubNode { node_at(local_ms(), broadcast) }

/// As [`node`], with the head block's time `head_ms`.
fn node_at(head_ms: i64, broadcast: impl Fn(&str) -> String + Send + Sync + 'static) -> StubNode {
    let head = NOW_BLOCK_BODY.replace("\"timestamp\":1700000000000", &format!("\"timestamp\":{head_ms}"));
    assert_ne!(
        head, NOW_BLOCK_BODY,
        "the head block body carries the timestamp that is replaced"
    );
    StubNode::start(move |path, body| match path {
        "/wallet/getnowblock" => Reply::ok(&head),
        "/wallet/broadcasthex" => Reply::ok(&broadcast(body)),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// The transaction that reaches the node carries the contract, the data, the call value and the `fee_limit`
/// given, on the reference block of the head, signed for the coin's address.
#[test]
fn the_broadcast_carries_the_call_as_given() {
    let head_ms = local_ms();
    let node = node_at(head_ms, |body| {
        json!({ "result": true, "txid": txid_of_broadcast(body) }).to_string()
    });
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    let tx = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .expect("an accepted broadcast");

    let bodies = node.bodies_for("/wallet/broadcasthex");
    assert_eq!(bodies.len(), 1);
    let sent = hex::decode(bodies[0]["transaction"].as_str().unwrap()).unwrap();
    assert_eq!(sent, tx.to_bytes());
    let sent = TronSwapTx::from_bytes(&sent).unwrap();
    assert_eq!(sent, tx);

    let trigger = sent.trigger().unwrap();
    assert_eq!(trigger.owner, TronAddress::from_hex(SENDER_WIRE_HEX).unwrap());
    assert_eq!(trigger.contract, swap_contract());
    assert_eq!(trigger.call_value, CALL_VALUE);
    assert_eq!(trigger.data, payment_data());

    let raw = sent.raw();
    assert_eq!(raw.fee_limit, FEE_LIMIT_SUN as i64);
    // Head block 58 251 145 = 0x0378d789 with the id 0000000003456789 abcdef0123456789 ...: the last two bytes of
    // the number and bytes 8 to 16 of the id.
    assert_eq!(raw.ref_block_bytes, vec![0xd7, 0x89]);
    assert_eq!(raw.ref_block_hash, hex::decode("abcdef0123456789").unwrap());
    assert_eq!(raw.expiration, head_ms + 60_000);
    assert!(raw.timestamp >= head_ms);
    assert_eq!(sent.signature().len(), 65);
}

/// An accepting answer without a `txid` is an acceptance: the id is known locally.
#[test]
fn an_acceptance_without_a_txid_is_an_acceptance() {
    let node = node(|_| r#"{"result":true}"#.to_owned());
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let tx = block_on(send_trigger(&coin, &swap_contract(), payment_data(), 0, FEE_LIMIT_SUN)).unwrap();
    assert_eq!(node.bodies_for("/wallet/broadcasthex").len(), 1);
    assert_eq!(tx.trigger().unwrap().call_value, 0);
}

/// A node that refuses the transaction (`result: false`) is a typed error that keeps the signed transaction; so
/// is an accepting node that names another transaction id.
#[test]
fn a_refused_broadcast_is_a_typed_error_with_the_signed_transaction() {
    let refusing = node(|_| r#"{"result":false,"code":"SIGERROR","message":"refused"}"#.to_owned());
    let (_ctx, coin) = block_on(trx_coin(&refusing.url()));
    let error = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .unwrap_err();
    let (tx, error) = match error {
        TronSwapError::Broadcast { tx, error } => (tx, error),
        other => panic!("not a broadcast error: {other}"),
    };
    assert!(
        matches!(&error, TronApiError::RemoteError { code, .. } if code == "SIGERROR"),
        "{error}"
    );
    assert_eq!(tx.trigger().unwrap().data, payment_data());
    let sent = refusing.bodies_for("/wallet/broadcasthex");
    assert_eq!(sent[0]["transaction"], json!(hex::encode(tx.to_bytes())));

    let other_id = node(|_| json!({ "result": true, "txid": "ab".repeat(32) }).to_string());
    let (_ctx, coin) = block_on(trx_coin(&other_id.url()));
    let error = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .unwrap_err();
    assert!(matches!(error, TronSwapError::Broadcast {
        error: TronApiError::InvalidResponse(_),
        ..
    }));
}

/// A head block that cannot be read fails the send before anything is signed or broadcast.
#[test]
fn a_failed_head_block_read_is_a_node_error_without_a_broadcast() {
    let node = StubNode::start(|_, _| Reply::status(500, "stub failure"));
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .unwrap_err();
    assert!(matches!(error, TronSwapError::Node(_)), "{error}");
    assert!(node.bodies_for("/wallet/broadcasthex").is_empty());
}

/// A TRX coin activated against the nodes `urls`. The coin puts them into an order of its own.
fn trx_coin_over(urls: &[String]) -> (MmArc, EthCoin) {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let mut key = [0u8; 32];
    key[31] = 1;
    let protocol = CoinProtocol::TRX { network: Network::Nile };
    let request = json!({ "urls": urls });
    let coin = block_on(tron_coin_from_conf_and_request(
        &ctx,
        "TRX",
        &json!({}),
        &request,
        &key,
        protocol,
    ))
    .expect("a TRX coin activates against stub nodes");
    (ctx, coin)
}

/// The head time is judged node by node: a node whose head block is outside the tolerance gave an unusable answer,
/// and the next node of the list is asked. The head of the first node inside the tolerance is the one built on.
#[test]
fn a_stale_head_moves_on_to_the_next_node() {
    let stale = node_at(local_ms() - 45_000, |_| r#"{"result":true}"#.to_owned());
    let fresh_ms = local_ms() + 5_000;
    let fresh = node_at(fresh_ms, |_| r#"{"result":true}"#.to_owned());

    // The list in a fixed order: the stale node is asked first, then the fresh one.
    let nodes = vec![TronNode::plain(stale.url()), TronNode::plain(fresh.url())];
    let client = TronApiClient::new_with_cutoff_s("TRX", nodes, 5.);
    let head = block_on(read_swap_head(&client)).expect("the second node's head");
    assert_eq!(head.block_header.raw_data.timestamp, fresh_ms);
    assert_eq!(stale.bodies_for("/wallet/getnowblock").len(), 1);
    assert_eq!(fresh.bodies_for("/wallet/getnowblock").len(), 1);

    // The call of a coin over both nodes is sent, whichever node its list starts with, on the fresh head.
    let (_ctx, coin) = trx_coin_over(&[stale.url(), fresh.url()]);
    let tx = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .expect("a call built on the fresh head");
    assert_eq!(tx.raw().expiration, fresh_ms + 60_000);
    let broadcasts = stale.bodies_for("/wallet/broadcasthex").len() + fresh.bodies_for("/wallet/broadcasthex").len();
    assert_eq!(broadcasts, 1);
}

/// Only when every node answers a head outside the tolerance is the error the head-time one, which names the local
/// clock as a possible cause; nothing is broadcast. A node that fails in another way beside a stale one is a node
/// error.
#[test]
fn every_node_out_of_tolerance_is_the_head_time_error() {
    let behind = node_at(local_ms() - 45_000, |_| r#"{"result":true}"#.to_owned());
    let ahead = node_at(local_ms() + 45_000, |_| r#"{"result":true}"#.to_owned());
    let (_ctx, coin) = trx_coin_over(&[behind.url(), ahead.url()]);
    let error = block_on(send_trigger(
        &coin,
        &swap_contract(),
        payment_data(),
        CALL_VALUE,
        FEE_LIMIT_SUN,
    ))
    .unwrap_err();
    assert!(matches!(error, TronSwapError::HeadTimeOutOfTolerance { .. }), "{error}");
    assert!(error.to_string().contains("the local clock may be wrong"), "{error}");
    assert_eq!(behind.bodies_for("/wallet/getnowblock").len(), 1);
    assert_eq!(ahead.bodies_for("/wallet/getnowblock").len(), 1);
    assert!(behind.bodies_for("/wallet/broadcasthex").is_empty());
    assert!(ahead.bodies_for("/wallet/broadcasthex").is_empty());

    let failing = StubNode::start(|_, _| Reply::status(500, "stub failure"));
    let nodes = vec![TronNode::plain(behind.url()), TronNode::plain(failing.url())];
    let client = TronApiClient::new_with_cutoff_s("TRX", nodes, 5.);
    let error = block_on(read_swap_head(&client)).unwrap_err();
    assert!(matches!(error, TronSwapError::Node(_)), "{error}");
}

/// The expiry counts from the head block's time, which one node states, so the local clock bounds it: a head block
/// more than the tolerance away from the local time, either way, is refused before anything is signed or
/// broadcast; one inside the tolerance is built on.
#[test]
fn a_head_block_time_outside_the_tolerance_is_refused_without_a_broadcast() {
    let tolerance_ms = SWAP_HEAD_TIME_TOLERANCE_SECS as i64 * 1000;
    assert_eq!(tolerance_ms, 30_000);
    let send_at = |offset_ms: i64| {
        let node = node_at(local_ms() + offset_ms, |_| r#"{"result":true}"#.to_owned());
        let (_ctx, coin) = block_on(trx_coin(&node.url()));
        let result = block_on(send_trigger(
            &coin,
            &swap_contract(),
            payment_data(),
            CALL_VALUE,
            FEE_LIMIT_SUN,
        ));
        (result, node.bodies_for("/wallet/broadcasthex").len())
    };

    // 45 seconds ahead of the local clock, and 45 seconds behind it.
    for offset_ms in [45_000, -45_000] {
        let (result, broadcasts) = send_at(offset_ms);
        assert!(
            matches!(result, Err(TronSwapError::HeadTimeOutOfTolerance { head_time_ms, local_time_ms })
                if (head_time_ms - local_time_ms - offset_ms).abs() < 10_000),
            "{offset_ms}"
        );
        assert_eq!(broadcasts, 0, "{offset_ms}");
    }
    // 15 seconds either way is accepted.
    for offset_ms in [15_000, -15_000] {
        let (result, broadcasts) = send_at(offset_ms);
        let tx = result.unwrap_or_else(|e| panic!("{offset_ms}: {e}"));
        assert_eq!(broadcasts, 1, "{offset_ms}");
        assert!((tx.raw().expiration - tx.raw().timestamp - offset_ms - 60_000).abs() < 10_000);
    }
}

/// The refusals that need no node: a negative call value, a `fee_limit` beyond the signed 64-bit field, a coin
/// that is no TRON coin, and a coin without a local key.
#[test]
fn the_local_refusals_come_before_any_request() {
    let node = node(|_| r#"{"result":true}"#.to_owned());
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let send = |call_value: i64, fee_limit_sun: u64| {
        block_on(send_trigger(
            &coin,
            &swap_contract(),
            payment_data(),
            call_value,
            fee_limit_sun,
        ))
    };
    assert!(matches!(send(-1, FEE_LIMIT_SUN), Err(TronSwapError::NegativeCallValue)));
    assert!(matches!(
        send(0, u64::MAX),
        Err(TronSwapError::FeeLimit(TronFeeError::Overflow))
    ));
    assert!(matches!(
        block_on(dry_run_energy(&coin, &swap_contract(), &payment_data(), -1)),
        Err(TronSwapError::NegativeCallValue)
    ));
    assert!(node.requests().is_empty(), "{:?}", node.requests());

    let (_eth_ctx, eth) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    assert!(matches!(
        block_on(send_trigger(&eth, &swap_contract(), payment_data(), 0, FEE_LIMIT_SUN)),
        Err(TronSwapError::NotTronCoin)
    ));
    assert!(matches!(
        block_on(dry_run_energy(&eth, &swap_contract(), &payment_data(), 0)),
        Err(TronSwapError::NotTronCoin)
    ));

    // A coin a hardware wallet signs for holds no key: refused before the node client is even looked at.
    #[cfg(not(target_os = "ios"))]
    {
        let (_trezor_ctx, trezor) = crate::eth::eth_tests::trezor_eth_coin_for_test(EthCoinType::Tron);
        assert!(matches!(
            block_on(send_trigger(
                &trezor,
                &swap_contract(),
                payment_data(),
                0,
                FEE_LIMIT_SUN
            )),
            Err(TronSwapError::NoLocalKey)
        ));
    }
}

/// A node whose `triggerconstantcontract` answers `answer`.
fn dry_run_node(answer: Json) -> StubNode {
    StubNode::start(move |path, _| match path {
        "/wallet/triggerconstantcontract" => Reply::ok(&answer.to_string()),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// The dry run asks for the call as given, made as the coin's address, and reads the node's energy figure.
#[test]
fn the_dry_run_reads_the_nodes_energy_figure() {
    let node = dry_run_node(json!({
        "result": { "result": true },
        "energy_used": 31_000,
        "constant_result": [""],
        "transaction": { "ret": [{}] },
    }));
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let energy = block_on(dry_run_energy(&coin, &swap_contract(), &payment_data(), CALL_VALUE)).unwrap();
    assert_eq!(energy, 31_000);

    let bodies = node.bodies_for("/wallet/triggerconstantcontract");
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0],
        json!({
            "owner_address": TronAddress::from_hex(SENDER_WIRE_HEX).unwrap().to_base58(),
            "contract_address": swap_contract().to_base58(),
            "data": hex::encode(payment_data()),
            "call_value": CALL_VALUE,
            "visible": true,
        })
    );
}

/// A reverting dry run is the contract's refusal, with the node's reason; an answer without a positive energy
/// figure is a node error. No figure stands in for a missing one.
#[test]
fn a_reverting_or_empty_dry_run_is_a_typed_error() {
    let reverting = dry_run_node(json!({
        "result": { "result": true, "message": "REVERT opcode executed" },
        "energy_used": 500,
        "constant_result": [""],
    }));
    let (_ctx, coin) = block_on(trx_coin(&reverting.url()));
    let error = block_on(dry_run_energy(&coin, &swap_contract(), &payment_data(), CALL_VALUE)).unwrap_err();
    assert!(
        matches!(&error, TronSwapError::Reverted { result } if result.contains("REVERT")),
        "{error}"
    );

    for answer in [
        json!({ "result": { "result": true }, "constant_result": [""] }),
        json!({ "result": { "result": true }, "energy_used": 0, "constant_result": [""] }),
    ] {
        let node = dry_run_node(answer);
        let (_ctx, coin) = block_on(trx_coin(&node.url()));
        let error = block_on(dry_run_energy(&coin, &swap_contract(), &payment_data(), CALL_VALUE)).unwrap_err();
        assert!(matches!(error, TronSwapError::Node(_)), "{error}");
    }
}

/// The call-data form of a contract read, with the answer a public Nile node gave on 2026-11-22 to `payments` of the
/// zero id (CRD ch.21 §21.11C): a true result, 643 energy and one `constant_result` of 96 zero bytes, which is the
/// record of a payment that does not exist.
#[test]
fn a_payments_read_in_the_call_data_form_decodes_the_nodes_answer() {
    let node = dry_run_node(json!({
        "result": { "result": true },
        "energy_used": 643,
        "constant_result": ["00".repeat(96)],
        "transaction": { "ret": [{}] },
    }));
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let api = coin.tron_api.clone().expect("a TRON coin has a node client");
    let owner = TronAddress::from_hex(SENDER_WIRE_HEX).unwrap();
    let query = encode_payments_query(&[0u8; 32]).unwrap();
    assert_eq!(hex::encode(&query), format!("0716326d{}", "00".repeat(32)));

    let answer = block_on(api.trigger_constant_read_data(&owner, &swap_contract(), &query)).unwrap();
    assert_eq!(answer.energy_used, Some(643));
    let output = hex::decode(&answer.constant_result.expect("a constant result")[0]).unwrap();
    assert_eq!(decode_payments_output(&output).unwrap(), PaymentRecord {
        payment_hash: [0u8; 32],
        lock_time: 0,
        state: PaymentState::Uninitialized,
    });

    let bodies = node.bodies_for("/wallet/triggerconstantcontract");
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0],
        json!({
            "owner_address": owner.to_base58(),
            "contract_address": swap_contract().to_base58(),
            "data": hex::encode(&query),
            "call_value": 0,
            "visible": true,
        })
    );
}
