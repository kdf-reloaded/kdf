//! Offline tests of the Solana withdraw and of the token balance (CRD ch.40 R40.6B.4, R40.6B.9 to R40.6B.13,
//! D40.6B.2 to D40.6B.6, T40.6B.7, T40.6B.9 to T40.6B.15) against a scripted JSON-RPC node on a loopback socket.
//! No network is used and the node stops when it is dropped. The built transactions are decoded and their
//! instructions, programs, accounts and amounts asserted. No assertion message carries a key or an address: values
//! derived from a key are compared with `assert!(a == b, "<fixed text>")`.

use super::*;
use crate::solana::solana_common::units_to_amount;
use crate::solana::spl::{associated_token_account, SplToken, SPL_TOKEN_ACCOUNT_LEN, SPL_TOKEN_PROGRAM_ID,
                         TOKEN_2022_PROGRAM_ID};
use crate::{TxFeeDetails, WithdrawFee};
use common::{block_on, Future01CompatExt, HttpStatusCode, StatusCode};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const PASSPHRASE: &str = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
const FEE: u64 = 5_000;
const RENT: u64 = 2_039_280;
const SYSTEM_PROGRAM: Pubkey = Pubkey::new_from_array([0; 32]);

fn mint() -> Pubkey { Pubkey::new_from_array([9; 32]) }

fn other_wallet() -> Pubkey { Pubkey::new_from_array([5; 32]) }

fn ata_program() -> Pubkey { spl_associated_token_account_client::program::ID }

fn wallet_key() -> Keypair {
    crate::solana::solana_common_tests::generate_key_pair_from_iguana_seed(PASSPHRASE.to_string())
}

fn wallet() -> Pubkey { wallet_key().pubkey() }

/// What the scripted node knows: SOL balances, existing accounts with their owner program, token-account amounts
/// as `(raw, decimals)`, the quoted fee and the quoted rent.
struct Script {
    sol: HashMap<Pubkey, u64>,
    accounts: HashMap<Pubkey, Pubkey>,
    token_amounts: HashMap<Pubkey, (u64, u8)>,
    fee: u64,
    rent: u64,
}

impl Script {
    fn new() -> Script {
        Script {
            sol: HashMap::new(),
            accounts: HashMap::new(),
            token_amounts: HashMap::new(),
            fee: FEE,
            rent: RENT,
        }
    }

    fn with_sol(mut self, owner: Pubkey, lamports: u64) -> Script {
        self.sol.insert(owner, lamports);
        self
    }

    /// The associated token account of `owner` for the test mint under `program` exists and holds `raw`.
    fn with_token_account(mut self, owner: &Pubkey, program: &Pubkey, raw: u64, decimals: u8) -> Script {
        let account = associated_token_account(owner, &mint(), program);
        self.accounts.insert(account, *program);
        self.token_amounts.insert(account, (raw, decimals));
        self
    }

    fn with_account(mut self, address: Pubkey, owner: Pubkey) -> Script {
        self.accounts.insert(address, owner);
        self
    }
}

