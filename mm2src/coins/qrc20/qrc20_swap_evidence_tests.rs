//! Offline tests of the QRC20 swap reads under the swap-evidence rule (CRD ch.51 §51.6G R100, tests T42 and T43).
//!
//! Every test builds a real token over scripted loopback Electrum servers ([`StubServer`]). Two servers are two hosts
//! only when their names differ, so the servers are listed under the names `a.test`, `b.test` and `c.test`, which a
//! test resolver maps to the loopback address. The resolver guard is process-wide, so these tests run one at a time.

use super::qrc20_swap_evidence::{undetermined_text, undetermined_texts};
use super::swap::ClaimCheck;
use super::*;
use crate::swap_evidence::{is_undetermined, Undetermined, UndeterminedReason};
use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
use crate::{CanRefundHtlc, SwapEvidencePolicy};
use common::block_on;
use common::dns_resolver::{install_test_map, TestResolverGuard};
use mm2_core::mm_ctx::MmCtxBuilder;
use std::net::{IpAddr, Ipv4Addr};

const TOKEN_CONTRACT: &str = "d362e096e873eb7907e205fadc6175c6fec7bc44";
const SWAP_CONTRACT: &str = "ba8b71f3544b93e2f681f996da519a98ace0107a";
const CALL: &str = "blockchain.contract.call";
const GET_HISTORY: &str = "blockchain.contract.event.get_history";
const GET_RECEIPT: &str = "blochchain.transaction.get_receipt";
const GET_TX: &str = "blockchain.transaction.get";
const TIP: &str = "blockchain.headers.subscribe";
const HEADERS: &str = "blockchain.block.headers";
const PREFIX: &str = "swap evidence undetermined: ";

const TIME_LOCK: u32 = 1_700_000_000;
const SECRET: [u8; 32] = [1; 32];
const UNINITIALIZED: u8 = 0;
const SENT: u8 = 1;
const SPENT: u8 = 2;
const REFUNDED: u8 = 3;

// priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
const PRIV_KEY: [u8; 32] = [
    3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144, 72, 172,
    110, 180, 13, 123, 179, 10, 49,
];

fn swap_contract() -> H160 { H160::from_slice(&hex::decode(SWAP_CONTRACT).unwrap()) }

fn counterparty() -> H160 { H160::from([0x33; 20]) }

fn secret_hash() -> Vec<u8> { dhash160(&SECRET).to_vec() }

fn swap_id() -> Vec<u8> { qrc20_swap_id(TIME_LOCK, &secret_hash()) }

fn value() -> U256 { U256::from(1000u64) }

fn now() -> u64 { now_ms() / 1000 }

/// The scripted servers of a test and the names they are listed under.
struct Hosts {
    _resolver: TestResolverGuard,
    servers: Vec<StubServer>,
}

impl Hosts {
    async fn start(count: usize) -> Hosts {
        let loopback: &[IpAddr] = &[IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let resolver = install_test_map(&[("a.test", loopback), ("b.test", loopback), ("c.test", loopback)]);
        let mut servers = Vec::new();
        for _ in 0..count {
            let server = StubServer::start(StubBehavior::Answer).await;
            server.set_result(TIP, json!({"height": 800, "hex": "00"}));
            server.set_result(GET_HISTORY, json!([]));
            servers.push(server);
        }
        Hosts {
            _resolver: resolver,
            servers,
        }
    }

    fn port(&self, index: usize) -> String {
        let addr = self.servers[index].addr();
        addr.rsplit(':').next().unwrap().to_owned()
    }

    /// The name the server `index` is listed under: `a.test:port`, `b.test:port`, `c.test:port`.
    fn name(&self, index: usize) -> String { format!("{}.test:{}", ["a", "b", "c"][index], self.port(index)) }

    fn urls(&self) -> Vec<String> { (0..self.servers.len()).map(|index| self.name(index)).collect() }

    fn a(&self) -> &StubServer { &self.servers[0] }

    fn b(&self) -> &StubServer { &self.servers[1] }

    fn both(&self) -> [&StubServer; 2] { [self.a(), self.b()] }
}

fn new_ctx() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({
            "electrum_tip_checks": false,
            "electrum_server_order": "listed",
            "electrum_spread_reads": false,
        }))
        .into_mm_arc()
}

/// [`new_ctx`] with the single-host switch (`swap_evidence_allow_single_host`) off: a coin with one host stays
/// `Insufficient`.
fn strict_ctx() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({
            "electrum_tip_checks": false,
            "electrum_server_order": "listed",
            "electrum_spread_reads": false,
            "swap_evidence_allow_single_host": false,
        }))
        .into_mm_arc()
}

async fn token_on(ctx: &MmArc, ticker: &str, urls: &[String], swap_evidence_node: Option<&str>) -> Qrc20Coin {
    let conf = json!({
        "coin": ticker,
        "decimals": 8,
        "required_confirmations": 0,
        "pubtype": 120,
        "p2shtype": 110,
        "wiftype": 128,
        "segwit": true,
        "mm2": 1,
        "mature_confirmations": 2000,
        "dust": 72800,
    });
    let servers: Vec<Json> = urls.iter().map(|url| json!({ "url": url })).collect();
    let req = json!({
        "method": "electrum",
        "servers": servers,
        "swap_contract_address": format!("0x{SWAP_CONTRACT}"),
        "swap_evidence_node": swap_evidence_node,
    });
    let params = Qrc20ActivationParams::from_legacy_req(&req).unwrap();
    let contract_address = H160::from_slice(&hex::decode(TOKEN_CONTRACT).unwrap());
    qrc20_coin_from_conf_and_params(ctx, ticker, "QTUM", &conf, &params, &PRIV_KEY, contract_address)
        .await
        .unwrap()
}

async fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !cond() {
        assert!(started.elapsed().as_secs_f64() < 20., "timed out waiting for {what}");
        Timer::sleep(0.02).await;
    }
}

/// A token over two hosts that both serve the contract-event history, admitted for swaps.
async fn two_host_token() -> (Hosts, MmArc, Qrc20Coin) {
    let hosts = Hosts::start(2).await;
    let ctx = new_ctx();
    let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
    wait_for("two hosts that served the event history", || {
        coin.swap_evidence_refusal().is_none()
    })
    .await;
    (hosts, ctx, coin)
}

/// A token that trusts the first of its two hosts.
async fn trusting_token() -> (Hosts, MmArc, Qrc20Coin) {
    let hosts = Hosts::start(2).await;
    let ctx = new_ctx();
    let coin = token_on(&ctx, "QRC20", &hosts.urls(), Some(&hosts.name(0))).await;
    wait_for("the trusted host that served the event history", || {
        coin.swap_evidence_refusal().is_none()
    })
    .await;
    (hosts, ctx, coin)
}

fn client_of(coin: &Qrc20Coin) -> ElectrumClient {
    let UtxoRpcClientEnum::Electrum(client) = coin.utxo.rpc_client.clone() else {
        panic!("an Electrum activation builds an Electrum client")
    };
    client
}

