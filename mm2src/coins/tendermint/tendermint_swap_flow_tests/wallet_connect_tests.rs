//! Tests of the Tendermint coin activated with a WalletConnect session (CRD ch.36 §36.14.2: R36.14.13 --
//! R36.14.15, D36.15, D36.16; T36.40, T36.41; ch.18 R18.11.23a).
//!
//! The wallet is scripted: [`ScriptedWallet`] implements the narrow [`CosmosWallet`] trait the coin reads the
//! subsystem through, keeps the sessions it is given and records every request it receives, and the node is the
//! scripted node of the swap-flow tests. No network is used. The keys are fixed throwaway keys; no address in this
//! file holds funds.

use super::super::tendermint_coin_from_conf_and_request_with_wallet_connect;
use super::super::tendermint_helpers::amino_bank_send_json;
use super::super::wc_integration::{CosmosWallet, TestWalletSlot, WcNamespace, WcSessionView};
use super::watch_only_tests::{script_t36_node, secp_key, t36_withdraw, watch_only};
use super::*;
use crate::rpc_command::tendermint::staking::DelegationPayload;
use crate::{DelegationError, TransactionDetails, TxFeeDetails, WithdrawError};
use async_trait::async_trait;
use common::{HttpStatusCode, StatusCode};
use cosmrs::proto::cosmos::tx::signing::v1beta1::SignMode as SignModeProto;
use cosmrs::proto::cosmos::tx::v1beta1::mode_info::Sum as ModeSum;
use cosmrs::proto::cosmos::tx::v1beta1::{AuthInfo as AuthInfoProto, TxBody as TxBodyProto};
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::session::KeyInfo;
use kdf_walletconnect::Topic;
use std::any::Any as AnyValue;
use std::collections::HashMap;

const TOPIC: &str = "session-topic";
/// The signature the scripted wallet answers; the node never verifies it.
const WALLET_SIGNATURE: [u8; 64] = [0x77; 64];
/// The gas the scripted node reports for a simulation.
const SIMULATED_GAS: u64 = 100_000;
/// The gas limit a WalletConnect request without an explicit one gets: the simulated gas times the margin 1.5.
const WC_GAS_LIMIT: u64 = 150_000;
/// The fee of such a request at the gas price 0.25 of the test chain: the limit times the price.
const WC_FEE_SAT: u64 = 37_500;
const MSG_SEND_URL: &str = "/cosmos.bank.v1beta1.MsgSend";

// ---------------------------------------------------------------------------
// The scripted wallet
// ---------------------------------------------------------------------------

type Responder = dyn Fn(&str, &Json) -> Result<Json, WalletConnectError> + Send + Sync;

struct ScriptedWallet {
    sessions: Mutex<HashMap<String, WcSessionView>>,
    /// The method and the whole inner payload of every request received, oldest first.
    requests: Mutex<Vec<(String, Json)>>,
    responder: Mutex<Arc<Responder>>,
}

impl ScriptedWallet {
    fn new(view: WcSessionView) -> Arc<Self> {
        let sessions = Mutex::new(HashMap::from([(TOPIC.to_owned(), view)]));
        let initial: Arc<Responder> = signing_responder();
        Arc::new(ScriptedWallet {
            sessions,
            requests: Mutex::new(Vec::new()),
            responder: Mutex::new(initial),
        })
    }

    fn respond(&self, f: impl Fn(&str, &Json) -> Result<Json, WalletConnectError> + Send + Sync + 'static) {
        let responder: Arc<Responder> = Arc::new(f);
        *self.responder.lock().unwrap() = responder;
    }

    fn methods(&self) -> Vec<String> { self.requests.lock().unwrap().iter().map(|(m, _)| m.clone()).collect() }

    fn payloads_of(&self, method: &str) -> Vec<Json> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    fn drop_session(&self) { self.sessions.lock().unwrap().remove(TOPIC); }

    fn set_view(&self, view: WcSessionView) { self.sessions.lock().unwrap().insert(TOPIC.to_owned(), view); }
}

#[async_trait]
impl CosmosWallet for ScriptedWallet {
    fn session_view(&self, topic: &Topic) -> Option<WcSessionView> {
        self.sessions.lock().unwrap().get(&topic.to_string()).cloned()
    }

    async fn send_request(&self, _topic: &Topic, payload: Json) -> Result<Json, WalletConnectError> {
        let method = payload["request"]["method"].as_str().unwrap_or_default().to_owned();
        self.requests.lock().unwrap().push((method.clone(), payload.clone()));
        let responder = self.responder.lock().unwrap().clone();
        responder(&method, &payload)
    }
}

/// A wallet that signs: `cosmos_signDirect` echoes the requested document, `cosmos_signAmino` echoes the Amino one.
fn signing_responder() -> Arc<Responder> {
    Arc::new(|method, payload| {
        let params = &payload["request"]["params"];
        let signature = json!({ "signature": hex::encode(WALLET_SIGNATURE) });
        match method {
            "cosmos_signDirect" => Ok(json!({
                "signature": signature,
                "signed": {
                    "chainId": params["signDoc"]["chainId"],
                    "accountNumber": params["signDoc"]["accountNumber"],
                    "bodyBytes": params["signDoc"]["bodyBytes"],
                    "authInfoBytes": params["signDoc"]["authInfoBytes"],
                },
            })),
            "cosmos_signAmino" => Ok(json!({ "signature": signature, "signed": params["signDoc"] })),
            other => Err(WalletConnectError::Internal(format!("not scripted: {other}"))),
        }
    })
}