enum Reply {
    Result(Json),
    RpcError { code: i64, message: &'static str },
}

fn pubkey_param(params: &Json, index: usize) -> Option<Pubkey> {
    params.get(index)?.as_str().and_then(|text| Pubkey::from_str(text).ok())
}

fn answer(script: &Script, method: &str, params: &Json) -> Reply {
    let context = json!({ "slot": 1 });
    match method {
        "getBalance" => {
            let lamports = pubkey_param(params, 0).and_then(|key| script.sol.get(&key).copied());
            Reply::Result(json!({ "context": context, "value": lamports.unwrap_or(0) }))
        },
        "getLatestBlockhash" => Reply::Result(json!({
            "context": context,
            "value": { "blockhash": solana_hash::Hash::new_from_array([7; 32]).to_string(), "lastValidBlockHeight": 100 }
        })),
        "getFeeForMessage" => Reply::Result(json!({ "context": context, "value": script.fee })),
        "getMinimumBalanceForRentExemption" => Reply::Result(json!(script.rent)),
        "getAccountInfo" => {
            let owner = pubkey_param(params, 0).and_then(|key| script.accounts.get(&key).copied());
            let value = owner.map_or(Json::Null, |owner| {
                json!({ "owner": owner.to_string(), "lamports": 1, "data": ["", "base64"], "executable": false,
                        "rentEpoch": 0 })
            });
            Reply::Result(json!({ "context": context, "value": value }))
        },
        "getTokenAccountBalance" => match pubkey_param(params, 0).and_then(|key| script.token_amounts.get(&key)) {
            Some((raw, decimals)) => Reply::Result(json!({ "context": context, "value": {
                "amount": raw.to_string(),
                "decimals": decimals,
                "uiAmount": *raw as f64 / 10f64.powi(i32::from(*decimals)),
                "uiAmountString": units_to_amount(*raw, *decimals).to_string(),
            } })),
            None => Reply::RpcError {
                code: -32602,
                message: "Invalid param: could not find account",
            },
        },
        "sendTransaction" => {
            let signature = params
                .get(0)
                .and_then(Json::as_str)
                .and_then(|encoded| base64::decode(encoded).ok())
                .and_then(|bytes| deserialize::<Transaction>(&bytes).ok())
                .and_then(|tx| tx.signatures.first().map(ToString::to_string));
            match signature {
                Some(signature) => Reply::Result(json!(signature)),
                None => Reply::RpcError {
                    code: -32602,
                    message: "invalid transaction",
                },
            }
        },
        _ => Reply::RpcError {
            code: -32601,
            message: "Method not found",
        },
    }
}

/// A scripted node. Its thread stops when the node is dropped; it records every request it served.
struct ScriptedNode {
    url: String,
    requests: Arc<Mutex<Vec<(String, Json)>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for ScriptedNode {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

impl ScriptedNode {
    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn params_of(&self, method: &str) -> Vec<Json> {
        self.requests
            .lock()
            .expect("the request log")
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    fn was_called(&self, method: &str) -> bool { self.methods().iter().any(|name| name == method) }
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

fn spawn(script: Script) -> ScriptedNode {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the scripted node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(script);
    let (stop_thread, requests_thread) = (stop.clone(), requests.clone());
    std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let (script, requests) = (script.clone(), requests_thread.clone());
                    std::thread::spawn(move || {
                        let request = read_body(&mut stream);
                        let method = request["method"].as_str().unwrap_or_default().to_owned();
                        let reply = answer(&script, &method, &request["params"]);
                        if let Ok(mut log) = requests.lock() {
                            log.push((method, request["params"].clone()));
                        }
                        let body = match reply {
                            Reply::Result(result) => {
                                json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
                            },
                            Reply::RpcError { code, message } => json!({ "jsonrpc": "2.0", "id": request["id"],
                                "error": { "code": code, "message": message } }),
                        }
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: \
                             close\r\n\r\n{body}",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).ok();
                        stream.flush().ok();
                    });
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                },
                Err(_) => break,
            }
        }
    });
    ScriptedNode {
        url: format!("http://127.0.0.1:{port}"),
        requests,
        stop,
    }
}

fn sol_on(url: &str) -> SolanaCoin {
    let client = SolanaRpcPool::with_commitment(vec![url.to_string()], CommitmentConfig {
        commitment: CommitmentLevel::Finalized,
    })
    .expect("non-empty endpoint list");
    let key_pair = wallet_key();
    SolanaCoin(Arc::new(SolanaCoinImpl {
        decimals: 9,
        my_address: key_pair.pubkey().to_string(),
        key_pair,
        ticker: "SOL".to_string(),
        client,
        spl_tokens_infos: Arc::new(Mutex::new(HashMap::new())),
        settings: SolanaCoinSettings::default(),
    }))
}

fn token_on(url: &str, program: Pubkey, decimals: u8) -> SplToken {
    SplToken::new(decimals, "USDC".to_string(), mint(), program, sol_on(url))
}

fn request(coin: &str, to: &str, amount: &str, max: bool) -> WithdrawRequest {
    WithdrawRequest {
        memo: None,
        coin: coin.to_string(),
        from: None,
        to: to.to_string(),
        amount: BigDecimal::from_str(amount).expect("a decimal"),
        max,
        fee: None,
    }
}

fn withdraw<C: MmCoin>(coin: &C, req: WithdrawRequest) -> WithdrawResult { block_on(coin.withdraw(req).compat()) }

fn error_of(result: WithdrawResult) -> WithdrawError {
    match result {
        Ok(_) => panic!("expected a withdraw error"),
        Err(e) => e.into_inner(),
    }
}

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).expect("a decimal") }

struct DecodedInstruction {
    program: Pubkey,
    accounts: Vec<Pubkey>,
    data: Vec<u8>,
}

fn decode(details: &TransactionDetails) -> (Transaction, Vec<DecodedInstruction>) {
    let tx: Transaction = deserialize(&details.tx_hex.0).expect("a serialized transaction");
    let keys = &tx.message.account_keys;
    let instructions = tx
        .message
        .instructions
        .iter()
        .map(|ix| DecodedInstruction {
            program: keys[usize::from(ix.program_id_index)],
            accounts: ix.accounts.iter().map(|index| keys[usize::from(*index)]).collect(),
            data: ix.data.clone(),
        })
        .collect();
    (tx, instructions)
}

fn transfer_checked_data(amount: u64, decimals: u8) -> Vec<u8> {
    let mut data = vec![12];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    data
}

fn system_transfer_data(lamports: u64) -> Vec<u8> {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    data
}

fn solana_fee_details(details: &TransactionDetails) -> crate::SolanaFeeDetails {
    match &details.fee_details {
        Some(TxFeeDetails::Solana(fee)) => fee.clone(),
        _ => panic!("expected Solana fee details"),
    }
}

