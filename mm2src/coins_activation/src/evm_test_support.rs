//! Test support of the EVM activation tests: a scripted EVM JSON-RPC node on a local socket and the test
//! contexts, so the real activation routines run end to end without a network.

use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The platform and the tokens of the test coins configuration.
pub const PLATFORM: &str = "ETH";
pub const USDT_CONTRACT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub const DAI_CONTRACT: &str = "0x6B175474E89094C44Da98b954EedeAC495271d0F";
pub const SWAP_CONTRACT: &str = "0x24ABE4c71FC658C91313b6552cd40cD808b3Ea80";

/// How the scripted node answers a JSON-RPC request.
pub enum Reply {
    /// A JSON-RPC success carrying this `result`.
    Result(Json),
    /// A JSON-RPC error object.
    RpcError,
    /// A bare HTTP status without a JSON-RPC body (a gateway failure).
    HttpStatus(u16),
}

/// A scripted EVM JSON-RPC node. Its thread stops when the node is dropped.
pub struct MockEvmNode {
    pub url: String,
    stop: Arc<AtomicBool>,
}

impl Drop for MockEvmNode {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn read_request_body(stream: &mut TcpStream) -> Json {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buf[..n]);
        if let Some(header_end) = find_subslice(&raw, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&raw[..header_end]).to_lowercase();
            let content_len = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= header_end + 4 + content_len {
                break;
            }
        }
    }
    let body_start = find_subslice(&raw, b"\r\n\r\n").map(|i| i + 4).unwrap_or(raw.len());
    serde_json::from_slice(&raw[body_start..]).unwrap_or(Json::Null)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.flush().ok();
}

/// The behaviour of a scripted node.
type NodeHandler = Arc<dyn Fn(&str, &Json) -> Reply + Send + Sync>;

fn serve(mut stream: TcpStream, handler: &(dyn Fn(&str, &Json) -> Reply + Send + Sync)) {
    let request = read_request_body(&mut stream);
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let id = request["id"].clone();
    match handler(&method, &request["params"]) {
        Reply::Result(result) => respond(
            &mut stream,
            200,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        ),
        Reply::RpcError => respond(
            &mut stream,
            200,
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "scripted failure" } })
                .to_string(),
        ),
        Reply::HttpStatus(status) => respond(&mut stream, status, "{}"),
    }
}

/// Starts a node answering every request through `handler(method, params)`.
pub fn spawn_node(handler: impl Fn(&str, &Json) -> Reply + Send + Sync + 'static) -> MockEvmNode {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the scripted node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let handler: NodeHandler = Arc::new(handler);
    std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let handler = handler.clone();
                    std::thread::spawn(move || serve(stream, handler.as_ref()));
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                },
                Err(_) => break,
            }
        }
    });
    MockEvmNode {
        url: format!("http://127.0.0.1:{port}"),
        stop,
    }
}

/// The error of a result that must be a failure; the success type needs no `Debug`.
pub fn error_of<T, E>(result: Result<T, E>, what: &str) -> E {
    match result {
        Ok(_) => panic!("{}", what),
        Err(e) => e,
    }
}

/// A node that answers every request with HTTP 502.
pub fn bad_gateway_node() -> MockEvmNode { spawn_node(|_, _| Reply::HttpStatus(502)) }

/// The URL of a port nothing listens on.
pub fn refusing_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A 32-byte ABI word holding `value`.
pub fn abi_word(value: u128) -> String { format!("0x{:064x}", value) }

