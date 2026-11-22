//! Per-ticker totals, the token-aware scan at activation, the rescan of an account with no known address and a new
//! address whose balance query fails (CRD ch.49 R49.35 to R49.37, T49.33 to T49.35; ch.38 R38.8.5; ch.05 R35a).
//!
//! The platform and its tokens are activated through the public routines against the scripted EVM node of the
//! `evm_hd_p2_*` tests, which records every `balanceOf` query. That shows what each activation asked.

use crate::erc20_token_activation::{enable_evm_token, EnableEvmTokenRequest};
use crate::eth_with_tokens_activation::EthWithTokensActivationRequest;
use crate::evm_hd_p2_tests::*;
use crate::evm_test_support::*;
use crate::init_platform_coin_with_tokens::{init_platform_coin_with_tokens, init_platform_coin_with_tokens_status};
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensReq,
                                       NoXPubExtractor};
use coins::coin_balance::{EnableCoinScanPolicy, HDWalletBalanceOps};
use coins::eth::EthCoin;
use coins::hd_wallet::{HDAddressId, HDWalletRpcOps};
use coins::rpc_command::account_balance::{AccountBalanceParams, AccountBalanceRpcOps};
use coins::rpc_command::init_account_balance::InitAccountBalanceRpcOps;
use coins::DerivationMethod;
use common::mm_number::BigDecimal;
use common::{block_on, HttpStatusCode, StatusCode};
use crypto::Bip44Chain;
use mm2_core::mm_ctx::MmArc;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::{json, Value as Json};
use std::str::FromStr;
use std::sync::Arc;

type Req = EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest>;

fn dec(balance: &Json) -> BigDecimal {
    BigDecimal::from_str(balance["spendable"].as_str().expect("a decimal string")).expect("a decimal")
}

fn request(node: &MockEvmNode, tokens: &[&str], extra: Json) -> Req {
    let tokens: Vec<Json> = tokens.iter().map(|ticker| json!({ "ticker": ticker })).collect();
    let mut request = json!({
        "ticker": PLATFORM,
        "nodes": [{ "url": node.url }],
        "erc20_tokens_requests": tokens,
        "swap_contract_address": SWAP_CONTRACT,
    });
    for (key, value) in extra.as_object().expect("an object") {
        request[key] = value.clone();
    }
    serde_json::from_value(request).unwrap_or_else(|e| panic!("bad request: {e}"))
}

/// The `enable_eth_with_tokens` answer, as the dispatcher serializes it.
fn activate_platform(ctx: &MmArc, node: &MockEvmNode, tokens: &[&str], extra: Json) -> Json {
    let result = block_on(enable_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        request(node, tokens, extra),
    ))
    .unwrap_or_else(|e| panic!("the platform activation failed: {e}"));
    serde_json::to_value(&result).unwrap()
}