fn txid_of(tx: &UtxoTx) -> String {
    json::to_value(H256Json::from(tx.hash().reversed()))
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn tx_of(calls: Vec<ContractCallOutput>, seed: u32) -> UtxoTx {
    let mut tx = UtxoTx::default();
    tx.lock_time = seed;
    tx.outputs = calls.into_iter().map(TransactionOutput::from).collect();
    tx
}

/// The payment of the swap, sent to `receiver`.
fn payment_tx(coin: &Qrc20Coin, receiver: H160) -> UtxoTx {
    let call = coin
        .erc20_payment_call_bytes(swap_id(), value(), TIME_LOCK, &secret_hash(), receiver)
        .unwrap();
    let script_pubkey = generate_contract_call_script_pubkey(
        &call,
        QRC20_PAYMENT_GAS_LIMIT,
        QRC20_GAS_PRICE_DEFAULT,
        swap_contract().as_bytes(),
    )
    .unwrap()
    .to_bytes();
    tx_of(
        vec![ContractCallOutput {
            value: OUTPUT_QTUM_AMOUNT,
            script_pubkey,
            gas_limit: QRC20_PAYMENT_GAS_LIMIT,
            gas_price: QRC20_GAS_PRICE_DEFAULT,
        }],
        1,
    )
}

fn claim_tx(coin: &Qrc20Coin, secret: &[u8], sender: H160) -> UtxoTx {
    let call = coin
        .receiver_spend_output(&swap_contract(), swap_id(), value(), secret.to_vec(), sender)
        .unwrap();
    tx_of(vec![call], 2)
}

fn refund_tx(coin: &Qrc20Coin, receiver: H160) -> UtxoTx {
    let call = coin
        .sender_refund_output(&swap_contract(), swap_id(), value(), secret_hash(), receiver)
        .unwrap();
    tx_of(vec![call], 3)
}

fn receipt(tx: &UtxoTx, logs: Vec<Json>, excepted: &str) -> Json {
    json!([{
        "blockHash": "11".repeat(32),
        "blockNumber": 790,
        "transactionHash": txid_of(tx),
        "transactionIndex": 1,
        "outputIndex": 0,
        "from": "22".repeat(20),
        "to": SWAP_CONTRACT,
        "cumulativeGasUsed": 50_000,
        "gasUsed": 50_000,
        "contractAddress": SWAP_CONTRACT,
        "log": logs,
        "excepted": excepted,
        "exceptedMessage": "",
    }])
}

/// The `Transfer` event of a payment from `sender` into the swap contract.
fn payment_transfer_log(sender: H160) -> Json {
    json!({
        "address": TOKEN_CONTRACT,
        "topics": [QRC20_TRANSFER_TOPIC, address_to_log_topic(&sender), address_to_log_topic(&swap_contract())],
        "data": format!("{:064x}", value()),
    })
}

fn swap_event_log(topic: &str, data: &[u8]) -> Json {
    json!({"address": SWAP_CONTRACT, "topics": [topic], "data": hex::encode(data)})
}

fn verbose(tx: &UtxoTx, confirmations: u32) -> Json {
    json!({
        "hex": hex::encode(serialize(tx).take()),
        "txid": txid_of(tx),
        "version": 1,
        "locktime": tx.lock_time,
        "vin": [],
        "vout": [],
        "height": 790,
        "time": 1_700_000_001,
        "blocktime": 1_700_000_001,
        "confirmations": confirmations,
    })
}

fn set_state(server: &StubServer, state: u8) {
    let output = format!("{}{:064x}", "00".repeat(64), state);
    server.set_params_prefix_result(
        CALL,
        json!([SWAP_CONTRACT]),
        json!({"address": SWAP_CONTRACT, "executionResult": {"output": output}}),
    );
}

fn set_tip(server: &StubServer, height: u64) { server.set_result(TIP, json!({"height": height, "hex": "00"})); }

/// Makes `server` answer no swap read: every read starts with the tip.
fn silence(server: &StubServer) { server.set_error(TIP, "the server is silent"); }

/// Serves `tx` with `logs` in its receipt, as a raw and as a verbose transaction, and lists it in the event history.
fn serve_tx(server: &StubServer, tx: &UtxoTx, logs: Vec<Json>) {
    let txid = txid_of(tx);
    server.set_params_result(GET_RECEIPT, json!([txid]), receipt(tx, logs, "None"));
    server.set_params_result(GET_TX, json!([txid, false]), json!(hex::encode(serialize(tx).take())));
    server.set_params_result(GET_TX, json!([txid, true]), verbose(tx, 5));
}

fn list_in_history(server: &StubServer, txs: &[&UtxoTx]) {
    let items: Vec<Json> = txs
        .iter()
        .map(|tx| json!({"tx_hash": txid_of(tx), "height": 795, "log_index": 0}))
        .collect();
    server.set_result(GET_HISTORY, json!(items));
}

/// Serves the counterparty's payment to this node in the state `state`.
fn serve_payment(server: &StubServer, payment: &UtxoTx, sender: H160, state: u8) {
    serve_tx(server, payment, vec![payment_transfer_log(sender)]);
    set_state(server, state);
}

/// Serves the payment as claimed by `claim` (whose call carries `secret`).
fn serve_claim(server: &StubServer, claim: &UtxoTx, secret: &[u8]) {
    let data = [swap_id().as_slice(), secret].concat();
    serve_tx(server, claim, vec![swap_event_log(QRC20_RECEIVER_SPENT_TOPIC, &data)]);
    list_in_history(server, &[claim]);
    set_state(server, SPENT);
}

fn serve_refund(server: &StubServer, refund: &UtxoTx) {
    serve_tx(server, refund, vec![swap_event_log(
        QRC20_SENDER_REFUNDED_TOPIC,
        &swap_id(),
    )]);
    list_in_history(server, &[refund]);
    set_state(server, REFUNDED);
}

/// A header list whose median time past is `mtp`: eleven Qtum headers of that time.
fn serve_mtp(server: &StubServer, mtp: u32) {
    let mut header = Vec::new();
    header.extend_from_slice(&1u32.to_le_bytes());
    header.extend_from_slice(&[0; 64]);
    header.extend_from_slice(&mtp.to_le_bytes());
    header.extend_from_slice(&0x1d00_ffffu32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    // the two state roots, the stake outpoint and an empty block signature
    header.extend_from_slice(&[0; 64]);
    header.extend_from_slice(&[0; 36]);
    header.push(0);
    server.set_result(
        HEADERS,
        json!({"count": 11, "hex": hex::encode(header.repeat(11)), "max": 2016}),
    );
}

async fn validate(coin: &Qrc20Coin, payment: &UtxoTx, wait_until: u64) -> Result<(), String> {
    coin.validate_payment(
        payment.clone(),
        TIME_LOCK,
        counterparty(),
        secret_hash(),
        big_decimal_from_sat(1000, 8),
        swap_contract(),
        wait_until,
    )
    .await
}

fn enum_txid(tx: &TransactionEnum) -> String {
    match tx {
        TransactionEnum::UtxoTx(tx) => txid_of(tx),
        _ => panic!("a QRC20 transaction is a UTXO transaction"),
    }
}

/// The spend search: `None` for "not spent", else whether the payment was claimed or refunded and the id of the
/// transaction that did it.
async fn search(coin: &Qrc20Coin, payment: &UtxoTx) -> Result<Option<(&'static str, String)>, String> {
    let found = coin
        .search_for_swap_tx_spend(TIME_LOCK, secret_hash(), payment.clone(), 0)
        .await?;
    Ok(found.map(|found| match found {
        FoundSwapTxSpend::Spent(tx) => ("claimed", enum_txid(&tx)),
        FoundSwapTxSpend::Refunded(tx) => ("refunded", enum_txid(&tx)),
    }))
}

async fn refund_gate(coin: &Qrc20Coin, locktime: u64) -> Result<CanRefundHtlc, String> {
    coin.can_refund_htlc(locktime).compat().await
}

async fn refund_gate_error(coin: &Qrc20Coin, locktime: u64) -> String {
    match refund_gate(coin, locktime).await {
        Err(error) => error,
        Ok(_) => panic!("the refund gate must not answer"),
    }
}

fn assert_undetermined(text: &str, reason: &str) {
    assert!(is_undetermined(text), "the text is an undetermined read");
    assert!(text.contains(reason), "the text names the expected reason");
    assert!(!text.contains("://"), "the text holds no URL");
}

fn me(coin: &Qrc20Coin) -> H160 { coin.my_addr_as_contract_addr().unwrap() }

// ---- P: the counterparty's payment ----

#[test]
fn payment_two_agreeing_hosts_validate() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        validate(&coin, &payment, now()).await.unwrap();
    });
}

