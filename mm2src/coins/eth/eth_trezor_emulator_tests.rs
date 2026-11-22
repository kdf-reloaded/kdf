//! Emulator-gated EVM Trezor signing integration tests (CRD §50.8, T50.*).
//!
//! These tests prove the EVM Trezor `task::withdraw` signing path works
//! end-to-end against a live Trezor **emulator** (`trezor-user-env`). Only the
//! **signing** goes to the device; the chain state (balance / nonce) is served
//! by a tiny in-process JSON-RPC mock node so the tests need no real chain (the
//! emulator signs, it is not a chain). They compile only under
//! `#[cfg(all(test, not(wasm), not(ios), feature = "trezor-emulator-tests"))]`.
//!
//! Run (serialize — one device):
//! ```text
//! TREZOR_EMULATOR_UDP=127.0.0.1:21324 \
//!   cargo test -p coins --features trezor-emulator-tests \
//!   eth_trezor_emulator_tests -- --nocapture --test-threads=1
//! ```
//! When `TREZOR_EMULATOR_UDP` is set, an unreachable emulator makes the tests fail;
//! when it is not set, the tests print a skip message and pass.
//!
//! Coverage of the §50.8 acceptance tests in this module:
//!   * T50.1 / T50.4 — `emulator_trezor_withdraw_native_no_from`
//!   * T50.2         — `emulator_trezor_withdraw_erc20_with_from_path`
//!   * T50.5         — `emulator_trezor_withdraw_pin_user_action` (`#[ignore]`; see below)
//!   * T50.6         — `emulator_trezor_withdraw_passphrase_user_action`
//!   * T50.7         — `emulator_trezor_withdraw_passphrase_action_mismatch`
//!   * T50.10        — `emulator_trezor_withdraw_bad_from_selector`
//!   * T50.11        — `emulator_trezor_withdraw_tron_unsupported`
//!   * T49.31        — `emulator_trezor_account_balance_of_a_platform_ticker_lists_the_platform_and_its_tokens`
//!   * T49.37        — `emulator_trezor_get_new_address_shows_the_address_and_records_it_after_the_button` (EVM),
//!                     `emulator_trezor_get_new_address_utxo_shows_the_address_and_records_it_after_the_button`
//!                     (UTXO, a Bitcoin-style node stub) and
//!                     `emulator_trezor_get_new_address_cancel_while_the_address_is_shown_records_nothing`
//!   * T49.38        — `emulator_trezor_get_new_address_refused_on_the_device_is_user_cancelled`,
//!                     `emulator_trezor_get_new_address_another_seed_is_found_unexpected_device`,
//!                     `emulator_trezor_get_new_address_another_passphrase_is_a_device_address_mismatch` and
//!                     `emulator_trezor_get_new_address_a_failing_balance_query_leaves_the_address_known`
//! The passphrase tests (T50.6 / T50.7) re-seed the shared device with
//! passphrase protection ON and, via a teardown RAII guard, restore the clean
//! SLIP-14 (no-pin, no-passphrase) state so the other serialized tests stay
//! stable. T50.5 (PIN) is `#[ignore]`d: driving a PIN requires answering the
//! device's scrambled `PinMatrixRequest` matrix, which the plain
//! host-passphrase harness here cannot compute; the PIN plumbing itself is
//! otherwise exercised by the connect-processor path. T50.8 / T50.9 (no-device /
//! foreign-device discriminants) are device-availability paths outside this
//! emulator harness's scope.

use super::eth_hd_wallet::{pubkey_from_extended, EthHDAccount, EthHDWallet};
use super::*;
use crate::hd_wallet::HDAccountsMutex;
use crate::hd_wallet_storage::HDWalletCoinStorage;
use crate::rpc_command::init_withdraw::{init_withdraw, withdraw_status, withdraw_user_action, WithdrawAwaitingStatus,
                                        WithdrawCompatRpcStatus, WithdrawStatusRequest, WithdrawUserAction,
                                        WithdrawUserActionRequest};
use crate::{CoinsContext, MmCoinEnum, WithdrawError, WithdrawFee, WithdrawRequest};
use common::block_on;
use crypto::trezor::client::TrezorClient;
use crypto::trezor::transport::udp::trezor_udp_client;
use crypto::trezor::utxo::TrezorUtxoCoin;
use crypto::trezor::{ProcessTrezorResponse, TrezorPassphraseResponse, TrezorPinMatrix3x3Response,
                     TrezorProcessingError, TrezorRequestProcessor};
use crypto::{Bip32DerPathOps, Bip44Chain, Bip44PathToAccount, Bip44PathToCoin, ChildNumber, CryptoCtx, DerivationPath,
             EcdsaCurve, HwClient, HwProcessingError, Secp256k1ExtendedPublicKey, TrezorConnectProcessor};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_eth::keys::public_to_address;
use primitives::hash::H264;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc as StdArc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Derivation paths / constants
// ---------------------------------------------------------------------------

/// Default enabled EVM path (R50.4): account 0, external chain, address 0.
const EVM_PATH: &str = "m/44'/60'/0'/0/0";
/// EVM BIP-44 coin path (purpose + coin type) used to seed `Bip44PathToCoin`.
const EVM_COIN_PATH: &str = "m/44'/60'";
/// EVM account path (`m/44'/60'/0'`); its extended public key is the HD account
/// node from which external addresses are derived.
const EVM_ACCOUNT_PATH: &str = "m/44'/60'/0'";
/// The MM2-internal derivation path the hardware-wallet ctx binds its identity
/// to (Komodo/secp256k1). Must match `crypto`'s internal constant so the
/// device-identity check in `HardwareWalletCtx::trezor` passes.
const MM2_INTERNAL_PATH: &str = "m/44'/141'/2147483647/0/0";
const DEFAULT_EMULATOR_ADDR: &str = "127.0.0.1:21324";
/// Fixed withdrawal recipient.
const RECIPIENT: &str = "0x1111111111111111111111111111111111111111";
/// A test ERC20 token contract address.
const TOKEN_ADDR: &str = "0x2b294F029Fde858b2c62184e8390591755521d8E";

fn emulator_addr() -> String {
    std::env::var("TREZOR_EMULATOR_UDP").unwrap_or_else(|_| DEFAULT_EMULATOR_ADDR.to_owned())
}

fn script_path() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../mm2src/coins ; the harness lives in the trezor crate.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../trezor/tests/emulator/emulator_control.py")
}

/// Quick, non-hanging reachability probe: the emulator answers a `PINGPING`
/// datagram with `PONGPONG`.
fn emulator_reachable(addr: &str) -> bool {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return false,
    };
    if socket.connect(addr).is_err() {
        return false;
    }
    if socket.set_read_timeout(Some(Duration::from_millis(800))).is_err() {
        return false;
    }
    if socket.send(b"PINGPING").is_err() {
        return false;
    }
    let mut buf = [0u8; 8];
    matches!(socket.recv(&mut buf), Ok(n) if &buf[..n] == b"PONGPONG")
}

/// Probes the emulator, retrying briefly so one lost datagram does not decide
/// the result. When `TREZOR_EMULATOR_UDP` is set the emulator must be reachable
/// (the test panics otherwise); when it is not set an unreachable emulator
/// prints a skip line and the test returns early.
fn emulator_available(test_name: &str) -> bool {
    let addr = emulator_addr();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        if emulator_reachable(&addr) {
            return true;
        }
    }
    if std::env::var_os("TREZOR_EMULATOR_UDP").is_some() {
        panic!(
            "{test_name}: emulator unreachable at {addr}; start it first, e.g. with `python3 mm2src/trezor/tests/emulator/emulator_control.py setup`"
        );
    }
    println!("SKIP {test_name}: emulator unreachable at {addr}");
    false
}

/// Start + wipe + seed the emulator with the SLIP-14 mnemonic (idempotent).
fn emulator_setup() {
    let status = Command::new("python3")
        .arg(script_path())
        .arg("setup")
        .status()
        .expect("failed to spawn emulator_control.py setup");
    assert!(status.success(), "emulator_control.py setup failed: {status}");
}

/// Start + wipe + seed the emulator with `mnemonic` (no PIN, no passphrase): another seed than the SLIP-14 one.
fn emulator_setup_with_mnemonic(mnemonic: &str) {
    let status = Command::new("python3")
        .arg(script_path())
        .arg("setup")
        .arg("--mnemonic")
        .arg(mnemonic)
        .status()
        .expect("failed to spawn emulator_control.py setup --mnemonic");
    assert!(
        status.success(),
        "emulator_control.py setup --mnemonic failed: {status}"
    );
}

/// Start + wipe + seed the emulator with the SLIP-14 mnemonic **and passphrase
/// protection ON** (host passphrase entry — the emulator default after a wipe).
/// A passphrase yields a distinct hidden wallet, so callers must read the sender
/// address *after* supplying the passphrase.
fn emulator_setup_passphrase() {
    let status = Command::new("python3")
        .arg(script_path())
        .arg("setup")
        .arg("--passphrase-protection")
        .status()
        .expect("failed to spawn emulator_control.py setup --passphrase-protection");
    assert!(
        status.success(),
        "emulator_control.py setup --passphrase-protection failed: {status}"
    );
}

/// RAII teardown guard: on drop (including panic unwind) re-seeds the emulator
/// with the clean SLIP-14 state (no PIN, no passphrase) so the other serialized
/// emulator tests keep passing. Never panics during unwind — it only logs on
/// failure.
struct Slip14Restore;

impl Drop for Slip14Restore {
    fn drop(&mut self) {
        match Command::new("python3").arg(script_path()).arg("setup").status() {
            Ok(status) if status.success() => println!("teardown: restored clean SLIP-14 emulator state"),
            other => eprintln!("teardown: FAILED to restore clean SLIP-14 emulator state: {other:?}"),
        }
    }
}

/// A background `confirm-loop` child that auto-approves on-device confirmations.
/// Killed on drop so a panicking test never leaks the process.
struct ConfirmLoop {
    child: Child,
}

