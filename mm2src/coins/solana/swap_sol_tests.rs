//! Tests of the SOL legs of the legacy swap (CRD ch.40 §40.6D, T40.6D.1 to T40.6D.13, the SOL parts).
//!
//! The payment, claim and refund transactions of T40.6D.5 and the program accounts of T40.6D.1 are public Solana
//! devnet data of the swap program `54mDjQz6jS9sggCV1P5GoUzqHaBEueGVZqmaMPLt2tDx`, captured by JSON-RPC on
//! 2026-11-22 into `test_fixtures/devnet_swap.json` (see its `_source` member): the payment `3G7v3uGo...` and its
//! claim `5SRwg5RW...` (payment address `Bxj7WMbn...`), the payment `3ih1aXsp...` and its refund `3JE5cZ3Y...`
//! (payment address `2jkYdBoU...`), the program account and the header of its program-data account. A scripted node on
//! a loopback socket serves them, so the real HTTP layer, node pool and RPC client run. Every answer a test alters or
//! builds by hand is marked "altered" or "hand-built" at the place; no captured transaction exists for those cases
//! (no captured claim is an inner instruction, no captured payment failed).
//!
//! No assertion message carries a key-derived value: values are compared with `assert!(a == b, "<fixed text>")`.

use super::solana_common_tests::generate_key_pair_from_iguana_seed;
use super::swap_decode::top_level_instructions;
use super::swap_program::{check_program_account, check_program_data, memo_instruction, ProgramCheck, SolCall,
                          SolTerms, MEMO_PROGRAM_ID};
use super::swap_sol::{exact_lamports, parse_swap_key, swap_key_field, SwapTiming, PAYMENT_ACCOUNT_SPACE};
use super::*;
use crate::{CanRefundHtlc, DexFee, HtlcPubkeyError, SwapOps, Transaction as CoinTransaction, ValidateFeeArgs,
            ValidatePaymentInput};
