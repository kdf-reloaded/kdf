//! Tests of the TRON platform on the mmrpc 2.0 route (CRD ch.21 R21.10B.2, R21.10B.4, R21.10B.5, T21.10B.1 --
//! T21.10B.4, T21.10B.9, D21.10B.3 (b), F21.B3, D21.B7).
//!
//! The activation routines run end to end against a loopback stub of a TRON node. The build switch
//! `TRON_ACTIVATION_AVAILABLE` stays `false`; a context opens the gate for itself with [`TRON_TEST_OVERRIDE_KEY`].

use super::*;
use crate::erc20_token_activation::{enable_evm_token, EnableEvmTokenRequest, EvmTokenActivationResult};
use crate::init_platform_coin_with_tokens::{init_platform_coin_with_tokens, init_platform_coin_with_tokens_status};
use crate::init_token::{init_token, init_token_status};
use crate::token::EnableTokenError;
use coins::{lp_coinfind, MmCoin as _};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmCtxBuilder;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const USDT_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

type Req = EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest>;

// ---------------------------------------------------------------------------------------------------------------
// A loopback stand-in for a TRON node
// ---------------------------------------------------------------------------------------------------------------

type Handler = dyn Fn(&str, &str) -> (u16, String) + Send + Sync;

/// A running stub node: `handler(path, body)` answers every request. Its thread stops when it is dropped.
struct TronStub {
    url: String,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<(String, String)>>>,
}

impl Drop for TronStub {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

fn serve(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<(String, String)>>) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while raw.len() < head_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&raw[head_end..]).into_owned();
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((path.clone(), body.clone()));
    let (status, answer) = handler(&path, &body);
    let response = format!(
        "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.flush().ok();
}

impl TronStub {
    fn start(handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
        let port = listener.local_addr().expect("local address").port();
        listener.set_nonblocking(true).expect("non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let log: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let (stop_thread, log_thread) = (stop.clone(), log.clone());
        std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        let (handler, log) = (handler.clone(), log_thread.clone());
                        std::thread::spawn(move || serve(stream, handler.as_ref(), &log));
                    },
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    },
                    Err(_) => break,
                }
            }
        });
        TronStub {
            url: format!("http://127.0.0.1:{port}"),
            stop,
            log,
        }
    }

    /// The number of requests the node received, at any path.
    fn request_count(&self) -> usize { self.log.lock().unwrap_or_else(|e| e.into_inner()).len() }

    /// The number of requests to `path`.
    fn count(&self, path: &str) -> usize {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(p, _)| p == path)
            .count()
    }
}

/// A 32-byte ABI word holding `value`.
fn word(value: u128) -> String { format!("{value:064x}") }

/// A healthy node: head block 4242, an account of 2.5 TRX, `decimals()` of 6 and a token balance of 1_234_567.
fn healthy(path: &str, body: &str) -> (u16, String) { healthy_with(path, body, true) }

/// As [`healthy`]; `decimals_ok` false makes the `decimals()` read fail with HTTP 500.
fn healthy_with(path: &str, body: &str, decimals_ok: bool) -> (u16, String) {
    match path {
        "/wallet/getnowblock" => (
            200,
            json!({
                "blockID": "ab".repeat(32),
                "block_header": { "raw_data": { "number": 4242, "timestamp": 1_700_000_000_000i64 } }
            })
            .to_string(),
        ),
        "/wallet/getaccount" => (200, json!({ "balance": 2_500_000, "create_time": 1 }).to_string()),
        "/wallet/triggerconstantcontract" => {
            let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
            match request["function_selector"].as_str() {
                Some("decimals()") if decimals_ok => (200, json!({ "constant_result": [word(6)] }).to_string()),
                Some("decimals()") => (500, "{}".to_owned()),
                Some("balanceOf(address)") => (200, json!({ "constant_result": [word(1_234_567)] }).to_string()),
                _ => (404, "{}".to_owned()),
            }
        },
        _ => (404, "{}".to_owned()),
    }
}

/// A node that never gets a request in the tests that use it: any request is answered by the healthy script, and
/// the test asserts the request count.
fn untouched_stub() -> TronStub { TronStub::start(healthy) }

// ---------------------------------------------------------------------------------------------------------------
// Contexts and requests
// ---------------------------------------------------------------------------------------------------------------

fn coins_conf() -> Json {
    json!([
        {
            "coin": "TRX",
            "name": "tron",
            "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
            "wallet_only": true,
            "required_confirmations": 2,
            "mm2": 1
        },
        {
            "coin": "TRXT",
            "name": "tron-trading",
            "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
            "mm2": 1
        },
        {
            "coin": "ETH",
            "name": "ethereum",
            "chain_id": 1,
            "protocol": { "type": "ETH", "protocol_data": { "chain_id": 1 } },
            "mm2": 1
        },
        {
            "coin": "USDT-TRC20",
            "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": USDT_CONTRACT } },
            "decimals": 6,
            "required_confirmations": 4,
            "mm2": 1
        },
        {
            "coin": "NODEC-TRC20",
            "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": USDT_CONTRACT } },
            "mm2": 1
        },
        {
            "coin": "BAD-TRC20",
            "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": "not-an-address" } },
            "decimals": 6,
            "mm2": 1
        },
        {
            "coin": "ETHSIDE-TRC20",
            "protocol": { "type": "TRC20", "protocol_data": { "platform": "ETH", "contract_address": USDT_CONTRACT } },
            "decimals": 6,
            "mm2": 1
        },
        {
            "coin": "WRONG-ERC20",
            "protocol": {
                "type": "ERC20",
                "protocol_data": { "platform": "TRX", "contract_address": "0xdAC17F958D2ee523a2206206994597C13D831ec7" }
            },
            "decimals": 6,
            "mm2": 1
        }
    ])
}

