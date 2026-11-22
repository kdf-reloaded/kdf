//! Tests of the Solana transaction history (CRD ch.40 §40.6C, T40.6C.2 to T40.6C.8): the row mapping of
//! `history_decode`, the paging and the states of the writer in `history`, and the answers of both `my_tx_history`
//! methods once the lists are written.
//!
//! The transactions, signature listings, block heights and token accounts are public Solana devnet data, captured by
//! JSON-RPC on 2026-11-22 into `test_fixtures/devnet_history.json` (see its `_source` member). The wallet is the
//! devnet lab wallet of the fixtures; its transactions are a send, a receive, a send to itself, an SPL withdraw that
//! created the receiver's token account, an SPL send to an existing account, and an SPL receive in which the token
//! account is created. The failed transaction and the version-0 transaction with loaded addresses are public
//! transactions of other devnet accounts (no wallet of the lab has such transactions): a test names the account whose
//! row it checks, the fee payer in both cases. Nothing in the fixtures is hand-built, except where a test marks its
//! scripted answer as altered (the unreadable transaction of T40.6C.7 and the block without height).
//!
//! A stub node on a loopback socket serves the fixtures, so the real HTTP layer, node pool and RPC client run. No
//! assertion message carries a key-derived value: values are compared with `assert!(a == b, "<fixed text>")`.

use super::history::{history_loop, run_pass_and_report, HistoryConfig, WalkState};
use super::history_decode::{parse_transaction, TokenSpec};
use super::rpc_client::ConfirmedTransaction;
use super::solana_common_tests::generate_key_pair_from_iguana_seed;
use super::spl::SplToken;
use super::*;
use crate::my_tx_history_v2::{my_tx_history_v2_rpc, MyTxHistoryErrorV2, MyTxHistoryRequestV2};
use crate::{CoinsContext, MmCoinEnum, TransactionType};
use common::block_on;
use common::executor::Timer;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const FIXTURES: &str = include_str!("test_fixtures/devnet_history.json");
const WALLET: &str = "Do7RKwFqtSJmUVCMvtyVjF27dSwinSAEUcnpednUtYQ8";
const MINT: &str = "HYTTzUoh6pUZCSGv1Bdo2tWmm6jHtPBtpj7XJfyUjAdn";
const TOKEN_TICKER: &str = "LABTOK";
const PEER: &str = "J6gvGE3E8uj71qYjRTQbpd3eg6Vxq5i4JsH5EmcnDsAZ";
const FAUCET: &str = "dev2JBjyB5CshoGsiJCwzdmJYiEUwAXMdqDR7txoFBJ";
/// The fee payer of the failed transaction `3ww7iVTU…`.
const FAILED_PAYER: &str = "AVm1wwtnLtsdJSpgz8S2SfC4JDepipEwBpLYuHgQ25Yb";
/// The fee payer of the version-0 transaction `5p6b7Xuk…`.
const V0_PAYER: &str = "FibsNjdfTKoaUEKcXg87ioaQ46JExKcvK4qY8ndiLQej";

const TX_SEND: &str = "fcNdqoBDdG8Z5yyj3Csd66fuHpd2cxngQkDgBNVoDgaF8LNfAi99brzg9WVw1YDuVtM6t1cGfqfRQ9ZB1MD9DNR";
const TX_RECEIVE: &str = "5WEumBmji5V5ZBDnDUKvhzhbMCKjqzx7LVeKYo2sSXeQQbywHzAjjXSouktBTYDto5EXmq4WaLGd6x8iy6y2Fp9N";
const TX_SELF: &str = "3Ae5WG6QubFeJ85FsGPdCeq4N6GEad5JjcLAZH3G4FPR7ji8p6n1uGvrFuEbxizV7oVjuW1eGUJdxFPQxfqEZtP9";
const TX_SPL_NEW_ACCOUNT: &str =
    "3TiTPDcmpfnb7EFsfeK6JBLLxJUhyBTjkG5Xt9JW9uXzFMQkQ7dS6i25RTyHUAJF448eVsuis4CoHaEieRWhvg1k";
const TX_SPL_SEND: &str = "4XP43b8akVpG1KXD2hfLhDyUmi961y8qt1S6LdfazByc9qr7hot9ZYQpqsYstbEXV9oM6BmtcgzWqXqn9TjoDDpw";
const TX_SPL_RECEIVE: &str = "413V1nuQY8xayonQqmRjYgKKozMyrMTC6aoeDqsyiuxtjdVvbioZ87aCiQZFgxFSnNFagZFmyodM2FJn5zsrd2Cz";
const TX_FAILED: &str = "3ww7iVTUn4vPEY6nf59CaGABGYmptRW7YXZ7vERu6TTK1YVFjGToNRaoxfgjQ9hwFqdhAoXeSvYPoe1gpr3JsFvh";
const TX_V0: &str = "5p6b7Xukgv3Fc6TLLBMcBSpZxmxZnD7eoe5dEFwTyzb7GBsk7uosxnkzdRWRHFoc4gtrLVswte4XiFWhrpFadaHG";
const TX_FIRST_FUNDING: &str =
    "34YyKn5QKk5Uugz6o44Qg66eFAmqbbVP6Ern6XWCA3rqTFxeZr3CiMdLgXVENi6fS1UQUJFNKq1RkJD8mAtSvmkd";

fn fixtures() -> Json { serde_json::from_str(FIXTURES).expect("the fixture file parses") }

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).expect("a decimal") }

fn token_spec() -> TokenSpec {
    TokenSpec {
        ticker: TOKEN_TICKER.to_owned(),
        mint: Pubkey::from_str(MINT).expect("a key"),
        decimals: 6,
    }
}

fn answer_of(signature: &str) -> ConfirmedTransaction {
    serde_json::from_value(fixtures()["transactions"][signature].clone()).expect("a getTransaction answer")
}

