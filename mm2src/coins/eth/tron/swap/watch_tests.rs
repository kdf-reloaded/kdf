//! Tests of the watch side of a TRON V1 swap (CRD ch.21 D21.B8, R21.B5; ch.51 R88, R91, R98, R100): payment and
//! fee validation, the two-node agreement behind the refund gate, discovery of the consuming transaction, the
//! secret, and our own spend and refund.
//!
//! Each node is a loopback stub that serves a scripted chain state ([`World`]): the real coin code and the real
//! HTTP layer run, only the nodes' answers are canned. Two nodes are two different loopback hosts (`127.0.0.1`,
//! `127.0.0.2`), because the agreement counts distinct hosts.

use super::contract_abi::{encode_call, TronSwapCall};
use super::ids_tests::{addr, secret_hash, LOCK_TIME, SECRET};
use super::watch::discovery::{find_consumer, Caps, Kind};
use super::watch::{extract_secret, extract_secret_from_spend, refund_payment_with, refund_time_decision,
                   require_payment_sent, search_for_spend_with, spend_payment_with, tron_can_refund_htlc,
                   validate_fee_with, validate_payment_with, wait_for_spend_with, ClaimKind, FeeCheck, PaymentCheck,
                   Waits, PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN};
use super::{tron_payment_hash, tron_swap_id, PaymentState, TronSwapError, TronSwapTx};
use crate::eth::tron::activation::tron_coin_from_conf_and_request_with_switch;
use crate::eth::tron::api::TronApiClient;
use crate::eth::tron::nodes::TronNode;
use crate::eth::tron::proto::TaposBlockData;
use crate::eth::tron::test_node::{Reply, StubNode};
use crate::eth::tron::tx_builder::{build_trigger_smart_contract, build_trx_transfer};
use crate::eth::tron::wallet_path_tests::NOW_BLOCK_BODY;
use crate::eth::tron::{Network, TronAddress};
use crate::eth::{addr_from_raw_pubkey, EthCoin};
use crate::{BytesJson, CanRefundHtlc, CoinProtocol, FoundSwapTxSpend, TransactionEnum, TransactionErr};
use bigdecimal::BigDecimal;
use common::block_on;
use ethereum_types::U256;
use kdf_crypto::keccak256;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use prost::Message;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const NOW_MS: i64 = 1_700_000_000_000;
const BLOCK_TIME_MS: i64 = 1_700_000_100_000;
const PAYMENT_BLOCK: u64 = 100;
/// 5 TRX in SUN.
const UNITS: u64 = 5_000_000;
const FAST: Waits = Waits {
    poll_s: 0.02,
    window_ms: 400,
};

fn contract() -> TronAddress { addr(0x10) }
fn other_contract() -> TronAddress { addr(0x11) }
fn selected(address: &TronAddress) -> Option<BytesJson> { Some(address.to_evm_address().as_bytes().to_vec().into()) }
fn amount() -> BigDecimal { BigDecimal::from_str("5").unwrap() }
fn pay_id() -> [u8; 32] { tron_swap_id(LOCK_TIME, &secret_hash()) }

fn secret_key(last: u8) -> SecretKey {
    let mut bytes = [0u8; 32];
    bytes[31] = last;
    SecretKey::from_slice(&bytes).unwrap()
}

/// The compressed public key of the key `last` and its TRON address.
fn key_and_address(last: u8) -> (Vec<u8>, TronAddress) {
    let public = PublicKey::from_secret_key(&Secp256k1::new(), &secret_key(last))
        .serialize()
        .to_vec();
    let address = TronAddress::from_evm_address(addr_from_raw_pubkey(&public).unwrap());
    (public, address)
}

fn counterparty() -> (Vec<u8>, TronAddress) { key_and_address(2) }

fn tapos() -> TaposBlockData {
    TaposBlockData {
        ref_block_bytes: vec![1, 2],
        ref_block_hash: vec![3; 8],
    }
}

fn sign(raw: crate::eth::tron::proto::TransactionRaw) -> TronSwapTx { TronSwapTx::new(raw, vec![0x5a; 65]) }

fn call_tx(owner: &TronAddress, to: &TronAddress, call: &TronSwapCall, call_value: i64) -> TronSwapTx {
    sign(build_trigger_smart_contract(
        owner,
        to,
        encode_call(call).unwrap(),
        call_value,
        &tapos(),
        NOW_MS,
        NOW_MS + 60_000,
        30_000_000,
    ))
}

fn txid(tx: &TronSwapTx) -> String { format!("{:x}", tx.txid()) }

fn payment_call(receiver: TronAddress, lock_time: u64) -> TronSwapCall {
    TronSwapCall::EthPayment {
        id: pay_id(),
        receiver,
        secret_hash: secret_hash(),
        lock_time,
    }
}

/// The counterparty's payment to us as it should be.
fn good_payment(me: &TronAddress) -> TronSwapTx {
    call_tx(
        &counterparty().1,
        &contract(),
        &payment_call(me.clone(), u64::from(LOCK_TIME)),
        UNITS as i64,
    )
}

fn spend_call() -> TronSwapCall {
    TronSwapCall::ReceiverSpend {
        id: pay_id(),
        amount: U256::from(UNITS),
        secret: SECRET,
        token: None,
        sender: counterparty().1,
    }
}

fn refund_call(receiver: TronAddress) -> TronSwapCall {
    TronSwapCall::SenderRefund {
        id: pay_id(),
        amount: U256::from(UNITS),
        secret_hash: secret_hash(),
        token: None,
        receiver,
    }
}

// ---- the scripted node -------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rec {
    hash: [u8; 32],
    lock: u64,
    state: u8,
}

struct World {
    head_number: u64,
    head_time_ms: i64,
    /// `None`: the solidity endpoints fail.
    solid_time_ms: Option<i64>,
    ordinary: Option<Rec>,
    solid: Option<Rec>,
    txs: HashMap<String, TronSwapTx>,
    infos: HashMap<String, Json>,
    blocks: HashMap<u64, Vec<Json>>,
    /// What a broadcast transaction's receipt says, and the block it is in.
    broadcast_result: String,
    broadcast_block: u64,
    broadcasts: Vec<TronSwapTx>,
    /// The `/v1/contracts/{address}/events` page, `None` for a node without an event service.
    events: Option<Json>,
    /// The dry run of a call that is going to be sent reverts.
    dry_run_reverts: bool,
    /// The TRX balance, in SUN, of the coin's account, and whether the read of it fails.
    balance: i64,
    balance_down: bool,
    /// The token's `basisPointsRate()` and `maximumFee()`; `None` is a token without that getter (the call reverts).
    rate: Option<u64>,
    maximum: Option<u64>,
}

impl World {
    fn new() -> Self {
        World {
            head_number: 102,
            // The sending side refuses a head more than a minute from the local clock.
            head_time_ms: common::now_ms() as i64,
            solid_time_ms: Some(BLOCK_TIME_MS),
            ordinary: None,
            solid: None,
            txs: HashMap::new(),
            infos: HashMap::new(),
            blocks: HashMap::new(),
            broadcast_result: "SUCCESS".to_owned(),
            broadcast_block: 102,
            broadcasts: Vec::new(),
            events: None,
            dry_run_reverts: false,
            balance: 1_000_000_000_000,
            balance_down: false,
            rate: None,
            maximum: None,
        }
    }

    fn with_state(mut self, hash: [u8; 32], lock: u64, state: u8) -> Self {
        let rec = Rec { hash, lock, state };
        self.ordinary = Some(rec);
        self.solid = Some(rec);
        self
    }

    fn with_tx(mut self, tx: &TronSwapTx, block: u64, result: &str, logs: Vec<Json>) -> Self {
        let info = info_json(&txid(tx), block, result, logs);
        self.txs.insert(txid(tx), tx.clone());
        self.blocks.entry(block).or_default().push(info.clone());
        self.infos.insert(txid(tx), info);
        self
    }
}

fn info_json(id: &str, block: u64, result: &str, logs: Vec<Json>) -> Json {
    json!({ "id": id, "blockNumber": block, "blockTimeStamp": BLOCK_TIME_MS,
            "receipt": { "result": result }, "log": logs })
}

