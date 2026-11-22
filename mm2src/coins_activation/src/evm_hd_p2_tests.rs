//! Balance maps of the HD address methods asked for the platform ticker of an EVM platform (CRD ch.49 R49.32 items
//! 1 to 3, R49.33, T49.31; ch.35 R35.1.3(b)).
//!
//! The platform and its tokens are activated through the public routines against a scripted EVM node that answers per
//! address and per token contract. The node records every balance and nonce query, which shows what each request asked.

use crate::erc20_token_activation::{enable_evm_token, EnableEvmTokenRequest};
use crate::eth_with_tokens_activation::EthWithTokensActivationRequest;
use crate::evm_test_support::*;
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensReq};
use coins::eth::{checksum_address, EthCoin};
use coins::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletRpcOps};
use coins::rpc_command::account_balance::AccountBalanceRpcOps;
use coins::rpc_command::init_account_balance::InitAccountBalanceRpcOps;
use coins::rpc_command::init_scan_for_new_addresses::InitScanAddressesRpcOps;
use coins::{lp_coinfind, DerivationMethod, MmCoinEnum};
use common::block_on;
use crypto::Bip44Chain;
use mm2_core::mm_ctx::MmArc;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) const USDT: &str = "USDT-ERC20";
pub(crate) const DAI: &str = "DAI-ERC20";

/// What the scripted chain holds per address, and every balance and nonce query it has answered.
#[derive(Default)]
pub(crate) struct Chain {
    native: Mutex<HashMap<String, u128>>,
    nonces: Mutex<HashMap<String, u64>>,
    /// The token balances by contract and holder, both in lower case.
    tokens: Mutex<HashMap<(String, String), u128>>,
    /// The contract whose `balanceOf` the node refuses.
    failing_contract: Mutex<Option<String>>,
    /// Whether the node refuses `eth_getBalance`.
    failing_native: std::sync::atomic::AtomicBool,
    queries: Mutex<Vec<String>>,
}

impl Chain {
    pub(crate) fn set_token(&self, contract: &str, address: &str, value: u128) {
        self.tokens
            .lock()
            .unwrap()
            .insert((contract.to_lowercase(), address.to_lowercase()), value);
    }
    pub(crate) fn set_nonce(&self, address: &str, value: u64) {
        self.nonces.lock().unwrap().insert(address.to_lowercase(), value);
    }
    pub(crate) fn set_native(&self, address: &str, value: u128) {
        self.native.lock().unwrap().insert(address.to_lowercase(), value);
    }
    /// Makes the node answer again: the failing contract and the failing platform balance are cleared.
    pub(crate) fn heal(&self) {
        *self.failing_contract.lock().unwrap() = None;
        self.failing_native.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) fn fail_native(&self) { self.failing_native.store(true, std::sync::atomic::Ordering::SeqCst); }
    pub(crate) fn fail_token(&self, contract: &str) {
        *self.failing_contract.lock().unwrap() = Some(contract.to_lowercase());
    }
    pub(crate) fn forget_queries(&self) { self.queries.lock().unwrap().clear(); }
    pub(crate) fn queries(&self) -> Vec<String> { self.queries.lock().unwrap().clone() }
    /// How many `balanceOf` queries of `contract` the node answered for `address`.
    pub(crate) fn token_queries(&self, contract: &str, address: &str) -> usize {
        let wanted = format!("token {} {}", contract.to_lowercase(), address.to_lowercase());
        self.queries().iter().filter(|q| **q == wanted).count()
    }
    /// Whether any `balanceOf` of `contract` was asked.
    pub(crate) fn asked_contract(&self, contract: &str) -> bool {
        let prefix = format!("token {} ", contract.to_lowercase());
        self.queries().iter().any(|q| q.starts_with(&prefix))
    }
}