#[test]
fn payment_two_differing_hosts_are_undetermined_and_a_new_agreeing_pair_decides() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        serve_payment(hosts.b(), &payment, counterparty(), UNINITIALIZED);
        let error = validate(&coin, &payment, now()).await.unwrap_err();
        assert_undetermined(&error, "the nodes disagree");

        // The retry inside the read decides once a new pair agrees.
        set_state(hosts.b(), SENT);
        validate(&coin, &payment, now() + 30).await.unwrap();
    });
}

#[test]
fn payment_one_answering_host_is_undetermined() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        silence(hosts.b());
        let error = validate(&coin, &payment, now()).await.unwrap_err();
        assert_undetermined(&error, "fewer than two nodes answered");
    });
}

#[test]
fn payment_that_both_hosts_show_as_not_sent_is_refused() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), REFUNDED);
        }
        let error = validate(&coin, &payment, now()).await.unwrap_err();
        assert!(
            error.contains("Payment state is not PAYMENT_STATE_SENT"),
            "the error names the payment state"
        );
        assert!(!is_undetermined(&error), "the error is not an undetermined read");
    });
}

#[test]
fn confirmation_wait_ends_when_a_second_host_confirms() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let txid = txid_of(&payment);
        hosts
            .b()
            .set_params_result(GET_TX, json!([txid, true]), verbose(&payment, 0));
        let error = coin
            .wait_for_confirmations_and_check_result(payment.clone(), 1, false, now() + 2, 1)
            .await
            .unwrap_err();
        assert_undetermined(&error, "the nodes disagree");

        hosts
            .b()
            .set_params_result(GET_TX, json!([txid, true]), verbose(&payment, 1));
        coin.wait_for_confirmations_and_check_result(payment.clone(), 1, false, now() + 20, 1)
            .await
            .unwrap();
    });
}

#[test]
fn confirmation_wait_fails_when_both_hosts_show_the_call_as_failed() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
            server.set_params_result(
                GET_RECEIPT,
                json!([txid_of(&payment)]),
                receipt(&payment, vec![], "Revert"),
            );
        }
        let error = coin
            .wait_for_confirmations_and_check_result(payment.clone(), 1, false, now() + 20, 1)
            .await
            .unwrap_err();
        assert!(
            error.contains("Contract call failed with an error: Revert"),
            "the error names the failed contract call"
        );
    });
}

#[test]
fn already_sent_is_what_two_hosts_agree_on() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, counterparty());
        let sent_log = swap_event_log(QRC20_PAYMENT_SENT_TOPIC, &swap_id());
        for server in hosts.both() {
            set_state(server, UNINITIALIZED);
        }
        let sent = coin
            .check_if_my_payment_sent_impl(swap_contract(), swap_id(), 0)
            .await
            .unwrap();
        assert!(sent.is_none());

        // One host shows the payment, the other does not yet: never "not sent".
        serve_tx(hosts.a(), &payment, vec![
            payment_transfer_log(me(&coin)),
            sent_log.clone(),
        ]);
        list_in_history(hosts.a(), &[&payment]);
        set_state(hosts.a(), SENT);
        let error = coin
            .check_if_my_payment_sent_impl(swap_contract(), swap_id(), 0)
            .await
            .unwrap_err();
        assert_undetermined(&error, "the nodes disagree");

        serve_tx(hosts.b(), &payment, vec![payment_transfer_log(me(&coin)), sent_log]);
        list_in_history(hosts.b(), &[&payment]);
        set_state(hosts.b(), SENT);
        let sent = coin
            .check_if_my_payment_sent_impl(swap_contract(), swap_id(), 0)
            .await
            .unwrap();
        assert_eq!(sent.map(|tx| enum_txid(&tx)), Some(txid_of(&payment)));
    });
}

// ---- S: claimed, refunded or not spent ----

#[test]
fn spend_two_agreeing_hosts_decide_claimed_refunded_and_not_spent() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
        // "Not spent" is decided by the contract's state: the event history is not read.
        let after_probe = hosts.a().requests(GET_HISTORY);
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
        assert_eq!(hosts.a().requests(GET_HISTORY), after_probe);

        let refund = refund_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_refund(server, &refund);
        }
        assert_eq!(
            search(&coin, &payment).await.unwrap(),
            Some(("refunded", txid_of(&refund)))
        );

        let claim = claim_tx(&coin, &SECRET, counterparty());
        for server in hosts.both() {
            serve_claim(server, &claim, &SECRET);
        }
        assert_eq!(
            search(&coin, &payment).await.unwrap(),
            Some(("claimed", txid_of(&claim)))
        );
    });
}

#[test]
fn spend_two_differing_hosts_never_read_as_not_spent() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        serve_refund(hosts.b(), &refund_tx(&coin, me(&coin)));
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "the nodes disagree");
    });
}

#[test]
fn spend_one_answering_host_is_undetermined() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        silence(hosts.b());
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "fewer than two nodes answered");
    });
}

#[test]
fn hosts_one_block_apart_with_the_same_decision_agree() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        set_tip(hosts.b(), 801);
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
    });
}

#[test]
fn a_tip_gap_above_the_tolerance_is_undetermined() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let tolerance = client_of(&coin).tolerance_blocks();
        set_tip(hosts.b(), 800 + tolerance + 1);
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "the tips of the nodes differ too much");
    });
}

