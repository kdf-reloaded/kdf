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
use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
use crate::utxo::rpc_clients::with_call_budget;
use crate::{disable_coin, lp_coinfind, lp_coininit, MmCoinEnum};
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
        let RequestTxHistoryResult::Ok(ids, _) = coin.request_tx_history(env.ctx.metrics.clone()).await else {
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
        assert!(!coin.check_if_history_update_is_needed(
            &history_map,
            &Some(balance.clone()),
            &balance,
            &AbsenceMarks::default(),
            false
        ));
        // A changed balance asks for the history again, and so does the first pass.
        let other = CoinBalance {
            spendable: balance_of(2),
            unspendable: BigDecimal::from(0),
        };
        assert!(coin.check_if_history_update_is_needed(
            &history_map,
            &Some(balance.clone()),
            &other,
            &AbsenceMarks::default(),
            false
        ));
        assert!(coin.check_if_history_update_is_needed(&history_map, &None, &balance, &AbsenceMarks::default(), false));
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

// ---- CRD ch.38 §38.5C (D38.5C.1 and D38.5C.2 status notes): rows the listing no longer holds, changed heights ----

/// One stored transfer row of the transaction `hash_byte`.
fn stored_row(hash_byte: u8, log_index: u64, block_height: u64) -> (H256Json, TxInternalId, crate::TransactionDetails) {
    let tx_hash = H256Json::from([hash_byte; 32]);
    let id = TxInternalId::new(tx_hash, 0, log_index);
    let row = utxo_common::history_row_for_test(block_height, BytesJson::from(id.clone()).0);
    (tx_hash, id, row)
}

fn history_map_of(rows: Vec<(H256Json, TxInternalId, crate::TransactionDetails)>) -> HistoryMapByHash {
    let mut map = HistoryMapByHash::new();
    for (hash, id, row) in rows {
        map.entry(hash).or_default().insert(id, row);
    }
    map
}

fn listing(entries: &[(u8, u64)]) -> TxIds {
    entries
        .iter()
        .map(|(byte, height)| (H256Json::from([*byte; 32]), *height))
        .collect()
}

fn rows_in(map: &HistoryMapByHash) -> usize { map.values().map(|rows| rows.len()).sum() }

#[test]
fn t38_5c_a_confirmed_transaction_absent_from_two_listings_one_block_apart_is_removed_with_all_its_rows() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        // A has one row; B is a transaction with two transfer rows.
        let mut map = history_map_of(vec![
            stored_row(0xa1, 0, 100),
            stored_row(0xb2, 0, 90),
            stored_row(0xb2, 1, 90),
        ]);
        let mut absence = AbsenceMarks::default();
        let only_a = listing(&[(0xa1, 100)]);
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 800, &only_a));
        assert_eq!(rows_in(&map), 3);
        assert!(coin.remove_unlisted_rows(&mut absence, &mut map, 801, &only_a));
        assert_eq!(rows_in(&map), 1, "both rows of B went together");
        assert!(map.contains_key(&H256Json::from([0xa1; 32])));
        assert!(!absence.any());
    });
}

#[test]
fn t38_5c_an_unconfirmed_transaction_follows_the_same_rule() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let mut map = history_map_of(vec![stored_row(0xe5, 0, 0)]);
        let mut absence = AbsenceMarks::default();
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 800, &listing(&[])));
        assert!(coin.remove_unlisted_rows(&mut absence, &mut map, 801, &listing(&[])));
        assert_eq!(rows_in(&map), 0);
    });
}

#[test]
fn t38_5c_one_absence_or_two_at_the_same_tip_keep_the_row_and_the_mark_keeps_the_need_to_run_test_true() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let mut map = history_map_of(vec![stored_row(0xb2, 0, 90)]);
        let mut absence = AbsenceMarks::default();
        let balance = CoinBalance {
            spendable: balance_of(1),
            unspendable: BigDecimal::from(0),
        };
        // No mark, an unchanged balance and a settled row: no pass.
        assert!(!coin.check_if_history_update_is_needed(&map, &Some(balance.clone()), &balance, &absence, false));
        // A chain-changed event (CRD ch.38 §38.5C amendment of 2026-11-18, item 3) forces the pass whatever the rest says.
        assert!(coin.check_if_history_update_is_needed(&map, &Some(balance.clone()), &balance, &absence, true));

        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 800, &listing(&[])));
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 800, &listing(&[])));
        assert_eq!(rows_in(&map), 1);
        // The mark: the pass runs although the balance is unchanged.
        assert!(coin.check_if_history_update_is_needed(&map, &Some(balance.clone()), &balance, &absence, false));
    });
}