use common::block_on;
use futures::compat::Future01CompatExt;
use serde_json::json;
use solana_message::{Instruction, Message};
use solana_transaction::versioned::VersionedTransaction;
use std::collections::{HashMap as Map, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const FIXTURES: &str = include_str!("test_fixtures/devnet_swap.json");
const PROGRAM: &str = "54mDjQz6jS9sggCV1P5GoUzqHaBEueGVZqmaMPLt2tDx";
const PAYMENT_1: &str = "3G7v3uGoADWCxkYjN3BrPNpdMKHBymT7YLNa9dqFeMKHRNomVHbVSmmH52qzhhVqA4VKiMMMkwByoipNJhjj312a";
const CLAIM_1: &str = "5SRwg5RWPMt2SosTeA6H6iSqVvPhYjgjvaUyX38iNsTh1PrRsNeStgQuCJKUoDiN9Yd7Zh6Qdnzicf8ZCnnLpFRr";
const PAYMENT_ADDRESS_1: &str = "Bxj7WMbnd7iAaN5AGpPcVR1LecJSDYCy6HWtpRKsSY7Z";
const PAYMENT_2: &str = "3ih1aXspkqeiQ1SqgJhqLzxJ87kNjYe2Qrz3Mgyng36CMujky4e9SvCx4EAtRFh368rMNgEUiCzcP4tKSWsDJMX5";
const REFUND_2: &str = "3JE5cZ3YxYbHmFTjAizvhAp7B4jonz9thyuGVcqQcQJXT37DE3i8LmqSgSMu4Farfs4KMYbKjUaARUSLwxGn7kr8";
const PAYMENT_ADDRESS_2: &str = "2jkYdBoU6Kh21K1e5PuaTdPedfgXivbzLE6rSLPid5Hu";
const PROGRAM_DATA_ADDRESS: &str = "D8p7EfW7dqGJECToSvxqA4oLiBDNrzEhJbAE54DHFJdy";
/// The rent-exempt minimum per byte on devnet, from the captured deposit (695,960 lamports for the nine bytes).
const RENT_PER_BYTE: u64 = 5080;

fn fixtures() -> Json { serde_json::from_str(FIXTURES).expect("the fixture file parses") }

fn key(text: &str) -> Pubkey { Pubkey::from_str(text).expect("a key") }

fn program() -> Pubkey { key(PROGRAM) }

fn tx_answer(signature: &str) -> Json { fixtures()["transactions"][signature].clone() }

fn wire_of(signature: &str) -> Vec<u8> {
    base64::decode(tx_answer(signature)["transaction"][0].as_str().expect("wire")).expect("base64")
}

/// The payment terms of the captured payment transaction `signature`, read from its program instruction.
fn captured_terms(signature: &str) -> SolTerms {
    let instructions = top_level_instructions(&wire_of(signature)).expect("decodes");
    instructions
        .iter()
        .filter(|ix| ix.program == program())
        .find_map(|ix| match SolCall::parse(&ix.data)? {
            SolCall::Payment {
                secret_hash,
                lock_time,
                amount,
            } => Some(SolTerms {
                sender: ix.accounts[0],
                receiver: ix.accounts[1],
                secret_hash,
                lock_time,
                amount,
            }),
            _ => None,
        })
        .expect("a payment instruction")
}

fn field(key: &Pubkey) -> Vec<u8> { swap_key_field(key).to_vec() }

// ---------------------------------------------------------------------------------------------------------------
// The scripted node
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

/// An account of the scripted chain.
#[derive(Clone)]
struct Acct {
    lamports: u64,
    owner: String,
    executable: bool,
    data: Vec<u8>,
}

/// What a scripted node knows and which of its methods fail.
#[derive(Default)]
struct World {
    transactions: Map<String, Json>,
    signatures: Map<String, Vec<Json>>,
    accounts: Map<String, Acct>,
    /// The latest finalized slot's block time; `None`: the block has no time.
    block_time: Option<i64>,
    block_height: u64,
    fail: HashSet<String>,
    /// The slot the node answers account reads at (its finalized slot).
    context_slot: u64,
    /// The most entries a listing page holds, whatever `limit` asks; 0 is no cap.
    page_cap: usize,
    /// The error `simulateTransaction` answers; `None`: the simulation succeeds.
    simulate_err: Option<Json>,
}

/// A slot after every captured transaction.
const FINALIZED_SLOT: u64 = 523_686_000;
/// The slot of an entry whose transaction the fixtures do not hold (between the payment and the finalized slot).
const MID_SLOT: u64 = 523_685_300;

fn fixture_world() -> Arc<Mutex<World>> {
    let fx = fixtures();
    let mut world = World {
        block_time: Some(1_795_000_000),
        block_height: 1_000_000,
        context_slot: FINALIZED_SLOT,
        ..World::default()
    };
    for (signature, answer) in fx["transactions"].as_object().expect("transactions") {
        world.transactions.insert(signature.clone(), answer.clone());
    }
    Arc::new(Mutex::new(world))
}

fn sig_entry(signature: &str, failed: bool) -> Json {
    let slot = fixtures()["transactions"][signature]["slot"]
        .as_u64()
        .unwrap_or(MID_SLOT);
    sig_entry_at(signature, slot, failed)
}

fn sig_entry_at(signature: &str, slot: u64, failed: bool) -> Json {
    json!({ "signature": signature, "slot": slot, "err": if failed { json!({"InstructionError": [0, "Custom"]}) } else { Json::Null }, "blockTime": 1 })
}

fn node_over(world: Arc<Mutex<World>>) -> Stub {
    spawn_stub(move |method, params| {
        let world = world.lock().expect("the world");
        if world.fail.contains(method) {
            return Answer::Error;
        }
        match method {
            "getHealth" => Answer::Result(json!("ok")),
            "getTransaction" => Answer::Result(
                world
                    .transactions
                    .get(params[0].as_str().unwrap_or_default())
                    .cloned()
                    .unwrap_or(Json::Null),
            ),
            "getSignaturesForAddress" => {
                let mut list = world
                    .signatures
                    .get(params[0].as_str().unwrap_or_default())
                    .cloned()
                    .unwrap_or_default();
                if let Some(until) = params[1]["until"].as_str() {
                    if let Some(at) = list.iter().position(|e| e["signature"] == until) {
                        list.truncate(at);
                    }
                }
                if let Some(before) = params[1]["before"].as_str() {
                    if let Some(at) = list.iter().position(|e| e["signature"] == before) {
                        list = list.split_off(at + 1);
                    }
                }
                let mut limit = params[1]["limit"].as_u64().unwrap_or(1000) as usize;
                if world.page_cap > 0 {
                    limit = limit.min(world.page_cap);
                }
                list.truncate(limit);
                Answer::Result(Json::Array(list))
            },
            "getAccountInfo" => {
                let value = match world.accounts.get(params[0].as_str().unwrap_or_default()) {
                    None => Json::Null,
                    Some(acct) => {
                        let mut data = acct.data.clone();
                        if let Some(length) = params[1]["dataSlice"]["length"].as_u64() {
                            data.truncate(length as usize);
                        }
                        json!({ "lamports": acct.lamports, "owner": acct.owner, "executable": acct.executable,
                            "data": [base64::encode(&data), "base64"], "rentEpoch": 0, "space": acct.data.len() })
                    },
                };
                Answer::Result(json!({ "context": { "slot": world.context_slot }, "value": value }))
            },
            "getMinimumBalanceForRentExemption" => {
                Answer::Result(json!((128 + params[0].as_u64().unwrap_or_default()) * RENT_PER_BYTE))
            },
            "getSlot" => Answer::Result(json!(777)),
            "getBlock" => match world.block_time {
                Some(time) => Answer::Result(json!({ "blockTime": time, "blockHeight": world.block_height })),
                None => Answer::Result(json!({ "blockHeight": world.block_height })),
            },
            "getLatestBlockhash" => Answer::Result(json!({ "context": { "slot": 1 },
                "value": { "blockhash": "11111111111111111111111111111111", "lastValidBlockHeight": 100 } })),
            "simulateTransaction" => {
                Answer::Result(json!({ "context": { "slot": 1 }, "value": { "err": world.simulate_err, "logs": [] } }))
            },
            "getFeeForMessage" => Answer::Result(json!({ "context": { "slot": 1 }, "value": 5000 })),
            "sendTransaction" => {
                let wire = base64::decode(params[0].as_str().unwrap_or_default()).unwrap_or_default();
                match bincode::deserialize::<Transaction>(&wire) {
                    Ok(tx) => Answer::Result(json!(tx.signatures[0].to_string())),
                    Err(_) => Answer::Error,
                }
            },
            _ => Answer::Error,
        }
    })
}

fn quick() -> SwapTiming {
    SwapTiming {
        poll: Duration::from_millis(10),
        validation_deadline: Duration::from_millis(250),
        claim_retry: Duration::from_millis(250),
    }
}

/// A coin of a fixed seed over the nodes at `urls`, trading with `program`.
fn coin_over(urls: Vec<String>) -> SolanaCoin {
    let key_pair = generate_key_pair_from_iguana_seed("solana swap tests".to_owned());
    let client = SolanaRpcPool::without_commitment(urls).expect("a pool");
    let coin = SolanaCoin(Arc::new(SolanaCoinImpl {
        ticker: "SOL".to_owned(),
        my_address: key_pair.pubkey().to_string(),
        key_pair,
        client,
        decimals: 9,
        spl_tokens_infos: Arc::new(Mutex::new(HashMap::new())),
        settings: SolanaCoinSettings::default(),
        history: SolanaHistoryStates::default(),
        swap: SolanaSwapConfig::default(),
    }));
    coin.set_swap_state_for_test(SwapProgramState::Tradable(program()));
    coin.set_swap_timing(quick());
    coin
}

fn one_node_coin() -> (SolanaCoin, Stub, Arc<Mutex<World>>) {
    let world = fixture_world();
    let stub = node_over(world.clone());
    let coin = coin_over(vec![stub.url.clone()]);
    (coin, stub, world)
}

fn lamports(amount: u64) -> BigDecimal { crate::solana::solana_common::lamports_to_sol(amount) }

fn validate(coin: &SolanaCoin, signature: &str, terms: &SolTerms, amount: u64) -> Result<(), String> {
    block_on(
        coin.validate_maker_payment(ValidatePaymentInput {
            payment_tx: wire_of(signature),
            time_lock: terms.lock_time as u32,
            taker_pub: field(&terms.receiver),
            maker_pub: field(&terms.sender),
            secret_hash: terms.secret_hash.to_vec(),
            amount: lamports(amount),
            swap_contract_address: None,
            try_spv_proof_until: 0,
            confirmations: 1,
        })
        .compat(),
    )
}

/// The world with the payment `signature` served and its payment account at `address` holding `lamports`.
fn paid(world: &Arc<Mutex<World>>, address: &str, held: u64) {
    world.lock().expect("world").accounts.insert(address.to_owned(), Acct {
        lamports: held,
        owner: PROGRAM.to_owned(),
        executable: false,
        data: vec![0u8; PAYMENT_ACCOUNT_SPACE],
    });
}

fn rent(len: usize) -> u64 { (128 + len as u64) * RENT_PER_BYTE }

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.1: configuration and the program check
// ---------------------------------------------------------------------------------------------------------------

fn program_world(
    owner: &str,
    executable: bool,
    data: Vec<u8>,
    programdata: Option<(String, Acct)>,
) -> Arc<Mutex<World>> {
    let world = fixture_world();
    {
        let mut w = world.lock().expect("world");
        w.accounts.insert(PROGRAM.to_owned(), Acct {
            lamports: 1,
            owner: owner.to_owned(),
            executable,
            data,
        });
        if let Some((address, account)) = programdata {
            w.accounts.insert(address, account);
        }
    }
    world
}

fn checked_state(world: Arc<Mutex<World>>, configured: Option<Pubkey>) -> (Option<WalletOnlyReason>, Option<Pubkey>) {
    let stub = node_over(world);
    let coin = coin_over(vec![stub.url.clone()]);
    block_on(coin.check_swap_program(configured));
    (coin.swap_wallet_only_reason(), coin.swap_program())
}

const UPGRADEABLE: &str = "BPFLoaderUpgradeab1e11111111111111111111111";

fn captured_program_account() -> Acct {
    let account = &fixtures()["program_account"];
    Acct {
        lamports: account["lamports"].as_u64().expect("lamports"),
        owner: account["owner"].as_str().expect("owner").to_owned(),
        executable: account["executable"].as_bool().expect("executable"),
        data: base64::decode(account["data"][0].as_str().expect("data")).expect("base64"),
    }
}

fn captured_header() -> Acct {
    let account = &fixtures()["program_data_header"];
    Acct {
        lamports: account["lamports"].as_u64().expect("lamports"),
        owner: account["owner"].as_str().expect("owner").to_owned(),
        executable: false,
        data: base64::decode(account["data"][0].as_str().expect("data")).expect("base64"),
    }
}

#[test]
fn t40_6d_1_the_captured_devnet_program_is_upgradeable_and_stays_wallet_only() {
    let program_account = captured_program_account();
    let world = program_world(
        &program_account.owner.clone(),
        program_account.executable,
        program_account.data.clone(),
        Some((PROGRAM_DATA_ADDRESS.to_owned(), captured_header())),
    );
    let (reason, tradable) = checked_state(world, Some(program()));
    assert!(
        reason == Some(WalletOnlyReason::ProgramUpgradeable),
        "the devnet program names an authority"
    );
    assert!(tradable.is_none(), "an upgradeable program is not tradable");
}

#[test]
fn t40_6d_1_the_program_check_over_hand_built_accounts() {
    // Hand-built: the program-data header of the captured program with its authority removed (option tag 0).
    let mut no_authority = captured_header();
    no_authority.data[12] = 0;
    let program_account = captured_program_account();
    let world = program_world(
        UPGRADEABLE,
        true,
        program_account.data.clone(),
        Some((PROGRAM_DATA_ADDRESS.to_owned(), no_authority)),
    );
    let (reason, tradable) = checked_state(world, Some(program()));
    assert!(
        reason.is_none() && tradable == Some(program()),
        "no authority passes (hand-built header)"
    );

    // Hand-built: a program of the loader without upgrades.
    let world = program_world("BPFLoader2111111111111111111111111111111111", true, vec![], None);
    let (reason, tradable) = checked_state(world, Some(program()));
    assert!(
        reason.is_none() && tradable == Some(program()),
        "loader 2 passes (hand-built account)"
    );
    let world = program_world("BPFLoader1111111111111111111111111111111111", true, vec![], None);
    let (_, tradable) = checked_state(world, Some(program()));
    assert!(
        tradable == Some(program()),
        "the deprecated loader passes (hand-built account)"
    );

    // Not executable, loader v4, and a missing account.
    let world = program_world("BPFLoader2111111111111111111111111111111111", false, vec![], None);
    let (reason, _) = checked_state(world, Some(program()));
    assert!(
        reason == Some(WalletOnlyReason::ProgramNotExecutable),
        "a program that is not executable is refused"
    );
    let world = program_world("LoaderV411111111111111111111111111111111111", true, vec![], None);
    let (reason, _) = checked_state(world, Some(program()));
    assert!(
        reason == Some(WalletOnlyReason::ProgramLoaderUnsupported),
        "loader v4 is refused"
    );
    let (reason, _) = checked_state(fixture_world(), Some(program()));
    assert!(
        reason == Some(WalletOnlyReason::ProgramMissing),
        "a missing program is refused"
    );
}

#[test]
fn t40_6d_1_an_unreadable_program_and_an_absent_member() {
    let world = fixture_world();
    world.lock().expect("world").fail.insert("getAccountInfo".to_owned());
    let (reason, tradable) = checked_state(world, Some(program()));
    assert!(
        reason == Some(WalletOnlyReason::ProgramUnreadable) && tradable.is_none(),
        "a failed read is wallet-only"
    );
    let (reason, _) = checked_state(fixture_world(), None);
    assert!(
        reason == Some(WalletOnlyReason::NoProgramConfigured),
        "no member, no program"
    );
}

#[test]
fn t40_6d_1_the_loader_judgement_is_pure_and_total() {
    let upgradeable = key(UPGRADEABLE);
    // A program account of the upgradeable loader with the wrong length or tag is refused.
    assert!(
        check_program_account(&upgradeable, true, &[2, 0, 0, 0])
            == ProgramCheck::Refused(WalletOnlyReason::ProgramLoaderUnsupported),
        "a short account is refused"
    );
    assert!(
        check_program_data(&upgradeable, &[3, 0, 0, 0]).is_err(),
        "a short program-data account is refused"
    );
    assert!(
        check_program_data(&key(PROGRAM), &captured_header().data).is_err(),
        "another owner is refused"
    );
}

#[test]
fn t40_6d_1_swap_program_id_of_a_coins_entry() {
    use crate::solana::swap_program_id_from_conf;
    let conf =
        |member: Json| json!({ "protocol": { "type": "SOLANA", "protocol_data": { "swap_program_id": member } } });
    assert!(
        swap_program_id_from_conf(&conf(json!(PROGRAM))) == Ok(Some(program())),
        "a valid member parses"
    );
    assert!(
        swap_program_id_from_conf(&json!({ "protocol": { "type": "SOLANA" } })) == Ok(None),
        "absent is none"
    );
    assert!(swap_program_id_from_conf(&conf(Json::Null)) == Ok(None), "null is none");
    assert!(
        swap_program_id_from_conf(&conf(json!("not base58 0OIl"))).is_err(),
        "an invalid member is refused"
    );
    assert!(
        swap_program_id_from_conf(&conf(json!(5))).is_err(),
        "a member of another type is refused"
    );
    assert!(
        swap_program_id_from_conf(&conf(json!("1111"))).is_err(),
        "a short key is refused"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.2: the contract field
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_2_the_contract_field() {
    let coin = coin_over(vec!["http://127.0.0.1:1".to_owned()]);
    let own = program().to_bytes();
    assert!(
        coin.negotiate_swap_contract_addr(Some(&own)).expect("accepted") == Some(own.to_vec().into()),
        "equal 32 bytes are accepted"
    );
    let short = [1u8; 20];
    assert!(
        matches!(
            coin.negotiate_swap_contract_addr(Some(&short))
                .map_err(|e| e.into_inner()),
            Err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(_))
        ),
        "another length is refused"
    );
    let other = [9u8; 32];
    assert!(
        matches!(
            coin.negotiate_swap_contract_addr(Some(&other))
                .map_err(|e| e.into_inner()),
            Err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(_))
        ),
        "other bytes are refused"
    );
    for absent in [None, Some(&[][..])] {
        assert!(
            matches!(
                coin.negotiate_swap_contract_addr(absent).map_err(|e| e.into_inner()),
                Err(NegotiateSwapContractAddrErr::NoSwapContract)
            ),
            "an absent or empty field is refused"
        );
    }
    let wallet_only = coin_over(vec!["http://127.0.0.1:1".to_owned()]);
    wallet_only.set_swap_state_for_test(SwapProgramState::WalletOnly(WalletOnlyReason::NoProgramConfigured));
    assert!(
        matches!(
            wallet_only
                .negotiate_swap_contract_addr(Some(&own))
                .map_err(|e| e.into_inner()),
            Err(NegotiateSwapContractAddrErr::NoSwapContract)
        ),
        "a wallet-only coin has no contract"
    );
    assert!(
        wallet_only.swap_contract_address().is_none(),
        "a wallet-only coin names no program"
    );
    assert!(
        coin.swap_contract_address() == Some(own.to_vec().into()),
        "a tradable coin names its program"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.4: keys
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_4_the_wire_form_of_the_key() {
    let coin = coin_over(vec!["http://127.0.0.1:1".to_owned()]);
    let me = coin.key_pair.pubkey();
    let node_key = keys::Public::Compressed(Default::default());
    let wire = coin.derive_htlc_pubkey(&node_key).expect("a key field");
    assert!(
        wire[..32] == me.to_bytes() && wire[32] == 0,
        "the 32-byte key then one zero byte"
    );
    assert!(
        coin.validate_other_pubkey(&wire).is_err(),
        "the own key is not a counterparty's"
    );
    let other = generate_key_pair_from_iguana_seed("another party".to_owned()).pubkey();
    assert!(
        coin.validate_other_pubkey(&swap_key_field(&other)).is_ok(),
        "another party's wire form is accepted"
    );
    assert!(parse_swap_key(&wire).expect("parses") == me, "the key is read back");
}

#[test]
fn t40_6d_4_a_non_zero_last_byte_and_an_off_curve_key_are_refused() {
    let coin = coin_over(vec!["http://127.0.0.1:1".to_owned()]);
    let mut wire = swap_key_field(&coin.key_pair.pubkey());
    wire[32] = 1;
    assert!(
        matches!(
            coin.validate_other_pubkey(&wire).map_err(|e| e.into_inner()),
            Err(HtlcPubkeyError::NotOnCurve(..))
        ),
        "a non-zero last byte is refused"
    );
    // The first value b whose 32 bytes [b; 32] are not a curve point (there are such values).
    let off_curve = (0u8..=255)
        .find(|b| !solana_pubkey::bytes_are_curve_point([*b; 32]))
        .expect("an off-curve key exists");
    let mut wire = [off_curve; 33];
    wire[32] = 0;
    assert!(
        matches!(
            coin.validate_other_pubkey(&wire).map_err(|e| e.into_inner()),
            Err(HtlcPubkeyError::NotOnCurve(..))
        ),
        "an off-curve key is refused"
    );
    assert!(
        matches!(
            coin.validate_other_pubkey(&wire[..32]).map_err(|e| e.into_inner()),
            Err(HtlcPubkeyError::UnexpectedLength(32))
        ),
        "another width is refused"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.5: instructions byte-exact against the captured transactions
// ---------------------------------------------------------------------------------------------------------------

/// The instruction of `program` in the captured transaction `signature`: its account keys with their signer and
/// writable flags, and its data.
fn captured_instruction(signature: &str) -> (Vec<(Pubkey, bool, bool)>, Vec<u8>) {
    let wire = wire_of(signature);
    let tx: VersionedTransaction = bincode::deserialize(&wire).expect("a transaction");
    let message = match &tx.message {
        solana_message::VersionedMessage::Legacy(message) => message.clone(),
        solana_message::VersionedMessage::V0(_) => panic!("the captured transaction is a legacy one"),
    };
    let ix = message
        .instructions
        .iter()
        .find(|ix| message.account_keys[usize::from(ix.program_id_index)] == program())
        .expect("an instruction of the program");
    let accounts = ix
        .accounts
        .iter()
        .map(|index| {
            let index = usize::from(*index);
            (
                message.account_keys[index],
                message.is_signer(index),
                message.is_maybe_writable(index, None),
            )
        })
        .collect();
    (accounts, ix.data.clone())
}

fn ours(ix: &Instruction) -> (Vec<(Pubkey, bool, bool)>, Vec<u8>) {
    (
        ix.accounts
            .iter()
            .map(|m| (m.pubkey, m.is_signer, m.is_writable))
            .collect(),
        ix.data.clone(),
    )
}

#[test]
fn t40_6d_5_the_payment_instruction_and_its_address_match_the_captured_payments() {
    for (signature, address) in [(PAYMENT_1, PAYMENT_ADDRESS_1), (PAYMENT_2, PAYMENT_ADDRESS_2)] {
        let terms = captured_terms(signature);
        let (derived, _) = terms.payment_address(&program());
        assert!(
            derived == key(address),
            "the derived payment address is the captured one"
        );
        let (captured_accounts, captured_data) = captured_instruction(signature);
        let (my_accounts, my_data) = ours(&terms.payment_instruction(&program()));
        assert!(my_data == captured_data, "the instruction data is byte-exact");
        assert!(
            my_accounts == captured_accounts,
            "the accounts, their order and flags are the captured ones"
        );
    }
}

#[test]
fn t40_6d_5_the_claim_instruction_matches_the_captured_claim() {
    let terms = captured_terms(PAYMENT_1);
    let (captured_accounts, captured_data) = captured_instruction(CLAIM_1);
    let secret: [u8; 32] = captured_data[8..40].try_into().expect("a secret");
    assert!(
        kdf_crypto::sha256(&secret).to_vec() == terms.secret_hash,
        "the captured claim's secret hashes to the payment's hash"
    );
    let (my_accounts, my_data) = ours(&terms.spend_instruction(&program(), &secret));
    assert!(my_data == captured_data, "the claim's data is byte-exact");
    assert!(
        my_accounts == captured_accounts,
        "the claim's accounts and flags are the captured ones"
    );
}

#[test]
fn t40_6d_5_the_refund_instruction_matches_the_captured_refund() {
    let terms = captured_terms(PAYMENT_2);
    let (captured_accounts, captured_data) = captured_instruction(REFUND_2);
    let (my_accounts, my_data) = ours(&terms.refund_instruction(&program()));
    assert!(my_data == captured_data, "the refund's data is byte-exact");
    assert!(
        my_accounts == captured_accounts,
        "the refund's accounts and flags are the captured ones"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.6: whole lamports
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_6_a_volume_with_more_decimals_than_the_coin_is_refused() {
    let coin = coin_over(vec!["http://127.0.0.1:1".to_owned()]);
    let ok = BigDecimal::from_str("0.000000001").expect("a decimal");
    let too_fine = BigDecimal::from_str("0.0000000001").expect("a decimal");
    assert!(coin.check_swap_volume_units(&ok).is_ok(), "a whole lamport is accepted");
    assert!(
        coin.check_swap_volume_units(&BigDecimal::from_str("1.5").expect("a decimal"))
            .is_ok(),
        "1.5 SOL is accepted"
    );
    assert!(
        coin.check_swap_volume_units(&too_fine).is_err(),
        "a tenth of a lamport is refused"
    );
    assert!(exact_lamports(&too_fine).is_err(), "the send side refuses it too");
}

#[test]
fn t40_6d_6_the_send_side_refuses_a_fractional_lamport_before_any_node_read() {
    let (coin, stub, _) = one_node_coin();
    let me = field(&coin.key_pair.pubkey());
    let other = field(&captured_terms(PAYMENT_1).receiver);
    let result = block_on(
        coin.send_maker_payment(
            1_795_000_000,
            &me,
            &other,
            &[7u8; 32],
            BigDecimal::from_str("0.0000000001").expect("a decimal"),
            &None,
        )
        .compat(),
    );
    assert!(result.is_err(), "the payment is refused");
    assert!(stub.log.lock().expect("log").is_empty(), "no node was read or written");
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.7 and T40.6D.12: validation of the other side's payment
// ---------------------------------------------------------------------------------------------------------------

fn valid_world() -> (SolanaCoin, Stub, Arc<Mutex<World>>, SolTerms) {
    let (coin, stub, world) = one_node_coin();
    let terms = captured_terms(PAYMENT_1);
    // The payment account holds the amount above its rent-exempt minimum (exactly, the boundary).
    paid(&world, PAYMENT_ADDRESS_1, terms.amount + rent(PAYMENT_ACCOUNT_SPACE));
    (coin, stub, world, terms)
}

#[test]
fn t40_6d_7_a_valid_payment_is_accepted_at_the_boundary() {
    let (coin, stub, world, terms) = valid_world();
    // The evidence needs two nodes: a second node serving the same chain.
    let second = node_over(world.clone());
    let coin2 = coin_over(vec![stub.url.clone(), second.url.clone()]);
    assert!(
        validate(&coin2, PAYMENT_1, &terms, terms.amount).is_ok(),
        "the payment is valid"
    );
    assert!(
        stub.count("getTransaction") >= 1 && second.count("getTransaction") >= 1,
        "both nodes were asked"
    );
    drop(coin);
}

#[test]
fn t40_6d_7_each_term_that_differs_is_refused() {
    let (_, stub, world, terms) = valid_world();
    let second = node_over(world.clone());
    let coin = coin_over(vec![stub.url.clone(), second.url.clone()]);
    let mut other_hash = terms;
    other_hash.secret_hash[0] ^= 1;
    let mut other_lock = terms;
    other_lock.lock_time += 1;
    let mut other_sender = terms;
    other_sender.sender = terms.receiver;
    let mut other_receiver = terms;
    other_receiver.receiver = terms.sender;
    for (changed, amount) in [
        (other_hash, terms.amount),
        (other_lock, terms.amount),
        (other_sender, terms.amount),
        (other_receiver, terms.amount),
        (terms, terms.amount + 1),
    ] {
        assert!(
            validate(&coin, PAYMENT_1, &changed, amount).is_err(),
            "a payment with another term is refused"
        );
    }
}

#[test]
fn t40_6d_7_not_final_failed_missing_account_and_short_vault_are_refused() {
    let (_, stub, world, terms) = valid_world();
    let second = node_over(world.clone());
    let coin = coin_over(vec![stub.url.clone(), second.url.clone()]);

    // Not final: the node serves no finalized transaction of that signature.
    let saved = world
        .lock()
        .expect("world")
        .transactions
        .remove(PAYMENT_1)
        .expect("served");
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "a transaction that is not final is refused"
    );

    // Altered: the captured payment served as failed (no captured failed payment exists).
    let mut failed = saved.clone();
    failed["meta"]["err"] = json!({ "InstructionError": [2, "Custom"] });
    world
        .lock()
        .expect("world")
        .transactions
        .insert(PAYMENT_1.to_owned(), failed);
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "a failed transaction is refused"
    );
    world
        .lock()
        .expect("world")
        .transactions
        .insert(PAYMENT_1.to_owned(), saved);

    // The payment account is missing.
    world.lock().expect("world").accounts.remove(PAYMENT_ADDRESS_1);
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "a missing payment account is refused"
    );

    // The account holds one lamport less than the amount above its rent-exempt minimum.
    paid(
        &world,
        PAYMENT_ADDRESS_1,
        terms.amount + rent(PAYMENT_ACCOUNT_SPACE) - 1,
    );
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "a short account is refused"
    );

    // Owned by another program.
    paid(&world, PAYMENT_ADDRESS_1, terms.amount + rent(PAYMENT_ACCOUNT_SPACE));
    world
        .lock()
        .expect("world")
        .accounts
        .get_mut(PAYMENT_ADDRESS_1)
        .expect("account")
        .owner = "11111111111111111111111111111111".to_owned();
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "an account of another owner is refused"
    );
}

#[test]
fn t40_6d_7_a_doctored_body_under_a_true_signature_is_refused() {
    let (_, stub, world, terms) = valid_world();
    let second = node_over(world.clone());
    let coin = coin_over(vec![stub.url.clone(), second.url.clone()]);
    let validate_bytes = |peer: Vec<u8>| {
        block_on(
            coin.validate_maker_payment(ValidatePaymentInput {
                payment_tx: peer,
                time_lock: terms.lock_time as u32,
                taker_pub: field(&terms.receiver),
                maker_pub: field(&terms.sender),
                secret_hash: terms.secret_hash.to_vec(),
                amount: lamports(terms.amount),
                swap_contract_address: None,
                try_spv_proof_until: 0,
                confirmations: 1,
            })
            .compat(),
        )
    };
    assert!(
        validate_bytes(wire_of(PAYMENT_1)).is_ok(),
        "the node's own bytes are valid"
    );
    // Altered: the peer's bytes keep the first signature but carry another instruction data byte; the node's
    // transaction for that signature is the captured one, and the bytes differ from it.
    let mut peer = wire_of(PAYMENT_1);
    let last = peer.len() - 1;
    peer[last] ^= 0xff;
    assert!(
        validate_bytes(peer).is_err(),
        "bytes that differ from the node's for the signature are refused"
    );
}

#[test]
fn t40_6d_7_an_instruction_of_another_program_is_not_the_payment() {
    let (_, stub, world, terms) = valid_world();
    let second = node_over(world.clone());
    let coin = coin_over(vec![stub.url.clone(), second.url.clone()]);
    coin.set_swap_state_for_test(SwapProgramState::Tradable(key("11111111111111111111111111111112")));
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "the payment instruction of another program is refused"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.8: the refund gate
// ---------------------------------------------------------------------------------------------------------------

fn two_node_coin(first: Arc<Mutex<World>>, second: Arc<Mutex<World>>) -> (SolanaCoin, Stub, Stub) {
    let a = node_over(first);
    let b = node_over(second);
    let coin = coin_over(vec![a.url.clone(), b.url.clone()]);
    (coin, a, b)
}

fn gate(coin: &SolanaCoin, lock: u64) -> CanRefundHtlc {
    block_on(coin.can_refund_htlc(lock).compat()).expect("an answer")
}

#[test]
fn t40_6d_8_before_at_and_after_the_lock_by_the_clusters_time() {
    let world = fixture_world();
    let (coin, _a, _b) = two_node_coin(world.clone(), world.clone());
    let now = 1_795_000_000u64;
    assert!(
        matches!(gate(&coin, now + 100), CanRefundHtlc::HaveToWait(100)),
        "before the lock the remaining seconds"
    );
    assert!(
        matches!(gate(&coin, now), CanRefundHtlc::CanRefundNow),
        "at the lock refund now"
    );
    assert!(
        matches!(gate(&coin, now - 1), CanRefundHtlc::CanRefundNow),
        "after the lock refund now"
    );
}

#[test]
fn t40_6d_8_the_local_clock_never_opens_the_gate_and_a_failed_read_waits() {
    let world = fixture_world();
    // The lock lies in the local past, the cluster's time before it.
    let (coin, _a, _b) = two_node_coin(world.clone(), world.clone());
    assert!(
        matches!(gate(&coin, 1_795_000_001), CanRefundHtlc::HaveToWait(1)),
        "the cluster's time decides"
    );
    world.lock().expect("world").fail.insert("getSlot".to_owned());
    assert!(
        matches!(gate(&coin, 1), CanRefundHtlc::HaveToWait(_)),
        "a failed read waits"
    );
    let silent = fixture_world();
    silent.lock().expect("world").block_time = None;
    let (coin, _a, _b) = two_node_coin(silent.clone(), silent);
    assert!(
        matches!(gate(&coin, 1), CanRefundHtlc::HaveToWait(_)),
        "a block without time waits"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.9: the spend search
// ---------------------------------------------------------------------------------------------------------------

fn spend_search(coin: &SolanaCoin, signature: &str, terms: &SolTerms) -> Result<Option<FoundSwapTxSpend>, String> {
    block_on(coin.search_for_swap_tx_spend_my(
        terms.lock_time as u32,
        &[],
        &[],
        &terms.secret_hash,
        &wire_of(signature),
        0,
        &None,
    ))
}

fn two_agreeing(world: &Arc<Mutex<World>>) -> (SolanaCoin, Stub, Stub) { two_node_coin(world.clone(), world.clone()) }

#[test]
fn t40_6d_9_a_top_level_claim_gives_the_secret_and_the_account_must_be_gone() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(CLAIM_1, false),
            sig_entry(PAYMENT_1, false),
        ]);
    let (coin, _a, _b) = two_agreeing(&world);
    match spend_search(&coin, PAYMENT_1, &terms).expect("determined") {
        Some(FoundSwapTxSpend::Spent(tx)) => {
            assert!(tx.tx_hex() == wire_of(CLAIM_1), "the claim transaction is returned");
            let secret = coin
                .extract_secret(&terms.secret_hash, &tx.tx_hex())
                .expect("the secret");
            assert!(
                kdf_crypto::sha256(&secret).to_vec() == terms.secret_hash,
                "the secret hashes to the agreed hash"
            );
            let from_node = block_on(coin.extract_secret_from_spend(&terms.secret_hash, &tx.tx_hex())).expect("secret");
            assert!(from_node == secret, "the node-read secret is the same");
        },
        _ => panic!("a claim is found"),
    }
    // The payment account still exists: not spent.
    paid(&world, PAYMENT_ADDRESS_1, 1);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).expect("determined").is_none(),
        "not spent while the account exists"
    );
}

#[test]
fn t40_6d_9_a_refund_is_a_refund() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_2);
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_2.to_owned(), vec![
            sig_entry(REFUND_2, false),
            sig_entry(PAYMENT_2, false),
        ]);
    let (coin, _a, _b) = two_agreeing(&world);
    assert!(
        matches!(
            spend_search(&coin, PAYMENT_2, &terms).expect("determined"),
            Some(FoundSwapTxSpend::Refunded(_))
        ),
        "the refund is found as a refund"
    );
}

