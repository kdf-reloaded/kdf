//! HD address methods and `from` of an ERC-20 withdraw on an HD EVM platform (CRD ch.49 R49.32 last paragraph,
//! R49.33, R49.34, T49.31, T49.32; ch.05 R54).
//!
//! The platform and the token are activated through the public activation routines against a scripted EVM node that
//! answers per address, so the HD methods and both withdraw routes run end to end. The node records every balance
//! and nonce query, which shows that a refused selector reaches the node with none.

use crate::erc20_token_activation::{enable_evm_token, EnableEvmTokenRequest};
use crate::eth_with_tokens_activation::EthWithTokensActivationRequest;
use crate::evm_test_support::*;
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensReq};
use coins::eth::{checksum_address, signed_eth_tx_from_bytes, EthCoin};
use coins::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletRpcOps};
use coins::rpc_command::account_balance::AccountBalanceRpcOps;
use coins::rpc_command::init_account_balance::InitAccountBalanceRpcOps;
use coins::rpc_command::init_scan_for_new_addresses::{init_scan_for_new_addresses, init_scan_for_new_addresses_status,
                                                      InitScanAddressesRpcOps};
use coins::rpc_command::init_withdraw::{init_withdraw, withdraw_status, WithdrawCompatRpcStatus};
use coins::{lp_coinfind, DerivationMethod, MmCoin, MmCoinEnum, TransactionDetails, WithdrawError, WithdrawRequest};
use common::{block_on, HttpStatusCode, StatusCode};
use crypto::Bip44Chain;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "USDT-ERC20";
const RECEIVER: &str = "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94";

/// What the scripted chain holds per address, and every balance and nonce query it has answered.
#[derive(Default)]
struct Chain {
    native: Mutex<HashMap<String, u128>>,
    nonces: Mutex<HashMap<String, u64>>,
    tokens: Mutex<HashMap<String, u128>>,
    queries: Mutex<Vec<String>>,
}

impl Chain {
    fn set_native(&self, address: &str, value: u128) {
        self.native.lock().unwrap().insert(address.to_lowercase(), value);
    }
    fn set_nonce(&self, address: &str, value: u64) {
        self.nonces.lock().unwrap().insert(address.to_lowercase(), value);
    }
    fn set_token(&self, address: &str, value: u128) {
        self.tokens.lock().unwrap().insert(address.to_lowercase(), value);
    }
    fn forget_queries(&self) { self.queries.lock().unwrap().clear(); }
    fn queries(&self) -> Vec<String> { self.queries.lock().unwrap().clone() }
    /// The queries of the withdraw kinds only: a token balance or a nonce.
    fn sender_queries(&self) -> Vec<String> {
        self.queries()
            .into_iter()
            .filter(|q| q.starts_with("nonce ") || q.starts_with("token "))
            .collect()
    }
}

fn chain_node(chain: Arc<Chain>) -> MockEvmNode {
    spawn_node(move |method, params| {
        let address = |value: &Json| value.as_str().unwrap_or_default().to_lowercase();
        match method {
            "eth_getBalance" => {
                let who = address(&params[0]);
                let value = chain.native.lock().unwrap().get(&who).copied().unwrap_or(0);
                chain.queries.lock().unwrap().push(format!("native {who}"));
                Reply::Result(json!(format!("0x{value:x}")))
            },
            "eth_getTransactionCount" => {
                let who = address(&params[0]);
                let value = chain.nonces.lock().unwrap().get(&who).copied().unwrap_or(0);
                chain.queries.lock().unwrap().push(format!("nonce {who}"));
                Reply::Result(json!(format!("0x{value:x}")))
            },
            "eth_call" => {
                let data = params[0]["data"]
                    .as_str()
                    .or_else(|| params[0]["input"].as_str())
                    .unwrap_or_default();
                if data.starts_with("0x70a08231") && data.len() >= 74 {
                    let who = format!("0x{}", &data[data.len() - 40..]).to_lowercase();
                    let value = chain.tokens.lock().unwrap().get(&who).copied().unwrap_or(0);
                    chain.queries.lock().unwrap().push(format!("token {who}"));
                    return Reply::Result(json!(abi_word(value)));
                }
                healthy_node_reply(method, params, 0, 0)
            },
            _ => healthy_node_reply(method, params, 0, 0),
        }
    })
}