/// The standard behaviour of a healthy node: a client version, a chain height, a native balance and the ERC-20
/// views of two tokens (`decimals()` of 6 and 18, `balanceOf` of `token_balance`).
pub fn healthy_node_reply(method: &str, params: &Json, native_balance: u128, token_balance: u128) -> Reply {
    match method {
        "web3_clientVersion" => Reply::Result(json!("Geth/v1.13.0")),
        "eth_blockNumber" => Reply::Result(json!("0x2a")),
        "eth_getBalance" => Reply::Result(json!(format!("0x{:x}", native_balance))),
        "eth_getTransactionCount" => Reply::Result(json!("0x0")),
        "eth_call" => {
            let call = &params[0];
            let data = call["data"]
                .as_str()
                .or_else(|| call["input"].as_str())
                .unwrap_or_default();
            let to = call["to"].as_str().unwrap_or_default().to_lowercase();
            if data.starts_with("0x313ce567") {
                // decimals(): USDT has 6, everything else 18.
                let decimals = if to == USDT_CONTRACT.to_lowercase() { 6 } else { 18 };
                Reply::Result(json!(abi_word(decimals)))
            } else if data.starts_with("0x70a08231") {
                Reply::Result(json!(abi_word(token_balance)))
            } else {
                Reply::RpcError
            }
        },
        _ => Reply::RpcError,
    }
}

/// A healthy node with a native balance of 1.5 ETH-wei-units and a token balance of 2_000_000 base units.
pub fn healthy_node() -> MockEvmNode {
    spawn_node(|method, params| healthy_node_reply(method, params, 1_500_000_000_000_000_000, 2_000_000))
}

/// The coins configuration of the EVM tests.
pub fn coins_conf() -> Json {
    json!([
        {
            "coin": PLATFORM,
            "name": "ethereum",
            "chain_id": 1,
            "derivation_path": "m/44'/60'",
            "trezor_coin": "Ethereum",
            "protocol": { "type": "ETH", "protocol_data": { "chain_id": 1 } },
            "mm2": 1,
            "required_confirmations": 2
        },
        {
            "coin": "ETHW",
            "name": "ethereum-wallet-only",
            "chain_id": 1,
            "wallet_only": true,
            "protocol": { "type": "ETH", "protocol_data": { "chain_id": 1 } },
            "mm2": 1
        },
        {
            "coin": "NOCHAIN",
            "name": "no-chain-id",
            "protocol": { "type": "ETH" },
            "mm2": 1
        },
        {
            "coin": "USDT-ERC20",
            "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": USDT_CONTRACT } },
            "decimals": 6,
            "required_confirmations": 4,
            "mm2": 1
        },
        {
            "coin": "DAI-ERC20",
            "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": DAI_CONTRACT } },
            "mm2": 1
        },
        {
            "coin": "BAD-ERC20",
            "protocol": {
                "type": "ERC20",
                "protocol_data": { "platform": PLATFORM, "contract_address": USDT_CONTRACT.to_lowercase() }
            },
            "decimals": 6,
            "mm2": 1
        },
        {
            "coin": "OTHER-ERC20",
            "protocol": { "type": "ERC20", "protocol_data": { "platform": "ETHW", "contract_address": USDT_CONTRACT } },
            "decimals": 6,
            "mm2": 1
        },
        {
            "coin": "NFT_ETH",
            "protocol": { "type": "NFT", "protocol_data": { "platform": PLATFORM } },
            "mm2": 1
        },
        {
            "coin": "KMD",
            "protocol": { "type": "UTXO" },
            "mm2": 1
        }
    ])
}

/// A context with the EVM coins configuration and an Iguana key.
pub fn iguana_ctx() -> MmArc {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "coins": coins_conf() }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "evm activation test passphrase")
        .expect("initialize the crypto context");
    ctx
}

/// The mnemonic of the HD test context.
pub const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// A context with the EVM coins configuration, a global HD account and an in-memory HD wallet database.
pub fn hd_ctx() -> MmArc { hd_ctx_with_conf(json!({})) }

/// The HD test context with the `extra` top-level `MM2.json` settings merged in.
pub fn hd_ctx_with_conf(extra: serde_json::Value) -> MmArc {
    let mut conf = json!({ "enable_hd": true, "coins": coins_conf() });
    if let (Some(conf), Some(extra)) = (conf.as_object_mut(), extra.as_object()) {
        conf.extend(extra.clone());
    }
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).expect("initialize the crypto context");
    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("in-memory database");
    let _ = ctx.sqlite_connection.pin(Arc::new(std::sync::Mutex::new(connection)));
    ctx
}