/// A wallet that refuses: the answer carries an `error` member and no result.
fn refusing_responder() -> Arc<Responder> {
    Arc::new(|_, _| Ok(json!({ "id": 5, "jsonrpc": "2.0", "error": { "code": 5000, "message": "User rejected" } })))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

fn own_address() -> String { account(Chain::Iris, &pubkey(&OWN_KEY)).to_string() }

fn peer_address() -> String { account(Chain::Iris, &pubkey(&PEER_KEY)).to_string() }

fn caip10(chain_id: &str, address: &str) -> String { format!("cosmos:{chain_id}:{address}") }

/// A key record of the session properties (ch.22 §22.8.1.6); the key is the hex of the compressed key bytes.
fn record(chain_id: &str, key: &[u8; 32], is_nano_ledger: bool) -> KeyInfo {
    KeyInfo {
        chain_id: chain_id.to_owned(),
        name: "account".to_owned(),
        algo: "secp256k1".to_owned(),
        pub_key: hex::encode(pubkey(key)),
        address: String::new(),
        bech32_address: String::new(),
        ethereum_hex_address: String::new(),
        is_nano_ledger,
        is_keystone: false,
    }
}

fn namespace(chains: &[&str], accounts: &[String]) -> WcNamespace {
    WcNamespace {
        chains: chains.iter().map(|chain| (*chain).to_owned()).collect(),
        accounts: accounts.to_vec(),
    }
}

fn view_of(namespaces: Vec<(&str, WcNamespace)>, keys: Vec<KeyInfo>) -> WcSessionView {
    WcSessionView {
        wallet_name: "TestWallet".to_owned(),
        namespaces: namespaces
            .into_iter()
            .map(|(name, namespace)| (name.to_owned(), namespace))
            .collect(),
        keys,
    }
}

/// The session of the account of `OWN_KEY` on the test chain, with its key record, Ledger-backed or not.
fn standard_view(ledger: bool) -> WcSessionView {
    view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![record("irishub-1", &OWN_KEY, ledger)],
    )
}

/// The same session without any key record: the key comes from the account enumeration.
fn view_without_records() -> WcSessionView {
    view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        Vec::new(),
    )
}

/// An enumeration answer of one account entry whose key is given by `pubkey`.
fn enumeration_answer(address: &str, algo: &str, pubkey: Json) -> Json {
    json!([{ "address": address, "algo": algo, "pubkey": pubkey }])
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

struct Harness {
    ctx: MmArc,
    node: FakeNode,
    wallet: Arc<ScriptedWallet>,
}

fn harness(view: WcSessionView) -> Harness {
    let wallet = ScriptedWallet::new(view);
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }))
        .into_mm_arc();
    let slot: Arc<dyn AnyValue + Send + Sync> = Arc::new(TestWalletSlot(wallet.clone()));
    *ctx.wallet_connect.lock().unwrap() = Some(slot);
    Harness {
        ctx,
        node: FakeNode::start(),
        wallet,
    }
}

impl Harness {
    fn activate(&self, topic: &str) -> Result<TendermintCoin, String> {
        common::block_on(tendermint_coin_from_conf_and_request_with_wallet_connect(
            &self.ctx,
            "IRIS".to_owned(),
            &Chain::Iris.conf(),
            vec![RpcNode::for_test(&self.node.url())],
            topic,
            false,
        ))
        .map_err(|e| e.to_string())
    }

    fn coin(&self) -> TendermintCoin {
        self.activate(TOPIC)
            .unwrap_or_else(|e| panic!("cannot activate with the session: {e}"))
    }

    /// The node of the withdraw tests: account number 7, sequence 3, height 1,000, balances of 10,000,000 base units,
    /// a simulation that reports [`SIMULATED_GAS`] and a broadcast that succeeds.
    fn script_node(&self) { self.script_node_with(None, None); }

    /// [`Self::script_node`] with another answer to the simulation (`abci_query` result) and/or to the broadcast
    /// (a result, or `{ "__rpc_error": .. }`).
    fn script_node_with(&self, simulate_answer: Option<Json>, broadcast_answer: Option<Json>) {
        let account = QueryAccountResponse {
            account: Some(Any {
                type_url: "/cosmos.auth.v1beta1.BaseAccount".to_owned(),
                value: BaseAccount {
                    account_number: 7,
                    sequence: 3,
                    ..Default::default()
                }
                .encode_to_vec(),
            }),
        }
        .encode_to_vec();
        let balance = QueryBalanceResponse {
            balance: Some(CoinProto {
                denom: "scripted".to_owned(),
                amount: "10000000".to_owned(),
            }),
        }
        .encode_to_vec();
        let simulation = SimulateResponse {
            gas_info: Some(GasInfo {
                gas_wanted: 125_000,
                gas_used: SIMULATED_GAS,
            }),
            ..Default::default()
        }
        .encode_to_vec();
        let tx_result = json!({ "code": 0, "data": null, "log": "", "info": "", "gas_wanted": "0", "gas_used": "0",
                                "events": [], "codespace": "" });
        let simulate_answer = simulate_answer.unwrap_or_else(|| abci_value(&simulation));
        let broadcast_answer = broadcast_answer.unwrap_or_else(
            || json!({ "check_tx": tx_result, "deliver_tx": tx_result, "hash": BROADCAST_HASH, "height": "1001" }),
        );
        self.node.answer(move |method, params| match method {
            "abci_info" => Some(
                json!({ "response": { "data": "test", "version": "0", "app_version": "1",
                                        "last_block_height": "1000", "last_block_app_hash": "" } }),
            ),
            "abci_query" if params["path"] == "/cosmos.auth.v1beta1.Query/Account" => Some(abci_value(&account)),
            "abci_query" if params["path"] == "/cosmos.bank.v1beta1.Query/Balance" => Some(abci_value(&balance)),
            "abci_query" if params["path"] == "/cosmos.tx.v1beta1.Service/Simulate" => Some(simulate_answer.clone()),
            "broadcast_tx_commit" => Some(broadcast_answer.clone()),
            _ => None,
        });
    }

    fn simulations(&self) -> usize {
        self.node
            .requests("abci_query")
            .iter()
            .filter(|params| params["path"] == "/cosmos.tx.v1beta1.Service/Simulate")
            .count()
    }

    fn broadcasts(&self) -> usize { self.node.requests("broadcast_tx_commit").len() }
}

/// The hash the scripted node answers to a broadcast.
const BROADCAST_HASH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn withdraw_typed(coin: &impl MmCoin, ticker: &str) -> Result<TransactionDetails, WithdrawError> {
    let request: WithdrawRequest = serde_json::from_value(json!({
        "coin": ticker, "to": peer_address(), "amount": "1" }))
    .expect("a withdraw request");
    common::block_on(coin.withdraw(request).compat()).map_err(|e| e.get_inner().clone())
}

fn delegation() -> DelegationPayload {
    serde_json::from_value(json!({ "validator_address": peer_address(), "amount": "1" })).expect("a delegation payload")
}

