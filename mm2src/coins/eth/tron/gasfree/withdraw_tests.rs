//! Tests of the gasless TRC20 withdraw (CRD ch.63 T8, T11, T12, T13, T15, T16, T17, and T18 to T20 with the
//! submission flow of D63.9) through the withdraw RPCs.
//!
//! The TRON node and the GasFree provider are loopback stubs; the coins are built the way the mmrpc 2.0 activation
//! builds them and registered, so that a test calls the direct `withdraw` method and `task::withdraw::init` as a
//! client does. TRON activation is on (`TRON_ACTIVATION_AVAILABLE`), so a client reaches the same rail.
//!
//! No assertion message of this module carries a credential, a signature, a rendering or an error text: only
//! labels.

use super::super::hd_tests::{enable_wallet, global_hd, hd_ctx, ADDRESS_0, ADDRESS_1};
use super::super::nodes::TronNode;
use super::super::test_node::{Reply, StubNode};
use super::super::wallet_path_tests::NOW_BLOCK_BODY;
use super::super::withdraw::{withdraw_tron, GUARD_ALLOWS_FALLBACK_KEY};
use super::super::{parse_trc20_contract, tron_coin_from_params, tron_coin_from_params_with_global_hd, Network,
                   TronAddress, TronCoinParams, TronGaslessProviderRequest, TronTokenGaslessRequest};
use super::address_word;
use super::authorization::{sign_permit, typed_data_hash, GasFreeRelayPayload, PermitTransfer, SignedPermit};
use super::binding::{gasfree_custody_address, GasFreeBinding};
use super::networks::gasfree_network;
use super::test_vectors::USERS;
use super::withdraw::{submit, track, MIN_POLL_INTERVAL_MS, SETTLEMENT_GRACE_MS};
use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
use crate::eth::eth_tests::eth_coin_for_test;
use crate::eth::{EthCoin, EthCoinType, EvmWithdrawSender};
use crate::rpc_command::get_my_address::PathToAddress;
use crate::rpc_command::init_withdraw::{init_withdraw, withdraw_cancel, withdraw_status, WithdrawCompatRpcStatus,
                                        WithdrawInProgressStatus, WithdrawStatusRequest};
use crate::{lp_register_coin, CoinProtocol, GaslessWithdrawError, MmCoinEnum, RegisterCoinParams, TransactionDetails,
            TxFeeDetails, WithdrawError, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::{block_on, now_ms, HttpStatusCode};
use ethereum_types::{H256, U256};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_eth::keys::{verify_address, Secret, Signature};
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const API_KEY: &str = "key-0f3a9c";
const API_SECRET: &str = "secret-7b1d4e";
const TOKEN: &str = "USDT-TRC20";
const TRC20_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const RECEIVER: &str = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";
const GET_GASFREE_ADDRESS: &str = "getGasFreeAddress(address)";
const TRANSFER: &str = "transfer(address,uint256)";
const BALANCE_OF: &str = "balanceOf(address)";
const SUBMIT_PATH: &str = "/nile/api/v1/gasfree/submit";
const TRACE_PATH_PREFIX: &str = "/nile/api/v1/gasfree/";
/// The trace id of the stub provider and the hash of its transaction.
const TRACE_ID: &str = "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0";
/// The advice the error of a submission with an unknown outcome ends with.
const MAYBE_ACCEPTED: &str = "The provider may have accepted the transfer; check the GasFree address balance and its \
                              history before trying again";
const TXN_HASH: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
/// The provider's fees of the stub, in the smallest unit of the 6-decimal token: 2 and 1 tokens.
const TRANSFER_FEE: u64 = 2_000_000;
const ACTIVATE_FEE: u64 = 1_000_000;
const PARAMETERS: &str = r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateNewAccountFeeInSystemContract","value":1000000},{"key":"getCreateAccountFee","value":100000}]}"#;

fn assert_no_credential(text: &str) {
    assert!(!text.contains(API_KEY), "the API key is rendered");
    assert!(!text.contains(API_SECRET), "the API secret is rendered");
}

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).expect("a decimal") }

/// The decimal a JSON string member holds.
fn dec_of(value: &Json) -> BigDecimal { dec(value.as_str().expect("a decimal string")) }

/// What the controller contract of the stub node answers to `getGasFreeAddress`.
#[derive(Clone, Copy, PartialEq)]
enum Controller {
    /// The address this build derives for the asked user on Nile.
    Agrees,
    /// The address the mainnet constants give.
    Disagrees,
    /// HTTP 500.
    Down,
}

/// What the stub node answers.
#[derive(Clone)]
struct Chain {
    /// The TRX balance of every account, in SUN.
    trx_sun: u64,
    /// The token balance of every sending address, in token units.
    token_units: u64,
    /// The token balance of every other address (the custody addresses), in token units.
    custody_units: u64,
    controller: Controller,
    /// An endpoint that answers HTTP 500.
    failing: Option<&'static str>,
    /// Whether the dry run of the token transfer reverts.
    transfer_reverts: bool,
    /// The sending addresses.
    senders: Vec<TronAddress>,
}

/// What the stub provider answers to the account call.
#[derive(Clone)]
enum Provider {
    /// The account record of the asked account, as [`Account`] describes it.
    Account(Account),
    /// This status and body.
    Raw(u16, String),
    /// The record of [`Account::usual`], after this many milliseconds.
    Slow(u64),
}

#[derive(Clone)]
struct Account {
    active: bool,
    allow_submit: bool,
    nonce: u64,
    /// Whether the record lists the token.
    enrolled: bool,
    decimal: u64,
    frozen: u64,
    /// The custody address the record reports, when it is not the derived one.
    custody: Option<TronAddress>,
}

impl Account {
    /// An activated account with the token enrolled, nothing locked and the nonce 7.
    fn usual() -> Self {
        Account {
            active: true,
            allow_submit: true,
            nonce: 7,
            enrolled: true,
            decimal: 6,
            frozen: 0,
            custody: None,
        }
    }

    /// The success envelope with the record of `account`.
    fn envelope(&self, account: &TronAddress) -> String {
        let custody = self
            .custody
            .unwrap_or_else(|| gasfree_custody_address(&Network::Nile, account));
        let assets = if self.enrolled {
            json!([{
                "tokenAddress": TRC20_CONTRACT,
                "tokenSymbol": "USDT",
                "activateFee": ACTIVATE_FEE,
                "transferFee": TRANSFER_FEE,
                "decimal": self.decimal,
                "frozen": self.frozen
            }])
        } else {
            json!([])
        };
        json!({
            "code": 200,
            "reason": null,
            "message": "",
            "data": {
                "accountAddress": account.to_base58(),
                "gasFreeAddress": custody.to_base58(),
                "active": self.active,
                "nonce": self.nonce,
                "allowSubmit": self.allow_submit,
                "assets": assets
            }
        })
        .to_string()
    }
}