/// A single-key context whose TRON gate is open.
fn tron_ctx() -> MmArc { tron_ctx_with(json!({ (TRON_TEST_OVERRIDE_KEY): true })) }

/// A single-key context with the `extra` top-level `MM2.json` settings.
fn tron_ctx_with(extra: Json) -> MmArc {
    let mut conf = json!({ "coins": coins_conf() });
    if let (Some(conf), Some(extra)) = (conf.as_object_mut(), extra.as_object()) {
        conf.extend(extra.clone());
    }
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "tron activation test passphrase")
        .expect("initialize the crypto context");
    ctx
}

/// A global-HD context whose TRON gate is open.
fn tron_hd_ctx() -> MmArc {
    let conf = json!({ "enable_hd": true, "coins": coins_conf(), (TRON_TEST_OVERRIDE_KEY): true });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).expect("initialize the crypto context");
    ctx
}

fn request_json(ticker: &str, node_urls: &[String], extra: Json) -> Json {
    let nodes: Vec<Json> = node_urls.iter().map(|url| json!({ "url": url })).collect();
    let mut request = json!({ "ticker": ticker, "nodes": nodes, "erc20_tokens_requests": [] });
    for (key, value) in extra.as_object().expect("an object").iter() {
        if value.is_null() {
            request.as_object_mut().expect("an object").remove(key);
        } else {
            request[key] = value.clone();
        }
    }
    request
}

fn request(ticker: &str, node_urls: &[String], extra: Json) -> Req {
    serde_json::from_value(request_json(ticker, node_urls, extra)).unwrap_or_else(|e| panic!("bad request: {e}"))
}

fn activate(
    ctx: &MmArc,
    req: Req,
) -> Result<EthWithTokensActivationResult, MmError<EnablePlatformCoinWithTokensError>> {
    block_on(enable_platform_coin_with_tokens::<EthCoin>(ctx.clone(), req))
}

fn activated(ctx: &MmArc, req: Req) -> Json {
    match activate(ctx, req) {
        Ok(result) => serde_json::to_value(&result).expect("a serializable result"),
        Err(e) => panic!("the activation failed: {e}"),
    }
}

/// The error as the dispatcher serializes it, and the HTTP status.
fn failure(ctx: &MmArc, req: Req) -> (Json, StatusCode) {
    match activate(ctx, req) {
        Ok(result) => panic!(
            "expected a failure, activated: {}",
            serde_json::to_value(&result).unwrap()
        ),
        Err(e) => (
            serde_json::to_value(e.get_inner()).expect("a serializable error"),
            e.get_inner().status_code(),
        ),
    }
}

fn assert_error(wire: &(Json, StatusCode), error_type: &str, status: StatusCode) {
    assert_eq!(wire.0["error_type"], error_type, "{:?}", wire.0);
    assert_eq!(wire.1, status, "{:?}", wire.0);
}

fn tokens(tickers: &[&str]) -> Json { Json::Array(tickers.iter().map(|ticker| json!({ "ticker": ticker })).collect()) }

fn registered(ctx: &MmArc, ticker: &str) -> EthCoin {
    match block_on(lp_coinfind(ctx, ticker)).unwrap().expect("the coin is active") {
        coins::MmCoinEnum::EthCoin(coin) => coin,
        other => panic!("not an EVM-family coin: {}", other.ticker()),
    }
}

fn is_registered(ctx: &MmArc, ticker: &str) -> bool { block_on(lp_coinfind(ctx, ticker)).unwrap().is_some() }

fn poll_terminal(ctx: &MmArc, task_id: u64) -> Json {
    for _ in 0..400 {
        let status = block_on(init_platform_coin_with_tokens_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": task_id, "forget_if_finished": false }))
                .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {e}"));
        let status = serde_json::to_value(&status).unwrap();
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            return status;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the task did not finish");
}

fn run_task(ctx: &MmArc, req: Req) -> Json {
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(ctx.clone(), req))
        .unwrap_or_else(|e| panic!("init failed: {e}"));
    poll_terminal(ctx, response.task_id)
}

fn token_request(ticker: &str) -> EnableEvmTokenRequest {
    serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).expect("a token request")
}

fn enable_token(ctx: &MmArc, ticker: &str) -> Result<EvmTokenActivationResult, MmError<EnableTokenError>> {
    block_on(enable_evm_token(ctx.clone(), token_request(ticker)))
}