/// The wire form of an error and its HTTP status.
fn wire(error: &WithdrawError) -> (Json, StatusCode) {
    (
        serde_json::to_value(error).expect("a serializable error"),
        error.status_code(),
    )
}

fn keys_of(value: &Json) -> std::collections::BTreeSet<String> {
    value.as_object().expect("an object").keys().cloned().collect()
}

fn names(list: &[&str]) -> std::collections::BTreeSet<String> { list.iter().map(|n| (*n).to_string()).collect() }

// ---------------------------------------------------------------------------------------------------------------
// SPL token withdraw
// ---------------------------------------------------------------------------------------------------------------

/// T40.6B.9: no destination account, enough SOL: creation then transfer, rent positive, total = fee + rent.
#[test]
fn t40_6b_9_missing_destination_creates_the_account_then_transfers() {
    let node = spawn(Script::new().with_sol(wallet(), 10_000_000).with_token_account(
        &wallet(),
        &SPL_TOKEN_PROGRAM_ID,
        12_500_000,
        6,
    ));
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let recipient = other_wallet();
    let details = withdraw(&token, request("USDC", &recipient.to_string(), "1.5", false)).expect("a withdraw");

    let (tx, instructions) = decode(&details);
    let source = associated_token_account(&wallet(), &mint(), &SPL_TOKEN_PROGRAM_ID);
    let destination = associated_token_account(&recipient, &mint(), &SPL_TOKEN_PROGRAM_ID);
    assert!(instructions.len() == 2, "creation and transfer");
    let create = &instructions[0];
    assert!(
        create.program == ata_program(),
        "the first instruction is the associated-account program's"
    );
    assert!(create.data == vec![1], "the creation is the idempotent one");
    assert!(
        create.accounts
            == vec![
                wallet(),
                destination,
                recipient,
                mint(),
                SYSTEM_PROGRAM,
                SPL_TOKEN_PROGRAM_ID
            ],
        "creation accounts: payer, new account, owner, mint, system, token program"
    );
    let transfer = &instructions[1];
    assert!(
        transfer.program == SPL_TOKEN_PROGRAM_ID,
        "the transfer is the token program's"
    );
    assert!(
        transfer.data == transfer_checked_data(1_500_000, 6),
        "a checked transfer of 1.5 units with 6 decimals"
    );
    assert!(
        transfer.accounts == vec![source, mint(), destination, wallet()],
        "transfer accounts: source, mint, destination, authority"
    );
    assert!(
        tx.message.header.num_required_signatures == 1,
        "one signature, the wallet's"
    );
    assert!(tx.message.account_keys[0] == wallet(), "the wallet pays");
    assert!(
        tx.signatures.len() == 1 && tx.signatures[0].verify(wallet().as_ref(), &tx.message_data()),
        "signed by the wallet"
    );

    let fee = solana_fee_details(&details);
    assert_eq!(fee.fee_amount, units_to_amount(FEE, 9));
    assert_eq!(fee.rent_amount, units_to_amount(RENT, 9));
    assert!(fee.rent_amount > 0.into(), "the rent is positive");
    assert_eq!(fee.total_amount, &fee.fee_amount + &fee.rent_amount);
    assert_eq!(
        node.params_of("getMinimumBalanceForRentExemption")[0][0],
        json!(SPL_TOKEN_ACCOUNT_LEN)
    );

    assert_eq!(details.coin, "USDC");
    assert_eq!(details.total_amount, dec("1.5"));
    assert_eq!(details.spent_by_me, dec("1.5"));
    assert_eq!(details.received_by_me, 0.into());
    assert_eq!(details.my_balance_change, dec("-1.5"));
    assert!(details.from == vec![wallet().to_string()], "from is the wallet address");
    assert!(
        details.to == vec![recipient.to_string()],
        "to is the recipient wallet address"
    );
    assert!(
        matches!(&details.transaction_type, TransactionType::TokenTransfer(id)
            if id.0 == kdf_crypto::sha256(&mint().to_bytes()).to_vec()),
        "the token transfer type carries the SHA-256 of the mint key"
    );
    assert!(
        details.internal_id.0 == details.tx_hash.as_bytes(),
        "internal_id is the hex of the characters of tx_hash"
    );
    assert!(
        details.tx_hash == tx.signatures[0].to_string(),
        "tx_hash is the first signature"
    );
    assert!(
        !node.was_called("getTokenAccountsByOwner"),
        "the source is derived, not listed"
    );
}