#[test]
fn a_claim_with_the_matching_secret_on_one_host_is_taken() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let claim = claim_tx(&coin, &SECRET, counterparty());
        serve_claim(hosts.b(), &claim, &SECRET);
        assert_eq!(
            search(&coin, &payment).await.unwrap(),
            Some(("claimed", txid_of(&claim)))
        );
    });
}

#[test]
fn a_claim_with_another_secret_on_one_host_is_not_taken() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let other_secret = [2; 32];
        serve_claim(
            hosts.b(),
            &claim_tx(&coin, &other_secret, counterparty()),
            &other_secret,
        );
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "fewer than two nodes answered");
    });
}

#[test]
fn spend_wait_returns_a_proven_claim_from_one_host() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        let claim = claim_tx(&coin, &SECRET, counterparty());
        serve_claim(hosts.a(), &claim, &SECRET);
        silence(hosts.b());
        let found = coin
            .wait_for_tx_spend_impl(payment.clone(), now() + 20, 0)
            .await
            .unwrap();
        assert_eq!(enum_txid(&found), txid_of(&claim));
    });
}

#[test]
fn spend_wait_runs_to_its_deadline_on_not_spent_and_on_an_undetermined_read() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let error = coin
            .wait_for_tx_spend_impl(payment.clone(), now(), 0)
            .await
            .unwrap_err();
        assert!(
            error.contains("Waited too long"),
            "the error is the deadline of the wait"
        );
        silence(hosts.b());
        let error = coin
            .wait_for_tx_spend_impl(payment.clone(), now(), 0)
            .await
            .unwrap_err();
        assert!(
            error.contains("Waited too long"),
            "the error is the deadline of the wait"
        );
    });
}

#[test]
fn the_makers_claim_rests_on_an_agreed_payment_state_and_the_takers_claim_is_not_held_back() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        silence(hosts.b());

        let error = coin
            .spend_hash_time_locked_payment(
                payment.clone(),
                swap_contract(),
                SECRET.to_vec(),
                ClaimCheck::AgreedPaymentState,
            )
            .await
            .unwrap_err();
        assert_undetermined(&format!("{:?}", error), "fewer than two nodes answered");

        // The secret is public already: the claim goes on to its send over the routed reads, where the stub has no
        // coins to spend. Whatever ends it there, it is not an undetermined read.
        let outcome = coin
            .spend_hash_time_locked_payment(
                payment.clone(),
                swap_contract(),
                SECRET.to_vec(),
                ClaimCheck::SecretAlreadyProven,
            )
            .await;
        if let Err(error) = outcome {
            assert!(
                !is_undetermined(&format!("{:?}", error)),
                "the claim does not end as an undetermined read"
            );
        }
    });
}

#[test]
fn a_refund_is_not_sent_on_an_undetermined_payment_state() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, counterparty());
        serve_payment(hosts.a(), &payment, me(&coin), SENT);
        serve_payment(hosts.b(), &payment, me(&coin), SPENT);
        let error = coin
            .refund_hash_time_locked_payment(swap_contract(), payment.clone())
            .await
            .unwrap_err();
        assert_undetermined(&format!("{:?}", error), "the nodes disagree");
    });
}

// ---- T: the chain time of the refund gate ----

#[test]
fn refund_gate_opens_on_two_different_mtps_past_the_lock() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let locktime = now() - 7200;
        serve_mtp(hosts.a(), (locktime + 10) as u32);
        serve_mtp(hosts.b(), (locktime + 13) as u32);
        assert!(matches!(
            refund_gate(&coin, locktime).await.unwrap(),
            CanRefundHtlc::CanRefundNow
        ));
    });
}

#[test]
fn refund_gate_stays_closed_when_one_mtp_is_not_past_the_lock() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let locktime = now() - 7200;
        serve_mtp(hosts.a(), (locktime + 10) as u32);
        serve_mtp(hosts.b(), (locktime - 10) as u32);
        let error = refund_gate_error(&coin, locktime).await;
        assert_undetermined(&error, "the nodes disagree");

        // Both not past the lock: a wait, as the single-server gate answers it.
        serve_mtp(hosts.a(), (locktime - 10) as u32);
        assert!(matches!(
            refund_gate(&coin, locktime).await.unwrap(),
            CanRefundHtlc::HaveToWait(11)
        ));
    });
}

/// The gate is the shared UTXO gate; what is the token's own is the policy it is given. While the capability of
/// the contract-event history is missing, that policy is `Insufficient` and the gate stays closed.
#[test]
fn refund_gate_stays_closed_while_the_capability_is_missing() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        hosts.b().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        let locktime = now() - 7200;
        for server in hosts.both() {
            serve_mtp(server, (locktime + 10) as u32);
        }
        let error = refund_gate_error(&coin, locktime).await;
        assert_undetermined(&error, "fewer than two nodes answered");
    });
}

// ---- The trusted host and the operator's own daemon ----

#[test]
fn trusted_host_answer_is_taken_alone() {
    block_on(async {
        let (hosts, _ctx, coin) = trusting_token().await;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Trusted {
            node: hosts.name(0)
        });
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        // The other host says otherwise and is not asked.
        serve_refund(hosts.b(), &refund_tx(&coin, me(&coin)));
        let calls = hosts.b().requests(CALL);
        validate(&coin, &payment, now()).await.unwrap();
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
        assert_eq!(hosts.b().requests(CALL), calls);

        let locktime = now() - 7200;
        serve_mtp(hosts.a(), (locktime - 10) as u32);
        serve_mtp(hosts.b(), (locktime + 10) as u32);
        assert!(matches!(
            refund_gate(&coin, locktime).await.unwrap(),
            CanRefundHtlc::HaveToWait(11)
        ));
        serve_mtp(hosts.a(), (locktime + 10) as u32);
        assert!(matches!(
            refund_gate(&coin, locktime).await.unwrap(),
            CanRefundHtlc::CanRefundNow
        ));
    });
}