fn log_json(from: &TronAddress, signature: &str, words: &[[u8; 32]]) -> Json {
    json!({
        "address": hex::encode(from.to_evm_address().as_bytes()),
        "topics": [hex::encode(keccak256(signature.as_bytes()).as_slice())],
        "data": hex::encode(words.concat()),
    })
}

fn spent_log(from: &TronAddress, id: [u8; 32], secret: [u8; 32]) -> Json {
    log_json(from, "ReceiverSpent(bytes32,bytes32)", &[id, secret])
}

fn now_block(number: u64, time_ms: i64) -> String {
    let mut block: Json = serde_json::from_str(NOW_BLOCK_BODY).unwrap();
    block["block_header"]["raw_data"]["number"] = json!(number);
    block["block_header"]["raw_data"]["timestamp"] = json!(time_ms);
    block.to_string()
}

fn payments_reply(rec: Option<Rec>) -> Reply {
    let Some(rec) = rec else {
        return Reply::status(500, "down");
    };
    let mut out = rec.hash.to_vec();
    out.extend([0u8; 24]);
    out.extend(rec.lock.to_be_bytes());
    out.extend([0u8; 31]);
    out.push(rec.state);
    Reply::ok(
        &json!({ "result": { "result": true }, "constant_result": [hex::encode(out)], "energy_used": 643 }).to_string(),
    )
}

fn setting_reply(value: Option<u64>) -> Reply {
    match value {
        Some(value) => {
            let mut word = [0u8; 32];
            word[24..].copy_from_slice(&value.to_be_bytes());
            Reply::ok(
                &json!({ "result": { "result": true }, "constant_result": [hex::encode(word)], "energy_used": 500 })
                    .to_string(),
            )
        },
        None => Reply::ok(
            &json!({ "result": { "result": true, "message": "REVERT opcode executed" }, "energy_used": 400,
                     "constant_result": [""] })
            .to_string(),
        ),
    }
}

fn answer(world: &Mutex<World>, path: &str, body: &str) -> Reply {
    let mut w = world.lock().unwrap_or_else(|e| e.into_inner());
    let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
    let payments_call = request["data"]
        .as_str()
        .is_some_and(|data| data.starts_with("0716326d"));
    let selector_of = |signature: &str| hex::encode(&keccak256(signature.as_bytes()).as_slice()[..4]);
    let data = request["data"].as_str().unwrap_or("");
    match path {
        "/wallet/triggerconstantcontract" if data.starts_with(&selector_of("basisPointsRate()")) => {
            setting_reply(w.rate)
        },
        "/wallet/triggerconstantcontract" if data.starts_with(&selector_of("maximumFee()")) => setting_reply(w.maximum),
        "/wallet/getnowblock" => Reply::ok(&now_block(w.head_number, w.head_time_ms)),
        "/walletsolidity/getnowblock" => match w.solid_time_ms {
            Some(time) => Reply::ok(&now_block(w.head_number, time)),
            None => Reply::status(500, "down"),
        },
        "/wallet/triggerconstantcontract" if payments_call => payments_reply(w.ordinary),
        "/walletsolidity/triggerconstantcontract" => payments_reply(w.solid),
        // The dry run of a call that is going to be sent.
        "/wallet/triggerconstantcontract" if w.dry_run_reverts => Reply::ok(
            &json!({ "result": { "result": true, "message": "REVERT opcode executed" }, "energy_used": 8624,
                     "constant_result": [""] })
            .to_string(),
        ),
        "/wallet/triggerconstantcontract" => Reply::ok(
            &json!({ "result": { "result": true }, "constant_result": ["00"], "energy_used": 50_000 }).to_string(),
        ),
        "/wallet/getaccount" if w.balance_down => Reply::status(500, "down"),
        "/wallet/getaccount" => Reply::ok(&json!({ "balance": w.balance, "create_time": 1 }).to_string()),
        "/wallet/getchainparameters" => Reply::ok(
            &json!({ "chainParameter": [{ "key": "getTransactionFee", "value": 1000 },
                                        { "key": "getEnergyFee", "value": 420 }] })
            .to_string(),
        ),
        "/wallet/broadcasthex" => {
            let bytes = hex::decode(request["transaction"].as_str().unwrap_or("")).unwrap_or_default();
            let Ok(tx) = TronSwapTx::from_bytes(&bytes) else {
                return Reply::ok(&json!({ "result": false, "code": "BAD", "message": "bad" }).to_string());
            };
            let (block, result) = (w.broadcast_block, w.broadcast_result.clone());
            let info = info_json(&txid(&tx), block, &result, vec![]);
            w.infos.insert(txid(&tx), info);
            w.txs.insert(txid(&tx), tx.clone());
            w.broadcasts.push(tx.clone());
            Reply::ok(&json!({ "result": true, "txid": txid(&tx) }).to_string())
        },
        "/wallet/gettransactionbyid" => match w.txs.get(request["value"].as_str().unwrap_or("")) {
            Some(tx) => Reply::ok(
                &json!({ "txID": txid(tx), "raw_data_hex": hex::encode(tx.raw().encode_to_vec()),
                         "signature": [hex::encode(tx.signature())] })
                .to_string(),
            ),
            None => Reply::ok("{}"),
        },
        "/wallet/gettransactioninfobyid" => Reply::ok(
            &w.infos
                .get(request["value"].as_str().unwrap_or(""))
                .cloned()
                .unwrap_or(json!({}))
                .to_string(),
        ),
        "/wallet/gettransactioninfobyblocknum" => {
            let infos = w
                .blocks
                .get(&request["num"].as_u64().unwrap_or(0))
                .cloned()
                .unwrap_or_default();
            Reply::ok(&Json::Array(infos).to_string())
        },
        events if events.starts_with("/v1/contracts/") => match &w.events {
            Some(page) => Reply::ok(&page.to_string()),
            None => Reply::status(404, "no event service"),
        },
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    }
}

struct Node {
    stub: StubNode,
    world: Arc<Mutex<World>>,
}

impl Node {
    fn start(index: u8, world: World) -> Node {
        let world = Arc::new(Mutex::new(world));
        let served = world.clone();
        let stub = StubNode::start_on(&format!("127.0.0.{}", index + 1), move |path, body| {
            answer(&served, path, body)
        });
        Node { stub, world }
    }

    fn url(&self) -> String { self.stub.url() }

    fn edit(&self, edit: impl FnOnce(&mut World)) { edit(&mut self.world.lock().unwrap_or_else(|e| e.into_inner())) }
}

/// A TRX coin over the nodes, the node at `evidence` named as the evidence node when given.
///
/// The node's `swap_evidence_allow_single_host` switch is off, so that a one-node coin keeps the strict policy these
/// tests exercise.
fn coin_over(nodes: &[&Node], evidence: Option<usize>, confirmations: u64) -> (MmArc, EthCoin) {
    coin_over_in(&strict_ctx(), nodes, evidence, confirmations)
}

fn strict_ctx() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({ "swap_evidence_allow_single_host": false }))
        .into_mm_arc()
}

fn coin_over_in(ctx: &MmArc, nodes: &[&Node], evidence: Option<usize>, confirmations: u64) -> (MmArc, EthCoin) {
    let ctx = ctx.clone();
    let urls: Vec<String> = nodes.iter().map(|node| node.url()).collect();
    let mut request = json!({ "urls": urls, "required_confirmations": confirmations });
    if let Some(index) = evidence {
        request["swap_evidence_node"] = json!(urls[index]);
    }
    let mut key = [0u8; 32];
    key[31] = 1;
    let coin = block_on(tron_coin_from_conf_and_request_with_switch(
        &ctx,
        "TRX",
        &json!({}),
        &request,
        &key,
        CoinProtocol::TRX { network: Network::Nile },
        false,
    ))
    .expect("a TRX coin activates against stub nodes");
    (ctx, coin)
}

fn me(coin: &EthCoin) -> TronAddress { TronAddress::from_evm_address(coin.my_address) }

fn expected_hash(me: &TronAddress) -> [u8; 32] {
    tron_payment_hash(me, &counterparty().1, &secret_hash(), None, U256::from(UNITS))
}