/// T40.6B.9: SOL below fee plus rent answers `NotSufficientPlatformBalanceForFee` with the platform ticker, HTTP
/// 400 and the `{coin, available, required}` wire form.
#[test]
fn t40_6b_9_sol_below_fee_plus_rent_is_not_sufficient_platform_balance() {
    let node = spawn(Script::new().with_sol(wallet(), 1_000_000).with_token_account(
        &wallet(),
        &SPL_TOKEN_PROGRAM_ID,
        12_500_000,
        6,
    ));
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let error = error_of(withdraw(
        &token,
        request("USDC", &other_wallet().to_string(), "1", false),
    ));
    assert!(
        matches!(&error, WithdrawError::NotSufficientPlatformBalanceForFee { coin, available, required }
            if coin == "SOL" && *available == units_to_amount(1_000_000, 9)
                && *required == units_to_amount(FEE + RENT, 9)),
        "{error:?}"
    );
    let (body, status) = wire(&error);
    assert!(
        body["error_type"] == "NotSufficientPlatformBalanceForFee",
        "the error token"
    );
    assert!(
        keys_of(&body["error_data"]) == names(&["coin", "available", "required"]),
        "the data members"
    );
    assert!(body["error_data"]["coin"] == "SOL", "the platform ticker");
    assert!(
        body["error_data"]["available"].is_string() && body["error_data"]["required"].is_string(),
        "decimal strings"
    );
    assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");

    // With the destination account present the rent is zero and the fee alone is checked.
    let node = spawn(
        Script::new()
            .with_sol(wallet(), 1_000)
            .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6)
            .with_token_account(&other_wallet(), &SPL_TOKEN_PROGRAM_ID, 0, 6),
    );
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let error = error_of(withdraw(
        &token,
        request("USDC", &other_wallet().to_string(), "1", false),
    ));
    assert!(
        matches!(&error, WithdrawError::NotSufficientPlatformBalanceForFee { required, .. }
            if *required == units_to_amount(FEE, 9)),
        "{error:?}"
    );
}

/// T40.6B.10: the destination account exists: the transfer only, rent zero.
#[test]
fn t40_6b_10_existing_destination_gets_the_transfer_only() {
    let node = spawn(
        Script::new()
            .with_sol(wallet(), 10_000_000)
            .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6)
            .with_token_account(&other_wallet(), &SPL_TOKEN_PROGRAM_ID, 3, 6),
    );
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let details = withdraw(&token, request("USDC", &other_wallet().to_string(), "2", false)).expect("a withdraw");
    let (_, instructions) = decode(&details);
    assert!(instructions.len() == 1, "the transfer only");
    assert!(instructions[0].program == SPL_TOKEN_PROGRAM_ID, "the token program's");
    assert!(instructions[0].data == transfer_checked_data(2_000_000, 6), "2 units");
    let fee = solana_fee_details(&details);
    assert_eq!(fee.rent_amount, 0.into());
    assert_eq!(fee.total_amount, fee.fee_amount);
    assert!(
        !node.was_called("getMinimumBalanceForRentExemption"),
        "no rent is quoted"
    );
    let wire = serde_json::to_value(&details.fee_details).expect("serializable");
    assert!(wire["rent_amount"] == "0", "the rent is the string 0");
}

/// T40.6B.11: `max` moves the token account's whole raw amount, whatever its fractional part, and the answer
/// reports the same amount.
#[test]
fn t40_6b_11_max_moves_the_whole_raw_amount() {
    for (raw, shown) in [(12_500_000u64, "12.5"), (500_000, "0.5"), (1, "0.000001")] {
        let node = spawn(
            Script::new()
                .with_sol(wallet(), 10_000_000)
                .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, raw, 6)
                .with_token_account(&other_wallet(), &SPL_TOKEN_PROGRAM_ID, 0, 6),
        );
        let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
        let details = withdraw(&token, request("USDC", &other_wallet().to_string(), "0", true)).expect("a withdraw");
        let (_, instructions) = decode(&details);
        assert!(
            instructions[0].data == transfer_checked_data(raw, 6),
            "the transfer carries the whole raw amount"
        );
        assert_eq!(details.total_amount, dec(shown));
        assert_eq!(details.spent_by_me, dec(shown));
    }
}

/// `max` of a token the wallet has no account for or holds nothing of is `AmountTooLow` with the token's
/// `min_tx_amount` as threshold; an amount of such a token is `NotSufficientBalance` with the token ticker.
#[test]
fn spl_zero_balance_answers() {
    let node = spawn(Script::new().with_sol(wallet(), 10_000_000));
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let error = error_of(withdraw(
        &token,
        request("USDC", &other_wallet().to_string(), "0", true),
    ));
    assert!(
        matches!(&error, WithdrawError::AmountTooLow { amount, threshold }
            if *amount == 0.into() && *threshold == dec("0.000001")),
        "{error:?}"
    );
    let error = error_of(withdraw(
        &token,
        request("USDC", &other_wallet().to_string(), "1", false),
    ));
    assert!(
        matches!(&error, WithdrawError::NotSufficientBalance { coin, available, required }
            if coin == "USDC" && *available == 0.into() && *required == 1.into()),
        "{error:?}"
    );
    assert!(
        !node.was_called("getTokenAccountBalance"),
        "no account, no balance call"
    );
}

