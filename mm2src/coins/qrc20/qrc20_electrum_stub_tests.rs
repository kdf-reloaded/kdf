//! Offline characterization tests of QRC20 tokens over Electrum (CRD ch.38 §38.6.4Q, tests T38.6.4Q.2 to Q.6).
//!
//! Every test activates a real token (`lp_coininit`, the builder the legacy `electrum` method uses) over one or two
//! scripted loopback Electrum servers ([`StubServer`]) and pins what this tree does today. Where this tree differs
//! from the reference text of §38.6.4Q, the test says so in a comment.
//!
//! The module is a child of `history` so that the collector's private steps (`process_tx_ids`,
//! `request_tx_history`, `check_if_history_update_is_needed`) can be called directly.
//!
//! Level of the activation tests: the legacy `electrum` method itself lives in `mm2_main` and answers over HTTP; these
//! tests run what it does to the coin: `lp_coininit` (build and registration), then the bounded balance read and,
//! when that fails, the deregistration (`activate_like_electrum`). The HTTP status and the response body are not
//! covered here.

use super::*;
use crate::utxo::qtum::{QtumBasedCoin, QtumCoin, QtumDelegationOps, QtumDelegationRequest};
use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
use crate::utxo::rpc_clients::with_call_budget;
use crate::utxo::UtxoCoinFields;
use crate::{disable_coin, lp_coinfind, lp_coininit, MmCoinEnum, StakingInfosDetails, WithdrawRequest};
use chain::{OutPoint, TransactionInput};
use common::block_on;
use crypto::CryptoCtx;
use mm2_core::mm_ctx::MmCtxBuilder;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

const TOKEN_CONTRACT: &str = "d362e096e873eb7907e205fadc6175c6fec7bc44";
const SWAP_CONTRACT: &str = "0xba8b71f3544b93e2f681f996da519a98ace0107a";
const CALL: &str = "blockchain.contract.call";
const GET_HISTORY: &str = "blockchain.contract.event.get_history";
/// The server's own spelling (R38.6.4Q.8 item 3).
const GET_RECEIPT: &str = "blochchain.transaction.get_receipt";
const GET_TX: &str = "blockchain.transaction.get";
const TICKER: &str = "QRC20";
/// ABI selector of `decimals()`.
const DECIMALS_CALL_DATA: &str = "313ce567";
const TIP_METHOD: &str = "blockchain.headers.subscribe";

// ---- Fixtures ----

static ENV_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A node context with the token's coins-file entry, the Electrum settings of the reference (listed server order,
/// no read spreading: the rank of a server is its place in the request) and a database directory of its own.
struct TokenEnv {
    ctx: MmArc,
    root: PathBuf,
}

impl Drop for TokenEnv {
    fn drop(&mut self) {
        // Ends the collector of a test at its next check.
        let _ = self.ctx.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn token_conf(decimals: Option<u8>) -> Json {
    let mut conf = json!({
        "coin": TICKER,
        "required_confirmations": 0,
        "pubtype": 120,
        "p2shtype": 110,
        "wiftype": 128,
        "segwit": true,
        "mm2": 1,
        "mature_confirmations": 2000,
        "dust": 72800,
        "protocol": {
            "type": "QRC20",
            "protocol_data": {"platform": "QTUM", "contract_address": format!("0x{TOKEN_CONTRACT}")},
        },
    });
    if let Some(decimals) = decimals {
        conf["decimals"] = json!(decimals);
    }
    conf
}

fn token_env(decimals: Option<u8>) -> TokenEnv {
    let root = std::env::temp_dir().join(format!(
        "kdf-qrc20-stub-{}-{}",
        std::process::id(),
        ENV_COUNTER.fetch_add(1, AtomicOrdering::SeqCst)
    ));
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({
            "coins": [token_conf(decimals)],
            "dbdir": root.to_str().unwrap(),
            "electrum_server_order": "listed",
            "electrum_spread_reads": false,
        }))
        .into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "qrc20 electrum stub tests").unwrap();
    std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).unwrap();
    std::fs::create_dir_all(ctx.dbdir().join("TX_CACHE")).unwrap();
    TokenEnv { ctx, root }
}

fn electrum_request(servers: &[&StubServer], tx_history: bool) -> Json {
    let servers: Vec<_> = servers.iter().map(|server| json!({"url": server.addr()})).collect();
    json!({
        "method": "electrum",
        "coin": TICKER,
        "servers": servers,
        "swap_contract_address": SWAP_CONTRACT,
        "tx_history": tx_history,
    })
}

/// A started stub that answers the tip and, for every `blockchain.contract.call` of the token contract, the balance
/// `units` (a rule that matches the contract only, so that the call data can be asserted afterwards).
async fn token_stub(units: u64) -> StubServer {
    let server = StubServer::start(crate::utxo::rpc_clients::electrum_stub_server::StubBehavior::Answer).await;
    server.set_result(TIP_METHOD, json!({"height": 777, "hex": "00"}));
    serve_balance(&server, units);
    server
}

fn balance_answer(units: u64) -> Json {
    json!({"address": TOKEN_CONTRACT, "executionResult": {"output": format!("{units:064x}")}})
}

fn serve_balance(server: &StubServer, units: u64) {
    server.set_params_prefix_result(CALL, json!([TOKEN_CONTRACT]), balance_answer(units));
}

async fn init_token(env: &TokenEnv, servers: &[&StubServer], tx_history: bool) -> Result<Qrc20Coin, String> {
    match lp_coininit(&env.ctx, TICKER, &electrum_request(servers, tx_history)).await? {
        MmCoinEnum::Qrc20Coin(coin) => Ok(coin),
        _ => panic!("a QRC20 coins-file entry must build a QRC20 coin"),
    }
}

/// What the legacy `electrum` method does to the coin: `lp_coininit`, then one balance read bounded by 20 s (which is
/// also the call budget of its Electrum requests), and the deregistration of the coin when the read fails. Returns the
/// outcome and the coin, when it was built.
async fn activate_like_electrum(
    env: &TokenEnv,
    servers: &[&StubServer],
    tx_history: bool,
) -> (Result<CoinBalance, String>, Option<Qrc20Coin>) {
    let coin = match init_token(env, servers, tx_history).await {
        Ok(coin) => coin,
        Err(e) => return (Err(e), None),
    };
    let balance = with_call_budget(Duration::from_secs(20), async {
        coin.my_balance().compat().await.map_err(|e| e.to_string())
    })
    .await;
    if balance.is_err() {
        disable_coin(&env.ctx, TICKER).await.unwrap();
    }
    (balance, Some(coin))
}

fn balance_of_call_data(coin: &Qrc20Coin) -> String {
    let function = eth::ERC20_CONTRACT.function("balanceOf").unwrap();
    let data = function
        .encode_input(&[Token::Address(coin.my_addr_as_contract_addr().unwrap())])
        .unwrap();
    hex::encode(data)
}

async fn is_registered(env: &TokenEnv) -> bool { lp_coinfind(&env.ctx, TICKER).await.unwrap().is_some() }

async fn wait_until(what: &str, secs: f64, cond: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !cond() {
        assert!(started.elapsed().as_secs_f64() < secs, "timed out waiting for {what}");
        Timer::sleep(0.02).await;
    }
}

fn balance_of(units: u64) -> BigDecimal { big_decimal_from_sat(units as i64, 8) }

// ---- T38.6.4Q.2: methods at activation ----

#[test]
fn t38_6_4q_2_activation_sends_one_balance_call_with_an_empty_sender() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(123_456_789).await;
        let (balance, coin) = activate_like_electrum(&env, &[&server], false).await;
        let coin = coin.unwrap();

        // The balance is decoded from `executionResult.output` (R38.6.4Q.8 item 1).
        assert_eq!(balance.unwrap().spendable, BigDecimal::from_str("1.23456789").unwrap());
        // Exactly one `blockchain.contract.call`: contract, `balanceOf(wallet address)` call data, empty sender.
        assert_eq!(server.requests(CALL), 1);
        assert_eq!(server.request_params(CALL), [json!([
            TOKEN_CONTRACT,
            balance_of_call_data(&coin),
            ""
        ])]);
        // Differs from R38.6.4Q.9 ("no block height"): this tree's build asks the tip once
        // (`blockchain.headers.subscribe`) before the balance call. Nothing else is asked.
        assert_eq!(server.request_methods_in_order(), [TIP_METHOD, CALL]);
        // Neither token information nor a contract-event subscription (R38.6.4Q.5 item 1, R38.6.4Q.8 item 4).
        assert_eq!(server.requests("blockchain.token.get_info"), 0);
        assert_eq!(server.requests("blockchain.contract.event.subscribe"), 0);
        // No history request without `tx_history`.
        assert_eq!(server.requests(GET_HISTORY), 0);
        assert_eq!(server.requests(GET_RECEIPT), 0);
    });
}