impl ConfirmLoop {
    fn spawn(timeout_secs: u32) -> ConfirmLoop { Self::spawn_deciding(timeout_secs, "yes") }

    /// A loop that presses `decision` (`"yes"` or `"no"`, the user's refusal) on every non-idle screen.
    fn spawn_deciding(timeout_secs: u32, decision: &str) -> ConfirmLoop {
        let child = Command::new("python3")
            .arg(script_path())
            .arg("confirm-loop")
            .arg("--timeout")
            .arg(timeout_secs.to_string())
            .arg("--decision")
            .arg(decision)
            .spawn()
            .expect("failed to spawn emulator_control.py confirm-loop");
        ConfirmLoop { child }
    }
}

impl Drop for ConfirmLoop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// In-process JSON-RPC mock node
//
// The withdrawal task runs on the shared executor's worker threads, so the
// `mocktopus` per-thread mocks the *unit* withdraw tests use cannot reach it.
// Instead we serve the two chain reads the EVM withdraw performs (balance +
// nonce, plus ERC20 `balanceOf` via `eth_call`) from a real, thread-safe local
// HTTP endpoint. All values are canned; nothing touches a real chain.
// ---------------------------------------------------------------------------

struct MockNode {
    stop: StdArc<AtomicBool>,
    url: String,
}

impl Drop for MockNode {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

/// Spawn the mock node. `native_balance_hex` answers `eth_getBalance`,
/// `nonce_hex` answers `eth_getTransactionCount` / `parity_nextNonce`, and
/// `erc20_balance_hex` (a 32-byte `0x…` word) answers the ERC20 `balanceOf`
/// `eth_call`.
fn spawn_mock_node(
    native_balance_hex: &'static str,
    nonce_hex: &'static str,
    erc20_balance_hex: &'static str,
) -> MockNode {
    spawn_scripted_node(move |request| rpc_reply(request, native_balance_hex, nonce_hex, erc20_balance_hex))
}

/// Spawn a node that answers every JSON-RPC request (each member of a batch) with `reply(request)`. The mock node of
/// the EVM reads, the Bitcoin-style node of the UTXO tests and the failing node are all of this kind.
fn spawn_scripted_node(reply: impl Fn(&serde_json::Value) -> serde_json::Value + Send + 'static) -> MockNode {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock node");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    handle_conn(stream, &reply);
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                },
                Err(_) => break,
            }
        }
    });

    MockNode {
        stop,
        url: format!("http://127.0.0.1:{port}"),
    }
}

/// A node whose every answer is a JSON-RPC error: a node that is up but cannot serve the read (the balance query of
/// the tests that expect it to fail). The error member has the form of both the Ethereum and the Bitcoin-style nodes.
fn spawn_failing_node() -> MockNode {
    spawn_scripted_node(|request| {
        let id = request.get("id").cloned().unwrap_or(serde_json::Value::from(0));
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null,
            "error": { "code": -32000, "message": "the node cannot answer" }
        })
    })
}

/// A Bitcoin-style node with no unspent outputs: every address has a zero balance (`listunspent` answers an empty
/// list), which is all the balance step of a new address asks.
fn spawn_empty_utxo_node() -> MockNode {
    spawn_scripted_node(|request| {
        let id = request.get("id").cloned().unwrap_or(serde_json::Value::from(0));
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": [], "error": null })
    })
}

fn handle_conn(mut stream: TcpStream, reply: &dyn Fn(&serde_json::Value) -> serde_json::Value) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    // Read until we have the full headers + body (Content-Length bytes).
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        raw.extend_from_slice(&buf[..n]);
        if let Some(header_end) = find_subslice(&raw, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&raw[..header_end]).to_lowercase();
            let content_len = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= header_end + 4 + content_len {
                break;
            }
        }
    }

    let body_start = find_subslice(&raw, b"\r\n\r\n").map(|i| i + 4).unwrap_or(raw.len());
    let body = &raw[body_start..];
    let json_body = serde_json::from_slice::<serde_json::Value>(body).unwrap_or(serde_json::Value::Null);

    let response_json = match &json_body {
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(reply).collect()),
        other => reply(other),
    };

    let payload = serde_json::to_vec(&response_json).unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

fn rpc_reply(
    req: &serde_json::Value,
    native_balance_hex: &str,
    nonce_hex: &str,
    erc20_balance_hex: &str,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::from(0));
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let result: serde_json::Value = match method {
        "eth_getBalance" => serde_json::Value::from(native_balance_hex),
        "eth_getTransactionCount" | "parity_nextNonce" => serde_json::Value::from(nonce_hex),
        "eth_call" => serde_json::Value::from(erc20_balance_hex),
        "eth_gasPrice" => serde_json::Value::from("0x3b9aca00"),
        "eth_estimateGas" => serde_json::Value::from("0x5208"),
        "eth_chainId" => serde_json::Value::from("0x1"),
        "eth_blockNumber" => serde_json::Value::from("0x1"),
        "net_version" => serde_json::Value::from("1"),
        _ => serde_json::Value::from("0x0"),
    };
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Device material + hardware-wallet ctx setup
// ---------------------------------------------------------------------------

struct DeviceMaterial {
    /// MM2-internal (Komodo/secp256k1) compressed pubkey — the hw-ctx identity.
    internal_pubkey: H264,
    /// EVM address at `m/44'/60'/0'/0/0`.
    eth_address: Address,
    /// Uncompressed secp256k1 public key at `m/44'/60'/0'/0/0`.
    eth_public: Public,
    /// Account-level extended public key at `m/44'/60'/0'`.
    eth_account_xpub: Secp256k1ExtendedPublicKey,
    /// The live UDP client (moved into the hw-ctx for the signing exchange).
    client: TrezorClient,
}

/// Connect to the emulator over UDP and read the identity/address/public-key
/// material the fixtures need. This mirrors the framework's activation reads
/// (R50.8 / R50.9) and the internal-identity binding done at hw-ctx init.
fn fetch_device_material() -> DeviceMaterial {
    let client = trezor_udp_client().expect("connect udp trezor client");
    block_on(async {
        let mm2_internal_path = DerivationPath::from_str(MM2_INTERNAL_PATH).unwrap();
        let evm_path = DerivationPath::from_str(EVM_PATH).unwrap();
        let evm_account_path = DerivationPath::from_str(EVM_ACCOUNT_PATH).unwrap();

        let mut session = client.session().await.expect("open session");

        let internal_xpub = session
            .get_public_key(mm2_internal_path, TrezorUtxoCoin::Komodo, EcdsaCurve::Secp256k1)
            .await
            .expect("get_public_key call")
            .ack_all()
            .await
            .expect("resolve internal xpub");
        let internal_ext = Secp256k1ExtendedPublicKey::from_str(&internal_xpub).expect("parse internal xpub");
        let internal_pubkey = H264::from(internal_ext.public_key().serialize());

        let eth_address_str = session
            .get_eth_address(evm_path.clone(), false)
            .await
            .expect("get_eth_address call")
            .ack_all()
            .await
            .expect("resolve eth address");
        let eth_address = Address::from_str(eth_address_str.trim_start_matches("0x")).expect("parse eth address");

        let eth_leaf_xpub = session
            .get_eth_public_key(evm_path, false)
            .await
            .expect("get_eth_public_key (leaf) call")
            .ack_all()
            .await
            .expect("resolve eth leaf xpub");
        let eth_leaf_ext = Secp256k1ExtendedPublicKey::from_str(&eth_leaf_xpub).expect("parse eth leaf xpub");
        let eth_public = pubkey_from_extended(&eth_leaf_ext);

        let eth_account_xpub_str = session
            .get_eth_public_key(evm_account_path, false)
            .await
            .expect("get_eth_public_key (account) call")
            .ack_all()
            .await
            .expect("resolve eth account xpub");
        let eth_account_xpub =
            Secp256k1ExtendedPublicKey::from_str(&eth_account_xpub_str).expect("parse eth account xpub");

        drop(session);

        // Sanity: the fetched leaf pubkey must map to the device's EVM address.
        assert_eq!(
            public_to_address(&eth_public),
            eth_address,
            "device leaf pubkey must map to the device EVM address"
        );

        DeviceMaterial {
            internal_pubkey,
            eth_address,
            eth_public,
            eth_account_xpub,
            client,
        }
    })
}

/// A minimal [`TrezorRequestProcessor`] that answers every device
/// `PassphraseRequest` with a fixed host passphrase and auto-acks button
/// requests. Used by [`fetch_device_material_with_passphrase`] to read the
/// *hidden* (passphrase) wallet's material outside the RPC-task user-action
/// machinery. A PIN request is unexpected here and fails loudly.
struct FixedPassphraseProcessor {
    passphrase: String,
}

#[async_trait::async_trait]
impl TrezorRequestProcessor for FixedPassphraseProcessor {
    type Error = String;

    async fn on_button_request(&self) -> MmResult<(), TrezorProcessingError<String>> { Ok(()) }

    async fn on_pin_request(&self) -> MmResult<TrezorPinMatrix3x3Response, TrezorProcessingError<String>> {
        MmError::err(TrezorProcessingError::ProcessorError(
            "unexpected PIN request while reading passphrase-wallet material".to_owned(),
        ))
    }

    async fn on_passphrase_request(&self) -> MmResult<String, TrezorProcessingError<String>> {
        Ok(self.passphrase.clone())
    }

    async fn on_ready(&self) -> MmResult<(), TrezorProcessingError<String>> { Ok(()) }
}