pub(crate) fn chain_node(chain: Arc<Chain>) -> MockEvmNode {
    spawn_node(move |method, params| {
        let lower = |value: &Json| value.as_str().unwrap_or_default().to_lowercase();
        match method {
            "eth_getBalance" => {
                if chain.failing_native.load(std::sync::atomic::Ordering::SeqCst) {
                    return Reply::RpcError;
                }
                let who = lower(&params[0]);
                let value = chain.native.lock().unwrap().get(&who).copied().unwrap_or(0);
                Reply::Result(json!(format!("0x{value:x}")))
            },
            "eth_getTransactionCount" => {
                let who = lower(&params[0]);
                let value = chain.nonces.lock().unwrap().get(&who).copied().unwrap_or(0);
                Reply::Result(json!(format!("0x{value:x}")))
            },
            "eth_call" => {
                let data = params[0]["data"]
                    .as_str()
                    .or_else(|| params[0]["input"].as_str())
                    .unwrap_or_default();
                if data.starts_with("0x70a08231") && data.len() >= 74 {
                    let who = format!("0x{}", &data[data.len() - 40..]).to_lowercase();
                    let contract = lower(&params[0]["to"]);
                    if chain.failing_contract.lock().unwrap().as_deref() == Some(contract.as_str()) {
                        return Reply::RpcError;
                    }
                    let value = chain
                        .tokens
                        .lock()
                        .unwrap()
                        .get(&(contract.clone(), who.clone()))
                        .copied()
                        .unwrap_or(0);
                    chain.queries.lock().unwrap().push(format!("token {contract} {who}"));
                    return Reply::Result(json!(abi_word(value)));
                }
                healthy_node_reply(method, params, 0, 0)
            },
            _ => healthy_node_reply(method, params, 0, 0),
        }
    })
}

pub(crate) fn platform_request(
    node: &MockEvmNode,
    tokens: &[&str],
) -> EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest> {
    let tokens: Vec<Json> = tokens.iter().map(|ticker| json!({ "ticker": ticker })).collect();
    serde_json::from_value(json!({
        "ticker": PLATFORM,
        "nodes": [{ "url": node.url }],
        "erc20_tokens_requests": tokens,
        "swap_contract_address": SWAP_CONTRACT,
    }))
    .unwrap()
}

pub(crate) fn set_known_addresses(ctx: &MmArc, known_addresses: u32) {
    if let DerivationMethod::HDWallet(hd_wallet) = &coin(ctx, PLATFORM).derivation_method {
        let mut accounts = block_on(hd_wallet.accounts.lock());
        accounts.get_mut(&0).expect("account 0").external_addresses_number = known_addresses;
    }
}

/// Activates the platform, makes `known_addresses` external addresses of account 0 known, and then activates both
/// tokens through `enable_erc20`.
fn activate(node: &MockEvmNode, known_addresses: u32) -> MmArc {
    let ctx = hd_ctx();
    block_on(enable_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        platform_request(node, &[]),
    ))
    .unwrap_or_else(|e| panic!("the platform activation failed: {}", e));
    set_known_addresses(&ctx, known_addresses);
    for ticker in [USDT, DAI] {
        let token: EnableEvmTokenRequest =
            serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).unwrap();
        block_on(enable_evm_token(ctx.clone(), token)).unwrap_or_else(|e| panic!("the token activation failed: {}", e));
    }
    ctx
}

pub(crate) fn coin(ctx: &MmArc, ticker: &str) -> EthCoin {
    match block_on(lp_coinfind(ctx, ticker)).unwrap().expect("the coin is active") {
        MmCoinEnum::EthCoin(coin) => coin,
        other => panic!("not an EVM coin: {}", other.ticker()),
    }
}

/// The address `index` of account 0, in lower case.
pub(crate) fn address_at(ctx: &MmArc, index: u32) -> String {
    let platform = coin(ctx, PLATFORM);
    let DerivationMethod::HDWallet(hd_wallet) = &platform.derivation_method else {
        panic!("an HD platform expected");
    };
    let accounts = block_on(hd_wallet.accounts.lock());
    let derived = platform
        .derive_address(accounts.get(&0).expect("account 0"), Bip44Chain::External, index)
        .unwrap_or_else(|e| panic!("derive: {}", e));
    format!("{:#x}", derived.address)
}