/// A node that holds the payment, its receipt in `PAYMENT_BLOCK`, and the payment record in `state`.
fn payment_node(index: u8, payment: &TronSwapTx, me: &TronAddress, state: u8) -> Node {
    let mut world = World::new()
        .with_state(expected_hash(me), u64::from(LOCK_TIME), state)
        .with_tx(payment, PAYMENT_BLOCK, "SUCCESS", vec![]);
    // The solidified chain time is well before the lock time.
    world.solid_time_ms = Some((i64::from(LOCK_TIME) - 10_000) * 1000);
    Node::start(index, world)
}

/// Two nodes that agree the payment is `PaymentSent` and far from its lock time: what the revealing claim needs.
fn claim_nodes(payment: &TronSwapTx, me: &TronAddress) -> (Node, Node) {
    (payment_node(0, payment, me, 1), payment_node(1, payment, me, 1))
}

fn broadcast_count(nodes: &[&Node]) -> usize {
    nodes
        .iter()
        .map(|node| node.world.lock().unwrap().broadcasts.len())
        .sum()
}

fn reveal(coin: &EthCoin, payment: &TronSwapTx) -> Result<TronSwapTx, super::watch::OwnStepFailure> {
    block_on(spend_payment_with(
        coin,
        &payment.to_bytes(),
        &SECRET,
        &selected(&contract()),
        ClaimKind::RevealsSecret,
        FAST,
    ))
}

fn my_address_of_test_coin() -> TronAddress {
    let mut key = [0u8; 32];
    key[31] = 1;
    let public = PublicKey::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&key).unwrap()).serialize();
    TronAddress::from_evm_address(addr_from_raw_pubkey(&public).unwrap())
}

fn validate(coin: &EthCoin, payment: &TronSwapTx, selected_contract: &TronAddress) -> Result<(), TronSwapError> {
    let (public, _) = counterparty();
    let (amount, selected_contract, hash) = (amount(), selected(selected_contract), secret_hash());
    let bytes = payment.to_bytes();
    let check = PaymentCheck {
        payment_tx: &bytes,
        time_lock: LOCK_TIME,
        sender_pub: &public,
        secret_hash: &hash,
        amount: &amount,
        swap_contract: &selected_contract,
        confirmations: 1,
    };
    block_on(validate_payment_with(coin, &check, FAST))
}

fn assert_mismatch(result: Result<(), TronSwapError>, expected: &str) {
    match result {
        Err(TronSwapError::Mismatch { field }) => assert_eq!(field, expected),
        other => panic!("expected a mismatch of {expected}, got {other:?}"),
    }
}

// ---- validation (A2, A3) -----------------------------------------------------------------------------------------

#[test]
fn a_matching_payment_validates_on_the_agreed_read() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    validate(&coin, &payment, &contract()).expect("the payment validates");
}

#[test]
fn every_field_mismatch_is_named() {
    let me = my_address_of_test_coin();
    let cp = counterparty().1;
    let cases: Vec<(&str, TronSwapTx)> = vec![
        (
            "amount",
            call_tx(
                &cp,
                &contract(),
                &payment_call(me.clone(), u64::from(LOCK_TIME)),
                UNITS as i64 + 1,
            ),
        ),
        (
            "receiver",
            call_tx(
                &cp,
                &contract(),
                &payment_call(addr(0x77), u64::from(LOCK_TIME)),
                UNITS as i64,
            ),
        ),
        (
            "sender",
            call_tx(
                &addr(0x66),
                &contract(),
                &payment_call(me.clone(), u64::from(LOCK_TIME)),
                UNITS as i64,
            ),
        ),
        (
            "lock time",
            call_tx(
                &cp,
                &contract(),
                &payment_call(me.clone(), u64::from(LOCK_TIME) + 1),
                UNITS as i64,
            ),
        ),
        (
            "token",
            call_tx(
                &cp,
                &contract(),
                &TronSwapCall::Erc20Payment {
                    id: pay_id(),
                    amount: U256::from(UNITS),
                    token: addr(0x44),
                    receiver: me.clone(),
                    secret_hash: secret_hash(),
                    lock_time: u64::from(LOCK_TIME),
                },
                0,
            ),
        ),
        ("method", call_tx(&cp, &contract(), &spend_call(), 0)),
        (
            "id",
            call_tx(
                &cp,
                &contract(),
                &TronSwapCall::EthPayment {
                    id: [0x99; 32],
                    receiver: me.clone(),
                    secret_hash: secret_hash(),
                    lock_time: u64::from(LOCK_TIME),
                },
                UNITS as i64,
            ),
        ),
    ];
    for (field, payment) in cases {
        let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
        let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
        assert_mismatch(validate(&coin, &payment, &contract()), field);
    }
}

#[test]
fn the_payment_must_call_the_selected_contract_not_the_primary_one() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    // The swap negotiated the fallback contract; the payment went to the other one.
    assert_mismatch(validate(&coin, &payment, &other_contract()), "swap contract");
}