#[test]
fn silent_trusted_host_is_undetermined_unless_a_secret_proves_the_claim() {
    block_on(async {
        let (hosts, _ctx, coin) = trusting_token().await;
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.b(), &payment, counterparty(), SENT);
        silence(hosts.a());
        let error = validate(&coin, &payment, now()).await.unwrap_err();
        assert_undetermined(&error, "the trusted node did not answer");
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "the trusted node did not answer");

        let claim = claim_tx(&coin, &SECRET, counterparty());
        serve_claim(hosts.b(), &claim, &SECRET);
        assert_eq!(
            search(&coin, &payment).await.unwrap(),
            Some(("claimed", txid_of(&claim)))
        );
    });
}

#[test]
fn own_daemon_policy_reads_as_before_and_is_not_refused() {
    block_on(async {
        let hosts = Hosts::start(1).await;
        let ctx = new_ctx();
        let mut coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        // The policy a native activation stores; the reads then take the routed path, whatever the client is.
        std::sync::Arc::get_mut(&mut coin.0).unwrap().utxo.swap_evidence = SwapEvidencePolicy::OwnDaemon;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::OwnDaemon);
        assert_eq!(coin.swap_evidence_refusal(), None);
        assert!(coin.agreed_swap_reads().is_none());
        let payment = payment_tx(&coin, me(&coin));
        serve_payment(hosts.a(), &payment, counterparty(), SENT);
        validate(&coin, &payment, now()).await.unwrap();
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
    });
}

// ---- The capability rule of the contract-event history ----

#[test]
fn two_hosts_that_served_the_event_history_admit_swaps() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        assert!(matches!(
            coin.swap_evidence_policy(),
            SwapEvidencePolicy::TwoNodes { .. }
        ));
        // The probe asked each host once.
        assert_eq!(hosts.a().requests(GET_HISTORY), 1);
        assert_eq!(hosts.b().requests(GET_HISTORY), 1);
    });
}

#[test]
fn one_capable_host_refuses_with_the_text_naming_the_hosts() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        hosts.b().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        wait_for("the probe of both hosts", || {
            hosts.a().requests(GET_HISTORY) == 1 && hosts.b().requests(GET_HISTORY) == 1
        })
        .await;
        wait_for("the answer of the first host", || {
            client_of(&coin).contract_history_servers().0.len() == 1
        })
        .await;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 1
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(
            refusal.contains("blockchain.contract.event.get_history"),
            "the refusal names the method"
        );
        assert!(
            refusal.contains(&format!("served: {}", hosts.name(0))),
            "the refusal lists the host that served the request"
        );
        assert!(
            refusal.contains(&format!("not served: {}", hosts.name(1))),
            "the refusal lists the host that did not serve the request"
        );
        assert!(
            refusal.contains("swap_evidence_node"),
            "the text names swap_evidence_node"
        );
        assert!(!refusal.contains("://"), "the refusal holds no URL");

        // A running swap of such a coin waits: its reads are undetermined.
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "fewer than two nodes answered");

        // The refusals asked for a new probe, which is bounded: the host was probed within the last minute.
        Timer::sleep(0.3).await;
        assert_eq!(hosts.b().requests(GET_HISTORY), 1);
        let client = client_of(&coin);
        assert!(client.claim_contract_history_probes(60., None).is_empty());

        // Once the host serves the read, to a probe or to any other request, the token is admitted.
        hosts.b().clear_error(GET_HISTORY);
        let address = contract_addr_into_rpc_format(&me(&coin));
        let token = contract_addr_into_rpc_format(&coin.contract_address);
        client
            .blockchain_contract_event_get_history_from(&hosts.name(1), &address, &token, QRC20_TRANSFER_TOPIC)
            .compat()
            .await
            .unwrap();
        assert_eq!(coin.swap_evidence_refusal(), None);
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
    });
}

#[test]
fn a_host_is_not_probed_while_a_probe_of_it_runs() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        hosts.b().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        wait_for("the probe of the second host", || hosts.b().requests(GET_HISTORY) == 1).await;
        let client = client_of(&coin);
        wait_for("the end of the probe", || {
            client.contract_history_servers().0.len() == 1 && client.claim_contract_history_probes(60., None).is_empty()
        })
        .await;
        // With no minimum gap the host that has not served the read is given once, and not again until its probe
        // ends; the host that served it is never given.
        assert_eq!(client.claim_contract_history_probes(0., None), vec![hosts.name(1)]);
        assert!(client.claim_contract_history_probes(0., None).is_empty());
        client.end_contract_history_probe(&hosts.name(1));
        assert_eq!(client.claim_contract_history_probes(0., None), vec![hosts.name(1)]);
        client.end_contract_history_probe(&hosts.name(1));
    });
}