/// R40.6B.10: digits beyond the token's decimals are dropped; a zero result is `AmountTooLow` with the token's
/// smallest unit; a balance below the amount is `NotSufficientBalance` with the token ticker.
#[test]
fn spl_amount_scaling_and_limits() {
    let node = spawn(
        Script::new()
            .with_sol(wallet(), 10_000_000)
            .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6)
            .with_token_account(&other_wallet(), &SPL_TOKEN_PROGRAM_ID, 0, 6),
    );
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let to = other_wallet().to_string();
    let details = withdraw(&token, request("USDC", &to, "1.2345678", false)).expect("a withdraw");
    assert!(
        decode(&details).1[0].data == transfer_checked_data(1_234_567, 6),
        "surplus digits dropped"
    );
    assert_eq!(details.total_amount, dec("1.234567"));

    let error = error_of(withdraw(&token, request("USDC", &to, "0.0000001", false)));
    assert!(
        matches!(&error, WithdrawError::AmountTooLow { amount, threshold }
            if *amount == 0.into() && *threshold == dec("0.000001")),
        "{error:?}"
    );
    let (body, status) = wire(&error);
    assert!(
        keys_of(&body["error_data"]) == names(&["amount", "threshold"]),
        "the data members"
    );
    assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");

    let error = error_of(withdraw(&token, request("USDC", &to, "12.500001", false)));
    assert!(
        matches!(&error, WithdrawError::NotSufficientBalance { coin, available, required }
            if coin == "USDC" && *available == dec("12.5") && *required == dec("12.500001")),
        "{error:?}"
    );
}

/// T40.6B.14: a `to` that is an existing account owned by a token program (classic or Token-2022) is refused with
/// `InvalidAddress` and no transaction is built; an account of another owner is a wallet like any other.
#[test]
fn t40_6b_14_a_token_account_is_not_a_recipient() {
    for program in [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let node = spawn(
            Script::new()
                .with_sol(wallet(), 10_000_000)
                .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6)
                .with_account(other_wallet(), program),
        );
        let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
        let error = error_of(withdraw(
            &token,
            request("USDC", &other_wallet().to_string(), "1", false),
        ));
        assert!(matches!(error, WithdrawError::InvalidAddress(_)), "{error:?}");
        let (_, status) = wire(&error);
        assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");
        assert!(
            !node.was_called("getLatestBlockhash") && !node.was_called("getFeeForMessage"),
            "no transaction was built"
        );
    }
    let node = spawn(
        Script::new()
            .with_sol(wallet(), 10_000_000)
            .with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6)
            .with_account(other_wallet(), SYSTEM_PROGRAM),
    );
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    assert!(
        withdraw(&token, request("USDC", &other_wallet().to_string(), "1", false)).is_ok(),
        "a system-owned account is a wallet"
    );
}

/// A `to` that is not 32 bytes of base58 is `InvalidAddress` on the token as on SOL.
#[test]
fn spl_invalid_recipient_is_refused() {
    let node = spawn(Script::new());
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let (thirty_one, thirty_three) = ([7u8; 31].to_base58(), [7u8; 33].to_base58());
    for text in ["", "not an address", thirty_one.as_str(), thirty_three.as_str()] {
        let error = error_of(withdraw(&token, request("USDC", text, "1", false)));
        assert!(matches!(error, WithdrawError::InvalidAddress(_)), "{error:?}");
    }
    assert!(node.methods().is_empty(), "the node was not asked");
}

/// D40.6B.4: until Token-2022 is supported, no transaction is built for a token of another program.
#[test]
fn token_2022_withdraw_is_refused_before_any_transaction() {
    let node = spawn(Script::new().with_sol(wallet(), 10_000_000).with_token_account(
        &wallet(),
        &TOKEN_2022_PROGRAM_ID,
        12_500_000,
        6,
    ));
    let token = token_on(&node.url, TOKEN_2022_PROGRAM_ID, 6);
    let error = error_of(withdraw(
        &token,
        request("USDC", &other_wallet().to_string(), "1", false),
    ));
    assert!(matches!(error, WithdrawError::UnsupportedError(_)), "{error:?}");
    assert!(!node.was_called("getLatestBlockhash"), "no transaction was built");
}

// ---------------------------------------------------------------------------------------------------------------
// Token balance
// ---------------------------------------------------------------------------------------------------------------

/// D40.6B.2 (a), T40.6B.7: the balance is read from the associated account derived with the token's `program_id`;
/// the node answers only that address.
#[test]
fn balance_is_read_from_the_program_derived_account() {
    for program in [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let node = spawn(Script::new().with_token_account(&wallet(), &program, 12_500_000, 6));
        let token = token_on(&node.url, program, 6);
        let balance = block_on(token.my_balance().compat()).expect("a balance");
        assert_eq!(balance.spendable, dec("12.5"));
        let account = associated_token_account(&wallet(), &mint(), &program).to_string();
        let asked = node.params_of("getTokenAccountBalance");
        assert!(
            asked.len() == 1 && asked[0][0] == account.as_str(),
            "the derived account was read"
        );
        assert!(!node.was_called("getTokenAccountsByOwner"), "no account listing");
    }
    // A node that knows only the classic account does not give a Token-2022 token that balance.
    let node = spawn(Script::new().with_token_account(&wallet(), &SPL_TOKEN_PROGRAM_ID, 12_500_000, 6));
    let token = token_on(&node.url, TOKEN_2022_PROGRAM_ID, 6);
    let balance = block_on(token.my_balance().compat()).expect("a balance");
    assert_eq!(balance.spendable, 0.into());
}