#[test]
fn t38_6_4q_2_activation_without_decimals_asks_for_decimals_before_the_balance() {
    block_on(async {
        let env = token_env(None);
        let server = token_stub(2_500_000_000).await;
        // The `decimals()` answer wins over the balance rule: the rule of the exact call data.
        server.set_params_result(CALL, json!([TOKEN_CONTRACT, DECIMALS_CALL_DATA, ""]), balance_answer(8));
        let (balance, coin) = activate_like_electrum(&env, &[&server], false).await;
        let coin = coin.unwrap();

        assert_eq!(coin.utxo.decimals, 8);
        assert_eq!(balance.unwrap().spendable, BigDecimal::from_str("25").unwrap());
        assert_eq!(server.request_params(CALL), [
            json!([TOKEN_CONTRACT, DECIMALS_CALL_DATA, ""]),
            json!([TOKEN_CONTRACT, balance_of_call_data(&coin), ""]),
        ]);
        assert_eq!(server.requests("blockchain.token.get_info"), 0);
        assert_eq!(server.requests("blockchain.contract.event.subscribe"), 0);
    });
}

// ---- T38.6.4Q.3: an error answer at activation ----

#[test]
fn t38_6_4q_3_error_answer_to_the_activation_balance_read_fails_it_and_a_retry_fails_the_same_way() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        server.set_error(CALL, "stub contract error");

        let (first, coin) = activate_like_electrum(&env, &[&server], false).await;
        let first = first.unwrap_err();
        assert!(first.contains("stub contract error"), "{first}");
        // The token is not active (deregistered again), and the stub's session is still open: the error answer
        // did not close the session of the client that is still held here.
        assert!(!is_registered(&env).await);
        let coin = coin.unwrap();
        assert!(server.open_connections() >= 1);
        assert_eq!(server.requests(CALL), 1, "nothing is repeated inside the activation");

        // A second attempt fails the same way: not as "already initialized".
        let (second, _coin) = activate_like_electrum(&env, &[&server], false).await;
        let second = second.unwrap_err();
        assert!(second.contains("stub contract error"), "{second}");
        assert!(!second.contains("already"), "{second}");
        assert!(!is_registered(&env).await);
        assert_eq!(server.requests(CALL), 2);
        drop(coin);
    });
}

#[test]
fn t38_6_4q_3_error_answer_to_the_decimals_read_fails_the_build_and_registers_nothing() {
    block_on(async {
        let env = token_env(None);
        let server = token_stub(1).await;
        server.set_error(CALL, "stub contract error");

        let first = init_token(&env, &[&server], false)
            .await
            .err()
            .expect("the build fails");
        assert!(first.contains("stub contract error"), "{first}");
        assert!(!is_registered(&env).await);
        assert_eq!(server.requests(CALL), 1, "nothing is repeated");
        // Only the `decimals()` call was asked.
        assert_eq!(server.request_params(CALL), [json!([
            TOKEN_CONTRACT,
            DECIMALS_CALL_DATA,
            ""
        ])]);

        // A second attempt fails the same way, not as "already initialized".
        let second = init_token(&env, &[&server], false)
            .await
            .err()
            .expect("the build fails again");
        assert!(second.contains("stub contract error"), "{second}");
        assert!(!second.contains("already"), "{second}");
        assert_eq!(server.requests(CALL), 2);
    });
}

#[test]
fn t38_6_4q_3_first_error_answer_ends_the_activation_although_another_server_answers_correctly() {
    block_on(async {
        let env = token_env(Some(8));
        // Listed first: answers the contract call with an error at once. Listed second: answers it correctly, later.
        let erring = token_stub(1).await;
        erring.set_error(CALL, "stub contract error");
        let correct = token_stub(5).await;
        correct.set_reply_delay(CALL, Duration::from_millis(400));

        let (outcome, _coin) = activate_like_electrum(&env, &[&erring, &correct], false).await;
        let error = outcome.unwrap_err();
        assert!(error.contains("stub contract error"), "{error}");
        assert!(!is_registered(&env).await);
        assert_eq!(erring.requests(CALL), 1);
        // The request is one request: the error answer ended it. Observed: the second server was not asked at all
        // (the request went to the highest-ranked session). Were it asked, it would be once and its answer unused.
        assert!(correct.requests(CALL) <= 1);
    });
}

// ---- T38.6.4Q.4: an error answer to a balance read ----

#[test]
fn t38_6_4q_4_error_answer_to_a_balance_read_fails_the_read_and_the_next_read_asks_again() {
    block_on(async {
        let env = token_env(Some(8));
        let first = token_stub(100).await;
        let second = token_stub(200).await;
        let coin = init_token(&env, &[&first, &second], false).await.unwrap();
        let UtxoRpcClientEnum::Electrum(client) = coin.utxo.rpc_client.clone() else {
            panic!("an Electrum activation builds an Electrum client")
        };
        let started = std::time::Instant::now();
        while client.count_connected().await != 2 {
            assert!(started.elapsed().as_secs() < 10, "both sessions must be maintained");
            Timer::sleep(0.02).await;
        }

        // The higher-ranked (first listed) server takes the read.
        assert_eq!(coin.my_balance().compat().await.unwrap().spendable, balance_of(100));
        assert_eq!((first.requests(CALL), second.requests(CALL)), (1, 0));

        // It starts answering with an error: the read fails, no other server is asked, no earlier value is served.
        first.set_error(CALL, "stub contract error");
        let sessions = first.open_connections();
        let error = coin.my_balance().compat().await.unwrap_err().to_string();
        assert!(error.contains("stub contract error"), "{error}");
        assert_eq!((first.requests(CALL), second.requests(CALL)), (2, 0));
        // The session stays open and the token stays enabled.
        assert_eq!(first.open_connections(), sessions);
        assert!(sessions >= 1);
        assert!(is_registered(&env).await);

        // Still failing: asked again, fails again (no value kept from the first read).
        let error = coin.my_balance().compat().await.unwrap_err().to_string();
        assert!(error.contains("stub contract error"), "{error}");
        assert_eq!((first.requests(CALL), second.requests(CALL)), (3, 0));

        // After the recovery the next read asks again and succeeds with the new value.
        first.clear_error(CALL);
        serve_balance(&first, 300);
        assert_eq!(coin.my_balance().compat().await.unwrap().spendable, balance_of(300));
        assert_eq!((first.requests(CALL), second.requests(CALL)), (4, 0));
    });
}

// ---- Transactions of the collector tests ----

fn my_script(coin: &Qrc20Coin) -> ScriptBytes {
    let my_address = coin.utxo.derivation_method.iguana_or_err().unwrap();
    ScriptBuilder::build_p2pkh(&my_address.hash).to_bytes()
}

/// A funding transaction with one output per entry of `values`, paying the wallet; `seed` makes its hash unique.
fn funding_tx(coin: &Qrc20Coin, seed: u32, values: &[u64]) -> UtxoTx {
    let mut tx = UtxoTx::default();
    tx.lock_time = seed;
    for value in values {
        tx.outputs.push(TransactionOutput {
            value: *value,
            script_pubkey: my_script(coin),
        });
    }
    tx
}

/// A transaction that spends the listed outputs and has one contract-call output (a `transfer` of the token, so that
/// the output index of the receipt is 0) and one change output. Its miner fee, gas included, is 5_000_000 satoshi.
fn contract_tx(coin: &Qrc20Coin, seed: u32, spends: &[(&UtxoTx, u32)]) -> UtxoTx {
    let mut tx = UtxoTx::default();
    tx.lock_time = seed;
    let mut input_value = 0;
    for (prev, index) in spends {
        input_value += prev.outputs[*index as usize].value;
        tx.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: prev.hash(),
                index: *index,
            },
            script_sig: Default::default(),
            sequence: u32::MAX,
            script_witness: vec![],
        });
    }
    let contract_call = coin
        .transfer_output(H160::default(), U256::from(1u64), 100_000, 40)
        .unwrap();
    tx.outputs.push(TransactionOutput::from(contract_call));
    tx.outputs.push(TransactionOutput {
        value: input_value - 5_000_000,
        script_pubkey: my_script(coin),
    });
    tx
}