fn assert_signing_error(error: &WithdrawError, context: &str) {
    assert!(matches!(error, WithdrawError::SigningError(_)), "{context}: {error:?}");
    assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR, "{context}");
    let wire = serde_json::to_value(error).expect("a serializable error");
    assert_eq!(wire["error_type"], "SigningError", "{context}");
    assert!(wire["error_data"].is_string(), "{context}");
}

fn tx_raw_of(details: &TransactionDetails) -> TxRaw {
    TxRaw::decode(details.tx_hex.0.as_slice()).expect("the signed transaction decodes as a TxRaw")
}

// ---------------------------------------------------------------------------
// T36.40, R36.14.14, D36.16: the activation
// ---------------------------------------------------------------------------

#[test]
fn a_key_record_for_the_chain_activates_without_a_wallet_request() {
    // T36.40: the address is the bech32 account of the record's key under the coin's prefix, the wallet is not
    // contacted and neither is the node; the coin holds no private key.
    let h = harness(standard_view(false));
    let coin = h.coin();
    assert_eq!(coin.my_address().expect("an address"), own_address());
    assert!(h.wallet.methods().is_empty(), "the wallet was contacted");
    assert!(h.node.seen.lock().unwrap().is_empty(), "the node was contacted");
    let (topic, ledger) = coin
        .activation_policy
        .wallet_connect_session()
        .expect("a WalletConnect policy");
    assert_eq!(topic.to_string(), TOPIC);
    assert!(!ledger);
    assert!(!coin.activation_policy.has_local_key());
    assert!(coin.activation_policy.watch_only_ledger_flag().is_none());
    assert!(coin.display_priv_key().is_err(), "no private key to display");
    assert!(
        coin.get_htlc_key_pair().is_none(),
        "a WalletConnect coin has no HTLC key"
    );
}

#[test]
fn the_ledger_flag_is_the_matching_records() {
    // R36.14.14 item 7, D36.16 (b): the flag of the record that matches the chain.
    let h = harness(standard_view(true));
    let coin = h.coin();
    assert_eq!(
        coin.activation_policy
            .wallet_connect_session()
            .map(|(_, ledger)| ledger),
        Some(true)
    );
}

#[test]
fn a_session_without_records_sends_exactly_one_parameterless_enumeration() {
    // T36.40: one `cosmos_getAccounts`, no parameter (ch.22 §22.8.1.2), for the coin's chain; the key of its answer
    // is read as base64 text, as hex text and as an array of byte values; the Ledger flag is false.
    let key = pubkey(&OWN_KEY);
    for pubkey_form in [
        json!(base64::encode(&key)),
        json!(hex::encode(&key)),
        json!(key.clone()),
    ] {
        let h = harness(view_without_records());
        let answer = enumeration_answer(&own_address(), "secp256k1", pubkey_form.clone());
        h.wallet.respond(move |_, _| Ok(answer.clone()));
        let coin = h.coin();
        assert_eq!(coin.my_address().expect("an address"), own_address(), "{pubkey_form}");
        assert_eq!(
            h.wallet.methods(),
            vec!["cosmos_getAccounts".to_owned()],
            "{pubkey_form}"
        );
        let payload = &h.wallet.payloads_of("cosmos_getAccounts")[0];
        assert_eq!(payload["chainId"], "cosmos:irishub-1");
        assert_eq!(payload["request"]["params"], json!({}), "parameterless");
        assert_eq!(
            coin.activation_policy
                .wallet_connect_session()
                .map(|(_, ledger)| ledger),
            Some(false)
        );
    }
}

#[test]
fn the_algorithm_names_of_the_two_spellings_are_accepted() {
    for algo in ["secp256k1", "tendermint/PubKeySecp256k1"] {
        let h = harness(view_without_records());
        let answer = enumeration_answer(&own_address(), algo, json!(hex::encode(pubkey(&OWN_KEY))));
        h.wallet.respond(move |_, _| Ok(answer.clone()));
        assert_eq!(h.coin().my_address().expect("an address"), own_address(), "{algo}");
    }
}

#[test]
fn the_record_of_another_chain_placed_first_is_not_used() {
    // D36.16 (b): the key record whose chain id is the coin's, not simply the first record; its Ledger flag too.
    let view = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![
            record("cosmoshub-4", &PEER_KEY, true),
            record("cosmos:irishub-1", &OWN_KEY, false),
        ],
    );
    let h = harness(view);
    let coin = h.coin();
    assert_eq!(coin.my_address().expect("an address"), own_address());
    assert!(h.wallet.methods().is_empty());
    assert_eq!(
        coin.activation_policy
            .wallet_connect_session()
            .map(|(_, ledger)| ledger),
        Some(false),
        "the flag of the other chain's record is not taken"
    );

    // With the record of another chain only, the enumeration is the fallback and its key is the one used.
    let view = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![record("cosmoshub-4", &PEER_KEY, true)],
    );
    let h = harness(view);
    let answer = enumeration_answer(&own_address(), "secp256k1", json!(hex::encode(pubkey(&OWN_KEY))));
    h.wallet.respond(move |_, _| Ok(answer.clone()));
    let coin = h.coin();
    assert_eq!(coin.my_address().expect("an address"), own_address());
    assert_eq!(h.wallet.methods(), vec!["cosmos_getAccounts".to_owned()]);
    assert_eq!(
        coin.activation_policy
            .wallet_connect_session()
            .map(|(_, ledger)| ledger),
        Some(false)
    );
}

#[test]
fn an_address_that_differs_from_the_sessions_account_is_refused() {
    // D36.16 (b): the key of the record gives another account than the session grants for the chain.
    let view = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![record("irishub-1", &PEER_KEY, false)],
    );
    let err = harness(view).activate(TOPIC).expect_err("a mismatch is refused");
    assert!(err.contains("WalletConnect") && err.contains("differs"), "{err}");

    // The same for a key that came from the enumeration.
    let h = harness(view_without_records());
    let answer = enumeration_answer(&peer_address(), "secp256k1", json!(hex::encode(pubkey(&PEER_KEY))));
    h.wallet.respond(move |_, _| Ok(answer.clone()));
    let err = h.activate(TOPIC).expect_err("a mismatch is refused");
    assert!(err.contains("differs"), "{err}");
}

