//! TRON is switched on as a wallet-only coin family (CRD ch.21 D6, R-D1, R21.B3; ch.55 D55.5): every activation
//! route activates a TRX platform and a TRC20 token through the whole server path, and the coins it registers are
//! wallet-only whatever their coins-config entries say.
//!
//! The routes are the legacy `enable` and `electrum` (a documented Reloaded extension, CRD ch.55 D55.5) and the
//! mmrpc 2.0 `enable_eth_with_tokens`, `task::enable_eth::init`, `enable_erc20` and `task::enable_erc20::init`
//! (CRD ch.21 §21.4). Requests run through `process_single_request` (the legacy table, the fallback, the versioned
//! dispatcher) on the test thread; the node is a loopback stub that answers what an activation reads.

use super::*;
use coins::{lp_coinfind, MmCoin, MmCoinEnum};
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const USDT_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

/// A loopback TRON node: head block 4242, an account of 2.5 TRX, `decimals()` of 6 and a token balance of
/// 1.234567. Its thread stops when the value is dropped.
struct TronStub {
    url: String,
    stop: Arc<AtomicBool>,
}

impl Drop for TronStub {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

fn word(value: u128) -> String { format!("{value:064x}") }

fn answer(path: &str, body: &str) -> (u16, String) {
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
            let request: Json = json::from_str(body).unwrap_or(Json::Null);
            match request["function_selector"].as_str() {
                Some("decimals()") => (200, json!({ "constant_result": [word(6)] }).to_string()),
                Some("balanceOf(address)") => (200, json!({ "constant_result": [word(1_234_567)] }).to_string()),
                _ => (404, "{}".to_owned()),
            }
        },
        _ => (404, "{}".to_owned()),
    }
}

fn serve(mut stream: std::net::TcpStream) {
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
    let (status, reply) = answer(&path, &body);
    let response = format!(
        "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.flush().ok();
}

impl TronStub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
        let port = listener.local_addr().expect("local address").port();
        listener.set_nonblocking(true).expect("non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        std::thread::spawn(move || serve(stream));
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
        }
    }
}

/// A node with a TRX platform and two TRC20 tokens. No entry says `wallet_only`, so each is a trading-capable
/// config as far as the coins file goes.
fn tron_node() -> MmArc {
    let trc20 = json!({ "type": "TRC20", "protocol_data": { "platform": "TRXT", "contract_address": USDT_CONTRACT } });
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "rpc_password": "pw", "coins": [
            { "coin": "TRXT", "mm2": 1, "required_confirmations": 1,
              "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } } },
            { "coin": "USDT-TRC20", "mm2": 1, "decimals": 6, "required_confirmations": 1, "protocol": trc20 },
            { "coin": "USDT2-TRC20", "mm2": 1, "decimals": 6, "required_confirmations": 1, "protocol": trc20 },
        ] }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "tron activation routes test passphrase")
        .expect("initialize the crypto context");
    ctx
}

fn client() -> SocketAddr { SocketAddr::from(([127, 0, 0, 1], 4242)) }

/// Runs a legacy request through the whole server path; a success is HTTP 200 and its body is returned.
fn legacy_success(ctx: &MmArc, method: &str, params: Json) -> Json {
    let mut request = json!({ "userpass": "pw", "method": method });
    for (key, value) in params.as_object().expect("an object") {
        request[key.as_str()] = value.clone();
    }
    let answer = block_on(process_single_request(ctx.clone(), request, client()))
        .unwrap_or_else(|e| panic!("{method} was refused: {e}"));
    assert_eq!(answer.status().as_u16(), 200, "{method}");
    json::from_slice(answer.body()).expect("a JSON body")
}

/// Runs a versioned request through the whole server path; a success is HTTP 200 and its `result` is returned.
fn versioned_result(ctx: &MmArc, method: &str, params: Json) -> Json {
    let request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "params": params, "id": 7 });
    let answer = block_on(process_single_request(ctx.clone(), request, client()))
        .unwrap_or_else(|e| panic!("{method}: the versioned surface answers every request: {e}"));
    let status = answer.status().as_u16();
    let body: Json = json::from_slice(answer.body()).expect("a JSON body");
    assert_eq!(status, 200, "{method}: {body}");
    body["result"].clone()
}