fn txid_of(tx: &UtxoTx) -> String {
    json::to_value(H256Json::from(tx.hash().reversed()))
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn verbose_answer(tx: &UtxoTx, height: u64, time: u32) -> Json {
    json!({
        "hex": hex::encode(serialize(tx).take()),
        "txid": txid_of(tx),
        "version": 1,
        "locktime": tx.lock_time,
        "vin": [],
        "vout": [],
        "height": height,
        "time": time,
        "blocktime": time,
        "confirmations": 100,
    })
}

/// One `Transfer` event log of the token between the wallet and a stranger.
fn transfer_log(coin: &Qrc20Coin, received: bool, units: u64) -> Json {
    let me = coin.my_addr_as_contract_addr().unwrap();
    let stranger = H160::from([0x11; 20]);
    let (from, to) = if received { (stranger, me) } else { (me, stranger) };
    json!({
        "address": TOKEN_CONTRACT,
        "topics": [QRC20_TRANSFER_TOPIC, address_to_log_topic(&from), address_to_log_topic(&to)],
        "data": format!("{units:064x}"),
    })
}

fn receipt_answer(tx: &UtxoTx, block: u64, logs: Vec<Json>) -> Json {
    json!([{
        "blockHash": "11".repeat(32),
        "blockNumber": block,
        "transactionHash": txid_of(tx),
        "transactionIndex": 1,
        "outputIndex": 0,
        "from": "22".repeat(20),
        "to": TOKEN_CONTRACT,
        "cumulativeGasUsed": 50_000,
        "gasUsed": 50_000,
        "contractAddress": TOKEN_CONTRACT,
        "log": logs,
        "excepted": "None",
        "exceptedMessage": "",
    }])
}

/// What the stub serves about one transaction of the history: its receipt and its verbose transaction, and the verbose
/// transactions of the transactions it spends from.
struct ServedTx {
    tx: UtxoTx,
    prevs: Vec<UtxoTx>,
    block: u64,
    time: u32,
    logs: Vec<Json>,
}

fn serve_tx(server: &StubServer, served: &ServedTx) {
    let txid = txid_of(&served.tx);
    server.set_params_result(
        GET_RECEIPT,
        json!([txid]),
        receipt_answer(&served.tx, served.block, served.logs.clone()),
    );
    server.set_params_result(
        GET_TX,
        json!([txid, true]),
        verbose_answer(&served.tx, served.block, served.time),
    );
    for prev in &served.prevs {
        server.set_params_result(
            GET_TX,
            json!([txid_of(prev), true]),
            verbose_answer(prev, 500, 1_600_000_000),
        );
    }
}

fn history_item(served: &ServedTx, log_index: u64) -> Json {
    json!({"tx_hash": txid_of(&served.tx), "height": served.block, "log_index": log_index})
}

/// Transaction A: one received transfer, spends two outputs of one funding transaction. Transaction B: two transfers
/// (one received, one sent) in one receipt, spends one output of another funding transaction.
fn two_served_txs(coin: &Qrc20Coin) -> (ServedTx, ServedTx) {
    let prev_a = funding_tx(coin, 1, &[4_000_000, 3_000_000]);
    let tx_a = contract_tx(coin, 11, &[(&prev_a, 0), (&prev_a, 1)]);
    let prev_b = funding_tx(coin, 2, &[9_000_000]);
    let tx_b = contract_tx(coin, 12, &[(&prev_b, 0)]);
    let a = ServedTx {
        tx: tx_a,
        prevs: vec![prev_a],
        block: 700,
        time: 1_700_000_001,
        logs: vec![transfer_log(coin, true, 500)],
    };
    let b = ServedTx {
        tx: tx_b,
        prevs: vec![prev_b],
        block: 701,
        time: 1_700_000_002,
        logs: vec![transfer_log(coin, true, 700), transfer_log(coin, false, 200)],
    };
    (a, b)
}

/// `(tx hash hex, output index, log index)` of every stored row, sorted.
fn row_ids(rows: &[crate::TransactionDetails]) -> Vec<(String, u64, u64)> {
    let mut ids: Vec<_> = rows
        .iter()
        .map(|row| {
            let id = TxInternalId::from_bytes(&row.internal_id).unwrap();
            (row.tx_hash.clone(), id.output_index, id.log_index)
        })
        .collect();
    ids.sort();
    ids
}

/// The ordinary requests of the four QRC20 history methods, in the order the stub answered them, as `(method, params)`.
fn history_requests(server: &StubServer) -> Vec<(String, Json)> {
    server
        .request_log()
        .into_iter()
        .filter(|(method, _)| matches!(method.as_str(), CALL | GET_HISTORY | GET_RECEIPT | GET_TX))
        .collect()
}

// ---- T38.6.4Q.6: history failures ----

#[test]
fn t38_6_4q_6_history_retry_delay_doubles_from_ten_seconds_to_a_cap_of_five_minutes() {
    // This tree backs off (§38.6.5 item 8); the reference repeats after a fixed 10 s (R38.6.4Q.6 item 4).
    let mut delay = HISTORY_RETRY_INITIAL_DELAY_SECS;
    let mut seen = vec![delay];
    for _ in 0..7 {
        delay = next_history_retry_delay(delay);
        seen.push(delay);
    }
    assert_eq!(seen, [10, 20, 40, 80, 160, 300, 300, 300]);
}

#[test]
fn t38_6_4q_6_history_request_error_is_a_retry_and_only_the_exact_history_too_large_object_ends_it() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();

        // An error answer, a transport failure of the request, or an answer that cannot be read: a retry.
        server.set_error(GET_HISTORY, "stub history error");
        let outcome = coin.request_tx_history(env.ctx.metrics.clone()).await;
        assert!(matches!(outcome, RequestTxHistoryResult::Retry { .. }));

        server.clear_error(GET_HISTORY);
        server.set_result(GET_HISTORY, json!("not a list"));
        let outcome = coin.request_tx_history(env.ctx.metrics.clone()).await;
        assert!(matches!(outcome, RequestTxHistoryResult::Retry { .. }));

        // The same message with another code is still a retry: the whole error object is compared.
        server.clear_error(GET_HISTORY);
        server.set_error_object(GET_HISTORY, json!({"code": -1, "message": "history too large"}));
        let outcome = coin.request_tx_history(env.ctx.metrics.clone()).await;
        assert!(matches!(outcome, RequestTxHistoryResult::Retry { .. }));

        // Exactly `{"code": 1, "message": "history too large"}` ends the collector.
        server.set_error_object(GET_HISTORY, json!({"code": 1, "message": "history too large"}));
        let outcome = coin.request_tx_history(env.ctx.metrics.clone()).await;
        assert!(matches!(outcome, RequestTxHistoryResult::HistoryTooLarge));

        // The answer is a list: the distinct pairs of hash and height (a repeated pair once).
        server.clear_error(GET_HISTORY);
        server.set_result(
            GET_HISTORY,
            json!([
                {"tx_hash": "aa".repeat(32), "height": 5, "log_index": 0},
                {"tx_hash": "aa".repeat(32), "height": 5, "log_index": 1},
                {"tx_hash": "bb".repeat(32), "height": 6, "log_index": 0},
            ]),
        );
        let RequestTxHistoryResult::Ok(ids) = coin.request_tx_history(env.ctx.metrics.clone()).await else {
            panic!("a list is a history")
        };
        assert_eq!(ids.len(), 2);
        assert_eq!(ids.iter().map(|(_, height)| *height).collect::<Vec<_>>(), [5, 6]);
        // The history request is the contract-event one: the wallet's hash, the token contract, the `Transfer` topic.
        let wallet = coin.my_addr_as_contract_addr().unwrap();
        assert_eq!(
            server.request_params(GET_HISTORY).last().unwrap(),
            &json!([
                json::to_value(contract_addr_into_rpc_format(&wallet)).unwrap(),
                TOKEN_CONTRACT,
                QRC20_TRANSFER_TOPIC
            ])
        );
    });
}

#[test]
fn t38_6_4q_6_history_request_error_is_asked_again_after_the_backoff_and_the_state_is_not_an_error() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        server.set_error(GET_HISTORY, "stub history error");
        // `tx_history` true: the collector of the token runs (the real activation path).
        let coin = init_token(&env, &[&server], true).await.unwrap();

        wait_until("the first history request", 10., || server.requests(GET_HISTORY) == 1).await;
        // The collector backs off 10 s (this tree's first retry delay); it does not ask again at once.
        Timer::sleep(4.).await;
        assert_eq!(server.requests(GET_HISTORY), 1);
        // The history state is not an error: the failed request leaves it where it was.
        assert!(matches!(coin.history_sync_status(), HistorySyncState::NotStarted));

        wait_until("the second history request", 12., || server.requests(GET_HISTORY) == 2).await;
        assert!(matches!(coin.history_sync_status(), HistorySyncState::NotStarted));
        // Every pass began with its own balance read.
        assert_eq!(server.requests(CALL), 2);
    });
}