#[test]
fn every_failure_of_the_session_reading_is_a_wallet_connect_activation_error() {
    // T36.40: an unknown topic, a session that does not grant the chain, a session without an account for the chain,
    // an `algo` other than the two, an invalid key, and a wallet that refuses, does not answer or returns no
    // account; each fails the activation and builds no coin.
    let own_key_hex = hex::encode(pubkey(&OWN_KEY));
    let failing = |h: Harness, topic: &str, needle: &str| {
        let err = h.activate(topic).expect_err(needle);
        assert!(err.contains("WalletConnect activation failed"), "{needle}: {err}");
        assert!(err.contains(needle), "{needle}: {err}");
    };

    failing(harness(standard_view(false)), "no-such-topic", "no live session");

    let other_chain = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:cosmoshub-4"], &[caip10("cosmoshub-4", &own_address())]),
        )],
        vec![record("cosmoshub-4", &OWN_KEY, false)],
    );
    failing(harness(other_chain), TOPIC, "does not grant the chain");

    let no_account = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("cosmoshub-4", &own_address())]),
        )],
        vec![record("irishub-1", &OWN_KEY, false)],
    );
    failing(harness(no_account), TOPIC, "no account for the chain");

    let mut wrong_algo = record("irishub-1", &OWN_KEY, false);
    wrong_algo.algo = "ed25519".to_owned();
    let view = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![wrong_algo],
    );
    failing(harness(view), TOPIC, "algorithm");

    let mut bad_key = record("irishub-1", &OWN_KEY, false);
    bad_key.pub_key = "00".repeat(33);
    let view = view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:irishub-1"], &[caip10("irishub-1", &own_address())]),
        )],
        vec![bad_key],
    );
    failing(harness(view), TOPIC, "valid secp256k1");

    // The enumeration: a refusal, no answer in time, an empty list, an entry of the wrong algorithm, no list.
    let h = harness(view_without_records());
    h.wallet
        .respond(|_, _| Ok(json!({ "error": { "code": 5000, "message": "rejected" } })));
    failing(h, TOPIC, "refused");
    let h = harness(view_without_records());
    h.wallet.respond(|_, _| Err(WalletConnectError::Timeout));
    failing(h, TOPIC, "enumeration failed");
    let h = harness(view_without_records());
    h.wallet.respond(|_, _| Ok(json!([])));
    failing(h, TOPIC, "no account");
    let h = harness(view_without_records());
    let answer = enumeration_answer(&own_address(), "ed25519", json!(own_key_hex));
    h.wallet.respond(move |_, _| Ok(answer.clone()));
    failing(h, TOPIC, "algorithm");
    let h = harness(view_without_records());
    h.wallet.respond(|_, _| Ok(json!({ "not": "a list" })));
    failing(h, TOPIC, "not a list");
}

#[test]
fn the_chain_is_granted_in_each_of_the_three_ways() {
    // R36.14.14 item 2. (a) The cosmos namespace lists the chain: `standard_view`. (b) It lists no chains and a key
    // record has the chain id. (c) The session has a namespace keyed by the full chain identifier.
    let accounts = [caip10("irishub-1", &own_address())];
    let b = view_of(vec![("cosmos", namespace(&[], &accounts))], vec![record(
        "irishub-1",
        &OWN_KEY,
        false,
    )]);
    assert_eq!(harness(b).coin().my_address().expect("an address"), own_address());
    let c = view_of(vec![("cosmos:irishub-1", namespace(&[], &accounts))], vec![record(
        "irishub-1",
        &OWN_KEY,
        false,
    )]);
    assert_eq!(harness(c).coin().my_address().expect("an address"), own_address());

    // Without a key record of the chain, a namespace that lists no chains grants nothing.
    let none = view_of(vec![("cosmos", namespace(&[], &accounts))], vec![record(
        "cosmoshub-4",
        &OWN_KEY,
        false,
    )]);
    let err = harness(none).activate(TOPIC).expect_err("not granted");
    assert!(err.contains("does not grant the chain"), "{err}");
}

#[test]
fn an_invalid_node_set_fails_before_the_wallet_is_contacted() {
    // R36.1.4 step 3: the node set is validated first; the wallet is not asked for nothing.
    let h = harness(view_without_records());
    let result = common::block_on(tendermint_coin_from_conf_and_request_with_wallet_connect(
        &h.ctx,
        "IRIS".to_owned(),
        &Chain::Iris.conf(),
        Vec::new(),
        TOPIC,
        false,
    ));
    assert!(result.is_err());
    assert!(h.wallet.methods().is_empty());
}

// ---------------------------------------------------------------------------
// R18.11.23a, D18.11.10 (a): orders are refused for the coin and its token
// ---------------------------------------------------------------------------

#[test]
fn orders_are_refused_for_a_wallet_connect_coin_and_its_token() {
    for ledger in [false, true] {
        let h = harness(standard_view(ledger));
        let coin = h.coin();
        assert!(coin.wallet_only_with_switch(&h.ctx, true), "the coin, ledger {ledger}");
        assert!(
            nyan_token(&coin, false).wallet_only_with_switch(&h.ctx, true),
            "the token, ledger {ledger}"
        );
        assert!(coin.wallet_only(&h.ctx) && nyan_token(&coin, false).wallet_only(&h.ctx));
    }
}

// ---------------------------------------------------------------------------
// T36.41, R36.14.15, D36.15: signing
// ---------------------------------------------------------------------------