/// Like [`fetch_device_material`], but for a device seeded with passphrase
/// protection ON: every device `PassphraseRequest` is answered with `passphrase`
/// so the returned identity/address/public-key material belongs to the resulting
/// hidden wallet. The withdraw flow must later supply the *same* passphrase (via
/// the task user-action API) for the device-identity check to match.
fn fetch_device_material_with_passphrase(passphrase: &str) -> DeviceMaterial {
    let client = trezor_udp_client().expect("connect udp trezor client");
    let processor = FixedPassphraseProcessor {
        passphrase: passphrase.to_owned(),
    };
    // Host-entered passphrases are confirmed on-device (a button press per
    // protected read), so auto-approve those confirmations while we read the
    // hidden-wallet material.
    let _confirm = ConfirmLoop::spawn(60);
    block_on(async {
        let mm2_internal_path = DerivationPath::from_str(MM2_INTERNAL_PATH).unwrap();
        let evm_path = DerivationPath::from_str(EVM_PATH).unwrap();
        let evm_account_path = DerivationPath::from_str(EVM_ACCOUNT_PATH).unwrap();

        let mut session = client.session().await.expect("open session");

        let internal_xpub = session
            .get_public_key(mm2_internal_path, TrezorUtxoCoin::Komodo, EcdsaCurve::Secp256k1)
            .await
            .expect("get_public_key call")
            .process(&processor)
            .await
            .map_err(|e| e.to_string())
            .expect("resolve internal xpub");
        let internal_ext = Secp256k1ExtendedPublicKey::from_str(&internal_xpub).expect("parse internal xpub");
        let internal_pubkey = H264::from(internal_ext.public_key().serialize());

        let eth_address_str = session
            .get_eth_address(evm_path.clone(), false)
            .await
            .expect("get_eth_address call")
            .process(&processor)
            .await
            .map_err(|e| e.to_string())
            .expect("resolve eth address");
        let eth_address = Address::from_str(eth_address_str.trim_start_matches("0x")).expect("parse eth address");

        let eth_leaf_xpub = session
            .get_eth_public_key(evm_path, false)
            .await
            .expect("get_eth_public_key (leaf) call")
            .process(&processor)
            .await
            .map_err(|e| e.to_string())
            .expect("resolve eth leaf xpub");
        let eth_leaf_ext = Secp256k1ExtendedPublicKey::from_str(&eth_leaf_xpub).expect("parse eth leaf xpub");
        let eth_public = pubkey_from_extended(&eth_leaf_ext);

        let eth_account_xpub_str = session
            .get_eth_public_key(evm_account_path, false)
            .await
            .expect("get_eth_public_key (account) call")
            .process(&processor)
            .await
            .map_err(|e| e.to_string())
            .expect("resolve eth account xpub");
        let eth_account_xpub =
            Secp256k1ExtendedPublicKey::from_str(&eth_account_xpub_str).expect("parse eth account xpub");

        drop(session);

        assert_eq!(
            public_to_address(&eth_public),
            eth_address,
            "device leaf pubkey must map to the device EVM address"
        );

        DeviceMaterial {
            internal_pubkey,
            eth_address,
            eth_public,
            eth_account_xpub,
            client,
        }
    })
}
/// Build an `MmArc` with a `CryptoCtx` and a Trezor hardware-wallet context
/// installed from live device material (identity + live UDP client), so the
/// withdrawal task reuses the connected device for signing.
fn ctx_with_hw(material_client: TrezorClient, internal_pubkey: H264) -> MmArc {
    let conf = serde_json::json!({
        "netid": mm2_net_config::SUPPORTED_NETIDS[0],
        "coins": [
            {"coin":"ETH","name":"ethereum","protocol":{"type":"ETH"},"rpcport":80,"mm2":1},
            {"coin":"JST","name":"jst","rpcport":80,"mm2":1,"decimals":18,"protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    // A CryptoCtx must exist to host the hw-ctx; its local key is never used for
    // a Trezor coin (the device holds the secret, CRD R50.1).
    let crypto_ctx =
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor emulator test passphrase").expect("init CryptoCtx");
    crypto_ctx.init_trezor_ctx_for_tests(internal_pubkey, Some(HwClient::Trezor(material_client)));
    ctx
}

// ---------------------------------------------------------------------------
// EthCoin fixtures under the Trezor signing policy
// ---------------------------------------------------------------------------

/// The hardware context the fixture's coin is activated with: the one `ctx_with_hw` installed (CRD ch.05 R58a).
fn activation_hw_ctx(ctx: &MmArc) -> crypto::HardwareWalletArc {
    CryptoCtx::from_ctx(ctx)
        .expect("a CryptoCtx")
        .hw_ctx()
        .expect("a hardware context")
}

#[allow(clippy::too_many_arguments)]
fn build_trezor_coin(
    ctx: &MmArc,
    coin_type: EthCoinType,
    ticker: &str,
    url: String,
    my_address: Address,
    signer: EthSigner,
    derivation_method: DerivationMethod<Address, EthHDWallet>,
) -> EthCoin {
    let web3 =
        crate::eth::alloy_compat::build_provider(crate::node_url::listed_nodes("TEST", vec![url]), vec![]).unwrap();
    EthCoin(Arc::new(EthCoinImpl {
        coin_type,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        my_address,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer,
        swap_contract_address: Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap(),
        fallback_swap_contract: None,
        ticker: ticker.to_string(),
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: false,
        }],
        web3,
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        tron_gasfree: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: Some(1),
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        logs_window: Default::default(),
        derivation_method,
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
        taker_fee_quote_at_limit: true,
    }))
}

/// Iguana-style Trezor coin: the signer carries the device's default enabled
/// address/path (R50.4); no HD selector is supported.
fn device_trezor_iguana_coin(
    ctx: &MmArc,
    coin_type: EthCoinType,
    url: String,
    address: Address,
    public: Public,
) -> EthCoin {
    let ticker = match coin_type {
        EthCoinType::Eth => "ETH",
        EthCoinType::Erc20 { .. } => "JST",
        EthCoinType::Tron => "TRX",
        EthCoinType::Trc20 { .. } => "TRC20",
    };
    let signer = EthSigner::Trezor(EthTrezorSigner {
        derivation_path: DerivationPath::from_str(EVM_PATH).unwrap(),
        address,
        public,
        hw_ctx: activation_hw_ctx(ctx),
    });
    build_trezor_coin(
        ctx,
        coin_type,
        ticker,
        url,
        address,
        signer,
        DerivationMethod::Iguana(address),
    )
}

/// HD-wallet Trezor coin bound to a single activated account (`m/44'/60'/0'`)
/// whose external address 0 is the device's default address. Supports a
/// derivation-path / address-id `from` selector resolving to that address.
fn device_trezor_hd_coin(
    ctx: &MmArc,
    coin_type: EthCoinType,
    url: String,
    account_xpub: Secp256k1ExtendedPublicKey,
    leaf_address: Address,
    leaf_public: Public,
) -> EthCoin {
    device_trezor_hd_coin_with_addresses(ctx, coin_type, url, account_xpub, leaf_address, leaf_public, 1)
}

/// Same as [`device_trezor_hd_coin`], with `external_addresses_number` known
/// external addresses on account 0 (address 0 stays the default address).
fn device_trezor_hd_coin_with_addresses(
    ctx: &MmArc,
    coin_type: EthCoinType,
    url: String,
    account_xpub: Secp256k1ExtendedPublicKey,
    leaf_address: Address,
    leaf_public: Public,
    external_addresses_number: u32,
) -> EthCoin {
    device_trezor_hd_coin_with_storage(
        ctx,
        coin_type,
        url,
        account_xpub,
        leaf_address,
        leaf_public,
        external_addresses_number,
        HDWalletCoinStorage::default(),
    )
}

/// Same as [`device_trezor_hd_coin_with_addresses`], over the wallet storage `hd_wallet_storage` (the default one is
/// a mock that records nothing and must not be written).
#[allow(clippy::too_many_arguments)]
fn device_trezor_hd_coin_with_storage(
    ctx: &MmArc,
    coin_type: EthCoinType,
    url: String,
    account_xpub: Secp256k1ExtendedPublicKey,
    leaf_address: Address,
    leaf_public: Public,
    external_addresses_number: u32,
    hd_wallet_storage: HDWalletCoinStorage,
) -> EthCoin {
    let ticker = match coin_type {
        EthCoinType::Eth => "ETH",
        EthCoinType::Erc20 { .. } => "JST",
        _ => panic!("unsupported coin_type for HD trezor fixture"),
    };
    let derivation_path = Bip44PathToCoin::from_str(EVM_COIN_PATH).unwrap();
    let account_derivation_path: Bip44PathToAccount =
        derivation_path.derive(ChildNumber::new(0, true).unwrap()).unwrap();
    let account = EthHDAccount {
        account_id: 0,
        extended_pubkey: account_xpub,
        account_derivation_path,
        external_addresses_number,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert(0, account);

    let signer = EthSigner::Trezor(EthTrezorSigner {
        derivation_path: DerivationPath::from_str(EVM_PATH).unwrap(),
        address: leaf_address,
        public: leaf_public,
        hw_ctx: activation_hw_ctx(ctx),
    });
    let derivation_method = DerivationMethod::HDWallet(EthHDWallet {
        hd_wallet_storage,
        derivation_path,
        accounts: HDAccountsMutex::new(accounts),
        gap_limit: 20,
    });
    build_trezor_coin(ctx, coin_type, ticker, url, leaf_address, signer, derivation_method)
}

// ---------------------------------------------------------------------------
// Task driver
// ---------------------------------------------------------------------------

/// Register a coin and drive a `task::withdraw::init` task to a terminal status,
/// polling `task::withdraw::status` (the real RPC path). Returns the completed
/// `TransactionDetails` on `Ok` or the structured `WithdrawError` on `Error`.
fn drive_withdraw(
    ctx: &MmArc,
    coin: EthCoin,
    req: WithdrawRequest,
    timeout: Duration,
) -> Result<TransactionDetails, WithdrawError> {
    let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::EthCoin(coin))).unwrap();

    let init = match block_on(init_withdraw(ctx.clone(), req)) {
        Ok(init) => init,
        Err(e) => panic!("init_withdraw failed: {}", e),
    };
    let task_id = init.task_id;

    let start = Instant::now();
    loop {
        let status = match block_on(withdraw_status(ctx.clone(), WithdrawStatusRequest {
            task_id,
            forget_if_finished: false,
        })) {
            Ok(status) => status,
            Err(e) => panic!("withdraw_status failed: {}", e),
        };
        match status {
            WithdrawCompatRpcStatus::Ok(details) => return Ok(details),
            WithdrawCompatRpcStatus::Error(e) => return Err(e),
            WithdrawCompatRpcStatus::InProgress(s) => {
                println!("  in-progress: {}", serde_json::to_value(&s).unwrap());
            },
            WithdrawCompatRpcStatus::UserActionRequired(_) => {
                println!("  awaiting user action");
            },
        }
        if start.elapsed() > timeout {
            panic!("withdraw task did not reach a terminal status within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// T50.1 / T50.4: native EVM Trezor withdraw without `from`. The device signs at
/// the enabled default HD address; the completed payload's `from` is that
/// address and the recovered sender of `tx_hex` equals it (byte-parity check).
#[test]
fn emulator_trezor_withdraw_native_no_from() {
    if !emulator_available("emulator_trezor_withdraw_native_no_from") {
        return;
    }
    emulator_setup();

    let material = fetch_device_material();
    let device_addr = material.eth_address;
    println!("DEVICE_ADDR = {:#x}", device_addr);
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    // 100 ETH balance, nonce 0 — enough for 1 ETH + gas.
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let coin = device_trezor_iguana_coin(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        device_addr,
        material.eth_public,
    );

    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: None,
        to: RECIPIENT.to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let _confirm = ConfirmLoop::spawn(120);
    let details = drive_withdraw(&ctx, coin, req, Duration::from_secs(120)).expect("native withdraw Ok");

    let expected_from = checksum_address(&format!("{:#02x}", device_addr));
    println!("FROM = {:?}", details.from);
    println!("TX_HASH = {}", details.tx_hash);
    assert_eq!(details.from, vec![expected_from]);
    assert!(!details.tx_hex.0.is_empty(), "tx_hex must be present");
    assert!(!details.tx_hash.is_empty(), "tx_hash must be present");

    // Definitive byte-parity + correctness check (T50.4): recover the sender
    // from the RLP-encoded signed transaction and confirm it is the device.
    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("decode signed tx");
    println!("RECOVERED_SENDER = {:#x}", signed.sender());
    assert_eq!(
        signed.sender(),
        device_addr,
        "recovered sender must be the device address"
    );
    assert_eq!(
        format!("{:02x}", signed.tx_hash()),
        details.tx_hash,
        "tx_hash must match the encoded tx"
    );
}

/// T50.2 / T49.9: ERC20 Trezor withdraw with a derivation-path `from` selector resolving
/// to a non-default activated HD address (external address 1) of the token's platform. Exercises the `transfer(address,uint256)`
/// ABI payload (the `EthereumTxAck` streaming path) and device signing.
#[test]
fn emulator_trezor_withdraw_erc20_with_from_path() {
    if !emulator_available("emulator_trezor_withdraw_erc20_with_from_path") {
        return;
    }
    emulator_setup();

    let material = fetch_device_material();
    let device_addr = material.eth_address;
    println!("DEVICE_ADDR = {:#x}", device_addr);
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    // native balance (unused for ERC20 amount check), nonce 0, balanceOf = 1000 tokens.
    let node = spawn_mock_node(
        "0x56bc75e2d63100000",
        "0x0",
        "0x00000000000000000000000000000000000000000000003635c9adc5dea00000",
    );
    // The token is built the way the token activation builds it, from its platform coin, and so carries the
    // single-address derivation method of an activated token (CRD ch.49 R49.34, T49.9). The platform is registered,
    // as `from` is resolved against its HD wallet.
    let platform = device_trezor_hd_coin_with_addresses(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        material.eth_account_xpub.clone(),
        device_addr,
        material.eth_public,
        2,
    );
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::EthCoin(platform.clone()))).unwrap();
    let coin = block_on(platform.erc20_token_from_conf_or_contract(
        "JST".to_string(),
        Address::from_str(TOKEN_ADDR.trim_start_matches("0x")).unwrap(),
        1,
        false,
    ))
    .expect("the token builds from its platform coin");
    assert!(
        matches!(coin.derivation_method, DerivationMethod::Iguana(_)),
        "an activated token carries the single-address derivation method"
    );

    // The selected, non-default address: external address 1 of account 0, derived from the account xpub.
    let selected_public = pubkey_from_extended(
        &material
            .eth_account_xpub
            .derive_child(ChildNumber::new(0, false).unwrap())
            .unwrap()
            .derive_child(ChildNumber::new(1, false).unwrap())
            .unwrap(),
    );
    let selected_addr = public_to_address(&selected_public);
    assert_ne!(
        selected_addr, device_addr,
        "address 1 must differ from the default address"
    );
    println!("SELECTED_ADDR = {:#x}", selected_addr);

    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::DerivationPath {
            derivation_path: "m/44'/60'/0'/0/1".to_string(),
        }),
        to: RECIPIENT.to_string(),
        coin: "JST".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let _confirm = ConfirmLoop::spawn(120);
    let details = drive_withdraw(&ctx, coin, req, Duration::from_secs(120)).expect("erc20 withdraw Ok");

    let expected_from = checksum_address(&format!("{:#02x}", selected_addr));
    let default_from = checksum_address(&format!("{:#02x}", device_addr));
    assert_ne!(
        details.from,
        vec![default_from],
        "`from` must not be the default address"
    );
    println!("FROM = {:?}", details.from);
    println!("TX_HASH = {}", details.tx_hash);
    assert_eq!(details.from, vec![expected_from]);
    assert!(!details.tx_hex.0.is_empty(), "tx_hex must be present");
    assert!(!details.tx_hash.is_empty(), "tx_hash must be present");

    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("decode signed tx");
    println!("RECOVERED_SENDER = {:#x}", signed.sender());
    assert_eq!(
        signed.sender(),
        selected_addr,
        "recovered sender must be the selected address"
    );

    // The signed payload data must be the ERC20 transfer(address,uint256) call:
    // selector 0xa9059cbb + 32-byte recipient + 32-byte amount, and the tx `to`
    // is the token contract with zero value.
    let data = signed.data.as_slice();
    println!("DATA = 0x{}", hex::encode(data));
    assert_eq!(data.len(), 68, "ERC20 transfer payload must be 4 + 32 + 32 bytes");
    assert_eq!(
        &data[..4],
        &[0xa9, 0x05, 0x9c, 0xbb],
        "selector must be transfer(address,uint256)"
    );
    let recipient = Address::from_str(RECIPIENT.trim_start_matches("0x")).unwrap();
    assert_eq!(&data[16..36], &recipient.0[..], "encoded recipient must match `to`");
    assert_eq!(
        signed.action,
        Action::Call(Address::from_str(TOKEN_ADDR.trim_start_matches("0x")).unwrap()),
        "tx `to` must be the token contract"
    );
    assert!(signed.value.is_zero(), "ERC20 transfer must carry zero value");
}

/// T49.31 (last paragraph), CRD ch.49 R49.32 item 1: under the Trezor policy, `task::account_balance::init` of the
/// platform ticker lists the platform and every token activated on it at each address, and its `total_balance`
/// sums each ticker. The address list and the balances come from the account xpub and the node; no device exchange
/// is needed, as the platform's Trezor coin derives the addresses from the xpub.
#[test]
fn emulator_trezor_account_balance_of_a_platform_ticker_lists_the_platform_and_its_tokens() {
    if !emulator_available("emulator_trezor_account_balance_of_a_platform_ticker_lists_the_platform_and_its_tokens") {
        return;
    }
    emulator_setup();

    let material = fetch_device_material();
    let device_addr = material.eth_address;
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);
    // native balance 100 coins, nonce 0, `balanceOf` = 1000 tokens.
    let node = spawn_mock_node(
        "0x56bc75e2d63100000",
        "0x0",
        "0x00000000000000000000000000000000000000000000003635c9adc5dea00000",
    );
    let platform = device_trezor_hd_coin_with_addresses(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        material.eth_account_xpub.clone(),
        device_addr,
        material.eth_public,
        2,
    );
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::EthCoin(platform.clone()))).unwrap();
    let token = block_on(platform.erc20_token_from_conf_or_contract(
        "JST".to_string(),
        Address::from_str(TOKEN_ADDR.trim_start_matches("0x")).unwrap(),
        1,
        false,
    ))
    .expect("the token builds from its platform coin");
    platform.add_erc20_token_info("JST".to_string(), token.erc20_token_info().expect("a token"));

    use crate::rpc_command::init_account_balance::InitAccountBalanceRpcOps;
    let params = serde_json::from_value(serde_json::json!({ "account_index": 0 })).unwrap();
    let report = block_on(platform.init_account_balance_rpc(params)).expect("the account balance");
    let report = serde_json::to_value(&report).unwrap();
    let tickers = |map: &serde_json::Value| {
        let mut tickers: Vec<String> = map.as_object().unwrap().keys().cloned().collect();
        tickers.sort();
        tickers
    };
    let addresses = report["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 2);
    for entry in addresses {
        assert_eq!(tickers(&entry["balance"]), vec!["ETH".to_owned(), "JST".to_owned()]);
        assert_eq!(entry["balance"]["JST"]["spendable"], "1000");
        let address = entry["address"].as_str().unwrap();
        assert_eq!(address, checksum_address(&address.to_lowercase()), "EIP-55 form");
    }
    assert_eq!(tickers(&report["total_balance"]), vec![
        "ETH".to_owned(),
        "JST".to_owned()
    ]);
    assert_eq!(report["total_balance"]["JST"]["spendable"], "2000");
}