/// The rows of a getTransaction answer `answer` (a JSON value) with the signature `signature`, for the wallet.
fn rows_of_value(answer: Json, signature: &str, tokens: &[TokenSpec]) -> Vec<(String, TransactionDetails)> {
    let answer: ConfirmedTransaction = serde_json::from_value(answer).expect("a getTransaction answer");
    let parsed = parse_transaction(&answer, signature).expect("the transaction decodes");
    let me = Pubkey::from_str(WALLET).expect("a key");
    let height = fixtures()["blocks"][answer.slot.to_string()]["blockHeight"]
        .as_u64()
        .expect("a block height");
    parsed.rows(&me, "SOL", tokens, height).expect("the rows build")
}

/// The rows of the fixture transaction `signature` for `me`, with the block height of its slot.
fn rows_of(signature: &str, me: &str, tokens: &[TokenSpec]) -> Vec<(String, TransactionDetails)> {
    let answer = answer_of(signature);
    let parsed = parse_transaction(&answer, signature).expect("the transaction decodes");
    let me = Pubkey::from_str(me).expect("a key");
    let height = fixtures()["blocks"][answer.slot.to_string()]["blockHeight"]
        .as_u64()
        .expect("a block height");
    parsed.rows(&me, "SOL", tokens, height).expect("the rows build")
}

fn sol_row(signature: &str, me: &str) -> TransactionDetails {
    let rows = rows_of(signature, me, &[]);
    let [(ticker, row)] = <[_; 1]>::try_from(rows).unwrap_or_else(|_| panic!("exactly one row"));
    assert!(ticker == "SOL", "the row is the platform's");
    row
}

fn token_row(signature: &str) -> TransactionDetails {
    let rows = rows_of(signature, WALLET, &[token_spec()]);
    rows.into_iter()
        .find(|(ticker, _)| ticker == TOKEN_TICKER)
        .map(|(_, row)| row)
        .expect("a token row")
}

fn fee_of(row: &TransactionDetails) -> SolanaFeeDetails {
    match &row.fee_details {
        Some(crate::TxFeeDetails::Solana(fee)) => fee.clone(),
        _ => panic!("the fee details are the Solana form"),
    }
}

