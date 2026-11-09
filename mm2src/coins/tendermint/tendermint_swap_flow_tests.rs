//! Tests of the Tendermint swap flow (CRD ch.18 §18.3, §18.5 and §18.11).
//!
//! The tests record what the swap code of `TendermintCoin` does: the HTLC create and claim messages, the HTLC id,
//! the payment checks, the fee validation, the payment search, the spend search, the refund answers, the secret
//! extraction, the confirmation wait, the fee labels of a token and the order-time refusals. A test named
//! `*_today_*` records a behavior that differs from the chapter (it waits for a maintainer decision); its comment
//! names the chapter rule and the gap, so the later fix flips that one assertion. Every other test names the
//! rule it checks in a `// CRD ch.18 R18.11.x` comment.
//!
//! No network is used. The coin talks to a throwaway server on a loopback port that answers the few JSON-RPC
//! requests the code under test makes (`health`, `abci_query`, `tx_search`) and records them. The keys are fixed
//! throwaway keys; no address in this file holds funds.

use super::htlc::{ClaimHtlcMsg, ClaimHtlcProto, CreateHtlcMsg, CreateHtlcProto, HtlcType, QueryHtlcRequestProto};
use super::tendermint_types::{tendermint_protocol_info_supported, tendermint_wallet_only};
use super::{tendermint_coin_from_conf_and_request, tendermint_coin_from_conf_and_request_with_global_hd,
            CosmosTransaction, RpcNode, TendermintActivationPolicy, TendermintCoin, TendermintCoinImpl,
            TendermintConf, TendermintInitErrorKind, TendermintProtocolInfo, TendermintToken};
use crate::rpc_command::get_my_address::PathToAddress;
use crate::{DexFee, DexFeeBurnDestination, FeeApproxStage, FoundSwapTxSpend, MarketCoinOps, MmCoin, PrivKeyPolicy,
            SwapOps, TradePreimageValue, TransactionEnum, TransactionErr, ValidateFeeArgs, ValidatePaymentInput,
            WithdrawRequest};
use bigdecimal::BigDecimal;
use common::mm_number::MmNumber;
use cosmrs::crypto::secp256k1::SigningKey;
use cosmrs::proto::cosmos::auth::v1beta1::{BaseAccount, QueryAccountResponse};
use cosmrs::proto::cosmos::bank::v1beta1::{Input, MsgMultiSend, MsgSend, Output, QueryBalanceResponse};
use cosmrs::proto::cosmos::base::abci::v1beta1::{GasInfo, TxResponse};
use cosmrs::proto::cosmos::base::v1beta1::Coin as CoinProto;
use cosmrs::proto::cosmos::tx::v1beta1::{GetTxRequest, GetTxResponse, SignDoc as SignDocProto, SimulateResponse, Tx,
                                         TxRaw};
use cosmrs::tx::{self, Fee, SignDoc, SignerInfo};
use cosmrs::{AccountId, Any, Coin};
use crypto::privkey::key_pair_from_secret;
use crypto::{Bip44Chain, CryptoCtx, GlobalHDAccountArc, KeyPairPolicy};
use futures::compat::Future01CompatExt;
use kdf_crypto::dhash160;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use prost::Message;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OWN_KEY: [u8; 32] = [0x11; 32];
const PEER_KEY: [u8; 32] = [0x22; 32];
const THIRD_KEY: [u8; 32] = [0x33; 32];
const PREIMAGE: [u8; 32] = [0x44; 32];
/// The key of the burn account of the burn-form fee.
const BURN_KEY: [u8; 32] = [0x55; 32];

/// 1.5 coins at the 6 decimals of the test chains.
const AMOUNT_SAT: u64 = 1_500_000;
const AUTO_REFUND_ERROR: &str = "Tendermint HTLCs auto-refund on chain; no broadcast required.";
/// The DEX fee of the fee tests, 0.3 coins, and the burn part of the burn form, 0.1 coins, in base units.
const FEE_SAT: u64 = 300_000;
const BURN_SAT: u64 = 100_000;

// ---------------------------------------------------------------------------
// The scripted node
// ---------------------------------------------------------------------------

type Answer = dyn Fn(&str, &Json) -> Option<Json> + Send + Sync;
type SharedAnswer = Arc<Mutex<Arc<Answer>>>;
type Seen = Arc<Mutex<Vec<(String, Json)>>>;

/// A JSON-RPC node on a loopback port. `health` is always answered; any other method is answered by the
/// function given to [`FakeNode::answer`], and by a JSON-RPC error while there is none (or it returns `None`).
struct FakeNode {
    port: u16,
    stop: Arc<AtomicBool>,
    answer: SharedAnswer,
    seen: Seen,
}

impl FakeNode {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        listener.set_nonblocking(true).expect("a non-blocking listener");
        let port = listener.local_addr().expect("the local address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let initial: Arc<Answer> = Arc::new(|_: &str, _: &Json| None);
        let answer: SharedAnswer = Arc::new(Mutex::new(initial));
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));

        let (stop_t, answer_t, seen_t) = (stop.clone(), answer.clone(), seen.clone());
        std::thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (answer, seen) = (answer_t.clone(), seen_t.clone());
                        std::thread::spawn(move || serve_connection(stream, &answer, &seen));
                    },
                    Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                    Err(_) => break,
                }
            }
        });
        FakeNode {
            port,
            stop,
            answer,
            seen,
        }
    }

    fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }

    /// Replaces the function that answers the requests other than `health`.
    fn answer(&self, f: impl Fn(&str, &Json) -> Option<Json> + Send + Sync + 'static) {
        let new_answer: Arc<Answer> = Arc::new(f);
        *self.answer.lock().unwrap() = new_answer;
    }

    /// The parameters of every request of this method received so far, oldest first.
    fn requests(&self, method: &str) -> Vec<Json> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

impl Drop for FakeNode {
    fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
}

fn serve_connection(mut stream: TcpStream, answer: &Mutex<Arc<Answer>>, seen: &Mutex<Vec<(String, Json)>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let body = match read_body(&mut stream) {
        Some(body) => body,
        None => return,
    };
    let request: Json = serde_json::from_str(&body).unwrap_or(Json::Null);
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let params = request["params"].clone();
    seen.lock().unwrap().push((method.clone(), params.clone()));

    let current = answer.lock().unwrap().clone();
    let result = if method == "health" {
        Some(json!({}))
    } else {
        current(method.as_str(), &params)
    };
    let payload = match result {
        Some(result) => json!({ "jsonrpc": "2.0", "id": "fake-node", "result": result }),
        None => json!({
            "jsonrpc": "2.0",
            "id": "fake-node",
            "error": { "code": -32601, "message": "not served by the test node" }
        }),
    }
    .to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
}

/// Reads one HTTP request and returns its body (the head and the body may arrive in separate segments).
fn read_body(stream: &mut TcpStream) -> Option<String> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        received.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&received).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    let line = line.to_ascii_lowercase();
                    line.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if body.len() >= length {
                return Some(body.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The two chains and the coin
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Chain {
    Iris,
    Nucleus,
}

impl Chain {
    const BOTH: [Chain; 2] = [Chain::Iris, Chain::Nucleus];

    fn ticker(self) -> &'static str {
        match self {
            Chain::Iris => "IRIS",
            Chain::Nucleus => "NUCLEUS",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Chain::Iris => "iaa",
            Chain::Nucleus => "nuc",
        }
    }

    fn denom(self) -> &'static str {
        match self {
            Chain::Iris => "uiris",
            Chain::Nucleus => "unuc",
        }
    }

    fn chain_id(self) -> &'static str {
        match self {
            Chain::Iris => "irishub-1",
            Chain::Nucleus => "nucleus-1",
        }
    }

    fn htlc_type(self) -> HtlcType {
        match self {
            Chain::Iris => HtlcType::Iris,
            Chain::Nucleus => HtlcType::Nucleus,
        }
    }

    fn create_type_url(self) -> &'static str {
        match self {
            Chain::Iris => "/irismod.htlc.MsgCreateHTLC",
            Chain::Nucleus => "/nucleus.htlc.MsgCreateHTLC",
        }
    }

    fn claim_type_url(self) -> &'static str {
        match self {
            Chain::Iris => "/irismod.htlc.MsgClaimHTLC",
            Chain::Nucleus => "/nucleus.htlc.MsgClaimHTLC",
        }
    }

    fn query_path(self) -> &'static str {
        match self {
            Chain::Iris => "/irismod.htlc.Query/HTLC",
            Chain::Nucleus => "/nucleus.htlc.Query/HTLC",
        }
    }

    fn conf(self) -> Json {
        json!({
            "coin": self.ticker(),
            "avg_blocktime": 5,
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": self.denom(),
                "decimals": 6,
                "account_prefix": self.prefix(),
                "chain_id": self.chain_id(),
            } },
            "mm2": 1
        })
    }
}

struct Fixture {
    coin: TendermintCoin,
    node: FakeNode,
    ctx: MmArc,
}

/// A coin of the given chain, signing with the fixed own key, whose only node is the scripted one.
fn fixture(chain: Chain) -> Fixture {
    let node = FakeNode::start();
    // The netid selects the DEX fee address that the fee-estimation preimages pay to.
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }))
        .into_mm_arc();
    let coin = common::block_on(tendermint_coin_from_conf_and_request(
        &ctx,
        chain.ticker().to_owned(),
        &chain.conf(),
        vec![RpcNode::for_test(&node.url())],
        &OWN_KEY,
        false,
    ))
    .unwrap_or_else(|e| panic!("cannot build the test coin: {}", e));
    Fixture { coin, node, ctx }
}

fn pubkey(key: &[u8; 32]) -> Vec<u8> {
    key_pair_from_secret(key)
        .unwrap_or_else(|e| panic!("a valid test key: {}", e))
        .public_slice()
        .to_vec()
}

fn account(chain: Chain, pubkey: &[u8]) -> AccountId {
    AccountId::new(chain.prefix(), dhash160(pubkey).as_slice()).expect("a valid account id")
}

fn coin_of(chain: Chain, sat: u64) -> Coin {
    Coin {
        denom: chain.denom().parse().expect("a valid denom"),
        amount: sat.into(),
    }
}

fn secret_hash() -> Vec<u8> { Sha256::digest(&PREIMAGE).to_vec() }

fn sha256_upper(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(*part);
    }
    hex::encode_upper(hasher.finalize())
}

/// Signs a one-message transaction with the key of the coin. The swap code never checks the signature, only
/// that the bytes decode as a transaction.
fn signed_tx_bytes(coin: &TendermintCoin, chain: Chain, payload: Any) -> Vec<u8> {
    let account_info = BaseAccount {
        account_number: 1,
        sequence: 1,
        ..Default::default()
    };
    let fee = Fee::from_amount_and_gas(coin_of(chain, 5_000), 125_000u64);
    let key = coin
        .activation_policy
        .activated_key_or_err()
        .map_err(|e| e.to_string())
        .expect("the activated key");
    coin.any_to_signed_raw_tx(key, &account_info, payload, fee, 100, "")
        .expect("a signed transaction")
        .to_bytes()
        .expect("the encoded transaction")
}

/// Signs a transaction with the key of the coin for the given messages and memo.
fn signed_tx_with(coin: &TendermintCoin, chain: Chain, payloads: Vec<Any>, memo: &str) -> Vec<u8> {
    let signing_key = SigningKey::from_slice(&OWN_KEY).expect("a signing key");
    let body = tx::Body::new(payloads, memo, 100u32);
    let fee = Fee::from_amount_and_gas(coin_of(chain, 5_000), 125_000u64);
    let auth_info = SignerInfo::single_direct(Some(signing_key.public_key()), 1).auth_info(fee);
    let sign_doc = SignDoc::new(&body, &auth_info, &coin.protocol_info.chain_id, 1).expect("a sign doc");
    sign_doc
        .sign(&signing_key)
        .expect("a signed transaction")
        .to_bytes()
        .expect("the encoded transaction")
}

fn create_htlc_any(
    chain: Chain,
    sender: &AccountId,
    to: &AccountId,
    coin: Coin,
    hash_lock: String,
    time_lock: u64,
) -> Any {
    CreateHtlcMsg::new(
        chain.htlc_type(),
        sender.clone(),
        to.clone(),
        vec![coin],
        hash_lock,
        0,
        time_lock,
    )
    .to_any()
    .expect("a create-HTLC message")
}