#[test]
fn a_direct_withdraw_is_signed_by_the_wallet_and_broadcast_by_nobody() {
    // T36.41: exactly one `cosmos_signDirect`; `tx_hex` and `tx_hash` answered; nothing broadcast; one simulation
    // prices the request (gas limit = simulated gas times 1.5, fee = limit times price); the signed transaction
    // carries the wallet's signature and the body that was requested.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let details = withdraw_typed(&coin, "IRIS").expect("a signed withdraw");

    assert_eq!(h.wallet.methods(), vec!["cosmos_signDirect".to_owned()]);
    assert_eq!(h.broadcasts(), 0, "withdraw broadcasts nothing");
    assert_eq!(h.simulations(), 1, "one simulation prices the request");
    assert!(!details.is_unsigned());
    let wire = serde_json::to_value(&details).expect("a serializable result");
    assert!(wire["tx_hex"].is_string() && wire["tx_hash"].is_string(), "{wire}");
    assert!(wire.get("sign_doc").is_none() && wire.get("legacy_amino_json").is_none());

    let raw = tx_raw_of(&details);
    assert_eq!(raw.signatures, vec![WALLET_SIGNATURE.to_vec()]);
    assert_eq!(details.tx_hash, hex::encode_upper(Sha256::digest(&details.tx_hex.0)));

    let payload = &h.wallet.payloads_of("cosmos_signDirect")[0];
    assert_eq!(payload["chainId"], "cosmos:irishub-1");
    assert_eq!(payload["request"]["params"]["signerAddress"], own_address());
    assert_eq!(payload["request"]["params"]["signDoc"]["chainId"], "irishub-1");
    assert_eq!(payload["request"]["params"]["signDoc"]["accountNumber"], "7");
    let requested_body = hex::decode(payload["request"]["params"]["signDoc"]["bodyBytes"].as_str().unwrap()).unwrap();
    assert_eq!(raw.body_bytes, requested_body, "the body that was asked to be signed");

    let body = TxBodyProto::decode(raw.body_bytes.as_slice()).expect("a body");
    assert_eq!(body.messages.len(), 1);
    assert_eq!(body.messages[0].type_url, MSG_SEND_URL);
    let send = MsgSend::decode(body.messages[0].value.as_slice()).expect("a bank send");
    assert_eq!(
        (send.from_address.as_str(), send.to_address.as_str()),
        (own_address().as_str(), peer_address().as_str())
    );
    assert_eq!(body.timeout_height, 1_100);
    let auth_info = AuthInfoProto::decode(raw.auth_info_bytes.as_slice()).expect("an auth info");
    assert_eq!(auth_info.signer_infos[0].sequence, 3);
    let Some(ModeSum::Single(single)) = auth_info.signer_infos[0].mode_info.as_ref().and_then(|m| m.sum.clone()) else {
        panic!("a single sign mode");
    };
    assert_eq!(single.mode, SignModeProto::Direct as i32);
    let fee = auth_info.fee.expect("a fee");
    assert_eq!(fee.amount[0].amount, WC_FEE_SAT.to_string());
    assert_eq!(fee.gas_limit, WC_GAS_LIMIT);
    // The reported fee is the one of the signed auth info.
    let TxFeeDetails::Tendermint(fee_details) = details.fee_details.clone().expect("fee details") else {
        panic!("a Tendermint fee");
    };
    assert_eq!((fee_details.uamount, fee_details.gas_limit), (WC_FEE_SAT, WC_GAS_LIMIT));
}

#[test]
fn the_from_selector_of_the_request_is_ignored() {
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let request: WithdrawRequest = serde_json::from_value(json!({
        "coin": "IRIS", "to": peer_address(), "amount": "1",
        "from": { "derivation_path": "m/44'/118'/0'/0/9" } }))
    .expect("a withdraw request");
    let details = common::block_on(coin.withdraw(request).compat()).expect("a signed withdraw");
    assert_eq!(details.from, vec![own_address()]);
    assert_eq!(h.wallet.methods(), vec!["cosmos_signDirect".to_owned()]);
}

#[test]
fn a_wallet_that_does_not_sign_fails_the_withdraw_with_a_signing_error() {
    // D36.15 (b), T36.41: a refusal, an unanswered request, a deleted session and a chain that is no longer granted
    // each fail with the typed `SigningError` (HTTP 500).
    // A refusal.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let refusing = refusing_responder();
    h.wallet.respond(move |method, payload| refusing(method, payload));
    assert_signing_error(&withdraw_typed(&coin, "IRIS").expect_err("a refusal"), "refusal");
    assert_eq!(h.wallet.methods().len(), 1, "the request was sent");

    // No answer within the time-to-live.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.respond(|_, _| Err(WalletConnectError::Timeout));
    assert_signing_error(&withdraw_typed(&coin, "IRIS").expect_err("a timeout"), "timeout");

    // The session is gone: no request is sent.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.drop_session();
    assert_signing_error(
        &withdraw_typed(&coin, "IRIS").expect_err("a deleted session"),
        "deleted session",
    );
    assert!(h.wallet.methods().is_empty(), "no request for a deleted session");

    // The session no longer grants the chain: no request is sent.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.set_view(view_of(
        vec![(
            "cosmos",
            namespace(&["cosmos:cosmoshub-4"], &[caip10("cosmoshub-4", &own_address())]),
        )],
        Vec::new(),
    ));
    assert_signing_error(
        &withdraw_typed(&coin, "IRIS").expect_err("a chain not granted"),
        "chain not granted",
    );
    assert!(
        h.wallet.methods().is_empty(),
        "no request when the chain is not granted"
    );
}

#[test]
fn a_failure_of_the_node_is_not_a_signing_error() {
    // D36.15 (b): only "the external wallet did not sign" is routed to the typed answer. The node answers nothing
    // here, so the withdraw fails before the wallet is asked, with another type.
    let h = harness(standard_view(false));
    let coin = h.coin();
    let error = withdraw_typed(&coin, "IRIS").expect_err("the node answers nothing");
    assert!(!matches!(error, WithdrawError::SigningError(_)), "{error:?}");
    assert!(h.wallet.methods().is_empty());
}