/// R40.6C.7: the identity `my_balance_change == received_by_me - spent_by_me` holds on every row.
fn assert_identity(row: &TransactionDetails) {
    assert!(
        row.my_balance_change == &row.received_by_me - &row.spent_by_me,
        "my_balance_change is received minus spent"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.4: SOL rows
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6c_4_a_send_is_a_row_with_the_fee_in_spent() {
    let row = sol_row(TX_SEND, WALLET);
    assert_identity(&row);
    assert!(row.tx_hash == TX_SEND, "the hash is the signature in base58");
    assert!(row.internal_id.0.len() == 64, "the id is the 64 signature bytes");
    assert!(row.block_height == 512018331, "the height of the slot");
    assert!(row.timestamp == 1795313330, "the block time");
    assert!(row.total_amount.to_string() == "0.001", "the transferred lamports");
    assert!(
        row.spent_by_me.to_string() == "0.001005000",
        "the transfer and the fee, full decimals"
    );
    assert!(row.received_by_me == dec("0"), "nothing received");
    assert!(
        row.my_balance_change.to_string() == "-0.001005000",
        "the change of the balance"
    );
    assert!(
        row.from == vec![WALLET.to_owned()] && row.to == vec![PEER.to_owned()],
        "from and to"
    );
    let fee = fee_of(&row);
    assert!(fee.fee_amount.to_string() == "0.000005", "the network fee");
    assert!(fee.rent_amount == dec("0"), "no rent");
    assert!(fee.total_amount.to_string() == "0.000005000", "the sum, full decimals");
    assert!(
        matches!(row.transaction_type, TransactionType::StandardTransfer),
        "a standard transfer"
    );
    assert!(!row.tx_hex.0.is_empty(), "the wire bytes");
}

#[test]
fn t40_6c_4_a_receive_is_a_row_without_a_fee() {
    let row = sol_row(TX_RECEIVE, WALLET);
    assert_identity(&row);
    assert!(row.total_amount == dec("5"), "the transferred amount");
    assert!(row.received_by_me == dec("5"), "received");
    assert!(
        row.spent_by_me == dec("0") && row.spent_by_me.to_string() == "0.000000000",
        "nothing spent"
    );
    assert!(row.my_balance_change == dec("5"), "the change");
    assert!(
        row.from == vec![FAUCET.to_owned()] && row.to == vec![WALLET.to_owned()],
        "from and to"
    );
    let fee = fee_of(&row);
    assert!(
        fee.fee_amount == dec("0") && fee.rent_amount == dec("0"),
        "the payer is another account"
    );
}

#[test]
fn t40_6c_4_the_first_funding_is_a_receive_too() {
    let row = sol_row(TX_FIRST_FUNDING, WALLET);
    assert_identity(&row);
    assert!(
        row.received_by_me == dec("5") && row.spent_by_me == dec("0"),
        "a receive"
    );
}

#[test]
fn t40_6c_4_a_send_to_self_has_the_figures_of_its_withdraw_answer() {
    let row = sol_row(TX_SELF, WALLET);
    assert_identity(&row);
    assert!(row.total_amount == dec("0.001"), "the lamports of the self transfer");
    assert!(row.received_by_me == dec("0.001"), "the amount is received");
    assert!(row.spent_by_me == dec("0.001005"), "the amount and the fee are spent");
    assert!(row.my_balance_change == dec("-0.000005"), "only the fee is lost");
    assert!(
        row.from == vec![WALLET.to_owned()] && row.to == vec![WALLET.to_owned()],
        "from and to"
    );
}

#[test]
fn t40_6c_4_an_spl_withdraw_that_creates_the_receivers_account_pays_rent() {
    let row = sol_row(TX_SPL_NEW_ACCOUNT, WALLET);
    assert_identity(&row);
    let fee = fee_of(&row);
    assert!(fee.fee_amount == dec("0.000005"), "the fee");
    assert!(fee.rent_amount == dec("0.00148844"), "the rent of the created account");
    assert!(
        fee.total_amount.to_string() == "0.001493440",
        "their sum, full decimals"
    );
    assert!(
        matches!(row.transaction_type, TransactionType::FeeForTokenTx),
        "the fee of a token transaction"
    );
    assert!(
        row.total_amount == dec("0.00148844"),
        "no transfer names the address: the change less the fee"
    );
    assert!(row.spent_by_me == dec("0.00149344"), "fee and rent");
    assert!(
        row.from == vec![WALLET.to_owned()] && row.to == vec![WALLET.to_owned()],
        "from and to"
    );
}

#[test]
fn t40_6c_4_an_spl_send_to_an_existing_account_is_a_fee_for_a_token_transaction() {
    let row = sol_row(TX_SPL_SEND, WALLET);
    assert_identity(&row);
    assert!(row.total_amount == dec("0"), "no lamport moved");
    assert!(row.spent_by_me == dec("0.000005"), "the fee");
    assert!(
        matches!(row.transaction_type, TransactionType::FeeForTokenTx),
        "the fee of a token transaction"
    );
}

#[test]
fn t40_6c_4_a_transaction_that_only_changes_a_token_account_is_no_sol_row() {
    // The receive of tokens: the wallet is a read-only account of it and pays nothing.
    let rows = rows_of(TX_SPL_RECEIVE, WALLET, &[]);
    assert!(rows.is_empty(), "no SOL row");
}

/// A failed transaction is a row of its fee payer: only the fee changes (R40.6C.10).
#[test]
fn t40_6c_4_a_failed_transaction_is_a_fee_only_row() {
    let rows = rows_of(TX_FAILED, FAILED_PAYER, &[token_spec()]);
    assert!(rows.len() == 1, "one row and no token row");
    let (ticker, row) = &rows[0];
    assert!(ticker == "SOL", "the platform's");
    assert_identity(row);
    assert!(row.total_amount == dec("0"), "nothing moved");
    assert!(
        row.spent_by_me == dec("0.000016033") && row.received_by_me == dec("0"),
        "the fee only"
    );
    let fee = fee_of(row);
    assert!(
        fee.fee_amount == dec("0.000016033") && fee.rent_amount == dec("0"),
        "the fee"
    );
    assert!(
        matches!(row.transaction_type, TransactionType::StandardTransfer),
        "a standard transfer"
    );
}

/// A version-0 transaction whose program is a loaded address: its inner System creations are read through the
/// account list of R40.6C.6.
#[test]
fn t40_6c_4_a_version_0_transaction_reads_its_loaded_addresses() {
    let row = sol_row(TX_V0, V0_PAYER);
    assert_identity(&row);
    let fee = fee_of(&row);
    assert!(fee.fee_amount == dec("0.0000054"), "the fee");
    assert!(fee.rent_amount == dec("0.03938524"), "the three funded accounts");
    assert!(row.spent_by_me == dec("0.03939064"), "fee and funded accounts");
    assert!(
        row.total_amount == dec("0.03938524"),
        "no transfer names the address: the change less the fee"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.5: SPL rows
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6c_5_an_spl_send_is_a_token_row_without_the_fee() {
    let row = token_row(TX_SPL_SEND);
    assert_identity(&row);
    assert!(row.coin == TOKEN_TICKER, "the token's coin");
    assert!(
        row.total_amount == dec("1.5") && row.spent_by_me == dec("1.5"),
        "the amount"
    );
    assert!(row.received_by_me == dec("0"), "nothing received");
    assert!(row.my_balance_change == dec("-1.5"), "the change");
    assert!(
        row.from == vec![WALLET.to_owned()] && row.to == vec![PEER.to_owned()],
        "owners, not accounts"
    );
    let fee = fee_of(&row);
    assert!(fee.fee_amount == dec("0.000005"), "the SOL fee");
    assert!(
        matches!(&row.transaction_type, TransactionType::TokenTransfer(id) if id.0.len() == 32),
        "a token transfer with the token's id"
    );
}

/// A send of the token to the wallet itself: a token row with a change of 0 and the figures of the withdraw answer
/// (R40.6C.8). The transaction is HAND-BUILT (fixture member `hand_built`): the captured SPL send with its
/// destination changed to the wallet's own token account. Its SOL row is the fee only.
#[test]
fn t40_6c_5_an_spl_send_to_self_is_a_token_row_with_a_change_of_zero() {
    let answer = fixtures()["hand_built"]["spl_self_send"].clone();
    let rows = rows_of_value(answer, TX_SPL_SEND, &[token_spec()]);
    assert!(rows.len() == 2, "a SOL row and a token row");
    let row = &rows
        .iter()
        .find(|(ticker, _)| ticker == TOKEN_TICKER)
        .expect("a token row")
        .1;
    assert_identity(row);
    assert!(row.my_balance_change == dec("0"), "a change of 0");
    assert!(
        row.received_by_me == dec("1.5") && row.spent_by_me == dec("1.5") && row.total_amount == dec("1.5"),
        "received, spent and total are the amount"
    );
    assert!(
        row.from == vec![WALLET.to_owned()] && row.to == vec![WALLET.to_owned()],
        "from and to are the wallet"
    );
    assert!(
        matches!(&row.transaction_type, TransactionType::TokenTransfer(id) if id.0.len() == 32),
        "a token transfer with the token's id"
    );
    assert!(fee_of(row).fee_amount == dec("0.000005"), "the SOL fee");
    let sol = &rows.iter().find(|(ticker, _)| ticker == "SOL").expect("a SOL row").1;
    assert_identity(sol);
    assert!(
        sol.my_balance_change == dec("-0.000005") && sol.received_by_me == dec("0"),
        "the SOL row is the fee only"
    );
}

/// A failed transaction that carries a token transfer instruction of the mint gives no token row (R40.6C.10). The
/// answer is the hand-built self send with `meta.err` set (altered answer).
#[test]
fn t40_6c_5_a_failed_transaction_with_a_token_transfer_gives_no_token_row() {
    let mut answer = fixtures()["hand_built"]["spl_self_send"].clone();
    answer["meta"]["err"] = json!({ "InstructionError": [0, "InsufficientFunds"] });
    let rows = rows_of_value(answer, TX_SPL_SEND, &[token_spec()]);
    assert!(rows.iter().all(|(ticker, _)| ticker == "SOL"), "no token row");
}

#[test]
fn t40_6c_5_a_send_that_creates_the_receivers_account_carries_the_rent_in_fee_details() {
    let row = token_row(TX_SPL_NEW_ACCOUNT);
    assert_identity(&row);
    assert!(
        row.spent_by_me == dec("1.5"),
        "the fee and the rent are no part of the token amount"
    );
    let fee = fee_of(&row);
    assert!(fee.rent_amount == dec("0.00148844"), "the rent");
}

#[test]
fn t40_6c_5_a_receive_in_which_the_account_is_created() {
    let row = token_row(TX_SPL_RECEIVE);
    assert_identity(&row);
    assert!(
        row.received_by_me == dec("1000") && row.total_amount == dec("1000"),
        "the amount"
    );
    assert!(row.spent_by_me == dec("0"), "nothing spent");
    assert!(row.my_balance_change == dec("1000"), "the change");
    assert!(row.to == vec![WALLET.to_owned()], "to the wallet");
    let fee = fee_of(&row);
    assert!(
        fee.fee_amount == dec("0") && fee.rent_amount == dec("0"),
        "another account paid fee and rent"
    );
}

#[test]
fn t40_6c_5_a_failed_transaction_has_no_token_row_and_a_foreign_token_none() {
    let rows = rows_of(TX_FAILED, FAILED_PAYER, &[token_spec()]);
    assert!(rows.iter().all(|(ticker, _)| ticker == "SOL"), "no token row");
    // The wallet's SOL-only transactions make no row of the token.
    let rows = rows_of(TX_SEND, WALLET, &[token_spec()]);
    assert!(rows.len() == 1, "only the SOL row");
}

// ---------------------------------------------------------------------------------------------------------------
// The scripted node and the writer
// ---------------------------------------------------------------------------------------------------------------

enum Answer {
    Result(Json),
    Error,
}

struct Stub {
    url: String,
    log: Arc<Mutex<Vec<(String, Json)>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Stub {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

impl Stub {
    fn params_of(&self, method: &str) -> Vec<Json> {
        self.log
            .lock()
            .expect("the log")
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn count(&self, method: &str) -> usize { self.params_of(method).len() }
}

fn read_body(stream: &mut TcpStream) -> Json {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body = text.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    serde_json::from_str(body).unwrap_or(Json::Null)
}

fn respond(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.flush().ok();
}

fn spawn_stub(handler: impl Fn(&str, &Json) -> Answer + Send + 'static) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    let stop = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (stop_t, log_t) = (stop.clone(), log.clone());
    std::thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    let request = read_body(&mut stream);
                    let method = request["method"].as_str().unwrap_or_default().to_owned();
                    log_t
                        .lock()
                        .expect("the log")
                        .push((method.clone(), request["params"].clone()));
                    let body = match handler(&method, &request["params"]) {
                        Answer::Result(result) => json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }),
                        Answer::Error => json!({ "jsonrpc": "2.0", "id": request["id"],
                            "error": { "code": -32000, "message": "scripted failure" } }),
                    };
                    respond(&mut stream, &body.to_string());
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                },
                Err(_) => break,
            }
        }
    });
    Stub {
        url: format!("http://127.0.0.1:{port}"),
        log,
        stop,
    }
}