#[test]
fn a_disabled_token_starts_no_probe() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        hosts.b().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        wait_for("the probe of the second host", || hosts.b().requests(GET_HISTORY) == 1).await;
        let client = client_of(&coin);
        wait_for("the end of the probe", || {
            client.contract_history_servers().0.len() == 1
        })
        .await;
        // The minute of the bound is taken out of the way: the host may be probed again at once.
        assert_eq!(client.claim_contract_history_probes(0., None), vec![hosts.name(1)]);
        client.end_contract_history_probe(&hosts.name(1));

        coin.end_capability_probes();
        coin.start_capability_probe();
        Timer::sleep(0.3).await;
        assert_eq!(hosts.b().requests(GET_HISTORY), 1);
    });
}

#[test]
fn an_error_answer_to_the_event_history_does_not_count() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        for server in hosts.both() {
            server.set_error_object(GET_HISTORY, json!({"code": 1, "message": "history too large"}));
        }
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        wait_for("the probe of both hosts", || {
            hosts.a().requests(GET_HISTORY) == 1 && hosts.b().requests(GET_HISTORY) == 1
        })
        .await;
        Timer::sleep(0.2).await;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 0
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(refusal.contains("served: none"), "the refusal lists no host as served");
    });
}

#[test]
fn a_trusted_host_needs_the_capability_too() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        hosts
            .a()
            .set_reply_delay(GET_HISTORY, std::time::Duration::from_millis(1500));
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), Some(&hosts.name(0))).await;

        // Refused before the trusted host has served the read, with a text that names it.
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 0
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(
            refusal.contains(&format!("trusted Electrum server {}", hosts.name(0))),
            "the refusal names the trusted host"
        );
        assert!(
            refusal.contains("blockchain.contract.event.get_history"),
            "the refusal names the method"
        );
        assert!(
            refusal.contains("swap_evidence_node"),
            "the text names swap_evidence_node"
        );
        assert!(!refusal.contains("://"), "the refusal holds no URL");
        // A running swap's reads wait meanwhile.
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        let error = search(&coin, &payment).await.unwrap_err();
        assert_undetermined(&error, "fewer than two nodes answered");

        // Admitted once it has; only the trusted host was probed, once.
        wait_for("the trusted host that served the event history", || {
            coin.swap_evidence_refusal().is_none()
        })
        .await;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Trusted {
            node: hosts.name(0)
        });
        assert_eq!(search(&coin, &payment).await.unwrap(), None);
        assert_eq!(hosts.a().requests(GET_HISTORY), 1);
        assert_eq!(hosts.b().requests(GET_HISTORY), 0);
    });
}

#[test]
fn a_trusted_host_that_answers_the_event_history_with_an_error_is_refused() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        // The other host serves the read; that does not help.
        hosts.a().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), Some(&hosts.name(0))).await;
        wait_for("the probe of the trusted host", || hosts.a().requests(GET_HISTORY) == 1).await;
        let client = client_of(&coin);
        wait_for("the end of the probe", || {
            client.claim_contract_history_probes(0., Some(&hosts.name(0))).len() == 1
        })
        .await;
        client.end_contract_history_probe(&hosts.name(0));
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 0
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(
            refusal.contains(&format!("trusted Electrum server {}", hosts.name(0))),
            "the refusal names the trusted host"
        );
        // The refusals asked for a new probe, which is bounded to once a minute for the host, and the other host is
        // never probed.
        Timer::sleep(0.3).await;
        assert_eq!(hosts.a().requests(GET_HISTORY), 1);
        assert_eq!(hosts.b().requests(GET_HISTORY), 0);

        // Once the trusted host serves the read, the token is admitted.
        hosts.a().clear_error(GET_HISTORY);
        let address = contract_addr_into_rpc_format(&me(&coin));
        let token = contract_addr_into_rpc_format(&coin.contract_address);
        client
            .blockchain_contract_event_get_history_from(&hosts.name(0), &address, &token, QRC20_TRANSFER_TOPIC)
            .compat()
            .await
            .unwrap();
        assert_eq!(coin.swap_evidence_refusal(), None);
    });
}

/// The single-host switch (`swap_evidence_allow_single_host`, on by default): a token whose platform lists ONE
/// configured Electrum host is `Trusted` on it through the UTXO builder, and the capability rule of a trusted server
/// then applies to that host: refused until it has served the contract-event history, admitted after.
#[test]
fn one_configured_host_is_trusted_and_needs_the_capability() {
    block_on(async {
        let hosts = Hosts::start(1).await;
        hosts.a().set_error(GET_HISTORY, "unknown method");
        let ctx = new_ctx();
        let coin = token_on(&ctx, "QRC20", &hosts.urls(), None).await;
        assert_eq!(coin.utxo.swap_evidence, SwapEvidencePolicy::Trusted {
            node: hosts.name(0)
        });
        wait_for("the probe of the one host", || hosts.a().requests(GET_HISTORY) == 1).await;
        let client = client_of(&coin);
        wait_for("the end of the probe", || {
            client.claim_contract_history_probes(0., Some(&hosts.name(0))).len() == 1
        })
        .await;
        client.end_contract_history_probe(&hosts.name(0));

        // Refused while the host has not served the read, with the text of a trusted server.
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 0
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(
            refusal.contains(&format!("trusted Electrum server {}", hosts.name(0))),
            "the refusal names the trusted host"
        );
        assert!(!refusal.contains("://"), "the refusal holds no URL");

        // Admitted once the host serves the read.
        hosts.a().clear_error(GET_HISTORY);
        let address = contract_addr_into_rpc_format(&me(&coin));
        let token = contract_addr_into_rpc_format(&coin.contract_address);
        client
            .blockchain_contract_event_get_history_from(&hosts.name(0), &address, &token, QRC20_TRANSFER_TOPIC)
            .compat()
            .await
            .unwrap();
        assert_eq!(coin.swap_evidence_refusal(), None);
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Trusted {
            node: hosts.name(0)
        });
    });
}