fn token_failure(ctx: &MmArc, ticker: &str) -> (Json, StatusCode) {
    match enable_token(ctx, ticker) {
        Ok(result) => panic!("expected a failure, got {}", serde_json::to_value(&result).unwrap()),
        Err(e) => (
            serde_json::to_value(e.get_inner()).expect("a serializable error"),
            e.get_inner().status_code(),
        ),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------------------------

/// While `TRON_ACTIVATION_AVAILABLE` is `false` and no override is set, the mmrpc route answers the refusal of the
/// legacy routes and registers nothing; no node is contacted. The inner routine below the gate is what the other
/// tests of this module run.
#[test]
fn the_gate_refuses_every_mmrpc_route_while_tron_is_switched_off() {
    const { assert!(!TRON_ACTIVATION_AVAILABLE, "the build switch stays off") };
    let node = untouched_stub();
    let ctx = tron_ctx_with(json!({}));
    assert!(!tron_activation_available(&ctx));

    // The platform route, with and without tokens.
    for extra in [json!({}), json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) })] {
        let (body, status) = failure(&ctx, request("TRX", std::slice::from_ref(&node.url), extra));
        assert_eq!(body["error_type"], "PlatformCoinCreationError", "{body}");
        assert_eq!(body["error_data"]["error"], TRON_NOT_AVAILABLE, "{body}");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
    // The single-token route, and the token task.
    let (body, status) = token_failure(&ctx, "USDT-TRC20");
    assert_eq!(body["error_type"], "Internal", "{body}");
    assert_eq!(body["error_data"], TRON_NOT_AVAILABLE, "{body}");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let init = serde_json::from_value(json!({ "ticker": "USDT-TRC20", "activation_params": {} })).unwrap();
    match block_on(init_token::<EthCoin>(ctx.clone(), init)) {
        Ok(_) => panic!("the token task must not start"),
        Err(e) => {
            assert_eq!(serde_json::to_value(e.get_inner()).unwrap()["error_type"], "Internal");
            assert_eq!(e.get_inner().status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        },
    }
    assert_eq!(node.request_count(), 0, "a refused route contacts no node");
    assert!(!is_registered(&ctx, "TRX") && !is_registered(&ctx, "USDT-TRC20"));
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.1, R21.10B.5: the result
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t21_10b_1_the_one_shot_result_is_keyed_by_the_base58_address() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
        ),
    );
    let coin = registered(&ctx, "TRX");
    let address = coin.my_address().expect("an address");
    assert!(address.starts_with('T') && address.len() == 34, "{address}");

    assert_eq!(result["current_block"], 4242, "the head block of the node");
    assert_eq!(result["nfts_infos"], json!({}));
    let trx = &result["eth_addresses_infos"][&address];
    assert_eq!(trx["derivation_method"], json!({ "type": "Iguana" }), "{result}");
    assert_eq!(
        trx["balances"],
        json!({ "spendable": "2.5", "unspendable": "0" }),
        "6 decimals"
    );
    let pubkey = trx["pubkey"].as_str().expect("a pubkey");
    assert!(pubkey.starts_with("04") && pubkey.len() == 130, "{pubkey}");
    assert_eq!(
        coins::eth::tron::addr_from_pubkey_str(pubkey).expect("the address of the key"),
        address,
        "the address is the one of the reported key"
    );
    let tokens_entry = &result["erc20_addresses_infos"][&address];
    assert_eq!(tokens_entry["derivation_method"], json!({ "type": "Iguana" }));
    assert_eq!(
        tokens_entry["balances"]["USDT-TRC20"],
        json!({ "spendable": "1.234567", "unspendable": "0" }),
        "the token's own decimals"
    );
    assert_eq!(result["eth_addresses_infos"].as_object().map(|map| map.len()), Some(1));

    // The platform and the token are registered; the token is recorded on its platform.
    assert!(is_registered(&ctx, "USDT-TRC20"));
    assert!(coin.get_erc20_tokens_infos().contains_key("USDT-TRC20"));
    assert_eq!(
        registered(&ctx, "USDT-TRC20").required_confirmations(),
        4,
        "the token's own config"
    );
    assert_eq!(
        coin.required_confirmations(),
        2,
        "the platform config, not the EVM default of 3"
    );
}

#[test]
fn get_balances_false_lists_the_tickers_and_reads_no_balance() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({
                "erc20_tokens_requests": tokens(&["USDT-TRC20"]),
                "get_balances": false
            }),
        ),
    );
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert!(
        result["eth_addresses_infos"][&address].get("balances").is_none(),
        "{result}"
    );
    assert_eq!(
        result["erc20_addresses_infos"][&address]["tickers"],
        json!(["USDT-TRC20"])
    );
    assert!(result["erc20_addresses_infos"][&address].get("balances").is_none());
    assert_eq!(node.count("/wallet/getaccount"), 0);
}

/// The task form delivers the same result in its status.
#[test]
fn the_task_form_delivers_the_same_result() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    let status = run_task(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
        ),
    );
    assert_eq!(status["status"], "Ok", "{status}");
    let details = &status["details"];
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(details["current_block"], 4242);
    assert_eq!(details["eth_addresses_infos"][&address]["balances"]["spendable"], "2.5");
    assert_eq!(
        details["erc20_addresses_infos"][&address]["balances"]["USDT-TRC20"]["spendable"],
        "1.234567"
    );
}

/// `required_confirmations`: the request, else the coin config, else 1; a token's request value overrides its config.
#[test]
fn required_confirmations_follow_the_request_then_the_config_then_one() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "required_confirmations": 7 }),
        ),
    );
    assert_eq!(registered(&ctx, "TRX").required_confirmations(), 7);

    let ctx = tron_ctx();
    activated(
        &ctx,
        request(
            "TRXT",
            std::slice::from_ref(&node.url),
            json!({
                "swap_contract_address": USDT_CONTRACT
            }),
        ),
    );
    assert_eq!(
        registered(&ctx, "TRXT").required_confirmations(),
        1,
        "no request value, no config value"
    );

    let ctx = tron_ctx();
    activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({
                "erc20_tokens_requests": [{ "ticker": "USDT-TRC20", "required_confirmations": 9 }, { "ticker": "NODEC-TRC20" }]
            }),
        ),
    );
    assert_eq!(
        registered(&ctx, "USDT-TRC20").required_confirmations(),
        9,
        "the token's request value"
    );
    assert_eq!(
        registered(&ctx, "NODEC-TRC20").required_confirmations(),
        2,
        "the platform's effective value"
    );
}