#[test]
fn t38_6_4q_6_history_too_large_ends_the_collector_with_error_minus_one_and_the_balance_still_answers() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(42).await;
        server.set_error_object(GET_HISTORY, json!({"code": 1, "message": "history too large"}));
        let coin = init_token(&env, &[&server], true).await.unwrap();

        wait_until("the history error state", 10., || {
            matches!(coin.history_sync_status(), HistorySyncState::Error(_))
        })
        .await;
        let HistorySyncState::Error(error) = coin.history_sync_status() else {
            unreachable!()
        };
        assert_eq!(error["code"], json!(-1));

        // The first pass would repeat its request after 10 s (the retry delay); it never does: the collector has
        // ended for the instance's life.
        Timer::sleep(12.).await;
        assert_eq!(server.requests(GET_HISTORY), 1);
        assert_eq!(server.requests(CALL), 1);
        assert!(matches!(coin.history_sync_status(), HistorySyncState::Error(_)));

        // The balance still answers.
        assert_eq!(coin.my_balance().compat().await.unwrap().spendable, balance_of(42));
    });
}

#[test]
fn t38_6_4q_6_failed_receipt_of_one_listed_transaction_leaves_it_out_and_stores_the_other() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let (a, b) = two_served_txs(&coin);
        serve_tx(&server, &a);
        serve_tx(&server, &b);
        // The receipt request of A answers an error object; B's receipt is fine.
        server.set_params_error(
            GET_RECEIPT,
            json!([txid_of(&a.tx)]),
            json!({"code": 3, "message": "no receipt"}),
        );

        let tx_ids = vec![
            (json::from_value::<H256Json>(json!(txid_of(&a.tx))).unwrap(), a.block),
            (json::from_value::<H256Json>(json!(txid_of(&b.tx))).unwrap(), b.block),
        ];
        let mut history_map = HistoryMapByHash::new();
        let updated = coin.process_tx_ids(&env.ctx, &mut history_map, tx_ids).await;

        // The pass goes on: B's two rows are stored, A is absent, and the state is finished (not an error).
        assert!(updated);
        let stored: Vec<_> = history_map.values().flat_map(|rows| rows.values().cloned()).collect();
        assert_eq!(row_ids(&stored), [(txid_of(&b.tx), 0, 0), (txid_of(&b.tx), 0, 1),]);
        assert!(matches!(coin.history_sync_status(), HistorySyncState::Finished));
        assert_eq!(server.requests(GET_RECEIPT), 2);
        // A's verbose transaction was never asked: the receipt comes first and its failure leaves the transaction out.
        assert!(!server.request_params(GET_TX).contains(&json!([txid_of(&a.tx), true])));

        // It stays absent while the balance is unchanged: the pass is not repeated (R38.6.4Q.6 item 1 and the
        // code-quality finding after item 5). The same balance and no row without timestamp or height: no update.
        let balance = CoinBalance {
            spendable: balance_of(1),
            unspendable: BigDecimal::from(0),
        };
        assert!(!coin.check_if_history_update_is_needed(&history_map, &Some(balance.clone()), &balance));
        // A changed balance asks for the history again, and so does the first pass.
        let other = CoinBalance {
            spendable: balance_of(2),
            unspendable: BigDecimal::from(0),
        };
        assert!(coin.check_if_history_update_is_needed(&history_map, &Some(balance.clone()), &other));
        assert!(coin.check_if_history_update_is_needed(&history_map, &None, &balance));
    });
}

// ---- T38.6.4Q.5: a collector pass ----

#[test]
fn t38_6_4q_5_collector_pass_asks_receipt_then_transaction_per_transaction_and_the_next_pass_asks_the_balance_only() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1_200).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let (a, b) = two_served_txs(&coin);
        serve_tx(&server, &a);
        serve_tx(&server, &b);
        // B appears twice in the answer (two events of one transaction): the distinct pairs are taken.
        server.set_result(
            GET_HISTORY,
            json!([history_item(&a, 0), history_item(&b, 0), history_item(&b, 1),]),
        );
        // The collector is started by hand, on the coin that is already registered and answered nothing yet.
        let collector = coin.clone();
        let ctx = env.ctx.clone();
        common::executor::spawn(async move { collector.history_loop(ctx).await });

        // The first pass, then the next one: its balance read is the second `blockchain.contract.call`.
        wait_until("the second pass to start", 10., || server.requests(CALL) == 2).await;
        Timer::sleep(1.).await;

        let wallet = coin.my_addr_as_contract_addr().unwrap();
        let balance_call = json!([TOKEN_CONTRACT, balance_of_call_data(&coin), ""]);
        let history_call = json!([
            json::to_value(contract_addr_into_rpc_format(&wallet)).unwrap(),
            TOKEN_CONTRACT,
            QRC20_TRANSFER_TOPIC
        ]);
        let tx = |served: &ServedTx| json!([txid_of(&served.tx), true]);
        let prev = |served: &ServedTx| json!([txid_of(&served.prevs[0]), true]);
        // The pass: the balance, the history, then per transaction its receipt, its verbose transaction and each
        // spent-from transaction once (A spends two outputs of one transaction), then the next pass's balance read.
        assert_eq!(history_requests(&server), [
            (CALL.to_owned(), balance_call.clone()),
            (GET_HISTORY.to_owned(), history_call),
            (GET_RECEIPT.to_owned(), json!([txid_of(&a.tx)])),
            (GET_TX.to_owned(), tx(&a)),
            (GET_TX.to_owned(), prev(&a)),
            (GET_RECEIPT.to_owned(), json!([txid_of(&b.tx)])),
            (GET_TX.to_owned(), tx(&b)),
            (GET_TX.to_owned(), prev(&b)),
            (CALL.to_owned(), balance_call),
        ]);
        // The receipt request is the misspelled method name; the correctly spelled one is never sent.
        assert_eq!(server.requests(GET_RECEIPT), 2);
        assert_eq!(server.requests("blockchain.transaction.get_receipt"), 0);

        // The rows stored: A one row, B two rows.
        let rows = coin.load_history_from_file(&env.ctx).compat().await.unwrap();
        assert_eq!(row_ids(&rows), [
            (txid_of(&a.tx), 0, 0),
            (txid_of(&b.tx), 0, 0),
            (txid_of(&b.tx), 0, 1),
        ]);
        let row = |tx_hash: &str, log_index: u64| {
            rows.iter()
                .find(|row| {
                    row.tx_hash == tx_hash && TxInternalId::from_bytes(&row.internal_id).unwrap().log_index == log_index
                })
                .unwrap()
        };
        assert_eq!(row(&txid_of(&a.tx), 0).my_balance_change, balance_of(500));
        assert_eq!(row(&txid_of(&b.tx), 0).my_balance_change, balance_of(700));
        assert_eq!(row(&txid_of(&b.tx), 1).my_balance_change, -balance_of(200));
        // The block height is the receipt's, the timestamp the verbose transaction's.
        assert_eq!(
            (row(&txid_of(&a.tx), 0).block_height, row(&txid_of(&a.tx), 0).timestamp),
            (700, 1_700_000_001)
        );
        assert_eq!(
            (row(&txid_of(&b.tx), 1).block_height, row(&txid_of(&b.tx), 1).timestamp),
            (701, 1_700_000_002)
        );
        assert!(matches!(coin.history_sync_status(), HistorySyncState::Finished));
    });
}

// ---- Part 2 (rows T38.6.4Q.1 and T38.6.4Q.7 to T38.6.4Q.11): the family together ----
//
// QTUM, two tokens and a plain UTXO coin over stubs of their own. The wallet key is one, so every instance spends from
// the same address. `electrum_server_order` stays `listed` and `electrum_spread_reads` `false` (as for the rows above);
// no row of this part needs the defaults.

const QTUM_TICKER: &str = "QTUM";
const TOKEN_B_TICKER: &str = "QRC20B";
const TOKEN_B_CONTRACT: &str = "fe59cbc1704e89a698571413a81f0de9d8f00c69";
const DOC_TICKER: &str = "DOC";
const LIST_UNSPENT: &str = "blockchain.scripthash.listunspent";
const BROADCAST: &str = "blockchain.transaction.broadcast";
const EVENT_SUBSCRIBE: &str = "blockchain.contract.event.subscribe";
const SERVER_VERSION: &str = "server.version";
/// The answer of every accepted broadcast: a transaction id (the client does not compare it with the transaction).
const ACCEPTED_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000001";