/// T50.11: a TRON-family coin under the Trezor policy reaches a terminal
/// unsupported error with NO device interaction (rejected before any exchange).
#[test]
fn emulator_trezor_withdraw_tron_unsupported() {
    if !emulator_available("emulator_trezor_withdraw_tron_unsupported") {
        return;
    }
    // No emulator_setup / no device material / no confirm-loop: the rejection is
    // raised before any device exchange (R50.20). The hardware context is
    // installed with no client, so any device exchange would fail the test.
    let conf = serde_json::json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [] });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let crypto = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor emulator test passphrase").unwrap();
    crypto.init_trezor_ctx_for_tests(H264::default(), None);

    let node = spawn_mock_node("0x0", "0x0", "0x0");
    let dummy = Address::from_str("0000000000000000000000000000000000000001").unwrap();
    let coin = device_trezor_iguana_coin(&ctx, EthCoinType::Tron, node.url.clone(), dummy, Public::default());

    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: None,
        to: RECIPIENT.to_string(),
        coin: "TRX".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let err = drive_withdraw(&ctx, coin, req, Duration::from_secs(20)).expect_err("TRON must be unsupported");
    println!("TRON_ERR = {:?}", err);
    assert!(matches!(err, WithdrawError::UnsupportedError(_)));
}