#[test]
fn t40_6d_9_a_wrong_secret_a_failed_claim_and_look_alikes_are_none() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(CLAIM_1, false),
            sig_entry(PAYMENT_1, false),
        ]);
    let (coin, _a, _b) = two_agreeing(&world);

    // Altered: the captured claim served with one byte of its secret changed (no captured wrong claim exists).
    let saved = world.lock().expect("world").transactions[CLAIM_1].clone();
    let mut wire = base64::decode(saved["transaction"][0].as_str().expect("wire")).expect("base64");
    let claim_data = captured_instruction(CLAIM_1).1;
    let at = wire
        .windows(claim_data.len())
        .position(|w| w == claim_data.as_slice())
        .expect("the data in the wire");
    wire[at + 8] ^= 1;
    let mut wrong = saved.clone();
    wrong["transaction"][0] = json!(base64::encode(&wire));
    world
        .lock()
        .expect("world")
        .transactions
        .insert(CLAIM_1.to_owned(), wrong);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).expect("determined").is_none(),
        "a wrong secret is not a claim"
    );

    // Altered: the captured claim served as failed in the transaction and in the listing.
    let mut failed = saved.clone();
    failed["meta"]["err"] = json!({ "InstructionError": [0, "Custom"] });
    world
        .lock()
        .expect("world")
        .transactions
        .insert(CLAIM_1.to_owned(), failed);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).expect("determined").is_none(),
        "a failed claim is neither"
    );
    world
        .lock()
        .expect("world")
        .transactions
        .insert(CLAIM_1.to_owned(), saved);
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(CLAIM_1, true),
            sig_entry(PAYMENT_1, false),
        ]);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).expect("determined").is_none(),
        "a listing marked failed is neither"
    );

    // The program for another payment address: the history of this address holds a claim of another payment.
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(CLAIM_1, false),
            sig_entry(PAYMENT_1, false),
        ]);
    let mut other_amount = terms;
    other_amount.amount += 1;
    let (address, _) = other_amount.payment_address(&program());
    world
        .lock()
        .expect("world")
        .signatures
        .insert(address.to_string(), vec![sig_entry(CLAIM_1, false)]);
    let (found, _) = block_on(coin.search_spend(&program(), &other_amount, PAYMENT_1, None)).expect("determined");
    assert!(found.is_none(), "a claim of another payment address is neither");

    // An instruction of another program with the same data and accounts.
    let (found, _) = block_on(coin.search_spend(&key("11111111111111111111111111111112"), &terms, PAYMENT_1, None))
        .expect("determined");
    assert!(
        found.is_none(),
        "a look-alike instruction of another program is neither"
    );
}