struct CreateFields {
    sender: AccountId,
    to: AccountId,
    amount: Vec<Coin>,
    hash_lock: String,
    timestamp: u64,
    time_lock: u64,
    /// `receiver_on_other_chain`, `sender_on_other_chain` and `transfer`; the Nucleus form has none of them.
    iris_extras: Option<(String, String, bool)>,
}

fn create_fields(chain: Chain, any: &Any) -> CreateFields {
    let proto = CreateHtlcProto::decode(chain.htlc_type(), any.value.as_slice()).expect("a create-HTLC message");
    match CreateHtlcMsg::try_from(proto).expect("a valid create-HTLC message") {
        CreateHtlcMsg::Iris(m) => CreateFields {
            sender: m.sender,
            to: m.to,
            amount: m.amount,
            hash_lock: m.hash_lock,
            timestamp: m.timestamp,
            time_lock: m.time_lock,
            iris_extras: Some((m.receiver_on_other_chain, m.sender_on_other_chain, m.transfer)),
        },
        CreateHtlcMsg::Nucleus(m) => CreateFields {
            sender: m.sender,
            to: m.to,
            amount: m.amount,
            hash_lock: m.hash_lock,
            timestamp: m.timestamp,
            time_lock: m.time_lock,
            iris_extras: None,
        },
    }
}

struct ClaimFields {
    sender: AccountId,
    id: String,
    secret: String,
}

fn claim_fields(chain: Chain, any: &Any) -> ClaimFields {
    let proto = ClaimHtlcProto::decode(chain.htlc_type(), any.value.as_slice()).expect("a claim-HTLC message");
    match ClaimHtlcMsg::try_from(proto).expect("a valid claim-HTLC message") {
        ClaimHtlcMsg::Iris(m) => ClaimFields {
            sender: m.sender,
            id: m.id,
            secret: m.secret,
        },
        ClaimHtlcMsg::Nucleus(m) => ClaimFields {
            sender: m.sender,
            id: m.id,
            secret: m.secret,
        },
    }
}

/// The answer of `abci_query` that carries the given protobuf bytes.
fn abci_value(bytes: &[u8]) -> Json { json!({ "response": { "code": 0, "value": base64::encode(bytes) } }) }

/// The answer of `tx_search` for the given transactions, each with its ABCI result code.
fn search_reply(entries: &[(Vec<u8>, u32)]) -> Json {
    let txs: Vec<Json> = entries
        .iter()
        .enumerate()
        .map(|(index, (tx, code))| {
            json!({
                "hash": sha256_upper(&[tx.as_slice()]),
                "height": "10",
                "index": index,
                "tx_result": { "code": code },
                "tx": base64::encode(tx),
            })
        })
        .collect();
    let total = txs.len().to_string();
    json!({ "txs": txs, "total_count": total })
}

// The HTLC state as the chain's query answers it: the same wire shape as the chain modules (the field numbers
// are the protocol), declared here so that the tests do not depend on the code under test for the answers.
#[derive(prost::Message)]
struct IrisHtlcWire {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "7")]
    hash_lock: String,
    #[prost(int32, tag = "11")]
    state: i32,
}

#[derive(prost::Message)]
struct IrisHtlcAnswer {
    #[prost(message, optional, tag = "1")]
    htlc: Option<IrisHtlcWire>,
}

#[derive(prost::Message)]
struct NucleusHtlcWire {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "5")]
    hash_lock: String,
    #[prost(int32, tag = "9")]
    state: i32,
}

#[derive(prost::Message)]
struct NucleusHtlcAnswer {
    #[prost(message, optional, tag = "1")]
    htlc: Option<NucleusHtlcWire>,
}

const STATE_OPEN: i32 = 0;
const STATE_COMPLETED: i32 = 1;
const STATE_REFUNDED: i32 = 2;

fn htlc_answer(chain: Chain, id: &str, state: i32) -> Json {
    htlc_answer_with_lock(chain, id, state, hex::encode(secret_hash()))
}

/// The HTLC query answer for an HTLC that the module recorded with the given hash lock text.
fn htlc_answer_with_lock(chain: Chain, id: &str, state: i32, hash_lock: String) -> Json {
    let bytes = match chain {
        Chain::Iris => IrisHtlcAnswer {
            htlc: Some(IrisHtlcWire {
                id: id.to_owned(),
                hash_lock,
                state,
            }),
        }
        .encode_to_vec(),
        Chain::Nucleus => NucleusHtlcAnswer {
            htlc: Some(NucleusHtlcWire {
                id: id.to_owned(),
                hash_lock,
                state,
            }),
        }
        .encode_to_vec(),
    };
    abci_value(&bytes)
}

// ---------------------------------------------------------------------------
// Fixture self-check, HTLC messages and the HTLC id (§18.2.2, §18.3.2)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_coin_account_is_the_hash160_of_the_public_key() {
    for chain in Chain::BOTH {
        let fx = fixture(chain);
        assert_eq!(fx.coin.account_id, account(chain, &pubkey(&OWN_KEY)));
        assert!(fx.coin.account_id.to_string().starts_with(chain.prefix()));
    }
}

fn check_create_message(chain: Chain) {
    let fx = fixture(chain);
    let own = fx.coin.account_id.clone();
    let peer = account(chain, &pubkey(&PEER_KEY));
    let hash = secret_hash();
    let time_lock = fx.coin.estimate_blocks_from_duration(5_000) as u64;

    let htlc = fx
        .coin
        .gen_create_htlc_tx(
            fx.coin.protocol_info.denom.clone(),
            &peer,
            cosmrs::Amount::from(AMOUNT_SAT),
            &hash,
            time_lock,
        )
        .expect("a create-HTLC message");
    assert_eq!(htlc.msg_payload.type_url, chain.create_type_url());

    let fields = create_fields(chain, &htlc.msg_payload);
    assert_eq!(fields.sender, own);
    assert_eq!(fields.to, peer);
    assert_eq!(fields.amount.len(), 1);
    assert_eq!(fields.amount[0].denom.to_string(), chain.denom());
    assert_eq!(fields.amount[0].amount, cosmrs::Amount::from(AMOUNT_SAT));
    // The hash lock is the hex of the 32-byte secret hash, in lower case; the timestamp is 0.
    assert_eq!(fields.hash_lock, hex::encode(&hash));
    assert_eq!(fields.hash_lock.len(), 64);
    assert_eq!(fields.timestamp, 0);
    assert_eq!(fields.time_lock, 1_000);
    match chain {
        Chain::Iris => assert_eq!(fields.iris_extras, Some((String::new(), String::new(), false))),
        Chain::Nucleus => assert_eq!(fields.iris_extras, None),
    }

    // The id returned with the message is the one the chain stores the HTLC under.
    let expected_id = sha256_upper(&[
        hash.as_slice(),
        own.to_bytes().as_slice(),
        peer.to_bytes().as_slice(),
        format!("{}{}", AMOUNT_SAT, chain.denom()).as_bytes(),
    ]);
    assert_eq!(htlc.id, expected_id);
}

#[test]
fn tm_flow_create_htlc_message_fields_iris() { check_create_message(Chain::Iris); }

#[test]
fn tm_flow_create_htlc_message_fields_nucleus() { check_create_message(Chain::Nucleus); }

#[test]
fn tm_flow_time_lock_blocks_follow_the_duration_within_the_chain_bounds() {
    let fx = fixture(Chain::Iris);
    // The test chain has a 5 second block time: the duration divided by it, clamped to 50..=34560 blocks.
    assert_eq!(fx.coin.estimate_blocks_from_duration(5_000), 1_000);
    assert_eq!(fx.coin.estimate_blocks_from_duration(250), 50);
    assert_eq!(fx.coin.estimate_blocks_from_duration(10), 50);
    assert_eq!(fx.coin.estimate_blocks_from_duration(0), 50);
    assert_eq!(fx.coin.estimate_blocks_from_duration(172_800), 34_560);
    assert_eq!(fx.coin.estimate_blocks_from_duration(10_000_000), 34_560);
}

fn check_claim_message(chain: Chain) {
    let fx = fixture(chain);
    let id = "ABCD".repeat(16);
    let htlc = fx
        .coin
        .gen_claim_htlc_tx(id.clone(), &PREIMAGE)
        .expect("a claim-HTLC message");
    assert_eq!(htlc.id, id);
    assert_eq!(htlc.msg_payload.type_url, chain.claim_type_url());

    let fields = claim_fields(chain, &htlc.msg_payload);
    assert_eq!(fields.id, id);
    assert_eq!(fields.sender, fx.coin.account_id);
    // The secret travels as the hex of its bytes.
    assert_eq!(fields.secret, hex::encode(PREIMAGE));
}

#[test]
fn tm_flow_claim_htlc_message_fields_iris() { check_claim_message(Chain::Iris); }

#[test]
fn tm_flow_claim_htlc_message_fields_nucleus() { check_claim_message(Chain::Nucleus); }

#[test]
fn tm_flow_htlc_id_is_the_upper_case_sha256_of_hash_from_to_and_amount() {
    let fx = fixture(Chain::Iris);
    let from = account(Chain::Iris, &pubkey(&OWN_KEY));
    let to = account(Chain::Iris, &pubkey(&PEER_KEY));
    let hash = secret_hash();
    let expected = sha256_upper(&[
        hash.as_slice(),
        from.to_bytes().as_slice(),
        to.to_bytes().as_slice(),
        b"1500000uiris",
    ]);

    let id = fx
        .coin
        .calculate_htlc_id(&from, &to, &[coin_of(Chain::Iris, AMOUNT_SAT)], &hash);
    assert_eq!(id, expected);
    assert_eq!(id.len(), 64);
    assert_eq!(id, id.to_uppercase());

    // A different amount, receiver or hash gives a different id.
    let other_amount = fx
        .coin
        .calculate_htlc_id(&from, &to, &[coin_of(Chain::Iris, AMOUNT_SAT + 1)], &hash);
    assert_ne!(other_amount, id);
    let third = account(Chain::Iris, &pubkey(&THIRD_KEY));
    let other_receiver = fx
        .coin
        .calculate_htlc_id(&from, &third, &[coin_of(Chain::Iris, AMOUNT_SAT)], &hash);
    assert_ne!(other_receiver, id);
}

// ---------------------------------------------------------------------------
// validate_maker_payment / validate_taker_payment (§18.5)
// ---------------------------------------------------------------------------

fn payment_input(tx: Vec<u8>, taker_pub: Vec<u8>, maker_pub: Vec<u8>, amount: &str) -> ValidatePaymentInput {
    ValidatePaymentInput {
        payment_tx: tx,
        time_lock: (common::now_ms() / 1000) as u32 + 3_600,
        taker_pub,
        maker_pub,
        secret_hash: secret_hash(),
        amount: BigDecimal::from_str(amount).expect("a decimal amount"),
        swap_contract_address: None,
        try_spv_proof_until: 0,
        confirmations: 0,
    }
}

/// A payment from `sender` to `to`, as a transaction.
fn payment_tx(
    fx: &Fixture,
    chain: Chain,
    sender: &AccountId,
    to: &AccountId,
    coin: Coin,
    hash_lock: String,
    time_lock: u64,
) -> Vec<u8> {
    let any = create_htlc_any(chain, sender, to, coin, hash_lock, time_lock);
    signed_tx_bytes(&fx.coin, chain, any)
}

/// The maker (the peer) paid us: validates the transaction as the maker payment of a 1.5 coin swap.
fn validate_maker(fx: &Fixture, tx: Vec<u8>) -> Result<(), String> {
    let input = payment_input(tx, pubkey(&OWN_KEY), pubkey(&PEER_KEY), "1.5");
    common::block_on(fx.coin.validate_maker_payment(input).compat())
}

/// The taker (the peer) paid us: validates the transaction as the taker payment of a 1.5 coin swap.
fn validate_taker(fx: &Fixture, tx: Vec<u8>) -> Result<(), String> {
    let input = payment_input(tx, pubkey(&PEER_KEY), pubkey(&OWN_KEY), "1.5");
    common::block_on(fx.coin.validate_taker_payment(input).compat())
}