/// The tests of this part take the process-wide build lock (`UTXO_LOCK`) or measure what it does; they run one after
/// another so that no other test of this part lengthens the wait of the one that measures.
static LOCK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn family_env() -> TokenEnv {
    let root = std::env::temp_dir().join(format!(
        "kdf-qrc20-stub-{}-{}",
        std::process::id(),
        ENV_COUNTER.fetch_add(1, AtomicOrdering::SeqCst)
    ));
    let mut token_a = token_conf(Some(8));
    token_a["txfee"] = json!(1000);
    let mut token_b = token_a.clone();
    token_b["coin"] = json!(TOKEN_B_TICKER);
    token_b["protocol"]["protocol_data"]["contract_address"] = json!(format!("0x{TOKEN_B_CONTRACT}"));
    let qtum = json!({
        "coin": QTUM_TICKER,
        "required_confirmations": 0,
        "pubtype": 120,
        "p2shtype": 110,
        "wiftype": 128,
        "segwit": true,
        "mm2": 1,
        "mature_confirmations": 2000,
        "dust": 72800,
        "txfee": 1000,
        "protocol": {"type": "QTUM"},
    });
    let doc = json!({
        "coin": DOC_TICKER,
        "required_confirmations": 0,
        "pubtype": 120,
        "p2shtype": 110,
        "wiftype": 128,
        "mm2": 1,
        "dust": 1000,
        "txfee": 1000,
        "protocol": {"type": "UTXO"},
    });
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({
            "coins": [qtum, token_a, token_b, doc],
            "dbdir": root.to_str().unwrap(),
            "electrum_server_order": "listed",
            "electrum_spread_reads": false,
        }))
        .into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "qrc20 electrum stub tests").unwrap();
    std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).unwrap();
    std::fs::create_dir_all(ctx.dbdir().join("TX_CACHE")).unwrap();
    TokenEnv { ctx, root }
}

/// A stub that answers the tip and the balance of both tokens.
async fn family_stub() -> StubServer {
    let server = token_stub(1_000).await;
    // The tip is set, so that the stub counts the sessions that asked for headers.
    server.set_tip(777);
    server.set_params_prefix_result(CALL, json!([TOKEN_B_CONTRACT]), balance_answer(2_000));
    server.set_result(BROADCAST, json!(ACCEPTED_TXID));
    server
}

/// What the legacy `electrum` method asks `lp_coininit` for: the request of `ticker` over `servers`.
async fn init_coin(env: &TokenEnv, ticker: &str, servers: &[&StubServer], tx_history: bool) -> MmCoinEnum {
    let mut request = electrum_request(servers, tx_history);
    request["coin"] = json!(ticker);
    // The QTUM and the UTXO coin would check the maturity of what they spend (the tokens always do, whatever is asked).
    request["check_utxo_maturity"] = json!(false);
    lp_coininit(&env.ctx, ticker, &request)
        .await
        .unwrap_or_else(|e| panic!("activation of {ticker}: {e}"))
}

async fn init_qtum(env: &TokenEnv, servers: &[&StubServer]) -> QtumCoin {
    match init_coin(env, QTUM_TICKER, servers, false).await {
        MmCoinEnum::QtumCoin(coin) => coin,
        _ => panic!("a QTUM coins-file entry must build a QTUM coin"),
    }
}

async fn init_token_named(env: &TokenEnv, ticker: &str, servers: &[&StubServer], tx_history: bool) -> Qrc20Coin {
    match init_coin(env, ticker, servers, tx_history).await {
        MmCoinEnum::Qrc20Coin(coin) => coin,
        _ => panic!("a QRC20 coins-file entry must build a QRC20 coin"),
    }
}

async fn init_doc(env: &TokenEnv, servers: &[&StubServer]) -> crate::utxo::utxo_standard::UtxoStandardCoin {
    match init_coin(env, DOC_TICKER, servers, false).await {
        MmCoinEnum::UtxoCoin(coin) => coin,
        _ => panic!("a UTXO coins-file entry must build a UTXO coin"),
    }
}

fn script_of(utxo: &UtxoCoinFields) -> ScriptBytes {
    let my_address = utxo.derivation_method.iguana_or_err().unwrap();
    ScriptBuilder::build_p2pkh(&my_address.hash).to_bytes()
}

/// A funding transaction with one output per entry of `values` paying `script`; the stub lists the outputs as unspent of
/// whatever script hash is asked and serves the verbose transaction of the funding transaction.
fn fund(server: &StubServer, script: &ScriptBytes, seed: u32, values: &[u64]) -> UtxoTx {
    let mut tx = UtxoTx::default();
    tx.lock_time = seed;
    for value in values {
        tx.outputs.push(TransactionOutput {
            value: *value,
            script_pubkey: script.clone(),
        });
    }
    let listed: Vec<Json> = values
        .iter()
        .enumerate()
        .map(|(index, value)| json!({"tx_hash": txid_of(&tx), "tx_pos": index, "height": 500, "value": value}))
        .collect();
    server.set_result(LIST_UNSPENT, json!(listed));
    server.set_params_result(
        GET_TX,
        json!([txid_of(&tx), true]),
        verbose_answer(&tx, 500, 1_600_000_000),
    );
    tx
}

/// Every transaction the stub was asked to broadcast, in order.
fn broadcast_txs(server: &StubServer) -> Vec<UtxoTx> {
    server
        .request_params(BROADCAST)
        .iter()
        .map(|params| {
            let bytes = hex::decode(params[0].as_str().expect("the broadcast carries the hex")).unwrap();
            deserialize(bytes.as_slice()).unwrap()
        })
        .collect()
}

/// The inputs `(previous transaction id, output index)` of a transaction.
fn inputs_of(tx: &UtxoTx) -> Vec<(String, u32)> {
    tx.inputs
        .iter()
        .map(|input| {
            let hash = json::to_value(H256Json::from(input.previous_output.hash.reversed())).unwrap();
            (hash.as_str().unwrap().to_owned(), input.previous_output.index)
        })
        .collect()
}

fn outpoint(tx: &UtxoTx, index: u32) -> (String, u32) { (txid_of(tx), index) }

// ---- T38.6.4Q.1: sessions per family ----

#[test]
fn t38_6_4q_1_one_session_per_instance_each_with_its_own_version_and_a_disabled_token_closes_one() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let qtum = init_qtum(&env, &[&server]).await;
        let token_a = init_token_named(&env, TICKER, &[&server], false).await;
        let token_b = init_token_named(&env, TOKEN_B_TICKER, &[&server], false).await;
        wait_until("three sessions", 10., || server.open_connections() == 3).await;
        // Let any further session show up.
        Timer::sleep(1.).await;

        // Parity (R38.6.4Q.1 item 4): three sessions to the one server, each with its own `server.version`.
        assert_eq!(server.open_connections(), 3);
        assert_eq!(server.accepted(), 3);
        assert_eq!(server.requests(SERVER_VERSION), 3);
        // Each instance asked its own tip over its own session.
        assert_eq!(server.requests(TIP_METHOD), 3);
        assert_eq!(server.header_subscriptions(), 3);

        // The node's handles are the registry entries. Drop the ones of this test, then disable token A.
        drop((qtum, token_a, token_b));
        disable_coin(&env.ctx, TICKER).await.unwrap();
        wait_until("two sessions", 10., || server.open_connections() == 2).await;
        Timer::sleep(1.).await;
        // Exactly one session closed; the other two stay and nothing reconnected.
        assert_eq!(server.open_connections(), 2);
        assert_eq!(server.accepted(), 3);

        // The token that is left still answers over its session.
        let Some(MmCoinEnum::Qrc20Coin(token_b)) = lp_coinfind(&env.ctx, TOKEN_B_TICKER).await.unwrap() else {
            panic!("token B is still enabled")
        };
        assert_eq!(
            token_b.my_balance().compat().await.unwrap().spendable,
            balance_of(2_000)
        );
        assert_eq!(server.accepted(), 3);
    });
}

#[test]
fn t38_6_4q_10_disabling_qtum_leaves_a_token_and_its_session_working() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let qtum = init_qtum(&env, &[&server]).await;
        let token_a = init_token_named(&env, TICKER, &[&server], false).await;
        wait_until("two sessions", 10., || server.open_connections() == 2).await;
        drop((qtum, token_a));

        disable_coin(&env.ctx, QTUM_TICKER).await.unwrap();
        wait_until("one session", 10., || server.open_connections() == 1).await;
        Timer::sleep(1.).await;
        // QTUM's session closed, the token's stays, and the token's balance read goes over it.
        assert_eq!(server.open_connections(), 1);
        assert_eq!(server.accepted(), 2);
        let Some(MmCoinEnum::Qrc20Coin(token)) = lp_coinfind(&env.ctx, TICKER).await.unwrap() else {
            panic!("the token is still enabled")
        };
        assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
        assert_eq!(server.open_connections(), 1);
        assert_eq!(server.accepted(), 2);
    });
}

// ---- T38.6.4Q.10: disable stops the token's requests ----