fn node_over(chain: Arc<Mutex<Chain>>) -> StubNode {
    StubNode::start(move |path, body| {
        let chain = chain.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if chain.failing == Some(path) {
            return Reply::status(500, "stub failure");
        }
        let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
        match path {
            "/wallet/getnowblock" => Reply::ok(NOW_BLOCK_BODY),
            "/wallet/getaccount" => {
                Reply::ok(&json!({ "balance": chain.trx_sun, "create_time": 1_700_000_000_000i64 }).to_string())
            },
            "/wallet/getaccountresource" => Reply::ok(r#"{"freeNetLimit":600}"#),
            "/wallet/getchainparameters" => Reply::ok(PARAMETERS),
            "/wallet/triggerconstantcontract" => {
                // The parameter starts with one ABI word: 12 zero bytes and a 20-byte address.
                let parameter = request["parameter"].as_str().unwrap_or_default();
                let asked = parameter
                    .get(24..64)
                    .and_then(|payload| TronAddress::from_hex(&format!("41{payload}")).ok());
                match request["function_selector"].as_str() {
                    Some("decimals()") => Reply::ok(&json!({ "constant_result": [format!("{:064x}", 6)] }).to_string()),
                    Some(BALANCE_OF) => {
                        let units = match asked {
                            Some(owner) if chain.senders.contains(&owner) => chain.token_units,
                            _ => chain.custody_units,
                        };
                        Reply::ok(&json!({ "constant_result": [format!("{units:064x}")] }).to_string())
                    },
                    Some(TRANSFER) if chain.transfer_reverts => Reply::ok(
                        &json!({
                            "result": { "result": true, "message": "REVERT opcode executed" },
                            "constant_result": [""],
                            "energy_used": 8_624
                        })
                        .to_string(),
                    ),
                    Some(TRANSFER) => Reply::ok(
                        &json!({ "result": { "result": true }, "constant_result": ["00"], "energy_used": 14_000 })
                            .to_string(),
                    ),
                    Some(GET_GASFREE_ADDRESS) => {
                        let Some(user) = asked else {
                            return Reply::status(400, "{}");
                        };
                        let answer = match chain.controller {
                            Controller::Agrees => gasfree_custody_address(&Network::Nile, &user),
                            Controller::Disagrees => gasfree_custody_address(&Network::Mainnet, &user),
                            Controller::Down => return Reply::status(500, "{}"),
                        };
                        Reply::ok(&json!({ "constant_result": [hex::encode(address_word(&answer))] }).to_string())
                    },
                    _ => Reply::status(404, "{}"),
                }
            },
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        }
    })
}

/// One answer of the stub provider to a trace call.
#[derive(Clone)]
enum Trace {
    /// The record of the submitted transfer in this `state`, with this `txnHash` and this `txnState`.
    Record(&'static str, Option<&'static str>, Option<&'static str>),
    /// This status and body.
    Raw(u16, &'static str),
}

/// What the stub provider answers to the submit and trace calls.
#[derive(Clone)]
struct Relay {
    /// The answer to a submit call, when it is not the record of the submitted transfer in the state `WAITING`.
    submit_raw: Option<(u16, &'static str)>,
    /// How long the answer to a submit call takes, in milliseconds.
    submit_delay_ms: u64,
    /// Members that replace those of the record in the answer to a submit call.
    submit_patch: Json,
    /// Whether the records have the shape the provider sends on the Nile test network instead of the shape of its
    /// published API: see [`to_live_shape`].
    live_shape: bool,
    /// The answers to the trace calls, one per call; the last one repeats.
    trace: Vec<Trace>,
    /// The body of the last submit call.
    submitted: Option<Json>,
    /// The number of trace calls answered since `trace` was set.
    traced: usize,
}

impl Relay {
    /// A provider that takes the submission and reports the transfer as settled at the first trace call.
    fn usual() -> Self {
        Relay {
            submit_raw: None,
            submit_delay_ms: 0,
            submit_patch: json!({}),
            live_shape: false,
            trace: vec![Trace::Record("SUCCEED", Some(TXN_HASH), Some("ON_CHAIN"))],
            submitted: None,
            traced: 0,
        }
    }

    fn set_trace(&mut self, trace: Vec<Trace>) {
        self.trace = trace;
        self.traced = 0;
    }
}

/// Rewrites `record` into the shape the provider sends on the Nile test network: the times as Unix milliseconds,
/// the integers as numbers, the fee estimate without the `d`, no `version`. A trace answer (`trace`) also has no
/// `signature`, `maxFee` and `updatedAt`, and carries the transaction members it does not know yet as `""`.
fn to_live_shape(record: &mut Json, trace: bool) {
    let Some(object) = record.as_object_mut() else { return };
    for (member, millis) in [
        ("createdAt", 1_795_354_473_207u64),
        ("updatedAt", 1_795_354_473_300),
        ("expiredAt", 1_795_354_773_000),
    ] {
        object.insert(member.to_owned(), json!(millis));
    }
    for member in ["amount", "maxFee", "nonce"] {
        let number = object[member].as_str().and_then(|text| text.parse::<u64>().ok());
        if let Some(number) = number {
            object.insert(member.to_owned(), json!(number));
        }
    }
    object.remove("version");
    if let Some(fee) = object.remove("estimatedTransferFee") {
        object.insert("estimateTransferFee".to_owned(), fee);
    }
    if trace {
        for member in ["signature", "maxFee", "updatedAt"] {
            object.remove(member);
        }
        for member in [
            "txnHash",
            "txnState",
            "txnBlockNum",
            "txnBlockTimestamp",
            "txnTotalCost",
        ] {
            object.entry(member).or_insert(json!(""));
        }
    }
}

/// The success envelope with the provider's record of the transfer the submit body `submitted` asks for. `live`
/// is `None` for the shape of the published API, and says whether the record is a trace answer for the shape of
/// [`to_live_shape`].
fn transfer_envelope(
    submitted: &Json,
    state: &str,
    txn_hash: Option<&str>,
    txn_state: Option<&str>,
    patch: &Json,
    live: Option<bool>,
) -> String {
    let custody = submitted["user"]
        .as_str()
        .and_then(|user| TronAddress::from_base58(user).ok())
        .map(|user| gasfree_custody_address(&Network::Nile, &user).to_base58());
    let mut record = json!({
        "id": TRACE_ID,
        "createdAt": "2026-11-22T10:00:00.000Z",
        "updatedAt": "2026-11-22T10:00:01.000Z",
        "expiredAt": "2026-11-22T10:05:00.000Z",
        "accountAddress": submitted["user"],
        "gasFreeAddress": custody,
        "providerAddress": submitted["serviceProvider"],
        "targetAddress": submitted["receiver"],
        "tokenAddress": submitted["token"],
        "amount": submitted["value"],
        "maxFee": submitted["maxFee"],
        "signature": submitted["sig"],
        "nonce": submitted["nonce"],
        "version": 1,
        "estimatedActivateFee": 0,
        "estimatedTransferFee": TRANSFER_FEE,
        "state": state
    });
    if let Some(txn_hash) = txn_hash {
        record["txnHash"] = json!(txn_hash);
    }
    if let Some(txn_state) = txn_state {
        record["txnState"] = json!(txn_state);
    }
    for (member, value) in patch.as_object().expect("an object").iter() {
        record[member] = value.clone();
    }
    if let Some(trace) = live {
        to_live_shape(&mut record, trace);
    }
    json!({ "code": 200, "reason": null, "message": "", "data": record }).to_string()
}

/// The stub provider's answer to a submit call with `body`.
fn submit_reply(relay: &Mutex<Relay>, body: &str) -> Reply {
    let submitted: Json = serde_json::from_str(body).unwrap_or(Json::Null);
    let script = {
        let mut relay = relay.lock().unwrap_or_else(|e| e.into_inner());
        relay.submitted = Some(submitted.clone());
        relay.clone()
    };
    std::thread::sleep(Duration::from_millis(script.submit_delay_ms));
    match script.submit_raw {
        Some((status, body)) => Reply::status(status, body),
        None => Reply::ok(&transfer_envelope(
            &submitted,
            "WAITING",
            None,
            None,
            &script.submit_patch,
            script.live_shape.then_some(false),
        )),
    }
}

/// The stub provider's answer to a trace call.
fn trace_reply(relay: &Mutex<Relay>) -> Reply {
    let (step, submitted, live) = {
        let mut relay = relay.lock().unwrap_or_else(|e| e.into_inner());
        let index = relay.traced.min(relay.trace.len().saturating_sub(1));
        relay.traced += 1;
        (
            relay.trace.get(index).cloned(),
            relay.submitted.clone(),
            relay.live_shape.then_some(true),
        )
    };
    match (step, submitted) {
        (Some(Trace::Raw(status, body)), _) => Reply::status(status, body),
        (Some(Trace::Record(state, txn_hash, txn_state)), Some(submitted)) => Reply::ok(&transfer_envelope(
            &submitted,
            state,
            txn_hash,
            txn_state,
            &json!({}),
            live,
        )),
        _ => Reply::status(404, r#"{"code":404,"message":"no such transfer"}"#),
    }
}

fn provider_over(provider: Arc<Mutex<Provider>>, relay: Arc<Mutex<Relay>>) -> StubNode {
    StubNode::start(move |path, body| {
        if path == SUBMIT_PATH {
            return submit_reply(&relay, body);
        }
        if path.starts_with(TRACE_PATH_PREFIX) {
            return trace_reply(&relay);
        }
        let mode = provider.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let account = path
            .strip_prefix("/nile/api/v1/address/")
            .and_then(|account| TronAddress::from_base58(account).ok());
        let Some(account) = account else {
            return Reply::status(404, r#"{"code":404,"message":"no such path"}"#);
        };
        match mode {
            Provider::Account(record) => Reply::ok(&record.envelope(&account)),
            Provider::Raw(status, body) => Reply::status(status, &body),
            Provider::Slow(millis) => {
                std::thread::sleep(Duration::from_millis(millis));
                Reply::ok(&Account::usual().envelope(&account))
            },
        }
    })
}

/// How the coins of a [`Lab`] are activated.
#[derive(Clone)]
struct Setup {
    /// Whether the platform has a `tron_gasless_provider`.
    provider: bool,
    /// Whether the token is activated with `gasless.enabled`.
    gasless: bool,
    /// The token's `gasless.transfer_max_fee`.
    transfer_max_fee: Option<&'static str>,
    /// The value of the MM2.json switch, when the configuration has the key.
    switch: Option<Json>,
    request_timeout_ms: Option<u64>,
    /// The provider object's `status_poll_interval_ms`.
    status_poll_interval_ms: Option<u64>,
    /// The provider URL, when it is not the stub's.
    provider_url: Option<String>,
    api_key: &'static str,
    /// Whether the login is the global-HD one of the HD tests.
    hd: bool,
}

impl Setup {
    /// A single-key login, a provider on the platform and a token with the gasless rail enabled and no cap. The
    /// task route asks for a submitted transfer every 20 milliseconds.
    fn usual() -> Self {
        Setup {
            provider: true,
            gasless: true,
            transfer_max_fee: None,
            switch: None,
            request_timeout_ms: None,
            status_poll_interval_ms: Some(20),
            provider_url: None,
            api_key: API_KEY,
            hd: false,
        }
    }
}

/// A node, a provider and the two registered coins.
struct Lab {
    ctx: MmArc,
    node: StubNode,
    provider: StubNode,
    chain: Arc<Mutex<Chain>>,
    provider_mode: Arc<Mutex<Provider>>,
    relay: Arc<Mutex<Relay>>,
    platform: EthCoin,
    token: EthCoin,
}

fn priv_key() -> [u8; 32] {
    // Deterministic non-zero key (do not use in production).
    let mut key = [0u8; 32];
    key[31] = 1;
    key
}

fn single_key_ctx(switch: Option<Json>) -> MmArc {
    let mut conf = json!({
        "coins": [
            {
                "coin": "TRX",
                "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
                "wallet_only": true,
                "mm2": 1
            },
            {
                "coin": TOKEN,
                "protocol": {
                    "type": "TRC20",
                    "protocol_data": { "platform": "TRX", "contract_address": TRC20_CONTRACT }
                },
                "decimals": 6,
                "mm2": 1
            }
        ]
    });
    if let Some(switch) = switch {
        conf[GUARD_ALLOWS_FALLBACK_KEY] = switch;
    }
    MmCtxBuilder::new().with_conf(conf).into_mm_arc()
}

async fn register(ctx: &MmArc, coin: MmCoinEnum, ticker: &str) {
    let registered = lp_register_coin(ctx, coin, RegisterCoinParams {
        ticker: ticker.to_owned(),
        tx_history: false,
    })
    .await;
    assert!(registered.is_ok(), "the coin registers");
}

impl Lab {
    fn new(setup: Setup) -> Self { Self::build(setup, true) }

    /// The lab of `setup`. With `register_token` unset the token coin is built and not registered.
    fn build(setup: Setup, register_token: bool) -> Self {
        let chain = Arc::new(Mutex::new(Chain {
            trx_sun: 50_000_000,
            token_units: 50_000_000,
            custody_units: 50_000_000,
            controller: Controller::Agrees,
            failing: None,
            transfer_reverts: false,
            senders: Vec::new(),
        }));
        let provider_mode = Arc::new(Mutex::new(Provider::Account(Account::usual())));
        let relay = Arc::new(Mutex::new(Relay::usual()));
        let node = node_over(chain.clone());
        let provider = provider_over(provider_mode.clone(), relay.clone());
        let ctx = if setup.hd {
            hd_ctx()
        } else {
            single_key_ctx(setup.switch.clone())
        };

        let gasfree = setup.provider.then(|| {
            let mut object = json!({
                "base_url": setup.provider_url.clone().unwrap_or_else(|| provider.url()),
                "api_key": setup.api_key,
                "api_secret": API_SECRET,
                "service_provider": USERS[4].address,
            });
            if let Some(timeout) = setup.request_timeout_ms {
                object["request_timeout_ms"] = json!(timeout);
            }
            if let Some(interval) = setup.status_poll_interval_ms {
                object["status_poll_interval_ms"] = json!(interval);
            }
            let request: TronGaslessProviderRequest = serde_json::from_value(object).expect("a provider object");
            GasFreeBinding::from_request(&Network::Nile, &request).expect("a valid provider object")
        });
        let gasless: Option<TronTokenGaslessRequest> = setup.gasless.then(|| {
            let mut block = json!({ "enabled": true });
            if let Some(cap) = setup.transfer_max_fee {
                block["transfer_max_fee"] = json!(cap);
            }
            serde_json::from_value(block).expect("a gasless block")
        });

        let (platform, token) = block_on(async {
            let params = TronCoinParams {
                nodes: vec![TronNode::plain(node.url())],
                required_confirmations: 1,
                tx_history: false,
                gasfree,
            };
            let conf = crate::coin_conf(&ctx, "TRX");
            let protocol = CoinProtocol::TRX { network: Network::Nile };
            let platform = if setup.hd {
                let built = tron_coin_from_params_with_global_hd(
                    &ctx,
                    "TRX",
                    &conf,
                    params,
                    &global_hd(&ctx),
                    &PathToAddress::default(),
                    20,
                    protocol,
                )
                .await;
                let Ok(platform) = built else {
                    panic!("the HD platform coin must build")
                };
                enable_wallet(&platform, 2).await;
                platform
            } else {
                let Ok(platform) = tron_coin_from_params(&ctx, "TRX", &conf, params, &priv_key(), protocol).await
                else {
                    panic!("the platform coin must build")
                };
                platform
            };
            register(&ctx, MmCoinEnum::EthCoin(platform.clone()), "TRX").await;
            let contract = parse_trc20_contract(TRC20_CONTRACT).expect("a contract");
            let Ok(token) = platform
                .trc20_token_from_conf_or_contract(TOKEN.to_owned(), contract, 1, gasless.as_ref())
                .await
            else {
                panic!("the token must build")
            };
            if register_token {
                register(&ctx, MmCoinEnum::EthCoin(token.clone()), TOKEN).await;
            }
            (platform, token)
        });

        let senders = if setup.hd {
            vec![tron(ADDRESS_0), tron(ADDRESS_1)]
        } else {
            vec![TronAddress::from_evm_address(platform.my_address)]
        };
        chain.lock().unwrap_or_else(|e| e.into_inner()).senders = senders;

        Lab {
            ctx,
            node,
            provider,
            chain,
            provider_mode,
            relay,
            platform,
            token,
        }
    }

    fn set_chain(&self, change: impl FnOnce(&mut Chain)) {
        change(&mut self.chain.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn set_provider(&self, mode: Provider) { *self.provider_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode; }

    fn set_relay(&self, change: impl FnOnce(&mut Relay)) {
        change(&mut self.relay.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The bodies of the submit calls the provider received.
    fn submit_calls(&self) -> Vec<Json> { self.provider.bodies_for(SUBMIT_PATH) }

    /// The number of trace calls the provider received.
    fn trace_calls(&self) -> usize {
        self.provider
            .requests()
            .iter()
            .filter(|(path, _)| path != SUBMIT_PATH && path.starts_with(TRACE_PATH_PREFIX))
            .count()
    }

    /// The provider binding of the platform coin.
    fn binding(&self) -> &GasFreeBinding {
        self.platform
            .gasfree_platform_binding()
            .expect("a provider binding on the platform coin")
    }

    /// An authorization of 10 tokens from the single-key sender to [`RECEIVER`] with `deadline`, signed one
    /// second before it.
    fn signed(&self, deadline: u64) -> SignedPermit {
        let permit = PermitTransfer {
            token: tron(TRC20_CONTRACT),
            service_provider: tron(USERS[4].address),
            user: self.sender(),
            receiver: tron(RECEIVER),
            value: U256::from(10_000_000u64),
            max_fee: U256::from(TRANSFER_FEE),
            deadline,
            nonce: 7,
        };
        let secret = Secret::from_slice(&priv_key()).expect("a valid test key");
        match sign_permit(&Network::Nile, permit, &secret, deadline - 1) {
            Ok(signed) => signed,
            Err(_) => panic!("the sender signs"),
        }
    }

    /// The sending address of a request without `from`.
    fn sender(&self) -> TronAddress { TronAddress::from_evm_address(self.platform.my_address) }

    fn custody(&self) -> TronAddress { gasfree_custody_address(&Network::Nile, &self.sender()) }

    /// The direct `withdraw` method.
    fn withdraw(&self, request: WithdrawRequest) -> Result<TransactionDetails, WithdrawError> {
        block_on(crate::withdraw(self.ctx.clone(), request)).map_err(|e| e.into_inner())
    }

    fn withdrawn(&self, request: WithdrawRequest) -> TransactionDetails {
        match self.withdraw(request) {
            Ok(details) => details,
            Err(_) => panic!("the withdraw must succeed"),
        }
    }

    fn refused(&self, request: WithdrawRequest) -> WithdrawError {
        match self.withdraw(request) {
            Ok(_) => panic!("the withdraw must be refused"),
            Err(error) => error,
        }
    }

    /// `task::withdraw::init`: the id of the started task.
    fn start_task(&self, request: WithdrawRequest) -> u64 {
        let Ok(init) = block_on(init_withdraw(self.ctx.clone(), request)) else {
            panic!("the task must start")
        };
        init.task_id
    }

    /// `task::withdraw::status` of the task `task_id`.
    fn task_status(&self, task_id: u64) -> WithdrawCompatRpcStatus {
        let request = WithdrawStatusRequest {
            task_id,
            forget_if_finished: false,
        };
        let Ok(status) = block_on(withdraw_status(self.ctx.clone(), request)) else {
            panic!("the task has a status")
        };
        status
    }

    /// Polls the status of the task `task_id` until its progress value, as JSON, has the member `phase`.
    fn wait_for_phase(&self, task_id: u64, phase: &str) {
        let started = Instant::now();
        loop {
            if let WithdrawCompatRpcStatus::InProgress(progress) = self.task_status(task_id) {
                if serde_json::to_value(progress)
                    .expect("a progress value")
                    .get(phase)
                    .is_some()
                {
                    return;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the task did not reach the phase"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The error a task withdraw of `request` ends with.
    fn task_refused(&self, request: WithdrawRequest) -> WithdrawError {
        match self.task_withdraw(request).0 {
            WithdrawCompatRpcStatus::Error(error) => error,
            _ => panic!("the task must end with an error"),
        }
    }

    /// `task::withdraw::init` and its status, polled to the terminal one. Returns the terminal status and every
    /// progress value seen on the way, as its JSON.
    fn task_withdraw(&self, request: WithdrawRequest) -> (WithdrawCompatRpcStatus, Vec<Json>) {
        let task_id = self.start_task(request);
        let started = Instant::now();
        let mut progress = Vec::new();
        loop {
            let status = self.task_status(task_id);
            match status {
                WithdrawCompatRpcStatus::InProgress(phase) => {
                    let phase = serde_json::to_value(phase).expect("a progress value");
                    if progress.last() != Some(&phase) {
                        progress.push(phase);
                    }
                },
                WithdrawCompatRpcStatus::UserActionRequired(_) => panic!("no user action is expected"),
                terminal => return (terminal, progress),
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the task withdraw did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The number of requests the node received for the contract function `selector`.
    fn contract_calls(&self, selector: &str) -> usize {
        self.node
            .bodies_for("/wallet/triggerconstantcontract")
            .iter()
            .filter(|body| body["function_selector"] == selector)
            .count()
    }

    /// The number of requests the node received at `path`.
    fn node_calls(&self, path: &str) -> usize { self.node.requests().iter().filter(|(p, _)| p == path).count() }

    /// The number of requests the node received that only the native rail makes: the TRX balance, the block, the
    /// resources, the prices and the dry run of the transfer.
    fn native_rail_reads(&self) -> usize {
        [
            "/wallet/getaccount",
            "/wallet/getnowblock",
            "/wallet/getaccountresource",
            "/wallet/getchainparameters",
        ]
        .iter()
        .map(|path| self.node_calls(path))
        .sum::<usize>()
            + self.contract_calls(TRANSFER)
    }
}

fn tron(base58: &str) -> TronAddress { TronAddress::from_base58(base58).expect("a valid address") }

/// A withdraw request of 10 `coin` to [`RECEIVER`], with the members of `extra` added or replaced.
fn request(coin: &str, extra: Json) -> WithdrawRequest {
    let mut request = json!({ "coin": coin, "to": RECEIVER, "amount": "10" });
    for (key, value) in extra.as_object().expect("an object").iter() {
        request[key] = value.clone();
    }
    serde_json::from_value(request).expect("a withdraw request")
}

fn gasless(extra: Json) -> WithdrawRequest {
    let mut members = json!({ "fee_method": "gasless" });
    for (key, value) in extra.as_object().expect("an object").iter() {
        members[key] = value.clone();
    }
    request(TOKEN, members)
}

/// Whether `details` is the result of the native rail: a signed transaction with the `Tron` fee details (R24).
fn is_native(details: &TransactionDetails) -> bool {
    matches!(details.fee_details, Some(TxFeeDetails::Tron(_))) && !details.tx_hex.0.is_empty() && !details.is_unsigned()
}

/// Whether `details` is the result of the gasless rail: no transaction and the `TronGasless` fee details (R24).
fn is_gasless(details: &TransactionDetails) -> bool {
    matches!(details.fee_details, Some(TxFeeDetails::TronGasless(_))) && details.is_unsigned()
}

fn is_unavailable(error: &WithdrawError) -> bool {
    matches!(error, WithdrawError::Gasless(GaslessWithdrawError::Unavailable(_))) && error.status_code().as_u16() == 503
}

/// The `error_type` values of `error` on the wire: the outer one, and the inner one of the gasless family.
fn wire_types(error: &WithdrawError) -> (String, Option<String>) {
    let document = serde_json::to_value(error).expect("a document");
    let outer = document["error_type"].as_str().expect("an error type").to_owned();
    let inner = document["error_data"]["error_type"].as_str().map(str::to_owned);
    (outer, inner)
}

// ---------------------------------------------------------------------------------------------------------------
// A JSON text with each member name at most once per object (T16)
// ---------------------------------------------------------------------------------------------------------------

/// Reads any JSON value and fails when an object repeats a member name.
struct NoRepeatedNames;

impl<'de> Deserialize<'de> for NoRepeatedNames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = NoRepeatedNames;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("any JSON value") }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(NoRepeatedNames) }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<NoRepeatedNames>()?.is_some() {}
                Ok(NoRepeatedNames)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut names = BTreeSet::new();
                while let Some(name) = map.next_key::<String>()? {
                    if !names.insert(name) {
                        return Err(serde::de::Error::custom("a member name is repeated"));
                    }
                    map.next_value::<NoRepeatedNames>()?;
                }
                Ok(NoRepeatedNames)
            }
        }
        deserializer.deserialize_any(Check)
    }
}

#[test]
fn the_name_check_finds_a_repeated_name() {
    assert!(serde_json::from_str::<NoRepeatedNames>(r#"{"a":1,"b":{"a":[{"c":null}]}}"#).is_ok());
    assert!(serde_json::from_str::<NoRepeatedNames>(r#"{"a":1,"a":2}"#).is_err());
    assert!(serde_json::from_str::<NoRepeatedNames>(r#"{"a":[{"b":1,"b":2}]}"#).is_err());
}

// ---------------------------------------------------------------------------------------------------------------
// T15, T16, the signature: the sign-only result
// ---------------------------------------------------------------------------------------------------------------

/// The checks of T15 and T16 on the answer text `text` of a sign-only gasless withdraw of 10 tokens from `sender`.
fn assert_sign_only_result(text: &str, sender: &str) { assert_rail_result(text, sender, None) }

/// The checks of T15 and T16 on the answer text `text` of a gasless withdraw of 10 tokens from `sender`. `settled`
/// is `None` for the sign-only result; for the result of a transfer the task route followed to its settlement it
/// is the trace id and the transaction hash the result must carry (D63.9).
fn assert_rail_result(text: &str, sender: &str, settled: Option<(&str, &str)>) {
    assert!(
        serde_json::from_str::<NoRepeatedNames>(text).is_ok(),
        "a member name is repeated"
    );
    assert_no_credential(text);
    let result: Json = serde_json::from_str(text).expect("JSON");
    let custody = gasfree_custody_address(&Network::Nile, &tron(sender)).to_base58();

    // R25 to R28 under D63.8 (a): exactly these members, each once; `tx_hash` beside them once the transfer is
    // settled, and never `tx_hex`.
    let names: BTreeSet<&str> = result
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected: BTreeSet<&str> = [
        "relay_type",
        "chain_id",
        "coin",
        "from_address",
        "gasfree_address",
        "verifying_contract",
        "signed_authorization",
        "created_at",
        "from",
        "to",
        "total_amount",
        "spent_by_me",
        "received_by_me",
        "my_balance_change",
        "block_height",
        "timestamp",
        "fee_details",
        "internal_id",
        "transaction_type",
    ]
    .into_iter()
    .collect();
    if let Some((_, tx_hash)) = settled {
        expected.insert("tx_hash");
        assert_eq!(result["tx_hash"], tx_hash);
    }
    assert_eq!(names, expected);

    // R26.
    assert_eq!(result["relay_type"], "tron_gasfree");
    assert_eq!(result["chain_id"], "3448148188");
    assert_eq!(result["coin"], TOKEN);
    assert_eq!(result["from_address"], sender);
    assert_eq!(result["gasfree_address"], custody.as_str());
    assert_eq!(
        result["verifying_contract"],
        gasfree_network(&Network::Nile).controller().to_base58().as_str()
    );
    let authorization = &result["signed_authorization"];
    let members: Vec<&str> = authorization
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected_members = vec![
        "token",
        "service_provider",
        "user",
        "receiver",
        "value",
        "max_fee",
        "deadline",
        "version",
        "nonce",
        "sig",
    ];
    expected_members.sort_unstable();
    let mut sorted_members = members.clone();
    sorted_members.sort_unstable();
    assert_eq!(sorted_members, expected_members);
    assert_eq!(authorization["token"], TRC20_CONTRACT);
    assert_eq!(authorization["service_provider"], USERS[4].address);
    assert_eq!(authorization["user"], sender);
    assert_eq!(authorization["receiver"], RECEIVER);
    assert_eq!(authorization["value"], "10000000");
    assert_eq!(authorization["max_fee"], "2000000");
    assert_eq!(authorization["version"], "1");
    assert_eq!(authorization["nonce"], "7");

    // The creation time: RFC 3339 in UTC with whole seconds, before the deadline.
    let created_at = result["created_at"].as_str().expect("a string");
    assert!(created_at.len() == 20 && created_at.ends_with('Z') && created_at.as_bytes()[10] == b'T');
    let payload: GasFreeRelayPayload = serde_json::from_value(json!({
        "relay_type": result["relay_type"],
        "chain_id": result["chain_id"],
        "from_address": result["from_address"],
        "gasfree_address": result["gasfree_address"],
        "verifying_contract": result["verifying_contract"],
        "signed_authorization": result["signed_authorization"],
        "created_at": result["created_at"],
    }))
    .expect("the members read back as the payload");
    let deadline: u64 = authorization["deadline"]
        .as_str()
        .expect("a string")
        .parse()
        .expect("Unix seconds");
    assert!(payload.created_at < deadline);
    assert_eq!(deadline - payload.created_at, 300, "the default window");
    assert_eq!(result["timestamp"], json!(payload.created_at));

    // R28.
    assert_eq!(result["from"], json!([sender]));
    assert_eq!(result["to"], json!([RECEIVER]));
    assert_eq!(dec_of(&result["total_amount"]), dec("10"));
    assert_eq!(dec_of(&result["spent_by_me"]), dec("12"));
    assert_eq!(dec_of(&result["received_by_me"]), dec("0"));
    assert_eq!(dec_of(&result["my_balance_change"]), dec("-12"));
    assert_eq!(result["block_height"], 0);
    assert_eq!(result["internal_id"], "");
    assert_eq!(result["transaction_type"], "StandardTransfer");

    // §63.3.5.
    let fee = &result["fee_details"];
    assert_eq!(fee["type"], "TronGasless");
    assert_eq!(fee["coin"], TOKEN);
    assert_eq!(fee["fee_method"], "gasless");
    assert_eq!(fee["provider_name"], "gasfree");
    assert_eq!(fee["gasfree_address"], custody.as_str());
    assert_eq!(dec_of(&fee["transfer_fee"]), dec("2"));
    assert_eq!(dec_of(&fee["activation_fee"]), dec("0"));
    assert_eq!(dec_of(&fee["total_token_fee"]), dec("2"));
    assert_eq!(dec_of(&fee["signed_max_fee"]), dec("2"));
    assert!(fee.as_object().expect("an object").contains_key("trace_id"));
    match settled {
        Some((trace_id, _)) => assert_eq!(fee["trace_id"], trace_id),
        None => assert!(fee["trace_id"].is_null()),
    }

    // The signature: 130 lower-case hex digits, the last byte 27 or 28, recovering to the sender for the
    // typed-data hash.
    let sig = authorization["sig"].as_str().expect("a string");
    assert_eq!(sig.len(), 130);
    assert!(
        sig.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "the signature is not lower-case hex"
    );
    assert!(sig.ends_with("1b") || sig.ends_with("1c"), "the recovery byte");
    let signed: SignedPermit = serde_json::from_value(authorization.clone()).expect("a signed authorization");
    let digest = typed_data_hash(&Network::Nile, signed.permit()).expect("a typed-data hash");
    let signature = Signature::from_str(sig).expect("65 bytes");
    let recovers = verify_address(&tron(sender).to_evm_address(), &signature, &H256::from(digest));
    assert!(
        matches!(recovers, Ok(true)),
        "the signature does not recover to the sender"
    );
}

/// ch.63 T15, T16 without `from`: the direct method answers the sign-only result and asks the provider for the
/// account record only; the task reports the quote phase and answers the same members with the trace id and the
/// transaction hash of the settled transfer (D63.9).
#[test]
fn t15_t16_the_sign_only_result_of_a_single_key_withdraw() {
    let lab = Lab::new(Setup::usual());
    let sender = lab.sender().to_base58();

    let direct = lab.withdrawn(gasless(json!({})));
    assert!(is_gasless(&direct));
    assert!(direct.tx_hex.0.is_empty() && direct.tx_hash.is_empty());
    assert_no_credential(&format!("{direct:?}"));
    let rendering = format!("{direct:?}");
    let text = serde_json::to_string(&direct).expect("the answer text");
    let sig = serde_json::from_str::<Json>(&text).expect("JSON")["signed_authorization"]["sig"]
        .as_str()
        .expect("a string")
        .to_owned();
    assert!(!rendering.contains(&sig), "Debug renders the signature");
    assert_sign_only_result(&text, &sender);
    assert!(
        lab.submit_calls().is_empty() && lab.trace_calls() == 0,
        "the direct method submitted or traced"
    );
    assert_eq!(lab.provider.requests().len(), 1);

    // The task: a slow provider keeps the quote phase visible.
    lab.set_provider(Provider::Slow(400));
    let (status, progress) = lab.task_withdraw(gasless(json!({})));
    assert!(matches!(status, WithdrawCompatRpcStatus::Ok(_)), "the task succeeds");
    assert!(
        progress.contains(&json!("FetchingGaslessQuote")),
        "the quote phase is not reported"
    );
    let status = serde_json::to_value(&status).expect("a status");
    assert_eq!(status["status"], "Ok");
    let text = serde_json::to_string(&status["details"]).expect("the details text");
    assert_rail_result(&text, &sender, Some((TRACE_ID, TXN_HASH)));
    // The whole status text repeats no name either.
    let status_text = serde_json::to_string(&status).expect("the status text");
    assert!(serde_json::from_str::<NoRepeatedNames>(&status_text).is_ok());
}

/// ch.63 T15, T16 with an HD `from` selector: the selected address sends and signs, `from` is the address array
/// and no selector is in the answer. The task submits the authorization of the selected address.
#[test]
fn t15_t16_the_sign_only_result_of_an_hd_withdraw_with_from() {
    let lab = Lab::new(Setup {
        hd: true,
        ..Setup::usual()
    });
    for from in [
        json!({ "account_id": 0, "chain": "External", "address_id": 1 }),
        json!({ "derivation_path": "m/44'/195'/0'/0/1" }),
    ] {
        let direct = lab.withdrawn(gasless(json!({ "from": from.clone() })));
        let text = serde_json::to_string(&direct).expect("the answer text");
        assert_sign_only_result(&text, ADDRESS_1);
        assert!(!text.contains("account_id") && !text.contains("derivation_path"));

        let (status, _) = lab.task_withdraw(gasless(json!({ "from": from })));
        let status = serde_json::to_value(&status).expect("a status");
        assert_eq!(status["status"], "Ok");
        let text = serde_json::to_string(&status["details"]).expect("the details text");
        assert_rail_result(&text, ADDRESS_1, Some((TRACE_ID, TXN_HASH)));
        let submitted = lab.submit_calls();
        assert_eq!(submitted.last().expect("a submit call")["user"], ADDRESS_1);
    }
    // Without `from` the activated address sends.
    let direct = lab.withdrawn(gasless(json!({})));
    assert_sign_only_result(&serde_json::to_string(&direct).expect("the answer text"), ADDRESS_0);
}

/// ch.63 T19, R33: the three phases of the submission flow in their wire forms. `tx_hash` is always present.
#[test]
fn t19_the_submission_phases_have_their_wire_forms() {
    let wire = |phase: WithdrawInProgressStatus| serde_json::to_string(&phase).expect("JSON");
    assert_eq!(
        wire(WithdrawInProgressStatus::SubmittingGaslessAuthorization),
        r#""SubmittingGaslessAuthorization""#
    );
    assert_eq!(
        wire(WithdrawInProgressStatus::WaitingForGaslessProvider {
            trace_id: TRACE_ID.to_owned(),
            provider_state: "INPROGRESS".to_owned(),
        }),
        format!(r#"{{"WaitingForGaslessProvider":{{"trace_id":"{TRACE_ID}","provider_state":"INPROGRESS"}}}}"#)
    );
    assert_eq!(
        wire(WithdrawInProgressStatus::WaitingForGaslessSettlement {
            trace_id: TRACE_ID.to_owned(),
            tx_hash: Some(TXN_HASH.to_owned()),
        }),
        format!(r#"{{"WaitingForGaslessSettlement":{{"trace_id":"{TRACE_ID}","tx_hash":"{TXN_HASH}"}}}}"#)
    );
    assert_eq!(
        wire(WithdrawInProgressStatus::WaitingForGaslessSettlement {
            trace_id: TRACE_ID.to_owned(),
            tx_hash: None,
        }),
        format!(r#"{{"WaitingForGaslessSettlement":{{"trace_id":"{TRACE_ID}","tx_hash":null}}}}"#)
    );
    // In the task status the phase is the `details` of an `InProgress` status.
    let status = serde_json::to_value(WithdrawCompatRpcStatus::InProgress(
        WithdrawInProgressStatus::WaitingForGaslessSettlement {
            trace_id: TRACE_ID.to_owned(),
            tx_hash: None,
        },
    ))
    .expect("a status");
    assert_eq!(
        status,
        json!({
            "status": "InProgress",
            "details": { "WaitingForGaslessSettlement": { "trace_id": TRACE_ID, "tx_hash": null } }
        })
    );
}

/// §63.3.6, D63.2 (a): the two phases the sign-only path emits have the reference's wire values.
#[test]
fn the_gasless_phases_have_their_wire_values() {
    assert_eq!(
        serde_json::to_value(WithdrawInProgressStatus::FetchingGaslessQuote).expect("JSON"),
        "FetchingGaslessQuote"
    );
    assert_eq!(
        serde_json::to_value(WithdrawInProgressStatus::SigningGaslessAuthorization).expect("JSON"),
        "SigningGaslessAuthorization"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// T8: rail selection, caps, fallback
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T8: `native` and an absent `fee_method` run the native rail and ask the provider nothing; `gasless` runs
/// the gasless rail, which estimates no TRX fee and does not refuse a sender without TRX.
#[test]
fn t8_native_and_gasless_select_their_rail() {
    let lab = Lab::new(Setup::usual());
    for extra in [json!({}), json!({ "fee_method": "native" })] {
        let details = lab.withdrawn(request(TOKEN, extra));
        assert!(is_native(&details));
    }
    assert!(lab.provider.requests().is_empty(), "the native rail asks the provider");
    assert_eq!(lab.contract_calls(GET_GASFREE_ADDRESS), 0);

    // No TRX at all: the gasless rail does not look.
    lab.set_chain(|chain| chain.trx_sun = 0);
    let before = lab.native_rail_reads();
    let details = lab.withdrawn(gasless(json!({})));
    assert!(is_gasless(&details));
    assert_eq!(
        lab.native_rail_reads(),
        before,
        "the gasless rail read a native-rail input"
    );
    assert_eq!(lab.provider.requests().len(), 1);
    // Its only balance read is the custody address.
    let balance_reads = lab.node.bodies_for("/wallet/triggerconstantcontract");
    let last_balance_read = balance_reads
        .iter()
        .rev()
        .find(|body| body["function_selector"] == BALANCE_OF)
        .expect("a balance read");
    assert_eq!(last_balance_read["owner_address"], lab.custody().to_base58().as_str());
}

/// ch.63 T8, §63.9.4, R14: the request-shape rejections. Under `gasless` each comes before any node or provider
/// request; under `auto` none of them asks the provider.
#[test]
fn t8_the_request_shape_rejections_ask_nobody() {
    let lab = Lab::new(Setup::usual());
    let refused_as_fee_validation = |members: Json| {
        let error = lab.refused(request(TOKEN, members));
        assert!(matches!(error, WithdrawError::InvalidFeePolicy(_)), "a shape rejection");
        assert_eq!(error.status_code().as_u16(), 400);
    };

    // Gasless options with the native rail.
    let node_before = lab.node.requests().len();
    for members in [
        json!({ "gasless": {} }),
        json!({ "gasless": { "max_fee": "1" } }),
        json!({ "fee_method": "native", "gasless": { "fallback_to_native": true } }),
    ] {
        refused_as_fee_validation(members);
    }
    // `gasless`: a zero deadline, an unusable `max_fee`, a `max_fee` the token cannot express, an amount of zero.
    for members in [
        json!({ "gasless": { "deadline_seconds": 0 } }),
        json!({ "gasless": { "max_fee": "plenty" } }),
        json!({ "gasless": { "max_fee": { "amount": 1 } } }),
        json!({ "gasless": { "max_fee": "-1" } }),
        json!({ "gasless": { "max_fee": "0.0000001" } }),
        json!({ "amount": "0" }),
        json!({ "amount": "0.0000001" }),
    ] {
        let mut members = members;
        members["fee_method"] = json!("gasless");
        refused_as_fee_validation(members);
    }
    // `max` on the gasless rail.
    let error = lab.refused(gasless(json!({ "max": true })));
    assert!(matches!(error, WithdrawError::UnsupportedError(_)), "max on the rail");
    assert_eq!(error.status_code().as_u16(), 400);
    assert_eq!(lab.node.requests().len(), node_before, "a node was asked");

    // `auto`: the terms are checked before the native rail runs; a cap the token cannot express is found when the
    // rail is reached, after the native rail could not be funded.
    for options in [
        json!({ "deadline_seconds": 0 }),
        json!({ "max_fee": "plenty" }),
        json!({ "max_fee": "-1" }),
    ] {
        refused_as_fee_validation(json!({ "fee_method": "auto", "gasless": options }));
    }
    assert_eq!(lab.node.requests().len(), node_before, "a node was asked");
    lab.set_chain(|chain| chain.token_units = 0);
    refused_as_fee_validation(json!({ "fee_method": "auto", "gasless": { "max_fee": "0.0000001" } }));
    assert!(lab.provider.requests().is_empty(), "the provider was asked");
}

/// ch.63 T8, R8: the effective cap is the smaller of the request's and the activation's; the quote above it is
/// refused, and the signed `maxFee` is the cap.
#[test]
fn t8_the_effective_cap_bounds_the_fee_and_is_signed() {
    // No cap: the quoted total is signed.
    let lab = Lab::new(Setup::usual());
    let details = lab.withdrawn(gasless(json!({})));
    let result = serde_json::to_value(&details).expect("JSON");
    assert_eq!(result["signed_authorization"]["max_fee"], "2000000");

    // A request cap above the quote is signed as it is; one below the quote refuses.
    let details = lab.withdrawn(gasless(json!({ "gasless": { "max_fee": "5" } })));
    let result = serde_json::to_value(&details).expect("JSON");
    assert_eq!(result["signed_authorization"]["max_fee"], "5000000");
    assert_eq!(dec_of(&result["fee_details"]["signed_max_fee"]), dec("5"));
    assert_eq!(dec_of(&result["fee_details"]["total_token_fee"]), dec("2"));
    let error = lab.refused(gasless(json!({ "gasless": { "max_fee": 1.5 } })));
    assert!(matches!(
        error,
        WithdrawError::Gasless(GaslessWithdrawError::MaxFeeExceeded(_))
    ));
    assert_eq!(error.status_code().as_u16(), 400);

    // An activation cap of 3: it bounds a larger request cap, and a smaller request cap bounds it.
    let capped = Lab::new(Setup {
        transfer_max_fee: Some("3"),
        ..Setup::usual()
    });
    for (request_cap, signed) in [(None, "3000000"), (Some("5"), "3000000"), (Some("2.5"), "2500000")] {
        let extra = match request_cap {
            Some(cap) => json!({ "gasless": { "max_fee": cap } }),
            None => json!({}),
        };
        let details = capped.withdrawn(gasless(extra));
        let result = serde_json::to_value(&details).expect("JSON");
        assert_eq!(result["signed_authorization"]["max_fee"], signed);
    }
    // An activation cap below the quote refuses whatever the request allows.
    let tight = Lab::new(Setup {
        transfer_max_fee: Some("1.5"),
        ..Setup::usual()
    });
    for extra in [json!({}), json!({ "gasless": { "max_fee": "100" } })] {
        let error = tight.refused(gasless(extra));
        assert!(matches!(
            error,
            WithdrawError::Gasless(GaslessWithdrawError::MaxFeeExceeded(_))
        ));
    }
    // A not-yet-activated custody account pays the activation fee too: the quote is 3.
    lab.set_provider(Provider::Account(Account {
        active: false,
        ..Account::usual()
    }));
    let details = lab.withdrawn(gasless(json!({})));
    let result = serde_json::to_value(&details).expect("JSON");
    assert_eq!(dec_of(&result["fee_details"]["activation_fee"]), dec("1"));
    assert_eq!(dec_of(&result["fee_details"]["total_token_fee"]), dec("3"));
    assert_eq!(dec_of(&result["spent_by_me"]), dec("13"));
}

/// ch.63 T8, R16: `auto` runs the native rail first and turns to the gasless rail only after a funding failure.
#[test]
fn t8_auto_is_native_first() {
    let lab = Lab::new(Setup::usual());
    let auto = |extra: Json| {
        let mut members = json!({ "fee_method": "auto" });
        for (key, value) in extra.as_object().expect("an object").iter() {
            members[key] = value.clone();
        }
        request(TOKEN, members)
    };

    // Funded: the native rail, and the provider is not asked.
    assert!(is_native(&lab.withdrawn(auto(json!({})))));
    assert!(
        lab.provider.requests().is_empty(),
        "a funded auto withdraw asks the provider"
    );

    // Too little TRX for the fee, then too little of the token: the gasless rail.
    lab.set_chain(|chain| chain.trx_sun = 0);
    assert!(is_gasless(&lab.withdrawn(auto(json!({})))));
    lab.set_chain(|chain| {
        chain.trx_sun = 50_000_000;
        chain.token_units = 0;
    });
    assert!(is_gasless(&lab.withdrawn(auto(json!({})))));

    // R16, the rail was attempted and failed: the gasless error.
    lab.set_chain(|chain| chain.custody_units = 0);
    assert!(matches!(
        lab.refused(auto(json!({}))),
        WithdrawError::NotSufficientCustodyBalance { .. }
    ));
    lab.set_chain(|chain| chain.custody_units = 50_000_000);
    lab.set_provider(Provider::Raw(500, r#"{"code":500,"message":"down"}"#.to_owned()));
    assert!(matches!(lab.refused(auto(json!({}))), WithdrawError::Transport(_)));
    lab.set_provider(Provider::Account(Account {
        allow_submit: false,
        ..Account::usual()
    }));
    assert!(matches!(
        lab.refused(auto(json!({}))),
        WithdrawError::Gasless(GaslessWithdrawError::PendingTransfer(_))
    ));
    lab.set_provider(Provider::Account(Account::usual()));

    // R14: a max-amount request never goes to the rail.
    let provider_before = lab.provider.requests().len();
    lab.set_chain(|chain| {
        chain.token_units = 50_000_000;
        chain.trx_sun = 0;
    });
    assert!(matches!(
        lab.refused(auto(json!({ "max": true }))),
        WithdrawError::NotSufficientPlatformBalanceForFee { .. }
    ));
    // A failure of the native rail that is no funding failure is returned as it is.
    lab.set_chain(|chain| chain.failing = Some("/wallet/getchainparameters"));
    assert!(matches!(lab.refused(auto(json!({}))), WithdrawError::Transport(_)));
    assert_eq!(lab.provider.requests().len(), provider_before, "the provider was asked");
}

/// ch.63 R16 and CRD ch.21 R21.10B.13: the refusal of a transfer whose dry run reverts belongs to the native rail.
/// It is no funding failure, so `auto` answers it and does not turn to the gasless rail; the gasless rail makes
/// no dry run and signs as before.
#[test]
fn a_reverting_dry_run_stays_on_the_native_rail() {
    let lab = Lab::new(Setup::usual());
    lab.set_chain(|chain| chain.transfer_reverts = true);

    for extra in [json!({}), json!({ "fee_method": "auto" })] {
        assert!(matches!(
            lab.refused(request(TOKEN, extra)),
            WithdrawError::UnsupportedError(_)
        ));
    }
    assert_eq!(lab.contract_calls(TRANSFER), 2);
    assert!(lab.provider.requests().is_empty(), "the provider was asked");
    assert_eq!(lab.contract_calls(GET_GASFREE_ADDRESS), 0);

    assert!(is_gasless(&lab.withdrawn(gasless(json!({})))));
    assert_eq!(lab.contract_calls(TRANSFER), 2, "the gasless rail made a dry run");
}

/// ch.63 T8, R16: where the rail does not apply to the coin, `auto` answers the native rail's error.
#[test]
fn t8_auto_answers_the_native_error_where_the_rail_does_not_apply() {
    // A token activated without the gasless rail, and one whose platform has no provider.
    for setup in [
        Setup {
            gasless: false,
            ..Setup::usual()
        },
        Setup {
            provider: false,
            gasless: false,
            ..Setup::usual()
        },
    ] {
        let lab = Lab::new(setup);
        lab.set_chain(|chain| chain.trx_sun = 0);
        let error = lab.refused(request(TOKEN, json!({ "fee_method": "auto" })));
        assert!(matches!(
            error,
            WithdrawError::NotSufficientPlatformBalanceForFee { .. }
        ));
        assert!(lab.provider.requests().is_empty(), "the provider was asked");
    }
    // Native TRX.
    let lab = Lab::new(Setup::usual());
    lab.set_chain(|chain| chain.trx_sun = 1_000_000);
    let error = lab.refused(request("TRX", json!({ "fee_method": "auto" })));
    assert!(matches!(error, WithdrawError::NotSufficientBalance { .. }));
    assert!(lab.provider.requests().is_empty(), "the provider was asked");
}

/// ch.63 T8, §63.8 step 2 and step 6, R24: where the rail does not exist, `gasless` is refused as unavailable, and
/// with `fallback_to_native` the native rail runs and shows in the result.
#[test]
fn t8_gasless_where_the_rail_is_not_configured() {
    let fallback = json!({ "gasless": { "fallback_to_native": true } });
    for setup in [
        Setup {
            gasless: false,
            ..Setup::usual()
        },
        Setup {
            provider: false,
            gasless: false,
            ..Setup::usual()
        },
    ] {
        let lab = Lab::new(setup);
        assert!(is_unavailable(&lab.refused(gasless(json!({})))));
        assert!(is_native(&lab.withdrawn(gasless(fallback.clone()))));
        assert!(lab.provider.requests().is_empty(), "the provider was asked");
    }
    // Native TRX.
    let lab = Lab::new(Setup::usual());
    let error = lab.refused(request("TRX", json!({ "fee_method": "gasless" })));
    assert!(is_unavailable(&error));
    let mut members = fallback.clone();
    members["fee_method"] = json!("gasless");
    let details = lab.withdrawn(request("TRX", members));
    assert!(is_native(&details));
}

/// ch.63 T8, §63.8 step 6: only a deterministic unavailability falls back; every other failure surfaces, also
/// with `fallback_to_native`.
#[test]
fn t8_the_fallback_takes_deterministic_unavailability_only() {
    let lab = Lab::new(Setup::usual());
    let fallback = || gasless(json!({ "gasless": { "fallback_to_native": true } }));

    // Deterministic: the token is not enrolled; the custody account is short.
    lab.set_provider(Provider::Account(Account {
        enrolled: false,
        ..Account::usual()
    }));
    assert!(is_unavailable(&lab.refused(gasless(json!({})))));
    assert!(is_native(&lab.withdrawn(fallback())));
    lab.set_provider(Provider::Account(Account::usual()));
    lab.set_chain(|chain| chain.custody_units = 11_999_999);
    assert!(matches!(
        lab.refused(gasless(json!({}))),
        WithdrawError::NotSufficientCustodyBalance { .. }
    ));
    assert!(is_native(&lab.withdrawn(fallback())));
    lab.set_chain(|chain| chain.custody_units = 50_000_000);

    // Not deterministic: each surfaces with the fallback allowed.
    let wrong_custody = gasfree_custody_address(&Network::Mainnet, &lab.sender());
    let cases = [
        (
            Provider::Account(Account {
                custody: Some(wrong_custody),
                ..Account::usual()
            }),
            "InvalidProviderResponse",
        ),
        (
            Provider::Account(Account {
                decimal: 18,
                ..Account::usual()
            }),
            "InvalidProviderResponse",
        ),
        (
            Provider::Account(Account {
                allow_submit: false,
                ..Account::usual()
            }),
            "PendingTransfer",
        ),
        (
            Provider::Raw(400, r#"{"code":400,"message":"no"}"#.to_owned()),
            "ProviderRejected",
        ),
    ];
    for (mode, member) in cases {
        lab.set_provider(mode);
        let (outer, inner) = wire_types(&lab.refused(fallback()));
        assert_eq!(outer, "Gasless");
        assert_eq!(inner.as_deref(), Some(member));
    }
    lab.set_provider(Provider::Raw(503, "down".to_owned()));
    assert!(matches!(lab.refused(fallback()), WithdrawError::Transport(_)));
}

// ---------------------------------------------------------------------------------------------------------------
// T11: the errors at the RPC
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T11, §63.9.3, R17: each provider-interaction condition reaches the RPC as its arm and status, and as no
/// other discriminant; no text names a credential.
#[test]
fn t11_each_provider_failure_collapses_onto_its_arm() {
    let log_captured = install_log_capture();
    let lab = Lab::new(Setup::usual());
    let invalid = ("Gasless", Some("InvalidProviderResponse"), 502);
    let rejected = ("Gasless", Some("ProviderRejected"), 502);
    let transport = ("Transport", None, 502);
    let cases: Vec<(u16, &str, (&str, Option<&str>, u16))> = vec![
        // The answer cannot be used.
        (200, "not json", invalid),
        (200, r#"{"code":200,"message":"ok"}"#, invalid),
        (200, r#"{"code":200,"data":{"accountAddress":"x"}}"#, invalid),
        (200, r#"{"code":7,"message":"odd"}"#, invalid),
        // A 4xx other than 401, 403 and 429, as a status and as an envelope code.
        (400, r#"{"code":400,"message":"bad request"}"#, rejected),
        (404, "not found", rejected),
        (200, r#"{"code":422,"message":"unprocessable"}"#, rejected),
        // 401, 403, 429 and 5xx, as a status and as an envelope code.
        (401, r#"{"code":401,"message":"bad key"}"#, transport),
        (
            200,
            r#"{"code":401,"reason":"Unauthorized","message":"bad key"}"#,
            transport,
        ),
        (403, "forbidden", transport),
        (200, r#"{"code":403,"message":"forbidden"}"#, transport),
        (429, "slow down", transport),
        (200, r#"{"code":429,"message":"slow down"}"#, transport),
        (500, "oops", transport),
        (503, r#"{"code":503,"message":"maintenance"}"#, transport),
        (200, r#"{"code":500,"message":"oops"}"#, transport),
    ];
    let mut transport_texts = BTreeSet::new();
    for (status, body, (outer, inner, http)) in cases {
        lab.set_provider(Provider::Raw(status, body.to_owned()));
        let error = lab.refused(gasless(json!({})));
        let (wire_outer, wire_inner) = wire_types(&error);
        assert_eq!(wire_outer, outer);
        assert_eq!(wire_inner.as_deref(), inner);
        assert_eq!(error.status_code().as_u16(), http);
        assert_no_credential(&format!("{error} {error:?}"));
        if outer == "Transport" {
            transport_texts.insert(error.to_string());
        }
    }
    // A caller cannot tell the transport kinds apart: one text for all of them.
    assert_eq!(transport_texts.len(), 1, "the transport kinds differ in their text");

    // The per-call timeout, and a provider that is not there.
    let slow = Lab::new(Setup {
        request_timeout_ms: Some(300),
        ..Setup::usual()
    });
    slow.set_provider(Provider::Slow(3_000));
    let error = slow.refused(gasless(json!({})));
    assert!(matches!(error, WithdrawError::Transport(_)), "the timeout");
    assert_eq!(error.status_code().as_u16(), 502);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a loopback port")
        .local_addr()
        .expect("a local address")
        .port();
    let unreachable = Lab::new(Setup {
        provider_url: Some(format!("http://127.0.0.1:{port}")),
        ..Setup::usual()
    });
    let error = unreachable.refused(gasless(json!({})));
    assert!(matches!(error, WithdrawError::Transport(_)), "the failed connection");
    assert_no_credential(&format!("{error} {error:?}"));

    // A request the client could not form: a credential that cannot be a header value.
    let unformable = Lab::new(Setup {
        api_key: "key\nwith a line break",
        ..Setup::usual()
    });
    let error = unformable.refused(gasless(json!({})));
    assert!(
        matches!(error, WithdrawError::InternalError(_)),
        "the unformable request"
    );
    assert_eq!(error.status_code().as_u16(), 500);
    assert!(!format!("{error} {error:?}").contains("with a line break"));
    assert!(unformable.provider.requests().is_empty());

    if log_captured {
        let lines = captured_log_lines();
        assert!(
            lines.iter().any(|line| line.contains("GasFree provider failure")),
            "the finer kind is not logged"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(API_KEY) && !line.contains(API_SECRET)),
            "a log line carries a credential"
        );
    }
}

/// ch.63 T11, §63.9.2, R19, D63.3 (c): each preflight outcome reaches the RPC as its error; the shortfall carries
/// the custody figures in the shared shape and names the custody address in its text.
#[test]
fn t11_each_preflight_outcome_maps_to_its_error() {
    let lab = Lab::new(Setup::usual());
    let custody = lab.custody().to_base58();

    lab.set_provider(Provider::Account(Account {
        enrolled: false,
        ..Account::usual()
    }));
    assert!(is_unavailable(&lab.refused(gasless(json!({})))));

    for record in [
        Account {
            custody: Some(gasfree_custody_address(&Network::Mainnet, &lab.sender())),
            ..Account::usual()
        },
        Account {
            decimal: 18,
            ..Account::usual()
        },
    ] {
        lab.set_provider(Provider::Account(record));
        let error = lab.refused(gasless(json!({})));
        assert!(matches!(
            error,
            WithdrawError::Gasless(GaslessWithdrawError::InvalidProviderResponse(_))
        ));
        assert_eq!(error.status_code().as_u16(), 502);
    }

    // A transfer in flight: 409, and the custody balance is not read for it.
    let balance_reads = lab.contract_calls(BALANCE_OF);
    lab.set_provider(Provider::Account(Account {
        allow_submit: false,
        ..Account::usual()
    }));
    let error = lab.refused(gasless(json!({})));
    assert!(matches!(
        error,
        WithdrawError::Gasless(GaslessWithdrawError::PendingTransfer(_))
    ));
    assert_eq!(error.status_code().as_u16(), 409);
    assert_eq!(
        lab.contract_calls(BALANCE_OF),
        balance_reads,
        "the wait decision read the chain"
    );

    // The shortfall of an activated account: the spendable balance against the amount plus the fee.
    let shortfall = |error: WithdrawError, available: &str, required: &str| {
        assert_eq!(error.status_code().as_u16(), 400);
        let text = error.to_string();
        assert!(text.contains(&custody), "the text does not name the custody address");
        let document = serde_json::to_value(&error).expect("a document");
        assert_eq!(document["error_type"], "NotSufficientBalance");
        let data = document["error_data"].as_object().expect("an object");
        let names: BTreeSet<&str> = data.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["available", "coin", "required"]
                .into_iter()
                .collect::<BTreeSet<&str>>()
        );
        assert_eq!(data["coin"], TOKEN);
        assert_eq!(dec_of(&data["available"]), dec(available));
        assert_eq!(dec_of(&data["required"]), dec(required));
    };
    lab.set_provider(Provider::Account(Account {
        frozen: 3_000_000,
        ..Account::usual()
    }));
    lab.set_chain(|chain| chain.custody_units = 14_999_999);
    shortfall(lab.refused(gasless(json!({}))), "11.999999", "12");
    lab.set_chain(|chain| chain.custody_units = 15_000_000);
    assert!(is_gasless(&lab.withdrawn(gasless(json!({})))));

    // The shortfall of a not-yet-activated account: the whole balance against the requirement with the locked
    // amount and the activation fee in it.
    lab.set_provider(Provider::Account(Account {
        active: false,
        frozen: 3_000_000,
        ..Account::usual()
    }));
    lab.set_chain(|chain| chain.custody_units = 15_999_999);
    shortfall(lab.refused(gasless(json!({}))), "15.999999", "16");
    lab.set_chain(|chain| chain.custody_units = 16_000_000);
    assert!(is_gasless(&lab.withdrawn(gasless(json!({})))));

    // A failed read of the custody balance is the node error.
    lab.set_chain(|chain| chain.failing = Some("/wallet/triggerconstantcontract"));
    let error = lab.refused(gasless(json!({})));
    assert!(matches!(error, WithdrawError::Transport(_)), "the failed balance read");
}

// ---------------------------------------------------------------------------------------------------------------
// T12: key-less policies
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T12, R22: under the hardware-wallet policy every fee method is refused as unsupported on the direct
/// method, on the task route and at the TRON entry itself, with no node request and no provider request. The
/// entry refuses any sender without a local key, which is how a WalletConnect or browser-extension policy would
/// arrive; this build has no such signer for the EVM family to activate a TRON coin with.
#[cfg(not(target_os = "ios"))]
#[test]
fn t12_a_key_less_policy_is_refused_before_any_read() {
    use crate::eth::{EthSigner, EthTrezorSigner};
    use crate::utxo::utxo_tests::hd_accounts_at_index_tests::{device_context, DEVICE_PUBKEY};
    use crate::MmCoin;
    use futures::compat::Future01CompatExt;
    use mm2_eth::keys::KeyPair;

    let lab = Lab::build(Setup::usual(), false);
    let Lab {
        ctx,
        node,
        provider,
        token,
        platform: _platform,
        ..
    } = lab;
    let key_pair = KeyPair::from_secret_slice(&priv_key()).expect("a key pair");
    let Ok(mut inner) = Arc::try_unwrap(token.0) else {
        panic!("a sole owner of the token coin")
    };
    inner.signer = EthSigner::Trezor(EthTrezorSigner {
        derivation_path: crypto::DerivationPath::from_str("m/44'/195'/0'/0/0").expect("a path"),
        address: key_pair.address(),
        public: *key_pair.public(),
        hw_ctx: device_context(DEVICE_PUBKEY),
    });
    let token = EthCoin(Arc::new(inner));
    block_on(register(&ctx, MmCoinEnum::EthCoin(token.clone()), TOKEN));
    let node_before = node.requests().len();

    for fee_method in ["gasless", "auto", "native"] {
        let members = json!({ "fee_method": fee_method });
        // The direct method, through the shared dispatch and through the coin.
        let direct = block_on(crate::withdraw(ctx.clone(), request(TOKEN, members.clone())));
        assert!(
            matches!(
                direct.map_err(|e| e.into_inner()),
                Err(WithdrawError::UnsupportedError(_))
            ),
            "the direct method"
        );
        let of_coin = block_on(token.withdraw(request(TOKEN, members.clone())).compat());
        assert!(
            matches!(
                of_coin.map_err(|e| e.into_inner()),
                Err(WithdrawError::UnsupportedError(_))
            ),
            "the coin's withdraw"
        );
        // The TRON entry itself.
        let entry = block_on(withdraw_tron(
            token.clone(),
            request(TOKEN, members.clone()),
            EvmWithdrawSender::activated(&token),
        ));
        assert!(
            matches!(
                entry.map_err(|e| e.into_inner()),
                Err(WithdrawError::UnsupportedError(_))
            ),
            "the TRON entry"
        );
        // The task route.
        let Ok(init) = block_on(init_withdraw(ctx.clone(), request(TOKEN, members))) else {
            panic!("the task must start")
        };
        let started = Instant::now();
        loop {
            let status_request = WithdrawStatusRequest {
                task_id: init.task_id,
                forget_if_finished: false,
            };
            let Ok(status) = block_on(withdraw_status(ctx.clone(), status_request)) else {
                panic!("the task has a status")
            };
            match status {
                WithdrawCompatRpcStatus::Error(error) => {
                    assert!(matches!(error, WithdrawError::UnsupportedError(_)), "the task route");
                    assert_eq!(error.status_code().as_u16(), 400);
                    break;
                },
                WithdrawCompatRpcStatus::Ok(_) => panic!("the task must fail"),
                _ => (),
            }
            assert!(started.elapsed() < Duration::from_secs(30), "the task did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(node.requests().len(), node_before, "a node was asked");
    assert!(provider.requests().is_empty(), "the provider was asked");
}

// ---------------------------------------------------------------------------------------------------------------
// T13: coins of other families
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T13, R21: on an EVM coin and on a UTXO coin `gasless` and `auto` answer 503 and gasless options with the
/// native rail answer 400, on the direct method and at the start of the task, before anything else is done with
/// the request: the destination of every request below is not an address of the coin.
#[test]
fn t13_other_coin_families_refuse_the_gasless_rail_first() {
    let (ctx, eth) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);
    let utxo = crate::utxo::utxo_tests::software_coin_for_test("RICK");
    block_on(async {
        register(&ctx, MmCoinEnum::EthCoin(eth.clone()), "ETH").await;
        register(&ctx, MmCoinEnum::UtxoCoin(utxo), "RICK").await;
    });
    let request_of = |coin: &str, extra: Json| -> WithdrawRequest {
        let mut request = json!({ "coin": coin, "to": "not an address", "amount": "1" });
        for (key, value) in extra.as_object().expect("an object").iter() {
            request[key] = value.clone();
        }
        serde_json::from_value(request).expect("a withdraw request")
    };
    for coin in ["ETH", "RICK"] {
        for fee_method in ["gasless", "auto"] {
            for extra in [
                json!({ "fee_method": fee_method }),
                json!({ "fee_method": fee_method, "gasless": { "max_fee": "plenty", "deadline_seconds": 0 } }),
                json!({ "fee_method": fee_method, "max": true }),
            ] {
                let direct = match block_on(crate::withdraw(ctx.clone(), request_of(coin, extra.clone()))) {
                    Ok(_) => panic!("the withdraw must be refused"),
                    Err(error) => error.into_inner(),
                };
                let task = match block_on(init_withdraw(ctx.clone(), request_of(coin, extra))) {
                    Ok(_) => panic!("the task must not start"),
                    Err(error) => error.into_inner(),
                };
                for error in [direct, task] {
                    assert!(is_unavailable(&error), "the unavailable refusal");
                    assert_eq!(
                        wire_types(&error),
                        ("Gasless".to_owned(), Some("Unavailable".to_owned()))
                    );
                }
            }
        }
        for extra in [
            json!({ "gasless": {} }),
            json!({ "fee_method": "native", "gasless": { "max_fee": "1" } }),
        ] {
            let direct = match block_on(crate::withdraw(ctx.clone(), request_of(coin, extra.clone()))) {
                Ok(_) => panic!("the withdraw must be refused"),
                Err(error) => error.into_inner(),
            };
            let task = match block_on(init_withdraw(ctx.clone(), request_of(coin, extra))) {
                Ok(_) => panic!("the task must not start"),
                Err(error) => error.into_inner(),
            };
            for error in [direct, task] {
                assert!(
                    matches!(error, WithdrawError::InvalidFeePolicy(_)),
                    "options with native"
                );
                assert_eq!(error.status_code().as_u16(), 400);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// T17 and the switch: the controller check in the rail
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T17, R29: a match is confirmed once and reused; a mismatch refuses with 503, signs nothing, asks the
/// provider nothing and is not remembered; a failed read is the node error.
#[test]
fn t17_the_controller_check_guards_the_rail() {
    let log_captured = install_log_capture();
    let lab = Lab::new(Setup::usual());

    for _ in 0..3 {
        assert!(is_gasless(&lab.withdrawn(gasless(json!({})))));
    }
    assert_eq!(lab.contract_calls(GET_GASFREE_ADDRESS), 1, "a confirmation is reused");

    let refusing = Lab::new(Setup::usual());
    refusing.set_chain(|chain| chain.controller = Controller::Disagrees);
    for round in 1..=2 {
        let error = refusing.refused(gasless(json!({})));
        assert!(is_unavailable(&error));
        assert_eq!(
            refusing.contract_calls(GET_GASFREE_ADDRESS),
            round,
            "a mismatch is remembered"
        );
    }
    assert!(refusing.provider.requests().is_empty(), "the provider was asked");
    assert_eq!(refusing.contract_calls(BALANCE_OF), 0);
    // `auto` attempted the rail: its refusal is the answer.
    refusing.set_chain(|chain| chain.trx_sun = 0);
    assert!(is_unavailable(
        &refusing.refused(request(TOKEN, json!({ "fee_method": "auto" })))
    ));
    // The native rail stays available.
    refusing.set_chain(|chain| chain.trx_sun = 50_000_000);
    assert!(is_native(&refusing.withdrawn(request(TOKEN, json!({})))));

    let unreadable = Lab::new(Setup::usual());
    unreadable.set_chain(|chain| chain.controller = Controller::Down);
    let error = unreadable.refused(gasless(json!({})));
    assert!(matches!(error, WithdrawError::Transport(_)), "the node error");
    assert!(unreadable.provider.requests().is_empty(), "the provider was asked");
    // Nothing was remembered: once the controller answers, it is asked and the rail runs.
    let reads = unreadable.contract_calls(GET_GASFREE_ADDRESS);
    unreadable.set_chain(|chain| chain.controller = Controller::Agrees);
    assert!(is_gasless(&unreadable.withdrawn(gasless(json!({})))));
    assert!(unreadable.contract_calls(GET_GASFREE_ADDRESS) > reads);

    if log_captured {
        let warnings = captured_log_lines()
            .into_iter()
            .filter(|line| line.starts_with("WARN") && line.contains("GasFree is refused on nile"))
            .count();
        assert!(warnings >= 3, "one warning line per refused check");
    }
}

/// The switch `tron_gasfree_guard_allows_fallback`: the refusal of the controller check falls back to the native
/// rail only with the switch on and `fallback_to_native` set.
#[test]
fn the_guard_refusal_falls_back_only_with_the_switch_and_the_request() {
    let fallback = || gasless(json!({ "gasless": { "fallback_to_native": true } }));
    // The switch off, absent, or not a boolean: the 503 surfaces, also with `fallback_to_native`.
    for switch in [None, Some(json!(false)), Some(json!("true")), Some(json!(1))] {
        let lab = Lab::new(Setup {
            switch,
            ..Setup::usual()
        });
        lab.set_chain(|chain| chain.controller = Controller::Disagrees);
        assert!(is_unavailable(&lab.refused(fallback())));
        assert!(is_unavailable(&lab.refused(gasless(json!({})))));
        assert!(lab.provider.requests().is_empty(), "the provider was asked");
    }
    // The switch on.
    let lab = Lab::new(Setup {
        switch: Some(json!(true)),
        ..Setup::usual()
    });
    lab.set_chain(|chain| chain.controller = Controller::Disagrees);
    assert!(
        is_native(&lab.withdrawn(fallback())),
        "the native rail with its own fee details"
    );
    assert!(
        is_unavailable(&lab.refused(gasless(json!({})))),
        "no fallback without the request's consent"
    );
    assert!(
        is_unavailable(&lab.refused(gasless(json!({ "gasless": { "fallback_to_native": false } })))),
        "no fallback without the request's consent"
    );
    assert!(lab.provider.requests().is_empty(), "the provider was asked");
    // The switch changes nothing else: an agreeing controller runs the rail.
    lab.set_chain(|chain| chain.controller = Controller::Agrees);
    assert!(is_gasless(&lab.withdrawn(fallback())));
}

// ---------------------------------------------------------------------------------------------------------------
// The recovery procedure (D63.4 (b), D63.7)
// ---------------------------------------------------------------------------------------------------------------

/// D63.4 (b): a gasless withdraw whose `to` is the sender's own address is an ordinary gasless withdraw; the
/// amount comes back to the sender and only the provider's fee is spent.
#[test]
fn a_gasless_withdraw_to_the_own_address_is_the_return_path() {
    let lab = Lab::new(Setup::usual());
    let own = lab.sender().to_base58();
    let details = lab.withdrawn(gasless(json!({ "to": own })));
    let result = serde_json::to_value(&details).expect("JSON");
    assert_eq!(result["to"], json!([own]));
    assert_eq!(result["signed_authorization"]["receiver"], own.as_str());
    assert_eq!(result["signed_authorization"]["user"], own.as_str());
    assert_eq!(dec_of(&result["received_by_me"]), dec("10"));
    assert_eq!(dec_of(&result["spent_by_me"]), dec("12"));
    assert_eq!(dec_of(&result["my_balance_change"]), dec("-2"));
}

// ---------------------------------------------------------------------------------------------------------------
// D63.9, T18 to T20: the task route submits the authorization and follows the transfer
// ---------------------------------------------------------------------------------------------------------------

/// Whether `error` is the member `member` of the gasless family with the HTTP status `status` and names the trace
/// id of the stub provider in its text.
fn is_gasless_with_trace_id(error: &WithdrawError, member: &str, status: u16) -> bool {
    wire_types(error) == ("Gasless".to_owned(), Some(member.to_owned()))
        && error.status_code().as_u16() == status
        && error.to_string().contains(TRACE_ID)
}

/// D63.9: the task route reports the phases in order, makes one submit request whose members are the signed
/// ones, and answers the result with the trace id and the transaction hash. Each submission has its own version-4
/// `requestId`. The two INFO lines carry the trace id and no secret.
#[test]
fn the_task_route_submits_and_follows_the_transfer_to_its_settlement() {
    let log_captured = install_log_capture();
    let lab = Lab::new(Setup {
        status_poll_interval_ms: Some(150),
        ..Setup::usual()
    });
    let sender = lab.sender().to_base58();
    lab.set_relay(|relay| {
        relay.submit_delay_ms = 300;
        relay.set_trace(vec![
            Trace::Record("INPROGRESS", None, Some("NOT_ON_CHAIN")),
            Trace::Record("CONFIRMING", Some(TXN_HASH), Some("ON_CHAIN")),
            Trace::Record("SUCCEED", Some(TXN_HASH), Some("SOLIDITY")),
        ]);
    });

    let (status, progress) = lab.task_withdraw(gasless(json!({})));
    let phases = [
        json!("SubmittingGaslessAuthorization"),
        json!({ "WaitingForGaslessProvider": { "trace_id": TRACE_ID, "provider_state": "WAITING" } }),
        json!({ "WaitingForGaslessProvider": { "trace_id": TRACE_ID, "provider_state": "INPROGRESS" } }),
        json!({ "WaitingForGaslessSettlement": { "trace_id": TRACE_ID, "tx_hash": TXN_HASH } }),
    ];
    let positions: Vec<Option<usize>> = phases
        .iter()
        .map(|phase| progress.iter().position(|seen| seen == phase))
        .collect();
    assert!(positions.iter().all(Option::is_some), "a phase is not reported");
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the phases are out of order"
    );

    let status = serde_json::to_value(&status).expect("a status");
    assert_eq!(status["status"], "Ok");
    let details = &status["details"];
    let text = serde_json::to_string(details).expect("the details text");
    assert_rail_result(&text, &sender, Some((TRACE_ID, TXN_HASH)));

    // One submit request: every member is the member of the signed authorization, the version is 1.
    let submitted = lab.submit_calls();
    assert_eq!(submitted.len(), 1);
    assert_eq!(lab.trace_calls(), 3);
    let (body, authorization) = (&submitted[0], &details["signed_authorization"]);
    for (sent, signed) in [
        ("token", "token"),
        ("serviceProvider", "service_provider"),
        ("user", "user"),
        ("receiver", "receiver"),
        ("value", "value"),
        ("maxFee", "max_fee"),
        ("deadline", "deadline"),
        ("version", "version"),
        ("nonce", "nonce"),
        ("sig", "sig"),
    ] {
        assert!(body[sent].is_string(), "a submit member is not a string");
        assert!(
            body[sent] == authorization[signed],
            "a submit member is not the signed one"
        );
    }
    assert_eq!(body["version"], "1");
    assert_eq!(body.as_object().expect("an object").len(), 11);
    let request_id = |body: &Json| {
        let id = uuid::Uuid::parse_str(body["requestId"].as_str().expect("a request id")).expect("a UUID");
        assert_eq!(id.get_version_num(), 4);
        id
    };
    let first_id = request_id(body);

    // The next submission has another request id.
    lab.set_relay(|relay| relay.set_trace(vec![Trace::Record("SUCCEED", Some(TXN_HASH), None)]));
    let (status, _) = lab.task_withdraw(gasless(json!({})));
    assert!(
        matches!(status, WithdrawCompatRpcStatus::Ok(_)),
        "the second task succeeds"
    );
    let submitted = lab.submit_calls();
    assert_eq!(submitted.len(), 2);
    assert!(request_id(&submitted[1]) != first_id, "a request id is reused");

    if log_captured {
        let lines = captured_log_lines();
        let sig = body["sig"].as_str().expect("a string");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("GasFree submission is being sent")
                    && line.contains(TOKEN)
                    && line.contains(&sender)
                    && line.contains(RECEIVER)
                    && line.contains(&first_id.to_string())),
            "the sending of the submission is not logged"
        );
        assert!(
            lines.iter().any(|line| line.contains("GasFree transfer submitted")
                && line.contains(TRACE_ID)
                && line.contains(TOKEN)
                && line.contains(&sender)
                && line.contains(RECEIVER)),
            "the submission is not logged"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains(TRACE_ID) && line.contains("settled") && line.contains(TXN_HASH)),
            "the settlement is not logged"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(sig) && !line.contains(API_KEY) && !line.contains(API_SECRET)),
            "a log line carries a signature or a credential"
        );
    }
}

/// D63.9: `state` `FAILED` and `txnState` `ON_CHAIN_FAILED`, in a trace answer or in the answer to the submission,
/// end the task with `ProviderRejected` (502) and the trace id; a settled transfer without a hash cannot be used.
#[test]
fn a_failed_transfer_ends_the_task_and_names_the_trace_id() {
    let lab = Lab::new(Setup::usual());
    for trace in [
        vec![Trace::Record("FAILED", None, None)],
        vec![
            Trace::Record("WAITING", None, Some("INIT")),
            Trace::Record("CONFIRMING", Some(TXN_HASH), Some("ON_CHAIN_FAILED")),
        ],
        vec![Trace::Record("SUCCEED", Some(TXN_HASH), Some("ON_CHAIN_FAILED"))],
    ] {
        lab.set_relay(|relay| relay.set_trace(trace));
        let error = lab.task_refused(gasless(json!({})));
        assert!(
            is_gasless_with_trace_id(&error, "ProviderRejected", 502),
            "a failed transfer"
        );
        assert_no_credential(&format!("{error} {error:?}"));
    }

    // The answer to the submission says so itself: no trace request follows.
    let traced = lab.trace_calls();
    for patch in [json!({ "state": "FAILED" }), json!({ "txnState": "ON_CHAIN_FAILED" })] {
        lab.set_relay(|relay| relay.submit_patch = patch);
        let error = lab.task_refused(gasless(json!({})));
        assert!(
            is_gasless_with_trace_id(&error, "ProviderRejected", 502),
            "a failed submission answer"
        );
    }
    assert_eq!(lab.trace_calls(), traced, "a failed transfer was traced");

    lab.set_relay(|relay| {
        relay.submit_patch = json!({});
        relay.set_trace(vec![Trace::Record("SUCCEED", None, None)]);
    });
    let error = lab.task_refused(gasless(json!({})));
    assert!(
        is_gasless_with_trace_id(&error, "InvalidProviderResponse", 502),
        "a settled transfer without a transaction"
    );
}

/// D63.9 item 3: an answer to the submission whose nonce, version, sender, receiver, token or amount is not the
/// signed one, or whose id is no UUID, is `InvalidProviderResponse` (502); nothing is traced.
#[test]
fn a_submission_answer_that_differs_from_the_signed_values_is_not_trusted() {
    let lab = Lab::new(Setup::usual());
    let sender = lab.sender().to_base58();
    let patches = [
        json!({ "nonce": 8 }),
        json!({ "accountAddress": RECEIVER }),
        json!({ "targetAddress": sender }),
        json!({ "tokenAddress": USERS[3].address }),
        json!({ "amount": "10000001" }),
        json!({ "version": 2 }),
        json!({ "id": "not a uuid" }),
    ];
    let cases = patches.len();
    for patch in patches {
        lab.set_relay(|relay| relay.submit_patch = patch);
        let error = lab.task_refused(gasless(json!({})));
        assert_eq!(
            wire_types(&error),
            ("Gasless".to_owned(), Some("InvalidProviderResponse".to_owned()))
        );
        assert_eq!(error.status_code().as_u16(), 502);
        assert!(error.to_string().contains(MAYBE_ACCEPTED), "the advice is missing");
        assert_no_credential(&format!("{error} {error:?}"));
    }
    assert_eq!(lab.submit_calls().len(), cases, "a submission was repeated");
    assert_eq!(lab.trace_calls(), 0, "an untrusted transfer was traced");
}

/// D63.9 item 4: an authorization whose deadline has passed is `QuoteExpired` (400) and nothing is sent; the
/// sender's lock is free again.
#[test]
fn an_authorization_past_its_deadline_is_not_submitted() {
    let lab = Lab::new(Setup::usual());
    let binding = lab.binding();
    let lock = binding.sender_lock(&lab.sender());
    for age_s in [0, 5] {
        let signed = lab.signed(now_ms() / 1000 - age_s);
        let guard = block_on(lock.lock());
        let Err(error) = block_on(submit(binding, &signed, guard, None, TOKEN, &dec("10"))) else {
            panic!("an expired authorization must be refused")
        };
        assert_eq!(
            wire_types(&error),
            ("Gasless".to_owned(), Some("QuoteExpired".to_owned()))
        );
        assert_eq!(error.status_code().as_u16(), 400);
        assert!(
            !error.to_string().contains(MAYBE_ACCEPTED),
            "nothing was sent, and the text says the provider may have accepted"
        );
        assert!(lock.try_lock().is_some(), "the sender's lock is still held");
    }
    assert!(lab.provider.requests().is_empty(), "the provider was asked");
}

/// D63.9 item 7: no trace request is made at or after the bound; the task then ends with `Transport`, whose text
/// names the trace id and says that the outcome is unknown. The rail's bound is the deadline plus 120 seconds.
#[test]
fn the_wait_is_bounded_and_its_end_names_the_trace_id() {
    assert_eq!(SETTLEMENT_GRACE_MS, 120_000);
    let lab = Lab::new(Setup {
        status_poll_interval_ms: Some(50),
        ..Setup::usual()
    });
    lab.set_relay(|relay| relay.set_trace(vec![Trace::Record("INPROGRESS", None, None)]));
    let binding = lab.binding();
    let lock = binding.sender_lock(&lab.sender());
    let signed = lab.signed(now_ms() / 1000 + 300);
    let guard = block_on(lock.lock());
    let Ok(submitted) = block_on(submit(binding, &signed, guard, None, TOKEN, &dec("10"))) else {
        panic!("the provider takes the submission")
    };
    assert_eq!(submitted.trace_id, TRACE_ID);

    let started = Instant::now();
    let Err(error) = block_on(track(binding, submitted, now_ms() + 2_500, None)) else {
        panic!("the transfer must not settle")
    };
    let elapsed = started.elapsed();
    assert!(
        matches!(&error, WithdrawError::Transport(text) if text.contains(TRACE_ID) && text.contains("unknown")),
        "the end of the wait"
    );
    assert_eq!(error.status_code().as_u16(), 502);
    assert!(
        elapsed >= Duration::from_millis(2_400) && elapsed < Duration::from_secs(5),
        "the wait did not end at its bound"
    );
    let polls = lab.trace_calls();
    assert_eq!(polls, 2, "the transfer was not traced once a second");
    std::thread::sleep(Duration::from_millis(1_300));
    assert_eq!(lab.trace_calls(), polls, "the transfer was traced after the bound");
}

/// D63.9: a trace request that fails with a 5xx or a 429 is not the end; the next one is made and the transfer
/// settles.
#[test]
fn a_failed_trace_request_is_followed_by_the_next_one() {
    let log_captured = install_log_capture();
    let lab = Lab::new(Setup::usual());
    lab.set_relay(|relay| {
        relay.set_trace(vec![
            Trace::Raw(503, r#"{"code":503,"message":"maintenance"}"#),
            Trace::Raw(429, "slow down"),
            Trace::Record("SUCCEED", Some(TXN_HASH), Some("ON_CHAIN")),
        ])
    });
    let (status, _) = lab.task_withdraw(gasless(json!({})));
    let status = serde_json::to_value(&status).expect("a status");
    assert_eq!(status["status"], "Ok");
    assert_eq!(status["details"]["tx_hash"], TXN_HASH);
    assert_eq!(lab.trace_calls(), 3);
    assert_eq!(lab.submit_calls().len(), 1);
    if log_captured {
        assert!(
            captured_log_lines()
                .iter()
                .any(|line| line.contains(TRACE_ID) && line.contains("a trace request failed")),
            "the failed trace request is not logged"
        );
    }
}

/// ch.63 T20: a 404 of the trace request, as the HTTP status and as the envelope code, reaches the caller as
/// `TraceNotFound` (404); a 404 of the submit request stays `ProviderRejected` (502). Another trace error that is
/// not a passing failure ends the task with its own arm and the trace id.
#[test]
fn t20_a_trace_404_is_trace_not_found_and_a_submit_404_is_a_rejection() {
    let lab = Lab::new(Setup::usual());
    for raw in [
        Trace::Raw(404, "Not Found"),
        Trace::Raw(200, r#"{"code":404,"reason":"NotFound","message":"no such transfer"}"#),
    ] {
        let traced = lab.trace_calls();
        lab.set_relay(|relay| relay.set_trace(vec![raw]));
        let error = lab.task_refused(gasless(json!({})));
        assert!(
            is_gasless_with_trace_id(&error, "TraceNotFound", 404),
            "an unknown trace"
        );
        assert_eq!(lab.trace_calls(), traced + 1, "an unknown trace was asked for again");
    }

    lab.set_relay(|relay| relay.set_trace(vec![Trace::Raw(401, r#"{"code":401,"message":"bad key"}"#)]));
    let error = lab.task_refused(gasless(json!({})));
    assert!(
        matches!(&error, WithdrawError::Transport(text) if text.contains(TRACE_ID)),
        "a refused trace request"
    );
    lab.set_relay(|relay| relay.set_trace(vec![Trace::Raw(400, r#"{"code":400,"message":"bad id"}"#)]));
    let error = lab.task_refused(gasless(json!({})));
    assert!(
        is_gasless_with_trace_id(&error, "ProviderRejected", 502),
        "a rejected trace request"
    );

    let (submitted, traced) = (lab.submit_calls().len(), lab.trace_calls());
    lab.set_relay(|relay| relay.submit_raw = Some((404, r#"{"code":404,"message":"no such path"}"#)));
    let error = lab.task_refused(gasless(json!({})));
    assert_eq!(
        wire_types(&error),
        ("Gasless".to_owned(), Some("ProviderRejected".to_owned()))
    );
    assert_eq!(error.status_code().as_u16(), 502);
    assert!(
        !error.to_string().contains(MAYBE_ACCEPTED),
        "the provider refused, and the text says it may have accepted"
    );
    assert_eq!(lab.submit_calls().len(), submitted + 1);
    assert_eq!(lab.trace_calls(), traced, "a refused submission was traced");
}

/// ch.63 T18 through the task route: a submission that fails with a 5xx answer or a timeout ends the task with
/// `Transport`; the submit request is made once and nothing is traced. The sender's lock is free afterwards.
#[test]
fn t18_a_failed_submission_is_not_repeated_by_the_task() {
    let log_captured = install_log_capture();
    let failing = Lab::new(Setup::usual());
    failing.set_relay(|relay| relay.submit_raw = Some((503, r#"{"code":503,"message":"maintenance"}"#)));
    let slow = Lab::new(Setup {
        request_timeout_ms: Some(300),
        ..Setup::usual()
    });
    slow.set_relay(|relay| relay.submit_delay_ms = 1_200);

    for lab in [&failing, &slow] {
        let error = lab.task_refused(gasless(json!({})));
        assert!(
            matches!(&error, WithdrawError::Transport(text) if text.contains(MAYBE_ACCEPTED)),
            "a failed submission"
        );
        assert_eq!(error.status_code().as_u16(), 502);
        assert_no_credential(&format!("{error} {error:?}"));
        assert!(
            lab.binding().sender_lock(&lab.sender()).try_lock().is_some(),
            "the sender's lock is still held"
        );
    }
    if log_captured {
        let lines = captured_log_lines();
        let sender = failing.sender().to_base58();
        let warned: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains("GasFree submission with an unknown outcome"))
            .collect();
        assert!(
            warned.iter().any(|line| line.contains(TOKEN)
                && line.contains(&sender)
                && line.contains(RECEIVER)
                && line.contains("request id")),
            "the unknown outcome is not logged"
        );
        let sigs: Vec<String> = [&failing, &slow]
            .iter()
            .flat_map(|lab| lab.submit_calls())
            .filter_map(|body| body["sig"].as_str().map(str::to_owned))
            .collect();
        assert!(
            warned.iter().all(|line| !line.contains(API_KEY)
                && !line.contains(API_SECRET)
                && !sigs.iter().any(|sig| line.contains(sig))),
            "the warning carries a signature or a credential"
        );
    }
    // Time for a repeated request to arrive, if one were made.
    std::thread::sleep(Duration::from_millis(1_400));
    for lab in [&failing, &slow] {
        assert_eq!(lab.submit_calls().len(), 1, "the submission was repeated");
        assert_eq!(lab.trace_calls(), 0, "a failed submission was traced");
    }
}

/// D63.9 item 1: the sender's lock is released when the provider answered the submission; a second send from the
/// same address takes it while the first transfer is still awaited.
#[test]
fn the_sender_lock_is_released_before_the_wait_for_the_settlement() {
    let lab = Lab::new(Setup {
        status_poll_interval_ms: Some(50),
        ..Setup::usual()
    });
    lab.set_relay(|relay| relay.set_trace(vec![Trace::Record("WAITING", None, None)]));
    let task_id = lab.start_task(gasless(json!({})));
    lab.wait_for_phase(task_id, "WaitingForGaslessProvider");

    assert!(
        lab.binding().sender_lock(&lab.sender()).try_lock().is_some(),
        "the sender's lock is held during the wait"
    );
    let second = lab.withdrawn(gasless(json!({})));
    assert!(is_gasless(&second));
    assert!(
        matches!(lab.task_status(task_id), WithdrawCompatRpcStatus::InProgress(_)),
        "the first transfer ended before the second send"
    );

    lab.set_relay(|relay| relay.set_trace(vec![Trace::Record("SUCCEED", Some(TXN_HASH), None)]));
    let started = Instant::now();
    while matches!(lab.task_status(task_id), WithdrawCompatRpcStatus::InProgress(_)) {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the first transfer did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        matches!(lab.task_status(task_id), WithdrawCompatRpcStatus::Ok(_)),
        "the first transfer settles"
    );
    assert_eq!(lab.submit_calls().len(), 1);
}

/// D63.9: `task::withdraw::cancel` during the wait stops the trace requests; the submission is not repeated.
#[test]
fn a_cancel_during_the_wait_stops_the_trace_requests() {
    let lab = Lab::new(Setup {
        status_poll_interval_ms: Some(50),
        ..Setup::usual()
    });
    lab.set_relay(|relay| relay.set_trace(vec![Trace::Record("WAITING", None, None)]));
    let task_id = lab.start_task(gasless(json!({})));
    lab.wait_for_phase(task_id, "WaitingForGaslessProvider");

    let cancel = WithdrawStatusRequest {
        task_id,
        forget_if_finished: false,
    };
    assert!(
        block_on(withdraw_cancel(lab.ctx.clone(), cancel)).is_ok(),
        "the task is cancelled"
    );
    // A trace request that was under way may still arrive.
    std::thread::sleep(Duration::from_millis(300));
    let polls = lab.trace_calls();
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(lab.trace_calls(), polls, "a cancelled task still traces");
    assert_eq!(lab.submit_calls().len(), 1);
}

/// D63.9: under `auto` the task route submits too, once the native rail could not be funded; the direct method
/// does not.
#[test]
fn auto_on_the_task_route_submits_after_a_funding_failure_of_the_native_rail() {
    let lab = Lab::new(Setup::usual());
    lab.set_chain(|chain| chain.trx_sun = 0);
    let auto = || request(TOKEN, json!({ "fee_method": "auto" }));

    let direct = lab.withdrawn(auto());
    assert!(is_gasless(&direct) && direct.tx_hash.is_empty());
    assert!(lab.submit_calls().is_empty() && lab.trace_calls() == 0);

    let (status, _) = lab.task_withdraw(auto());
    let status = serde_json::to_value(&status).expect("a status");
    assert_eq!(status["status"], "Ok");
    assert_eq!(status["details"]["tx_hash"], TXN_HASH);
    assert_eq!(status["details"]["fee_details"]["trace_id"], TRACE_ID);
    assert_eq!(lab.submit_calls().len(), 1);
}

/// D63.9: two trace requests of one transfer are at least a second apart, also when the configured interval is 0.
#[test]
fn a_poll_interval_of_zero_is_raised_to_a_second() {
    assert_eq!(MIN_POLL_INTERVAL_MS, 1_000);
    let lab = Lab::new(Setup {
        status_poll_interval_ms: Some(0),
        ..Setup::usual()
    });
    assert_eq!(lab.binding().status_poll_interval_ms(), 0);
    lab.set_relay(|relay| {
        relay.set_trace(vec![
            Trace::Record("INPROGRESS", None, None),
            Trace::Record("SUCCEED", Some(TXN_HASH), None),
        ])
    });
    let binding = lab.binding();
    let lock = binding.sender_lock(&lab.sender());
    let signed = lab.signed(now_ms() / 1000 + 300);
    let guard = block_on(lock.lock());
    let Ok(submitted) = block_on(submit(binding, &signed, guard, None, TOKEN, &dec("10"))) else {
        panic!("the provider takes the submission")
    };
    let started = Instant::now();
    let settled = block_on(track(binding, submitted, now_ms() + 30_000, None));
    let elapsed = started.elapsed();
    assert!(settled.is_ok(), "the transfer settles");
    assert_eq!(lab.trace_calls(), 2);
    assert!(
        elapsed >= Duration::from_millis(1_900) && elapsed < Duration::from_secs(10),
        "the trace requests are less than a second apart"
    );
}

/// §63.4.7, status note of 2026-11-22: the task route settles a transfer whose records have the shape the
/// provider sends on the Nile test network (the times as Unix milliseconds, a trace answer without `signature`,
/// `maxFee`, `updatedAt` and `version`, unknown transaction members as empty strings).
#[test]
fn the_task_route_settles_a_transfer_with_the_records_of_the_live_provider() {
    let lab = Lab::new(Setup::usual());
    let sender = lab.sender().to_base58();
    lab.set_relay(|relay| {
        relay.live_shape = true;
        relay.set_trace(vec![
            Trace::Record("WAITING", None, None),
            Trace::Record("SUCCEED", Some(TXN_HASH), Some("ON_CHAIN")),
        ]);
    });
    let (status, progress) = lab.task_withdraw(gasless(json!({})));
    let status = serde_json::to_value(&status).expect("a status");
    assert_eq!(status["status"], "Ok");
    let text = serde_json::to_string(&status["details"]).expect("the details text");
    assert_rail_result(&text, &sender, Some((TRACE_ID, TXN_HASH)));
    assert!(
        progress
            .contains(&json!({ "WaitingForGaslessProvider": { "trace_id": TRACE_ID, "provider_state": "WAITING" } })),
        "the waiting phase is not reported"
    );
    assert_eq!(lab.submit_calls().len(), 1);
    assert_eq!(lab.trace_calls(), 2);
}