#[test]
fn tm_flow_validate_payment_accepts_a_matching_payment() {
    for chain in Chain::BOTH {
        let fx = fixture(chain);
        let peer = account(chain, &pubkey(&PEER_KEY));
        let own = fx.coin.account_id.clone();
        let hash_lock = hex::encode(secret_hash());

        let tx = payment_tx(
            &fx,
            chain,
            &peer,
            &own,
            coin_of(chain, AMOUNT_SAT),
            hash_lock.clone(),
            1_000,
        );
        assert_eq!(validate_maker(&fx, tx.clone()), Ok(()));
        assert_eq!(validate_taker(&fx, tx), Ok(()));

        // The hash lock is compared without regard to the letter case.
        let tx = payment_tx(
            &fx,
            chain,
            &peer,
            &own,
            coin_of(chain, AMOUNT_SAT),
            hash_lock.to_uppercase(),
            1_000,
        );
        assert_eq!(validate_maker(&fx, tx), Ok(()));
    }
}

#[test]
fn tm_flow_validate_payment_rejects_a_wrong_hash_lock() {
    let fx = fixture(Chain::Iris);
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let own = fx.coin.account_id.clone();
    let other_hash = hex::encode(Sha256::digest(b"another preimage"));

    let tx = payment_tx(
        &fx,
        Chain::Iris,
        &peer,
        &own,
        coin_of(Chain::Iris, AMOUNT_SAT),
        other_hash,
        1_000,
    );
    let err = validate_maker(&fx, tx).expect_err("the payment is refused");
    assert!(err.contains("Invalid secret hash"), "{err}");
}

#[test]
fn tm_flow_validate_payment_rejects_a_wrong_sender() {
    let fx = fixture(Chain::Iris);
    let third = account(Chain::Iris, &pubkey(&THIRD_KEY));
    let own = fx.coin.account_id.clone();
    let hash_lock = hex::encode(secret_hash());

    let tx = payment_tx(
        &fx,
        Chain::Iris,
        &third,
        &own,
        coin_of(Chain::Iris, AMOUNT_SAT),
        hash_lock,
        1_000,
    );
    let err = validate_maker(&fx, tx).expect_err("the payment is refused");
    assert!(err.contains("HTLC sender"), "{err}");
}

#[test]
fn tm_flow_validate_payment_rejects_a_wrong_receiver() {
    let fx = fixture(Chain::Iris);
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let third = account(Chain::Iris, &pubkey(&THIRD_KEY));
    let hash_lock = hex::encode(secret_hash());

    let tx = payment_tx(
        &fx,
        Chain::Iris,
        &peer,
        &third,
        coin_of(Chain::Iris, AMOUNT_SAT),
        hash_lock,
        1_000,
    );
    let err = validate_maker(&fx, tx).expect_err("the payment is refused");
    assert!(err.contains("HTLC receiver"), "{err}");
}

#[test]
fn tm_flow_validate_payment_rejects_a_wrong_amount_or_denom() {
    let fx = fixture(Chain::Iris);
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let own = fx.coin.account_id.clone();
    let hash_lock = hex::encode(secret_hash());

    for wrong in [
        coin_of(Chain::Iris, AMOUNT_SAT - 1),
        coin_of(Chain::Iris, AMOUNT_SAT + 1),
        Coin {
            denom: "uother".parse().expect("a valid denom"),
            amount: AMOUNT_SAT.into(),
        },
    ] {
        let tx = payment_tx(&fx, Chain::Iris, &peer, &own, wrong, hash_lock.clone(), 1_000);
        let err = validate_maker(&fx, tx).expect_err("the payment is refused");
        assert!(err.contains("Invalid HTLC amount"), "{err}");
    }
}

#[test]
fn tm_flow_validate_payment_today_accepts_any_time_lock() {
    // CRD ch.18 §18.5: the payment is validated against the negotiated parameters, and the lock time is one of
    // them. Today the time lock of the HTLC is not compared with the negotiated one at all (gap G1): a lock of
    // one block and one of the chain's maximum are both accepted. The later fix turns these two into refusals.
    let fx = fixture(Chain::Iris);
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let own = fx.coin.account_id.clone();
    let hash_lock = hex::encode(secret_hash());

    for blocks in [1, 34_560, 10_000_000] {
        let tx = payment_tx(
            &fx,
            Chain::Iris,
            &peer,
            &own,
            coin_of(Chain::Iris, AMOUNT_SAT),
            hash_lock.clone(),
            blocks,
        );
        assert_eq!(validate_maker(&fx, tx.clone()), Ok(()));
        assert_eq!(validate_taker(&fx, tx), Ok(()));
    }
}

// ---------------------------------------------------------------------------
// check_if_my_payment_sent_for_amount (§18.5, CRD ch.18 R18.11.22)
// ---------------------------------------------------------------------------

/// The id of an HTLC that the module is asked for, read from the data of an `abci_query` request.
fn requested_htlc_id(params: &Json) -> Option<String> {
    let data = hex::decode(params["data"].as_str()?).ok()?;
    Some(QueryHtlcRequestProto::decode(data.as_slice()).ok()?.id)
}

/// A module that holds the given HTLCs as `(id, state, recorded hash lock)` and no other, and a transaction
/// search that answers the given transactions with their result codes.
fn script_module(fx: &Fixture, chain: Chain, htlcs: Vec<(String, i32, String)>, found: Vec<(Vec<u8>, u32)>) {
    fx.node.answer(move |method, params| match method {
        "abci_query" if params["path"] == chain.query_path() => {
            let id = requested_htlc_id(params)?;
            match htlcs.iter().find(|(known, ..)| *known == id) {
                Some((id, state, hash_lock)) => Some(htlc_answer_with_lock(chain, id, *state, hash_lock.clone())),
                // The module answers an unknown id with an empty record.
                None => Some(abci_value(&[])),
            }
        },
        "tx_search" => Some(search_reply(&found)),
        _ => None,
    });
}

/// Asks whether our payment of `amount` coins to the peer's account (or to the account of `other` when given) is
/// sent, as the swap machine does.
fn sent_check(fx: &Fixture, other: &[u8], amount: &str) -> Result<Option<TransactionEnum>, String> {
    let amount = BigDecimal::from_str(amount).expect("a decimal amount");
    common::block_on(
        fx.coin
            .check_if_my_payment_sent_for_amount(0, &pubkey(&OWN_KEY), other, &secret_hash(), 0, &None, &amount)
            .compat(),
    )
}

fn raw_tx_enum(bytes: &[u8]) -> TransactionEnum {
    TransactionEnum::CosmosTransaction(CosmosTransaction {
        data: TxRaw::decode(bytes).expect("a raw transaction"),
    })
}

#[test]
fn tm_flow_check_if_my_payment_sent_finds_the_payment_by_its_id() {
    // CRD ch.18 R18.11.22: the HTLC id is asked of the module, then the create transaction is searched by the id.
    for chain in Chain::BOTH {
        for state in [STATE_OPEN, STATE_COMPLETED, STATE_REFUNDED] {
            let fx = fixture(chain);
            let (ours, id) = own_payment(&fx, chain);
            script_module(&fx, chain, vec![(id.clone(), state, hex::encode(secret_hash()))], vec![
                (ours.clone(), 0),
            ]);

            let found = sent_check(&fx, &pubkey(&PEER_KEY), "1.5");
            assert_eq!(found, Ok(Some(raw_tx_enum(&ours))));

            // The module is asked for the id, and nothing else is searched but the create transaction.
            let queries = fx.node.requests("abci_query");
            assert_eq!(queries.len(), 1);
            let expected_data = hex::encode_upper(QueryHtlcRequestProto { id: id.clone() }.encode_to_vec());
            assert_eq!(
                queries[0]["data"].as_str().map(str::to_ascii_uppercase),
                Some(expected_data)
            );
            let searches = fx.node.requests("tx_search");
            assert_eq!(searches.len(), 1);
            let query = format!("create_htlc.id='{id}'");
            assert_eq!(searches[0]["query"].as_str(), Some(query.as_str()));
            assert_eq!(searches[0]["order_by"].as_str(), Some("desc"));
            // One result is asked for (the number travels as text or as a number, whichever the client sends).
            assert_eq!(searches[0]["per_page"].to_string().trim_matches('"'), "1");
        }
    }
}

#[test]
fn tm_flow_check_if_my_payment_sent_is_none_when_the_module_has_no_htlc() {
    // CRD ch.18 R18.11.22: no HTLC under the id means the payment was not sent; nothing is searched.
    let fx = fixture(Chain::Iris);
    // Even a create transaction with the swap's hash lock in the search results does not matter.
    let (ours, _) = own_payment(&fx, Chain::Iris);
    script_module(&fx, Chain::Iris, Vec::new(), vec![(ours, 0)]);

    assert_eq!(sent_check(&fx, &pubkey(&PEER_KEY), "1.5"), Ok(None));
    assert!(fx.node.requests("tx_search").is_empty());
}

#[test]
fn tm_flow_check_if_my_payment_sent_does_not_match_another_receiver_or_amount() {
    // CRD ch.18 R18.11.22: the receiver and the amount take part in the HTLC id, so a payment with the same hash
    // lock to another receiver, or of another amount, is not "my payment" of this swap. (The page scan this check
    // replaced returned such a payment, gap G5.)
    let fx = fixture(Chain::Iris);
    let own = fx.coin.account_id.clone();
    let third = account(Chain::Iris, &pubkey(&THIRD_KEY));
    let hash = secret_hash();
    let other_payment = payment_tx(
        &fx,
        Chain::Iris,
        &own,
        &third,
        coin_of(Chain::Iris, 7),
        hex::encode(&hash),
        1,
    );
    let other_id = fx
        .coin
        .calculate_htlc_id(&own, &third, &[coin_of(Chain::Iris, 7)], &hash);
    script_module(
        &fx,
        Chain::Iris,
        vec![(other_id, STATE_OPEN, hex::encode(&hash))],
        vec![(other_payment, 0)],
    );

    // Another receiver, another amount: the id differs, the module has no such HTLC.
    assert_eq!(sent_check(&fx, &pubkey(&PEER_KEY), "1.5"), Ok(None));
    assert_eq!(sent_check(&fx, &pubkey(&THIRD_KEY), "1.5"), Ok(None));
    assert_eq!(sent_check(&fx, &pubkey(&THIRD_KEY), "0.000008"), Ok(None));
    assert!(fx.node.requests("tx_search").is_empty());

    // The payment that was sent is found by the receiver and the amount it was sent with.
    assert!(matches!(sent_check(&fx, &pubkey(&THIRD_KEY), "0.000007"), Ok(Some(_))));
}

#[test]
fn tm_flow_check_if_my_payment_sent_fails_on_a_non_zero_result_code() {
    // CRD ch.18 R18.11.22: a create transaction that the chain recorded with a non-zero result code is an error.
    let fx = fixture(Chain::Iris);
    let (ours, id) = own_payment(&fx, Chain::Iris);
    script_module(
        &fx,
        Chain::Iris,
        vec![(id.clone(), STATE_OPEN, hex::encode(secret_hash()))],
        vec![(ours, 5)],
    );

    let err = sent_check(&fx, &pubkey(&PEER_KEY), "1.5").expect_err("a failed create transaction is an error");
    assert!(err.contains(&id), "{err}");
}

#[test]
fn tm_flow_check_if_my_payment_sent_fails_when_the_module_has_the_htlc_and_the_search_finds_nothing() {
    // CRD ch.18 R18.11.22: the module holds the HTLC, so its create transaction must be found; not finding it is
    // an error naming the id, not "not sent".
    let fx = fixture(Chain::Iris);
    let (_, id) = own_payment(&fx, Chain::Iris);
    script_module(
        &fx,
        Chain::Iris,
        vec![(id.clone(), STATE_OPEN, hex::encode(secret_hash()))],
        Vec::new(),
    );

    let err = sent_check(&fx, &pubkey(&PEER_KEY), "1.5").expect_err("the create transaction is missing");
    assert!(err.contains(&id), "{err}");
}

#[test]
fn tm_flow_check_if_my_payment_sent_compares_the_hash_lock_without_regard_to_the_letter_case() {
    // CRD ch.18 R18.11.3 and R18.11.22: the module's recorded hash lock and the create message's are compared
    // case-insensitively, whichever of the two is in upper case.
    for (module_lock, message_lock) in [
        (hex::encode_upper(secret_hash()), hex::encode(secret_hash())),
        (hex::encode(secret_hash()), hex::encode_upper(secret_hash())),
    ] {
        let fx = fixture(Chain::Iris);
        let own = fx.coin.account_id.clone();
        let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
        let ours = payment_tx(
            &fx,
            Chain::Iris,
            &own,
            &peer,
            coin_of(Chain::Iris, AMOUNT_SAT),
            message_lock,
            1_000,
        );
        let id = fx
            .coin
            .calculate_htlc_id(&own, &peer, &[coin_of(Chain::Iris, AMOUNT_SAT)], &secret_hash());
        script_module(&fx, Chain::Iris, vec![(id, STATE_OPEN, module_lock)], vec![(
            ours.clone(),
            0,
        )]);

        assert_eq!(sent_check(&fx, &pubkey(&PEER_KEY), "1.5"), Ok(Some(raw_tx_enum(&ours))));
    }
}