/// D40.6B.2 (a): a wallet with no account for the token has balance zero, not an error.
#[test]
fn a_wallet_without_a_token_account_has_balance_zero() {
    let node = spawn(Script::new());
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let balance = block_on(token.my_balance().compat()).expect("a balance");
    assert_eq!(balance.spendable, 0.into());
    assert_eq!(balance.unspendable, 0.into());
    let balance = block_on(token.platform_coin.my_balance_spl(&token.get_info())).expect("a balance");
    assert_eq!(balance.spendable, 0.into());
}

/// A node failure stays an error: a balance is not zero because the node cannot be reached.
#[test]
fn a_node_failure_is_still_an_error() {
    let url = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
        format!("http://127.0.0.1:{}", listener.local_addr().expect("address").port())
    };
    let token = token_on(&url, SPL_TOKEN_PROGRAM_ID, 6);
    assert!(block_on(token.my_balance().compat()).is_err(), "the balance read fails");
}

// ---------------------------------------------------------------------------------------------------------------
// The mint-owner check of activation
// ---------------------------------------------------------------------------------------------------------------

/// R40.2.2: a mint owned by the token's program passes, a mint account that does not exist is accepted, a mint
/// owned by another program is refused, and a node failure is its own error.
#[test]
fn mint_owner_check() {
    let platform_node = spawn(Script::new().with_account(mint(), SPL_TOKEN_PROGRAM_ID));
    let sol = sol_on(&platform_node.url);
    assert!(
        block_on(sol.check_mint_owner(&mint(), &SPL_TOKEN_PROGRAM_ID)).is_ok(),
        "the right owner"
    );
    let error = block_on(sol.check_mint_owner(&mint(), &TOKEN_2022_PROGRAM_ID)).expect_err("the wrong owner");
    assert!(matches!(error.get_inner(), MintOwnerError::WrongOwner), "{error:?}");

    let node = spawn(Script::new());
    let sol = sol_on(&node.url);
    assert!(
        block_on(sol.check_mint_owner(&mint(), &SPL_TOKEN_PROGRAM_ID)).is_ok(),
        "a mint account that does not exist is accepted"
    );

    let url = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
        format!("http://127.0.0.1:{}", listener.local_addr().expect("address").port())
    };
    let sol = sol_on(&url);
    let error = block_on(sol.check_mint_owner(&mint(), &SPL_TOKEN_PROGRAM_ID)).expect_err("a closed node");
    assert!(matches!(error.get_inner(), MintOwnerError::Node(_)), "{error:?}");
}

// ---------------------------------------------------------------------------------------------------------------
// SOL withdraw
// ---------------------------------------------------------------------------------------------------------------

fn sol_node(lamports: u64) -> (ScriptedNode, SolanaCoin) {
    let node = spawn(Script::new().with_sol(wallet(), lamports));
    let coin = sol_on(&node.url);
    (node, coin)
}

/// R40.6B.9: the transaction is one system transfer, the wallet pays and signs; the answer has the members of the
/// reference.
#[test]
fn sol_withdraw_answer() {
    let (_node, coin) = sol_node(1_000_000_000);
    let to = other_wallet();
    let details = withdraw(&coin, request("SOL", &to.to_string(), "0.25", false)).expect("a withdraw");
    let (tx, instructions) = decode(&details);
    assert!(instructions.len() == 1, "one instruction");
    assert!(instructions[0].program == SYSTEM_PROGRAM, "the system program's");
    assert!(
        instructions[0].data == system_transfer_data(250_000_000),
        "a transfer of 0.25 SOL"
    );
    assert!(
        instructions[0].accounts == vec![wallet(), to],
        "from the wallet to the recipient"
    );
    assert!(tx.message.account_keys[0] == wallet(), "the wallet pays");
    assert!(
        tx.signatures[0].verify(wallet().as_ref(), &tx.message_data()),
        "signed by the wallet"
    );
    assert_eq!(details.total_amount, dec("0.25"));
    assert_eq!(details.spent_by_me, dec("0.250005"));
    assert_eq!(details.received_by_me, 0.into());
    assert_eq!(details.my_balance_change, dec("-0.250005"));
    assert!(
        details.from == vec![wallet().to_string()] && details.to == vec![to.to_string()],
        "from and to"
    );
    assert!(
        matches!(details.transaction_type, TransactionType::StandardTransfer),
        "a standard transfer"
    );
    assert!(
        details.internal_id.0 == details.tx_hash.as_bytes(),
        "internal_id is the hex of tx_hash's characters"
    );
    assert!(
        details.tx_hash == tx.signatures[0].to_string(),
        "tx_hash is the first signature"
    );
    assert!(
        details.block_height == 0 && details.memo.is_none(),
        "no height, no memo"
    );

    // To the wallet itself: the amount comes back.
    let details = withdraw(&coin, request("SOL", &wallet().to_string(), "0.25", false)).expect("a withdraw");
    assert_eq!(details.received_by_me, dec("0.25"));
    assert_eq!(details.my_balance_change, dec("-0.000005"));
}