#[test]
fn an_echoed_body_that_differs_from_the_requested_one_fails_the_request() {
    // Finding 3 of §36.14.2 (Reloaded-original as-built point): the wallet's `signed` body must be the body that was
    // asked to be signed; the auth-info bytes may differ and the echoed ones are used.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.respond(|_, payload| {
        let params = &payload["request"]["params"];
        let body = format!("{}00", params["signDoc"]["bodyBytes"].as_str().unwrap());
        Ok(json!({ "signature": { "signature": hex::encode(WALLET_SIGNATURE) },
                   "signed": { "bodyBytes": body, "authInfoBytes": params["signDoc"]["authInfoBytes"] } }))
    });
    let error = withdraw_typed(&coin, "IRIS").expect_err("a different body");
    assert_signing_error(&error, "echo mismatch");
    assert!(error.to_string().contains("differs"), "{error}");

    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    // An auth info that still carries a fee (a wallet may rewrite the transaction's other parts) is used as echoed;
    // one without a fee cannot be reported and is refused.
    let echoed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let echoed_in_wallet = echoed.clone();
    h.wallet.respond(move |_, payload| {
        let params = &payload["request"]["params"];
        let requested = hex::decode(params["signDoc"]["authInfoBytes"].as_str().unwrap()).unwrap();
        let mut auth_info = AuthInfoProto::decode(requested.as_slice()).unwrap();
        auth_info.fee.as_mut().unwrap().granter = "iaa1granter".to_owned();
        let bytes = auth_info.encode_to_vec();
        *echoed_in_wallet.lock().unwrap() = bytes.clone();
        Ok(json!({ "signature": { "signature": hex::encode(WALLET_SIGNATURE) },
                   "signed": { "bodyBytes": params["signDoc"]["bodyBytes"], "authInfoBytes": hex::encode(bytes) } }))
    });
    let details = withdraw_typed(&coin, "IRIS").expect("another auth info is allowed");
    assert_eq!(
        tx_raw_of(&details).auth_info_bytes,
        *echoed.lock().unwrap(),
        "the echoed auth info is used"
    );

    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.respond(|_, payload| {
        let params = &payload["request"]["params"];
        Ok(json!({ "signature": { "signature": hex::encode(WALLET_SIGNATURE) },
                   "signed": { "bodyBytes": params["signDoc"]["bodyBytes"], "authInfoBytes": "0a00" } }))
    });
    let error = withdraw_typed(&coin, "IRIS").expect_err("an auth info without a fee");
    assert!(error.to_string().contains("carries no fee"), "{error}");
}

#[test]
fn an_answer_without_a_signature_is_a_signing_error() {
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.respond(|_, _| Ok(json!({ "unexpected": true })));
    assert_signing_error(
        &withdraw_typed(&coin, "IRIS").expect_err("no signature"),
        "no signature",
    );
}

#[test]
fn a_ledger_session_withdraw_signs_the_amino_document_with_no_timeout_height() {
    // T36.41, R36.14.15, R36.14.18, R36.14.31: `cosmos_signAmino`, a document without `timeout_height`, a fee three
    // times the simulated one, and a broadcast transaction whose body carries no timeout height and whose signer
    // uses the Amino sign mode.
    let h = harness(standard_view(true));
    h.script_node();
    let coin = h.coin();
    let details = withdraw_typed(&coin, "IRIS").expect("a signed Amino withdraw");
    assert_eq!(h.wallet.methods(), vec!["cosmos_signAmino".to_owned()]);
    assert_eq!(h.broadcasts(), 0);
    assert_eq!(h.simulations(), 1);

    let payload = &h.wallet.payloads_of("cosmos_signAmino")[0];
    let doc = &payload["request"]["params"]["signDoc"];
    assert!(doc.is_object() && doc.get("timeout_height").is_none(), "{doc}");
    assert_eq!(doc["chain_id"], "irishub-1");
    assert_eq!(doc["account_number"], "7");
    assert_eq!(doc["sequence"], "3");
    assert_eq!(doc["memo"], "");
    assert_eq!(doc["fee"]["gas"], WC_GAS_LIMIT.to_string());
    assert_eq!(
        doc["fee"]["amount"],
        json!([{ "amount": (WC_FEE_SAT * 3).to_string(), "denom": "uiris" }])
    );
    assert_eq!(doc["msgs"][0]["type"], "cosmos-sdk/MsgSend");

    let raw = tx_raw_of(&details);
    let body = TxBodyProto::decode(raw.body_bytes.as_slice()).expect("a body");
    assert_eq!(body.timeout_height, 0, "the broadcast body carries no timeout height");
    assert_eq!(body.messages.len(), 1);
    assert_eq!(body.messages[0].type_url, MSG_SEND_URL);
    let auth_info = AuthInfoProto::decode(raw.auth_info_bytes.as_slice()).expect("an auth info");
    let Some(ModeSum::Single(single)) = auth_info.signer_infos[0].mode_info.as_ref().and_then(|m| m.sum.clone()) else {
        panic!("a single sign mode");
    };
    assert_eq!(single.mode, SignModeProto::LegacyAminoJson as i32);
    assert_eq!(auth_info.signer_infos[0].sequence, 3);
    assert_eq!(
        auth_info.fee.expect("a fee").amount[0].amount,
        (WC_FEE_SAT * 3).to_string()
    );
    assert_eq!(raw.signatures, vec![WALLET_SIGNATURE.to_vec()]);
    let TxFeeDetails::Tendermint(fee_details) = details.fee_details.clone().expect("fee details") else {
        panic!("a Tendermint fee");
    };
    // The reported fee is the one of the signed (Amino) auth info.
    assert_eq!(
        (fee_details.uamount, fee_details.gas_limit),
        (WC_FEE_SAT * 3, WC_GAS_LIMIT)
    );
}

#[test]
fn the_amino_document_of_the_wallet_request_is_the_one_of_the_watch_only_result() {
    // R36.14.31 member by member: the document a WalletConnect coin sends to the wallet equals, as a JSON value, the
    // `legacy_amino_json` a watch-only coin of the same key, flag, node and request answers (one builder).
    let h = harness(standard_view(true));
    h.script_node();
    let coin = h.coin();
    withdraw_typed(&coin, "IRIS").expect("a signed Amino withdraw");
    let sent = h.wallet.payloads_of("cosmos_signAmino")[0]["request"]["params"]["signDoc"].clone();

    let watch = watch_only(secp_key(), true);
    script_t36_node(&watch, 7, 3);
    let wire = t36_withdraw(&watch.coin, "IRIS", "1", None);
    // The two documents are one builder's output; they differ in the fee only: a watch-only result carries the fixed
    // assumed fee (R36.14.8), a WalletConnect request the simulated one (as-built note of 2026-11-17).
    let (mut watch_doc, mut sent_doc) = (wire["legacy_amino_json"].clone(), sent.clone());
    assert_ne!(watch_doc["fee"], sent_doc["fee"]);
    watch_doc.as_object_mut().expect("an object").remove("fee");
    sent_doc.as_object_mut().expect("an object").remove("fee");
    assert_eq!(watch_doc, sent_doc);
    // The members of R36.14.31 item 3 and no other.
    let document = sent.as_object().expect("an object");
    let mut members: Vec<&str> = document.keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, [
        "account_number",
        "chain_id",
        "fee",
        "memo",
        "msgs",
        "sequence"
    ]);
    assert_eq!(
        wire["original_tx_type_url"], MSG_SEND_URL,
        "the type URL is the watch-only result's member, the message of the broadcast body is the bank send"
    );
}