#[test]
fn tm_flow_check_if_my_payment_sent_refuses_a_create_transaction_with_another_hash_lock() {
    // CRD ch.18 R18.11.22: the create message found must carry the hash lock the module recorded.
    let fx = fixture(Chain::Iris);
    let own = fx.coin.account_id.clone();
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let other_hash = hex::encode(Sha256::digest(b"another preimage"));
    let wrong = payment_tx(
        &fx,
        Chain::Iris,
        &own,
        &peer,
        coin_of(Chain::Iris, AMOUNT_SAT),
        other_hash,
        1_000,
    );
    let (_, id) = own_payment(&fx, Chain::Iris);
    script_module(
        &fx,
        Chain::Iris,
        vec![(id.clone(), STATE_OPEN, hex::encode(secret_hash()))],
        vec![(wrong, 0)],
    );

    let err = sent_check(&fx, &pubkey(&PEER_KEY), "1.5").expect_err("the hash lock differs");
    assert!(err.contains(&id), "{err}");
}

#[test]
fn tm_flow_check_if_my_payment_sent_of_a_token_uses_the_token_denom_and_decimals() {
    // CRD ch.18 R18.11.22 and R18.11.11: a token's payment is the single coin in the token's denom, its amount in
    // the token's decimals; the account and the node are the platform's.
    let fx = fixture(Chain::Iris);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        2,
        "unyan".parse().expect("a denom"),
    );
    let own = fx.coin.account_id.clone();
    let peer = account(Chain::Iris, &pubkey(&PEER_KEY));
    let nyan = Coin {
        denom: "unyan".parse().expect("a denom"),
        amount: 150u64.into(),
    };
    let ours = payment_tx(
        &fx,
        Chain::Iris,
        &own,
        &peer,
        nyan.clone(),
        hex::encode(secret_hash()),
        1_000,
    );
    let id = fx.coin.calculate_htlc_id(&own, &peer, &[nyan], &secret_hash());
    script_module(
        &fx,
        Chain::Iris,
        vec![(id, STATE_OPEN, hex::encode(secret_hash()))],
        vec![(ours.clone(), 0)],
    );

    let amount = BigDecimal::from_str("1.5").expect("a decimal amount");
    let found = common::block_on(
        token
            .check_if_my_payment_sent_for_amount(
                0,
                &pubkey(&OWN_KEY),
                &pubkey(&PEER_KEY),
                &secret_hash(),
                0,
                &None,
                &amount,
            )
            .compat(),
    );
    assert_eq!(found, Ok(Some(raw_tx_enum(&ours))));
}

#[test]
fn tm_flow_check_if_my_payment_sent_without_the_amount_is_an_error() {
    // CRD ch.18 R18.11.22: the lookup needs the amount, which `check_if_my_payment_sent` does not carry. The
    // coin and the token answer an error instead of a hash-lock-only guess, and ask the node nothing.
    let fx = fixture(Chain::Iris);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        6,
        "unyan".parse().expect("a denom"),
    );

    let from_coin = common::block_on(
        fx.coin
            .check_if_my_payment_sent(0, &pubkey(&OWN_KEY), &pubkey(&PEER_KEY), &secret_hash(), 0, &None)
            .compat(),
    );
    let from_token = common::block_on(
        token
            .check_if_my_payment_sent(0, &pubkey(&OWN_KEY), &pubkey(&PEER_KEY), &secret_hash(), 0, &None)
            .compat(),
    );
    for answer in [from_coin, from_token] {
        let err = answer.expect_err("the amount is required");
        assert!(err.contains("needs the payment amount"), "{err}");
    }
    assert!(fx.node.requests("abci_query").is_empty());
    assert!(fx.node.requests("tx_search").is_empty());
}

// ---------------------------------------------------------------------------
// search_for_swap_tx_spend_my / _other (§18.3.3, §18.5)
// ---------------------------------------------------------------------------

/// Our payment of 1.5 coins to the peer, and the id of its HTLC.
fn own_payment(fx: &Fixture, chain: Chain) -> (Vec<u8>, String) {
    let own = fx.coin.account_id.clone();
    let peer = account(chain, &pubkey(&PEER_KEY));
    let hash = secret_hash();
    let tx = payment_tx(
        fx,
        chain,
        &own,
        &peer,
        coin_of(chain, AMOUNT_SAT),
        hex::encode(&hash),
        1_000,
    );
    let id = fx
        .coin
        .calculate_htlc_id(&own, &peer, &[coin_of(chain, AMOUNT_SAT)], &hash);
    (tx, id)
}

fn search_my(fx: &Fixture, payment: &[u8]) -> Result<Option<FoundSwapTxSpend>, String> {
    common::block_on(fx.coin.search_for_swap_tx_spend_my(
        0,
        &pubkey(&OWN_KEY),
        &pubkey(&PEER_KEY),
        &secret_hash(),
        payment,
        0,
        &None,
    ))
}

fn search_other(fx: &Fixture, payment: &[u8]) -> Result<Option<FoundSwapTxSpend>, String> {
    common::block_on(fx.coin.search_for_swap_tx_spend_other(
        0,
        &pubkey(&OWN_KEY),
        &pubkey(&PEER_KEY),
        &secret_hash(),
        payment,
        0,
        &None,
    ))
}

/// Answers the HTLC query of the chain with the given state and the transaction search with the given claims.
fn script_htlc(fx: &Fixture, chain: Chain, id: &str, state: i32, claims: Vec<(Vec<u8>, u32)>) {
    let id = id.to_owned();
    fx.node.answer(move |method, params| match method {
        "abci_query" if params["path"] == chain.query_path() => Some(htlc_answer(chain, &id, state)),
        "tx_search" => Some(search_reply(&claims)),
        _ => None,
    });
}

#[test]
fn tm_flow_search_for_swap_tx_spend_of_an_open_htlc_is_none() {
    let fx = fixture(Chain::Iris);
    let (payment, id) = own_payment(&fx, Chain::Iris);
    script_htlc(&fx, Chain::Iris, &id, STATE_OPEN, Vec::new());

    assert_eq!(search_my(&fx, &payment), Ok(None));
    assert_eq!(search_other(&fx, &payment), Ok(None));

    // The chain is asked for the HTLC under its id.
    let queries = fx.node.requests("abci_query");
    assert_eq!(queries.len(), 2);
    let expected_data = hex::encode_upper(QueryHtlcRequestProto { id }.encode_to_vec());
    for query in queries {
        assert_eq!(query["path"].as_str(), Some("/irismod.htlc.Query/HTLC"));
        assert_eq!(
            query["data"].as_str().map(str::to_ascii_uppercase),
            Some(expected_data.clone())
        );
    }
}

#[test]
fn tm_flow_search_for_swap_tx_spend_of_a_completed_htlc_is_the_claim() {
    let fx = fixture(Chain::Iris);
    let (payment, id) = own_payment(&fx, Chain::Iris);
    let claim_any = fx
        .coin
        .gen_claim_htlc_tx(id.clone(), &PREIMAGE)
        .expect("a claim-HTLC message")
        .msg_payload;
    let claim = signed_tx_bytes(&fx.coin, Chain::Iris, claim_any);
    script_htlc(&fx, Chain::Iris, &id, STATE_COMPLETED, vec![(claim, 0)]);

    let found = search_my(&fx, &payment).expect("the search succeeds");
    match found {
        Some(FoundSwapTxSpend::Spent(TransactionEnum::CosmosTransaction(spend))) => {
            // The transaction found is the claim: the secret is read from it.
            let secret = fx.coin.extract_secret(&secret_hash(), &spend.data.encode_to_vec());
            assert_eq!(secret, Ok(PREIMAGE.to_vec()));
        },
        other => panic!("expected the claim as the spend, got {:?}", other),
    }

    // The claim is searched by the HTLC id, oldest first, one result.
    let searches = fx.node.requests("tx_search");
    assert_eq!(searches.len(), 1);
    let query = format!("claim_htlc.id='{id}'");
    assert_eq!(searches[0]["query"].as_str(), Some(query.as_str()));
    assert_eq!(searches[0]["order_by"].as_str(), Some("asc"));
}

#[test]
fn tm_flow_search_for_swap_tx_spend_today_gives_an_empty_tx_for_a_refunded_htlc() {
    // CRD ch.18 §18.3.1: a refund is made by the chain, not by a transaction of ours, so there is no refund
    // transaction to report. Today a refunded HTLC is reported as `Refunded` with an empty transaction (gap G3),
    // which the swap state machine cannot tell from a real one. The later fix changes the answer to the outcome
    // the state machine recognizes, and this test then expects it.
    let fx = fixture(Chain::Iris);
    let (payment, id) = own_payment(&fx, Chain::Iris);
    script_htlc(&fx, Chain::Iris, &id, STATE_REFUNDED, Vec::new());

    let empty = TransactionEnum::CosmosTransaction(CosmosTransaction { data: TxRaw::default() });
    assert_eq!(
        search_my(&fx, &payment),
        Ok(Some(FoundSwapTxSpend::Refunded(empty.clone())))
    );
    assert_eq!(search_other(&fx, &payment), Ok(Some(FoundSwapTxSpend::Refunded(empty))));
}

#[test]
fn tm_flow_search_for_swap_tx_spend_of_a_nucleus_htlc_uses_the_nucleus_query() {
    let fx = fixture(Chain::Nucleus);
    let (payment, id) = own_payment(&fx, Chain::Nucleus);
    script_htlc(&fx, Chain::Nucleus, &id, STATE_OPEN, Vec::new());
    assert_eq!(search_my(&fx, &payment), Ok(None));

    let queries = fx.node.requests("abci_query");
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0]["path"].as_str(), Some("/nucleus.htlc.Query/HTLC"));

    // The same dialect reports its refunded and completed states the same way.
    let claim_any = fx
        .coin
        .gen_claim_htlc_tx(id.clone(), &PREIMAGE)
        .expect("a claim-HTLC message")
        .msg_payload;
    let claim = signed_tx_bytes(&fx.coin, Chain::Nucleus, claim_any);
    script_htlc(&fx, Chain::Nucleus, &id, STATE_COMPLETED, vec![(claim, 0)]);
    assert!(matches!(
        search_my(&fx, &payment),
        Ok(Some(FoundSwapTxSpend::Spent(TransactionEnum::CosmosTransaction(_))))
    ));
    script_htlc(&fx, Chain::Nucleus, &id, STATE_REFUNDED, Vec::new());
    assert!(matches!(
        search_my(&fx, &payment),
        Ok(Some(FoundSwapTxSpend::Refunded(TransactionEnum::CosmosTransaction(_))))
    ));
}

// ---------------------------------------------------------------------------
// send_taker_refunds_payment / send_maker_refunds_payment (§18.3.1)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_refund_methods_today_answer_the_auto_refund_error() {
    // CRD ch.18 §18.3.1: the chain refunds an expired HTLC by itself; the refund methods are to answer an outcome
    // the swap state machine recognizes as "no refund transaction is needed". Today both answer a plain error
    // with a fixed text (gap G3). The later fix changes the answer, and this test then expects the new outcome.
    for chain in Chain::BOTH {
        let fx = fixture(chain);
        let peer = pubkey(&PEER_KEY);
        let hash = secret_hash();

        let taker = common::block_on(
            fx.coin
                .send_taker_refunds_payment(&[], 0, &peer, &hash, &[], &None)
                .compat(),
        );
        match taker {
            Err(TransactionErr::Plain(text)) => assert_eq!(text, AUTO_REFUND_ERROR),
            other => panic!("unexpected answer of the taker refund: {:?}", other),
        }

        let maker = common::block_on(
            fx.coin
                .send_maker_refunds_payment(&[], 0, &peer, &hash, &[], &None)
                .compat(),
        );
        match maker {
            Err(TransactionErr::Plain(text)) => assert_eq!(text, AUTO_REFUND_ERROR),
            other => panic!("unexpected answer of the maker refund: {:?}", other),
        }

        // Nothing was asked of the node.
        assert!(fx.node.requests("abci_query").is_empty());
        assert!(fx.node.requests("tx_search").is_empty());
    }
}