/// T50.10: an unresolvable / foreign `from` selector fails with the ch. 49
/// sender-selector discriminant BEFORE any device exchange (no confirm-loop).
#[test]
fn emulator_trezor_withdraw_bad_from_selector() {
    if !emulator_available("emulator_trezor_withdraw_bad_from_selector") {
        return;
    }
    emulator_setup();

    let material = fetch_device_material();
    let device_addr = material.eth_address;
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let coin = device_trezor_hd_coin(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        material.eth_account_xpub.clone(),
        device_addr,
        material.eth_public,
    );

    // Foreign coin-type path (BTC coin type 0'): rejected at selector validation
    // before any device exchange.
    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::DerivationPath {
            derivation_path: "m/44'/0'/0'/0/0".to_string(),
        }),
        to: RECIPIENT.to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let err = drive_withdraw(&ctx, coin, req, Duration::from_secs(20)).expect_err("bad selector must fail");
    println!("BAD_FROM_ERR = {:?}", err);
    assert!(matches!(
        err,
        WithdrawError::UnexpectedFromAddress(_)
            | WithdrawError::UnknownAccount { .. }
            | WithdrawError::FromAddressNotFound
    ));
}

// ---------------------------------------------------------------------------
// User-action (PIN / passphrase) driver + tests (T50.5 / T50.6 / T50.7)
// ---------------------------------------------------------------------------

/// The action to submit whenever the withdraw task reaches `UserActionRequired`.
enum SubmitAction {
    /// Answer a passphrase request with this host passphrase.
    Passphrase(String),
    /// Deliberately answer with a (mismatched) PIN to exercise the action-type
    /// guard.
    Pin(String),
}

/// Outcome of driving a withdraw that goes through the user-action machinery.
struct UserActionRun {
    /// Terminal task result (`Ok` details or the structured error).
    result: Result<TransactionDetails, WithdrawError>,
    /// The awaiting discriminants observed, in order (e.g. `"EnterTrezorPassphrase"`).
    awaiting_seen: Vec<String>,
    /// The `Result` of every `task::withdraw::user_action` RPC call, in order.
    user_action_results: Vec<Result<(), String>>,
}