/// The `task::enable_eth::init` answer: the details of the finished task.
fn activate_platform_by_task(ctx: &MmArc, node: &MockEvmNode, tokens: &[&str], extra: Json) -> Json {
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        request(node, tokens, extra),
    ))
    .unwrap_or_else(|e| panic!("init failed: {e}"));
    for _ in 0..400 {
        let status = block_on(init_platform_coin_with_tokens_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(
                json!({ "task_id": response.task_id, "forget_if_finished": false }),
            )
            .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {e}"));
        let status = serde_json::to_value(&status).unwrap();
        match status["status"].as_str() {
            Some("Ok") => return status["details"].clone(),
            Some("Error") => panic!("the task failed: {status}"),
            _ => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    panic!("the task did not finish");
}

fn first_account(result: &Json) -> &Json { &result["wallet_balance"]["accounts"][0] }

/// The platform and `tokens` activated, `known_addresses` addresses of account 0 known, and each of the three
/// first addresses holding both assets: the platform at 1, 3 and 5, the token at 1, 2 and 4.
fn t4933_setup() -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    activate_platform(&ctx, &node, &[USDT], json!({}));
    set_known_addresses(&ctx, 3);
    for (index, (native, token)) in [(1u128, 1u128), (3, 2), (5, 4)].into_iter().enumerate() {
        let address = address_at(&ctx, index as u32);
        chain.set_native(&address, native * 1_000_000_000_000_000_000);
        chain.set_token(USDT_CONTRACT, &address, token * 1_000_000);
    }
    (chain, node, ctx)
}

fn page_params(limit: usize, page: usize) -> AccountBalanceParams {
    serde_json::from_value(json!({
        "account_index": 0,
        "chain": "External",
        "limit": limit,
        "paging_options": { "PageNumber": page },
    }))
    .unwrap()
}

fn init_account_params() -> coins::rpc_command::init_account_balance::InitAccountBalanceParams {
    serde_json::from_value(json!({ "account_index": 0 })).unwrap()
}

// ---------------------------------------------------------------------------------------------------------------
// T49.33: the page and account totals are per-ticker maps
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn evm_hd_p3_t4933_the_page_total_sums_each_ticker_over_the_returned_page() {
    let (_chain, _node, ctx) = t4933_setup();
    let platform = coin(&ctx, PLATFORM);
    let expected_tickers = vec![PLATFORM.to_owned(), USDT.to_owned()];

    let first = serde_json::to_value(block_on(platform.account_balance_rpc(page_params(2, 1))).unwrap()).unwrap();
    assert_eq!(first["addresses"].as_array().unwrap().len(), 2);
    assert_eq!(tickers(&first["page_balance"]), expected_tickers);
    assert_eq!(dec(&first["page_balance"][PLATFORM]), BigDecimal::from(4));
    assert_eq!(dec(&first["page_balance"][USDT]), BigDecimal::from(3));

    let second = serde_json::to_value(block_on(platform.account_balance_rpc(page_params(2, 2))).unwrap()).unwrap();
    assert_eq!(second["addresses"].as_array().unwrap().len(), 1);
    assert_eq!(tickers(&second["page_balance"]), expected_tickers);
    assert_eq!(dec(&second["page_balance"][PLATFORM]), BigDecimal::from(5));
    assert_eq!(dec(&second["page_balance"][USDT]), BigDecimal::from(4));
}

/// A page past the known range lists no address. This repository answers the requested ticker at zero where the
/// reference answers `{}` (CRD ch.38 R38.8.5, the recorded divergence of ch.49 R49.35).
#[test]
fn evm_hd_p3_t4933_a_page_past_the_known_range_holds_the_requested_ticker_at_zero() {
    let (_chain, _node, ctx) = t4933_setup();
    for (ticker, key) in [(PLATFORM, PLATFORM), (USDT, USDT)] {
        let answer =
            serde_json::to_value(block_on(coin(&ctx, ticker).account_balance_rpc(page_params(2, 5))).unwrap()).unwrap();
        assert_eq!(answer["addresses"], json!([]), "{ticker}");
        assert_eq!(tickers(&answer["page_balance"]), vec![key.to_owned()], "{ticker}");
        assert_eq!(dec(&answer["page_balance"][key]), BigDecimal::from(0), "{ticker}");
    }
}

#[test]
fn evm_hd_p3_t4933_the_account_total_sums_each_ticker_over_every_address() {
    let (_chain, _node, ctx) = t4933_setup();
    let initiated = block_on(coin(&ctx, PLATFORM).init_account_balance_rpc(init_account_params()))
        .unwrap_or_else(|e| panic!("{e}"));
    let initiated = serde_json::to_value(&initiated).unwrap();
    assert_eq!(tickers(&initiated["total_balance"]), vec![
        PLATFORM.to_owned(),
        USDT.to_owned()
    ]);
    assert_eq!(dec(&initiated["total_balance"][PLATFORM]), BigDecimal::from(9));
    assert_eq!(dec(&initiated["total_balance"][USDT]), BigDecimal::from(7));
}

#[test]
fn evm_hd_p3_t4933_a_token_ticker_answers_a_total_with_the_token_alone() {
    let (_chain, _node, ctx) = t4933_setup();
    let token = coin(&ctx, USDT);
    let page = serde_json::to_value(block_on(token.account_balance_rpc(page_params(2, 1))).unwrap()).unwrap();
    assert_eq!(tickers(&page["page_balance"]), vec![USDT.to_owned()]);
    assert_eq!(dec(&page["page_balance"][USDT]), BigDecimal::from(3));
    let initiated = block_on(token.init_account_balance_rpc(init_account_params())).unwrap_or_else(|e| panic!("{e}"));
    let initiated = serde_json::to_value(&initiated).unwrap();
    assert_eq!(tickers(&initiated["total_balance"]), vec![USDT.to_owned()]);
    assert_eq!(dec(&initiated["total_balance"][USDT]), BigDecimal::from(7));
}

/// An account with no known address answers the requested ticker at zero (CRD ch.38 R38.8.5).
#[test]
fn evm_hd_p3_t4933_an_account_with_no_known_address_totals_the_requested_ticker_at_zero() {
    let (_chain, _node, ctx) = t4933_setup();
    set_known_addresses(&ctx, 0);
    for ticker in [PLATFORM, USDT] {
        let initiated = block_on(coin(&ctx, ticker).init_account_balance_rpc(init_account_params()))
            .unwrap_or_else(|e| panic!("{e}"));
        let initiated = serde_json::to_value(&initiated).unwrap();
        assert_eq!(initiated["addresses"], json!([]), "{ticker}");
        assert_eq!(
            tickers(&initiated["total_balance"]),
            vec![ticker.to_owned()],
            "{ticker}"
        );
        assert_eq!(
            dec(&initiated["total_balance"][ticker]),
            BigDecimal::from(0),
            "{ticker}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------
// T49.34: the scan at activation takes the tokens of the request into account
// ---------------------------------------------------------------------------------------------------------------

/// The scripted chain of T49.34: only address 2 of account 0 is used, by a token balance alone.
fn t4934_chain(ctx_for_addresses: &MmArc) -> Arc<Chain> {
    let chain = Arc::new(Chain::default());
    chain.set_token(USDT_CONTRACT, &address_at(ctx_for_addresses, 2), 7_000_000);
    chain
}

/// The addresses of the test mnemonic do not depend on the node, so a scratch context derives them before the real
/// activation runs.
fn scratch_chain() -> Arc<Chain> {
    let scratch = hd_ctx();
    let node = chain_node(Arc::new(Chain::default()));
    activate_platform(&scratch, &node, &[], json!({ "scan_policy": "do_not_scan" }));
    t4934_chain(&scratch)
}

fn check_the_activation_report_of_t4934(result: &Json, ctx: &MmArc, chain: &Chain) {
    let account = first_account(result);
    let addresses = account["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 3, "{result}");
    for (index, entry) in addresses.iter().enumerate() {
        assert_eq!(
            entry["address"].as_str().unwrap().to_lowercase(),
            address_at(ctx, index as u32)
        );
    }
    assert_eq!(addresses[0]["balance"], json!({}), "passed over as unused");
    assert_eq!(addresses[1]["balance"], json!({}), "passed over as unused");
    assert_eq!(tickers(&addresses[2]["balance"]), vec![
        PLATFORM.to_owned(),
        USDT.to_owned()
    ]);
    assert_eq!(dec(&addresses[2]["balance"][USDT]), BigDecimal::from(7));
    assert_eq!(dec(&addresses[2]["balance"][PLATFORM]), BigDecimal::from(0));
    assert_eq!(tickers(&account["total_balance"]), vec![
        PLATFORM.to_owned(),
        USDT.to_owned()
    ]);
    assert_eq!(dec(&account["total_balance"][USDT]), BigDecimal::from(7));
    // The address found by its token is asked once, not once by the scan and once by the report.
    assert_eq!(
        chain.token_queries(USDT_CONTRACT, &address_at(ctx, 2)),
        1,
        "{:?}",
        chain.queries()
    );

    // The passed-over entries are known from now on, and a later `account_balance` shows them with zero values.
    assert_eq!(known_addresses(ctx), 3);
    let listed = block_on(coin(ctx, PLATFORM).account_balance_rpc(account_params())).unwrap();
    let listed = serde_json::to_value(&listed).unwrap();
    let listed_addresses = listed["addresses"].as_array().unwrap();
    assert_eq!(listed_addresses.len(), 3);
    for entry in listed_addresses {
        assert_eq!(tickers(&entry["balance"]), vec![PLATFORM.to_owned(), USDT.to_owned()]);
    }
    assert_eq!(dec(&listed_addresses[0]["balance"][USDT]), BigDecimal::from(0));
    assert_eq!(dec(&listed_addresses[1]["balance"][PLATFORM]), BigDecimal::from(0));
}

#[test]
fn evm_hd_p3_t4934_the_activation_scan_finds_a_token_only_address_and_lists_the_passed_over_ones_empty() {
    let chain = scratch_chain();
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    let result = activate_platform(&ctx, &node, &[USDT], json!({}));
    check_the_activation_report_of_t4934(&result, &ctx, &chain);
}

#[test]
fn evm_hd_p3_t4934_the_task_route_runs_the_same_scan() {
    let chain = scratch_chain();
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    let details = activate_platform_by_task(&ctx, &node, &[USDT], json!({}));
    check_the_activation_report_of_t4934(&details, &ctx, &chain);
}

#[test]
fn evm_hd_p3_t4934_do_not_scan_on_fresh_storage_lists_address_zero_only() {
    let chain = scratch_chain();
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    let result = activate_platform(&ctx, &node, &[USDT], json!({ "scan_policy": "do_not_scan" }));
    let addresses = first_account(&result)["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 1, "{result}");
    assert_eq!(
        addresses[0]["address"].as_str().unwrap().to_lowercase(),
        address_at(&ctx, 0)
    );
    assert_eq!(tickers(&addresses[0]["balance"]), vec![
        PLATFORM.to_owned(),
        USDT.to_owned()
    ]);
    assert_eq!(known_addresses(&ctx), 1);
    assert!(
        chain.token_queries(USDT_CONTRACT, &address_at(&ctx, 2)) == 0,
        "no address is probed: {:?}",
        chain.queries()
    );
}

#[test]
fn evm_hd_p3_t4934_a_failing_token_query_fails_the_activation_and_records_no_address() {
    let chain = scratch_chain();
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    chain.fail_token(USDT_CONTRACT);
    let failed = error_of(
        block_on(enable_platform_coin_with_tokens::<EthCoin>(
            ctx.clone(),
            request(&node, &[USDT], json!({})),
        )),
        "the activation must fail",
    );
    assert_eq!(
        serde_json::to_value(failed.get_inner()).unwrap()["error_type"],
        FAILED_KIND
    );
    assert!(
        block_on(coins::lp_coinfind(&ctx, PLATFORM)).unwrap().is_none(),
        "a failed activation registers nothing"
    );

    // What the failed call stored holds no address: the next activation under the default policy scans the account
    // from its start (the account has no known address, so it is scanned as a new one would be) and finds all three.
    chain.heal();
    let result = activate_platform(&ctx, &node, &[USDT], json!({}));
    let addresses = first_account(&result)["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 3, "{result}");
    assert_eq!(addresses[0]["balance"], json!({}));
    assert_eq!(addresses[1]["balance"], json!({}));
    assert_eq!(dec(&addresses[2]["balance"][USDT]), BigDecimal::from(7));
    assert_eq!(known_addresses(&ctx), 3);
}

/// Each token's `balanceOf` is asked once per address the report queries: the address the scan found by its nonce,
/// the one it probed and passed over, and the ones generated to reach the minimum count.
#[test]
fn evm_hd_p3_t4934_each_token_is_asked_once_per_listed_queried_address() {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let scratch = hd_ctx();
    activate_platform(&scratch, &node, &[], json!({ "scan_policy": "do_not_scan" }));
    chain.set_nonce(&address_at(&scratch, 1), 3);

    let ctx = hd_ctx();
    let result = activate_platform(&ctx, &node, &[USDT, DAI], json!({ "min_addresses_number": 4 }));
    let addresses = first_account(&result)["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 4, "{result}");
    assert_eq!(
        addresses[0]["balance"],
        json!({}),
        "address 0 was passed over by the scan"
    );
    for entry in &addresses[1..] {
        assert_eq!(tickers(&entry["balance"]), all_tickers(), "{entry}");
    }
    for index in 0..4 {
        for contract in [USDT_CONTRACT, DAI_CONTRACT] {
            assert_eq!(
                chain.token_queries(contract, &address_at(&ctx, index)),
                1,
                "{contract} at address {index}: {:?}",
                chain.queries()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The rescan of an account with no known address (ch.05 R35a; the recorded divergence of ch.49 R49.36 item 7)
// ---------------------------------------------------------------------------------------------------------------

fn enable_again(ctx: &MmArc, policy: EnableCoinScanPolicy) -> Json {
    let platform = coin(ctx, PLATFORM);
    let DerivationMethod::HDWallet(hd_wallet) = &platform.derivation_method else {
        panic!("an HD platform expected");
    };
    let report = block_on(platform.enable_hd_wallet(
        hd_wallet,
        None::<&NoXPubExtractor>,
        policy,
        0,
        Some(HDAddressId {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 0,
        }),
    ))
    .unwrap_or_else(|e| panic!("the activation failed: {e}"));
    serde_json::to_value(&report).unwrap()
}

#[test]
fn evm_hd_p3_a_stored_account_with_no_known_address_is_scanned_under_the_default_policy() {
    let (chain, _node, ctx) = t4931_like_setup();
    chain.set_token(USDT_CONTRACT, &address_at(&ctx, 2), 7_000_000);
    set_known_addresses(&ctx, 0);

    let report = enable_again(&ctx, EnableCoinScanPolicy::ScanIfNewWallet);
    let addresses = report["accounts"][0]["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 3, "{report}");
    assert_eq!(addresses[0]["balance"], json!({}));
    assert_eq!(addresses[1]["balance"], json!({}));
    assert_eq!(dec(&addresses[2]["balance"][USDT]), BigDecimal::from(7));
    assert_eq!(known_addresses(&ctx), 3);
}

#[test]
fn evm_hd_p3_an_account_with_known_addresses_is_not_rescanned_under_the_default_policy() {
    let (chain, _node, ctx) = t4931_like_setup();
    chain.set_token(USDT_CONTRACT, &address_at(&ctx, 2), 7_000_000);
    set_known_addresses(&ctx, 1);

    let report = enable_again(&ctx, EnableCoinScanPolicy::ScanIfNewWallet);
    let addresses = report["accounts"][0]["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 1, "{report}");
    assert_eq!(tickers(&addresses[0]["balance"]), all_tickers());
    assert_eq!(known_addresses(&ctx), 1);

    // `scan` still scans a stored account; `do_not_scan` never does, whether or not the account knows an address.
    set_known_addresses(&ctx, 0);
    let report = enable_again(&ctx, EnableCoinScanPolicy::DoNotScan);
    assert_eq!(
        report["accounts"][0]["addresses"].as_array().unwrap().len(),
        1,
        "{report}"
    );
    assert_eq!(known_addresses(&ctx), 1);
    let report = enable_again(&ctx, EnableCoinScanPolicy::Scan);
    assert_eq!(
        report["accounts"][0]["addresses"].as_array().unwrap().len(),
        3,
        "{report}"
    );
}

/// The platform and both tokens activated with three known addresses (the setup of `evm_hd_p2_*`).
fn t4931_like_setup() -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    activate_platform(&ctx, &node, &[], json!({}));
    set_known_addresses(&ctx, 3);
    for ticker in [USDT, DAI] {
        let token: EnableEvmTokenRequest =
            serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).unwrap();
        block_on(enable_evm_token(ctx.clone(), token)).unwrap_or_else(|e| panic!("the token activation failed: {e}"));
    }
    (chain, node, ctx)
}

// ---------------------------------------------------------------------------------------------------------------
// T49.35: a new address whose balance query fails stays known (no rollback)
// ---------------------------------------------------------------------------------------------------------------

fn t4935_setup() -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    activate_platform(&ctx, &node, &[USDT], json!({ "scan_policy": "do_not_scan" }));
    assert_eq!(known_addresses(&ctx), 1);
    (chain, node, ctx)
}

fn check_the_failed_get_new_address(ctx: &MmArc, chain: &Chain) {
    let failed = error_of(
        block_on(coin(ctx, PLATFORM).get_new_address_rpc(get_new_address_params())),
        "get_new_address must fail",
    );
    assert_eq!(failed.get_inner().status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    let wire = serde_json::to_value(failed.get_inner()).unwrap();
    assert_eq!(wire["error_type"], FAILED_KIND, "{wire}");
    assert!(wire.get("new_address").is_none(), "no address in the answer: {wire}");

    // The raised count stays: the address is known, and the node being healthy again the next one is index 2.
    chain.heal();
    let listed = block_on(coin(ctx, PLATFORM).account_balance_rpc(account_params())).unwrap();
    assert_eq!(serde_json::to_value(&listed).unwrap()["total"], 2);
    let next =
        block_on(coin(ctx, PLATFORM).get_new_address_rpc(get_new_address_params())).unwrap_or_else(|e| panic!("{e}"));
    let next = serde_json::to_value(&next).unwrap();
    assert_eq!(
        next["new_address"]["address"].as_str().unwrap().to_lowercase(),
        address_at(ctx, 2)
    );
}

#[test]
fn evm_hd_p3_t4935_a_failing_token_query_fails_get_new_address_and_the_address_stays_known() {
    let (chain, _node, ctx) = t4935_setup();
    chain.fail_token(USDT_CONTRACT);
    check_the_failed_get_new_address(&ctx, &chain);
}

#[test]
fn evm_hd_p3_t4935_a_failing_platform_query_fails_get_new_address_and_the_address_stays_known() {
    let (chain, _node, ctx) = t4935_setup();
    chain.fail_native();
    check_the_failed_get_new_address(&ctx, &chain);
}