// ---------------------------------------------------------------------------
// extract_secret and wait_for_confirmations (§18.5)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_extract_secret_returns_the_secret_of_a_claim_message() {
    for chain in Chain::BOTH {
        let fx = fixture(chain);
        let id = "ABCD".repeat(16);
        let claim_any = fx
            .coin
            .gen_claim_htlc_tx(id, &PREIMAGE)
            .expect("a claim-HTLC message")
            .msg_payload;
        let claim = signed_tx_bytes(&fx.coin, chain, claim_any);

        assert_eq!(fx.coin.extract_secret(&secret_hash(), &claim), Ok(PREIMAGE.to_vec()));
        // A transaction that is not a transaction at all is an error, not a secret.
        assert!(fx.coin.extract_secret(&secret_hash(), b"not a transaction").is_err());
    }
}

/// A transaction to wait for, and a node that reports it with the given ABCI code.
fn script_confirmation(fx: &Fixture, chain: Chain, code: u32) -> Vec<u8> {
    let claim_any = fx
        .coin
        .gen_claim_htlc_tx("ABCD".repeat(16), &PREIMAGE)
        .expect("a claim-HTLC message")
        .msg_payload;
    let tx = signed_tx_bytes(&fx.coin, chain, claim_any);
    let answer = GetTxResponse {
        tx_response: Some(TxResponse {
            code,
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec();
    fx.node.answer(move |method, params| match method {
        "abci_query" if params["path"] == "/cosmos.tx.v1beta1.Service/GetTx" => Some(abci_value(&answer)),
        _ => None,
    });
    tx
}

#[test]
fn tm_flow_wait_for_confirmations_passes_on_an_ok_status_whatever_the_count() {
    // CRD ch.18 R18.11.18: a Tendermint coin requires zero confirmations and ignores the count, because
    // consensus gives instant finality: a transaction that the node reports with an OK status passes at once,
    // even when 100 confirmations are asked for.
    let fx = fixture(Chain::Iris);
    let tx = script_confirmation(&fx, Chain::Iris, 0);
    let wait_until = common::now_ms() / 1000 + 600;

    let waited = common::block_on(fx.coin.wait_for_confirmations(&tx, 100, false, wait_until, 1).compat());
    assert_eq!(waited, Ok(()));

    // The node is asked for the transaction by the upper-case hex of its SHA-256 hash.
    let queries = fx.node.requests("abci_query");
    assert_eq!(queries.len(), 1);
    let request = GetTxRequest {
        hash: sha256_upper(&[tx.as_slice()]),
    };
    let expected_data = hex::encode_upper(request.encode_to_vec());
    assert_eq!(
        queries[0]["data"].as_str().map(str::to_ascii_uppercase),
        Some(expected_data)
    );
}

#[test]
fn tm_flow_wait_for_confirmations_fails_on_an_error_status() {
    let fx = fixture(Chain::Iris);
    let tx = script_confirmation(&fx, Chain::Iris, 5);
    let wait_until = common::now_ms() / 1000 + 600;

    let err = common::block_on(fx.coin.wait_for_confirmations(&tx, 1, false, wait_until, 1).compat())
        .expect_err("the transaction failed on the chain");
    assert!(err.contains("Got error code: '5'"), "{err}");
}

// ---------------------------------------------------------------------------
// extract_secret: the length of the secret (CRD ch.18 R18.11.14)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_extract_secret_requires_exactly_32_bytes() {
    // CRD ch.18 R18.11.14: the secret of a claim is hex-decoded to exactly 32 bytes; any other length is an
    // error that names the length.
    for chain in Chain::BOTH {
        let fx = fixture(chain);
        let claim_with = |secret: &[u8]| {
            let any = ClaimHtlcMsg::new(
                chain.htlc_type(),
                "ABCD".repeat(16),
                fx.coin.account_id.clone(),
                hex::encode(secret),
            )
            .to_any()
            .expect("a claim-HTLC message");
            signed_tx_bytes(&fx.coin, chain, any)
        };

        for length in [1usize, 31, 33, 64] {
            let claim = claim_with(&vec![0x44; length]);
            let err = fx
                .coin
                .extract_secret(&secret_hash(), &claim)
                .expect_err("a secret of the wrong length is refused");
            assert!(err.contains(&format!("{length} bytes")), "{err}");
        }
        assert_eq!(
            fx.coin.extract_secret(&secret_hash(), &claim_with(&PREIMAGE)),
            Ok(PREIMAGE.to_vec())
        );

        // The token reads the claim the same way.
        let token = TendermintToken::new(
            "TOKEN".to_owned(),
            fx.coin.clone(),
            6,
            "unyan".parse().expect("a denom"),
        );
        assert!(token.extract_secret(&secret_hash(), &claim_with(&[0x44; 31])).is_err());
        assert!(token.extract_secret(&secret_hash(), &claim_with(&[0x44; 33])).is_err());
        assert_eq!(
            token.extract_secret(&secret_hash(), &claim_with(&PREIMAGE)),
            Ok(PREIMAGE.to_vec())
        );
    }
}

// ---------------------------------------------------------------------------
// validate_fee (CRD ch.18 R18.11.21)
// ---------------------------------------------------------------------------

fn swap_uuid() -> uuid::Uuid { uuid::Uuid::from_bytes([0x07; 16]) }

fn coin_proto(denom: &str, sat: u64) -> CoinProto {
    CoinProto {
        denom: denom.to_owned(),
        amount: sat.to_string(),
    }
}

fn msg_send_any(from: &AccountId, to: &AccountId, coins: Vec<CoinProto>) -> Any {
    Any {
        type_url: "/cosmos.bank.v1beta1.MsgSend".to_owned(),
        value: MsgSend {
            from_address: from.to_string(),
            to_address: to.to_string(),
            amount: coins,
        }
        .encode_to_vec(),
    }
}

type AccountCoins = (AccountId, Vec<CoinProto>);

fn multi_send_any(inputs: Vec<AccountCoins>, outputs: Vec<AccountCoins>) -> Any {
    Any {
        type_url: "/cosmos.bank.v1beta1.MsgMultiSend".to_owned(),
        value: MsgMultiSend {
            inputs: inputs
                .into_iter()
                .map(|(address, coins)| Input {
                    address: address.to_string(),
                    coins,
                })
                .collect(),
            outputs: outputs
                .into_iter()
                .map(|(address, coins)| Output {
                    address: address.to_string(),
                    coins,
                })
                .collect(),
        }
        .encode_to_vec(),
    }
}

/// The taker (the fee sender), the fee address and the burn address of the fee tests on a chain.
struct FeeParties {
    taker: AccountId,
    fee_address: AccountId,
    burn_address: AccountId,
    stranger: AccountId,
}

fn fee_parties(chain: Chain) -> FeeParties {
    FeeParties {
        taker: account(chain, &pubkey(&PEER_KEY)),
        fee_address: account(chain, &pubkey(&THIRD_KEY)),
        burn_address: account(chain, &pubkey(&BURN_KEY)),
        stranger: account(chain, &pubkey(&[0x66; 32])),
    }
}

fn standard_fee_any(chain: Chain) -> Any {
    let parties = fee_parties(chain);
    msg_send_any(&parties.taker, &parties.fee_address, vec![coin_proto(
        chain.denom(),
        FEE_SAT,
    )])
}

fn valid_burn_inputs(chain: Chain) -> Vec<AccountCoins> {
    let parties = fee_parties(chain);
    vec![(parties.taker, vec![coin_proto(chain.denom(), FEE_SAT + BURN_SAT)])]
}

fn valid_burn_outputs(chain: Chain) -> Vec<AccountCoins> {
    let parties = fee_parties(chain);
    vec![
        (parties.fee_address, vec![coin_proto(chain.denom(), FEE_SAT)]),
        (parties.burn_address, vec![coin_proto(chain.denom(), BURN_SAT)]),
    ]
}

fn decimal(text: &str) -> MmNumber { MmNumber::from(BigDecimal::from_str(text).expect("a decimal amount")) }

fn standard_dex_fee() -> DexFee { DexFee::Standard(decimal("0.3")) }

fn burn_dex_fee() -> DexFee {
    DexFee::WithBurn {
        fee_amount: decimal("0.3"),
        burn_amount: decimal("0.1"),
        burn_destination: DexFeeBurnDestination::PreBurnAccount {
            burn_pubkey: pubkey(&BURN_KEY),
        },
    }
}

/// The answer of the GetTx query: the node holds the given raw transaction, or none.
fn script_node_copy(fx: &Fixture, copy: Option<Vec<u8>>) {
    let answer = GetTxResponse {
        tx: copy.map(|bytes| Tx::decode(bytes.as_slice()).expect("a transaction")),
        ..Default::default()
    }
    .encode_to_vec();
    fx.node.answer(move |method, params| match method {
        "abci_query" if params["path"] == "/cosmos.tx.v1beta1.Service/GetTx" => Some(abci_value(&answer)),
        _ => None,
    });
}

/// A fee transaction of the given messages and memo, and a node that holds exactly that transaction.
fn fee_fixture(chain: Chain, payloads: Vec<Any>, memo: &str) -> (Fixture, Vec<u8>) {
    let fx = fixture(chain);
    let tx = signed_tx_with(&fx.coin, chain, payloads, memo);
    script_node_copy(&fx, Some(tx.clone()));
    (fx, tx)
}

/// Validates the fee transaction as the maker does: the taker is the peer, the fee address that of the third key.
fn validate_fee_tx(coin: &impl SwapOps, tx: &[u8], dex_fee: &DexFee) -> Result<(), String> {
    let fee_tx = raw_tx_enum(tx);
    let sender = pubkey(&PEER_KEY);
    let fee_addr = pubkey(&THIRD_KEY);
    let uuid = swap_uuid();
    let args = ValidateFeeArgs {
        fee_tx: &fee_tx,
        expected_sender: &sender,
        fee_addr: &fee_addr,
        dex_fee,
        min_block_number: 0,
        uuid: uuid.as_bytes(),
    };
    common::block_on(coin.validate_fee(args).compat())
}

fn uuid_memo() -> String { swap_uuid().to_string() }

/// The fee transaction is refused, with the given text in the error and before the node is asked for a copy.
fn assert_fee_refused(payloads: Vec<Any>, memo: &str, dex_fee: &DexFee, needle: &str) {
    let (fx, tx) = fee_fixture(Chain::Iris, payloads, memo);
    let err = validate_fee_tx(&fx.coin, &tx, dex_fee).expect_err("the fee is refused");
    assert!(err.contains(needle), "{err}");
    // The body is checked first: the node is not asked for a copy of a transaction that is wrong already.
    assert!(fx.node.requests("abci_query").is_empty());
}

#[test]
fn tm_flow_validate_fee_accepts_the_standard_fee() {
    // CRD ch.18 R18.11.21: one MsgSend to the fee address from the taker with exactly the fee coin, the swap uuid
    // as the memo, and a node copy equal to the supplied bytes.
    for chain in Chain::BOTH {
        let (fx, tx) = fee_fixture(chain, vec![standard_fee_any(chain)], &uuid_memo());
        assert_eq!(validate_fee_tx(&fx.coin, &tx, &standard_dex_fee()), Ok(()));

        // The node is asked for the transaction by the upper-case hex of the SHA-256 hash of the raw bytes.
        let queries = fx.node.requests("abci_query");
        assert_eq!(queries.len(), 1);
        let request = GetTxRequest {
            hash: sha256_upper(&[tx.as_slice()]),
        };
        assert_eq!(
            queries[0]["data"].as_str().map(str::to_ascii_uppercase),
            Some(hex::encode_upper(request.encode_to_vec()))
        );
    }
}

#[test]
fn tm_flow_validate_fee_refuses_a_wrong_recipient() {
    // CRD ch.18 R18.11.21: the recipient is the fee address.
    let parties = fee_parties(Chain::Iris);
    let any = msg_send_any(&parties.taker, &parties.stranger, vec![coin_proto("uiris", FEE_SAT)]);
    assert_fee_refused(vec![any], &uuid_memo(), &standard_dex_fee(), "Invalid receiver");
}

#[test]
fn tm_flow_validate_fee_refuses_a_wrong_sender() {
    // CRD ch.18 R18.11.21: the sender is the taker's derived account.
    let parties = fee_parties(Chain::Iris);
    let any = msg_send_any(&parties.stranger, &parties.fee_address, vec![coin_proto(
        "uiris", FEE_SAT,
    )]);
    assert_fee_refused(vec![any], &uuid_memo(), &standard_dex_fee(), "Invalid sender");
}

#[test]
fn tm_flow_validate_fee_refuses_two_coins_a_wrong_amount_and_a_wrong_denom() {
    // CRD ch.18 R18.11.21: the amount list has exactly one coin, equal in denom and decimal string to the fee.
    let parties = fee_parties(Chain::Iris);
    let to_fee_address = |coins: Vec<CoinProto>| msg_send_any(&parties.taker, &parties.fee_address, coins);
    for coins in [
        vec![coin_proto("uiris", FEE_SAT), coin_proto("uiris", 1)],
        vec![coin_proto("uiris", FEE_SAT), coin_proto("uother", 1)],
        vec![coin_proto("uother", 1), coin_proto("uiris", FEE_SAT)],
        vec![coin_proto("uiris", FEE_SAT - 1)],
        vec![coin_proto("uiris", FEE_SAT + 1)],
        vec![coin_proto("uother", FEE_SAT)],
        Vec::new(),
    ] {
        assert_fee_refused(
            vec![to_fee_address(coins)],
            &uuid_memo(),
            &standard_dex_fee(),
            "Invalid fee amount",
        );
    }
}

#[test]
fn tm_flow_validate_fee_refuses_two_messages() {
    // CRD ch.18 R18.11.21: exactly one message in the body, even when the first is the right one.
    let any = standard_fee_any(Chain::Iris);
    assert_fee_refused(
        vec![any.clone(), any],
        &uuid_memo(),
        &standard_dex_fee(),
        "Invalid number of messages",
    );
    assert_fee_refused(
        Vec::new(),
        &uuid_memo(),
        &standard_dex_fee(),
        "Invalid number of messages",
    );
}

#[test]
fn tm_flow_validate_fee_refuses_a_wrong_memo() {
    // CRD ch.18 R18.11.21: the memo is the swap uuid in hyphenated form.
    let any = standard_fee_any(Chain::Iris);
    assert_fee_refused(vec![any.clone()], "not the uuid", &standard_dex_fee(), "Invalid memo");
    assert_fee_refused(vec![any.clone()], "", &standard_dex_fee(), "Invalid memo");
    assert_fee_refused(
        vec![any],
        &swap_uuid().simple().to_string(),
        &standard_dex_fee(),
        "Invalid memo",
    );
}

#[test]
fn tm_flow_validate_fee_refuses_a_message_of_the_other_form() {
    // CRD ch.18 R18.11.21: the standard descriptor is a MsgSend and the burn descriptor a MsgMultiSend; the
    // message of the other form is refused whatever its fields decode to.
    let burn = multi_send_any(valid_burn_inputs(Chain::Iris), valid_burn_outputs(Chain::Iris));
    assert_fee_refused(
        vec![burn],
        &uuid_memo(),
        &standard_dex_fee(),
        "Invalid fee message type",
    );
    assert_fee_refused(
        vec![standard_fee_any(Chain::Iris)],
        &uuid_memo(),
        &burn_dex_fee(),
        "Invalid fee message type",
    );
}

#[test]
fn tm_flow_validate_fee_refuses_no_fee_descriptor() {
    // CRD ch.18 R18.11.21: a no-fee descriptor is rejected on the validating side (and on the sending side).
    assert_fee_refused(
        vec![standard_fee_any(Chain::Iris)],
        &uuid_memo(),
        &DexFee::NoFee,
        "NoFee",
    );
}

#[test]
fn tm_flow_validate_fee_accepts_the_burn_fee() {
    // CRD ch.18 R18.11.21: one MsgMultiSend with one input of the taker and two outputs, the fee address with
    // exactly the fee coin and then the burn address with exactly the burn coin.
    for chain in Chain::BOTH {
        let any = multi_send_any(valid_burn_inputs(chain), valid_burn_outputs(chain));
        let (fx, tx) = fee_fixture(chain, vec![any], &uuid_memo());
        assert_eq!(validate_fee_tx(&fx.coin, &tx, &burn_dex_fee()), Ok(()));
        assert_eq!(fx.node.requests("abci_query").len(), 1);
    }
}

#[test]
fn tm_flow_validate_fee_refuses_a_wrong_burn_fee() {
    // CRD ch.18 R18.11.21: the burn amount, the burn address, the number of outputs and inputs, the fee output
    // and the sender are all checked.
    let chain = Chain::Iris;
    let parties = fee_parties(chain);
    let denom = chain.denom();
    let check = |inputs: Vec<AccountCoins>, outputs: Vec<AccountCoins>, needle: &str| {
        assert_fee_refused(
            vec![multi_send_any(inputs, outputs)],
            &uuid_memo(),
            &burn_dex_fee(),
            needle,
        );
    };
    let fee_output = (parties.fee_address.clone(), vec![coin_proto(denom, FEE_SAT)]);
    let burn_output = (parties.burn_address.clone(), vec![coin_proto(denom, BURN_SAT)]);

    // A wrong burn amount.
    check(
        valid_burn_inputs(chain),
        vec![
            fee_output.clone(),
            (parties.burn_address.clone(), vec![coin_proto(denom, BURN_SAT + 1)]),
        ],
        "Invalid burn output amount",
    );
    // A wrong burn address.
    check(
        valid_burn_inputs(chain),
        vec![
            fee_output.clone(),
            (parties.stranger.clone(), vec![coin_proto(denom, BURN_SAT)]),
        ],
        "Invalid burn output address",
    );
    // One output, three outputs.
    check(
        valid_burn_inputs(chain),
        vec![fee_output.clone()],
        "Invalid number of outputs",
    );
    check(
        valid_burn_inputs(chain),
        vec![fee_output.clone(), burn_output.clone(), burn_output.clone()],
        "Invalid number of outputs",
    );
    // Two inputs, no input.
    let input = (parties.taker.clone(), vec![coin_proto(denom, FEE_SAT + BURN_SAT)]);
    check(
        vec![input.clone(), input],
        valid_burn_outputs(chain),
        "Invalid number of inputs",
    );
    check(Vec::new(), valid_burn_outputs(chain), "Invalid number of inputs");
    // A wrong fee output: the amount, two coins, the address, and the outputs in the other order.
    check(
        valid_burn_inputs(chain),
        vec![
            (parties.fee_address.clone(), vec![coin_proto(denom, FEE_SAT - 1)]),
            burn_output.clone(),
        ],
        "Invalid fee output amount",
    );
    check(
        valid_burn_inputs(chain),
        vec![
            (parties.fee_address.clone(), vec![
                coin_proto(denom, FEE_SAT),
                coin_proto(denom, 1),
            ]),
            burn_output.clone(),
        ],
        "Invalid fee output amount",
    );
    check(
        valid_burn_inputs(chain),
        vec![
            (parties.stranger.clone(), vec![coin_proto(denom, FEE_SAT)]),
            burn_output.clone(),
        ],
        "Invalid fee output address",
    );
    check(
        valid_burn_inputs(chain),
        vec![burn_output, fee_output],
        "Invalid fee output address",
    );
    // A wrong sender.
    check(
        vec![(parties.stranger, vec![coin_proto(denom, FEE_SAT + BURN_SAT)])],
        valid_burn_outputs(chain),
        "Invalid sender",
    );
}

#[test]
fn tm_flow_validate_fee_refuses_a_node_copy_that_differs() {
    // CRD ch.18 R18.11.21: the node's copy of the transaction must equal the supplied bytes.
    let chain = Chain::Iris;
    let fx = fixture(chain);
    let tx = signed_tx_with(&fx.coin, chain, vec![standard_fee_any(chain)], &uuid_memo());
    let mut other = Tx::decode(tx.as_slice()).expect("a transaction");
    other.signatures[0][0] ^= 0xFF;
    assert_ne!(other.encode_to_vec(), tx);
    script_node_copy(&fx, Some(other.encode_to_vec()));

    let err = validate_fee_tx(&fx.coin, &tx, &standard_dex_fee()).expect_err("the copies differ");
    assert!(err.contains(&sha256_upper(&[tx.as_slice()])), "{err}");
}

#[test]
fn tm_flow_validate_fee_refuses_a_transaction_the_node_does_not_have() {
    // CRD ch.18 R18.11.21: a transaction that the node does not hold is a validation error naming the hash.
    let chain = Chain::Iris;
    let fx = fixture(chain);
    let tx = signed_tx_with(&fx.coin, chain, vec![standard_fee_any(chain)], &uuid_memo());
    script_node_copy(&fx, None);

    let err = validate_fee_tx(&fx.coin, &tx, &standard_dex_fee()).expect_err("the node has no copy");
    assert!(err.contains(&sha256_upper(&[tx.as_slice()])), "{err}");
}

#[test]
fn tm_flow_token_validate_fee_uses_the_token_denom() {
    // CRD ch.18 R18.11.21 and R18.11.11: a token's fee is paid in the token's denom; the same checks apply.
    let chain = Chain::Iris;
    let fx = fixture(chain);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        6,
        "unyan".parse().expect("a denom"),
    );
    let parties = fee_parties(chain);

    let in_token = signed_tx_with(
        &fx.coin,
        chain,
        vec![msg_send_any(&parties.taker, &parties.fee_address, vec![coin_proto(
            "unyan", FEE_SAT,
        )])],
        &uuid_memo(),
    );
    script_node_copy(&fx, Some(in_token.clone()));
    assert_eq!(validate_fee_tx(&token, &in_token, &standard_dex_fee()), Ok(()));

    // The platform denom is not the token's.
    let in_platform = signed_tx_with(&fx.coin, chain, vec![standard_fee_any(chain)], &uuid_memo());
    script_node_copy(&fx, Some(in_platform.clone()));
    let err = validate_fee_tx(&token, &in_platform, &standard_dex_fee()).expect_err("the denom is wrong");
    assert!(err.contains("Invalid fee amount"), "{err}");

    // The burn form of a token is checked as the coin's is.
    let burn = multi_send_any(
        vec![(parties.taker.clone(), vec![coin_proto("unyan", FEE_SAT + BURN_SAT)])],
        vec![
            (parties.fee_address.clone(), vec![coin_proto("unyan", FEE_SAT)]),
            (parties.burn_address.clone(), vec![coin_proto("unyan", BURN_SAT + 1)]),
        ],
    );
    let wrong_burn = signed_tx_with(&fx.coin, chain, vec![burn], &uuid_memo());
    script_node_copy(&fx, Some(wrong_burn.clone()));
    let err = validate_fee_tx(&token, &wrong_burn, &burn_dex_fee()).expect_err("the burn amount is wrong");
    assert!(err.contains("Invalid burn output amount"), "{err}");
}

// ---------------------------------------------------------------------------
// avg_blocktime at activation (CRD ch.18 R18.11.1)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_activation_refuses_an_avg_blocktime_of_zero() {
    // CRD ch.18 R18.11.1: the block count of a lock is the lock duration divided by `avg_blocktime`, so a value
    // of 0 is refused at activation with an error that names the key; a positive value is accepted.
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let activate = |avg_blocktime: Json| {
        let mut conf = Chain::Iris.conf();
        conf["avg_blocktime"] = avg_blocktime;
        common::block_on(tendermint_coin_from_conf_and_request(
            &ctx,
            "IRIS".to_owned(),
            &conf,
            vec![RpcNode::for_test(&node.url())],
            &OWN_KEY,
            false,
        ))
    };

    let err = activate(json!(0)).expect_err("a block time of 0 is refused");
    let text = err.to_string();
    assert!(text.contains("avg_blocktime"), "{text}");
    assert!(text.contains("IRIS"), "{text}");

    for accepted in [json!(1), json!(5), json!(255)] {
        assert!(activate(accepted.clone()).is_ok(), "avg_blocktime {accepted}");
    }
    // The values that were refused before are still refused.
    for refused in [json!(-1), json!(256), json!("5"), json!(null)] {
        assert!(activate(refused.clone()).is_err(), "avg_blocktime {refused}");
    }
}