/// `tx_history` is ignored: no history process starts and the coin reports no history state.
#[test]
fn tx_history_is_ignored() {
    let node = TronStub::start(healthy);
    // The context has no SQL database: a history start would fail on it.
    let ctx = tron_ctx();
    activated(
        &ctx,
        request("TRX", std::slice::from_ref(&node.url), json!({ "tx_history": true })),
    );
    let coin = registered(&ctx, "TRX");
    assert!(matches!(
        coin.history_sync_status(),
        coins::HistorySyncState::NotEnabled
    ));
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.2: the NFT request
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t21_10b_2_an_nft_request_is_internal_before_any_key_or_node_work() {
    let node = untouched_stub();
    let ctx = tron_ctx();
    let nft_req = json!({ "provider": { "type": "Moralis", "info": { "url": "https://indexer.example.com/" } } });
    let wire = failure(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({
                "erc20_tokens_requests": tokens(&["USDT-TRC20"]),
                "nft_req": nft_req.clone()
            }),
        ),
    );
    assert_error(&wire, "Internal", StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(node.request_count(), 0);
    assert!(!is_registered(&ctx, "TRX") && !is_registered(&ctx, "USDT-TRC20"));

    // The same token in the task status.
    let status = run_task(
        &ctx,
        request("TRX", std::slice::from_ref(&node.url), json!({ "nft_req": nft_req })),
    );
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "Internal");
    assert_eq!(node.request_count(), 0);
    assert!(!is_registered(&ctx, "TRX"));
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.3: a token without decimals
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t21_10b_3_a_token_without_decimals_reads_them_and_a_failed_read_is_transport() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["NODEC-TRC20"]) }),
        ),
    );
    assert_eq!(
        registered(&ctx, "NODEC-TRC20").decimals(),
        6,
        "the answer of `decimals()`"
    );
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(
        result["erc20_addresses_infos"][&address]["balances"]["NODEC-TRC20"]["spendable"],
        "1.234567"
    );

    // A stub that fails the read: `Transport`, HTTP 502, and nothing registered.
    let failing = TronStub::start(|path, body| healthy_with(path, body, false));
    let ctx = tron_ctx();
    let wire = failure(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&failing.url),
            json!({ "erc20_tokens_requests": tokens(&["NODEC-TRC20"]) }),
        ),
    );
    assert_error(&wire, "Transport", StatusCode::BAD_GATEWAY);
    assert!(!is_registered(&ctx, "TRX") && !is_registered(&ctx, "NODEC-TRC20"));

    // A value above 255 is the same.
    let huge = TronStub::start(|path, body| {
        if path == "/wallet/triggerconstantcontract" && body.contains("decimals()") {
            (200, json!({ "constant_result": [word(256)] }).to_string())
        } else {
            healthy(path, body)
        }
    });
    let ctx = tron_ctx();
    let wire = failure(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&huge.url),
            json!({ "erc20_tokens_requests": tokens(&["NODEC-TRC20"]) }),
        ),
    );
    assert_error(&wire, "Transport", StatusCode::BAD_GATEWAY);
}