/// Register `coin` and drive `task::withdraw::init` to a terminal status,
/// submitting `action` each time the task reaches `UserActionRequired`. The
/// on-device signing confirmation is auto-approved by a `confirm-loop` spawned
/// only *after* the first user action is submitted (so the host-passphrase entry
/// screen is never accidentally pressed). Records the awaiting discriminants and
/// the `user_action` RPC results so callers can assert the round-trip / mismatch
/// contract.
fn drive_withdraw_with_user_action(
    ctx: &MmArc,
    coin: EthCoin,
    req: WithdrawRequest,
    action: SubmitAction,
    timeout: Duration,
) -> UserActionRun {
    let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::EthCoin(coin))).unwrap();

    let init = match block_on(init_withdraw(ctx.clone(), req)) {
        Ok(init) => init,
        Err(e) => panic!("init_withdraw failed: {}", e),
    };
    let task_id = init.task_id;

    let mut awaiting_seen = Vec::new();
    let mut user_action_results = Vec::new();
    let mut confirm: Option<ConfirmLoop> = None;

    let start = Instant::now();
    loop {
        let status = match block_on(withdraw_status(ctx.clone(), WithdrawStatusRequest {
            task_id,
            forget_if_finished: false,
        })) {
            Ok(status) => status,
            Err(e) => panic!("withdraw_status failed: {}", e),
        };
        match status {
            WithdrawCompatRpcStatus::Ok(details) => {
                return UserActionRun {
                    result: Ok(details),
                    awaiting_seen,
                    user_action_results,
                }
            },
            WithdrawCompatRpcStatus::Error(e) => {
                return UserActionRun {
                    result: Err(e),
                    awaiting_seen,
                    user_action_results,
                }
            },
            WithdrawCompatRpcStatus::InProgress(s) => {
                println!("  in-progress: {}", serde_json::to_value(&s).unwrap());
            },
            WithdrawCompatRpcStatus::UserActionRequired(awaiting) => {
                let discr = match &awaiting {
                    WithdrawAwaitingStatus::EnterTrezorPin => "EnterTrezorPin",
                    WithdrawAwaitingStatus::EnterTrezorPassphrase => "EnterTrezorPassphrase",
                };
                println!("  UserActionRequired: {discr}");
                awaiting_seen.push(discr.to_owned());

                let user_action: WithdrawUserAction = match &action {
                    SubmitAction::Passphrase(pp) => {
                        WithdrawUserAction::TrezorPassphrase(TrezorPassphraseResponse { passphrase: pp.clone() })
                    },
                    SubmitAction::Pin(pin) => {
                        WithdrawUserAction::TrezorPin(TrezorPinMatrix3x3Response { pin: pin.clone() })
                    },
                };
                let submit = block_on(withdraw_user_action(ctx.clone(), WithdrawUserActionRequest {
                    task_id,
                    user_action,
                }))
                .map(|_| ())
                .map_err(|e| e.to_string());
                println!("  user_action submit result: {submit:?}");
                user_action_results.push(submit);

                // Only start auto-approving on-device confirmations once the
                // host input has been submitted, so the confirm-loop never
                // presses on the passphrase-entry screen.
                if confirm.is_none() {
                    confirm = Some(ConfirmLoop::spawn(120));
                }
            },
        }
        if start.elapsed() > timeout {
            panic!("withdraw task did not reach a terminal status within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// T50.6: native EVM Trezor withdraw against a device with **passphrase
/// protection ON**. The task surfaces the passphrase request as
/// `UserActionRequired(EnterTrezorPassphrase)`; the host submits the
/// passphrase via `task::withdraw::user_action`, the device signs, and the
/// recovered sender of the signed tx equals the *hidden* (passphrase) wallet's
/// address. This proves `on_passphrase_request` -> `EnterTrezorPassphrase` ->
/// `user_action` -> `ack_passphrase` end-to-end.
#[test]
fn emulator_trezor_withdraw_passphrase_user_action() {
    if !emulator_available("emulator_trezor_withdraw_passphrase_user_action") {
        return;
    }
    emulator_setup_passphrase();
    // Restore the clean SLIP-14 state on the way out (even on panic).
    let _restore = Slip14Restore;

    const PASSPHRASE: &str = "test-pp";

    // The hidden-wallet material for PASSPHRASE — the sender must NOT be assumed
    // to be the plain SLIP-14 address.
    let material = fetch_device_material_with_passphrase(PASSPHRASE);
    let device_addr = material.eth_address;
    println!("PASSPHRASE_WALLET_ADDR = {:#x}", device_addr);
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    // 100 ETH balance, nonce 0 — enough for 1 ETH + gas.
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let coin = device_trezor_iguana_coin(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        device_addr,
        material.eth_public,
    );

    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: None,
        to: RECIPIENT.to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let run = drive_withdraw_with_user_action(
        &ctx,
        coin,
        req,
        SubmitAction::Passphrase(PASSPHRASE.to_owned()),
        Duration::from_secs(180),
    );

    println!("AWAITING_SEEN = {:?}", run.awaiting_seen);
    println!("USER_ACTION_RESULTS = {:?}", run.user_action_results);

    // The task must have surfaced (at least) the passphrase request, and every
    // awaiting discriminant observed must be the passphrase one.
    assert!(
        run.awaiting_seen.iter().any(|s| s == "EnterTrezorPassphrase"),
        "expected at least one EnterTrezorPassphrase awaiting status, saw {:?}",
        run.awaiting_seen
    );
    assert!(
        run.awaiting_seen.iter().all(|s| s == "EnterTrezorPassphrase"),
        "unexpected non-passphrase awaiting status among {:?}",
        run.awaiting_seen
    );
    // Every passphrase submission must have been accepted by the RPC layer.
    for r in &run.user_action_results {
        assert!(r.is_ok(), "passphrase user_action submit failed: {r:?}");
    }

    let details = run.result.expect("passphrase withdraw Ok");
    let expected_from = checksum_address(&format!("{:#02x}", device_addr));
    println!("FROM = {:?}", details.from);
    println!("TX_HASH = {}", details.tx_hash);
    assert_eq!(details.from, vec![expected_from]);
    assert!(!details.tx_hex.0.is_empty(), "tx_hex must be present");
    assert!(!details.tx_hash.is_empty(), "tx_hash must be present");

    // Recover the sender from the signed tx and confirm it is the hidden
    // (passphrase) wallet's address.
    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("decode signed tx");
    println!("RECOVERED_SENDER = {:#x}", signed.sender());
    assert_eq!(
        signed.sender(),
        device_addr,
        "recovered sender must be the passphrase-wallet address"
    );
    assert_eq!(
        format!("{:02x}", signed.tx_hash()),
        details.tx_hash,
        "tx_hash must match the encoded tx"
    );
}

/// T50.7: with the task awaiting a passphrase
/// (`UserActionRequired(EnterTrezorPassphrase)`), submitting a PIN action
/// instead is rejected by the mutual action-type guard added in `crypto`
/// (`HwRpcTaskUserAction` -> `TrezorPassphraseResponse` conversion). The task
/// fails cleanly with a structured error naming the expected action type — no
/// panic, no malformed transaction.
#[test]
fn emulator_trezor_withdraw_passphrase_action_mismatch() {
    if !emulator_available("emulator_trezor_withdraw_passphrase_action_mismatch") {
        return;
    }
    emulator_setup_passphrase();
    let _restore = Slip14Restore;

    const PASSPHRASE: &str = "test-pp";

    let material = fetch_device_material_with_passphrase(PASSPHRASE);
    let device_addr = material.eth_address;
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let coin = device_trezor_iguana_coin(
        &ctx,
        EthCoinType::Eth,
        node.url.clone(),
        device_addr,
        material.eth_public,
    );

    let req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: None,
        to: RECIPIENT.to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    // Submit a PIN while a passphrase is expected.
    let run = drive_withdraw_with_user_action(
        &ctx,
        coin,
        req,
        SubmitAction::Pin("1".to_owned()),
        Duration::from_secs(120),
    );

    println!("AWAITING_SEEN = {:?}", run.awaiting_seen);
    println!("USER_ACTION_RESULTS = {:?}", run.user_action_results);

    assert!(
        run.awaiting_seen.iter().any(|s| s == "EnterTrezorPassphrase"),
        "expected the task to await a passphrase, saw {:?}",
        run.awaiting_seen
    );

    // Per the task-manager contract, delivering the action succeeds at the RPC
    // layer; the type mismatch is caught when the worker consumes it, failing
    // the task cleanly. Assert the terminal error names the expected action.
    let err = run.result.expect_err("mismatched PIN action must fail the task");
    println!("MISMATCH_ERR = {:?}", err);
    let msg = err.to_string();
    assert!(
        matches!(err, WithdrawError::InternalError(_)),
        "expected a structured internal error, got {err:?}"
    );
    assert!(
        msg.contains("TrezorPassphrase"),
        "error must identify the expected action type ('TrezorPassphrase'), got: {msg}"
    );
}

/// T50.5 (`#[ignore]`): PIN user-action round-trip. Driving a real PIN requires
/// answering the device's scrambled 3x3 `PinMatrixRequest` — the emulator
/// shuffles the keypad and the host must map the digits to the shuffled matrix
/// positions (obtainable via DebugLink in debug mode). That matrix handling is
/// out of scope for this host-passphrase harness and would be flaky, so this
/// test is ignored. The PIN plumbing itself
/// (`EnterTrezorPin` / `HwRpcTaskUserAction::TrezorPin` /
/// `PinMatrixRequest::ack_pin`) is exercised by the connect-processor path and
/// by the mutual action-type guard in T50.7. Run explicitly with
/// `--ignored` once a matrix-mapping helper is added.
#[test]
#[ignore = "requires DebugLink PIN-matrix mapping; see doc comment (T50.5)"]
fn emulator_trezor_withdraw_pin_user_action() {
    // Intentionally a documented skip rather than a live assertion: driving a
    // real PIN needs the emulator's shuffled 3x3 matrix positions (read from
    // the DebugLink layout) to translate the PIN digits before submitting via
    // `HwRpcTaskUserAction::TrezorPin`. That matrix-mapping helper is not yet
    // implemented, so running this test would not exercise a genuine PIN
    // round-trip. The PIN *shipped* path is already covered: the awaiting ->
    // user_action -> ack mechanism is identical to the passphrase round-trip
    // validated by T50.6 (both flow through `TrezorResponse::process`), the
    // mutual action-type guard is validated by T50.7, and PIN-during-signing
    // uses the same `sign_eth_tx_with_processor` path. Once a matrix-mapping
    // helper exists, replace this body with the live round-trip and run with
    // `--ignored`.
    println!("SKIP emulator_trezor_withdraw_pin_user_action (T50.5): PIN-matrix mapping helper not implemented");
}

// ---------------------------------------------------------------------------
// EVM Trezor activation (task::enable_eth policy) — CRD ch35/ch48/ch50
// ---------------------------------------------------------------------------

/// A minimal [`TrezorConnectProcessor`] that auto-acks every connect / button /
/// ready event and answers a `PassphraseRequest` with an empty passphrase (the
/// clean SLIP-14 device never asks). A PIN request is unexpected and fails
/// loudly. Used to exercise the activation device-read path without spinning up
/// the full platform-activation RPC task.
struct AutoConnectProcessor;

#[async_trait::async_trait]
impl TrezorRequestProcessor for AutoConnectProcessor {
    type Error = String;

    async fn on_button_request(&self) -> MmResult<(), TrezorProcessingError<String>> { Ok(()) }

    async fn on_pin_request(&self) -> MmResult<TrezorPinMatrix3x3Response, TrezorProcessingError<String>> {
        MmError::err(TrezorProcessingError::ProcessorError(
            "unexpected PIN request during activation".to_owned(),
        ))
    }

    async fn on_passphrase_request(&self) -> MmResult<String, TrezorProcessingError<String>> { Ok(String::new()) }

    async fn on_ready(&self) -> MmResult<(), TrezorProcessingError<String>> { Ok(()) }
}

#[async_trait::async_trait]
impl TrezorConnectProcessor for AutoConnectProcessor {
    async fn on_connect(&self) -> MmResult<Duration, HwProcessingError<String>> { Ok(Duration::from_secs(60)) }

    async fn on_connected(&self) -> MmResult<(), HwProcessingError<String>> { Ok(()) }

    async fn on_connection_failed(&self) -> MmResult<(), HwProcessingError<String>> { Ok(()) }
}

/// Activate a native EVM coin under the Trezor signing policy: the coin's
/// address + account public key are sourced FROM THE DEVICE at activation
/// (`eth_coin_activate_with_trezor`, the routine the `task::enable_eth`
/// Trezor policy drives), and the activated coin's address must equal the
/// device's SLIP-14 EVM address. Then a full withdraw is driven through the
/// activated coin and the recovered sender of the signed tx must equal that
/// same device address (activate -> withdraw end-to-end).
#[test]
fn emulator_trezor_activate_eth_native() {
    if !emulator_available("emulator_trezor_activate_eth_native") {
        return;
    }
    emulator_setup();
    let _restore = Slip14Restore;

    let material = fetch_device_material();
    let device_addr = material.eth_address;
    println!("DEVICE_ADDR = {:#x}", device_addr);
    let ctx = ctx_with_hw(material.client, material.internal_pubkey);

    // 100 ETH balance, nonce 0 — enough to later withdraw 1 ETH + gas.
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");

    let conf = serde_json::json!({
        "coin": "ETH",
        "name": "ethereum",
        "chain_id": 1,
        "protocol": {"type": "ETH"},
    });
    let req = serde_json::json!({
        "urls": [node.url.clone()],
        "swap_contract_address": "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94",
        "tx_history": false,
    });

    let processor = AutoConnectProcessor;
    let coin = block_on(super::eth_coin_activate_with_trezor(
        &ctx,
        "ETH",
        &conf,
        &req,
        crate::CoinProtocol::ETH { chain_id: Some(1) },
        &processor,
    ))
    .expect("activate eth under Trezor policy");

    // R50.1 / R50.4: the activated coin's address is the device SLIP-14 address.
    let activated_addr = coin.my_address().expect("activated coin address");
    println!("ACTIVATED_ADDR = {}", activated_addr);
    assert_eq!(
        activated_addr.to_lowercase(),
        format!("{:#x}", device_addr).to_lowercase(),
        "activated coin address must be the device SLIP-14 EVM address"
    );

    // Full activate -> withdraw: the activated (Trezor-signer) coin signs a
    // withdrawal on the device; the recovered sender must be the same address.
    let withdraw_req = WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        amount: 1.into(),
        from: None,
        to: RECIPIENT.to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let _confirm = ConfirmLoop::spawn(120);
    let details =
        drive_withdraw(&ctx, coin, withdraw_req, Duration::from_secs(120)).expect("activated coin withdraw Ok");

    let expected_from = checksum_address(&format!("{:#02x}", device_addr));
    println!("FROM = {:?}", details.from);
    println!("TX_HASH = {}", details.tx_hash);
    assert_eq!(details.from, vec![expected_from]);
    assert!(!details.tx_hex.0.is_empty(), "tx_hex must be present");

    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("decode signed tx");
    println!("RECOVERED_SENDER = {:#x}", signed.sender());
    assert_eq!(
        signed.sender(),
        device_addr,
        "recovered sender of the activated-coin withdraw must be the device address"
    );
}

// ---------------------------------------------------------------------------
// task::get_new_address under the Trezor policy (CRD ch.49 §49.2.3, R49.39, R49.40, R49.42; T49.37)
// ---------------------------------------------------------------------------

/// An emulator-backed platform coin of the Trezor policy over a real (in-memory) wallet storage that holds account 0
/// with one known address, registered on `ctx`. The returned `ctx` has the storage database pinned.
fn trezor_platform_with_storage(ctx: &MmArc, material: &DeviceMaterial, node_url: String) -> EthCoin {
    use crate::hd_wallet_storage::{HDWalletCoinStorage, HDWalletCoinWithStorageOps};

    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("an in-memory database");
    let _ = ctx
        .sqlite_connection
        .pin(StdArc::new(std::sync::Mutex::new(connection)));
    let hw_ctx = activation_hw_ctx(ctx);
    let storage = block_on(HDWalletCoinStorage::init_for_device(ctx, "ETH".to_owned(), &hw_ctx))
        .expect("the device's wallet storage opens");
    let platform = device_trezor_hd_coin_with_storage(
        ctx,
        EthCoinType::Eth,
        node_url,
        material.eth_account_xpub.clone(),
        material.eth_address,
        material.eth_public.clone(),
        1,
        storage,
    );
    let DerivationMethod::HDWallet(wallet) = &platform.derivation_method else {
        unreachable!("an HD coin")
    };
    let account_item = block_on(wallet.accounts.lock())[&0].to_storage_item();
    block_on(platform.upload_new_account(wallet, account_item)).expect("account 0 is stored");
    let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::EthCoin(platform.clone()))).unwrap();
    platform
}

fn known_external_addresses(platform: &EthCoin) -> u32 {
    use crate::hd_wallet::HDAccountOps;
    match &platform.derivation_method {
        DerivationMethod::HDWallet(wallet) => block_on(wallet.accounts.lock())[&0]
            .known_addresses_number(Bip44Chain::External)
            .expect("the external chain"),
        _ => unreachable!("an HD coin"),
    }
}

fn get_new_address_status(ctx: &MmArc, task_id: u64) -> Option<serde_json::Value> {
    use crate::rpc_command::init_get_new_address::init_get_new_address_status;
    block_on(init_get_new_address_status(
        ctx.clone(),
        serde_json::from_value(serde_json::json!({ "task_id": task_id, "forget_if_finished": false })).unwrap(),
    ))
    .ok()
    .map(|status| serde_json::to_value(&status).unwrap())
}

/// Polls the task until its status satisfies `done`, or panics after `timeout`.
fn wait_for_status(
    ctx: &MmArc,
    task_id: u64,
    timeout: Duration,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        if let Some(status) = get_new_address_status(ctx, task_id) {
            println!("  status: {status}");
            if done(&status) {
                return status;
            }
        }
        assert!(
            start.elapsed() < timeout,
            "the task did not reach the expected status in {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// T49.37 (CRD ch.49 R49.39, R49.42): under the Trezor policy the status shows
/// `{"ConfirmAddress": {"expected_address"}}` while the device waits for the button, the address is not recorded
/// meanwhile, and the button press completes the task with the answer of the direct method; the address of that answer
/// is the expected one. Needs the emulator (see `emulator_available`).
#[test]
fn emulator_trezor_get_new_address_shows_the_address_and_records_it_after_the_button() {
    use crate::rpc_command::init_get_new_address::init_get_new_address;

    if !emulator_available("emulator_trezor_get_new_address_shows_the_address_and_records_it_after_the_button") {
        return;
    }
    emulator_setup();
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());
    assert_eq!(known_external_addresses(&platform), 1);

    let request = serde_json::from_value(serde_json::json!({ "coin": "ETH", "account_id": 0 })).unwrap();
    let init = block_on(init_get_new_address(ctx.clone(), request)).unwrap_or_else(|e| panic!("init failed: {}", e));

    // Nobody presses the button yet: the status shows the address to confirm.
    let status = wait_for_status(&ctx, init.task_id, Duration::from_secs(60), |status| {
        status["status"] == "InProgress" && status["details"].get("ConfirmAddress").is_some()
    });
    let expected = status["details"]["ConfirmAddress"]["expected_address"]
        .as_str()
        .expect("the expected address")
        .to_owned();
    assert_eq!(expected, checksum_address(&expected.to_lowercase()), "EIP-55 form");
    assert_eq!(
        known_external_addresses(&platform),
        1,
        "nothing is recorded before the confirmation"
    );

    let _confirm = ConfirmLoop::spawn(60);
    let status = wait_for_status(&ctx, init.task_id, Duration::from_secs(60), |status| {
        matches!(status["status"].as_str(), Some("Ok") | Some("Error"))
    });
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(status["details"]["new_address"]["address"], expected.as_str());
    assert_eq!(status["details"]["new_address"]["derivation_path"], "m/44'/60'/0'/0/1");
    assert_eq!(
        known_external_addresses(&platform),
        2,
        "the address is recorded after the press"
    );
}

/// CRD ch.49 R49.40 items 1 and 2: cancelling while the device shows the address answers success, the task is gone
/// (`NoSuchTask`), nothing is recorded, and the next task offers the same expected address. Needs the emulator.
#[test]
fn emulator_trezor_get_new_address_cancel_while_the_address_is_shown_records_nothing() {
    use crate::rpc_command::init_get_new_address::{init_get_new_address, init_get_new_address_cancel};

    if !emulator_available("emulator_trezor_get_new_address_cancel_while_the_address_is_shown_records_nothing") {
        return;
    }
    emulator_setup();
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());

    let mut offered = Vec::new();
    for _ in 0..2 {
        let request = serde_json::from_value(serde_json::json!({ "coin": "ETH", "account_id": 0 })).unwrap();
        let init =
            block_on(init_get_new_address(ctx.clone(), request)).unwrap_or_else(|e| panic!("init failed: {}", e));
        let status = wait_for_status(&ctx, init.task_id, Duration::from_secs(60), |status| {
            status["status"] == "InProgress" && status["details"].get("ConfirmAddress").is_some()
        });
        offered.push(status["details"]["ConfirmAddress"]["expected_address"].clone());

        let cancel = serde_json::from_value(serde_json::json!({ "task_id": init.task_id })).unwrap();
        block_on(init_get_new_address_cancel(ctx.clone(), cancel)).unwrap_or_else(|e| panic!("cancel failed: {}", e));
        assert!(
            get_new_address_status(&ctx, init.task_id).is_none(),
            "the task is gone after the cancel"
        );
        assert_eq!(known_external_addresses(&platform), 1, "nothing is recorded");
    }
    assert_eq!(offered[0], offered[1], "the next request offers the same address");
}

// ---------------------------------------------------------------------------
// task::get_new_address: more device runs (T49.37 for a UTXO coin, T49.38)
// ---------------------------------------------------------------------------

/// The BIP-44 path of the UTXO fixture coin (Komodo, SLIP-44 coin type 141) and its account 0.
const UTXO_COIN_PATH: &str = "m/44'/141'";
const UTXO_ACCOUNT_PATH: &str = "m/44'/141'/0'";

/// Reads the extended public key of account 0 of the UTXO fixture coin from the device (the activation's read).
fn fetch_utxo_account_xpub(client: &TrezorClient) -> Secp256k1ExtendedPublicKey {
    block_on(async {
        let mut session = client.session().await.expect("open session");
        let xpub = session
            .get_public_key(
                DerivationPath::from_str(UTXO_ACCOUNT_PATH).unwrap(),
                TrezorUtxoCoin::Komodo,
                EcdsaCurve::Secp256k1,
            )
            .await
            .expect("get_public_key call")
            .ack_all()
            .await
            .expect("resolve the account xpub");
        Secp256k1ExtendedPublicKey::from_str(&xpub).expect("parse the account xpub")
    })
}

/// An emulator-backed UTXO coin (`KMD`, standard address format) of the Trezor policy over a real (in-memory) wallet
/// storage that holds account 0 with one known address, registered on `ctx`. Its node is the Bitcoin-style JSON-RPC
/// endpoint `node_url` (the native client reads balances with `listunspent`).
fn trezor_utxo_coin_with_storage(
    ctx: &MmArc,
    material: &DeviceMaterial,
    node_url: String,
) -> crate::utxo::utxo_standard::UtxoStandardCoin {
    use crate::hd_wallet_storage::HDWalletCoinWithStorageOps;
    use crate::utxo::rpc_clients::{NativeClient, NativeClientImpl, UtxoRpcClientEnum};
    use crate::utxo::utxo_tests::{utxo_coin_fields_for_test, utxo_coin_from_fields};
    use crate::utxo::{UtxoAddressFormat, UtxoHDAccount, UtxoHDWallet};

    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("an in-memory database");
    let _ = ctx
        .sqlite_connection
        .pin(StdArc::new(std::sync::Mutex::new(connection)));
    let hw_ctx = activation_hw_ctx(ctx);
    let storage = block_on(HDWalletCoinStorage::init_for_device(ctx, "KMD".to_owned(), &hw_ctx))
        .expect("the device's wallet storage opens");

    let client = NativeClient(StdArc::new(NativeClientImpl {
        coin_ticker: "KMD".to_owned(),
        uri: node_url,
        auth: "Basic dGVzdDp0ZXN0".to_owned(),
        ..NativeClientImpl::default()
    }));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.conf.ticker = "KMD".to_owned();
    fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
    fields.priv_key_policy = crate::PrivKeyPolicy::Trezor;
    fields.hw_ctx = Some(hw_ctx);

    let coin_path = crypto::HDPathToCoin::from_str(UTXO_COIN_PATH).unwrap();
    let account = UtxoHDAccount {
        account_id: 0,
        extended_pubkey: fetch_utxo_account_xpub(&material.client),
        account_derivation_path: crypto::HDPathToAccount::from_str(UTXO_ACCOUNT_PATH).unwrap(),
        external_addresses_number: 1,
        internal_addresses_number: 0,
    };
    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: storage,
        address_format: UtxoAddressFormat::Standard,
        derivation_path: coin_path,
        accounts: HDAccountsMutex::new(BTreeMap::from([(0, account.clone())])),
        gap_limit: 20,
    });
    let coin = utxo_coin_from_fields(fields);
    let DerivationMethod::HDWallet(wallet) = &coin.as_ref().derivation_method else {
        unreachable!("an HD coin")
    };
    block_on(coin.upload_new_account(wallet, account.to_storage_item())).expect("account 0 is stored");
    let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::UtxoCoin(coin.clone()))).unwrap();
    coin
}