/// What a test changes in the scripted node.
#[derive(Default)]
struct Script {
    /// Every listing fails while set.
    fail_listing: AtomicBool,
    /// The most entries a listing page holds, whatever `limit` asks; 0 is no cap (a node may serve fewer).
    page_cap: AtomicUsize,
    /// The transaction with this signature is answered with bytes that are no transaction (altered answer).
    unreadable: Mutex<Option<String>>,
    /// The block of this slot is answered without `blockHeight` (altered answer).
    no_height_slot: Mutex<Option<u64>>,
    /// The coin whose history state is recorded at every `getTransaction`.
    watched_coin: Mutex<Option<SolanaCoin>>,
    states: Mutex<Vec<Json>>,
}

fn fixture_node(script: Arc<Script>) -> Stub {
    let fx = fixtures();
    spawn_stub(move |method, params| match method {
        "getHealth" => Answer::Result(json!("ok")),
        "getBlockHeight" => Answer::Result(json!(512021000)),
        "getSignaturesForAddress" => {
            if script.fail_listing.load(Ordering::SeqCst) {
                return Answer::Error;
            }
            let address = params[0].as_str().unwrap_or_default();
            let list = if address == WALLET {
                &fx["wallet_signatures"]
            } else if fx["token_accounts"]
                .as_array()
                .is_some_and(|a| a.iter().any(|k| k == address))
            {
                &fx["token_account_signatures"]
            } else {
                return Answer::Result(json!([]));
            };
            let all = list.as_array().expect("a list").clone();
            let start = match params[1]["before"].as_str() {
                Some(before) => all
                    .iter()
                    .position(|e| e["signature"] == before)
                    .map_or(all.len(), |i| i + 1),
                None => 0,
            };
            let mut limit = params[1]["limit"].as_u64().unwrap_or(1000) as usize;
            let cap = script.page_cap.load(Ordering::SeqCst);
            if cap > 0 {
                limit = limit.min(cap);
            }
            Answer::Result(Json::Array(all.into_iter().skip(start).take(limit).collect()))
        },
        "getTransaction" => {
            let signature = params[0].as_str().unwrap_or_default();
            if let Some(coin) = script.watched_coin.lock().expect("coin").as_ref() {
                let state = serde_json::to_value(coin.history_sync_status()).expect("a state");
                script.states.lock().expect("states").push(state);
            }
            let mut transaction = fx["transactions"][signature].clone();
            if script.unreadable.lock().expect("unreadable").as_deref() == Some(signature) {
                transaction["transaction"] = json!(["AAAA", "base64"]);
            }
            Answer::Result(transaction)
        },
        "getBlock" => {
            let slot = params[0].as_u64().unwrap_or_default();
            let mut block = fx["blocks"][slot.to_string()].clone();
            if *script.no_height_slot.lock().expect("slot") == Some(slot) {
                block = json!({ "blockTime": block["blockTime"] });
            }
            Answer::Result(block)
        },
        "getTokenAccountsByOwner" => {
            let accounts: Vec<Json> = fx["token_accounts"]
                .as_array()
                .expect("accounts")
                .iter()
                .map(|key| json!({ "pubkey": key, "account": {} }))
                .collect();
            Answer::Result(json!({ "context": { "slot": 1 }, "value": accounts }))
        },
        _ => Answer::Error,
    })
}