// ---------------------------------------------------------------------------------------------------------------
// Node failure at activation
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn no_node_yielding_the_current_block_is_internal_and_a_failed_balance_read_is_transport() {
    // No head block: `Internal`, HTTP 500.
    let no_block = TronStub::start(|path, body| {
        if path == "/wallet/getnowblock" {
            (500, "{}".to_owned())
        } else {
            healthy(path, body)
        }
    });
    let ctx = tron_ctx();
    let wire = failure(&ctx, request("TRX", std::slice::from_ref(&no_block.url), json!({})));
    assert_error(&wire, "Internal", StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!is_registered(&ctx, "TRX"));

    // A failed balance read: `Transport`, HTTP 502.
    let no_balance = TronStub::start(|path, body| {
        if path == "/wallet/getaccount" {
            (500, "{}".to_owned())
        } else {
            healthy(path, body)
        }
    });
    let ctx = tron_ctx();
    let wire = failure(&ctx, request("TRX", std::slice::from_ref(&no_balance.url), json!({})));
    assert_error(&wire, "Transport", StatusCode::BAD_GATEWAY);
    assert!(!is_registered(&ctx, "TRX"));
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.4: the node list
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn t21_10b_4_the_node_list_is_checked_without_contacting_a_node() {
    let node = untouched_stub();
    let ctx = tron_ctx();

    // An empty list and an absent list.
    let wire = failure(&ctx, request("TRX", &[], json!({})));
    assert_error(&wire, "AtLeastOneNodeRequired", StatusCode::BAD_REQUEST);
    let wire = failure(&ctx, request("TRX", &[], json!({ "nodes": null })));
    assert_error(&wire, "AtLeastOneNodeRequired", StatusCode::BAD_REQUEST);

    // A scheme other than http and https, and a URL that does not parse. The text does not echo the URL.
    for url in [
        "ws://SECRETHOST.example",
        "wss://SECRETHOST.example",
        "ftp://SECRETHOST.example",
        "https://user:SECRETPW@bad host/APIKEY",
    ] {
        let wire = failure(
            &ctx,
            request("TRX", &["http://127.0.0.1:1".to_owned(), url.to_owned()], json!({})),
        );
        assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
        let text = wire.0.to_string();
        for secret in ["SECRET", "APIKEY"] {
            assert!(!text.contains(secret), "{text} echoes a configured string");
        }
    }
    assert_eq!(node.request_count(), 0);
    assert!(!is_registered(&ctx, "TRX"));
}

/// The per-node `headers` object and the `tron_api_key` apply to the entries of `nodes` (CRD ch.21 R-N9).
#[test]
fn node_entries_take_headers_and_refuse_bad_ones_without_echoing_them() {
    let ctx = tron_ctx_with(json!({ "tron_api_key": "KEY-SECRET" }));
    let req = request_json(
        "TRX",
        &[],
        json!({
            "nodes": [
                { "url": "http://127.0.0.1:1", "komodo_proxy": true },
                { "url": "http://127.0.0.1:2", "headers": { "X-Token": "HDRSECRET-1" } }
            ]
        }),
    );
    let req: Req = serde_json::from_value(req).expect("a request");
    let nodes = tron_nodes(&ctx, &req.request).expect("both entries parse");
    assert_eq!(nodes.len(), 2);
    assert!(
        !format!("{:?}", req.request).contains("HDRSECRET"),
        "a header value is never rendered"
    );

    for headers in [
        json!({ "bad name": "v" }),
        json!({ "X-A": "SECRET\nVALUE" }),
        json!({ "X-A": 7 }),
        json!(["list"]),
    ] {
        let req = request_json(
            "TRX",
            &[],
            json!({ "nodes": [{ "url": "http://127.0.0.1:1", "headers": headers }] }),
        );
        let req: Req = serde_json::from_value(req).expect("a request");
        let err = tron_nodes(&ctx, &req.request).expect_err("refused");
        assert!(matches!(err.get_inner(), ActivationErr::InvalidPayload(_)), "{err}");
        let text = err.to_string();
        for secret in ["SECRET", "VALUE", "bad name"] {
            assert!(!text.contains(secret), "{text} echoes a configured string");
        }
    }
    // An invalid `tron_api_key` is a configuration failure of the node, not of the payload.
    let ctx = tron_ctx_with(json!({ "tron_api_key": "KEY\nSECRET" }));
    let req: Req = serde_json::from_value(request_json("TRX", &["http://127.0.0.1:1".to_owned()], json!({}))).unwrap();
    let err = tron_nodes(&ctx, &req.request).expect_err("refused");
    assert!(matches!(err.get_inner(), ActivationErr::Internal(_)));
    assert!(!err.to_string().contains("SECRET"));
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.9, D21.10B.3 (b): key policies
// ---------------------------------------------------------------------------------------------------------------

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn t21_10b_9_trezor_is_unexpected_device_activation_policy_on_the_one_shot_route() {
    let node = untouched_stub();
    let ctx = tron_ctx();
    let wire = failure(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "priv_key_policy": { "type": "Trezor" } }),
        ),
    );
    assert_error(&wire, "UnexpectedDeviceActivationPolicy", StatusCode::BAD_REQUEST);
    assert_eq!(node.request_count(), 0);
}

/// On the task route the Trezor policy ends with `Internal` in the status, before any device exchange: the context
/// has no hardware context, and the failure is that of the TRON rule, not of a missing device.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn t21_10b_9_trezor_on_the_task_route_is_internal_before_any_device_exchange() {
    let node = untouched_stub();
    let ctx = tron_ctx();
    let status = run_task(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "priv_key_policy": { "type": "Trezor" } }),
        ),
    );
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "Internal");
    assert!(
        status["details"]["error_data"]
            .as_str()
            .is_some_and(|text| text.contains("TRON")),
        "{status}"
    );
    assert_eq!(node.request_count(), 0);
    assert!(!is_registered(&ctx, "TRX"));
}

#[test]
fn t21_10b_9_walletconnect_is_internal_on_both_routes() {
    let node = untouched_stub();
    let ctx = tron_ctx();
    let policy = json!({ "priv_key_policy": { "type": "WalletConnect", "params": { "session_topic": "topic" } } });
    let wire = failure(&ctx, request("TRX", std::slice::from_ref(&node.url), policy.clone()));
    assert_error(&wire, "Internal", StatusCode::INTERNAL_SERVER_ERROR);
    let status = run_task(&ctx, request("TRX", std::slice::from_ref(&node.url), policy));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "Internal");
    assert_eq!(node.request_count(), 0);
}