/// Activates the platform and then the token through the public routines, and makes three external addresses of
/// account 0 known.
fn activate(ctx: MmArc, node: &MockEvmNode, known_addresses: u32) -> MmArc {
    let request: EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest> = serde_json::from_value(json!({
        "ticker": PLATFORM,
        "nodes": [{ "url": node.url }],
        "erc20_tokens_requests": [],
        "swap_contract_address": SWAP_CONTRACT,
    }))
    .unwrap();
    block_on(enable_platform_coin_with_tokens::<EthCoin>(ctx.clone(), request))
        .unwrap_or_else(|e| panic!("the platform activation failed: {}", e));
    if let DerivationMethod::HDWallet(hd_wallet) = &coin(&ctx, PLATFORM).derivation_method {
        let mut accounts = block_on(hd_wallet.accounts.lock());
        accounts.get_mut(&0).expect("account 0").external_addresses_number = known_addresses;
    }
    let token: EnableEvmTokenRequest =
        serde_json::from_value(json!({ "ticker": TOKEN, "activation_params": {} })).unwrap();
    block_on(enable_evm_token(ctx.clone(), token)).unwrap_or_else(|e| panic!("the token activation failed: {}", e));
    ctx
}

fn coin(ctx: &MmArc, ticker: &str) -> EthCoin {
    match block_on(lp_coinfind(ctx, ticker)).unwrap().expect("the coin is active") {
        MmCoinEnum::EthCoin(coin) => coin,
        other => panic!("not an EVM coin: {}", other.ticker()),
    }
}

/// The address `index` of account 0, in lower case.
fn address_at(ctx: &MmArc, index: u32) -> String {
    let platform = coin(ctx, PLATFORM);
    let DerivationMethod::HDWallet(hd_wallet) = &platform.derivation_method else {
        panic!("an HD platform expected");
    };
    let accounts = block_on(hd_wallet.accounts.lock());
    let account = accounts.get(&0).expect("account 0");
    let derived = platform
        .derive_address(account, Bip44Chain::External, index)
        .unwrap_or_else(|e| panic!("derive: {}", e));
    format!("{:#x}", derived.address)
}

fn known_addresses(ctx: &MmArc) -> u32 {
    let platform = coin(ctx, PLATFORM);
    let DerivationMethod::HDWallet(hd_wallet) = &platform.derivation_method else {
        panic!("an HD platform expected");
    };
    let accounts = block_on(hd_wallet.accounts.lock());
    accounts
        .get(&0)
        .expect("account 0")
        .known_addresses_number(Bip44Chain::External)
        .unwrap()
}

fn hd_setup() -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = activate(hd_ctx(), &node, 3);
    (chain, node, ctx)
}

/// The tickers of a balance map in the wire form.
fn tickers(balance: &Json) -> Vec<String> {
    let mut tickers: Vec<String> = balance.as_object().expect("a balance map").keys().cloned().collect();
    tickers.sort();
    tickers
}

fn assert_full_checksummed(address: &str) {
    assert_eq!(address.len(), 42, "the full 20-byte address: {address}");
    assert!(!address.contains('\u{2026}') && !address.contains(".."), "{address}");
    assert_eq!(
        address,
        checksum_address(&address.to_lowercase()),
        "EIP-55 form: {address}"
    );
}

fn get_new_address_params() -> coins::hd_wallet::GetNewHDAddressParams {
    serde_json::from_value(json!({ "account_id": 0, "chain": "External" })).unwrap()
}

fn account_params() -> coins::rpc_command::account_balance::AccountBalanceParams {
    serde_json::from_value(json!({ "account_index": 0, "chain": "External", "limit": 20 })).unwrap()
}

fn scan_params(gap_limit: u32) -> coins::rpc_command::init_scan_for_new_addresses::ScanAddressesParams {
    serde_json::from_value(json!({ "account_index": 0, "gap_limit": gap_limit })).unwrap()
}

