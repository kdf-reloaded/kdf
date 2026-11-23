//! `task::get_new_address` for an EVM platform and an ERC-20 token under the software policy (CRD ch.49 §49.2.3,
//! R49.38 -- R49.41; the software case of ch.05 R54; T49.36 as the 2026-11-22 status note rewrites it).
//!
//! The platform and its token are activated through the public routines against a scripted EVM node, then the task
//! is started, polled and read the way a client does.

use crate::evm_hd_p2_tests::*;
use crate::evm_test_support::*;
use crate::platform_coin_with_tokens::enable_platform_coin_with_tokens;
use coins::eth::{checksum_address, EthCoin};
use coins::hd_wallet::HDWalletRpcError;
use coins::rpc_command::account_balance::AccountBalanceRpcOps;
use coins::rpc_command::init_get_new_address::{init_get_new_address, init_get_new_address_status};
use common::{block_on, HttpStatusCode, StatusCode};
use crypto::CryptoCtx;
use mm2_core::mm_ctx::MmArc;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::{json, Value as Json};
use std::str::FromStr;
use std::sync::Arc;

/// The platform and the USDT token activated, with the known addresses `known_addresses` (address 0 is always known).
fn setup(known_addresses: u32) -> (Arc<Chain>, MockEvmNode, MmArc) {
    let chain = Arc::new(Chain::default());
    let node = chain_node(chain.clone());
    let ctx = hd_ctx();
    block_on(enable_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        platform_request(&node, &[USDT]),
    ))
    .unwrap_or_else(|e| panic!("the platform activation failed: {e}"));
    set_known_addresses(&ctx, known_addresses);
    (chain, node, ctx)
}