/// D21.10B.3 (b): the MetaMask policy and the MetaMask `rpc_mode` are refused with the route's `Internal`. They
/// exist only on the browser build, so the rule is tested on its facts.
#[test]
fn d21_10b_3_the_metamask_policy_and_rpc_mode_are_refused_with_internal() {
    for facts in [
        KeyPolicyFacts {
            metamask_policy: true,
            ..Default::default()
        },
        KeyPolicyFacts {
            metamask_rpc_mode: true,
            ..Default::default()
        },
        KeyPolicyFacts {
            metamask_policy: true,
            metamask_rpc_mode: true,
            ..Default::default()
        },
    ] {
        let refusal = key_policy_refusal(facts, false).expect("refused");
        assert_eq!(refusal, KeyPolicyRefusal::MetaMask);
        match refusal.into_error() {
            ActivationErr::Internal(text) => assert!(text.contains("MetaMask"), "{text}"),
            other => panic!("expected Internal, got {other}"),
        }
        // Whatever the login.
        assert_eq!(key_policy_refusal(facts, true), Some(KeyPolicyRefusal::MetaMask));
    }
    // The other policies and no policy.
    assert_eq!(key_policy_refusal(KeyPolicyFacts::default(), false), None);
    assert_eq!(
        key_policy_refusal(
            KeyPolicyFacts {
                wallet_connect: true,
                ..Default::default()
            },
            false
        ),
        Some(KeyPolicyRefusal::WalletConnect)
    );
    assert_eq!(
        key_policy_refusal(
            KeyPolicyFacts {
                trezor: true,
                ..Default::default()
            },
            false
        ),
        Some(KeyPolicyRefusal::Trezor)
    );
    // The facts of a request that names none of them.
    let req: Req = serde_json::from_value(request_json("TRX", &[], json!({}))).unwrap();
    assert_eq!(req.request.key_policy_facts(), KeyPolicyFacts::default());
}