#[test]
fn t40_6d_9_a_claim_that_is_an_inner_instruction_gives_the_secret() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    let (accounts, data) = captured_instruction(CLAIM_1);
    // Hand-built: a transaction of another program whose inner instruction is the captured claim; the node answer
    // carries it in `innerInstructions` (no captured claim is an inner instruction). Unsigned, so its signature is the
    // all-zero one.
    let outer = Pubkey::new_from_array([9u8; 32]);
    let mut keys: Vec<Pubkey> = accounts.iter().map(|(k, _, _)| *k).collect();
    keys.push(program());
    let outer_ix = Instruction {
        program_id: outer,
        accounts: accounts
            .iter()
            .map(|(k, s, w)| solana_message::AccountMeta {
                pubkey: *k,
                is_signer: *s,
                is_writable: *w,
            })
            .collect(),
        data: vec![1],
    };
    let message = Message::new(&[outer_ix], Some(&accounts[0].0));
    let tx = Transaction::new_unsigned(message.clone());
    let wire = bincode::serialize(&tx).expect("serializes");
    let signature = tx.signatures[0].to_string();
    let index_of = |k: &Pubkey| message.account_keys.iter().position(|x| x == k).expect("a key");
    // The program is not in the message's key list; add it as the inner instruction's program through loaded
    // addresses (readonly).
    let program_index = message.account_keys.len();
    let answer = json!({
        "slot": MID_SLOT, "blockTime": 1,
        "transaction": [base64::encode(&wire), "base64"],
        "meta": { "err": null, "fee": 5000, "preBalances": vec![0u64; program_index + 1],
            "postBalances": vec![0u64; program_index + 1],
            "loadedAddresses": { "writable": [], "readonly": [PROGRAM] },
            "innerInstructions": [{ "index": 0, "instructions": [{
                "programIdIndex": program_index,
                "accounts": accounts.iter().map(|(k, _, _)| index_of(k)).collect::<Vec<_>>(),
                "data": data.to_base58() }] }] }
    });
    world
        .lock()
        .expect("world")
        .transactions
        .insert(signature.clone(), answer);
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(&signature, false),
            sig_entry(PAYMENT_1, false),
        ]);
    let (coin, _a, _b) = two_agreeing(&world);
    match spend_search(&coin, PAYMENT_1, &terms).expect("determined") {
        Some(FoundSwapTxSpend::Spent(found)) => {
            let secret = block_on(coin.extract_secret_from_spend(&terms.secret_hash, &found.tx_hex())).expect("secret");
            assert!(
                kdf_crypto::sha256(&secret).to_vec() == terms.secret_hash,
                "the inner claim gives the secret"
            );
            assert!(
                coin.extract_secret(&terms.secret_hash, &found.tx_hex()).is_err(),
                "the bytes alone show no claim"
            );
        },
        _ => panic!("an inner claim is found"),
    }
}