#[test]
fn the_amino_document_has_every_number_as_a_string_also_at_zero() {
    // R36.14.31 item 3: the fee coin is present also at zero, `"0"` for a zero account number and sequence, and the
    // memo is present also when empty.
    let fee = Fee::from_amount_and_gas(coin_of(Chain::Iris, 0), 125_000u64);
    let msg = Any {
        type_url: MSG_SEND_URL.to_owned(),
        value: MsgSend {
            from_address: own_address(),
            to_address: peer_address(),
            amount: vec![CoinProto {
                denom: "uiris".to_owned(),
                amount: "1".to_owned(),
            }],
        }
        .encode_to_vec(),
    };
    let amino = amino_bank_send_json(&msg).expect("a bank send has an Amino form");
    let doc = super::super::tendermint_helpers::amino_sign_doc("irishub-1", 0, 0, &fee, "", vec![amino]);
    assert_eq!(doc["account_number"], "0");
    assert_eq!(doc["sequence"], "0");
    assert_eq!(doc["memo"], "");
    assert_eq!(
        doc["fee"],
        json!({ "amount": [{ "amount": "0", "denom": "uiris" }], "gas": "125000" })
    );
    assert!(doc.get("timeout_height").is_none());
}

#[test]
fn a_ledger_session_cannot_sign_staking_and_asks_the_wallet_nothing() {
    // R36.14.18 item 3: the Amino form exists for the bank send only, so a delegation fails when it is built, before
    // any wallet request (and so would an IBC `withdraw`, which this tree does not have).
    let h = harness(standard_view(true));
    h.script_node();
    let coin = h.coin();
    let err = common::block_on(coin.delegate(delegation())).expect_err("a delegation fails with the flag");
    assert!(
        matches!(err.get_inner(), DelegationError::InternalError(text) if text.contains("Amino")),
        "{err}"
    );
    assert!(h.wallet.methods().is_empty(), "no wallet request");
    assert_eq!(h.broadcasts(), 0);
}

#[test]
fn a_delegation_is_signed_by_the_wallet_and_broadcast_as_with_a_local_key() {
    // R36.14.15: the staking requests sign through the wallet and then broadcast.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let details = common::block_on(coin.delegate(delegation())).expect("a signed delegation");
    assert_eq!(h.wallet.methods(), vec!["cosmos_signDirect".to_owned()]);
    assert_eq!(h.broadcasts(), 1);
    assert_eq!(h.simulations(), 1);
    assert!(!details.is_unsigned());
    assert_eq!(details.tx_hash, BROADCAST_HASH);
    let raw = tx_raw_of(&details);
    assert_eq!(raw.signatures, vec![WALLET_SIGNATURE.to_vec()]);
    let body = TxBodyProto::decode(raw.body_bytes.as_slice()).expect("a body");
    assert_eq!(body.messages[0].type_url, "/cosmos.staking.v1beta1.MsgDelegate");
    // The node got exactly the signed bytes.
    assert_eq!(
        h.node.requests("broadcast_tx_commit")[0]["tx"],
        base64::encode(&details.tx_hex.0)
    );
}

#[test]
fn a_wallet_that_does_not_sign_fails_a_delegation_with_a_signing_error() {
    // D36.15 (b) for the staking requests: the typed answer (HTTP 500), and nothing is broadcast.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let refusing = refusing_responder();
    h.wallet.respond(move |method, payload| refusing(method, payload));
    let err = common::block_on(coin.delegate(delegation())).expect_err("a refusal");
    assert!(matches!(err.get_inner(), DelegationError::SigningError(_)), "{err}");
    assert_eq!(err.get_inner().status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    let wire = serde_json::to_value(err.get_inner()).expect("a serializable error");
    assert_eq!(wire["error_type"], "SigningError");
    assert_eq!(h.broadcasts(), 0);
}

#[test]
fn a_token_withdraw_follows_the_platform_and_the_ledger_factor_is_the_platforms_only() {
    // A token on a WalletConnect platform is signed by the wallet like the platform coin: a bank send in the token's
    // denomination, the fee in the platform denomination; the factor three of the Ledger flag is not applied to it
    // (R36.14.31 item 5).
    let plain = harness(standard_view(false));
    plain.script_node();
    let coin = plain.coin();
    let details = withdraw_typed(&nyan_token(&coin, false), "IRIS-NYAN").expect("a signed token withdraw");
    assert_eq!(plain.wallet.methods(), vec!["cosmos_signDirect".to_owned()]);
    let raw = tx_raw_of(&details);
    let body = TxBodyProto::decode(raw.body_bytes.as_slice()).expect("a body");
    let send = MsgSend::decode(body.messages[0].value.as_slice()).expect("a bank send");
    assert_eq!(send.amount[0].denom, "unyan");
    let fee = AuthInfoProto::decode(raw.auth_info_bytes.as_slice())
        .unwrap()
        .fee
        .expect("a fee");
    assert_eq!(
        (fee.amount[0].denom.as_str(), fee.amount[0].amount.as_str()),
        ("uiris", WC_FEE_SAT.to_string().as_str())
    );

    let ledger = harness(standard_view(true));
    ledger.script_node();
    let coin = ledger.coin();
    let details = withdraw_typed(&nyan_token(&coin, false), "IRIS-NYAN").expect("a signed Amino token withdraw");
    assert_eq!(ledger.wallet.methods(), vec!["cosmos_signAmino".to_owned()]);
    let doc = &ledger.wallet.payloads_of("cosmos_signAmino")[0]["request"]["params"]["signDoc"];
    assert_eq!(
        doc["fee"]["amount"],
        json!([{ "amount": WC_FEE_SAT.to_string(), "denom": "uiris" }])
    );
    assert_eq!(
        doc["msgs"][0]["value"]["amount"],
        json!([{ "amount": "1000000", "denom": "unyan" }])
    );
    let TxFeeDetails::Tendermint(fee_details) = details.fee_details.expect("fee details") else {
        panic!("a Tendermint fee");
    };
    assert_eq!(fee_details.uamount, WC_FEE_SAT);
}

// ---------------------------------------------------------------------------
// CRD ch.36 R36.14.15, as-built note of 2026-11-17: the simulated fee, the reported fee, the broadcast
// ---------------------------------------------------------------------------

fn withdraw_with_fee(coin: &impl MmCoin, fee: Json) -> Result<TransactionDetails, WithdrawError> {
    let request: WithdrawRequest = serde_json::from_value(json!({
        "coin": "IRIS", "to": peer_address(), "amount": "1", "fee": fee }))
    .expect("a withdraw request");
    common::block_on(coin.withdraw(request).compat()).map_err(|e| e.get_inner().clone())
}

#[test]
fn the_simulation_carries_the_activated_key_and_an_empty_signature() {
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    withdraw_typed(&coin, "IRIS").expect("a signed withdraw");

    let requests: Vec<Json> = h
        .node
        .requests("abci_query")
        .into_iter()
        .filter(|params| params["path"] == "/cosmos.tx.v1beta1.Service/Simulate")
        .collect();
    assert_eq!(requests.len(), 1);
    let data = requests[0]["data"].as_str().expect("the request data");
    let simulate = cosmrs::proto::cosmos::tx::v1beta1::SimulateRequest::decode(hex::decode(data).unwrap().as_slice())
        .expect("a simulate request");
    let raw = TxRaw::decode(simulate.tx_bytes.as_slice()).expect("a TxRaw");
    assert_eq!(raw.signatures, vec![Vec::<u8>::new()], "one empty signature slot");
    let auth_info = AuthInfoProto::decode(raw.auth_info_bytes.as_slice()).expect("an auth info");
    assert!(
        auth_info.signer_infos[0].public_key.is_some(),
        "the activated public key"
    );
    assert_eq!(auth_info.signer_infos[0].sequence, 3);
}

#[test]
fn a_request_with_an_explicit_gas_limit_keeps_it() {
    // The limit of the request is kept; the amount is the simulated gas times the margin times the price.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    let details = withdraw_with_fee(
        &coin,
        json!({ "type": "CosmosGas", "gas_limit": 200_000, "gas_price": 0.5 }),
    )
    .expect("a signed withdraw");
    let fee = AuthInfoProto::decode(tx_raw_of(&details).auth_info_bytes.as_slice())
        .unwrap()
        .fee
        .expect("a fee");
    assert_eq!(fee.gas_limit, 200_000);
    assert_eq!(fee.amount[0].amount, "75000", "100,000 * 1.5 * 0.5");
}