/// Activates the token with `tx_history` true over `server` (an empty history) and waits until its collector is in the
/// 30-s idle wait: pass one (balance, history), pass two (balance, unchanged). Returns when the wait began.
async fn token_with_collector_in_the_idle_wait(env: &TokenEnv, server: &StubServer) -> std::time::Instant {
    server.set_result(GET_HISTORY, json!([]));
    drop(init_token_named(env, TICKER, &[server], true).await);
    wait_until("the second balance read of the collector", 10., || {
        server.requests(CALL) == 2
    })
    .await;
    // Nothing follows: the wait has begun.
    Timer::sleep(0.5).await;
    assert_eq!((server.requests(CALL), server.requests(GET_HISTORY)), (2, 1));
    std::time::Instant::now() - Duration::from_millis(500)
}

#[test]
fn t38_6_4q_10_disable_keeps_the_collectors_session_until_its_next_registry_check_and_sends_nothing() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let idle_since = token_with_collector_in_the_idle_wait(&env, &server).await;
        assert_eq!(server.open_connections(), 1);
        let requests_before = server.ordinary_requests();

        disable_coin(&env.ctx, TICKER).await.unwrap();
        let disabled_at = std::time::Instant::now();
        // This tree stops nothing at the disable (differs from R38.6.4Q.10 item 1): the collector holds the client, so
        // the session is still open, and it stays open while the collector waits.
        assert_eq!(server.open_connections(), 1);
        Timer::sleep(10.).await;
        assert_eq!(server.open_connections(), 1);
        assert_eq!(server.ordinary_requests(), requests_before);

        // The wait ends 30 s after the second balance read; the registry check finds no ticker and the collector ends.
        // The session closes with it (the collector held the last handle of the client).
        wait_until("the session to close", 40., || server.open_connections() == 0).await;
        let closed_after_idle = idle_since.elapsed().as_secs_f64();
        eprintln!(
            "Q10 observed: session closed {:.1} s after the disable, {closed_after_idle:.1} s after the wait began",
            disabled_at.elapsed().as_secs_f64()
        );
        assert!((29.0..40.0).contains(&closed_after_idle), "{closed_after_idle}");
        // The stub received nothing from the disabled token, before or after the check: no request at all.
        Timer::sleep(1.).await;
        assert_eq!(server.ordinary_requests(), requests_before);
        assert_eq!(server.requests(CALL), 2);
        assert_eq!(server.accepted(), 1);
    });
}

#[test]
fn t38_6_4q_10_enable_again_inside_the_wait_leaves_two_collectors_and_two_sessions() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let idle_since = token_with_collector_in_the_idle_wait(&env, &server).await;

        disable_coin(&env.ctx, TICKER).await.unwrap();
        // Enable again at once: a new client, and a new collector (`tx_history` true) with its two passes.
        drop(init_token_named(&env, TICKER, &[&server], true).await);
        wait_until("the new collector's passes", 10., || server.requests(CALL) == 4).await;
        assert_eq!(server.requests(GET_HISTORY), 2);
        // Two clients now serve the token: the new one and the one the first collector still holds.
        assert_eq!(server.open_connections(), 2);
        assert_eq!(server.accepted(), 2);

        // The first collector's wait ends 30 s after its second read: its check finds the ticker registered (by the new
        // instance) and it goes on, beside the new collector, whose own wait ends a moment later.
        let until_waits_end = 28.0 - idle_since.elapsed().as_secs_f64();
        Timer::sleep(until_waits_end.max(0.)).await;
        wait_until("both collectors' reads", 6., || server.requests(CALL) == 6).await;
        Timer::sleep(2.).await;
        // Both collectors read the balance once in this interval, over two sessions: two reads, not one (R38.6.4Q.10
        // item 3 says exactly one collector and one client serve the token after an enable again).
        assert_eq!(server.requests(CALL), 6);
        assert_eq!(server.open_connections(), 2);
        assert_eq!(server.accepted(), 2);
        // No history request: the balance is unchanged for both.
        assert_eq!(server.requests(GET_HISTORY), 2);

        // The next idle interval repeats it: two more reads, one per collector.
        let calls_now = server.requests(CALL);
        Timer::sleep(32.).await;
        assert_eq!(server.requests(CALL), calls_now + 2);
        assert_eq!(server.open_connections(), 2);
    });
}

// ---- T38.6.4Q.7: no subscription to restore ----

#[test]
fn t38_6_4q_7_no_contract_event_subscription_at_activation_or_after_a_reconnect() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let qtum = init_qtum(&env, &[&server]).await;
        let token = init_token_named(&env, TICKER, &[&server], false).await;
        wait_until("two sessions", 10., || server.open_connections() == 2).await;
        assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 0);
        assert_eq!(server.accepted(), 2);

        // Every session is dropped; the clients reconnect.
        server.kill_connections();
        wait_until("both clients to reconnect", 60., || {
            server.accepted() >= 4 && server.open_connections() == 2
        })
        .await;
        // Give a subscription a moment to arrive, were any going to.
        Timer::sleep(2.).await;
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 0);
        // What a reconnect does place again: the tip subscription of each client (R38.6.4h).
        assert_eq!(server.header_subscriptions(), 2);
        // Activation, balance and the rest keep working over the new sessions.
        assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 0);
        eprintln!("Q7 observed: methods {:?}", server.request_methods_in_order());
        drop(qtum);
    });
}

// ---- T38.6.4Q.8: spends of the shared outputs ----

#[derive(Clone, Copy, Debug)]
enum Spend {
    QtumSwapSend,
    Token(&'static str),
}

/// Funds the one address with a single output of 10 QTUM, activates QTUM and both tokens over one stub that accepts
/// every broadcast and keeps listing that output, and runs `first` then `second`. Returns the funding transaction and
/// the transactions the stub was asked to broadcast, in order.
async fn run_spend_pair(first: Spend, second: Spend) -> (UtxoTx, Vec<UtxoTx>) {
    let env = family_env();
    let server = family_stub().await;
    let qtum = init_qtum(&env, &[&server]).await;
    let token_a = init_token_named(&env, TICKER, &[&server], false).await;
    let token_b = init_token_named(&env, TOKEN_B_TICKER, &[&server], false).await;
    let funding = fund(&server, &script_of(qtum.as_ref()), 99, &[1_000_000_000]);
    for spend in [first, second] {
        let outcome: Result<(), String> = match spend {
            Spend::QtumSwapSend => {
                let output = TransactionOutput {
                    value: 100_000_000,
                    script_pubkey: script_of(qtum.as_ref()),
                };
                utxo_common::send_outputs_from_my_address(qtum.clone(), vec![output])
                    .compat()
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("{e:?}"))
            },
            Spend::Token(ticker) => {
                let token = if ticker == TICKER { &token_a } else { &token_b };
                let call = token
                    .transfer_output(H160::from([0x11; 20]), U256::from(1u64), 100_000, 40)
                    .unwrap();
                token
                    .send_contract_calls(vec![call])
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("{e:?}"))
            },
        };
        outcome.unwrap_or_else(|e| panic!("{spend:?}: {e}"));
    }
    let txs = broadcast_txs(&server);
    drop((qtum, token_a, token_b));
    (funding, txs)
}

/// Asserts what the pair is expected to show in this tree: two broadcasts, both spending the one funding output (the
/// second does not avoid the first one's input).
async fn assert_second_spends_the_input_of_the_first(first: Spend, second: Spend) {
    let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (funding, txs) = run_spend_pair(first, second).await;
    assert_eq!(txs.len(), 2, "{first:?} then {second:?}");
    eprintln!(
        "Q8 observed {first:?} then {second:?}: inputs {:?} then {:?}; funding {:?}",
        inputs_of(&txs[0]),
        inputs_of(&txs[1]),
        outpoint(&funding, 0)
    );
    assert_eq!(inputs_of(&txs[0]), [outpoint(&funding, 0)]);
    // Differs from R38.6.4Q.7 item 6 only if the second selects another input; here it is the same one.
    assert_eq!(inputs_of(&txs[1]), [outpoint(&funding, 0)], "{first:?} then {second:?}");
}

#[test]
fn t38_6_4q_8_token_broadcast_then_qtum_swap_send_selects_the_same_input() {
    block_on(assert_second_spends_the_input_of_the_first(
        Spend::Token(TICKER),
        Spend::QtumSwapSend,
    ));
}

#[test]
fn t38_6_4q_8_qtum_swap_send_then_token_broadcast_selects_the_same_input() {
    block_on(assert_second_spends_the_input_of_the_first(
        Spend::QtumSwapSend,
        Spend::Token(TICKER),
    ));
}

#[test]
fn t38_6_4q_8_token_a_broadcast_then_token_b_broadcast_selects_the_same_input() {
    block_on(assert_second_spends_the_input_of_the_first(
        Spend::Token(TICKER),
        Spend::Token(TOKEN_B_TICKER),
    ));
}