#[test]
fn a_state_other_than_payment_sent_and_a_different_stored_hash_are_refused() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 2), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_mismatch(validate(&coin, &payment, &contract()), "payment state");

    let world = |index| {
        Node::start(
            index,
            World::new().with_state([0xee; 32], u64::from(LOCK_TIME), 1).with_tx(
                &payment,
                PAYMENT_BLOCK,
                "SUCCESS",
                vec![],
            ),
        )
    };
    let (a, b) = (world(0), world(1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_mismatch(validate(&coin, &payment, &contract()), "payment hash");
}

/// The payment hash does not cover the lock time: a payment whose call decodes with the agreed terms and whose
/// stored hash is the expected one is still refused when the stored lock time is not the agreed one.
#[test]
fn a_stored_lock_time_other_than_the_agreed_one_is_refused() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let node = |index| {
        Node::start(
            index,
            World::new()
                .with_state(expected_hash(&me), u64::from(LOCK_TIME) - 3_600, 1)
                .with_tx(&payment, PAYMENT_BLOCK, "SUCCESS", vec![]),
        )
    };
    let (a, b) = (node(0), node(1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_mismatch(validate(&coin, &payment, &contract()), "lock time");
}

#[test]
fn a_receipt_that_is_not_a_success_is_reverted() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let world = |index| {
        Node::start(
            index,
            World::new()
                .with_state(expected_hash(&me), u64::from(LOCK_TIME), 1)
                .with_tx(&payment, PAYMENT_BLOCK, "REVERT", vec![]),
        )
    };
    let (a, b) = (world(0), world(1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(
        validate(&coin, &payment, &contract()),
        Err(TronSwapError::Reverted { .. })
    ));
}

#[test]
fn the_deciding_evidence_is_the_nodes_not_the_peers_bytes() {
    // The peer's bytes name the id of a payment whose node-held call is another amount: the node's call is read.
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let lying = call_tx(
        &counterparty().1,
        &contract(),
        &payment_call(me.clone(), u64::from(LOCK_TIME)),
        1,
    );
    // Both transactions are different ids: the lying one is unknown to the nodes, so nothing validates.
    assert!(validate(&coin, &lying, &contract()).is_err());
}

#[test]
fn the_dex_fee_is_a_transfer_to_the_fee_address() {
    let me = my_address_of_test_coin();
    let (sender_pub, sender) = counterparty();
    let (fee_pub, fee_to) = key_and_address(3);
    let fee_tx = |to: &TronAddress, units: i64| {
        sign(build_trx_transfer(
            &sender,
            to,
            units,
            &tapos(),
            NOW_MS,
            NOW_MS + 60_000,
        ))
    };
    let fee = fee_tx(&fee_to, 1_000_000);
    let node = Node::start(0, World::new().with_tx(&fee, 101, "SUCCESS", vec![]));
    let _ = &me;
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let amount = BigDecimal::from_str("1").unwrap();
    let run = |tx: &TronSwapTx, amount: &BigDecimal, min_block: u64| {
        block_on(validate_fee_with(
            &coin,
            &FeeCheck {
                fee_tx: tx,
                sender_pub: &sender_pub,
                fee_addr_pub: &fee_pub,
                amount,
                min_block_number: min_block,
            },
            FAST,
        ))
    };
    run(&fee, &amount, 100).expect("the fee validates");
    assert_mismatch(run(&fee, &BigDecimal::from_str("2").unwrap(), 100), "amount");
    assert_mismatch(run(&fee, &amount, 200), "block number");
    // Another recipient: the node holds that transaction too.
    let wrong = fee_tx(&addr(0x77), 1_000_000);
    node.edit(|w| *w = std::mem::replace(w, World::new()).with_tx(&wrong, 101, "SUCCESS", vec![]));
    assert_mismatch(run(&wrong, &amount, 100), "recipient");
}

// ---- R100 / T42 over two scripted nodes and the refund gate (A1, A5) -----------------------------------------

#[test]
fn the_refund_decisions_are_pure() {
    let lock = 1_000;
    assert!(refund_time_decision(lock, lock).is_ok());
    assert!(matches!(
        refund_time_decision(lock - 1, lock),
        Err(TronSwapError::TooEarly { .. })
    ));
    assert!(require_payment_sent(PaymentState::PaymentSent).is_ok());
    for state in [
        PaymentState::ReceiverSpent,
        PaymentState::SenderRefunded,
        PaymentState::Uninitialized,
    ] {
        assert!(matches!(
            require_payment_sent(state),
            Err(TronSwapError::Mismatch { .. })
        ));
    }
}

fn gate_node(index: u8, chain_time_s: i64, state: u8) -> Node {
    let lock = i64::from(LOCK_TIME);
    let _ = lock;
    Node::start(index, {
        let mut world = World::new().with_state([0; 32], u64::from(LOCK_TIME), state);
        world.solid_time_ms = Some(chain_time_s * 1000);
        world
    })
}

fn gate(coin: &EthCoin) -> Result<CanRefundHtlc, TronSwapError> {
    block_on(tron_can_refund_htlc(coin, u64::from(LOCK_TIME)))
}

#[test]
fn agreeing_chain_times_past_the_lock_allow_a_refund_even_when_they_differ() {
    let lock = i64::from(LOCK_TIME);
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 500, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(gate(&coin), Ok(CanRefundHtlc::CanRefundNow)));
}

#[test]
fn agreeing_chain_times_before_the_lock_wait() {
    let lock = i64::from(LOCK_TIME);
    let (a, b) = (gate_node(0, lock - 100, 1), gate_node(1, lock - 40, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    match gate(&coin) {
        Ok(CanRefundHtlc::HaveToWait(seconds)) => assert!((41..=101).contains(&seconds), "{seconds}"),
        other => panic!("expected a wait, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn differing_answers_leave_the_step_waiting() {
    let lock = i64::from(LOCK_TIME);
    // One node is past the lock, the other is not.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock - 5, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(gate(&coin), Err(TronSwapError::Undetermined(_))));
}

#[test]
fn a_single_answering_node_leaves_the_step_waiting() {
    let lock = i64::from(LOCK_TIME);
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    b.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(gate(&coin).is_err());
    // A coin with a single node has no second answer at all.
    let (_ctx, coin) = coin_over(&[&a], None, 1);
    assert!(gate(&coin).is_err());
}

/// A node that holds our own payment (to the counterparty) in `PAYMENT_BLOCK` and says `state` for it.
fn own_payment_node(index: u8, own: &TronSwapTx, state: u8) -> Node {
    Node::start(
        index,
        World::new()
            .with_state([0; 32], u64::from(LOCK_TIME), state)
            .with_tx(own, PAYMENT_BLOCK, "SUCCESS", vec![]),
    )
}

fn own_payment(me: &TronAddress) -> TronSwapTx {
    call_tx(
        me,
        &contract(),
        &payment_call(counterparty().1, u64::from(LOCK_TIME)),
        UNITS as i64,
    )
}

#[test]
fn a_refund_is_never_sent_unless_the_agreed_state_is_payment_sent() {
    let me = my_address_of_test_coin();
    let own = own_payment(&me);
    let refund = |nodes: &[&Node]| {
        let (_ctx, coin) = coin_over(nodes, None, 1);
        block_on(refund_payment_with(
            &coin,
            &own.to_bytes(),
            &secret_hash(),
            &selected(&contract()),
            FAST,
        ))
    };
    // Spent, refunded or uninitialized on both nodes.
    for state in [2, 3, 0] {
        let (a, b) = (own_payment_node(0, &own, state), own_payment_node(1, &own, state));
        let failure = refund(&[&a, &b]).expect_err("no refund");
        assert!(matches!(failure.error, TronSwapError::Mismatch { .. }), "state {state}");
        assert!(failure.tx.is_none());
        assert!(a.world.lock().unwrap().broadcasts.is_empty());
    }
    // The nodes disagree: undetermined, nothing is signed.
    let (a, b) = (own_payment_node(0, &own, 1), own_payment_node(1, &own, 2));
    let failure = refund(&[&a, &b]).expect_err("no refund");
    assert!(matches!(failure.error, TronSwapError::Undetermined(_)));
    assert!(a.world.lock().unwrap().broadcasts.is_empty() && b.world.lock().unwrap().broadcasts.is_empty());
    // A single node: no second answer.
    let a = own_payment_node(0, &own, 1);
    assert!(refund(&[&a]).is_err());
    assert!(a.world.lock().unwrap().broadcasts.is_empty());
}

#[test]
fn the_evidence_node_alone_is_taken_and_a_silent_one_is_not_replaced() {
    let lock = i64::from(LOCK_TIME);
    // The evidence node says past the lock; the other node disagrees and is not asked to outvote it.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock - 5, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], Some(0), 1);
    assert!(matches!(gate(&coin), Ok(CanRefundHtlc::CanRefundNow)));
    // The evidence node is silent: the other node, which is fine, does not stand in for it.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    a.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a, &b], Some(0), 1);
    assert!(gate(&coin).is_err());
}

/// The single-host switch on (the default): a TRX coin over one node is relaxed to `Trusted` on that node, and the
/// warning of the relaxation names the ticker and no host.
#[test]
fn a_one_node_coin_is_relaxed_to_trusted_when_the_switch_is_on() {
    let node = Node::start(0, World::new());
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let (ctx, coin) = coin_over_in(&ctx, &[&node], None, 1);
    assert_eq!(
        crate::MmCoin::swap_evidence_policy(&coin),
        crate::SwapEvidencePolicy::Trusted { node: node.url() }
    );
    assert!(crate::swap_evidence::single_host_relaxed(&ctx, "TRX"));
    assert_eq!(crate::MmCoin::swap_evidence_refusal(&coin), None);
    let warning = crate::swap_evidence::single_host_warning("TRX");
    assert!(
        warning.contains("TRX") && !warning.contains(&node.url()) && !warning.contains("127.0.0"),
        "{warning}"
    );
    // the switch off: the strict policy
    let (ctx, strict) = coin_over(&[&node], None, 1);
    assert_eq!(
        crate::MmCoin::swap_evidence_policy(&strict),
        crate::SwapEvidencePolicy::Insufficient { hosts: 1 }
    );
    assert!(!crate::swap_evidence::single_host_relaxed(&ctx, "TRX"));
}

#[test]
fn an_evidence_node_outside_the_node_list_is_refused_at_activation() {
    let node = Node::start(0, World::new());
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let request = json!({ "urls": [node.url()], "swap_evidence_node": "http://127.0.0.9:1" });
    let mut key = [0u8; 32];
    key[31] = 1;
    let result = block_on(tron_coin_from_conf_and_request_with_switch(
        &ctx,
        "TRX",
        &json!({}),
        &request,
        &key,
        CoinProtocol::TRX { network: Network::Nile },
        false,
    ));
    assert!(result.is_err());
}

// ---- the secret (A6) ---------------------------------------------------------------------------------------------

#[test]
fn a_secret_is_used_only_when_its_hash_matches() {
    let me = my_address_of_test_coin();
    let spend = call_tx(&me, &contract(), &spend_call(), 0);
    assert_eq!(
        extract_secret(&secret_hash(), &spend.to_bytes()).unwrap(),
        SECRET.to_vec()
    );
    // The same call against another secret hash: refused.
    let other_hash = [0xab; 32];
    assert!(extract_secret(&other_hash, &spend.to_bytes()).is_err());
    // A call of another method has no secret.
    let refund = call_tx(&me, &contract(), &refund_call(addr(0x30)), 0);
    assert!(extract_secret(&secret_hash(), &refund.to_bytes()).is_err());
}

#[test]
fn the_secret_of_the_event_is_taken_from_one_answer_and_checked_against_the_hash() {
    let me = my_address_of_test_coin();
    // A spend whose call carries another secret than the event: only the event's value can match.
    let call = TronSwapCall::ReceiverSpend {
        id: pay_id(),
        amount: U256::from(UNITS),
        secret: [0x01; 32],
        token: None,
        sender: counterparty().1,
    };
    let spend = call_tx(&me, &contract(), &call, 0);
    let with_logs = |secret: [u8; 32]| {
        Node::start(
            0,
            World::new().with_tx(&spend, 101, "SUCCESS", vec![spent_log(&contract(), pay_id(), secret)]),
        )
    };
    let good = with_logs(SECRET);
    let (_ctx, coin) = coin_over(&[&good], None, 1);
    let secret = block_on(extract_secret_from_spend(&coin, &secret_hash(), &spend.to_bytes())).unwrap();
    assert_eq!(secret, SECRET.to_vec());
    let bad = with_logs([0x02; 32]);
    let (_ctx, coin) = coin_over(&[&bad], None, 1);
    assert!(block_on(extract_secret_from_spend(&coin, &secret_hash(), &spend.to_bytes())).is_err());
}

// ---- discovery (A6) ----------------------------------------------------------------------------------------------

/// A node whose chain has the payment in block 100, the spend candidate `spend` in block 101 and a head of 102.
fn discovery_node(index: u8, me: &TronAddress, payment: &TronSwapTx, candidates: &[(&TronSwapTx, &str, Json)]) -> Node {
    let mut world = World::new()
        .with_state(expected_hash(me), u64::from(LOCK_TIME), 2)
        .with_tx(payment, PAYMENT_BLOCK, "SUCCESS", vec![]);
    for (tx, result, log) in candidates {
        world = world.with_tx(tx, 101, result, vec![log.clone()]);
    }
    Node::start(index, world)
}

const CAPS: Caps = Caps {
    event_pages: 3,
    walk_blocks: 50,
};

fn search(coin: &EthCoin, payment: &TronSwapTx, caps: Caps) -> Result<Option<FoundSwapTxSpend>, TronSwapError> {
    block_on(search_for_spend_with(
        coin,
        &payment.to_bytes(),
        &selected(&contract()),
        caps,
    ))
}

fn the_spend(me: &TronAddress) -> TronSwapTx { call_tx(me, &contract(), &spend_call(), 0) }

#[test]
fn a_spent_payment_is_found_by_the_block_walk_when_the_event_service_is_down() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let spend = the_spend(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    let (a, b) = (
        discovery_node(0, &me, &payment, &[(&spend, "SUCCESS", log.clone())]),
        discovery_node(1, &me, &payment, &[(&spend, "SUCCESS", log)]),
    );
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    match search(&coin, &payment, CAPS) {
        Ok(Some(FoundSwapTxSpend::Spent(TransactionEnum::TronTransaction(found)))) => assert_eq!(found, spend),
        other => panic!("expected the spend, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_payment_that_is_still_locked_is_not_spent_only_on_the_agreed_state() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(search(&coin, &payment, CAPS), Ok(None)));
    // One node says spent: undetermined, never "not spent".
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(
        search(&coin, &payment, CAPS),
        Err(TronSwapError::Undetermined(_))
    ));
}

#[test]
fn candidates_that_are_not_the_consuming_call_are_skipped() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    // A failed transaction, another id, another contract, another method: each with a matching-looking log.
    let failed = call_tx(&me, &contract(), &spend_call(), 1);
    let other_id = call_tx(
        &me,
        &contract(),
        &TronSwapCall::ReceiverSpend {
            id: [0x42; 32],
            amount: U256::from(UNITS),
            secret: SECRET,
            token: None,
            sender: counterparty().1,
        },
        0,
    );
    let other_contract_tx = call_tx(&me, &other_contract(), &spend_call(), 0);
    let other_method = call_tx(&me, &contract(), &refund_call(addr(0x30)), 0);
    let candidates = [
        (&failed, "REVERT", log.clone()),
        (&other_id, "SUCCESS", log.clone()),
        (&other_contract_tx, "SUCCESS", log.clone()),
        (&other_method, "SUCCESS", log),
    ];
    let (a, b) = (
        discovery_node(0, &me, &payment, &candidates),
        discovery_node(1, &me, &payment, &candidates),
    );
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    // The state says spent and nothing consumed it: an error, not "not spent".
    assert!(matches!(search(&coin, &payment, CAPS), Err(TronSwapError::NotYetFinal)));
}

#[test]
fn both_sources_down_is_a_typed_error() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (
        discovery_node(0, &me, &payment, &[]),
        discovery_node(1, &me, &payment, &[]),
    );
    for node in [&a, &b] {
        node.edit(|w| w.head_number = 0);
    }
    // No head above the payment block and no events: the walk has nothing to read and finds nothing.
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(search(&coin, &payment, CAPS).is_err());
}

#[test]
fn a_cap_that_is_reached_warns_and_is_a_typed_error() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (
        discovery_node(0, &me, &payment, &[]),
        discovery_node(1, &me, &payment, &[]),
    );
    for node in [&a, &b] {
        node.edit(|w| w.head_number = PAYMENT_BLOCK + 1_000);
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let caps = Caps {
        event_pages: 3,
        walk_blocks: 5,
    };
    assert!(matches!(
        search(&coin, &payment, caps),
        Err(TronSwapError::SearchCapped { source: "block walk" })
    ));
}

#[test]
fn the_event_listing_finds_the_consumer_when_the_service_answers() {
    let me = my_address_of_test_coin();
    let spend = the_spend(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    let page = json!({ "success": true, "data": [
        { "transaction_id": "00".repeat(32), "event_name": "ReceiverSpent", "result": { "id": hex::encode([0x42u8; 32]) } },
        { "transaction_id": txid(&spend), "event_name": "ReceiverSpent", "result": { "id": hex::encode(pay_id()) } },
    ], "meta": {} });
    let node = Node::start(0, {
        let mut world = World::new().with_tx(&spend, 101, "SUCCESS", vec![log]);
        world.events = Some(page);
        world
    });
    let api = TronApiClient::new_with_history_nodes("TRX", vec![TronNode::plain(node.url())], &[true], 5.0);
    let found = block_on(find_consumer(
        &api,
        &contract(),
        &pay_id(),
        Kind::Spend,
        PAYMENT_BLOCK,
        Some(BLOCK_TIME_MS),
        CAPS,
    ))
    .expect("the listing names the spend");
    assert_eq!(found, spend);
}

#[test]
fn the_event_listing_cap_is_a_typed_error() {
    let page = json!({ "success": true, "data": [], "meta": { "fingerprint": "next" } });
    let node = Node::start(0, {
        let mut world = World::new();
        world.events = Some(page);
        world
    });
    let api = TronApiClient::new_with_history_nodes("TRX", vec![TronNode::plain(node.url())], &[true], 5.0);
    let result = block_on(find_consumer(
        &api,
        &contract(),
        &pay_id(),
        Kind::Spend,
        PAYMENT_BLOCK,
        Some(BLOCK_TIME_MS),
        CAPS,
    ));
    assert!(matches!(
        result,
        Err(TronSwapError::SearchCapped {
            source: "contract event listing"
        })
    ));
}

#[test]
fn the_wait_keeps_polling_past_a_failed_candidate() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    let failed = call_tx(&me, &contract(), &spend_call(), 1);
    let good = the_spend(&me);
    let node = discovery_node(0, &me, &payment, &[(&failed, "REVERT", log.clone())]);
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let world = node.world.clone();
    let late = good.clone();
    let helper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut w = world.lock().unwrap_or_else(|e| e.into_inner());
        let info = info_json(&txid(&late), 101, "SUCCESS", vec![log]);
        w.txs.insert(txid(&late), late.clone());
        w.blocks.entry(101).or_default().push(info.clone());
        w.infos.insert(txid(&late), info);
    });
    let wait_until = common::now_ms() / 1000 + 10;
    let found = block_on(wait_for_spend_with(
        &coin,
        &payment.to_bytes(),
        wait_until,
        &selected(&contract()),
        0.05,
        CAPS,
    ))
    .expect("the later valid spend is found");
    helper.join().unwrap();
    assert_eq!(found, good);
}

// ---- our own spend and refund (A4) -------------------------------------------------------------------------------

#[test]
fn our_spend_counts_after_a_successful_receipt_and_goes_to_the_selected_contract() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = claim_nodes(&payment, &me);
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let tx = reveal(&coin, &payment).expect("the spend is confirmed");
    let (to, _, call) = super::call_of(&tx).unwrap();
    assert_eq!(to, contract());
    assert!(matches!(call, TronSwapCall::ReceiverSpend { .. }));
    assert_eq!(broadcast_count(&[&a, &b]), 1);
}

/// Each precondition of the revealing claim, in order: the agreed state, the margin before the lock, the dry run,
/// the balance. A failure signs and sends nothing, and the error is plain.
fn assert_not_sent(result: Result<TronSwapTx, super::watch::OwnStepFailure>, nodes: &[&Node]) -> String {
    let failure = result.expect_err("a failed precondition sends nothing");
    assert!(failure.tx.is_none());
    let error = {
        let text = failure.error.to_string();
        let plain = TransactionErr::from(failure);
        assert!(matches!(plain, TransactionErr::Plain(ref shown) if *shown == text));
        text
    };
    assert_eq!(broadcast_count(nodes), 0, "{error}");
    error
}

#[test]
fn a_revealing_claim_is_not_sent_unless_the_agreed_state_is_payment_sent() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    for state in [2, 3] {
        let (a, b) = (
            payment_node(0, &payment, &me, state),
            payment_node(1, &payment, &me, state),
        );
        let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
        let failure = reveal(&coin, &payment).expect_err("no claim");
        assert!(
            matches!(failure.error, TronSwapError::Mismatch { field: "payment state" }),
            "state {state}"
        );
        assert_eq!(broadcast_count(&[&a, &b]), 0);
    }
    // The nodes disagree: undetermined.
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(
        reveal(&coin, &payment).expect_err("no claim").error,
        TronSwapError::Undetermined(_)
    ));
}