pub(crate) fn known_addresses(ctx: &MmArc) -> u32 {
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

/// The tickers of a balance map in the wire form.
pub(crate) fn tickers(balance: &Json) -> Vec<String> {
    let mut tickers: Vec<String> = balance.as_object().expect("a balance map").keys().cloned().collect();
    tickers.sort();
    tickers
}

/// The error kind of a failed token balance query: the node answers a JSON-RPC error.
pub(crate) const FAILED_KIND: &str = "Transport";

pub(crate) mod erased {
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

pub(crate) fn all_tickers() -> Vec<String> { vec![DAI.to_owned(), PLATFORM.to_owned(), USDT.to_owned()] }

pub(crate) fn get_new_address_params() -> coins::hd_wallet::GetNewHDAddressParams {
    serde_json::from_value(json!({ "account_id": 0, "chain": "External" })).unwrap()
}

pub(crate) fn account_params() -> coins::rpc_command::account_balance::AccountBalanceParams {
    serde_json::from_value(json!({ "account_index": 0, "chain": "External", "limit": 20 })).unwrap()
}

pub(crate) fn scan_params(gap_limit: u32) -> coins::rpc_command::init_scan_for_new_addresses::ScanAddressesParams {
    serde_json::from_value(json!({ "account_index": 0, "gap_limit": gap_limit })).unwrap()
}

/// The setup of T49.31: three addresses known, and the USDT token holding a balance only at address 2.
fn t4931_setup() -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = activate(&node, 3);
    chain.set_token(USDT_CONTRACT, &address_at(&ctx, 2), 7_000_000);
    (chain, node, ctx)
}

#[test]
fn evm_hd_p2_get_new_address_for_the_platform_answers_the_platform_and_every_token() {
    let (chain, _node, ctx) = t4931_setup();
    let fourth = address_at(&ctx, 3);
    chain.set_token(DAI_CONTRACT, &fourth, 3_000_000_000_000_000_000);
    let response =
        block_on(coin(&ctx, PLATFORM).get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    let address = answer["new_address"]["address"].as_str().unwrap();
    assert_eq!(address, checksum_address(&address.to_lowercase()), "EIP-55 form");
    assert_eq!(address.to_lowercase(), fourth);
    assert_eq!(tickers(&answer["new_address"]["balance"]), all_tickers());
    assert_eq!(answer["new_address"]["balance"][DAI]["spendable"], "3");
    assert_eq!(answer["new_address"]["balance"][USDT]["spendable"], "0");
}

#[test]
fn evm_hd_p2_account_balance_and_its_task_form_list_every_ticker_at_every_address() {
    let (_chain, _node, ctx) = t4931_setup();
    let platform = coin(&ctx, PLATFORM);

    let answer = serde_json::to_value(block_on(platform.account_balance_rpc(account_params())).unwrap()).unwrap();
    let addresses = answer["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 3);
    for entry in addresses {
        assert_eq!(tickers(&entry["balance"]), all_tickers());
    }
    assert_eq!(addresses[2]["balance"][USDT]["spendable"], "7");
    assert_eq!(addresses[1]["balance"][USDT]["spendable"], "0");
    // The page total is a map that sums each ticker over the page (CRD ch.49 R49.35 item 1).
    assert_eq!(tickers(&answer["page_balance"]), all_tickers());
    assert_eq!(answer["page_balance"][USDT]["spendable"], "7");
    assert_eq!(answer["page_balance"][DAI]["spendable"], "0");

    let initiated =
        block_on(platform.init_account_balance_rpc(serde_json::from_value(json!({ "account_index": 0 })).unwrap()))
            .unwrap_or_else(|e| panic!("{e}"));
    let initiated = serde_json::to_value(&initiated).unwrap();
    for entry in initiated["addresses"].as_array().unwrap() {
        assert_eq!(tickers(&entry["balance"]), all_tickers());
    }
    assert_eq!(tickers(&initiated["total_balance"]), all_tickers());
    assert_eq!(initiated["total_balance"][USDT]["spendable"], "7");
    assert_eq!(initiated["total_balance"][DAI]["spendable"], "0");
}

#[test]
fn evm_hd_p2_the_scan_lists_intervening_addresses_with_an_empty_map_and_finds_a_token_only_address() {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = activate(&node, 1);
    // Address 2 holds only a token: nonce 0 and no native balance.
    let token_only = address_at(&ctx, 2);
    chain.set_token(USDT_CONTRACT, &token_only, 7_000_000);

    let response = block_on(coin(&ctx, PLATFORM).init_scan_for_new_addresses_rpc(scan_params(2)))
        .unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    let found = answer["new_addresses"].as_array().unwrap();
    assert_eq!(found.len(), 2, "{answer}");
    assert_eq!(
        found[0]["address"].as_str().unwrap().to_lowercase(),
        address_at(&ctx, 1)
    );
    assert_eq!(
        found[0]["balance"],
        json!({}),
        "the unused address between carries an empty map"
    );
    assert_eq!(found[1]["address"].as_str().unwrap().to_lowercase(), token_only);
    assert_eq!(tickers(&found[1]["balance"]), all_tickers());
    assert_eq!(found[1]["balance"][USDT]["spendable"], "7");
    assert_eq!(known_addresses(&ctx), 3);
}

#[test]
fn evm_hd_p2_the_scan_counts_an_address_used_by_its_nonce_or_platform_balance_with_the_full_map() {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = activate(&node, 1);
    chain.set_nonce(&address_at(&ctx, 1), 1);
    let response = block_on(coin(&ctx, PLATFORM).init_scan_for_new_addresses_rpc(scan_params(2)))
        .unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    let found = answer["new_addresses"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{answer}");
    assert_eq!(tickers(&found[0]["balance"]), all_tickers());
}

#[test]
fn evm_hd_p2_a_token_removed_by_disable_coin_leaves_the_maps() {
    let (_chain, _node, ctx) = t4931_setup();
    block_on(coins::disable_coin(&ctx, DAI)).expect("the token is disabled");
    let answer = block_on(coin(&ctx, PLATFORM).account_balance_rpc(account_params())).unwrap();
    let answer = serde_json::to_value(&answer).unwrap();
    for entry in answer["addresses"].as_array().unwrap() {
        assert_eq!(tickers(&entry["balance"]), vec![PLATFORM.to_owned(), USDT.to_owned()]);
    }
}

#[test]
fn evm_hd_p2_a_token_ticker_answers_that_token_only_and_the_platform_counts_the_new_address() {
    let (chain, _node, ctx) = t4931_setup();
    chain.forget_queries();
    let response =
        block_on(coin(&ctx, USDT).get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&response).unwrap();
    assert_eq!(tickers(&answer["new_address"]["balance"]), vec![USDT.to_owned()]);
    assert!(
        !chain.asked_contract(DAI_CONTRACT),
        "a token ticker asks no other token: {:?}",
        chain.queries()
    );

    let listed = block_on(coin(&ctx, PLATFORM).account_balance_rpc(account_params())).unwrap();
    let listed = serde_json::to_value(&listed).unwrap();
    let addresses = listed["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 4);
    assert_eq!(
        addresses[3]["address"].as_str().unwrap().to_lowercase(),
        address_at(&ctx, 3)
    );
}

#[test]
fn evm_hd_p2_a_failing_token_balance_fails_the_platform_ticker_request_as_a_whole() {
    let (chain, _node, ctx) = t4931_setup();
    chain.fail_token(DAI_CONTRACT);
    let platform = coin(&ctx, PLATFORM);
    let kind = |error: &dyn erased::Wire| error.json()["error_type"].clone();
    let failed = error_of(block_on(platform.account_balance_rpc(account_params())), "failed");
    assert_eq!(kind(&failed), FAILED_KIND, "account_balance");
    let failed = error_of(
        block_on(platform.init_account_balance_rpc(serde_json::from_value(json!({ "account_index": 0 })).unwrap())),
        "failed",
    );
    assert_eq!(kind(&failed), FAILED_KIND, "task::account_balance::init");
    // The new address is stored before any balance is asked, so it stays known after the failure.
    assert_eq!(known_addresses(&ctx), 3);
    let failed = error_of(
        block_on(platform.get_new_address_rpc(get_new_address_params())),
        "failed",
    );
    assert_eq!(kind(&failed), FAILED_KIND, "get_new_address");
    assert_eq!(
        known_addresses(&ctx),
        4,
        "the address stays known although the request failed"
    );
    chain.set_nonce(&address_at(&ctx, 4), 1);
    let failed = error_of(
        block_on(platform.init_scan_for_new_addresses_rpc(scan_params(2))),
        "failed",
    );
    assert_eq!(kind(&failed), FAILED_KIND, "task::scan_for_new_addresses::init");
}

#[test]
fn evm_hd_p2_a_token_ticker_asks_no_other_tokens_balance() {
    let (chain, _node, ctx) = t4931_setup();
    chain.fail_token(DAI_CONTRACT);
    chain.set_nonce(&address_at(&ctx, 3), 1);
    chain.forget_queries();
    let token = coin(&ctx, USDT);

    let answer = block_on(token.account_balance_rpc(account_params())).unwrap_or_else(|e| panic!("{e}"));
    let answer = serde_json::to_value(&answer).unwrap();
    for entry in answer["addresses"].as_array().unwrap() {
        assert_eq!(tickers(&entry["balance"]), vec![USDT.to_owned()]);
    }
    assert_eq!(tickers(&answer["page_balance"]), vec![USDT.to_owned()]);
    assert_eq!(answer["page_balance"][USDT]["spendable"], "7");
    let initiated =
        block_on(token.init_account_balance_rpc(serde_json::from_value(json!({ "account_index": 0 })).unwrap()))
            .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        tickers(&serde_json::to_value(&initiated).unwrap()["total_balance"]),
        vec![USDT.to_owned()]
    );
    let scanned = block_on(token.init_scan_for_new_addresses_rpc(scan_params(2))).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(scanned.new_addresses.len(), 1);
    assert_eq!(
        tickers(&serde_json::to_value(&scanned.new_addresses[0].balance).unwrap()),
        vec![USDT.to_owned()]
    );
    assert!(
        !chain.asked_contract(DAI_CONTRACT),
        "no other token is asked: {:?}",
        chain.queries()
    );
}

#[test]
fn evm_hd_p2_the_activation_report_keeps_the_platform_and_every_token_and_asks_each_token_once_per_address() {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    chain.set_token(USDT_CONTRACT, ACTIVATED_ADDRESS, 5_000_000);
    let result = block_on(enable_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        platform_request(&node, &[USDT, DAI]),
    ))
    .unwrap_or_else(|e| panic!("the platform activation failed: {}", e));
    let result = serde_json::to_value(&result).unwrap();
    let accounts = result["wallet_balance"]["accounts"].as_array().expect("an HD report");
    let mut reported = 0;
    for account in accounts {
        assert_eq!(tickers(&account["total_balance"]), all_tickers());
        for entry in account["addresses"].as_array().unwrap() {
            assert_eq!(tickers(&entry["balance"]), all_tickers(), "{entry}");
            let address = entry["address"].as_str().unwrap();
            for contract in [USDT_CONTRACT, DAI_CONTRACT] {
                assert_eq!(
                    chain.token_queries(contract, address),
                    1,
                    "{contract} asked once at {address}: {:?}",
                    chain.queries()
                );
            }
            if address.to_lowercase() == ACTIVATED_ADDRESS {
                assert_eq!(entry["balance"][USDT]["spendable"], "5");
            }
            reported += 1;
        }
    }
    assert!(reported > 0, "the report lists an address");
}

/// The activated address of the HD test context: address 0 of account 0 of the test mnemonic at `m/44'/60'/0'/0/0`.
pub(crate) const ACTIVATED_ADDRESS: &str = "0x9858effd232b4033e47d90003d41ec34ecaeda94";