#[test]
fn two_ports_of_one_host_count_once() {
    block_on(async {
        let hosts = Hosts::start(2).await;
        let urls = vec![format!("a.test:{}", hosts.port(0)), format!("A.test:{}", hosts.port(1))];
        let ctx = strict_ctx();
        let coin = token_on(&ctx, "QRC20", &urls, None).await;
        assert_eq!(coin.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 1
        });
        let refusal = coin.swap_evidence_refusal().unwrap();
        assert!(
            refusal.contains("fewer than two nodes"),
            "the refusal says that the coin has fewer than two nodes"
        );
        assert!(
            refusal.contains("swap_evidence_node"),
            "the text names swap_evidence_node"
        );
        Timer::sleep(0.3).await;
        assert_eq!(hosts.a().requests(GET_HISTORY), 0);
        assert_eq!(hosts.b().requests(GET_HISTORY), 0);
    });
}

#[test]
fn capability_recorded_by_one_token_serves_another_on_the_shared_client() {
    block_on(async {
        let (hosts, ctx, coin) = two_host_token().await;
        let other = token_on(&ctx, "QRC20B", &hosts.urls(), None).await;
        assert!(std::sync::Arc::ptr_eq(&client_of(&coin).0, &client_of(&other).0));
        // Admitted at once, and the hosts are not asked again.
        assert_eq!(other.swap_evidence_refusal(), None);
        Timer::sleep(0.3).await;
        assert_eq!(hosts.a().requests(GET_HISTORY), 1);
        assert_eq!(hosts.b().requests(GET_HISTORY), 1);
    });
}

// ---- The text of an undetermined read ----

#[test]
fn an_undetermined_qrc20_read_carries_the_prefix_and_names_hosts_only() {
    block_on(async {
        let (hosts, _ctx, coin) = two_host_token().await;
        let payment = payment_tx(&coin, me(&coin));
        for server in hosts.both() {
            serve_payment(server, &payment, counterparty(), SENT);
        }
        serve_refund(hosts.b(), &refund_tx(&coin, me(&coin)));
        let error = search(&coin, &payment).await.unwrap_err();
        assert!(
            error.starts_with(PREFIX),
            "the error starts with the marker of an undetermined read"
        );
        assert!(error.contains(&hosts.name(0)), "the error names the first host");
        assert!(error.contains(&hosts.name(1)), "the error names the second host");
        for forbidden in ["://", "tcp", "ssl", "wss", "@"] {
            assert!(
                !error.to_lowercase().contains(forbidden),
                "the error holds no URL, scheme or credential mark"
            );
        }
        assert!(
            error.contains("swap_evidence_node"),
            "the text names swap_evidence_node"
        );
    });
}

#[test]
fn the_undetermined_text_is_built_from_the_undetermined_value() {
    let undetermined = Undetermined {
        reason: UndeterminedReason::Disagree,
        per_node: vec![
            ("a.test:50001".to_owned(), "NotSpent at block 800".to_owned()),
            ("b.test:50001".to_owned(), "no answer: timeout".to_owned()),
        ],
    };
    let text = undetermined_text(&undetermined);
    assert!(is_undetermined(&text), "the text is an undetermined read");
    assert!(
        text.contains("a.test:50001") && text.contains("b.test:50001"),
        "the text names both hosts"
    );
}

/// CRD ch.51 R80, R81: the warning of an undetermined read names the kind and the fixes, and no server; the error
/// text keeps what each server said.
#[test]
fn the_warning_of_an_undetermined_read_holds_no_host_label() {
    let undetermined = Undetermined {
        reason: UndeterminedReason::Disagree,
        per_node: vec![
            ("a.test:50001".to_owned(), "NotSpent at block 800".to_owned()),
            ("b.test:50002".to_owned(), "no answer: timeout".to_owned()),
        ],
    };
    let (warning, text) = undetermined_texts("QRC20", "spend state", "payment transaction 11", &undetermined);
    assert!(
        is_undetermined(&warning),
        "the warning carries the marker of an undetermined read"
    );
    for expected in ["the nodes disagree", "QRC20", "spend state", "swap_evidence_node"] {
        assert!(
            warning.contains(expected),
            "the warning names the reason, the coin, the read and the fix"
        );
    }
    for node_text in [
        "a.test",
        "b.test",
        "50001",
        "50002",
        "NotSpent",
        "no answer",
        "block 800",
    ] {
        assert!(
            !warning.contains(node_text),
            "the warning names no server and no answer of a server"
        );
    }
    assert!(
        text.contains("a.test:50001: NotSpent at block 800") && text.contains("b.test:50002: no answer: timeout"),
        "the error text keeps what each server said"
    );
}

#[test]
fn the_qrc20_family_has_adopted_the_rule() {
    assert!(crate::swap_evidence::family_adopted(
        crate::swap_evidence::SwapFamily::Qrc20
    ));
}