static CONTEXTS: AtomicUsize = AtomicUsize::new(0);

fn history_ctx() -> MmArc {
    let root = std::env::temp_dir().join(format!(
        "kdf-solana-history-{}-{}",
        std::process::id(),
        CONTEXTS.fetch_add(1, Ordering::SeqCst)
    ));
    let conf = json!({ "dbdir": root.to_str().expect("a UTF-8 path"), "coins": [] });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).expect("the history directory");
    ctx
}

/// The coin of the fixture wallet over the node at `url`, with one served token.
fn history_coin(url: &str, history: bool) -> SolanaCoin {
    let key_pair = generate_key_pair_from_iguana_seed("solana history tests".to_owned());
    let client = SolanaRpcPool::without_commitment(vec![url.to_owned()]).expect("a pool");
    let coin = SolanaCoin(Arc::new(SolanaCoinImpl {
        ticker: "SOL".to_owned(),
        key_pair,
        client,
        decimals: 9,
        my_address: WALLET.to_owned(),
        spl_tokens_infos: Arc::new(Mutex::new(HashMap::new())),
        settings: SolanaCoinSettings::default(),
        history: SolanaHistoryStates::default(),
        swap: SolanaSwapConfig::default(),
    }));
    coin.add_spl_token_info(TOKEN_TICKER.to_owned(), SplTokenInfo {
        token_contract_address: Pubkey::from_str(MINT).expect("a key"),
        program_id: spl_token::ID,
        decimals: 6,
    });
    if history {
        coin.history.enable();
    }
    coin
}

fn test_config(page_limit: usize) -> HistoryConfig {
    HistoryConfig {
        poll_interval_s: 0.,
        request_spacing_s: 0.,
        page_limit,
    }
}

fn token_of(coin: &SolanaCoin) -> SplToken {
    SplToken::new(
        6,
        TOKEN_TICKER.to_owned(),
        Pubkey::from_str(MINT).expect("a key"),
        spl_token::ID,
        coin.clone(),
    )
}

fn stored(ctx: &MmArc, coin: &MmCoinEnum) -> Vec<TransactionDetails> {
    block_on(async { coin.load_history_from_file(ctx).compat().await }).expect("the list loads")
}

async fn stored_async(ctx: &MmArc, coin: &MmCoinEnum) -> Vec<TransactionDetails> {
    coin.load_history_from_file(ctx).compat().await.expect("the list loads")
}

fn state_name(state: &HistorySyncState) -> String {
    serde_json::to_value(state).expect("a state")["state"]
        .as_str()
        .expect("a name")
        .to_owned()
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.3: paging
// ---------------------------------------------------------------------------------------------------------------

/// Three pages of signatures, the cursor of each request, the end at an empty page, and a later pass that stops at the
/// first page without a new signature (R40.6C.4, R40.6C.5). The page limit of the test is 4; the production limit is
/// checked in `t40_6c_3_the_requests_carry_the_documented_parameters`.
#[test]
fn t40_6c_3_pages_cursors_and_the_stop_of_a_later_pass() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    let signatures: Vec<String> = fixtures()["wallet_signatures"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|e| e["signature"].as_str().expect("a signature").to_owned())
        .collect();
    assert!(signatures.len() == 9, "the fixture lists nine signatures");

    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(4)));
    let listings = node.params_of("getSignaturesForAddress");
    let wallet_cursors: Vec<Option<String>> = listings
        .iter()
        .filter(|p| p[0] == WALLET)
        .map(|p| p[1]["before"].as_str().map(str::to_owned))
        .collect();
    assert!(
        wallet_cursors
            == vec![
                None,
                Some(signatures[3].clone()),
                Some(signatures[7].clone()),
                Some(signatures[8].clone())
            ],
        "three pages and the empty one, each after the oldest signature of the previous one"
    );
    assert!(
        listings.len() == 6,
        "the token account is listed in a page and its empty page"
    );
    assert!(
        state_name(&coin.history_sync_status()) == "Finished",
        "a pass that read everything finishes"
    );

    // A later pass of the same process stops at the first page without a new signature: one request per account.
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(4)));
    assert!(
        node.count("getSignaturesForAddress") == 8,
        "one first page per watched account, no further page"
    );
    assert!(node.count("getTransaction") == 9, "no signature is read twice");
    assert!(
        stored(&ctx, &MmCoinEnum::from(coin.clone())).len() == 8,
        "the rows of the platform coin"
    );
}