/// Starts `task::get_new_address::init` with `request` and polls it to its end. Returns the final status and the
/// in-progress values seen on the way.
fn run_task(ctx: &MmArc, request: Json) -> (Json, Vec<Json>) {
    let request = serde_json::from_value(request).expect("a task request");
    let response = block_on(init_get_new_address(ctx.clone(), request))
        .unwrap_or_else(|e| panic!("the task must start: {}", e.get_inner()));
    let mut seen = Vec::new();
    for _ in 0..4000 {
        let status = block_on(init_get_new_address_status(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(
                json!({ "task_id": response.task_id, "forget_if_finished": false }),
            )
            .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {e}"));
        let status = serde_json::to_value(&status).unwrap();
        match status["status"].as_str() {
            Some("Ok") | Some("Error") => return (status, seen),
            Some("InProgress") => {
                if !seen.contains(&status["details"]) {
                    seen.push(status["details"].clone());
                }
            },
            other => panic!("unexpected status {other:?}: {status}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the task did not finish");
}

fn task_request(ticker: &str) -> Json { json!({ "coin": ticker, "account_id": 0 }) }

fn total(ctx: &MmArc) -> Json {
    let listed = block_on(coin(ctx, PLATFORM).account_balance_rpc(account_params())).unwrap();
    serde_json::to_value(&listed).unwrap()["total"].clone()
}

fn assert_only_software_statuses(seen: &[Json]) {
    for status in seen {
        assert!(
            *status == json!("Preparing") || *status == json!("RequestingAccountBalance"),
            "a software-policy task shows no other state, saw {status}"
        );
    }
}

/// T49.36 (the divergence): with a software HD login and no hardware context, the task for an EVM platform and for an
/// ERC-20 token reaches `Ok` with the result shape of the direct method, shows only the software statuses, and the
/// total of `account_balance` rises by one each time.
#[test]
fn evm_hd_p4_t4936_the_software_task_completes_for_a_platform_and_a_token() {
    let (_chain, _node, ctx) = setup(1);
    assert!(CryptoCtx::from_ctx(&ctx).unwrap().hw_ctx().is_none());
    assert_eq!(total(&ctx), 1);

    let (status, seen) = run_task(&ctx, task_request(PLATFORM));
    assert_eq!(status["status"], "Ok", "{status}");
    assert_only_software_statuses(&seen);
    let new_address = &status["details"]["new_address"];
    let expected = address_at(&ctx, 1);
    assert_eq!(
        new_address["address"],
        checksum_address(&expected).as_str(),
        "EIP-55 form"
    );
    assert_eq!(new_address["derivation_path"], "m/44'/60'/0'/0/1");
    assert_eq!(new_address["chain"], "External");
    assert_eq!(tickers(&new_address["balance"]), [PLATFORM, USDT]);
    assert!(
        status["details"].as_object().unwrap().len() == 1,
        "no wrapper: {status}"
    );
    assert_eq!(total(&ctx), 2);

    let (status, seen) = run_task(&ctx, task_request(USDT));
    assert_eq!(status["status"], "Ok", "{status}");
    assert_only_software_statuses(&seen);
    let new_address = &status["details"]["new_address"];
    assert_eq!(new_address["address"], checksum_address(&address_at(&ctx, 2)).as_str());
    assert_eq!(new_address["derivation_path"], "m/44'/60'/0'/0/2");
    assert_eq!(tickers(&new_address["balance"]), [USDT], "the token's ticker only");
    assert_eq!(total(&ctx), 3);
}

/// T49.36: a ticker that is not activated answers `NoSuchCoin` from init itself (HTTP 400).
#[test]
fn evm_hd_p4_t4936_a_ticker_that_is_not_activated_answers_no_such_coin_from_init() {
    let (_chain, _node, ctx) = setup(1);
    let request = serde_json::from_value(task_request("NOPE")).unwrap();
    let error = error_of(block_on(init_get_new_address(ctx, request)), "init must fail");
    assert_eq!(error.get_inner().status_code(), StatusCode::BAD_REQUEST);
    assert!(matches!(error.get_inner(), HDWalletRpcError::NoSuchCoin { coin } if coin == "NOPE"));
}

/// T49.36: a hardware context beside a software login is not touched by a software-policy coin: the task completes,
/// and the device (a context with no transport behind it) is never asked.
#[test]
fn evm_hd_p4_t4936_a_hardware_context_beside_a_software_login_is_not_touched() {
    use primitives::hash::H264;
    let (_chain, _node, ctx) = setup(1);
    CryptoCtx::from_ctx(&ctx).unwrap().init_trezor_ctx_for_tests(
        H264::from_str("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap(),
        None,
    );
    let (status, seen) = run_task(&ctx, task_request(PLATFORM));
    assert_eq!(status["status"], "Ok", "{status}");
    assert_only_software_statuses(&seen);
    assert_eq!(total(&ctx), 2);
}

/// R54, decision 2: the internal chain is refused for an EVM platform and for a token, in the task form, with
/// `InvalidBip44Chain` and its chain; nothing is recorded.
#[test]
fn evm_hd_p4_the_internal_chain_is_refused_for_an_evm_platform_and_token() {
    let (_chain, _node, ctx) = setup(1);
    for ticker in [PLATFORM, USDT] {
        let (status, _) = run_task(&ctx, json!({ "coin": ticker, "account_id": 0, "chain": "Internal" }));
        assert_eq!(status["status"], "Error", "{ticker}: {status}");
        assert_eq!(
            status["details"]["error_type"], "InvalidBip44Chain",
            "{ticker}: {status}"
        );
        assert_eq!(
            status["details"]["error_data"]["chain"], "Internal",
            "{ticker}: {status}"
        );
    }
    assert_eq!(known_addresses(&ctx), 1);
}

/// R49.41: an account the wallet does not hold is `UnknownAccount` with its index, and nothing is recorded.
#[test]
fn evm_hd_p4_an_unknown_account_is_an_error_status() {
    let (_chain, _node, ctx) = setup(1);
    let (status, _) = run_task(&ctx, json!({ "coin": PLATFORM, "account_id": 9 }));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], "UnknownAccount", "{status}");
    assert_eq!(status["details"]["error_data"]["account_id"], 9, "{status}");
    assert_eq!(known_addresses(&ctx), 1);
}

/// Decision 5, R49.41 item 1: the gap check runs with the request's `gap_limit`. The last `gap_limit` known addresses
/// being all unused answers `EmptyAddressesLimitReached` before anything is derived; a used one among them lets the
/// task through; fewer known addresses than the limit never hit it.
#[test]
fn evm_hd_p4_the_gap_check_refuses_a_run_of_unused_addresses() {
    let (chain, _node, ctx) = setup(3);
    let refused = |ctx: &MmArc, gap_limit: u32| {
        let (status, _) = run_task(
            ctx,
            json!({ "coin": PLATFORM, "account_id": 0, "gap_limit": gap_limit }),
        );
        status
    };

    let status = refused(&ctx, 2);
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(
        status["details"]["error_type"], "EmptyAddressesLimitReached",
        "{status}"
    );
    assert_eq!(status["details"]["error_data"]["gap_limit"], 2, "{status}");
    assert_eq!(known_addresses(&ctx), 3, "nothing is recorded");

    // Address 0 is outside the last two; its use does not help.
    chain.set_nonce(&address_at(&ctx, 0), 1);
    assert_eq!(refused(&ctx, 2)["details"]["error_type"], "EmptyAddressesLimitReached");

    // Address 1 is inside the last two.
    chain.set_nonce(&address_at(&ctx, 1), 1);
    let status = refused(&ctx, 2);
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(known_addresses(&ctx), 4);

    // Fewer known addresses than the limit: not checked at all.
    let status = refused(&ctx, 50);
    assert_eq!(status["status"], "Ok", "{status}");
}

/// For a token ticker, an address is used by the token's balance as well (the rule of the ticker's scan).
#[test]
fn evm_hd_p4_the_gap_check_of_a_token_counts_its_balance() {
    let (chain, _node, ctx) = setup(3);
    let request = json!({ "coin": USDT, "account_id": 0, "gap_limit": 2 });
    let (status, _) = run_task(&ctx, request.clone());
    assert_eq!(
        status["details"]["error_type"], "EmptyAddressesLimitReached",
        "{status}"
    );
    chain.set_token(USDT_CONTRACT, &address_at(&ctx, 2), 1_000_000);
    let (status, _) = run_task(&ctx, request);
    assert_eq!(status["status"], "Ok", "{status}");
}

/// R49.40 item 3, decision 6: a balance query that fails after the recording leaves the address known (no rollback);
/// the answer is the error of the direct method's class.
#[test]
fn evm_hd_p4_a_failing_balance_query_leaves_the_address_known() {
    let (chain, _node, ctx) = setup(1);
    chain.fail_native();
    let (status, _) = run_task(&ctx, task_request(PLATFORM));
    assert_eq!(status["status"], "Error", "{status}");
    assert_eq!(status["details"]["error_type"], FAILED_KIND, "{status}");
    assert_eq!(known_addresses(&ctx), 2, "the raised count stays");

    chain.heal();
    let (status, _) = run_task(&ctx, task_request(PLATFORM));
    assert_eq!(status["status"], "Ok", "{status}");
    assert_eq!(status["details"]["new_address"]["derivation_path"], "m/44'/60'/0'/0/2");
}