#[test]
fn t40_6d_9_a_failed_read_is_undetermined() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    world
        .lock()
        .expect("world")
        .fail
        .insert("getSignaturesForAddress".to_owned());
    let (coin, _a, _b) = two_agreeing(&world);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).is_err(),
        "a failed read is neither spent nor not spent"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.12: a repeated payment send
// ---------------------------------------------------------------------------------------------------------------

fn send_payment(coin: &SolanaCoin, terms: &SolTerms) -> Result<TransactionEnum, TransactionErr> {
    block_on(
        coin.send_maker_payment(
            terms.lock_time as u32,
            &field(&coin.key_pair.pubkey()),
            &field(&terms.receiver),
            &terms.secret_hash,
            lamports(terms.amount),
            &None,
        )
        .compat(),
    )
}

#[test]
fn t40_6d_12_a_payment_is_sent_once_and_a_failed_read_sends_nothing() {
    let (coin, stub, world) = one_node_coin();
    let mut terms = captured_terms(PAYMENT_1);
    terms.sender = coin.key_pair.pubkey();

    // A failed read before the send: nothing is sent.
    world
        .lock()
        .expect("world")
        .fail
        .insert("getSignaturesForAddress".to_owned());
    assert!(send_payment(&coin, &terms).is_err(), "an undetermined read is an error");
    assert!(stub.count("sendTransaction") == 0, "nothing was sent on a failed read");
    world.lock().expect("world").fail.clear();

    // An empty history: one payment is sent, with the program's instruction.
    let sent = send_payment(&coin, &terms).expect("sent");
    assert!(stub.count("sendTransaction") == 1, "one transaction was sent");
    let TransactionEnum::SolanaTransaction(sent) = sent else {
        panic!("a Solana transaction")
    };
    let instructions = top_level_instructions(&sent.tx_hex()).expect("decodes");
    assert!(
        instructions
            .iter()
            .any(|ix| ix.program == program() && ix.data == terms.payment_instruction(&program()).data),
        "the payment instruction is in the transaction"
    );

    // The payment is now on the chain: a second send with the same terms returns it and pays nothing.
    let (address, _) = terms.payment_address(&program());
    {
        let mut w = world.lock().expect("world");
        // Hand-built answer for the transaction this coin sent: no captured transaction has the coin's key.
        w.transactions.insert(
            sent.signature().to_owned(),
            json!({ "slot": 1, "blockTime": 1, "transaction": [base64::encode(&sent.tx_hex()), "base64"],
                "meta": { "err": null, "fee": 5000, "preBalances": [], "postBalances": [] } }),
        );
        w.signatures
            .insert(address.to_string(), vec![sig_entry(sent.signature(), false)]);
    }
    let again = send_payment(&coin, &terms).expect("found");
    assert!(
        stub.count("sendTransaction") == 1,
        "the repeated send paid nothing more"
    );
    assert!(
        again.tx_hash() == TransactionEnum::SolanaTransaction(sent).tx_hash(),
        "the existing payment is returned"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.10: the dex fee
// ---------------------------------------------------------------------------------------------------------------

const UUID: [u8; 16] = [
    0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
];
const UUID_TEXT: &str = "12345678-9abc-def0-1122-334455667788";
const FEE_ADDRESS: [u8; 32] = [0x77; 32];

fn fee_of(amount: &str) -> DexFee { DexFee::Standard(MmNumber::from(BigDecimal::from_str(amount).expect("a decimal"))) }

#[test]
fn t40_6d_10_the_fee_is_a_transfer_with_the_uuid_memo() {
    let (coin, stub, _) = one_node_coin();
    let tx = block_on(coin.send_taker_fee(&fee_of("0.0078"), &FEE_ADDRESS, &UUID).compat()).expect("sent");
    assert!(stub.count("sendTransaction") == 1, "one transaction was sent");
    let TransactionEnum::SolanaTransaction(tx) = tx else {
        panic!("a Solana transaction")
    };
    let instructions = top_level_instructions(&tx.tx_hex()).expect("decodes");
    let memo = instructions
        .iter()
        .find(|ix| ix.program == MEMO_PROGRAM_ID)
        .expect("a memo");
    assert!(
        memo.accounts.is_empty() && memo.data == UUID_TEXT.as_bytes(),
        "the memo is the uuid's hyphenated text"
    );
    let transfer = instructions
        .iter()
        .find(|ix| ix.program == solana_sdk_ids::system_program::ID)
        .expect("a transfer");
    assert!(
        transfer.accounts == vec![coin.key_pair.pubkey(), Pubkey::new_from_array(FEE_ADDRESS)],
        "from the taker's key to the fee address"
    );
    assert!(system_transfer(&transfer.data) == 7_800_000, "the fee in lamports");
    assert!(
        block_on(coin.send_taker_fee(&DexFee::NoFee, &FEE_ADDRESS, &UUID).compat()).is_err(),
        "only the standard fee form is sent"
    );
}

fn system_transfer(data: &[u8]) -> u64 {
    match bincode::deserialize::<solana_system_interface::instruction::SystemInstruction>(data)
        .expect("a system instruction")
    {
        solana_system_interface::instruction::SystemInstruction::Transfer { lamports } => lamports,
        _ => panic!("a transfer"),
    }
}

/// Hand-built: a signed fee transaction of `payer` (no captured dex-fee transaction exists), served as the answer
/// of the node for its signature.
fn fee_answer(payer: &Keypair, to: &Pubkey, lamports: u64, memo: &str, failed: bool) -> (String, Json) {
    let transfer = solana_system_interface::instruction::transfer(&payer.pubkey(), to, lamports);
    let message = Message::new(&[transfer, memo_instruction(memo)], Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.try_sign(&[payer], solana_hash::Hash::default()).expect("signs");
    let wire = bincode::serialize(&tx).expect("serializes");
    let signature = tx.signatures[0].to_string();
    let answer = json!({
        "slot": 5, "blockTime": 1,
        "transaction": [base64::encode(&wire), "base64"],
        "meta": { "err": if failed { json!({"InstructionError": [0, "Custom"]}) } else { Json::Null }, "fee": 5000,
            "preBalances": vec![0u64; tx.message.account_keys.len()], "postBalances": vec![0u64; tx.message.account_keys.len()] }
    });
    (signature, answer)
}

fn validate_fee_of(
    coin: &SolanaCoin,
    world: &Arc<Mutex<World>>,
    answer: (String, Json),
    min_block: u64,
    memo_uuid: &[u8],
) -> Result<(), String> {
    let (signature, answer) = answer;
    world
        .lock()
        .expect("world")
        .transactions
        .insert(signature.clone(), answer.clone());
    let wire = base64::decode(answer["transaction"][0].as_str().expect("wire")).expect("base64");
    let tx = TransactionEnum::from(SolanaSwapTx::from_wire(&wire).expect("a transaction"));
    let dex_fee = fee_of("0.0078");
    block_on(
        coin.validate_fee(ValidateFeeArgs {
            fee_tx: &tx,
            expected_sender: &swap_key_field(&generate_key_pair_from_iguana_seed("a taker".to_owned()).pubkey()),
            fee_addr: &FEE_ADDRESS,
            dex_fee: &dex_fee,
            min_block_number: min_block,
            uuid: memo_uuid,
        })
        .compat(),
    )
}

#[test]
fn t40_6d_10_the_fee_validation_checks_amount_sender_destination_memo_and_height() {
    let (coin, _stub, world) = one_node_coin();
    let payer = generate_key_pair_from_iguana_seed("a taker".to_owned());
    let to = Pubkey::new_from_array(FEE_ADDRESS);
    let good = || fee_answer(&payer, &to, 7_800_000, UUID_TEXT, false);
    assert!(
        validate_fee_of(&coin, &world, good(), 1_000_000, &UUID).is_ok(),
        "a valid fee (at the start height)"
    );
    assert!(
        validate_fee_of(
            &coin,
            &world,
            fee_answer(&payer, &to, 7_799_999, UUID_TEXT, false),
            1_000_000,
            &UUID
        )
        .is_err(),
        "a fee below the expected amount is refused"
    );
    let other = generate_key_pair_from_iguana_seed("another payer".to_owned());
    assert!(
        validate_fee_of(
            &coin,
            &world,
            fee_answer(&other, &to, 7_800_000, UUID_TEXT, false),
            1_000_000,
            &UUID
        )
        .is_err(),
        "a fee paid by another key is refused"
    );
    assert!(
        validate_fee_of(
            &coin,
            &world,
            fee_answer(&payer, &Pubkey::new_from_array([5u8; 32]), 7_800_000, UUID_TEXT, false),
            1_000_000,
            &UUID
        )
        .is_err(),
        "a fee to another destination is refused"
    );
    assert!(
        validate_fee_of(
            &coin,
            &world,
            fee_answer(&payer, &to, 7_800_000, "00000000-0000-0000-0000-000000000000", false),
            1_000_000,
            &UUID
        )
        .is_err(),
        "the fee of another swap (other memo) is refused"
    );
    assert!(
        validate_fee_of(&coin, &world, good(), 1_000_001, &UUID).is_err(),
        "a transfer from before the swap's start is refused"
    );
    assert!(
        validate_fee_of(
            &coin,
            &world,
            fee_answer(&payer, &to, 7_800_000, UUID_TEXT, true),
            1_000_000,
            &UUID
        )
        .is_err(),
        "a failed fee transaction is refused"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.11: trade fees and admission
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_11_the_fee_answers_and_the_admission_figures() {
    let (coin, _stub, _) = one_node_coin();
    let sol = |lamports: u64| MmNumber::from(lamports_to_sol(lamports));
    let deposit = rent(PAYMENT_ACCOUNT_SPACE);
    let sender =
        block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::WithoutApprox))
            .expect("a fee");
    assert!(
        sender.coin == "SOL" && sender.amount == sol(5000 + deposit),
        "the payment's fee plus the rent it deposits"
    );
    assert!(!sender.paid_from_trading_vol, "not paid from the trading volume");
    let receiver = block_on(coin.get_receiver_trade_fee(FeeApproxStage::WithoutApprox).compat()).expect("a fee");
    assert!(receiver.amount == sol(5000), "the claim's fee");
    let taker = block_on(coin.get_fee_to_send_taker_fee(1.into(), FeeApproxStage::WithoutApprox)).expect("a fee");
    assert!(
        taker.amount == sol(5000),
        "the fee transaction's fee (no token account for SOL)"
    );
    let per_swap = block_on(coin.get_trade_fee().compat()).expect("a fee");
    assert!(per_swap.amount == sol(5000 + deposit), "the fee of one swap payment");

    let admission = |side| block_on(coin.swap_admission_lamports(side)).expect("a figure");
    assert!(
        admission(SwapSide::Sender { taker_fee: false }) == 5000 + deposit + 5000,
        "payment fee, rent and one refund"
    );
    assert!(
        admission(SwapSide::Sender { taker_fee: true }) == 5000 + deposit + 5000 + 5000,
        "the taker's figure adds the fee transaction's fee"
    );
    assert!(
        admission(SwapSide::Receiver) == 5000,
        "the receiver needs one claim's fee"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T40.6D.13: the evidence
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_13_two_agreeing_nodes_a_disagreement_and_one_answer() {
    let terms = captured_terms(PAYMENT_1);
    let amount = terms.amount + rent(PAYMENT_ACCOUNT_SPACE);

    // Two agreeing nodes.
    let world_a = fixture_world();
    paid(&world_a, PAYMENT_ADDRESS_1, amount);
    let (coin, _a, _b) = two_node_coin(world_a.clone(), world_a.clone());
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_ok(),
        "two agreeing nodes establish the payment"
    );

    // A disagreement: one node's account is short. The read is undetermined, so the validation never says valid.
    let world_b = fixture_world();
    paid(&world_b, PAYMENT_ADDRESS_1, amount - 1);
    let (coin, _a, _b) = two_node_coin(world_a.clone(), world_b);
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "a disagreement is undetermined"
    );

    // One answer: a single-node pool, and a pool whose second node fails.
    let (coin, _stub, world) = one_node_coin();
    paid(&world, PAYMENT_ADDRESS_1, amount);
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "fewer than two answers is undetermined"
    );
    let broken = fixture_world();
    broken.lock().expect("world").fail.insert("getTransaction".to_owned());
    let (coin, _a, _b) = two_node_coin(world_a, broken);
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "one node answering is undetermined"
    );

    // The same rule governs the refund gate and the spend search.
    let (coin, _stub, _) = one_node_coin();
    assert!(
        matches!(gate(&coin, 1), CanRefundHtlc::HaveToWait(_)),
        "the gate waits on one answer"
    );
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![sig_entry(CLAIM_1, false)]);
    assert!(
        block_on(coin.search_spend(&program(), &terms, PAYMENT_1, None)).is_err(),
        "the search is undetermined on one answer"
    );
}