#[test]
fn a_revealing_claim_needs_the_margin_before_the_lock_time() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let lock = i64::from(LOCK_TIME);
    // Exactly the margin before the lock: not below it, so no claim; one second more: the claim goes.
    for (before_lock_s, sent) in [(299, false), (300, false), (301, true)] {
        let (a, b) = claim_nodes(&payment, &me);
        for node in [&a, &b] {
            node.edit(|w| w.solid_time_ms = Some((lock - before_lock_s) * 1000));
        }
        let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
        let result = reveal(&coin, &payment);
        if sent {
            result.expect("sent with the margin");
        } else {
            let failure = result.expect_err("no claim inside the margin");
            assert!(matches!(failure.error, TronSwapError::Mismatch {
                field: "lock margin"
            }));
            assert!(failure.tx.is_none());
            assert_eq!(broadcast_count(&[&a, &b]), 0);
        }
    }
}

#[test]
fn a_revealing_claim_needs_the_balance_to_cover_the_fee_limit() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = claim_nodes(&payment, &me);
    for node in [&a, &b] {
        node.edit(|w| w.balance = 1_000_000);
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let error = assert_not_sent(reveal(&coin, &payment), &[&a, &b]);
    assert!(error.contains("balance"), "{error}");
    // A failed balance read is an error, not a pass.
    let (a, b) = claim_nodes(&payment, &me);
    for node in [&a, &b] {
        node.edit(|w| w.balance_down = true);
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_not_sent(reveal(&coin, &payment), &[&a, &b]);
}

/// The claim is sent only after its dry run succeeded: a reverting dry run sends nothing, keeps no transaction
/// (so the error is not recoverable), and the secret is in no request but the dry run's own.
#[test]
fn a_revealing_claim_whose_dry_run_reverts_is_not_signed_or_sent() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = claim_nodes(&payment, &me);
    for node in [&a, &b] {
        node.edit(|w| w.dry_run_reverts = true);
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let failure = reveal(&coin, &payment).expect_err("a reverting dry run sends nothing");
    assert!(matches!(failure.error, TronSwapError::Reverted { .. }));
    assert!(failure.tx.is_none());
    assert!(matches!(TransactionErr::from(failure), TransactionErr::Plain(_)));
    assert_eq!(broadcast_count(&[&a, &b]), 0);
    let secret_hex = hex::encode(SECRET);
    for node in [&a, &b] {
        let requests = node.stub.requests();
        assert!(requests.iter().all(|(path, _)| path != "/wallet/broadcasthex"));
        for (path, body) in requests {
            if body.contains(&secret_hex) {
                assert_eq!(path, "/wallet/triggerconstantcontract", "the secret went to {path}");
            }
        }
    }
}

#[test]
fn a_secret_that_does_not_open_the_hash_is_never_sent() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let node = payment_node(0, &payment, &me, 1);
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let failure = block_on(spend_payment_with(
        &coin,
        &payment.to_bytes(),
        &[0x09; 32],
        &selected(&contract()),
        ClaimKind::RevealsSecret,
        FAST,
    ))
    .expect_err("refused");
    assert!(failure.tx.is_none());
    assert!(node.world.lock().unwrap().broadcasts.is_empty());
}