#[test]
fn tm_flow_conf_refuses_an_avg_blocktime_of_zero_and_names_the_key() {
    // CRD ch.18 R18.11.1: the same check, on the configuration parser alone.
    let conf = |value: Json| json!({ "coin": "IRIS", "avg_blocktime": value });
    let refused = match TendermintConf::try_from_json("IRIS", &conf(json!(0))) {
        Ok(_) => panic!("a block time of 0 is accepted"),
        Err(e) => e.to_string(),
    };
    assert!(refused.contains("`avg_blocktime`"), "{refused}");
    assert!(TendermintConf::try_from_json("IRIS", &conf(json!(1))).is_ok());
}

// ---------------------------------------------------------------------------
// The fees of a token carry the platform's label and decimals (CRD ch.18 R18.11.10, R18.11.11)
// ---------------------------------------------------------------------------

/// A node that estimates fees: it reports the block height, serves the account and simulates every transaction
/// with the given gas use. At the gas price 0.25 the fee is `ceil(gas_used * 1.5 * 0.25)` base units.
fn script_fee_estimation(fx: &Fixture, gas_used: u64) {
    let account = QueryAccountResponse {
        account: Some(Any {
            type_url: "/cosmos.auth.v1beta1.BaseAccount".to_owned(),
            value: BaseAccount {
                account_number: 1,
                sequence: 1,
                ..Default::default()
            }
            .encode_to_vec(),
        }),
    }
    .encode_to_vec();
    let simulation = SimulateResponse {
        gas_info: Some(GasInfo {
            gas_wanted: 125_000,
            gas_used,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    fx.node.answer(move |method, params| match method {
        "abci_info" => Some(json!({
            "response": {
                "data": "test",
                "version": "0",
                "app_version": "1",
                "last_block_height": "100",
                "last_block_app_hash": ""
            }
        })),
        "abci_query" if params["path"] == "/cosmos.auth.v1beta1.Query/Account" => Some(abci_value(&account)),
        "abci_query" if params["path"] == "/cosmos.tx.v1beta1.Service/Simulate" => Some(abci_value(&simulation)),
        _ => None,
    });
}

#[test]
fn tm_flow_token_trade_fees_carry_the_platform_ticker_and_decimals() {
    // CRD ch.18 R18.11.10 and R18.11.11: the network fee of a token's payment is paid in the platform denom, so
    // the sender-side trade fee and the fee to send the dex fee are labeled with the platform ticker and
    // converted with the platform decimals (here 6), not the token's (here 2). The receiver-side fee is the
    // platform's already.
    let fx = fixture(Chain::Iris);
    // 80,000 gas units cost ceil(80,000 * 1.5 * 0.25) = 30,000 base units: 0.03 at 6 decimals (300 at 2).
    script_fee_estimation(&fx, 80_000);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        2,
        "unyan".parse().expect("a denom"),
    );
    let expected = BigDecimal::from_str("0.03").expect("a decimal amount");

    let sender = common::block_on(token.get_sender_trade_fee(
        TradePreimageValue::Exact(BigDecimal::from_str("1.5").expect("a decimal amount")),
        FeeApproxStage::OrderIssue,
    ))
    .unwrap_or_else(|e| panic!("the sender trade fee: {}", e));
    assert_eq!(sender.coin, "IRIS");
    assert_eq!(sender.amount.to_decimal(), expected);
    assert!(!sender.paid_from_trading_vol);

    let to_send_fee = common::block_on(token.get_fee_to_send_taker_fee(
        BigDecimal::from_str("0.5").expect("a decimal amount"),
        FeeApproxStage::OrderIssue,
    ))
    .unwrap_or_else(|e| panic!("the fee to send the dex fee: {}", e));
    assert_eq!(to_send_fee.coin, "IRIS");
    assert_eq!(to_send_fee.amount.to_decimal(), expected);
    assert!(!to_send_fee.paid_from_trading_vol);

    let receiver = common::block_on(token.get_receiver_trade_fee(FeeApproxStage::OrderIssue).compat())
        .unwrap_or_else(|e| panic!("the receiver trade fee: {}", e));
    assert_eq!(receiver.coin, "IRIS");
    assert_eq!(receiver.amount.to_decimal(), expected);

    // The platform coin's own fees are unchanged: its ticker and its decimals.
    let own = common::block_on(fx.coin.get_sender_trade_fee(
        TradePreimageValue::Exact(BigDecimal::from_str("1.5").expect("a decimal amount")),
        FeeApproxStage::OrderIssue,
    ))
    .unwrap_or_else(|e| panic!("the platform trade fee: {}", e));
    assert_eq!(own.coin, "IRIS");
    assert_eq!(own.amount.to_decimal(), expected);
}

// ---------------------------------------------------------------------------
// Order-time refusals (CRD ch.18 R18.11.23, R18.11.25)
// ---------------------------------------------------------------------------

#[test]
fn tm_flow_a_chain_without_an_htlc_module_is_wallet_only() {
    // CRD ch.18 R18.11.23: a coin whose account prefix selects no HTLC dialect (only `iaa` and `nuc` have one) is
    // wallet-only, whatever the coins file says, so it is refused at order time and not at payment time. The
    // build switch is off in this build, so the predicate is tested with the switch on.
    for prefix in ["cosmos", "osmo", "atom", ""] {
        assert!(
            tendermint_wallet_only(true, prefix, false, true, false),
            "prefix '{prefix}'"
        );
        assert!(
            tendermint_wallet_only(true, prefix, true, true, false),
            "prefix '{prefix}'"
        );
    }
    for prefix in ["iaa", "nuc"] {
        assert!(
            !tendermint_wallet_only(true, prefix, false, true, false),
            "prefix '{prefix}'"
        );
        // The coins-file flag still decides for the chains that have a module.
        assert!(
            tendermint_wallet_only(true, prefix, true, true, false),
            "prefix '{prefix}'"
        );
        // The build switch off makes every Tendermint coin wallet-only.
        assert!(
            tendermint_wallet_only(false, prefix, false, true, false),
            "prefix '{prefix}'"
        );
    }
}

#[test]
fn tm_flow_the_wallet_only_answer_of_the_coin_and_token_follows_the_prefix() {
    // CRD ch.18 R18.11.23: the coin and the token answer through the predicate; with the build switch off both
    // are wallet-only.
    let fx = fixture(Chain::Iris);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        6,
        "unyan".parse().expect("a denom"),
    );
    assert!(fx.coin.wallet_only(&fx.ctx));
    assert!(token.wallet_only(&fx.ctx));
}

#[test]
fn tm_flow_ibc_routing_is_never_advertised_or_accepted() {
    // CRD ch.18 R18.11.25: Reloaded advertises no protocol information for the family, so no IBC routing channel
    // travels with an order, and it declines a match whose counterparty information is not empty. The decline
    // does not depend on the `ibc-routing-for-swaps` feature: the check is in the coin.
    let fx = fixture(Chain::Iris);
    let token = TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        6,
        "unyan".parse().expect("a denom"),
    );
    assert!(fx.coin.coin_protocol_info().is_empty());
    assert!(token.coin_protocol_info().is_empty());

    for swaps_available in [false, true] {
        for info in [Some(vec![1u8]), Some(b"channel-0".to_vec()), Some(vec![0u8; 64])] {
            assert!(!tendermint_protocol_info_supported(swaps_available, &info));
        }
    }
    // Empty information is what a counterparty without routing sends; it is accepted when swaps are on.
    assert!(tendermint_protocol_info_supported(true, &None));
    assert!(tendermint_protocol_info_supported(true, &Some(Vec::new())));
    assert!(!tendermint_protocol_info_supported(false, &None));
    assert!(!tendermint_protocol_info_supported(false, &Some(Vec::new())));

    // With the build switch off, as here, the coin and the token decline every match.
    for info in [None, Some(Vec::new()), Some(vec![1u8])] {
        assert!(!fx.coin.is_coin_protocol_supported(&info));
        assert!(!token.is_coin_protocol_supported(&info));
    }
}