fn known_utxo_external_addresses(coin: &crate::utxo::utxo_standard::UtxoStandardCoin) -> u32 {
    match &coin.as_ref().derivation_method {
        DerivationMethod::HDWallet(wallet) => block_on(wallet.accounts.lock())[&0].external_addresses_number,
        _ => unreachable!("an HD coin"),
    }
}

fn start_get_new_address(ctx: &MmArc, coin: &str) -> u64 {
    use crate::rpc_command::init_get_new_address::init_get_new_address;

    let request = serde_json::from_value(serde_json::json!({ "coin": coin, "account_id": 0 })).unwrap();
    block_on(init_get_new_address(ctx.clone(), request))
        .unwrap_or_else(|e| panic!("init failed: {}", e))
        .task_id
}

/// Waits until the device shows the address and returns the `expected_address` of the status.
fn wait_for_confirm_address(ctx: &MmArc, task_id: u64) -> String {
    let status = wait_for_status(ctx, task_id, Duration::from_secs(60), |status| {
        status["status"] == "InProgress" && status["details"].get("ConfirmAddress").is_some()
    });
    status["details"]["ConfirmAddress"]["expected_address"]
        .as_str()
        .expect("the expected address")
        .to_owned()
}

fn wait_for_end(ctx: &MmArc, task_id: u64, timeout: Duration) -> serde_json::Value {
    wait_for_status(ctx, task_id, timeout, |status| {
        matches!(status["status"].as_str(), Some("Ok") | Some("Error"))
    })
}