#[test]
fn t40_6d_13_the_named_evidence_node_alone_and_no_fallback() {
    let terms = captured_terms(PAYMENT_1);
    let amount = terms.amount + rent(PAYMENT_ACCOUNT_SPACE);
    let world_a = fixture_world();
    paid(&world_a, PAYMENT_ADDRESS_1, amount);
    let world_b = fixture_world();
    paid(&world_b, PAYMENT_ADDRESS_1, amount - 1);
    let a = node_over(world_a.clone());
    let b = node_over(world_b);
    let coin = coin_over(vec![a.url.clone(), b.url.clone()]);

    // The named node is A: its answer alone is the evidence, B is not read at all.
    coin.set_swap_evidence_node(Some(a.url.clone()));
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_ok(),
        "the named node alone is evidence"
    );
    assert!(
        b.count("getTransaction") == 0 && b.count("getAccountInfo") == 0,
        "the other node is not read"
    );

    // The named node is B, whose chain is short: undetermined or invalid, never valid.
    coin.set_swap_evidence_node(Some(b.url.clone()));
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "the named node decides, and it says short"
    );

    // The named node does not answer: no other node takes its place.
    world_a.lock().expect("world").fail.insert("getTransaction".to_owned());
    coin.set_swap_evidence_node(Some(a.url.clone()));
    let before = b.count("getTransaction");
    assert!(
        validate(&coin, PAYMENT_1, &terms, terms.amount).is_err(),
        "an unanswering named node is undetermined"
    );
    assert!(
        b.count("getTransaction") == before,
        "no other node of the pool is taken in its place"
    );
}