// ---------------------------------------------------------------------------
// Activation features (CRD ch.36): a key-less policy, a custom token and the HD key
// ---------------------------------------------------------------------------

const HD_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
/// The account number the scripted node reports for every account.
const ACCOUNT_NUMBER: u64 = 7;

/// A coin on the chain and account of `fx.coin` whose activation policy is the key-less hardware policy, the one
/// policy of `PrivKeyPolicy` that holds no private key.
fn keyless_coin(fx: &Fixture) -> TendermintCoin {
    let conf = Chain::Iris.conf();
    let tendermint_conf =
        TendermintConf::try_from_json("IRIS", &conf).unwrap_or_else(|e| panic!("the test configuration: {}", e));
    let protocol_info: TendermintProtocolInfo =
        serde_json::from_value(conf["protocol"]["protocol_data"].clone()).expect("the protocol data");
    TendermintCoin::from(TendermintCoinImpl::new(
        "IRIS".to_owned(),
        tendermint_conf,
        protocol_info,
        fx.coin.account_id.clone(),
        TendermintActivationPolicy::with_private_key_policy(PrivKeyPolicy::Trezor),
        Vec::new(),
        fx.ctx.weak(),
        false,
    ))
}

fn nyan_token(platform: &TendermintCoin, custom_token: bool) -> TendermintToken {
    TendermintToken::new_with_custom_flag(
        "IRIS-NYAN".to_owned(),
        platform.clone(),
        6,
        "unyan".parse().expect("a denom"),
        custom_token,
    )
}

#[test]
fn tm_act_the_predicate_is_true_for_each_reason_alone() {
    // CRD ch.18 R18.11.23, R18.11.23a, ch.36 R36.1.2: with swaps on, a chain with an HTLC module, no coins-file
    // flag, a local key and no custom token is the one case that can trade; each other input alone makes the coin
    // wallet-only.
    assert!(!tendermint_wallet_only(true, "iaa", false, true, false));
    assert!(tendermint_wallet_only(false, "iaa", false, true, false), "swaps off");
    assert!(
        tendermint_wallet_only(true, "cosmos", false, true, false),
        "no HTLC dialect"
    );
    assert!(
        tendermint_wallet_only(true, "iaa", true, true, false),
        "coins-file flag"
    );
    assert!(tendermint_wallet_only(true, "iaa", false, false, false), "no local key");
    assert!(tendermint_wallet_only(true, "iaa", false, true, true), "custom token");
}

#[test]
fn tm_act_a_coin_and_token_without_a_local_key_are_wallet_only() {
    // CRD ch.18 R18.11.23a (Reloaded rule): a coin whose activation policy holds no local key, and a token of such a
    // coin, are wallet-only, so order placement refuses them before the legacy swap path could announce this
    // node's own key as the coin's HTLC public key. The build switch is off, so the coin and the token are
    // evaluated with the switch on.
    let fx = fixture(Chain::Iris);
    let keyless = keyless_coin(&fx);

    assert!(fx.coin.activation_policy.has_local_key());
    assert!(!keyless.activation_policy.has_local_key());
    // The key-less coin has no HTLC key pair: the case the refusal exists for.
    assert!(fx.coin.get_htlc_key_pair().is_some());
    assert!(keyless.get_htlc_key_pair().is_none());

    // `iaa` has an HTLC module and nothing else refuses the coin, so the key alone decides.
    assert!(
        !fx.coin.wallet_only_with_switch(&fx.ctx, true),
        "a coin with a key is unaffected"
    );
    assert!(keyless.wallet_only_with_switch(&fx.ctx, true), "a coin without a key");
    assert!(
        !nyan_token(&fx.coin, false).wallet_only_with_switch(&fx.ctx, true),
        "a token with a key"
    );
    assert!(
        nyan_token(&keyless, false).wallet_only_with_switch(&fx.ctx, true),
        "a token uses the policy of its platform coin"
    );

    // With the build switch off, as in this build, the answer is wallet-only either way.
    assert!(keyless.wallet_only(&fx.ctx));
    assert!(nyan_token(&keyless, false).wallet_only(&fx.ctx));
}

#[test]
fn tm_act_a_custom_token_is_wallet_only_and_a_configured_token_is_not() {
    // CRD ch.36 R36.1.2 (Reloaded rule): a custom token, activated from an inline protocol, is wallet-only as a
    // custom EVM token is (ch.35 R35.2.7). A token of the coins file is unchanged.
    let fx = fixture(Chain::Iris);
    let custom = nyan_token(&fx.coin, true);
    let configured = nyan_token(&fx.coin, false);
    assert!(custom.custom_token);
    assert!(!configured.custom_token);
    assert!(custom.wallet_only_with_switch(&fx.ctx, true));
    assert!(!configured.wallet_only_with_switch(&fx.ctx, true));
    assert!(custom.wallet_only(&fx.ctx));

    // The constructors that take no flag build a configured token.
    let from_protocol = TendermintToken::from_protocol("IRIS-NYAN".to_owned(), fx.coin.clone(), 6, "unyan")
        .unwrap_or_else(|e| panic!("the token: {}", e));
    assert!(!from_protocol.custom_token);
    let custom_from_protocol =
        TendermintToken::from_protocol_with_custom_flag("IRIS-NYAN".to_owned(), fx.coin.clone(), 6, "unyan", true)
            .unwrap_or_else(|e| panic!("the token: {}", e));
    assert!(custom_from_protocol.custom_token);
}