/// T49.37 for a UTXO coin (CRD ch.49 R49.39, R49.40, R49.42): the device is asked with the coin's `trezor_coin` and
/// the script type of its address format (a wrong pair would show another address), the status shows the address the
/// daemon derived, nothing is recorded before the button, and the press completes the task with that address. The
/// node is a Bitcoin-style stub that answers an empty `listunspent`. Needs the emulator.
#[test]
fn emulator_trezor_get_new_address_utxo_shows_the_address_and_records_it_after_the_button() {
    if !emulator_available("emulator_trezor_get_new_address_utxo_shows_the_address_and_records_it_after_the_button") {
        return;
    }
    emulator_setup();
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_empty_utxo_node();
    let coin = trezor_utxo_coin_with_storage(&ctx, &material, node.url.clone());
    assert_eq!(known_utxo_external_addresses(&coin), 1);

    let task_id = start_get_new_address(&ctx, "KMD");
    let expected = wait_for_confirm_address(&ctx, task_id);
    assert!(expected.starts_with('R'), "a Komodo address: {expected}");
    assert_eq!(
        known_utxo_external_addresses(&coin),
        1,
        "nothing is recorded before the confirmation"
    );

    let _confirm = ConfirmLoop::spawn(60);
    let status = wait_for_end(&ctx, task_id, Duration::from_secs(60));
    assert_eq!(status["status"], "Ok", "{status}");
    let new_address = &status["details"]["new_address"];
    assert_eq!(new_address["address"], expected.as_str());
    assert_eq!(new_address["derivation_path"], "m/44'/141'/0'/0/1");
    assert_eq!(new_address["chain"], "External");
    assert_eq!(
        new_address["balance"],
        serde_json::json!({ "KMD": { "spendable": "0", "unspendable": "0" } })
    );
    assert_eq!(
        known_utxo_external_addresses(&coin),
        2,
        "the address is recorded after the press"
    );
}

/// T49.38, the refusal (CRD ch.49 R49.41 item 4, ch.05 R35c): the user presses "no" on the device while it shows the
/// address. The task ends with `HwError` `UserCancelled` and the known count is unchanged. The controller's NO button
/// is `DebugLinkDecision` with `DebugButton.NO`. Needs the emulator.
///
/// `#[ignore]`d: on the pinned T2T1 emulator (`2-main`) the "Receive address" screen has a single confirm button and
/// no cancel. The debug link's NO opens an "Address mismatch?" screen (a back button and a "Quit" button); YES there
/// ("Quit") ends the emulator process, so the host never gets `UserCancelled`. The refusal is
/// open, to be checked by hand on a Trezor (CRD ch.49 T49.38 as-built note). Run with `--ignored` to see it.
#[test]
#[ignore = "the T2T1 emulator's address screen has no cancel: NO opens an Address mismatch screen whose Quit ends the emulator (T49.38)"]
fn emulator_trezor_get_new_address_refused_on_the_device_is_user_cancelled() {
    if !emulator_available("emulator_trezor_get_new_address_refused_on_the_device_is_user_cancelled") {
        return;
    }
    emulator_setup();
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());

    let task_id = start_get_new_address(&ctx, "ETH");
    wait_for_confirm_address(&ctx, task_id);

    let _refuse = ConfirmLoop::spawn_deciding(60, "no");
    let status = wait_for_end(&ctx, task_id, Duration::from_secs(60));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "HwError", "{status}");
    assert_eq!(status["details"]["error_data"], "UserCancelled", "{status}");
    assert_eq!(
        known_external_addresses(&platform),
        1,
        "the refused address is not recorded"
    );
}

/// T49.38, another seed (CRD ch.05 R58a): the emulator is wiped and loaded with another seed after the coin was
/// activated. The device-identity check ends the task with `HwError` `FoundUnexpectedDevice` before the address is
/// shown, and the known count is unchanged. The reconnect the check triggers reaches the emulator through the
/// `TREZOR_EMULATOR_UDP` shim, so the test needs that variable; CI sets it, so the check never skips there.
#[test]
fn emulator_trezor_get_new_address_another_seed_is_found_unexpected_device() {
    if !emulator_available("emulator_trezor_get_new_address_another_seed_is_found_unexpected_device") {
        return;
    }
    if std::env::var("TREZOR_EMULATOR_UDP")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        println!("SKIP emulator_trezor_get_new_address_another_seed_is_found_unexpected_device: TREZOR_EMULATOR_UDP is not set");
        return;
    }
    emulator_setup();
    // Restore the clean SLIP-14 state on the way out (even on panic).
    let _restore = Slip14Restore;
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());

    emulator_setup_with_mnemonic(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    );

    let task_id = start_get_new_address(&ctx, "ETH");
    let status = wait_for_end(&ctx, task_id, Duration::from_secs(90));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "HwError", "{status}");
    assert_eq!(status["details"]["error_data"], "FoundUnexpectedDevice", "{status}");
    assert_eq!(known_external_addresses(&platform), 1);
}

/// Answers every passphrase request of the task in turn with the next of `passphrases` (the last one repeats), and
/// starts a confirm loop after the first answer, as the withdraw driver does. Returns the terminal status and the
/// number of passphrase requests seen.
fn drive_with_passphrases(
    ctx: &MmArc,
    task_id: u64,
    passphrases: &[&str],
    timeout: Duration,
) -> (serde_json::Value, usize) {
    use crate::rpc_command::init_get_new_address::init_get_new_address_user_action;

    let start = Instant::now();
    let mut answered = 0usize;
    let mut answer_pending = false;
    let mut confirm: Option<ConfirmLoop> = None;
    loop {
        let status = get_new_address_status(ctx, task_id).expect("the task exists");
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            return (status, answered);
        }
        let awaiting = status["status"] == "UserActionRequired" && status["details"] == "EnterTrezorPassphrase";
        if !awaiting {
            answer_pending = false;
        } else if !answer_pending {
            let passphrase = passphrases[answered.min(passphrases.len() - 1)];
            let request = serde_json::from_value(serde_json::json!({
                "task_id": task_id,
                "user_action": { "action_type": "TrezorPassphrase", "passphrase": passphrase }
            }))
            .unwrap();
            block_on(init_get_new_address_user_action(ctx.clone(), request))
                .unwrap_or_else(|e| panic!("the passphrase was refused: {}", e));
            answered += 1;
            answer_pending = true;
            if confirm.is_none() {
                confirm = Some(ConfirmLoop::spawn(120));
            }
        }
        assert!(start.elapsed() < timeout, "the task did not end in {timeout:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// T49.38, the mismatch (decision: `DeviceAddressMismatch`). The same device answers for another passphrase than the
/// one the coin was activated with. The task opens a device session twice (the identity check, then the address
/// request) and the device asks for the passphrase in each, so the first is answered with the activation's
/// passphrase (the check passes) and the second with another one: the device shows the address of the other hidden
/// wallet, and after the button the task ends with `DeviceAddressMismatch` naming both addresses. The count is
/// unchanged. If the device asks only once the task completes and the test fails with that message. Needs the
/// emulator.
#[test]
fn emulator_trezor_get_new_address_another_passphrase_is_a_device_address_mismatch() {
    if !emulator_available("emulator_trezor_get_new_address_another_passphrase_is_a_device_address_mismatch") {
        return;
    }
    emulator_setup_passphrase();
    // Restore the clean SLIP-14 state on the way out (even on panic).
    let _restore = Slip14Restore;

    let material = fetch_device_material_with_passphrase("pp-one");
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_mock_node("0x56bc75e2d63100000", "0x0", "0x0");
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());

    let task_id = start_get_new_address(&ctx, "ETH");
    let (status, requests) = drive_with_passphrases(&ctx, task_id, &["pp-one", "pp-two"], Duration::from_secs(180));
    let ended_in_error = status["status"] == "Error";
    assert!(
        ended_in_error,
        "the task did not end in an error; a mismatch needs a second passphrase request"
    );
    assert!(
        requests >= 2,
        "the device asked for the passphrase fewer than two times"
    );
    assert_eq!(
        status["details"]["error_type"], "DeviceAddressMismatch",
        "the task ended with another error type"
    );
    let data = &status["details"]["error_data"];
    let expected = data["expected_address"].as_str().expect("the expected address");
    let device = data["device_address"].as_str().expect("the device's address");
    let addresses_differ = !expected.eq_ignore_ascii_case(device);
    assert!(addresses_differ, "the two addresses do not differ");
    assert_eq!(known_external_addresses(&platform), 1, "nothing is recorded");
}

/// T49.38, a failing balance query after the confirmation (CRD ch.49 R49.37 item 3, R49.40 item 3): the node answers
/// an error to the balance read once the button is pressed. The task ends with `Transport`, and the address stays
/// known (the count is raised: no rollback). Needs the emulator.
#[test]
fn emulator_trezor_get_new_address_a_failing_balance_query_leaves_the_address_known() {
    if !emulator_available("emulator_trezor_get_new_address_a_failing_balance_query_leaves_the_address_known") {
        return;
    }
    emulator_setup();
    let material = fetch_device_material();
    let ctx = ctx_with_hw(material.client.clone(), material.internal_pubkey);
    let node = spawn_failing_node();
    let platform = trezor_platform_with_storage(&ctx, &material, node.url.clone());

    let task_id = start_get_new_address(&ctx, "ETH");
    wait_for_confirm_address(&ctx, task_id);
    assert_eq!(known_external_addresses(&platform), 1);

    let _confirm = ConfirmLoop::spawn(60);
    let status = wait_for_end(&ctx, task_id, Duration::from_secs(120));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "Transport", "{status}");
    assert_eq!(
        known_external_addresses(&platform),
        2,
        "the address stays known after the failed balance query"
    );
}