#[test]
fn t40_6d_13_the_evidence_node_must_be_one_of_the_coins_nodes() {
    let request = |node: Option<&str>| {
        let mut request = json!({ "confirmation_commitment": "finalized", "client_urls": ["http://127.0.0.1:1", "http://127.0.0.1:2"] });
        if let Some(node) = node {
            request["swap_evidence_node"] = json!(node);
        }
        serde_json::from_value::<SolanaActivationParams>(request).expect("parses")
    };
    let build = |params| {
        block_on(solana_coin_with_node_order(
            "SOL",
            &json!({}),
            params,
            &[7u8; 32],
            crate::solana::rpc_pool::SolanaNodeOrder::Listed,
        ))
    };
    let coin = build(request(Some("http://127.0.0.1:2"))).expect("a node of the list is accepted");
    assert!(
        coin.evidence_node().as_deref() == Some("http://127.0.0.1:2"),
        "the node is recorded"
    );
    assert!(
        build(request(None)).expect("no node").evidence_node().is_none(),
        "no evidence node by default"
    );
    let refused = build(request(Some("http://127.0.0.1:3")));
    assert!(
        matches!(refused, Err(ref text) if text == &SwapEvidenceNodeError::NotInNodeList.to_string()),
        "a URL that is not in the node list is refused"
    );
    assert!(
        format!("{:?}", request(Some("http://user:secret@127.0.0.1:2/path"))).contains("127.0.0.1")
            && !format!("{:?}", request(Some("http://user:secret@127.0.0.1:2/path"))).contains("secret"),
        "the debug form names the node host-only"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Confirmations, the transaction type and the spend wait
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t40_6d_8_a_transaction_counts_when_included_successful_and_finalized() {
    let (coin, _stub, world) = one_node_coin();
    let wire = wire_of(PAYMENT_1);
    let in_a_minute = now_ms() / 1000 + 60;
    assert!(
        block_on(coin.wait_for_confirmations(&wire, 1, false, in_a_minute, 1).compat()).is_ok(),
        "a final success counts"
    );
    let saved = world.lock().expect("world").transactions[PAYMENT_1].clone();
    let mut failed = saved.clone();
    failed["meta"]["err"] = json!({ "InstructionError": [0, "Custom"] });
    world
        .lock()
        .expect("world")
        .transactions
        .insert(PAYMENT_1.to_owned(), failed);
    assert!(
        block_on(coin.wait_for_confirmations(&wire, 1, false, in_a_minute, 1).compat()).is_err(),
        "an error does not count"
    );
    world.lock().expect("world").transactions.remove(PAYMENT_1);
    let started = now_ms() / 1000;
    assert!(
        block_on(coin.wait_for_confirmations(&wire, 1, false, started, 1).compat()).is_err(),
        "not final: the wait ends"
    );
}

#[test]
fn t40_6d_9_the_transaction_type_round_trips_and_the_spend_wait_returns_the_claim() {
    let (coin, _stub, world) = one_node_coin();
    let wire = wire_of(PAYMENT_1);
    let tx = coin.tx_enum_from_bytes(&wire).expect("decodes");
    assert!(tx.tx_hex() == wire, "the wire bytes are kept");
    assert!(
        tx.tx_hash() == PAYMENT_1.as_bytes().to_vec().into(),
        "the hash is the base58 first signature"
    );
    assert!(
        coin.tx_enum_from_bytes(&[1, 2, 3]).is_err(),
        "bytes that are no transaction are refused"
    );

    // The wait for the spend needs two agreeing nodes.
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![sig_entry(CLAIM_1, false)]);
    let second = node_over(world.clone());
    let a = node_over(world.clone());
    let coin = coin_over(vec![a.url.clone(), second.url.clone()]);
    let found = block_on(coin.wait_for_tx_spend(&wire, now_ms() / 1000 + 30, 0, &None).compat()).expect("the claim");
    assert!(found.tx_hex() == wire_of(CLAIM_1), "the claim transaction is returned");
}

/// A payment to this coin by the captured sender, as unsigned wire bytes (hand-built: the captured receiver's key is
/// not the coin's), with the captured claim's secret.
fn claim_fixture(coin: &SolanaCoin) -> (SolTerms, Vec<u8>, [u8; 32]) {
    let me = coin.key_pair.pubkey();
    let captured = captured_terms(PAYMENT_1);
    let terms = SolTerms {
        sender: captured.sender,
        receiver: me,
        secret_hash: captured.secret_hash,
        lock_time: 1_795_021_693,
        amount: 1_000_000,
    };
    let payment_tx = Transaction::new_unsigned(Message::new(
        &[terms.payment_instruction(&program())],
        Some(&terms.sender),
    ));
    let wire = bincode::serialize(&payment_tx).expect("serializes");
    let (_, claim_data) = captured_instruction(CLAIM_1);
    let secret: [u8; 32] = claim_data[8..40].try_into().expect("a secret");
    (terms, wire, secret)
}

fn claim_world(coin_terms: &SolTerms) -> Arc<Mutex<World>> {
    let world = fixture_world();
    let (address, _) = coin_terms.payment_address(&program());
    paid(
        &world,
        &address.to_string(),
        coin_terms.amount + rent(PAYMENT_ACCOUNT_SPACE),
    );
    world
}

fn maker_claim(
    coin: &SolanaCoin,
    terms: &SolTerms,
    wire: &[u8],
    secret: &[u8; 32],
) -> Result<TransactionEnum, TransactionErr> {
    block_on(
        coin.send_maker_spends_taker_payment(wire, terms.lock_time as u32, &field(&terms.sender), secret, &[], &None)
            .compat(),
    )
}

fn taker_claim(
    coin: &SolanaCoin,
    terms: &SolTerms,
    wire: &[u8],
    secret: &[u8; 32],
) -> Result<TransactionEnum, TransactionErr> {
    block_on(
        coin.send_taker_spends_maker_payment(wire, terms.lock_time as u32, &field(&terms.sender), secret, &[], &None)
            .compat(),
    )
}

fn error_text(result: Result<TransactionEnum, TransactionErr>) -> String {
    match result {
        Err(e) => e.get_plain_text_format(),
        Ok(_) => panic!("an error"),
    }
}

#[test]
fn t40_6d_10_both_claims_are_sent_with_the_claim_instruction_when_all_is_well() {
    let (probe, _stub, _) = one_node_coin();
    let (terms, wire, secret) = claim_fixture(&probe);
    let world = claim_world(&terms);
    let (coin, a, _b) = two_agreeing(&world);
    for claim in [maker_claim, taker_claim] {
        let before = a.count("sendTransaction");
        let sent = claim(&coin, &terms, &wire, &secret).expect("sent");
        let TransactionEnum::SolanaTransaction(sent) = sent else {
            panic!("a Solana transaction")
        };
        assert!(
            top_level_instructions(&sent.tx_hex())
                .expect("decodes")
                .iter()
                .any(|ix| ix.data == terms.spend_instruction(&program(), &secret).data),
            "the claim instruction is sent"
        );
        assert!(a.count("sendTransaction") == before + 1, "one transaction was sent");
    }
    assert!(a.count("simulateTransaction") == 2, "each claim was simulated first");
    // A secret that does not hash to the payment's hash is not sent.
    let before = a.count("sendTransaction");
    assert!(
        maker_claim(&coin, &terms, &wire, &[3u8; 32]).is_err(),
        "a wrong secret is refused"
    );
    assert!(a.count("sendTransaction") == before, "nothing was sent");
}

#[test]
fn t40_6d_10_the_makers_claim_is_not_sent_without_its_three_conditions() {
    let (probe, _stub, _) = one_node_coin();
    let (terms, wire, secret) = claim_fixture(&probe);

    // (a) The account is gone.
    let world = claim_world(&terms);
    let (address, _) = terms.payment_address(&program());
    world.lock().expect("world").accounts.remove(&address.to_string());
    let (coin, a, _b) = two_agreeing(&world);
    let text = error_text(maker_claim(&coin, &terms, &wire, &secret));
    assert!(text.contains("(a)"), "the error names condition (a)");
    assert!(
        a.count("sendTransaction") == 0,
        "nothing was sent with the account gone"
    );

    // (a) The read is undetermined (one node only): the maker's claim is held back.
    let world = claim_world(&terms);
    let node = node_over(world.clone());
    let one = coin_over(vec![node.url.clone()]);
    assert!(
        error_text(maker_claim(&one, &terms, &wire, &secret)).contains("(a)"),
        "an undetermined read holds it back"
    );
    assert!(node.count("sendTransaction") == 0, "nothing was sent");

    // (b) The simulation fails.
    let world = claim_world(&terms);
    world.lock().expect("world").simulate_err = Some(json!({ "InstructionError": [0, "Custom"] }));
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        error_text(maker_claim(&coin, &terms, &wire, &secret)).contains("(b)"),
        "the error names condition (b)"
    );
    assert!(
        a.count("simulateTransaction") == 1 && a.count("sendTransaction") == 0,
        "simulated, not sent"
    );

    // (c) Fewer than 300 seconds to the lock: 299 refuses, 300 sends.
    let world = claim_world(&terms);
    world.lock().expect("world").block_time = Some(terms.lock_time as i64 - 299);
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        error_text(maker_claim(&coin, &terms, &wire, &secret)).contains("(c)"),
        "the error names condition (c)"
    );
    assert!(a.count("sendTransaction") == 0, "nothing was sent under 300 seconds");
    world.lock().expect("world").block_time = Some(terms.lock_time as i64 - 300);
    assert!(
        maker_claim(&coin, &terms, &wire, &secret).is_ok(),
        "300 seconds before the lock is enough"
    );
    world.lock().expect("world").block_time = Some(terms.lock_time as i64 + 1);
    assert!(
        maker_claim(&coin, &terms, &wire, &secret).is_err(),
        "after the lock the maker's claim is not sent"
    );
}

/// A world whose finalized block time is `time` and whose payment account of `terms` exists.
fn timed_claim_world(terms: &SolTerms, time: i64) -> Arc<Mutex<World>> {
    let world = claim_world(terms);
    world.lock().expect("world").block_time = Some(time);
    world
}

#[test]
fn t40_6d_10_the_claim_margin_uses_the_later_reading_and_the_gate_the_earlier() {
    let (probe, _stub, _) = one_node_coin();
    let (terms, wire, secret) = claim_fixture(&probe);
    let t = 1_795_000_000i64;
    // Two nodes read T and T + 10; the lock is T + 305: the later reading leaves 295 s.
    let lock = (t + 305) as u64;
    let terms = SolTerms {
        lock_time: lock,
        ..terms
    };
    let payment_tx = Transaction::new_unsigned(Message::new(
        &[terms.payment_instruction(&program())],
        Some(&terms.sender),
    ));
    let wire = {
        let _ = wire;
        bincode::serialize(&payment_tx).expect("serializes")
    };
    let (coin, a, _b) = two_node_coin(timed_claim_world(&terms, t), timed_claim_world(&terms, t + 10));
    assert!(
        error_text(maker_claim(&coin, &terms, &wire, &secret)).contains("(c)"),
        "the later reading leaves 295 s, so condition (c) refuses"
    );
    assert!(a.count("sendTransaction") == 0, "nothing was sent");
    // The refund gate waits from the earlier reading T.
    assert!(
        matches!(gate(&coin, lock), CanRefundHtlc::HaveToWait(305)),
        "the gate's waiting time is computed from T"
    );
    // With the lock one second later both readings leave 300 s or more.
    let terms = SolTerms {
        lock_time: lock + 5,
        ..terms
    };
    let payment_tx = Transaction::new_unsigned(Message::new(
        &[terms.payment_instruction(&program())],
        Some(&terms.sender),
    ));
    let wire = bincode::serialize(&payment_tx).expect("serializes");
    let (coin, _a, _b) = two_node_coin(timed_claim_world(&terms, t), timed_claim_world(&terms, t + 10));
    assert!(
        maker_claim(&coin, &terms, &wire, &secret).is_ok(),
        "300 s from the later reading is enough"
    );
}