#[test]
fn t40_6c_3_the_requests_carry_the_documented_parameters() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &HistoryConfig {
        poll_interval_s: 0.,
        request_spacing_s: 0.,
        page_limit: HistoryConfig::default().page_limit,
    }));
    let listing = &node.params_of("getSignaturesForAddress")[0];
    assert!(
        listing[1] == json!({ "limit": 1000, "commitment": "finalized" }),
        "limit 1000, finalized, no cursor on the first page"
    );
    let transaction = &node.params_of("getTransaction")[0];
    assert!(
        transaction[1]
            == json!({ "encoding": "base64", "commitment": "finalized", "maxSupportedTransactionVersion": 0 }),
        "base64, finalized, version 0"
    );
    let block = &node.params_of("getBlock")[0];
    assert!(
        block[1]
            == json!({ "transactionDetails": "none", "rewards": false, "commitment": "finalized",
                "maxSupportedTransactionVersion": 0 }),
        "no transactions, no rewards, finalized"
    );
    let owned = &node.params_of("getTokenAccountsByOwner")[0];
    assert!(owned[1] == json!({ "mint": MINT }), "the mint filter");
    assert!(
        HistoryConfig::default().request_spacing_s >= 0.25,
        "at most four requests per second"
    );
    assert!(
        HistoryConfig::default().poll_interval_s == 60.,
        "60 seconds between passes"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.6: one read, one row in each list, no duplicate after a restart
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6c_6_a_transaction_listed_twice_is_read_once_and_a_restart_adds_no_duplicate() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    // The three token transactions are listed by the wallet address and by its token account.
    assert!(node.count("getTransaction") == 9, "every signature is read once");
    assert!(node.count("getBlock") == 9, "every slot is read once");
    let platform = stored(&ctx, &MmCoinEnum::from(coin.clone()));
    let token = stored(&ctx, &MmCoinEnum::from(token_of(&coin)));
    assert!(platform.len() == 8, "the rows of the platform coin");
    assert!(token.len() == 3, "the rows of the token");
    for rows in [&platform, &token] {
        let heights: Vec<u64> = rows.iter().map(|row| row.block_height).collect();
        assert!(heights.windows(2).all(|pair| pair[0] >= pair[1]), "newest first");
        let ids: HashSet<Vec<u8>> = rows.iter().map(|row| row.internal_id.0.clone()).collect();
        assert!(ids.len() == rows.len(), "one row per transaction in a list");
    }
    // A token transaction is a row in each list it changes.
    assert!(
        platform.iter().any(|r| r.tx_hash == TX_SPL_SEND) && token.iter().any(|r| r.tx_hash == TX_SPL_SEND),
        "a row in both lists"
    );

    // A restart: a new process state over the same stored lists. Every one of the nine transactions made a row,
    // so none is read again.
    let before = node.count("getTransaction");
    let mut restarted = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut restarted, &test_config(1000)));
    assert!(
        node.count("getTransaction") == before,
        "no stored signature is read again after a restart"
    );
    assert!(
        stored(&ctx, &MmCoinEnum::from(coin.clone())).len() == 8
            && stored(&ctx, &MmCoinEnum::from(token_of(&coin))).len() == 3,
        "the lists are unchanged"
    );
}