/// Polls the task `task_id` of the `family` task methods to its terminal status and returns that status.
fn task_result(ctx: &MmArc, family: &str, task_id: &Json) -> Json {
    for _ in 0..400 {
        let status = versioned_result(
            ctx,
            &format!("task::{family}::status"),
            json!({ "task_id": task_id, "forget_if_finished": false }),
        );
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            return status;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the {family} task did not finish");
}

/// The coin `ticker` is registered, is of the TRON family and is wallet-only, although its coins-config entry does
/// not say so, and it declines every match.
fn assert_wallet_only_tron_coin(ctx: &MmArc, ticker: &str) {
    let coin = match block_on(lp_coinfind(ctx, ticker)).unwrap() {
        Some(MmCoinEnum::EthCoin(coin)) => coin,
        Some(_) => panic!("{ticker} is not a coin of the EVM family"),
        None => panic!("{ticker} is not registered"),
    };
    assert!(coin.is_tron_family(), "{ticker}");
    assert!(coins::coin_conf(ctx, ticker)["wallet_only"].is_null(), "{ticker}");
    assert!(coin.wallet_only(ctx), "{ticker} must be wallet-only");
    assert!(!coin.is_coin_protocol_supported(&None), "{ticker} must decline a match");
}

/// CRD ch.55 D55.5: the legacy `enable` and `electrum` activate a TRX coin and a TRC20 token, each from its own
/// `urls`, and answer the legacy success body with the base58check address and the balance.
#[test]
fn legacy_enable_and_electrum_activate_tron_coins() {
    const { assert!(coins::TRON_ACTIVATION_AVAILABLE) };
    let node = TronStub::start();
    for method in ["enable", "electrum"] {
        let ctx = tron_node();
        for (ticker, balance) in [("TRXT", "2.5"), ("USDT-TRC20", "1.234567")] {
            let body = legacy_success(
                &ctx,
                method,
                json!({ "coin": ticker, "urls": [node.url], "tx_history": false }),
            );
            assert_eq!(body["result"], "success", "{method} {ticker}: {body}");
            assert_eq!(body["coin"], ticker, "{method}: {body}");
            assert_eq!(body["balance"], balance, "{method} {ticker}: {body}");
            let address = body["address"].as_str().unwrap_or_default();
            assert!(
                address.starts_with('T') && address.len() == 34,
                "{method} {ticker}: {body}"
            );
            assert_wallet_only_tron_coin(&ctx, ticker);
        }
    }
}

/// CRD ch.21 §21.4: `enable_eth_with_tokens` and `task::enable_eth::init` activate a TRX platform with a TRC20
/// token of the request; `enable_erc20` and `task::enable_erc20::init` add a token to the active platform. The
/// platform's config is a trading-capable one, so the request carries `swap_contract_address` (F21.B3).
#[test]
fn the_mmrpc_routes_activate_tron_coins() {
    let node = TronStub::start();
    let platform_request = |tokens: Json| {
        json!({
            "ticker": "TRXT",
            "nodes": [{ "url": node.url }],
            "swap_contract_address": USDT_CONTRACT,
            "erc20_tokens_requests": tokens,
        })
    };

    // The one-shot platform route, then a token through `enable_erc20`.
    let ctx = tron_node();
    let result = versioned_result(
        &ctx,
        "enable_eth_with_tokens",
        platform_request(json!([{ "ticker": "USDT-TRC20" }])),
    );
    assert_eq!(result["current_block"], 4242, "{result}");
    let result = versioned_result(
        &ctx,
        "enable_erc20",
        json!({ "ticker": "USDT2-TRC20", "activation_params": {} }),
    );
    assert_eq!(result["platform_coin"], "TRXT", "{result}");
    for ticker in ["TRXT", "USDT-TRC20", "USDT2-TRC20"] {
        assert_wallet_only_tron_coin(&ctx, ticker);
    }

    // The task platform route, then a token through `task::enable_erc20`.
    let ctx = tron_node();
    let init = versioned_result(
        &ctx,
        "task::enable_eth::init",
        platform_request(json!([{ "ticker": "USDT-TRC20" }])),
    );
    let status = task_result(&ctx, "enable_eth", &init["task_id"]);
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(status["details"]["current_block"], 4242, "{status}");
    let init = versioned_result(
        &ctx,
        "task::enable_erc20::init",
        json!({ "ticker": "USDT2-TRC20", "activation_params": {} }),
    );
    let status = task_result(&ctx, "enable_erc20", &init["task_id"]);
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(status["details"]["platform_coin"], "TRXT", "{status}");
    for ticker in ["TRXT", "USDT-TRC20", "USDT2-TRC20"] {
        assert_wallet_only_tron_coin(&ctx, ticker);
    }
}