#[test]
fn a_reverted_receipt_of_our_spend_is_reverted_and_keeps_the_transaction() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = claim_nodes(&payment, &me);
    for node in [&a, &b] {
        node.edit(|w| w.broadcast_result = "REVERT".to_owned());
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let failure = reveal(&coin, &payment).expect_err("a reverted spend");
    assert!(matches!(failure.error, TronSwapError::Reverted { .. }));
    assert!(failure.tx.is_some());
}

#[test]
fn a_spend_that_is_not_yet_at_depth_is_not_counted() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = claim_nodes(&payment, &me);
    // Five confirmations are required; the receipt's block is the head.
    let (_ctx, coin) = coin_over(&[&a, &b], None, 5);
    let failure = reveal(&coin, &payment).expect_err("not deep enough");
    assert!(matches!(failure.error, TronSwapError::NotYetFinal));
    assert!(failure.tx.is_some());
}

#[test]
fn our_refund_goes_to_the_selected_contract_with_our_own_payment() {
    let me = my_address_of_test_coin();
    let own = own_payment(&me);
    let (a, b) = (own_payment_node(0, &own, 1), own_payment_node(1, &own, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let tx = block_on(refund_payment_with(
        &coin,
        &own.to_bytes(),
        &secret_hash(),
        &selected(&contract()),
        FAST,
    ))
    .expect("the refund is confirmed");
    let (to, _, call) = super::call_of(&tx).unwrap();
    assert_eq!(to, contract());
    assert!(matches!(call, TronSwapCall::SenderRefund { .. }));
}

// ---- the claim whose secret is public (taker claims the maker payment) -----------------------------------------

fn public_claim(coin: &EthCoin, payment: &TronSwapTx) -> Result<TronSwapTx, super::watch::OwnStepFailure> {
    block_on(spend_payment_with(
        coin,
        &payment.to_bytes(),
        &SECRET,
        &selected(&contract()),
        ClaimKind::SecretPublic,
        FAST,
    ))
}

#[test]
fn a_public_secret_claim_is_sent_despite_a_reverting_dry_run_with_the_fallback_fee_limit() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let node = payment_node(0, &payment, &me, 1);
    node.edit(|w| w.dry_run_reverts = true);
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let tx = public_claim(&coin, &payment).expect("the claim is sent anyway");
    assert_eq!(tx.raw().fee_limit, PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN as i64);
    let sent = node.world.lock().unwrap().broadcasts.clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].raw().fee_limit,
        PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN as i64
    );
}