#[test]
fn t38_6_4q_8_the_same_token_twice_selects_the_same_input() {
    block_on(assert_second_spends_the_input_of_the_first(
        Spend::Token(TICKER),
        Spend::Token(TICKER),
    ));
}

#[test]
fn t38_6_4q_8_control_two_qtum_swap_sends_in_a_row_avoid_the_input_and_spend_the_change() {
    block_on(async {
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (funding, txs) = run_spend_pair(Spend::QtumSwapSend, Spend::QtumSwapSend).await;
        assert_eq!(txs.len(), 2);
        eprintln!(
            "Q8 control observed: inputs {:?} then {:?}; first tx {}",
            inputs_of(&txs[0]),
            inputs_of(&txs[1]),
            txid_of(&txs[0])
        );
        assert_eq!(inputs_of(&txs[0]), [outpoint(&funding, 0)]);
        // The QTUM record of the first send replaced the spent output with the first send's outputs: the second send
        // spends one of them (T38.5B.1), not the funding output.
        assert!(inputs_of(&txs[1]).iter().all(|(hash, _)| *hash == txid_of(&txs[0])));
    });
}

#[test]
fn t38_6_4q_8_token_broadcast_and_qtum_swap_send_started_together_list_at_the_same_time_and_select_one_input() {
    block_on(async {
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let env = family_env();
        let server = family_stub().await;
        let qtum = init_qtum(&env, &[&server]).await;
        let token = init_token_named(&env, TICKER, &[&server], false).await;
        let funding = fund(&server, &script_of(qtum.as_ref()), 94, &[1_000_000_000]);
        let delay = Duration::from_millis(1_500);
        server.set_reply_delay(LIST_UNSPENT, delay);

        let token_send = async {
            let call = token
                .transfer_output(H160::from([0x11; 20]), U256::from(1u64), 100_000, 40)
                .unwrap();
            token
                .send_contract_calls(vec![call])
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        };
        let qtum_send = async {
            let output = TransactionOutput {
                value: 100_000_000,
                script_pubkey: script_of(qtum.as_ref()),
            };
            utxo_common::send_outputs_from_my_address(qtum.clone(), vec![output])
                .compat()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        };
        let watch = async {
            Timer::sleep(0.75).await;
            server.request_messages().iter().filter(|m| is_listing(m)).count()
        };
        let ((token_result, qtum_result), listings_halfway) =
            futures::join!(futures::future::join(token_send, qtum_send), watch);
        token_result.unwrap();
        qtum_result.unwrap();
        eprintln!("Q8 together observed: {listings_halfway} listing message(s) at 0.75 s");
        // Different locks (R38.6.4Q.7 items 1 and 3): the token's build holds the process-wide lock, the QTUM send the
        // lock of QTUM's own record. Both list while the delayed listings are pending, and both spend the one output.
        assert_eq!(listings_halfway, 2);
        let txs = broadcast_txs(&server);
        assert_eq!(txs.len(), 2);
        assert_eq!(inputs_of(&txs[0]), [outpoint(&funding, 0)]);
        assert_eq!(inputs_of(&txs[1]), [outpoint(&funding, 0)]);
    });
}

// ---- T38.6.4Q.11: the process-wide lock ----

#[test]
fn t38_6_4q_11_a_withdraw_of_another_coin_waits_for_the_pending_broadcast_of_a_token() {
    block_on(async {
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let env = family_env();
        let token_server = family_stub().await;
        let doc_server = family_stub().await;
        let token = init_token_named(&env, TICKER, &[&token_server], false).await;
        let doc = init_doc(&env, &[&doc_server]).await;
        let _funding = fund(&token_server, &script_of(token.as_ref()), 98, &[1_000_000_000]);
        let _doc_funding = fund(&doc_server, &script_of(doc.as_ref()), 97, &[1_000_000_000]);
        let delay = Duration::from_millis(2_000);
        token_server.set_reply_delay(BROADCAST, delay);

        let token_send = async {
            let call = token
                .transfer_output(H160::from([0x11; 20]), U256::from(1u64), 100_000, 40)
                .unwrap();
            let outcome = token.send_contract_calls(vec![call]).await;
            (outcome.is_ok(), std::time::Instant::now())
        };
        let doc_withdraw = async {
            // The token's broadcast is on the wire (its answer is delayed).
            wait_until("the token's broadcast", 10., || token_server.requests(BROADCAST) == 1).await;
            let started = std::time::Instant::now();
            let request = WithdrawRequest {
                coin: DOC_TICKER.to_owned(),
                from: None,
                to: doc.my_address().unwrap(),
                amount: BigDecimal::from_str("0.1").unwrap(),
                max: false,
                fee: None,
                memo: None,
            };
            let outcome = doc.withdraw(request).compat().await;
            (outcome.is_ok(), started, std::time::Instant::now())
        };
        let ((token_ok, token_done), (withdraw_ok, withdraw_started, withdraw_done)) =
            futures::join!(token_send, doc_withdraw);
        eprintln!(
            "Q11 observed: withdraw returned {:.2} s after it started, {:.2} s after the token's answer",
            withdraw_done.duration_since(withdraw_started).as_secs_f64(),
            withdraw_done.saturating_duration_since(token_done).as_secs_f64()
        );
        assert!(token_ok && withdraw_ok);
        // The withdraw of another coin, served by another stub, does not return before the token's broadcast has been
        // answered (R38.6.4Q.7 item 6, last point): it waits for the process-wide build lock.
        assert!(withdraw_done >= token_done);
        assert!(withdraw_done.duration_since(withdraw_started) >= delay - Duration::from_millis(600));
        // The withdraw did not broadcast anything, and the second stub saw its listing.
        assert_eq!(doc_server.requests(BROADCAST), 0);
        assert!(doc_server.requests(LIST_UNSPENT) >= 1);
    });
}

// ---- T38.6.4Q.9: delegation ----

/// The delegation contract of the Qtum network and the topics of its two events (as the delegation code spells them).
const QTUM_DELEGATE_CONTRACT: &str = "0000000000000000000000000000000000000086";
const QTUM_ADD_DELEGATION_TOPIC: &str = "a23803f3b2b56e71f2921c22b23c32ef596a439dbe03f7250e6b58a30eb910b5";
const QTUM_REMOVE_DELEGATION_TOPIC: &str = "7fe28d2d0b16cf95b5ea93f4305f89133b3892543e616381a1336fc1e7a01fa0";
const STAKER_HEX: &str = "6d9d2b554d768232320587df75c4338ecc8bf37d";

/// The three requests of the delegation status check, keyed by their parameters, for a wallet that added one delegation
/// and removed none: `(add history params, remove history params, receipt params)`.
fn serve_one_delegation(server: &StubServer, qtum: &QtumCoin) -> (Json, Json, Json) {
    let wallet = qtum.my_addr_as_contract_addr().unwrap();
    let wallet = json::to_value(contract_addr_into_rpc_format(&wallet)).unwrap();
    let delegate_contract = json::to_value(contract_addr_into_rpc_format(
        &H160::from_str(QTUM_DELEGATE_CONTRACT).unwrap(),
    ))
    .unwrap();
    let add = json!([wallet, delegate_contract, QTUM_ADD_DELEGATION_TOPIC]);
    let remove = json!([wallet, delegate_contract, QTUM_REMOVE_DELEGATION_TOPIC]);
    let mut add_tx = UtxoTx::default();
    add_tx.lock_time = 77;
    let receipt_log = json!({
        "address": QTUM_DELEGATE_CONTRACT,
        "topics": [QTUM_ADD_DELEGATION_TOPIC, format!("{:0>64}", STAKER_HEX), format!("{:0>64}", "11")],
        "data": "00",
    });
    server.set_params_result(
        GET_HISTORY,
        add.clone(),
        json!([{"tx_hash": txid_of(&add_tx), "height": 10, "log_index": 0}]),
    );
    server.set_params_result(GET_HISTORY, remove.clone(), json!([]));
    let receipt = json!([txid_of(&add_tx)]);
    server.set_params_result(
        GET_RECEIPT,
        receipt.clone(),
        receipt_answer(&add_tx, 10, vec![receipt_log]),
    );
    (add, remove, receipt)
}