#[test]
fn t38_5c_a_listing_that_contains_the_transaction_again_clears_the_mark() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let mut map = history_map_of(vec![stored_row(0xb2, 0, 90)]);
        let mut absence = AbsenceMarks::default();
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 800, &listing(&[])));
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, 801, &listing(&[(0xb2, 90)])));
        assert!(!absence.any());
        assert_eq!(rows_in(&map), 1);
    });
}

/// A failed, cut-off or `history too large` listing gives no tip, so it never reaches the rule.
#[test]
fn t38_5c_a_listing_that_failed_or_was_too_large_carries_no_tip_and_a_good_one_carries_the_tip_of_its_server() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();

        server.set_error(GET_HISTORY, "stub history error");
        assert!(matches!(
            coin.request_tx_history(env.ctx.metrics.clone()).await,
            RequestTxHistoryResult::Retry { .. }
        ));
        server.clear_error(GET_HISTORY);
        server.set_error_object(GET_HISTORY, json!({"code": 1, "message": "history too large"}));
        assert!(matches!(
            coin.request_tx_history(env.ctx.metrics.clone()).await,
            RequestTxHistoryResult::HistoryTooLarge
        ));

        // One connected server: its tip with each listing, and a new block moves it.
        server.clear_error(GET_HISTORY);
        server.set_result(GET_HISTORY, json!([]));
        let RequestTxHistoryResult::Ok(_, first) = coin.request_tx_history(env.ctx.metrics.clone()).await else {
            panic!("a list is a history")
        };
        assert_eq!(first, Some(777));
        // That a new block moves the tip of the single server is tested on the pool
        // (`t38_5c_one_connected_server_gives_its_tip_for_each_listing`); here the next tip is taken as given.
        let second = first.map(|tip| tip + 1);
        // Two listings from the single server, one block apart, remove the row.
        let mut map = history_map_of(vec![stored_row(0xb2, 0, 90)]);
        let mut absence = AbsenceMarks::default();
        assert!(!coin.remove_unlisted_rows(&mut absence, &mut map, first.unwrap(), &listing(&[])));
        assert!(coin.remove_unlisted_rows(&mut absence, &mut map, second.unwrap(), &listing(&[])));
        assert_eq!(rows_in(&map), 0);
    });
}

#[test]
fn t38_5c_a_changed_height_is_taken_over_with_the_new_time_and_an_unconfirmed_listing_resets_all_rows() {
    block_on(async {
        let env = token_env(Some(8));
        let server = token_stub(1).await;
        let coin = init_token(&env, &[&server], false).await.unwrap();
        let (mut a, b) = two_served_txs(&coin);
        serve_tx(&server, &a);
        serve_tx(&server, &b);
        let id = |served: &ServedTx| json::from_value::<H256Json>(json!(txid_of(&served.tx))).unwrap();
        let mut history_map = HistoryMapByHash::new();
        let tx_ids = vec![(id(&a), a.block), (id(&b), b.block)];
        assert!(coin.process_tx_ids(&env.ctx, &mut history_map, tx_ids).await);
        assert_eq!(rows_in(&history_map), 3);

        // The block of A changed, and its block has another time; B (two rows) is listed as unconfirmed.
        a.block = 710;
        a.time = 1_700_000_999;
        serve_tx(&server, &a);
        let tx_ids = vec![(id(&a), 710), (id(&b), 0)];
        assert!(coin.process_tx_ids(&env.ctx, &mut history_map, tx_ids).await);

        let rows_of = |served: &ServedTx| -> Vec<crate::TransactionDetails> {
            history_map[&id(served)].values().cloned().collect()
        };
        let a_rows = rows_of(&a);
        assert_eq!(a_rows.len(), 1);
        assert_eq!((a_rows[0].block_height, a_rows[0].timestamp), (710, 1_700_000_999));
        let b_rows = rows_of(&b);
        assert_eq!(b_rows.len(), 2);
        assert!(b_rows.iter().all(|row| row.block_height == 0), "{b_rows:?}");
    });
}