/// T40.6B.12: `max` is the balance less the fee; the answer spends the whole balance.
#[test]
fn t40_6b_12_sol_max_is_balance_less_fee() {
    let (_node, coin) = sol_node(1_000_000_000);
    let details = withdraw(&coin, request("SOL", &other_wallet().to_string(), "0", true)).expect("a withdraw");
    let (_, instructions) = decode(&details);
    assert!(
        instructions[0].data == system_transfer_data(1_000_000_000 - FEE),
        "balance less fee"
    );
    assert_eq!(details.total_amount, dec("0.999995"));
    assert_eq!(details.spent_by_me, dec("1"));
    assert_eq!(details.my_balance_change, dec("-1"));

    // A balance that is at most the fee leaves nothing: `AmountTooLow`.
    for lamports in [0, FEE - 1, FEE] {
        let (_node, coin) = sol_node(lamports);
        let error = error_of(withdraw(&coin, request("SOL", &other_wallet().to_string(), "0", true)));
        assert!(
            matches!(&error, WithdrawError::AmountTooLow { amount, threshold }
                if *amount == 0.into() && *threshold == dec("0.000000001")),
            "{error:?}"
        );
    }
}

/// T40.6B.12: amount plus fee above the balance is `NotSufficientBalance` with `required` = amount plus fee.
#[test]
fn t40_6b_12_sol_amount_plus_fee_above_balance() {
    let (_node, coin) = sol_node(1_000_000_000);
    let to = other_wallet().to_string();
    let error = error_of(withdraw(&coin, request("SOL", &to, "1", false)));
    assert!(
        matches!(&error, WithdrawError::NotSufficientBalance { coin, available, required }
            if coin == "SOL" && *available == dec("1") && *required == dec("1.000005")),
        "{error:?}"
    );
    let (body, status) = wire(&error);
    assert!(
        keys_of(&body["error_data"]) == names(&["coin", "available", "required"]),
        "the data members"
    );
    assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");
    // Exactly the balance less the fee still passes.
    assert!(
        withdraw(&coin, request("SOL", &to, "0.999995", false)).is_ok(),
        "amount plus fee equals the balance"
    );
}

/// T40.6B.12: below one lamport is `AmountTooLow`; digits beyond the ninth decimal are dropped.
#[test]
fn t40_6b_12_sol_amount_limits_and_truncation() {
    let (_node, coin) = sol_node(1_000_000_000);
    let to = other_wallet().to_string();
    for amount in ["0", "0.0000000001", "0.0000000009", "-1"] {
        let error = error_of(withdraw(&coin, request("SOL", &to, amount, false)));
        assert!(
            matches!(&error, WithdrawError::AmountTooLow { threshold, .. } if *threshold == dec("0.000000001")),
            "{error:?}"
        );
    }
    let details = withdraw(&coin, request("SOL", &to, "0.1234567891", false)).expect("a withdraw");
    assert!(
        decode(&details).1[0].data == system_transfer_data(123_456_789),
        "digits beyond the ninth decimal are dropped, not rounded"
    );
    assert_eq!(details.total_amount, dec("0.123456789"));
}

/// T40.6B.12: a `to` that is not 32 bytes of base58 is `InvalidAddress`, HTTP 400.
#[test]
fn t40_6b_12_sol_invalid_recipient() {
    let (_node, coin) = sol_node(1_000_000_000);
    let (thirty_one, thirty_three) = ([7u8; 31].to_base58(), [7u8; 33].to_base58());
    for text in ["", "abc", "0OIl", thirty_one.as_str(), thirty_three.as_str()] {
        let error = error_of(withdraw(&coin, request("SOL", text, "0.1", false)));
        assert!(matches!(error, WithdrawError::InvalidAddress(_)), "{error:?}");
        let (_, status) = wire(&error);
        assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");
    }
    // A key of 32 bytes need not be on the curve or 44 characters long.
    let short = (0..=255u8)
        .map(|first| {
            let mut key = [0xAB_u8; 32];
            key[0] = first;
            key.to_base58()
        })
        .find(|text| text.len() < 44)
        .expect("a 32-byte key whose text is shorter than 44 characters");
    assert!(
        withdraw(&coin, request("SOL", &short, "0.1", false)).is_ok(),
        "a shorter address is accepted"
    );
}

