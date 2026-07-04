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
//! If the emulator is unreachable the tests print a skip message and pass.
//!
//! Coverage of the §50.8 acceptance tests in this module:
//!   * T50.1 / T50.4 — `emulator_trezor_withdraw_native_no_from`
//!   * T50.2         — `emulator_trezor_withdraw_erc20_with_from_path`
//!   * T50.5         — `emulator_trezor_withdraw_pin_user_action` (`#[ignore]`; see below)
//!   * T50.6         — `emulator_trezor_withdraw_passphrase_user_action`
//!   * T50.7         — `emulator_trezor_withdraw_passphrase_action_mismatch`
//!   * T50.10        — `emulator_trezor_withdraw_bad_from_selector`
//!   * T50.11        — `emulator_trezor_withdraw_tron_unsupported`
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

/// Start + wipe + seed the emulator with the SLIP-14 mnemonic (idempotent).
fn emulator_setup() {
    let status = Command::new("python3")
        .arg(script_path())
        .arg("setup")
        .status()
        .expect("failed to spawn emulator_control.py setup");
    assert!(status.success(), "emulator_control.py setup failed: {status}");
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
    fn spawn(timeout_secs: u32) -> ConfirmLoop {
        let child = Command::new("python3")
            .arg(script_path())
            .arg("confirm-loop")
            .arg("--timeout")
            .arg(timeout_secs.to_string())
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock node");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    handle_conn(stream, native_balance_hex, nonce_hex, erc20_balance_hex);
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

fn handle_conn(mut stream: TcpStream, native_balance_hex: &str, nonce_hex: &str, erc20_balance_hex: &str) {
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
        serde_json::Value::Array(items) => {
            let replies: Vec<serde_json::Value> = items
                .iter()
                .map(|it| rpc_reply(it, native_balance_hex, nonce_hex, erc20_balance_hex))
                .collect();
            serde_json::Value::Array(replies)
        },
        other => rpc_reply(other, native_balance_hex, nonce_hex, erc20_balance_hex),
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
            {"coin":"JST","name":"jst","rpcport":80,"mm2":1,"protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
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
    let web3 = crate::eth::alloy_compat::build_provider(vec![url], vec![]).unwrap();
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
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        chain_id: Some(1),
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method,
        swap_v2_contracts: None,
        gas_limit_v2: EthGasLimitV2::default(),
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
        external_addresses_number: 1,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert(0, account);

    let signer = EthSigner::Trezor(EthTrezorSigner {
        derivation_path: DerivationPath::from_str(EVM_PATH).unwrap(),
        address: leaf_address,
        public: leaf_public,
    });
    let derivation_method = DerivationMethod::HDWallet(EthHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
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
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_native_no_from: emulator unreachable at {addr}");
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

/// T50.2: ERC20 Trezor withdraw with a derivation-path `from` selector resolving
/// to the activated default HD address. Exercises the `transfer(address,uint256)`
/// ABI payload (the `EthereumTxAck` streaming path) and device signing.
#[test]
fn emulator_trezor_withdraw_erc20_with_from_path() {
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_erc20_with_from_path: emulator unreachable at {addr}");
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
    let coin = device_trezor_hd_coin(
        &ctx,
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_str(TOKEN_ADDR.trim_start_matches("0x")).unwrap(),
        },
        node.url.clone(),
        material.eth_account_xpub.clone(),
        device_addr,
        material.eth_public,
    );

    let req = WithdrawRequest {
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::DerivationPath {
            derivation_path: EVM_PATH.to_string(),
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

    let expected_from = checksum_address(&format!("{:#02x}", device_addr));
    println!("FROM = {:?}", details.from);
    println!("TX_HASH = {}", details.tx_hash);
    assert_eq!(details.from, vec![expected_from]);
    assert!(!details.tx_hex.0.is_empty(), "tx_hex must be present");
    assert!(!details.tx_hash.is_empty(), "tx_hash must be present");

    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("decode signed tx");
    println!("RECOVERED_SENDER = {:#x}", signed.sender());
    assert_eq!(
        signed.sender(),
        device_addr,
        "recovered sender must be the resolved address"
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

/// T50.11: a TRON-family coin under the Trezor policy reaches a terminal
/// unsupported error with NO device interaction (rejected before any exchange).
#[test]
fn emulator_trezor_withdraw_tron_unsupported() {
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_tron_unsupported: emulator unreachable at {addr}");
        return;
    }
    // No emulator_setup / no device material / no confirm-loop: the rejection is
    // raised before any device exchange (R50.20).
    let conf = serde_json::json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [] });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let _crypto = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor emulator test passphrase").unwrap();

    let node = spawn_mock_node("0x0", "0x0", "0x0");
    let dummy = Address::from_str("0000000000000000000000000000000000000001").unwrap();
    let coin = device_trezor_iguana_coin(&ctx, EthCoinType::Tron, node.url.clone(), dummy, Public::default());

    let req = WithdrawRequest {
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
    assert!(matches!(err, WithdrawError::UnsupportedUnderTrezor(_)));
}

/// T50.10: an unresolvable / foreign `from` selector fails with the ch. 49
/// sender-selector discriminant BEFORE any device exchange (no confirm-loop).
#[test]
fn emulator_trezor_withdraw_bad_from_selector() {
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_bad_from_selector: emulator unreachable at {addr}");
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
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_passphrase_user_action: emulator unreachable at {addr}");
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
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_withdraw_passphrase_action_mismatch: emulator unreachable at {addr}");
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
    let addr = emulator_addr();
    if !emulator_reachable(&addr) {
        println!("SKIP emulator_trezor_activate_eth_native: emulator unreachable at {addr}");
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