/// The secret key at `path` of the test mnemonic, derived with the BIP-39 and BIP-32 crates directly, not through
/// the wallet code under test.
fn independent_secret(path: &str) -> [u8; 32] {
    let mnemonic = bip39::Mnemonic::from_phrase(HD_MNEMONIC, bip39::Language::English).expect("a valid mnemonic");
    let seed = bip39::Seed::new(&mnemonic, "");
    let mut key: bip32::ExtendedPrivateKey<secp256k1::SecretKey> =
        bip32::ExtendedPrivateKey::new(seed.as_bytes()).expect("a master key");
    let path: bip32::DerivationPath = path.parse().expect("a derivation path");
    for child in path {
        key = key.derive_child(child).expect("a child key");
    }
    *key.private_key().as_ref()
}

/// The bech32 account address of the key with the given secret.
fn address_of_secret(secret: &[u8; 32], prefix: &str) -> String {
    SigningKey::from_slice(secret)
        .expect("a signing key")
        .public_key()
        .account_id(prefix)
        .expect("an account id")
        .to_string()
}

/// A Cosmos Hub style coin with the derivation path `m/44'/118'`.
fn atom_conf() -> Json {
    json!({
        "coin": "ATOM",
        "avg_blocktime": 6,
        "derivation_path": "m/44'/118'",
        "protocol": { "type": "TENDERMINT", "protocol_data": {
            "denom": "uatom",
            "decimals": 6,
            "account_prefix": "cosmos",
            "chain_id": "cosmoshub-4",
        } },
        "mm2": 1
    })
}

fn hd_login_ctx() -> MmArc {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }))
        .into_mm_arc();
    CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).unwrap();
    ctx
}

fn global_hd_of(ctx: &MmArc) -> GlobalHDAccountArc {
    match CryptoCtx::from_ctx(ctx).unwrap().key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        KeyPairPolicy::Iguana => panic!("expected an HD login"),
    }
}

fn hd_coin_of(ctx: &MmArc, conf: &Json, node: &FakeNode, path_to_address: &PathToAddress) -> TendermintCoin {
    common::block_on(tendermint_coin_from_conf_and_request_with_global_hd(
        ctx,
        "ATOM".to_owned(),
        conf,
        vec![RpcNode::for_test(&node.url())],
        &global_hd_of(ctx),
        path_to_address,
        false,
    ))
    .unwrap_or_else(|e| panic!("cannot build the HD test coin: {}", e))
}

fn hd_coin(ctx: &MmArc, node: &FakeNode, path_to_address: &PathToAddress) -> TendermintCoin {
    hd_coin_of(ctx, &atom_conf(), node, path_to_address)
}

/// The secret of the coin's activation policy.
fn activated_secret(coin: &TendermintCoin) -> Vec<u8> {
    coin.activation_policy
        .activated_key_or_err()
        .map_err(|e| e.to_string())
        .expect("the activated key")
        .as_slice()
        .to_vec()
}

/// What the coin reports as its address, its public key and its HTLC key must be those of the key with `secret`.
fn assert_coin_is_the_key(coin: &TendermintCoin, secret: &[u8; 32], context: &str) {
    assert_eq!(
        coin.my_address(),
        Ok(address_of_secret(secret, "cosmos")),
        "address, {context}"
    );
    assert_eq!(activated_secret(coin), secret.to_vec(), "activated key, {context}");
    // The public key of the activation policy is the one of the activated private key, not the wallet's root
    // public key (CRD ch.36 §36.7 code-quality finding).
    assert_eq!(
        coin.activation_policy.public_key().expect("a public key"),
        cosmrs::tendermint::PublicKey::from_raw_secp256k1(&pubkey(secret)).expect("a valid public key"),
        "public key of the policy, {context}"
    );
    assert_eq!(
        coin.get_htlc_key_pair().expect("an HTLC key pair").public_slice(),
        &pubkey(secret)[..],
        "HTLC public key, {context}"
    );
    assert_eq!(
        coin.get_public_key().ok(),
        Some(
            SigningKey::from_slice(secret)
                .expect("a signing key")
                .public_key()
                .to_string()
        ),
        "reported public key, {context}"
    );
}

#[test]
fn tm_act_iguana_login_address_is_unchanged() {
    // CRD ch.36 R36.1.2: without HD mode the activated address is the one of the single key, as before.
    let fx = fixture(Chain::Iris);
    assert_eq!(fx.coin.my_address(), Ok(address_of_secret(&OWN_KEY, "iaa")));
    assert_eq!(activated_secret(&fx.coin), OWN_KEY.to_vec());
}

#[test]
fn tm_act_hd_login_activates_the_standard_address_at_the_default_path() {
    // CRD ch.36 R36.1.2, R36.1.4 step 3: under an HD login the coin is the address at account 0, External, address
    // 0 under the coin's `derivation_path`: `m/44'/118'/0'/0/0`, as any BIP-44 wallet derives it.
    let ctx = hd_login_ctx();
    let node = FakeNode::start();
    let coin = hd_coin(&ctx, &node, &PathToAddress::default());
    let expected = independent_secret("m/44'/118'/0'/0/0");
    assert_coin_is_the_key(&coin, &expected, "default path");
    assert!(coin.activation_policy.has_local_key());
}

#[test]
fn tm_act_hd_login_activates_the_address_at_the_given_path_to_address() {
    // CRD ch.36 R36.1.2: another `path_to_address` selects the address at that path.
    let ctx = hd_login_ctx();
    let node = FakeNode::start();
    let default_address = hd_coin(&ctx, &node, &PathToAddress::default()).my_address();
    let cases = [
        (
            PathToAddress {
                account_id: 1,
                chain: Bip44Chain::External,
                address_id: 0,
            },
            "m/44'/118'/1'/0/0",
        ),
        (
            PathToAddress {
                account_id: 0,
                chain: Bip44Chain::Internal,
                address_id: 0,
            },
            "m/44'/118'/0'/1/0",
        ),
        (
            PathToAddress {
                account_id: 2,
                chain: Bip44Chain::Internal,
                address_id: 7,
            },
            "m/44'/118'/2'/1/7",
        ),
    ];
    for (path_to_address, path) in cases {
        let coin = hd_coin(&ctx, &node, &path_to_address);
        assert_coin_is_the_key(&coin, &independent_secret(path), path);
        assert_ne!(coin.my_address(), default_address, "{path}");
    }
}

#[test]
fn tm_act_hd_login_without_a_derivation_path_fails_activation() {
    // CRD ch.36 R36.1.4 step 3: a coin configuration without `derivation_path` cannot be activated under an HD login.
    let ctx = hd_login_ctx();
    let node = FakeNode::start();
    let mut conf = atom_conf();
    conf.as_object_mut().expect("an object").remove("derivation_path");
    let err = common::block_on(tendermint_coin_from_conf_and_request_with_global_hd(
        &ctx,
        "ATOM".to_owned(),
        &conf,
        vec![RpcNode::for_test(&node.url())],
        &global_hd_of(&ctx),
        &PathToAddress::default(),
        false,
    ))
    .err()
    .expect("the activation needs a derivation path");
    assert_eq!(err.get_inner().ticker, "ATOM");
    assert!(
        matches!(err.get_inner().kind, TendermintInitErrorKind::DerivationPathMissing),
        "{err}"
    );
    assert!(err.to_string().contains("derivation_path"), "{err}");
}

/// A node that serves the block height, the account, the simulation and the balance a withdrawal asks for.
fn script_withdraw_node(node: &FakeNode, balance_sat: u64) {
    let account = QueryAccountResponse {
        account: Some(Any {
            type_url: "/cosmos.auth.v1beta1.BaseAccount".to_owned(),
            value: BaseAccount {
                account_number: ACCOUNT_NUMBER,
                sequence: 1,
                ..Default::default()
            }
            .encode_to_vec(),
        }),
    }
    .encode_to_vec();
    let simulation = SimulateResponse {
        gas_info: Some(GasInfo {
            gas_wanted: 125_000,
            gas_used: 80_000,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let balance = QueryBalanceResponse {
        balance: Some(CoinProto {
            denom: "uatom".to_owned(),
            amount: balance_sat.to_string(),
        }),
    }
    .encode_to_vec();
    node.answer(move |method, params| match method {
        "abci_info" => Some(json!({
            "response": {
                "data": "test",
                "version": "0",
                "app_version": "1",
                "last_block_height": "100",
                "last_block_app_hash": ""
            }
        })),
        "abci_query" if params["path"] == "/cosmos.auth.v1beta1.Query/Account" => Some(abci_value(&account)),
        "abci_query" if params["path"] == "/cosmos.tx.v1beta1.Service/Simulate" => Some(abci_value(&simulation)),
        "abci_query" if params["path"] == "/cosmos.bank.v1beta1.Query/Balance" => Some(abci_value(&balance)),
        _ => None,
    });
}

/// Whether the first signature of the encoded transaction verifies, over the sign document of the chain and
/// account number, against the public key.
fn tx_signature_verifies(tx_bytes: &[u8], chain_id: &str, account_number: u64, public_key: &[u8]) -> bool {
    let raw = TxRaw::decode(tx_bytes).expect("a transaction");
    let sign_doc = SignDocProto {
        body_bytes: raw.body_bytes.clone(),
        auth_info_bytes: raw.auth_info_bytes.clone(),
        chain_id: chain_id.to_owned(),
        account_number,
    };
    let digest = Sha256::digest(&sign_doc.encode_to_vec());
    let message = secp256k1::Message::from_digest_slice(digest.as_slice()).expect("a 32-byte digest");
    let signature = secp256k1::ecdsa::Signature::from_compact(&raw.signatures[0]).expect("a compact signature");
    let public_key = secp256k1::PublicKey::from_slice(public_key).expect("a public key");
    secp256k1::Secp256k1::verification_only()
        .verify_ecdsa(&message, &signature, &public_key)
        .is_ok()
}

#[test]
fn tm_act_hd_withdrawal_is_signed_with_the_key_at_the_path() {
    // CRD ch.36 §36.7 code-quality finding: a withdrawal of a coin activated at a non-default path is signed with
    // the key of that path and verifies against that path's public key, not against the wallet's root public key
    // nor against the key of the default path.
    let ctx = hd_login_ctx();
    let node = FakeNode::start();
    script_withdraw_node(&node, 10_000_000);
    let path_to_address = PathToAddress {
        account_id: 1,
        chain: Bip44Chain::External,
        address_id: 4,
    };
    let coin = hd_coin(&ctx, &node, &path_to_address);

    let to = AccountId::new("cosmos", dhash160(&pubkey(&PEER_KEY)).as_slice())
        .expect("a valid account id")
        .to_string();
    let request: WithdrawRequest =
        serde_json::from_value(json!({ "coin": "ATOM", "to": to, "amount": "1" })).expect("a withdraw request");
    let details =
        common::block_on(coin.withdraw(request).compat()).unwrap_or_else(|e| panic!("the withdrawal failed: {}", e));

    let own_address = address_of_secret(&independent_secret("m/44'/118'/1'/0/4"), "cosmos");
    assert_eq!(details.from, vec![own_address]);
    assert_eq!(details.coin, "ATOM");

    let chain_id = "cosmoshub-4";
    let tx_bytes = details.tx_hex.0.clone();
    let path_key = pubkey(&independent_secret("m/44'/118'/1'/0/4"));
    assert!(
        tx_signature_verifies(&tx_bytes, chain_id, ACCOUNT_NUMBER, &path_key),
        "the transaction is signed with the key of the path"
    );
    for (other, path) in [
        (pubkey(&independent_secret("m/44'/118'/0'/0/0")), "the default path"),
        (pubkey(&independent_secret("m")), "the wallet root"),
    ] {
        assert!(
            !tx_signature_verifies(&tx_bytes, chain_id, ACCOUNT_NUMBER, &other),
            "the signature must not verify against {path}"
        );
    }
}