#[test]
fn a_public_secret_claim_has_no_time_gate() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let node = payment_node(0, &payment, &me, 1);
    // The chain time is long past the lock time.
    node.edit(|w| {
        w.solid_time_ms = Some((i64::from(LOCK_TIME) + 100_000) * 1000);
    });
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    public_claim(&coin, &payment).expect("sent past the lock time");
}

#[test]
fn a_public_secret_claim_is_not_sent_when_the_payment_is_refunded_or_spent() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    for state in [3, 2] {
        let node = payment_node(0, &payment, &me, state);
        let (_ctx, coin) = coin_over(&[&node], None, 1);
        let failure = public_claim(&coin, &payment).expect_err("no send");
        assert!(matches!(failure.error, TronSwapError::Mismatch { .. }), "state {state}");
        assert!(node.world.lock().unwrap().broadcasts.is_empty());
    }
}

#[test]
fn a_failed_state_read_does_not_block_a_public_secret_claim() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let node = payment_node(0, &payment, &me, 1);
    node.edit(|w| w.ordinary = None);
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    public_claim(&coin, &payment).expect("sent although the read failed");
}

// ---- the search window of the wait, and the transaction id of a validated payment ---------------------------------

/// The wait remembers the last read that showed the payment unspent and searches from there: the claim lies beyond
/// the walk cap counted from the payment and within it counted from that read.
#[test]
fn the_wait_searches_from_the_last_unspent_read_not_from_the_payment() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let spend = the_spend(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    let node = Node::start(0, {
        let mut world = World::new()
            .with_state(expected_hash(&me), u64::from(LOCK_TIME), 1)
            .with_tx(&payment, PAYMENT_BLOCK, "SUCCESS", vec![])
            .with_tx(&spend, 505, "SUCCESS", vec![log]);
        world.head_number = 500;
        world
    });
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let world = node.world.clone();
    let helper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let mut w = world.lock().unwrap_or_else(|e| e.into_inner());
        w.head_number = 510;
        w.ordinary = Some(Rec {
            hash: expected_hash(&my_address_of_test_coin()),
            lock: u64::from(LOCK_TIME),
            state: 2,
        });
    });
    let caps = Caps {
        event_pages: 3,
        walk_blocks: 20,
    };
    let wait_until = common::now_ms() / 1000 + 10;
    let found = block_on(wait_for_spend_with(
        &coin,
        &payment.to_bytes(),
        wait_until,
        &selected(&contract()),
        0.05,
        caps,
    ))
    .expect("the claim is found from the last unspent read");
    helper.join().unwrap();
    assert_eq!(found, spend);
    // Counted from the payment (block 100) the claim at block 505 is far beyond the cap of 20 blocks.
    assert!(505 - PAYMENT_BLOCK > caps.walk_blocks);
}

#[test]
fn a_transaction_the_node_returns_under_another_id_is_refused() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let other = call_tx(
        &counterparty().1,
        &contract(),
        &payment_call(me.clone(), u64::from(LOCK_TIME)),
        UNITS as i64 + 7,
    );
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    for node in [&a, &b] {
        node.edit(|w| {
            w.txs.insert(txid(&payment), other.clone());
        });
    }
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_mismatch(validate(&coin, &payment, &contract()), "transaction id");
}

/// The node that gave the state lagged the node that gave the head: the claim lies one block before the start
/// recorded from the last unspent read, and the search from the payment finds it.
#[test]
fn the_wait_falls_back_to_the_payment_when_the_claim_lies_before_the_recorded_start() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let spend = the_spend(&me);
    let log = spent_log(&contract(), pay_id(), SECRET);
    let node = Node::start(0, {
        let mut world = World::new()
            .with_state(expected_hash(&me), u64::from(LOCK_TIME), 1)
            .with_tx(&payment, 497, "SUCCESS", vec![])
            // The recorded start is block 499 (head 500, minus the one-block overlap); the claim is at 498.
            .with_tx(&spend, 498, "SUCCESS", vec![log]);
        world.head_number = 500;
        world
    });
    let (_ctx, coin) = coin_over(&[&node], None, 1);
    let world = node.world.clone();
    let helper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let mut w = world.lock().unwrap_or_else(|e| e.into_inner());
        w.head_number = 510;
        w.ordinary = Some(Rec {
            hash: expected_hash(&my_address_of_test_coin()),
            lock: u64::from(LOCK_TIME),
            state: 2,
        });
    });
    let caps = Caps {
        event_pages: 3,
        walk_blocks: 20,
    };
    let wait_until = common::now_ms() / 1000 + 10;
    let found = block_on(wait_for_spend_with(
        &coin,
        &payment.to_bytes(),
        wait_until,
        &selected(&contract()),
        0.05,
        caps,
    ))
    .expect("the claim before the recorded start is found from the payment");
    helper.join().unwrap();
    assert_eq!(found, spend);
}

// ---- the refund gate through `can_refund_htlc` ---------------------------------------------------------------------

fn trait_gate(coin: &EthCoin, lock: u64) -> Result<CanRefundHtlc, String> {
    use crate::SwapOps;
    use futures::compat::Future01CompatExt;
    block_on(coin.can_refund_htlc(lock).compat())
}

#[test]
fn can_refund_htlc_of_a_tron_coin_waits_before_the_lock_and_refunds_after_it_on_two_nodes() {
    let lock = i64::from(LOCK_TIME);
    let (a, b) = (gate_node(0, lock - 100, 1), gate_node(1, lock - 100, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(
        trait_gate(&coin, u64::from(LOCK_TIME)),
        Ok(CanRefundHtlc::HaveToWait(101))
    ));
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 9, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(
        trait_gate(&coin, u64::from(LOCK_TIME)),
        Ok(CanRefundHtlc::CanRefundNow)
    ));
}

#[test]
fn can_refund_htlc_of_a_tron_coin_is_an_error_when_the_evidence_is_undetermined() {
    let lock = i64::from(LOCK_TIME);
    // The nodes disagree.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock - 5, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(trait_gate(&coin, u64::from(LOCK_TIME)).is_err());
    // One node does not answer.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    b.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(trait_gate(&coin, u64::from(LOCK_TIME)).is_err());
}

// ---- a TRC20 leg and the token's transfer-fee setting ----------------------------------------------------------

fn trc20_payment_validation(rate: Option<u64>, maximum: Option<u64>) -> Result<(), TronSwapError> {
    let token = TronAddress::parse(crate::eth::tron::wallet_path_tests::TRC20_CONTRACT).unwrap();
    let (_, cp) = counterparty();
    let me = my_address_of_test_coin();
    let payment = call_tx(
        &cp,
        &contract(),
        &TronSwapCall::Erc20Payment {
            id: pay_id(),
            amount: U256::from(UNITS),
            token: token.clone(),
            receiver: me.clone(),
            secret_hash: secret_hash(),
            lock_time: u64::from(LOCK_TIME),
        },
        0,
    );
    let hash = tron_payment_hash(&me, &cp, &secret_hash(), Some(&token), U256::from(UNITS));
    let node = |index| {
        let mut world =
            World::new()
                .with_state(hash, u64::from(LOCK_TIME), 1)
                .with_tx(&payment, PAYMENT_BLOCK, "SUCCESS", vec![]);
        world.rate = rate;
        world.maximum = maximum;
        Node::start(index, world)
    };
    let (a, b) = (node(0), node(1));
    let urls = [a.url(), b.url()];
    let (_ctx, coin) = block_on(crate::eth::tron::wallet_path_tests::trc20_coin_over(
        &[urls[0].as_str(), urls[1].as_str()],
        6,
    ));
    validate(&coin, &payment, &contract())
}

#[test]
fn a_trc20_payment_is_refused_when_the_token_takes_a_transfer_fee() {
    assert_mismatch(
        trc20_payment_validation(Some(10), Some(1_000_000)),
        "token transfer fee",
    );
    // A rate and no readable maximum is a fee too.
    assert_mismatch(trc20_payment_validation(Some(10), None), "token transfer fee");
}