/// A token activated later is served: its rows are made, and only what its own listings need is read. The platform
/// listing reads nothing that is stored already (one read of the token receive, which made no SOL row); the token
/// account's listing reads the two signatures that are not in the token's list yet.
#[test]
fn t40_6c_6_a_token_served_later_gets_its_rows_with_few_reads() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    coin.remove_spl_token_info(TOKEN_TICKER);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    assert!(
        stored(&ctx, &MmCoinEnum::from(token_of(&coin))).is_empty(),
        "no token is served yet"
    );
    assert!(
        stored(&ctx, &MmCoinEnum::from(coin.clone())).len() == 8 && node.count("getTransaction") == 9,
        "the platform's rows, every signature read once"
    );
    coin.add_spl_token_info(TOKEN_TICKER.to_owned(), SplTokenInfo {
        token_contract_address: Pubkey::from_str(MINT).expect("a key"),
        program_id: spl_token::ID,
        decimals: 6,
    });
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    assert!(
        stored(&ctx, &MmCoinEnum::from(token_of(&coin))).len() == 3,
        "the token's rows after the next pass"
    );
    assert!(
        node.count("getTransaction") == 9 + 3,
        "only the token receive and the two token sends are read again"
    );
    assert!(
        stored(&ctx, &MmCoinEnum::from(coin.clone())).len() == 8,
        "no duplicate row of the platform"
    );

    // A restart after that reads nothing.
    let mut restarted = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut restarted, &test_config(1000)));
    assert!(
        node.count("getTransaction") == 9 + 3,
        "nothing stored is read after a restart"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.7: rows not read
// ---------------------------------------------------------------------------------------------------------------

/// An undecodable transaction and a block without height are left out; the pass goes on. Both answers are altered
/// copies of the captured ones (see the file header).
#[test]
fn t40_6c_7_an_unreadable_transaction_and_a_block_without_height_are_left_out() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    *script.unreadable.lock().expect("unreadable") = Some(TX_FIRST_FUNDING.to_owned());
    let receive_slot = answer_of(TX_RECEIVE).slot;
    *script.no_height_slot.lock().expect("slot") = Some(receive_slot);
    let node = fixture_node(script);
    let coin = history_coin(&node.url, true);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    let platform = stored(&ctx, &MmCoinEnum::from(coin.clone()));
    assert!(
        platform.len() == 6,
        "the two transactions are left out, the other six are rows"
    );
    assert!(
        platform
            .iter()
            .all(|r| r.tx_hash != TX_FIRST_FUNDING && r.tx_hash != TX_RECEIVE),
        "neither is stored"
    );
    assert!(
        stored(&ctx, &MmCoinEnum::from(token_of(&coin))).len() == 3,
        "the token rows are all there"
    );
    assert!(
        state_name(&coin.history_sync_status()) == "Finished",
        "the pass went on and finished"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.8: states
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6c_8_states_in_progress_finished_and_error() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    let node = fixture_node(script.clone());
    let coin = history_coin(&node.url, true);
    *script.watched_coin.lock().expect("coin") = Some(coin.clone());
    assert!(
        state_name(&coin.history_sync_status()) == "NotStarted",
        "enabled and not yet started"
    );

    // The listing fails before the history has finished once: `Error` with a message.
    script.fail_listing.store(true, Ordering::SeqCst);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    let failed = serde_json::to_value(coin.history_sync_status()).expect("a state");
    assert!(failed["state"] == "Error", "an Error before the first finish");
    assert!(
        failed["additional_info"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "with a message"
    );

    // A pass that reads everything: `InProgress` with the number of listed signatures not yet read, then `Finished`.
    script.fail_listing.store(false, Ordering::SeqCst);
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    let seen = script.states.lock().expect("states").clone();
    assert!(seen.len() == 9, "nine signatures were read");
    let left: Vec<u64> = seen
        .iter()
        .map(|s| {
            assert!(s["state"] == "InProgress", "in progress while reading");
            s["additional_info"]["transactions_left"].as_u64().expect("a count")
        })
        .collect();
    assert!(
        left == vec![9, 8, 7, 6, 5, 4, 3, 2, 1],
        "the listed signatures not yet read"
    );
    assert!(
        state_name(&coin.history_sync_status()) == "Finished"
            && state_name(&token_of(&coin).history_sync_status()) == "Finished",
        "the platform and its token have finished"
    );

    // After the first finish a failed pass is logged only: the state stays `Finished`.
    script.fail_listing.store(true, Ordering::SeqCst);
    let mut later = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut later, &test_config(1000)));
    assert!(
        state_name(&coin.history_sync_status()) == "Finished",
        "no Error after the first finish"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.2 and the serving of both forms
// ---------------------------------------------------------------------------------------------------------------

fn register(ctx: &MmArc, coin: MmCoinEnum) {
    let ticker = coin.ticker().to_owned();
    let coins_ctx = CoinsContext::from_ctx(ctx).expect("a coins context");
    block_on(async { coins_ctx.coins.lock().await.insert(ticker, coin) });
}

/// Without history, both forms answer as before (R40.6C.2) and no history request reaches the node.
#[test]
fn t40_6c_2_without_history_both_forms_refuse_and_the_node_is_not_asked() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, false);
    register(&ctx, MmCoinEnum::from(coin.clone()));
    register(&ctx, MmCoinEnum::from(token_of(&coin)));
    for ticker in ["SOL", TOKEN_TICKER] {
        let err = block_on(crate::my_tx_history(ctx.clone(), json!({ "coin": ticker }))).unwrap_err();
        assert!(err.contains(ticker), "the legacy form names the coin");
        let request: MyTxHistoryRequestV2 = serde_json::from_value(json!({ "coin": ticker })).expect("a request");
        let Err(err) = block_on(my_tx_history_v2_rpc(ctx.clone(), request)) else {
            panic!("the versioned form must refuse");
        };
        assert!(
            matches!(err.get_inner(), MyTxHistoryErrorV2::NotSupportedFor(name) if name == ticker),
            "NotSupportedFor"
        );
    }
    assert!(
        state_name(&coin.history_sync_status()) == "NotEnabled",
        "the state is NotEnabled"
    );
    assert!(
        state_name(&token_of(&coin).history_sync_status()) == "NotEnabled",
        "and the token's too"
    );
    assert!(
        node.count("getSignaturesForAddress") == 0 && node.count("getTransaction") == 0,
        "no history request is sent"
    );
}