#[test]
fn t38_6_4q_9_delegation_status_check_asks_the_history_twice_and_the_receipt_once_and_an_error_answer_fails_it() {
    block_on(async {
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let env = family_env();
        let server = family_stub().await;
        let qtum = init_qtum(&env, &[&server]).await;
        let _funding = fund(&server, &script_of(qtum.as_ref()), 96, &[15_000_000_000]);
        let (add, remove, receipt) = serve_one_delegation(&server, &qtum);

        // (a) The check succeeds: add history, remove history, then (more added than removed) the receipt.
        let infos = qtum.get_delegation_infos().compat().await.unwrap();
        let StakingInfosDetails::Qtum(details) = infos.staking_infos_details;
        assert!(details.am_i_staking);
        assert!(
            details.staker.is_some(),
            "the staker comes from the receipt's second topic"
        );
        // The listing of the unspents follows (the amount staked is read from it): 150 QTUM at or over the bound.
        assert_eq!(details.amount, BigDecimal::from_str("150").unwrap());
        assert_eq!(
            server
                .request_log()
                .into_iter()
                .filter(|(method, _)| matches!(method.as_str(), GET_HISTORY | GET_RECEIPT))
                .collect::<Vec<_>>(),
            [
                (GET_HISTORY.to_owned(), add.clone()),
                (GET_HISTORY.to_owned(), remove.clone()),
                (GET_RECEIPT.to_owned(), receipt.clone()),
            ]
        );

        // An error answer to any of the three fails the request (nothing is repeated and no other result is made up).
        let error = json!({"code": 3, "message": "stub delegation error"});
        for (method, params) in [(GET_HISTORY, &add), (GET_HISTORY, &remove), (GET_RECEIPT, &receipt)] {
            server.set_params_error(method, params.clone(), error.clone());
            let outcome = qtum.get_delegation_infos().compat().await;
            let message = outcome.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(message.contains("stub delegation error"), "{method}: {message}");
            // Back to what it was.
            server.clear_params_rules(method);
            serve_one_delegation(&server, &qtum);
        }
        assert!(qtum.get_delegation_infos().compat().await.is_ok());
    });
}

#[test]
fn t38_6_4q_9_delegation_and_withdraw_from_one_address_list_one_after_the_other() {
    block_on(async {
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let env = family_env();
        let server = family_stub().await;
        server.set_result(GET_HISTORY, json!([]));
        let qtum = init_qtum(&env, &[&server]).await;
        let funding = fund(&server, &script_of(qtum.as_ref()), 95, &[1_000_000_000]);
        let listings = |server: &StubServer| server.request_messages().iter().filter(|m| is_listing(m)).count();
        let add_delegation = || async {
            let request = QtumDelegationRequest {
                address: "qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE".to_owned(),
                fee: Some(10),
            };
            qtum.add_delegation(request).compat().await.map_err(|e| e.to_string())
        };
        let withdraw = || async {
            let request = WithdrawRequest {
                coin: QTUM_TICKER.to_owned(),
                from: None,
                to: qtum.my_address().unwrap(),
                amount: BigDecimal::from_str("0.1").unwrap(),
                max: false,
                fee: None,
                memo: None,
            };
            qtum.withdraw(request).compat().await.map_err(|e| e.to_string())
        };

        // Alone, without a delay: how many listings (messages with a `listunspent`) each build makes.
        let before = listings(&server);
        withdraw().await.unwrap();
        let withdraw_listings = listings(&server) - before;
        let before = listings(&server);
        add_delegation().await.unwrap();
        let delegation_listings = listings(&server) - before;
        eprintln!(
            "Q9b observed: alone, a withdraw lists {withdraw_listings} time(s), a delegation {delegation_listings}"
        );
        assert!(withdraw_listings >= 1 && delegation_listings >= 1);

        // Together, with every listing delayed: the first listing is the only one for as long as it takes, and the
        // builds as a whole take as long as all their listings one after the other.
        let delay = Duration::from_millis(1_500);
        server.set_reply_delay(LIST_UNSPENT, delay);
        let before = listings(&server);
        let started = std::time::Instant::now();
        let watch = async {
            Timer::sleep(0.75).await;
            listings(&server) - before
        };
        let (((delegation, _), (withdraw, _)), listings_halfway) = futures::join!(
            futures::future::join(async { (add_delegation().await, std::time::Instant::now()) }, async {
                (withdraw().await, std::time::Instant::now())
            }),
            watch
        );
        let elapsed = started.elapsed().as_secs_f64();
        let listings_in_all = listings(&server) - before;
        eprintln!(
            "Q9b observed: together, {listings_halfway} listing(s) at 0.75 s, {listings_in_all} in all, {elapsed:.1} s"
        );
        let (delegation, withdraw) = (delegation.unwrap(), withdraw.unwrap());
        // With the lock (PR #354) the second build lists only after the first has signed: one listing at 0.75 s, all of
        // them one after the other (differs from the reference text of T38.6.4Q.9 (b), where both list at the same time).
        assert_eq!(listings_halfway, 1);
        assert_eq!(listings_in_all, withdraw_listings + delegation_listings);
        assert!(
            elapsed >= (withdraw_listings + delegation_listings) as f64 * 1.5 * 0.9,
            "{elapsed}"
        );
        // Neither build broadcasts, neither enters a record: both spend the one funding output.
        assert_eq!(server.requests(BROADCAST), 0);
        let delegation_tx: UtxoTx = deserialize(delegation.tx_hex.0.as_slice()).unwrap();
        let withdraw_tx: UtxoTx = deserialize(withdraw.tx_hex.0.as_slice()).unwrap();
        assert_eq!(inputs_of(&delegation_tx), [outpoint(&funding, 0)]);
        assert_eq!(inputs_of(&withdraw_tx), [outpoint(&funding, 0)]);
    });
}

/// Whether a message the stub received lists unspent outputs.
fn is_listing(message: &Json) -> bool {
    let lists = |request: &Json| request["method"] == json!(LIST_UNSPENT);
    match message {
        Json::Array(batch) => batch.iter().any(lists),
        single => lists(single),
    }
}

/// The call the balance stream of `mm2_main` makes for a token (`qrc20_subscription_target` and
/// `blockchain_contract_event_subscribe`), made here on the token's client: the wallet's contract-format address, the
/// token contract and the `Transfer` topic. The stream itself (the enabling of it) is not covered here.
#[test]
fn t38_6_4q_7_contract_event_subscription_of_the_balance_stream_leaves_the_rest_working_and_is_not_placed_again() {
    block_on(async {
        let env = family_env();
        let server = family_stub().await;
        let token = init_token_named(&env, TICKER, &[&server], false).await;
        let UtxoRpcClientEnum::Electrum(client) = token.utxo.rpc_client.clone() else {
            panic!("an Electrum activation builds an Electrum client")
        };
        let address = contract_addr_into_rpc_format(&token.my_addr_as_contract_addr().unwrap());
        let contract = contract_addr_into_rpc_format(&token.contract_address);
        let wanted = json!([
            json::to_value(&address).unwrap(),
            json::to_value(&contract).unwrap(),
            QRC20_TRANSFER_TOPIC
        ]);
        let subscribe = || {
            client
                .blockchain_contract_event_subscribe(&address, &contract, QRC20_TRANSFER_TOPIC)
                .compat()
        };

        // An error answer: the request fails, and balance and history keep working.
        server.set_error(EVENT_SUBSCRIBE, "stub subscribe error");
        let error = subscribe().await.unwrap_err().to_string();
        assert!(error.contains("stub subscribe error"), "{error}");
        assert_eq!(server.request_params(EVENT_SUBSCRIBE), [wanted.clone()]);
        assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
        server.set_result(GET_HISTORY, json!([]));
        assert!(matches!(
            token.request_tx_history(env.ctx.metrics.clone()).await,
            RequestTxHistoryResult::Ok(_)
        ));

        // No answer at all: the request stays pending, and balance and history keep working meanwhile.
        server.clear_error(EVENT_SUBSCRIBE);
        server.set_reply_delay(EVENT_SUBSCRIBE, Duration::from_secs(3_600));
        let pending = futures::future::select(Box::pin(subscribe()), Box::pin(Timer::sleep(1.5)));
        let rest = async {
            Timer::sleep(0.3).await;
            assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
            assert!(matches!(
                token.request_tx_history(env.ctx.metrics.clone()).await,
                RequestTxHistoryResult::Ok(_)
            ));
        };
        let (outcome, ()) = futures::join!(pending, rest);
        assert!(matches!(outcome, futures::future::Either::Right(_)), "no answer came");
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 2);

        // An answer: the request succeeds. After a reconnect the client places nothing again.
        server.set_reply_delay(EVENT_SUBSCRIBE, Duration::ZERO);
        subscribe().await.unwrap();
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 3);
        server.kill_connections();
        wait_until("the client to reconnect", 60., || {
            server.accepted() >= 2 && server.open_connections() == 1
        })
        .await;
        assert_eq!(token.my_balance().compat().await.unwrap().spendable, balance_of(1_000));
        Timer::sleep(2.).await;
        assert_eq!(server.requests(EVENT_SUBSCRIBE), 3);
    });
}