#[test]
fn a_trc20_payment_is_accepted_when_the_transfer_fee_is_zero_or_the_token_has_no_getters() {
    trc20_payment_validation(Some(0), Some(1_000_000)).expect("a zero rate");
    trc20_payment_validation(Some(10), Some(0)).expect("a zero maximum");
    trc20_payment_validation(None, None).expect("no getters");
}

// ---- the undetermined marker of the core (ch.51 R100 item 9, ch.21 R21.11D.1) -----------------------------------

/// What every undetermined error of the watch side must be: it passes the engine's marker test, names no node URL and
/// still names the fix.
fn assert_marked(text: &str, nodes: &[&Node]) {
    assert!(
        crate::swap_evidence::is_undetermined(text),
        "the error is not marked undetermined"
    );
    assert!(
        text.starts_with(crate::swap_evidence::UNDETERMINED_PREFIX),
        "the error does not start with the marker"
    );
    assert!(!text.contains("://"), "the error holds a URL scheme");
    for node in nodes {
        assert!(!text.contains(&node.url()), "the error holds a node URL");
    }
    assert!(
        text.contains("swap_evidence_node"),
        "the error does not name swap_evidence_node"
    );
    assert!(text.contains("another host"), "the error does not name another host");
}

/// The undetermined error of a trusted node that did not answer: marked, no URL, and the advice about that node.
fn assert_trusted_silent(text: &str, nodes: &[&Node]) {
    assert!(
        crate::swap_evidence::is_undetermined(text),
        "the error is not marked undetermined"
    );
    assert!(
        text.starts_with(crate::swap_evidence::UNDETERMINED_PREFIX),
        "the error does not start with the marker"
    );
    assert!(!text.contains("://"), "the error holds a URL scheme");
    for node in nodes {
        assert!(!text.contains(&node.url()), "the error holds a node URL");
    }
    assert!(
        text.contains("the node named by swap_evidence_node did not answer"),
        "the error does not name the silent trusted node"
    );
    assert!(
        text.contains("remove swap_evidence_node"),
        "the error does not advise removing swap_evidence_node"
    );
    assert!(
        !text.contains("add a node on another host"),
        "the error gives the two-node advice"
    );
}

/// What a determined error is: no marker.
fn assert_not_marked(text: &str) {
    assert!(
        !crate::swap_evidence::is_undetermined(text),
        "the error is not marked undetermined"
    );
}

#[test]
fn the_refund_gate_error_of_an_undetermined_read_carries_the_marker() {
    let lock = i64::from(LOCK_TIME);
    // Two nodes that disagree.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock - 5, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_marked(&gate(&coin).err().expect("undetermined").to_string(), &[&a, &b]);
    // The same through the coin's `can_refund_htlc`, whose error is a string.
    assert_marked(
        &trait_gate(&coin, u64::from(LOCK_TIME)).err().expect("undetermined"),
        &[&a, &b],
    );
    // One of two nodes does not answer.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    b.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_marked(&gate(&coin).err().expect("undetermined").to_string(), &[&a, &b]);
    // The trusted node does not answer: its own advice, not the two-node one.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    a.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a, &b], Some(0), 1);
    assert_trusted_silent(&gate(&coin).err().expect("undetermined").to_string(), &[&a, &b]);
    assert_trusted_silent(
        &trait_gate(&coin, u64::from(LOCK_TIME)).err().expect("undetermined"),
        &[&a, &b],
    );
    // A single node.
    let (a, b) = (gate_node(0, lock + 5, 1), gate_node(1, lock + 5, 1));
    b.edit(|w| w.solid_time_ms = None);
    let (_ctx, coin) = coin_over(&[&a], None, 1);
    assert_marked(&gate(&coin).err().expect("undetermined").to_string(), &[&a]);
    assert_marked(
        &trait_gate(&coin, u64::from(LOCK_TIME)).err().expect("undetermined"),
        &[&a],
    );
    // A determined outcome is no error at all: two agreeing nodes before the lock wait.
    let (a, b) = (gate_node(0, lock - 100, 1), gate_node(1, lock - 40, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert!(matches!(gate(&coin), Ok(CanRefundHtlc::HaveToWait(_))));
}

#[test]
fn the_search_error_of_an_undetermined_read_carries_the_marker() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_marked(
        &search(&coin, &payment, CAPS).expect_err("undetermined").to_string(),
        &[&a, &b],
    );
    // A single node.
    let a = payment_node(0, &payment, &me, 1);
    let (_ctx, coin) = coin_over(&[&a], None, 1);
    assert_marked(
        &search(&coin, &payment, CAPS).expect_err("undetermined").to_string(),
        &[&a],
    );
    // A determined error: the payment is uninitialized on both nodes.
    let (a, b) = (payment_node(0, &payment, &me, 0), payment_node(1, &payment, &me, 0));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_not_marked(&search(&coin, &payment, CAPS).expect_err("a mismatch").to_string());
}

#[test]
fn the_validation_error_of_an_undetermined_read_carries_the_marker() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_marked(
        &validate(&coin, &payment, &contract())
            .expect_err("undetermined")
            .to_string(),
        &[&a, &b],
    );
    // A single node.
    let a = payment_node(0, &payment, &me, 1);
    let (_ctx, coin) = coin_over(&[&a], None, 1);
    assert_marked(
        &validate(&coin, &payment, &contract())
            .expect_err("undetermined")
            .to_string(),
        &[&a],
    );
    // Determined errors: a mismatch, and a reverted receipt.
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_not_marked(
        &validate(&coin, &payment, &other_contract())
            .expect_err("a mismatch")
            .to_string(),
    );
    let reverted = |index| {
        Node::start(
            index,
            World::new()
                .with_state(expected_hash(&me), u64::from(LOCK_TIME), 1)
                .with_tx(&payment, PAYMENT_BLOCK, "REVERT", vec![]),
        )
    };
    let (a, b) = (reverted(0), reverted(1));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_not_marked(
        &validate(&coin, &payment, &contract())
            .expect_err("reverted")
            .to_string(),
    );
}

#[test]
fn the_revealing_claim_error_of_an_undetermined_read_carries_the_marker() {
    let me = my_address_of_test_coin();
    let payment = good_payment(&me);
    let (a, b) = (payment_node(0, &payment, &me, 1), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    let text = assert_not_sent(reveal(&coin, &payment), &[&a, &b]);
    assert_marked(&text, &[&a, &b]);
    // A single node.
    let a = payment_node(0, &payment, &me, 1);
    let (_ctx, coin) = coin_over(&[&a], None, 1);
    assert_marked(&assert_not_sent(reveal(&coin, &payment), &[&a]), &[&a]);
    // A determined refusal: the agreed state is spent.
    let (a, b) = (payment_node(0, &payment, &me, 2), payment_node(1, &payment, &me, 2));
    let (_ctx, coin) = coin_over(&[&a, &b], None, 1);
    assert_not_marked(&assert_not_sent(reveal(&coin, &payment), &[&a, &b]));
}

#[test]
fn the_refund_error_of_an_undetermined_read_carries_the_marker() {
    let me = my_address_of_test_coin();
    let own = own_payment(&me);
    let refund = |nodes: &[&Node]| {
        let (_ctx, coin) = coin_over(nodes, None, 1);
        let failure = block_on(refund_payment_with(
            &coin,
            &own.to_bytes(),
            &secret_hash(),
            &selected(&contract()),
            FAST,
        ))
        .expect_err("no refund");
        assert!(failure.tx.is_none());
        // The text the engine sees is the one of the `TransactionErr` the failure becomes.
        match TransactionErr::from(failure) {
            TransactionErr::Plain(text) => text,
            other => panic!("expected a plain error, got {other:?}"),
        }
    };
    let (a, b) = (own_payment_node(0, &own, 1), own_payment_node(1, &own, 2));
    assert_marked(&refund(&[&a, &b]), &[&a, &b]);
    let a = own_payment_node(0, &own, 1);
    assert_marked(&refund(&[&a]), &[&a]);
    // A determined refusal: both nodes say the payment is spent.
    let (a, b) = (own_payment_node(0, &own, 2), own_payment_node(1, &own, 2));
    assert_not_marked(&refund(&[&a, &b]));
}
