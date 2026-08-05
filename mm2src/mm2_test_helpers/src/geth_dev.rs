//! A throwaway `geth --dev` node for EVM integration tests, shared by the
//! `coins` and `mm2_main` test suites.
//!
//! [`GethDev::start`] spawns a fresh, prefunded `geth --dev` chain on a random
//! port and returns handles for the JSON-RPC endpoint, the unlocked developer
//! account, and the chain id. It exposes just enough JSON-RPC (`deploy`,
//! `fund_eth`, `send_call`, `wait_receipt`, `rpc`) to set up EVM test fixtures.
//! The node is killed and its datadir removed on drop.
//!
//! `start` returns `None` when the `geth` binary is not on `PATH`, so callers
//! can skip the test and keep the default offline suite green; a CI job with
//! geth installed runs them.
//!
//! Deployable bytecode for a clean-room `EtomicSwap` HTLC contract and a minimal
//! ERC20 is embedded here (compiled with solc 0.8.26; sources in
//! `for_tests/*.sol`). Addresses are passed around as `0x`-prefixed hex strings
//! so the harness stays free of any coin-crate types.

use common::block_on;
use http::Request;
use mm2_net::transport::slurp_req;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Deployable bytecode of the clean-room `EtomicSwap` v1 HTLC contract. No `0x` prefix.
pub const ETOMIC_SWAP_BYTECODE: &str = include_str!("../for_tests/EtomicSwap_sol_EtomicSwap.bin");
/// Deployable bytecode of a minimal ERC20 that mints its supply to the deployer. No `0x` prefix.
pub const TEST_ERC20_BYTECODE: &str = include_str!("../for_tests/TestErc20_sol_TestErc20.bin");

/// A running `geth --dev` node. Killed and its datadir removed on drop.
pub struct GethDev {
    child: Child,
    datadir: std::path::PathBuf,
    /// HTTP JSON-RPC endpoint, e.g. `http://127.0.0.1:PORT`.
    pub rpc_url: String,
    /// The prefunded, unlocked developer account (`0x`-prefixed hex).
    pub dev_account: String,
    /// The chain id geth --dev picked (usually 1337).
    pub chain_id: u64,
}

impl GethDev {
    /// Starts a `geth --dev` node, or returns `None` if the `geth` binary is not
    /// on `PATH` (so callers can skip the test) or it fails to come up.
    pub fn start() -> Option<GethDev> {
        if Command::new("geth").arg("version").output().is_err() {
            return None;
        }

        // Grab a free TCP port for the HTTP-RPC endpoint.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
            listener.local_addr().ok()?.port()
        };
        let rpc_url = format!("http://127.0.0.1:{}", port);
        let datadir = std::env::temp_dir().join(format!("kdf-geth-dev-{}-{}", std::process::id(), port));
        let _ = std::fs::create_dir_all(&datadir);

        let child = Command::new("geth")
            .args([
                "--dev",
                "--http",
                "--http.addr",
                "127.0.0.1",
                "--http.port",
                &port.to_string(),
                "--http.api",
                "eth,web3,net,debug",
                "--datadir",
                datadir.to_str().unwrap(),
                "--ipcdisable",
                "--nodiscover",
                "--maxpeers",
                "0",
                "--rpc.allow-unprotected-txs",
                "--verbosity",
                "1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let mut node = GethDev {
            child,
            datadir,
            rpc_url,
            dev_account: String::new(),
            chain_id: 0,
        };

        // Wait for the RPC to come up.
        let mut ready = false;
        for _ in 0..120 {
            if node.rpc_result("eth_blockNumber", json!([])).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if !ready {
            return None;
        }

        let accounts = node.rpc("eth_accounts", json!([]));
        node.dev_account = accounts
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .expect("geth --dev exposes a developer account")
            .to_owned();
        let chain_id_hex = node.rpc("eth_chainId", json!([]));
        node.chain_id =
            u64::from_str_radix(chain_id_hex.as_str().unwrap().trim_start_matches("0x"), 16).expect("valid chain id");
        Some(node)
    }

    fn rpc_result(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let payload = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let request = Request::builder()
            .method("POST")
            .uri(&self.rpc_url)
            .header("Content-Type", "application/json")
            .body(payload)
            .map_err(|e| e.to_string())?;
        let (_status, _headers, resp) = block_on(slurp_req(request)).map_err(|e| e.to_string())?;
        let resp: Value = serde_json::from_slice(&resp).map_err(|e| e.to_string())?;
        if let Some(err) = resp.get("error") {
            if !err.is_null() {
                return Err(err.to_string());
            }
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Single JSON-RPC call; panics on transport/RPC error.
    pub fn rpc(&self, method: &str, params: Value) -> Value {
        self.rpc_result(method, params.clone())
            .unwrap_or_else(|e| panic!("rpc {} failed: {} (params={})", method, e, params))
    }

    /// Waits for a transaction receipt and returns it.
    pub fn wait_receipt(&self, tx_hash: &str) -> Value {
        for _ in 0..200 {
            // Tolerate the transient "transaction indexing is in progress" error geth
            // returns right after startup, and a null (not-yet-mined) result.
            if let Ok(receipt) = self.rpc_result("eth_getTransactionReceipt", json!([tx_hash])) {
                if !receipt.is_null() {
                    return receipt;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for receipt of {}", tx_hash);
    }

    /// Deploys a contract from the unlocked dev account, returning its `0x`-address.
    /// `ctor_args_hex` is the ABI-encoded constructor arguments (no `0x`), or `""`.
    pub fn deploy(&self, bytecode: &str, ctor_args_hex: &str) -> String {
        let data = format!("0x{}{}", bytecode.trim(), ctor_args_hex);
        let tx = json!({
            "from": self.dev_account,
            "data": data,
            "gas": "0x3d0900", // 4_000_000
        });
        let hash = self.rpc("eth_sendTransaction", json!([tx]));
        let receipt = self.wait_receipt(hash.as_str().unwrap());
        assert_eq!(
            receipt["status"].as_str(),
            Some("0x1"),
            "contract deploy failed: receipt={}",
            receipt
        );
        receipt["contractAddress"]
            .as_str()
            .expect("deploy receipt has contractAddress")
            .to_owned()
    }

    /// Sends `wei` from the dev account to `to` (`0x`-address).
    pub fn fund_eth(&self, to: &str, wei: u128) {
        let tx = json!({
            "from": self.dev_account,
            "to": to,
            "value": format!("0x{:x}", wei),
        });
        let hash = self.rpc("eth_sendTransaction", json!([tx]));
        self.wait_receipt(hash.as_str().unwrap());
    }

    /// Sends a contract call (`data`) from the dev account to `to` (`0x`-address).
    pub fn send_call(&self, to: &str, data: &[u8]) {
        let tx = json!({
            "from": self.dev_account,
            "to": to,
            "data": format!("0x{}", hex::encode(data)),
            "gas": "0x100000",
        });
        let hash = self.rpc("eth_sendTransaction", json!([tx]));
        self.wait_receipt(hash.as_str().unwrap());
    }
}

impl Drop for GethDev {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.datadir);
    }
}