#[test]
fn t40_6d_10_the_takers_claim_goes_out_unless_the_account_is_known_to_be_gone() {
    let (probe, _stub, _) = one_node_coin();
    let (terms, wire, secret) = claim_fixture(&probe);

    // After the lock time, with the account present: sent.
    let world = claim_world(&terms);
    world.lock().expect("world").block_time = Some(terms.lock_time as i64 + 10_000);
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        taker_claim(&coin, &terms, &wire, &secret).is_ok(),
        "no 300 second condition applies"
    );
    assert!(a.count("sendTransaction") == 1, "one transaction was sent");

    // An undetermined read (one node only) does not hold it back.
    let world = claim_world(&terms);
    let node = node_over(world.clone());
    let one = coin_over(vec![node.url.clone()]);
    assert!(
        taker_claim(&one, &terms, &wire, &secret).is_ok(),
        "an undetermined read still lets the claim go out"
    );
    assert!(node.count("sendTransaction") == 1, "one transaction was sent");

    // A disagreement (one node has the account, the other does not) does not hold it back either.
    let present = claim_world(&terms);
    let gone = fixture_world();
    let (coin, a, _b) = two_node_coin(present, gone);
    assert!(
        taker_claim(&coin, &terms, &wire, &secret).is_ok(),
        "a disagreement still lets the claim go out"
    );
    assert!(a.count("sendTransaction") == 1, "one transaction was sent");

    // Known to be gone (both nodes): nothing is sent.
    let world = fixture_world();
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        error_text(taker_claim(&coin, &terms, &wire, &secret)).contains("gone"),
        "the error says the account is gone"
    );
    assert!(
        a.count("sendTransaction") == 0,
        "nothing was sent with the account gone"
    );
}

#[test]
fn t40_6d_10_the_takers_claim_is_retried_after_a_failed_simulation() {
    let (probe, _stub, _) = one_node_coin();
    let (terms, wire, secret) = claim_fixture(&probe);
    let world = claim_world(&terms);
    world.lock().expect("world").simulate_err = Some(json!({ "InstructionError": [0, "Custom"] }));
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        taker_claim(&coin, &terms, &wire, &secret).is_err(),
        "a claim that never simulates is not sent"
    );
    assert!(a.count("simulateTransaction") > 1, "it was retried");
    assert!(a.count("sendTransaction") == 0, "nothing was sent");
}

#[test]
fn t40_6d_5_a_refund_is_built_from_the_payment_and_signed_by_the_coin() {
    let (coin, _stub, _) = one_node_coin();
    let me = coin.key_pair.pubkey();
    let terms = SolTerms {
        sender: me,
        receiver: captured_terms(PAYMENT_1).receiver,
        secret_hash: captured_terms(PAYMENT_1).secret_hash,
        lock_time: 1_795_021_693,
        amount: 1_000_000,
    };
    let own = Transaction::new_unsigned(Message::new(&[terms.payment_instruction(&program())], Some(&me)));
    let refunded = block_on(
        coin.send_maker_refunds_payment(
            &bincode::serialize(&own).expect("serializes"),
            terms.lock_time as u32,
            &field(&terms.receiver),
            &terms.secret_hash,
            &[],
            &None,
        )
        .compat(),
    )
    .expect("sent");
    let TransactionEnum::SolanaTransaction(refunded) = refunded else {
        panic!("a Solana transaction")
    };
    assert!(
        top_level_instructions(&refunded.tx_hex())
            .expect("decodes")
            .iter()
            .any(|ix| ix.data == terms.refund_instruction(&program()).data),
        "the refund instruction is sent"
    );
}

#[test]
fn t40_6d_5_a_counterparty_key_equal_to_the_coins_own_is_refused() {
    let (coin, stub, _) = one_node_coin();
    let own = field(&coin.key_pair.pubkey());
    assert!(
        matches!(
            coin.validate_other_pubkey(&own).map_err(|e| e.into_inner()),
            Err(HtlcPubkeyError::NotOnCurve(..))
        ),
        "the negotiation refuses the own key"
    );
    let result = block_on(
        coin.send_maker_payment(1_795_000_000, &own, &own, &[7u8; 32], lamports(1_000_000), &None)
            .compat(),
    );
    assert!(result.is_err(), "a payment to the own key is not sent");
    let (terms, wire, secret) = claim_fixture(&coin);
    let own_sender = SolTerms {
        sender: coin.key_pair.pubkey(),
        ..terms
    };
    assert!(
        maker_claim(&coin, &own_sender, &wire, &secret).is_err(),
        "a claim of an own-key payment is refused"
    );
    assert!(stub.count("sendTransaction") == 0, "nothing was sent");
}

// ---------------------------------------------------------------------------------------------------------------
// R40.6D.12: the bounded spend search
// ---------------------------------------------------------------------------------------------------------------

/// Hand-built listing entries of transactions the fixtures do not hold, at `slot`.
fn unrelated(count: usize, slot: u64, tag: &str) -> Vec<Json> {
    (0..count)
        .map(|i| sig_entry_at(&format!("{tag}{i}"), slot + i as u64, false))
        .collect()
}

#[test]
fn t40_6d_9_a_listing_with_many_unrelated_entries_still_gives_the_secret_and_only_the_range_is_fetched() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    // Hand-built entries around the captured claim: 30 newer than the account-gone slot and 30 older than the
    // payment's slot. The payment's own entry ends the listing.
    let mut list = unrelated(30, FINALIZED_SLOT + 10, "newer");
    list.push(sig_entry(CLAIM_1, false));
    list.extend(unrelated(30, 1_000, "older"));
    list.push(sig_entry(PAYMENT_1, false));
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), list);
    let (coin, a, b) = two_agreeing(&world);
    match spend_search(&coin, PAYMENT_1, &terms).expect("determined") {
        Some(FoundSwapTxSpend::Spent(tx)) => assert!(tx.tx_hex() == wire_of(CLAIM_1), "the claim is found"),
        _ => panic!("a claim is found"),
    }
    for node in [&a, &b] {
        let fetched: HashSet<String> = node
            .params_of("getTransaction")
            .iter()
            .map(|p| p[0].as_str().expect("a signature").to_owned())
            .collect();
        assert!(
            fetched == HashSet::from([PAYMENT_1.to_owned(), CLAIM_1.to_owned()]),
            "only the payment (its slot is the lower bound) and the in-range claim are fetched"
        );
        assert!(
            node.params_of("getSignaturesForAddress")
                .iter()
                .all(|p| p[1]["until"] == PAYMENT_1),
            "the listing ends at the payment's own signature"
        );
    }
}

#[test]
fn t40_6d_9_a_node_serving_short_pages_is_read_to_its_empty_page() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    // Three entries out of range, two per page: pages of 2, 1 and then the empty one. No claim is in the listing.
    let list = unrelated(3, FINALIZED_SLOT + 10, "newer");
    let last = list[2]["signature"].as_str().expect("a signature").to_owned();
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), list);
    world.lock().expect("world").page_cap = 2;
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        spend_search(&coin, PAYMENT_1, &terms).expect("determined").is_none(),
        "no claim is in the listing"
    );
    let calls = a.params_of("getSignaturesForAddress");
    assert!(calls.len() == 3, "the short page of one entry did not end the listing");
    assert!(
        calls[2][1]["before"] == last.as_str(),
        "the last call asked for what follows the last entry"
    );
}

#[test]
fn t40_6d_9_the_wait_keeps_the_slot_the_account_was_seen_at_as_the_lower_bound() {
    let world = fixture_world();
    let terms = captured_terms(PAYMENT_1);
    let wire = wire_of(PAYMENT_1);
    // The account exists, so no history is read and no transaction is fetched.
    paid(&world, PAYMENT_ADDRESS_1, terms.amount + rent(PAYMENT_ACCOUNT_SPACE));
    let (coin, a, _b) = two_agreeing(&world);
    assert!(
        block_on(coin.wait_for_tx_spend(&wire, now_ms() / 1000, 0, &None).compat()).is_err(),
        "an unspent payment ends the wait at its deadline"
    );
    assert!(
        a.count("getSignaturesForAddress") == 0 && a.count("getTransaction") == 0,
        "nothing is listed or fetched"
    );
    // The account goes; the claim is found in [seen, gone] and the payment's own transaction is not fetched.
    world.lock().expect("world").accounts.remove(PAYMENT_ADDRESS_1);
    world.lock().expect("world").context_slot = FINALIZED_SLOT + 50;
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_1.to_owned(), vec![
            sig_entry(CLAIM_1, false),
            sig_entry(PAYMENT_1, false),
        ]);
    let found = block_on(coin.wait_for_tx_spend(&wire, now_ms() / 1000 + 30, 0, &None).compat()).expect("the claim");
    assert!(found.tx_hex() == wire_of(CLAIM_1), "the claim transaction is returned");
}

#[test]
fn t40_6d_9_the_wait_answers_with_a_refund_when_the_payment_was_refunded() {
    let world = fixture_world();
    world
        .lock()
        .expect("world")
        .signatures
        .insert(PAYMENT_ADDRESS_2.to_owned(), vec![
            sig_entry(REFUND_2, false),
            sig_entry(PAYMENT_2, false),
        ]);
    let (coin, _a, _b) = two_agreeing(&world);
    let found = block_on(
        coin.wait_for_tx_spend(&wire_of(PAYMENT_2), now_ms() / 1000 + 30, 0, &None)
            .compat(),
    )
    .expect("the refund");
    assert!(
        found.tx_hex() == wire_of(REFUND_2),
        "the spend that is a refund is returned, as the other coins do"
    );
    assert!(
        coin.extract_secret(&captured_terms(PAYMENT_2).secret_hash, &found.tx_hex())
            .is_err(),
        "it carries no secret, so the taker's extraction ends the swap"
    );
}