/// The interim rule of this change: under a global-HD login the default and the `ContextPrivKey` policies are
/// refused with a typed error naming TRON HD as not yet available, before any node is contacted.
#[test]
fn an_hd_login_is_refused_for_the_default_and_context_policies_until_the_tron_hd_wallet_is_built() {
    let node = untouched_stub();
    let ctx = tron_hd_ctx();
    for extra in [json!({}), json!({ "priv_key_policy": { "type": "ContextPrivKey" } })] {
        let (body, status) = failure(&ctx, request("TRX", std::slice::from_ref(&node.url), extra.clone()));
        assert_eq!(body["error_type"], "PrivKeyPolicyNotAllowed", "{extra}: {body}");
        assert!(
            body["error_data"]
                .as_str()
                .is_some_and(|text| text.contains("not yet available")),
            "{body}"
        );
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
    assert_eq!(node.request_count(), 0);
    assert!(!is_registered(&ctx, "TRX"));
    // The rule on its facts: only `IguanaPrivKey` passes an HD login.
    let iguana = KeyPolicyFacts {
        iguana_priv_key: true,
        ..Default::default()
    };
    assert_eq!(
        key_policy_refusal(KeyPolicyFacts::default(), true),
        Some(KeyPolicyRefusal::HdLogin)
    );
    assert_eq!(key_policy_refusal(iguana, true), None);
    assert_eq!(key_policy_refusal(iguana, false), None);
}

/// Under a global-HD login `IguanaPrivKey` is honored: the coin uses the node's internal key, the key the UTXO
/// family uses for the same policy, on both routes. The reference is `CryptoCtx::mm2_internal_pubkey_hex`.
#[test]
fn iguana_priv_key_under_an_hd_login_uses_the_internal_key() {
    let node = TronStub::start(healthy);
    let ctx = tron_hd_ctx();
    let internal_pubkey = crypto::CryptoCtx::from_ctx(&ctx).unwrap().mm2_internal_pubkey_hex();
    let expected = coins::eth::tron::addr_from_pubkey_str(&internal_pubkey).expect("the TRON address of the key");
    let policy = json!({ "priv_key_policy": { "type": "IguanaPrivKey" } });

    let result = activated(&ctx, request("TRX", std::slice::from_ref(&node.url), policy.clone()));
    let coin = registered(&ctx, "TRX");
    assert_eq!(coin.my_address().expect("an address"), expected);
    assert!(matches!(coin.derivation_method, coins::DerivationMethod::Iguana(_)));
    let pubkey = result["eth_addresses_infos"][&expected]["pubkey"]
        .as_str()
        .expect("a pubkey")
        .to_owned();
    assert!(pubkey.starts_with("04") && pubkey.len() == 130, "{pubkey}");
    assert_eq!(
        coins::eth::tron::addr_from_pubkey_str(&pubkey).expect("address"),
        expected
    );
    assert_eq!(
        result["eth_addresses_infos"][&expected]["derivation_method"],
        json!({ "type": "Iguana" })
    );

    // The task route.
    let ctx = tron_hd_ctx();
    let status = run_task(&ctx, request("TRX", std::slice::from_ref(&node.url), policy));
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(registered(&ctx, "TRX").my_address().expect("an address"), expected);
}

/// Under an iguana login the policies behave as before: no policy, `ContextPrivKey` and `IguanaPrivKey` all
/// activate the single key.
#[test]
fn under_an_iguana_login_every_single_key_policy_activates() {
    let node = TronStub::start(healthy);
    let mut addresses = Vec::new();
    for extra in [
        json!({}),
        json!({ "priv_key_policy": { "type": "ContextPrivKey" } }),
        json!({ "priv_key_policy": { "type": "IguanaPrivKey" } }),
    ] {
        let ctx = tron_ctx();
        activated(&ctx, request("TRX", std::slice::from_ref(&node.url), extra));
        addresses.push(registered(&ctx, "TRX").my_address().expect("an address"));
    }
    assert!(addresses.windows(2).all(|pair| pair[0] == pair[1]));
}

// ---------------------------------------------------------------------------------------------------------------
// F21.B3, D21.B7: the swap contracts
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn swap_contracts_follow_the_wallet_only_rule_and_take_both_tron_forms() {
    let node = TronStub::start(healthy);
    let zero = TronAddress::from_evm_address(Address::default());
    let contract = TronAddress::parse(USDT_CONTRACT).expect("valid");
    let any_form = [
        USDT_CONTRACT.to_owned(),
        contract.to_hex(),
        format!("0x{}", contract.to_hex()),
        format!("0x{}", hex::encode(contract.to_evm_address())),
    ];

    // A wallet-only coin ignores them: absent, zero, even a member that is not a usable address once decoded.
    for extra in [
        json!({}),
        json!({ "swap_contract_address": zero.to_base58() }),
        json!({ "swap_contract_address": USDT_CONTRACT, "fallback_swap_contract": zero.to_base58() }),
    ] {
        let ctx = tron_ctx();
        activated(&ctx, request("TRX", std::slice::from_ref(&node.url), extra));
    }

    // A trading coin needs `swap_contract_address`, non-zero, in either form; the fallback is optional, non-zero.
    let ctx = tron_ctx();
    let wire = failure(&ctx, request("TRXT", std::slice::from_ref(&node.url), json!({})));
    assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    for form in [zero.to_base58(), zero.to_hex()] {
        let wire = failure(
            &ctx,
            request(
                "TRXT",
                std::slice::from_ref(&node.url),
                json!({ "swap_contract_address": form }),
            ),
        );
        assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    }
    let wire = failure(
        &ctx,
        request(
            "TRXT",
            std::slice::from_ref(&node.url),
            json!({
                "swap_contract_address": USDT_CONTRACT,
                "fallback_swap_contract": zero.to_base58()
            }),
        ),
    );
    assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    for form in any_form {
        let ctx = tron_ctx();
        activated(
            &ctx,
            request(
                "TRXT",
                std::slice::from_ref(&node.url),
                json!({
                    "swap_contract_address": form,
                    "fallback_swap_contract": USDT_CONTRACT
                }),
            ),
        );
        assert!(is_registered(&ctx, "TRXT"), "{form}");
    }

    // The v2 contracts are required while trading protocol v2 is on, and take both forms.
    let ctx = tron_ctx_with(json!({ (TRON_TEST_OVERRIDE_KEY): true, "use_trading_proto_v2": true }));
    let wire = failure(
        &ctx,
        request(
            "TRXT",
            std::slice::from_ref(&node.url),
            json!({
                "swap_contract_address": USDT_CONTRACT
            }),
        ),
    );
    assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    activated(
        &ctx,
        request(
            "TRXT",
            std::slice::from_ref(&node.url),
            json!({
                "swap_contract_address": USDT_CONTRACT,
                "swap_v2_contracts": {
                    "maker_swap_v2_contract": USDT_CONTRACT,
                    "taker_swap_v2_contract": contract.to_hex(),
                    "nft_maker_swap_v2_contract": format!("0x{}", hex::encode(contract.to_evm_address()))
                }
            }),
        ),
    );
    assert!(is_registered(&ctx, "TRXT"));

    // A value that is no address in any form is a request-parse failure, as on an EVM platform.
    assert!(
        serde_json::from_value::<Req>(request_json("TRX", &[], json!({ "swap_contract_address": "0xnothex" })))
            .is_err()
    );
}

/// An EVM platform reads the swap-contract members exactly as before: a TRON form fails request decoding (the
/// dispatcher turns this error into `InvalidRequest`, HTTP 400, as on dev), while a TRON platform passes.
#[test]
fn an_evm_platform_keeps_the_request_decoding_failure_for_the_tron_forms() {
    let ctx = tron_ctx();
    let contract = TronAddress::parse(USDT_CONTRACT).expect("valid");
    for form in [
        USDT_CONTRACT.to_owned(),
        contract.to_hex(),
        format!("0x{}", contract.to_hex()),
    ] {
        for members in [
            json!({ "swap_contract_address": form }),
            json!({ "fallback_swap_contract": form }),
            json!({ "swap_v2_contracts": {
                "maker_swap_v2_contract": form, "taker_swap_v2_contract": form, "nft_maker_swap_v2_contract": form
            } }),
        ] {
            let mut params = json!({ "ticker": "ETH", "nodes": [], "erc20_tokens_requests": [] });
            params
                .as_object_mut()
                .unwrap()
                .extend(members.as_object().unwrap().clone());
            assert!(evm_swap_contract_members_strict(&ctx, &params).is_err(), "{params}");
            // The same request for a TRON platform passes the check.
            params["ticker"] = json!("TRXT");
            assert!(evm_swap_contract_members_strict(&ctx, &params).is_ok(), "{params}");
        }
    }
    // An EVM form, and an absent member, pass for an EVM platform.
    let evm = format!("0x{}", hex::encode(contract.to_evm_address()));
    let params = json!({ "ticker": "ETH", "swap_contract_address": evm, "fallback_swap_contract": null });
    assert!(evm_swap_contract_members_strict(&ctx, &params).is_ok());
    assert!(evm_swap_contract_members_strict(&ctx, &json!({ "ticker": "ETH" })).is_ok());
}

/// Defense below the decoder: should an EVM platform ever receive a decoded TRON form, activation refuses it.
#[test]
fn an_evm_platform_refuses_the_tron_forms_of_a_swap_contract() {
    let ctx = tron_ctx();
    let contract = TronAddress::parse(USDT_CONTRACT).expect("valid");
    for form in [USDT_CONTRACT.to_owned(), contract.to_hex()] {
        let wire = failure(
            &ctx,
            request(
                "ETH",
                &["http://127.0.0.1:1".to_owned()],
                json!({ "swap_contract_address": form }),
            ),
        );
        assert_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// `enable_erc20` and `task::enable_erc20` with a TRC20 token
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn enable_erc20_activates_a_trc20_token_on_an_active_trx_platform() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    activated(&ctx, request("TRX", std::slice::from_ref(&node.url), json!({})));
    let address = registered(&ctx, "TRX").my_address().expect("an address");

    let result = enable_token(&ctx, "USDT-TRC20").unwrap_or_else(|e| panic!("enable_erc20 failed: {e}"));
    let result = serde_json::to_value(&result).unwrap();
    assert_eq!(result["platform_coin"], "TRX");
    assert_eq!(result["token_contract_address"], USDT_CONTRACT, "base58check");
    assert_eq!(result["required_confirmations"], 4);
    assert_eq!(
        result["balances"][&address],
        json!({ "spendable": "1.234567", "unspendable": "0" })
    );
    assert!(is_registered(&ctx, "USDT-TRC20"));
    assert!(registered(&ctx, "TRX")
        .get_erc20_tokens_infos()
        .contains_key("USDT-TRC20"));

    // A token without `decimals` in its config reads them.
    let result = enable_token(&ctx, "NODEC-TRC20").unwrap_or_else(|e| panic!("enable_erc20 failed: {e}"));
    assert_eq!(
        serde_json::to_value(&result).unwrap()["balances"][&address]["spendable"],
        "1.234567"
    );
    assert_eq!(registered(&ctx, "NODEC-TRC20").decimals(), 6);
}

#[test]
fn enable_erc20_with_a_token_of_the_other_family_is_a_platform_coin_mismatch() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    activated(&ctx, request("TRX", std::slice::from_ref(&node.url), json!({})));
    // An ERC-20 token whose platform is the active TRON platform.
    let wire = token_failure(&ctx, "WRONG-ERC20");
    assert_eq!(wire.0["error_type"], "PlatformCoinMismatch", "{:?}", wire.0);
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
    assert!(!is_registered(&ctx, "WRONG-ERC20"));
    // A TRC20 token whose platform is not active: the usual refusal, not a mismatch.
    let wire = token_failure(&ctx, "ETHSIDE-TRC20");
    assert_eq!(wire.0["error_type"], "PlatformCoinIsNotActivated", "{:?}", wire.0);
    // A TRC20 config with an address that is neither form is a protocol parse failure.
    let wire = token_failure(&ctx, "BAD-TRC20");
    assert_eq!(wire.0["error_type"], "TokenProtocolParseError", "{:?}", wire.0);
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
}

/// In the platform request, a token whose platform is not the requested one, or of the other family, is
/// `PlatformCoinMismatch`, HTTP 400, and nothing is registered.
#[test]
fn a_token_of_another_platform_or_family_in_the_platform_request_is_a_platform_coin_mismatch() {
    let node = TronStub::start(healthy);
    for ticker in ["ETHSIDE-TRC20", "WRONG-ERC20"] {
        let ctx = tron_ctx();
        let wire = failure(
            &ctx,
            request(
                "TRX",
                std::slice::from_ref(&node.url),
                json!({
                    "erc20_tokens_requests": tokens(&["USDT-TRC20", ticker])
                }),
            ),
        );
        assert_error(&wire, "PlatformCoinMismatch", StatusCode::BAD_REQUEST);
        assert!(
            !is_registered(&ctx, "TRX") && !is_registered(&ctx, "USDT-TRC20"),
            "{ticker}"
        );
    }
    // A TRC20 config whose contract is neither form is a protocol parse failure in the platform request too.
    let ctx = tron_ctx();
    let wire = failure(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["BAD-TRC20"]) }),
        ),
    );
    assert_error(&wire, "TokenProtocolParseError", StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn task_enable_erc20_activates_a_trc20_token_and_reports_base58check() {
    let node = TronStub::start(healthy);
    let ctx = tron_ctx();
    activated(&ctx, request("TRX", std::slice::from_ref(&node.url), json!({})));
    let address = registered(&ctx, "TRX").my_address().expect("an address");

    let init = serde_json::from_value(json!({ "ticker": "USDT-TRC20", "activation_params": {} })).unwrap();
    let response = block_on(init_token::<EthCoin>(ctx.clone(), init)).unwrap_or_else(|e| panic!("init failed: {e}"));
    let mut status = Json::Null;
    for _ in 0..400 {
        let polled = block_on(init_token_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({
                "task_id": response.task_id,
                "forget_if_finished": false
            }))
            .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {e}"));
        status = serde_json::to_value(&polled).unwrap();
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(status["status"], "Ok", "{status}");
    let details = &status["details"];
    assert_eq!(details["platform_coin"], "TRX");
    assert_eq!(details["token_contract_address"], USDT_CONTRACT);
    assert_eq!(details["current_block"], 4242);
    assert_eq!(details["wallet_balance"]["wallet_type"], "Iguana");
    assert_eq!(details["wallet_balance"]["address"], address);
    assert_eq!(
        details["wallet_balance"]["balance"]["USDT-TRC20"]["spendable"],
        "1.234567"
    );
    assert!(is_registered(&ctx, "USDT-TRC20"));
}