// ---------------------------------------------------------------------------------------------------------------
// F1: the address form of `get_new_address`
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn evm_hd_p1_get_new_address_answers_the_full_checksummed_address() {
    let (_chain, _node, ctx) = hd_setup();
    let platform = coin(&ctx, PLATFORM);
    let response = block_on(platform.get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    let address = answer["new_address"]["address"].as_str().unwrap();
    assert_full_checksummed(address);
    assert_eq!(address.to_lowercase(), address_at(&ctx, 3));
    assert_eq!(
        tickers(&answer["new_address"]["balance"]),
        vec![PLATFORM.to_owned(), TOKEN.to_owned()],
        "the platform ticker and the activated token (R49.32 item 1)"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// F2: the scan task of an EVM HD platform
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn evm_hd_p1_the_scan_task_for_an_evm_hd_platform_reaches_the_evm_scan() {
    let (chain, _node, ctx) = hd_setup();
    let used = address_at(&ctx, 3);
    chain.set_nonce(&used, 1);

    let request = serde_json::from_value(json!({ "coin": PLATFORM, "account_index": 0, "gap_limit": 2 })).unwrap();
    let task = block_on(init_scan_for_new_addresses(ctx.clone(), request)).unwrap_or_else(|e| panic!("{e}"));
    let mut status = Json::Null;
    for _ in 0..400 {
        let polled = block_on(init_scan_for_new_addresses_status(
            ctx.clone(),
            serde_json::from_value(json!({ "task_id": task.task_id, "forget_if_finished": false })).unwrap(),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        status = serde_json::to_value(&polled).unwrap();
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(status["status"], "Ok", "{status}");
    let found = status["details"]["new_addresses"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{status}");
    assert_full_checksummed(found[0]["address"].as_str().unwrap());
    assert_eq!(found[0]["address"].as_str().unwrap().to_lowercase(), used);
    assert_eq!(known_addresses(&ctx), 4);
}

// ---------------------------------------------------------------------------------------------------------------
// R49.32 last paragraph: an ERC-20 ticker is served from its platform's wallet
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn evm_hd_p1_get_new_address_with_a_token_ticker_answers_the_token_only_and_the_platform_lists_it() {
    let (chain, _node, ctx) = hd_setup();
    let fourth = address_at(&ctx, 3);
    chain.set_token(&fourth, 5_000_000);
    chain.set_native(&fourth, 9);

    let token = coin(&ctx, TOKEN);
    let response = block_on(token.get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    let address = answer["new_address"]["address"].as_str().unwrap();
    assert_full_checksummed(address);
    assert_eq!(address.to_lowercase(), fourth);
    assert_eq!(tickers(&answer["new_address"]["balance"]), vec![TOKEN.to_owned()]);
    assert_eq!(answer["new_address"]["balance"][TOKEN]["spendable"], "5");

    // The platform's own answer lists the generated address, with the platform ticker and the token.
    assert_eq!(known_addresses(&ctx), 4);
    let platform = coin(&ctx, PLATFORM);
    let listed = block_on(platform.account_balance_rpc(account_params())).unwrap_or_else(|e| panic!("{e}"));
    let listed = serde_json::to_value(&listed).unwrap();
    let addresses = listed["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 4);
    assert_eq!(addresses[3]["address"].as_str().unwrap().to_lowercase(), fourth);
    for entry in addresses {
        assert_eq!(tickers(&entry["balance"]), vec![PLATFORM.to_owned(), TOKEN.to_owned()]);
    }
}

#[test]
fn evm_hd_p1_account_balance_with_a_token_ticker_answers_token_only_maps() {
    let (chain, _node, ctx) = hd_setup();
    chain.set_token(&address_at(&ctx, 1), 2_000_000);
    chain.set_token(&address_at(&ctx, 2), 3_000_000);
    chain.set_native(&address_at(&ctx, 1), 1_000_000_000_000_000_000);
    let token = coin(&ctx, TOKEN);

    let answer = block_on(token.account_balance_rpc(account_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&answer).unwrap();
    let addresses = answer["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 3);
    for (index, entry) in addresses.iter().enumerate() {
        assert_full_checksummed(entry["address"].as_str().unwrap());
        assert_eq!(
            entry["address"].as_str().unwrap().to_lowercase(),
            address_at(&ctx, index as u32)
        );
        assert_eq!(tickers(&entry["balance"]), vec![TOKEN.to_owned()]);
    }
    assert_eq!(addresses[1]["balance"][TOKEN]["spendable"], "2");
    assert_eq!(answer["page_balance"]["spendable"], "5", "the page sums the token only");

    let initiated =
        block_on(token.init_account_balance_rpc(serde_json::from_value(json!({ "account_index": 0 })).unwrap()))
            .unwrap_or_else(|e| panic!("{e}"));
    let initiated = serde_json::to_value(&initiated).unwrap();
    assert_eq!(tickers(&initiated["total_balance"]), vec![TOKEN.to_owned()]);
    assert_eq!(initiated["total_balance"][TOKEN]["spendable"], "5");
    for entry in initiated["addresses"].as_array().unwrap() {
        assert_eq!(tickers(&entry["balance"]), vec![TOKEN.to_owned()]);
    }
}

#[test]
fn evm_hd_p1_the_scan_with_a_token_ticker_finds_an_address_that_holds_only_that_token() {
    let (chain, _node, ctx) = hd_setup();
    // Address 3 holds only the token: nonce 0 and no native balance.
    let token_only = address_at(&ctx, 3);
    chain.set_token(&token_only, 4_000_000);

    // (The platform ticker's scan finds this address too, by the token-aware rule of R49.32 item 3; see
    // `evm_hd_p2_the_scan_lists_unqueried_addresses_with_an_empty_map_and_finds_a_token_only_address`.)
    assert_eq!(known_addresses(&ctx), 3);

    let token = coin(&ctx, TOKEN);
    let by_token = block_on(token.init_scan_for_new_addresses_rpc(scan_params(2))).unwrap_or_else(|e| panic!("{e}"));
    let by_token = serde_json::to_value(&by_token).unwrap();
    let found = by_token["new_addresses"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{by_token}");
    assert_full_checksummed(found[0]["address"].as_str().unwrap());
    assert_eq!(found[0]["address"].as_str().unwrap().to_lowercase(), token_only);
    assert_eq!(tickers(&found[0]["balance"]), vec![TOKEN.to_owned()]);
    assert_eq!(found[0]["balance"][TOKEN]["spendable"], "4");
    assert_eq!(known_addresses(&ctx), 4, "the platform's wallet now knows the address");
}

#[test]
fn evm_hd_p1_a_token_on_a_single_key_platform_keeps_its_current_answer() {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain);
    let ctx = activate(iguana_ctx(), &node, 0);
    let token = coin(&ctx, TOKEN);
    let platform = coin(&ctx, PLATFORM);

    let wire = |e: &dyn erased::Wire| e.json();
    let by_token = error_of(block_on(token.get_new_address_rpc(get_new_address_params())), "refused");
    let by_platform = error_of(
        block_on(platform.get_new_address_rpc(get_new_address_params())),
        "refused",
    );
    assert_eq!(wire(&by_token), wire(&by_platform));
    assert_eq!(wire(&by_token)["error_type"], "CoinIsActivatedNotWithHDWallet");

    let by_token = error_of(block_on(token.account_balance_rpc(account_params())), "refused");
    assert_eq!(wire(&by_token)["error_type"], "CoinIsActivatedNotWithHDWallet");
    let by_token = error_of(
        block_on(token.init_scan_for_new_addresses_rpc(scan_params(2))),
        "refused",
    );
    assert_eq!(wire(&by_token)["error_type"], "CoinIsActivatedNotWithHDWallet");
}

mod erased {
    use mm2_err_handle::prelude::{MmError, NotMmError};
    use serde_json::Value as Json;

    /// The wire form of an error.
    pub trait Wire {
        fn json(&self) -> Json;
    }

    impl<E: serde::Serialize + NotMmError> Wire for MmError<E> {
        fn json(&self) -> Json { serde_json::to_value(self.get_inner()).unwrap() }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// R49.34, T49.32: `from` on an ERC-20 withdraw under software HD
// ---------------------------------------------------------------------------------------------------------------

fn withdraw_request(from: Option<Json>) -> WithdrawRequest {
    let mut request = json!({
        "coin": TOKEN,
        "to": RECEIVER,
        "amount": "1",
        "fee": { "type": "EthGas", "gas_price": "1", "gas": 150000 },
    });
    if let Some(from) = from {
        request["from"] = from;
    }
    serde_json::from_value(request).unwrap_or_else(|e| panic!("a withdraw request: {e}"))
}

fn direct(ctx: &MmArc, request: WithdrawRequest) -> Result<TransactionDetails, (WithdrawError, StatusCode)> {
    let token = coin(ctx, TOKEN);
    block_on(token.withdraw(request).compat()).map_err(|e| {
        let status = e.get_inner().status_code();
        (e.into_inner(), status)
    })
}

fn by_task(ctx: &MmArc, request: WithdrawRequest) -> Result<TransactionDetails, WithdrawError> {
    let init = block_on(init_withdraw(ctx.clone(), request)).unwrap_or_else(|e| panic!("init: {e}"));
    for _ in 0..400 {
        let status = block_on(withdraw_status(
            ctx.clone(),
            serde_json::from_value(json!({ "task_id": init.task_id, "forget_if_finished": false })).unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status: {e}"));
        match status {
            WithdrawCompatRpcStatus::Ok(details) => return Ok(details),
            WithdrawCompatRpcStatus::Error(e) => return Err(e),
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    panic!("the withdraw task did not finish");
}

/// The sender the signature of `details` recovers to, in lower case.
fn signer(details: &TransactionDetails) -> String {
    let signed = signed_eth_tx_from_bytes(&details.tx_hex.0).expect("a signed transaction");
    format!("{:#x}", signed.sender())
}

/// The `from` list of the completed transaction.
fn from_of(details: &TransactionDetails) -> Vec<String> {
    serde_json::from_value(serde_json::to_value(details).unwrap()["from"].clone()).unwrap()
}

fn id_form(address_id: u32) -> Json { json!({ "account_id": 0, "chain": "External", "address_id": address_id }) }

fn path_form(path: &str) -> Json { json!({ "derivation_path": path }) }

/// CRD ch.43 D43.1F.3 (b): a token looks its platform up in the registered view, so a token of a passive HD platform
/// keeps answering `get_new_address`, `account_balance` and `withdraw` with `from` from the platform's wallet.
#[test]
fn evm_hd_p1_a_token_of_a_passive_hd_platform_keeps_answering_its_hd_methods_and_withdraw_from() {
    let (chain, _node, ctx) = hd_setup();
    let first = address_at(&ctx, 0);
    let selected = address_at(&ctx, 1);
    let fourth = address_at(&ctx, 3);
    chain.set_token(&first, 10_000_000);
    chain.set_token(&selected, 10_000_000);
    chain.set_token(&fourth, 5_000_000);
    chain.set_nonce(&selected, 7);

    let coins_ctx = coins::CoinsContext::from_ctx(&ctx).unwrap();
    assert!(
        block_on(coins_ctx.make_passive(PLATFORM)),
        "the platform has a dependent token"
    );
    assert!(
        block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none(),
        "the passive platform is not an active coin"
    );

    let token = coin(&ctx, TOKEN);
    let response = block_on(token.get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    assert_eq!(
        answer["new_address"]["address"].as_str().unwrap().to_lowercase(),
        fourth
    );
    assert_eq!(tickers(&answer["new_address"]["balance"]), vec![TOKEN.to_owned()]);
    assert_eq!(answer["new_address"]["balance"][TOKEN]["spendable"], "5");

    let answer = block_on(token.account_balance_rpc(account_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&answer).unwrap();
    let addresses = answer["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 4);
    for entry in addresses {
        assert_eq!(tickers(&entry["balance"]), vec![TOKEN.to_owned()]);
    }
    assert_eq!(addresses[1]["balance"][TOKEN]["spendable"], "10");

    let details = direct(&ctx, withdraw_request(Some(id_form(1)))).unwrap_or_else(|e| panic!("{:?}", e.0));
    assert_eq!(signer(&details), selected, "the key of the selected address signs");
    assert_eq!(from_of(&details), vec![checksum_address(&selected)]);
}

#[test]
fn evm_hd_p1_a_token_withdraw_with_from_is_sent_from_the_selected_address_on_both_routes() {
    let (chain, _node, ctx) = hd_setup();
    let selected = address_at(&ctx, 1);
    chain.set_token(&selected, 10_000_000);
    chain.set_token(&address_at(&ctx, 0), 10_000_000);
    chain.set_nonce(&selected, 7);

    for from in [id_form(1), path_form("m/44'/60'/0'/0/1")] {
        for route in ["direct", "task"] {
            chain.forget_queries();
            let request = withdraw_request(Some(from.clone()));
            let details = match route {
                "direct" => direct(&ctx, request).unwrap_or_else(|e| panic!("{:?}", e.0)),
                _ => by_task(&ctx, request).unwrap_or_else(|e| panic!("{e}")),
            };
            assert_eq!(
                signer(&details),
                selected,
                "{route}: the key of the selected address signs"
            );
            assert_eq!(from_of(&details), vec![checksum_address(&selected)], "{route}");
            assert_eq!(
                signed_eth_tx_from_bytes(&details.tx_hex.0).unwrap().nonce,
                7.into(),
                "{route}"
            );
            let queries = chain.sender_queries();
            assert!(queries.contains(&format!("token {selected}")), "{route}: {queries:?}");
            assert!(queries.contains(&format!("nonce {selected}")), "{route}: {queries:?}");
            let activated = address_at(&ctx, 0);
            assert!(
                !queries.iter().any(|q| q.ends_with(&activated)),
                "{route}: nothing is asked of the activated address: {queries:?}"
            );
        }
    }
}

#[test]
fn evm_hd_p1_a_token_withdraw_without_from_is_sent_from_the_activated_address() {
    let (chain, _node, ctx) = hd_setup();
    let activated = address_at(&ctx, 0);
    chain.set_token(&activated, 10_000_000);
    for details in [
        direct(&ctx, withdraw_request(None)).unwrap_or_else(|e| panic!("{:?}", e.0)),
        by_task(&ctx, withdraw_request(None)).unwrap_or_else(|e| panic!("{e}")),
    ] {
        assert_eq!(signer(&details), activated);
        assert_eq!(from_of(&details), vec![checksum_address(&activated)]);
    }
}

#[test]
fn evm_hd_p1_a_token_selector_that_does_not_resolve_is_refused_before_any_query() {
    let (chain, _node, ctx) = hd_setup();
    chain.set_token(&address_at(&ctx, 0), 10_000_000);
    let cases = [
        (id_form(3), "UnexpectedFromAddress"),
        (
            json!({ "account_id": 1, "chain": "External", "address_id": 0 }),
            "UnknownAccount",
        ),
        (path_form("m/44'/61'/0'/0/1"), "UnexpectedFromAddress"),
        (path_form("m/44'/60'/0'/0/3"), "UnexpectedFromAddress"),
    ];
    for (from, error_type) in cases {
        chain.forget_queries();
        let (error, status) = error_of(direct(&ctx, withdraw_request(Some(from.clone()))), "refused");
        assert_eq!(
            serde_json::to_value(&error).unwrap()["error_type"],
            error_type,
            "{from}"
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error = error_of(by_task(&ctx, withdraw_request(Some(from.clone()))), "refused");
        assert_eq!(
            serde_json::to_value(&error).unwrap()["error_type"],
            error_type,
            "{from}"
        );
        assert!(
            chain.sender_queries().is_empty(),
            "{from}: {:?}",
            chain.sender_queries()
        );
    }
    // Nothing was recorded: the address is still unknown, and the platform lists three addresses.
    assert_eq!(known_addresses(&ctx), 3);
    let listed = block_on(coin(&ctx, PLATFORM).account_balance_rpc(account_params())).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(listed.addresses.len(), 3);
}

#[test]
fn evm_hd_p1_a_token_on_a_single_key_platform_refuses_from_with_the_existing_text() {
    let node = chain_node(Arc::new(Chain::default()));
    let ctx = activate(iguana_ctx(), &node, 0);
    let (error, status) = error_of(direct(&ctx, withdraw_request(Some(id_form(1)))), "refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    match &error {
        WithdrawError::UnexpectedFromAddress(text) => assert!(text.contains("single private key"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        by_task(&ctx, withdraw_request(Some(id_form(1)))),
        Err(WithdrawError::UnexpectedFromAddress(_))
    ));
}