/// With history, both forms serve the written lists with the rules of §38.5A (R40.6C.3), for the platform coin and
/// for the token.
#[test]
fn t40_6c_3_both_forms_serve_the_written_lists() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    register(&ctx, MmCoinEnum::from(coin.clone()));
    register(&ctx, MmCoinEnum::from(token_of(&coin)));

    for (ticker, total) in [("SOL", 8usize), (TOKEN_TICKER, 3)] {
        let request: MyTxHistoryRequestV2 =
            serde_json::from_value(json!({ "coin": ticker, "limit": 2 })).expect("a request");
        let answer = block_on(my_tx_history_v2_rpc(ctx.clone(), request))
            .map_err(|e| e.into_inner())
            .expect("the versioned form serves");
        let answer = serde_json::to_value(answer).expect("serializable");
        assert!(answer["total"] == total, "the total of the list");
        assert!(
            answer["transactions"].as_array().is_some_and(|t| t.len() == 2),
            "a page of two"
        );
        assert!(answer["sync_status"]["state"] == "Finished", "the state of the writer");
        let heights: Vec<u64> = answer["transactions"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|t| t["block_height"].as_u64().expect("a height"))
            .collect();
        assert!(heights[0] >= heights[1], "newest first");

        let legacy = block_on(crate::my_tx_history(
            ctx.clone(),
            json!({ "coin": ticker, "max": true }),
        ))
        .expect("the legacy form serves");
        let legacy: Json = serde_json::from_slice(legacy.body()).expect("a JSON body");
        assert!(legacy["result"]["total"] == total, "the legacy total");
        assert!(
            legacy["result"]["transactions"]
                .as_array()
                .is_some_and(|t| t.len() == total),
            "every row"
        );
        assert!(
            legacy["result"]["sync_status"]["state"] == "Finished",
            "the legacy state"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.3: a node that serves pages shorter than the limit
// ---------------------------------------------------------------------------------------------------------------

/// A node serves at most two entries a page although the limit is 1000: the walk goes on to the empty page and the
/// history holds every row (R40.6C.4).
#[test]
fn t40_6c_3_a_node_with_pages_shorter_than_the_limit_is_walked_to_its_empty_page() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    script.page_cap.store(2, Ordering::SeqCst);
    let node = fixture_node(script);
    let coin = history_coin(&node.url, true);
    let mut state = WalkState::default();
    block_on(run_pass_and_report(&coin, &ctx, &mut state, &test_config(1000)));
    let wallet_pages = node
        .params_of("getSignaturesForAddress")
        .iter()
        .filter(|p| p[0] == WALLET)
        .count();
    assert!(wallet_pages == 6, "five pages of at most two entries and the empty one");
    assert!(node.count("getTransaction") == 9, "every signature is read");
    assert!(
        stored(&ctx, &MmCoinEnum::from(coin.clone())).len() == 8,
        "the rows of the platform coin"
    );
    assert!(
        stored(&ctx, &MmCoinEnum::from(token_of(&coin))).len() == 3,
        "the rows of the token"
    );
    assert!(
        state_name(&coin.history_sync_status()) == "Finished",
        "the history finished"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6C.9: disable_coin
// ---------------------------------------------------------------------------------------------------------------

fn register_for_disable(ctx: &MmArc, coin: MmCoinEnum) {
    let params = crate::RegisterCoinParams {
        ticker: coin.ticker().to_owned(),
        tx_history: false,
    };
    block_on(crate::lp_register_coin(ctx, coin, params)).expect("the coin registers");
}

/// Waits until the history of `coin` has finished once (the node is asked at most 10 seconds).
async fn wait_finished(coin: &SolanaCoin) {
    for _ in 0..1000 {
        if state_name(&coin.history_sync_status()) == "Finished" {
            return;
        }
        Timer::sleep(0.01).await;
    }
    panic!("the history did not finish");
}

/// After `disable_coin` of the platform the writer sends no further history request, writes no list and ends; after
/// a token's disable its list is no longer written and its accounts are no longer asked, while the platform's history
/// goes on (R40.6C.1).
#[test]
fn t40_6c_9_disable_coin_stops_the_writer_of_the_platform_and_of_a_token() {
    let ctx = history_ctx();
    let node = fixture_node(Arc::default());
    let coin = history_coin(&node.url, true);
    register_for_disable(&ctx, MmCoinEnum::from(coin.clone()));
    register_for_disable(&ctx, MmCoinEnum::from(token_of(&coin)));
    let token_account = fixtures()["token_accounts"][0]
        .as_str()
        .expect("a token account")
        .to_owned();
    let config = HistoryConfig {
        poll_interval_s: 0.2,
        request_spacing_s: 0.,
        page_limit: 1000,
    };
    let requests = |node: &Stub| {
        (
            node.count("getSignaturesForAddress"),
            node.count("getTransaction"),
            node.count("getTokenAccountsByOwner"),
        )
    };
    let token_listings = |node: &Stub| {
        node.params_of("getSignaturesForAddress")
            .iter()
            .filter(|p| p[0] == token_account.as_str())
            .count()
    };
    let wallet_listings = |node: &Stub| {
        node.params_of("getSignaturesForAddress")
            .iter()
            .filter(|p| p[0] == WALLET)
            .count()
    };

    block_on(async {
        let writer = history_loop(coin.clone(), ctx.clone(), config);
        let driver = async {
            wait_finished(&coin).await;
            // The token goes; the writer sleeps between passes.
            crate::disable_coin(&ctx, TOKEN_TICKER)
                .await
                .expect("the token is disabled");
            let token_rows = stored_async(&ctx, &MmCoinEnum::from(token_of(&coin))).await.len();
            let (accounts_asked, token_listed, wallet_listed) = (
                node.count("getTokenAccountsByOwner"),
                token_listings(&node),
                wallet_listings(&node),
            );
            // The platform's history goes on: its listing is asked again, the token's accounts are not.
            for _ in 0..1000 {
                if wallet_listings(&node) > wallet_listed {
                    break;
                }
                Timer::sleep(0.01).await;
            }
            assert!(wallet_listings(&node) > wallet_listed, "the platform's history goes on");
            assert!(
                node.count("getTokenAccountsByOwner") == accounts_asked && token_listings(&node) == token_listed,
                "the token's accounts are no longer asked"
            );
            assert!(
                stored_async(&ctx, &MmCoinEnum::from(token_of(&coin))).await.len() == token_rows,
                "the token's list is no longer written"
            );
            // The platform goes: no further request is sent.
            wait_finished(&coin).await;
            crate::disable_coin(&ctx, "SOL")
                .await
                .expect("the platform is disabled");
            let (listed, read, asked) = requests(&node);
            let platform_rows = stored_async(&ctx, &MmCoinEnum::from(coin.clone())).await.len();
            (listed, read, asked, platform_rows)
        };
        let (writer_end, (listed, read, asked, platform_rows)) = futures::join!(
            async { futures::future::select(Box::pin(writer), Box::pin(Timer::sleep(20.))).await },
            driver
        );
        assert!(
            matches!(writer_end, futures::future::Either::Left(_)),
            "the writer ends once the platform is disabled"
        );
        assert!(requests(&node) == (listed, read, asked), "no request after the disable");
        assert!(
            stored_async(&ctx, &MmCoinEnum::from(coin.clone())).await.len() == platform_rows,
            "no list is written after the disable"
        );
    });
}