/// T40.6B.12: `fee`, `memo`, `from` and unknown members change nothing: the same request with and without them
/// gives the same signed transaction.
#[test]
fn t40_6b_12_sol_ignored_members_change_nothing() {
    let (_node, coin) = sol_node(1_000_000_000);
    let to = other_wallet().to_string();
    let plain = withdraw(&coin, request("SOL", &to, "0.5", false)).expect("a withdraw");
    let mut with_fee = request("SOL", &to, "0.5", false);
    with_fee.fee = Some(WithdrawFee::UtxoFixed { amount: dec("123") });
    with_fee.memo = Some("memo".to_string());
    let with_fee = withdraw(&coin, with_fee).expect("a withdraw");
    assert!(plain.tx_hex == with_fee.tx_hex, "the same transaction");
    assert!(plain.tx_hash == with_fee.tx_hash, "the same hash");
    assert!(plain.memo.is_none() && with_fee.memo.is_none(), "no memo in the answer");
    let from: WithdrawRequest = serde_json::from_value(json!({
        "coin": "SOL", "to": to, "amount": "0.5", "from": { "derivation_path": "m/44'/501'" }, "broadcast": true,
        "something_else": 1
    }))
    .expect("a request with unknown members");
    let with_members = withdraw(&coin, from).expect("a withdraw");
    assert!(plain.tx_hex == with_members.tx_hex, "the same transaction");
}

// ---------------------------------------------------------------------------------------------------------------
// fee_details and send_raw_transaction
// ---------------------------------------------------------------------------------------------------------------

/// T40.6B.13: the `fee_details` of a SOL and of an SPL withdraw is exactly `type`, `fee_amount`, `rent_amount` and
/// `total_amount`, the last three decimal strings in SOL, and the stored form reads back as Solana.
#[test]
fn t40_6b_13_fee_details_wire_shape() {
    let node = spawn(Script::new().with_sol(wallet(), 1_000_000_000).with_token_account(
        &wallet(),
        &SPL_TOKEN_PROGRAM_ID,
        12_500_000,
        6,
    ));
    let sol = sol_on(&node.url);
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let to = other_wallet().to_string();
    let sol_details = withdraw(&sol, request("SOL", &to, "0.1", false)).expect("a withdraw");
    let token_details = withdraw(&token, request("USDC", &to, "1", false)).expect("a withdraw");
    for (details, rent) in [(sol_details, "0"), (token_details, "0.00203928")] {
        let body = serde_json::to_value(&details.fee_details).expect("serializable");
        assert!(
            keys_of(&body) == names(&["type", "fee_amount", "rent_amount", "total_amount"]),
            "exactly the four members"
        );
        assert!(body["type"] == "Solana", "the type");
        assert!(body["fee_amount"] == "0.000005", "the fee");
        assert!(body["rent_amount"] == rent, "the rent");
        assert!(
            body["total_amount"].is_string()
                && dec(body["total_amount"].as_str().unwrap_or_default()) == dec("0.000005") + dec(rent),
            "the total is fee plus rent"
        );
        let stored: Option<TxFeeDetails> = serde_json::from_value(body).expect("a stored entry reads back");
        assert!(
            matches!(stored, Some(TxFeeDetails::Solana(_))),
            "it reads back as Solana"
        );
    }
}

/// T40.6B.15: `send_raw_transaction` with the `tx_hex` of a withdraw answers the withdraw's `tx_hash`, on SOL and
/// on a token (which takes the platform's path).
#[test]
fn t40_6b_15_send_raw_transaction_answers_the_withdraw_hash() {
    let node = spawn(Script::new().with_sol(wallet(), 1_000_000_000).with_token_account(
        &wallet(),
        &SPL_TOKEN_PROGRAM_ID,
        12_500_000,
        6,
    ));
    let sol = sol_on(&node.url);
    let token = token_on(&node.url, SPL_TOKEN_PROGRAM_ID, 6);
    let to = other_wallet().to_string();
    let sol_details = withdraw(&sol, request("SOL", &to, "0.1", false)).expect("a withdraw");
    let hash = block_on(sol.send_raw_tx(&hex::encode(&sol_details.tx_hex.0)).compat()).expect("a broadcast");
    assert!(hash == sol_details.tx_hash, "SOL: the same hash");
    let hash = block_on(sol.send_raw_tx_bytes(&sol_details.tx_hex.0).compat()).expect("a broadcast");
    assert!(hash == sol_details.tx_hash, "SOL: the same hash from bytes");

    let token_details = withdraw(&token, request("USDC", &to, "1", false)).expect("a withdraw");
    let hash = block_on(token.send_raw_tx(&hex::encode(&token_details.tx_hex.0)).compat()).expect("a broadcast");
    assert!(hash == token_details.tx_hash, "token: the same hash");
    assert!(
        node.params_of("sendTransaction").len() == 3,
        "all three went to the node"
    );
}