#[test]
fn a_failed_simulation_fails_the_withdraw_before_the_wallet_is_asked() {
    // No guessed limit is signed: the node answers no simulation, so the request fails and the wallet is not asked.
    let h = harness(standard_view(false));
    h.script_node_with(
        Some(json!({ "response": { "code": 11, "log": "out of gas in simulation", "value": "" } })),
        None,
    );
    let coin = h.coin();
    let error = withdraw_typed(&coin, "IRIS").expect_err("the simulation fails");
    assert!(!matches!(error, WithdrawError::SigningError(_)), "{error:?}");
    assert!(h.wallet.methods().is_empty(), "the wallet is not asked");
}

#[test]
fn the_reported_fee_is_the_fee_of_the_auth_info_the_wallet_signed() {
    // Direct: the wallet answers an auth info whose fee differs from the requested one (a wallet may rewrite it);
    // `fee_details` carries the fee of the transaction that is answered.
    let h = harness(standard_view(false));
    h.script_node();
    let coin = h.coin();
    h.wallet.respond(|_, payload| {
        let params = &payload["request"]["params"];
        let requested = hex::decode(params["signDoc"]["authInfoBytes"].as_str().unwrap()).unwrap();
        let mut auth_info = AuthInfoProto::decode(requested.as_slice()).unwrap();
        let fee = auth_info.fee.as_mut().unwrap();
        fee.amount[0].amount = "900".to_owned();
        fee.gas_limit = 150_001;
        Ok(json!({ "signature": { "signature": hex::encode(WALLET_SIGNATURE) },
                   "signed": { "bodyBytes": params["signDoc"]["bodyBytes"],
                               "authInfoBytes": hex::encode(auth_info.encode_to_vec()) } }))
    });
    let details = withdraw_typed(&coin, "IRIS").expect("a signed withdraw");
    let signed = AuthInfoProto::decode(tx_raw_of(&details).auth_info_bytes.as_slice())
        .unwrap()
        .fee
        .expect("a fee");
    assert_eq!((signed.amount[0].amount.as_str(), signed.gas_limit), ("900", 150_001));
    let TxFeeDetails::Tendermint(fee_details) = details.fee_details.expect("fee details") else {
        panic!("a Tendermint fee");
    };
    assert_eq!((fee_details.uamount, fee_details.gas_limit), (900, 150_001));
    assert_eq!(fee_details.amount.to_string(), "0.0009");
    // The totals follow the signed fee: the amount sent (1) plus 0.0009.
    let expected_total = "1.0009".parse::<BigDecimal>().unwrap();
    assert_eq!(details.total_amount, expected_total);
    assert_eq!(details.spent_by_me, expected_total);
    assert_eq!(details.my_balance_change, -expected_total);
}

#[test]
fn a_broadcast_the_node_already_holds_answers_the_hash_of_the_transaction() {
    // The node answers "tx already exists in cache" to a broadcast it has seen: the broadcast succeeded earlier, and
    // the answer is the hash computed from the bytes.
    let h = harness(standard_view(false));
    h.script_node_with(
        None,
        Some(json!({ "__rpc_error": { "code": -32603, "message": "Internal error",
                                       "data": "tx already exists in cache" } })),
    );
    let coin = h.coin();
    let details = withdraw_typed(&coin, "IRIS").expect("a signed withdraw");
    let hash = common::block_on(coin.send_raw_tx_bytes(&details.tx_hex.0).compat()).expect("an accepted rebroadcast");
    assert_eq!(hash, details.tx_hash);

    // Any other node error stays an error.
    let h = harness(standard_view(false));
    h.script_node_with(
        None,
        Some(json!({ "__rpc_error": { "code": -32603, "message": "Internal error", "data": "mempool is full" } })),
    );
    let coin = h.coin();
    let details = withdraw_typed(&coin, "IRIS").expect("a signed withdraw");
    assert!(common::block_on(coin.send_raw_tx_bytes(&details.tx_hex.0).compat()).is_err());
}
