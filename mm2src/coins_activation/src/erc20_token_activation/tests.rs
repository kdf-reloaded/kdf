//! Wire-shape tests of `enable_erc20`, `enable_nft` and `task::enable_erc20` (CRD ch. 35 §35.2, §35.3A, ch. 48
//! §48.11; T35.12, T35.13, T35.16).
//!
//! Every test serializes the response or the error as the dispatcher would and asserts the `error_type`, the JSON
//! type of `error_data` (or its absence) and the HTTP status. The activation runs end to end against a scripted
//! EVM node on a local socket.

use super::*;
use crate::eth_with_tokens_activation::EthWithTokensActivationRequest;
use crate::evm_test_support::*;
use crate::init_token::{cancel_init_token, init_token, init_token_status};
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensReq};
use coins::{lp_register_coin, RegisterCoinParams, TestCoin};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmCtxBuilder;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Which backend calls of the scripted node fail.
#[derive(Default)]
struct Failures {
    decimals: AtomicBool,
    balance_of: AtomicBool,
    slow_balance_of: AtomicBool,
}

fn node_with(failures: Arc<Failures>) -> MockEvmNode {
    spawn_node(move |method, params| {
        let data = params[0]["data"].as_str().unwrap_or_default();
        if method == "eth_call" && data.starts_with("0x313ce567") && failures.decimals.load(Ordering::SeqCst) {
            return Reply::HttpStatus(502);
        }
        if method == "eth_call" && data.starts_with("0x70a08231") {
            if failures.balance_of.load(Ordering::SeqCst) {
                return Reply::RpcError;
            }
            if failures.slow_balance_of.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        healthy_node_reply(method, params, 1_500_000_000_000_000_000, 2_000_000)
    })
}

/// Activates the platform without tokens on `node` and returns the context.
fn ctx_with_platform(ctx: MmArc, node: &MockEvmNode) -> MmArc {
    let request: EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest> = serde_json::from_value(json!({
        "ticker": PLATFORM,
        "nodes": [{ "url": node.url }],
        "erc20_tokens_requests": [],
        "swap_contract_address": SWAP_CONTRACT,
    }))
    .unwrap();
    block_on(enable_platform_coin_with_tokens::<EthCoin>(ctx.clone(), request))
        .unwrap_or_else(|e| panic!("the platform activation failed: {}", e));
    ctx
}

fn token_request(extra: Json) -> EnableEvmTokenRequest {
    serde_json::from_value(extra).unwrap_or_else(|e| panic!("bad request: {}", e))
}

fn erc20(ticker: &str) -> EnableEvmTokenRequest { token_request(json!({ "ticker": ticker, "activation_params": {} })) }

fn enable(ctx: &MmArc, req: EnableEvmTokenRequest) -> Result<EvmTokenActivationResult, MmError<EnableTokenError>> {
    block_on(enable_evm_token(ctx.clone(), req))
}

fn wire<E: serde::Serialize + HttpStatusCode + NotMmError>(error: &MmError<E>) -> (Json, StatusCode) {
    (
        serde_json::to_value(error.get_inner()).expect("a serializable error"),
        error.get_inner().status_code(),
    )
}

fn failure(ctx: &MmArc, req: EnableEvmTokenRequest) -> (Json, StatusCode) {
    match enable(ctx, req) {
        Ok(result) => panic!(
            "expected a failure, activated: {}",
            serde_json::to_value(&result).unwrap()
        ),
        Err(e) => wire(&e),
    }
}

fn assert_error(wire: &(Json, StatusCode), error_type: &str, error_data: Option<Json>, status: StatusCode) {
    assert_eq!(wire.0["error_type"], error_type, "{:?}", wire.0);
    match error_data {
        Some(expected) => assert_eq!(wire.0["error_data"], expected, "{:?}", wire.0),
        None => assert!(wire.0.get("error_data").is_none(), "no payload expected: {:?}", wire.0),
    }
    assert_eq!(wire.1, status, "{:?}", wire.0);
}

fn string_error(wire: &(Json, StatusCode), error_type: &str, status: StatusCode) {
    assert_eq!(wire.0["error_type"], error_type, "{:?}", wire.0);
    assert!(
        wire.0["error_data"].is_string(),
        "a string payload expected: {:?}",
        wire.0
    );
    assert_eq!(wire.1, status, "{:?}", wire.0);
}

fn platform_coin(ctx: &MmArc) -> EthCoin {
    match block_on(coins::lp_coinfind(ctx, PLATFORM))
        .unwrap()
        .expect("the platform is active")
    {
        coins::MmCoinEnum::EthCoin(coin) => coin,
        other => panic!("not an EVM coin: {}", other.ticker()),
    }
}

// ---------------------------------------------------------------------------
// T35.12: `enable_erc20`
// ---------------------------------------------------------------------------

#[test]
fn the_result_carries_the_lower_case_contract_and_the_token_confirmations() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let result = enable(&ctx, erc20("USDT-ERC20")).unwrap_or_else(|e| panic!("enable_erc20 failed: {}", e));
    let result = serde_json::to_value(&result).unwrap();

    assert_eq!(result["platform_coin"], PLATFORM);
    assert_eq!(result["token_contract_address"], USDT_CONTRACT.to_lowercase());
    assert_eq!(
        result["required_confirmations"], 4,
        "the token config, not the platform's 2"
    );
    let balances = result["balances"].as_object().unwrap();
    assert_eq!(balances.len(), 1, "exactly the platform's single address");
    let (address, balance) = balances.iter().next().unwrap();
    assert_eq!(address, &platform_coin(&ctx).my_address().unwrap());
    assert_eq!(balance["spendable"], "2");
    assert_eq!(balance["unspendable"], "0");
    let mut keys: Vec<_> = result.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, [
        "balances",
        "platform_coin",
        "required_confirmations",
        "token_contract_address"
    ]);

    // Registered, and recorded on the platform.
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_some());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().contains_key("USDT-ERC20"));
}

#[test]
fn the_requested_confirmations_override_the_configured_ones() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let req = token_request(json!({ "ticker": "USDT-ERC20", "activation_params": { "required_confirmations": 11 } }));
    let result = serde_json::to_value(enable(&ctx, req).unwrap()).unwrap();
    assert_eq!(result["required_confirmations"], 11);
    // Without a token config value the platform's effective value applies.
    let result = serde_json::to_value(enable(&ctx, erc20("DAI-ERC20")).unwrap()).unwrap();
    assert_eq!(result["required_confirmations"], 2);
}

#[test]
fn a_failing_decimals_read_is_transport_string_500_and_registers_nothing() {
    let failures = Arc::new(Failures::default());
    let node = node_with(failures.clone());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    failures.decimals.store(true, Ordering::SeqCst);
    let wire = failure(&ctx, erc20("DAI-ERC20"));
    string_error(&wire, "Transport", StatusCode::INTERNAL_SERVER_ERROR);
    assert!(block_on(coins::lp_coinfind(&ctx, "DAI-ERC20")).unwrap().is_none());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().is_empty());
}

#[test]
fn a_failing_balance_read_is_transport_string_500_and_records_no_token_on_the_platform() {
    let failures = Arc::new(Failures::default());
    let node = node_with(failures.clone());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    failures.balance_of.store(true, Ordering::SeqCst);
    let wire = failure(&ctx, erc20("USDT-ERC20"));
    string_error(&wire, "Transport", StatusCode::INTERNAL_SERVER_ERROR);
    // R35.2.6: a failure leaves the token unregistered and unrecorded on the platform.
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_none());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().is_empty());
}

#[test]
fn the_check_order_follows_the_chapter() {
    let node = node_with(Arc::default());
    // No platform: the platform comes after the token configuration.
    let ctx = iguana_ctx();
    assert_error(
        &failure(&ctx, erc20("NOSUCH")),
        "TokenConfigIsNotFound",
        Some(json!("NOSUCH")),
        StatusCode::BAD_REQUEST,
    );
    assert_error(
        &failure(&ctx, erc20("USDT-ERC20")),
        "PlatformCoinIsNotActivated",
        Some(json!(PLATFORM)),
        StatusCode::BAD_REQUEST,
    );
    let ctx = ctx_with_platform(ctx, &node);
    enable(&ctx, erc20("USDT-ERC20")).unwrap();
    assert_error(
        &failure(&ctx, erc20("USDT-ERC20")),
        "TokenIsAlreadyActivated",
        Some(json!("USDT-ERC20")),
        StatusCode::BAD_REQUEST,
    );
}

#[test]
fn configuration_and_protocol_errors_serialize_as_bound() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);

    let (wire_json, status) = failure(&ctx, erc20("BAD-ERC20"));
    assert_eq!(wire_json["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(wire_json["error_data"]["ticker"], "BAD-ERC20");
    assert_eq!(wire_json["error_data"]["protocol"]["type"], "ERC20");
    assert_eq!(
        wire_json["error_data"]["protocol"]["protocol_data"]["platform"],
        PLATFORM
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (wire_json, status) = failure(&ctx, erc20("KMD"));
    assert_eq!(wire_json["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The protocol of a configured token cannot be parsed.
    let mut conf = ctx.conf.clone();
    conf["coins"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "coin": "BROKEN", "protocol": { "type": "ERC20" } }));
    let broken_ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(broken_ctx.clone(), "evm activation test passphrase").unwrap();
    let (wire_json, status) = failure(&broken_ctx, erc20("BROKEN"));
    assert_eq!(wire_json["error_type"], "TokenProtocolParseError");
    assert_eq!(wire_json["error_data"]["ticker"], "BROKEN");
    assert!(wire_json["error_data"]["error"].is_string());
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "500 on the one-shot call");
}

#[test]
fn a_platform_that_is_not_an_evm_coin_is_unsupported_platform_coin() {
    let mut conf = iguana_ctx().conf.clone();
    conf["coins"].as_array_mut().unwrap().push(json!({
        "coin": "ODD-ERC20",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": "KMD", "contract_address": USDT_CONTRACT } },
        "decimals": 6,
    }));
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "evm activation test passphrase").unwrap();
    block_on(lp_register_coin(&ctx, TestCoin::default().into(), RegisterCoinParams {
        ticker: "KMD".to_owned(),
        tx_history: false,
    }))
    .unwrap();
    assert_error(
        &failure(&ctx, erc20("ODD-ERC20")),
        "UnsupportedPlatformCoin",
        Some(json!({ "platform_coin_ticker": "KMD", "token_ticker": "ODD-ERC20" })),
        StatusCode::INTERNAL_SERVER_ERROR,
    );
}

/// An HD platform whose first account knows `addresses` external addresses; the scripted node answers every
/// token balance read with `balance_of`.
fn hd_platform_with_addresses(addresses: u32, balance_of: u128) -> (MockEvmNode, MmArc) {
    let node = spawn_node(move |method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, balance_of),
    });
    let ctx = ctx_with_platform(hd_ctx(), &node);
    match &platform_coin(&ctx).derivation_method {
        DerivationMethod::HDWallet(hd_wallet) => {
            let mut accounts = block_on(hd_wallet.accounts.lock());
            let account = accounts.values_mut().next().expect("the default account");
            account.external_addresses_number = addresses;
        },
        DerivationMethod::Iguana(_) => panic!("an HD platform expected"),
    }
    (node, ctx)
}

#[test]
fn enable_erc20_on_an_hd_platform_reports_every_known_address() {
    let (_node, ctx) = hd_platform_with_addresses(3, 7_000_000);
    let result = serde_json::to_value(enable(&ctx, erc20("USDT-ERC20")).unwrap_or_else(|e| panic!("{}", e))).unwrap();

    assert_eq!(result["platform_coin"], PLATFORM);
    assert_eq!(result["token_contract_address"], USDT_CONTRACT.to_lowercase());
    assert_eq!(result["required_confirmations"], 4);
    let balances = result["balances"].as_object().unwrap();
    assert_eq!(balances.len(), 3, "one entry per known address");
    assert!(balances.contains_key(&platform_coin(&ctx).my_address().unwrap()));
    for balance in balances.values() {
        assert!(balance["spendable"].is_string() && balance["unspendable"].is_string());
    }
}

#[test]
fn the_one_shot_hd_token_is_the_coin_the_task_registers() {
    let (_node, one_shot_ctx) = hd_platform_with_addresses(2, 7_000_000);
    enable(&one_shot_ctx, erc20("USDT-ERC20")).unwrap_or_else(|e| panic!("{}", e));
    let (_task_node, task_ctx) = hd_platform_with_addresses(2, 7_000_000);
    let status = run_task(&task_ctx, json!({ "ticker": "USDT-ERC20", "activation_params": {} }));
    assert_eq!(status["status"], "Ok", "{}", status);

    let token_of = |ctx: &MmArc| match block_on(coins::lp_coinfind(ctx, "USDT-ERC20")).unwrap() {
        Some(coins::MmCoinEnum::EthCoin(token)) => token,
        _ => panic!("the token is registered as an EVM coin"),
    };
    let (one_shot, task) = (token_of(&one_shot_ctx), token_of(&task_ctx));
    // Both paths build the token through the same helper, so the derivation method is the same whatever it is.
    assert_eq!(
        format!("{:?}", one_shot.derivation_method),
        format!("{:?}", task.derivation_method)
    );
    assert_eq!(one_shot.my_address().unwrap(), task.my_address().unwrap());
    assert_eq!(one_shot.required_confirmations(), task.required_confirmations());
    assert_eq!(
        format!("{:?}", one_shot.erc20_token_info()),
        format!("{:?}", task.erc20_token_info())
    );
    assert_eq!(
        platform_coin(&one_shot_ctx).my_address().unwrap(),
        platform_coin(&task_ctx).my_address().unwrap()
    );
    let recorded = |ctx: &MmArc| format!("{:?}", platform_coin(ctx).get_erc20_tokens_infos());
    assert_eq!(
        recorded(&one_shot_ctx),
        recorded(&task_ctx),
        "the same state recorded on the platform"
    );
}

#[test]
fn enable_erc20_on_an_hd_platform_checks_path_to_address_against_the_enabled_address() {
    let (_node, ctx) = hd_platform_with_addresses(1, 1);
    let at = |address_id: u32| {
        token_request(json!({ "ticker": "USDT-ERC20", "activation_params": {
            "path_to_address": { "account_id": 0, "chain": "External", "address_id": address_id } } }))
    };
    string_error(&failure(&ctx, at(5)), "Internal", StatusCode::INTERNAL_SERVER_ERROR);
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_none());
    assert!(enable(&ctx, at(0)).is_ok());
}

#[test]
fn erc20_hd_activation_false_restores_the_refusal() {
    let node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 0),
    });
    let ctx = ctx_with_platform(hd_ctx_with_conf(json!({ "erc20_hd_activation": false })), &node);
    assert_error(
        &failure(&ctx, erc20("USDT-ERC20")),
        "UnexpectedDerivationMethod",
        Some(json!("ExpectedSingleAddress")),
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_none());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().is_empty());
}

#[test]
fn only_an_explicit_false_disables_erc20_hd_activation() {
    for (conf, expected) in [
        (json!({}), true),
        (json!({ "erc20_hd_activation": true }), true),
        (json!({ "erc20_hd_activation": false }), false),
        (json!({ "erc20_hd_activation": "false" }), true),
        (json!({ "erc20_hd_activation": null }), true),
    ] {
        let ctx = MmCtxBuilder::new().with_conf(conf.clone()).into_mm_arc();
        assert_eq!(erc20_hd_activation(&ctx), expected, "{}", conf);
    }
}

#[test]
fn custom_tokens_serialize_each_rejection_as_tabled() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let custom = |ticker: &str, contract: &str| {
        token_request(json!({
            "ticker": ticker,
            "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": contract } },
            "activation_params": {},
        }))
    };
    let free_contract = "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56";

    assert_error(
        &failure(&ctx, custom("USDT-ERC20", free_contract)),
        "CustomTokenError",
        Some(json!({ "DuplicateTickerInConfig": { "ticker_in_config": "USDT-ERC20" } })),
        StatusCode::BAD_REQUEST,
    );
    assert_error(
        &failure(&ctx, custom("MINE", USDT_CONTRACT)),
        "CustomTokenError",
        Some(json!({ "DuplicateContractInConfig": { "ticker_in_config": "USDT-ERC20" } })),
        StatusCode::BAD_REQUEST,
    );
    assert_error(
        &failure(&ctx, custom("MINE", "0xnothex")),
        "CustomTokenError",
        Some(json!("InvalidTokenAddress")),
        StatusCode::BAD_REQUEST,
    );

    // A custom token activates as a wallet-only coin: its decimals are read on-chain.
    let result = serde_json::to_value(enable(&ctx, custom("MINE", free_contract)).unwrap()).unwrap();
    assert_eq!(result["token_contract_address"], free_contract.to_lowercase());
    // The same contract under another ticker is now active.
    assert_error(
        &failure(&ctx, custom("YOURS", free_contract)),
        "CustomTokenError",
        Some(
            json!({ "TokenWithSameContractAlreadyActivated": { "ticker": "MINE", "contract_address": free_contract } }),
        ),
        StatusCode::BAD_REQUEST,
    );
    // An inline protocol that is not an ERC-20 is an unexpected protocol.
    let not_erc20 = token_request(json!({
        "ticker": "MINE2",
        "protocol": { "type": "UTXO" },
        "activation_params": {},
    }));
    assert_eq!(failure(&ctx, not_erc20).0["error_type"], "UnexpectedTokenProtocol");
}

/// CRD ch.35 R35.2.7: a custom token activated through `enable_erc20` is a wallet-only coin although it has no
/// coins-config entry; a coins-config token keeps the wallet-only setting of its entry.
#[test]
fn a_custom_token_of_enable_erc20_is_wallet_only_and_a_config_token_is_not() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let custom = token_request(json!({
        "ticker": "MINE",
        "protocol": {
            "type": "ERC20",
            "protocol_data": { "platform": PLATFORM, "contract_address": "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56" }
        },
        "activation_params": {},
    }));
    enable(&ctx, custom).unwrap_or_else(|e| panic!("the custom token must activate: {}", e));
    enable(&ctx, erc20("USDT-ERC20")).unwrap_or_else(|e| panic!("the config token must activate: {}", e));

    let wallet_only = |ticker: &str| {
        block_on(coins::lp_coinfind(&ctx, ticker))
            .unwrap()
            .unwrap_or_else(|| panic!("{} is not active", ticker))
            .wallet_only(&ctx)
    };
    assert!(wallet_only("MINE"), "a custom token is wallet-only");
    assert!(!wallet_only("USDT-ERC20"), "a config token follows its config entry");
}

#[test]
fn the_activation_params_are_required_and_an_nft_provider_does_not_fit_an_erc20() {
    assert!(serde_json::from_value::<EnableEvmTokenRequest>(json!({ "ticker": "USDT-ERC20" })).is_err());
    assert!(serde_json::from_value::<EnableEvmTokenRequest>(
        json!({ "ticker": "USDT-ERC20", "activation_params": null })
    )
    .is_err());
    assert!(serde_json::from_value::<EnableEvmTokenRequest>(
        json!({ "ticker": "USDT-ERC20", "activation_params": {} })
    )
    .is_ok());

    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let with_provider = token_request(json!({
        "ticker": "USDT-ERC20",
        "activation_params": { "provider": { "type": "Moralis", "info": { "url": "https://indexer.example.com/" } } },
    }));
    string_error(
        &failure(&ctx, with_provider),
        "Internal",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
}

#[test]
fn the_reserved_token_errors_serialize_as_tabled() {
    let cases: Vec<(EnableTokenError, &str, Option<Json>, StatusCode)> = vec![
        (
            EnableTokenError::PrivKeyPolicyNotAllowed("e".into()),
            "PrivKeyPolicyNotAllowed",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            EnableTokenError::PlatformCoinMismatch,
            "PlatformCoinMismatch",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            EnableTokenError::CouldNotFetchBalance("e".into()),
            "CouldNotFetchBalance",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            EnableTokenError::InvalidConfig("e".into()),
            "InvalidConfig",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            EnableTokenError::InvalidPayload("e".into()),
            "InvalidPayload",
            Some(json!("e")),
            StatusCode::BAD_REQUEST,
        ),
        (
            EnableTokenError::Transport("e".into()),
            "Transport",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            EnableTokenError::TokenProtocolParseError {
                ticker: "T".into(),
                error: "e".into(),
            },
            "TokenProtocolParseError",
            Some(json!({ "ticker": "T", "error": "e" })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            EnableTokenError::CustomTokenError(CustomTokenError::InvalidTokenAddress),
            "CustomTokenError",
            Some(json!("InvalidTokenAddress")),
            StatusCode::BAD_REQUEST,
        ),
        (
            EnableTokenError::UnexpectedDerivationMethod(TokenDerivationError::ExpectedSingleAddress),
            "UnexpectedDerivationMethod",
            Some(json!("ExpectedSingleAddress")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];
    for (error, error_type, error_data, status) in cases {
        let wire = (serde_json::to_value(&error).unwrap(), error.status_code());
        assert_error(&wire, error_type, error_data, status);
    }
}

// ---------------------------------------------------------------------------
// T35.13: `enable_nft` shares the single-token contract (R35.2.8)
// ---------------------------------------------------------------------------

fn nft(ticker: &str, params: Json) -> EnableEvmTokenRequest {
    token_request(json!({ "ticker": ticker, "activation_params": params }))
}

fn provider(url: &str) -> Json { json!({ "provider": { "type": "Moralis", "info": { "url": url } } }) }

#[test]
fn enable_nft_failures_use_the_single_token_discriminants() {
    let node = node_with(Arc::default());

    // No platform yet.
    let ctx = iguana_ctx();
    assert_error(
        &failure(&ctx, nft("NFT_ETH", provider("https://indexer.example.com/"))),
        "PlatformCoinIsNotActivated",
        Some(json!(PLATFORM)),
        StatusCode::BAD_REQUEST,
    );
    // No configuration entry.
    assert_error(
        &failure(&ctx, nft("NFT_FTM", provider("https://indexer.example.com/"))),
        "TokenConfigIsNotFound",
        Some(json!("NFT_FTM")),
        StatusCode::BAD_REQUEST,
    );

    let ctx = ctx_with_platform(ctx, &node);
    // Unusable providers are an internal failure, not a request-parse failure.
    for params in [
        json!({}),
        json!({ "provider": { "type": "Unknown", "info": { "url": "https://indexer.example.com/" } } }),
        json!({ "provider": { "type": "Moralis", "info": { "url": "not a url" } } }),
    ] {
        string_error(
            &failure(&ctx, nft("NFT_ETH", params)),
            "Internal",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    // An unreachable provider is a transport failure; 500 on this method, not 502.
    string_error(
        &failure(&ctx, nft("NFT_ETH", provider(&refusing_url()))),
        "Transport",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    // The provider rejected the request.
    let rejecting = spawn_node(|_, _| Reply::HttpStatus(400));
    string_error(
        &failure(&ctx, nft("NFT_ETH", provider(&rejecting.url))),
        "InvalidPayload",
        StatusCode::BAD_REQUEST,
    );
    // Nothing was marked active by the failures.
    let nft_ctx = NftCtx::from_mm_ctx(&ctx).unwrap();
    assert!(!nft_ctx.is_activated(Chain::Eth));
}

#[test]
fn an_active_nft_is_token_is_already_activated() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    NftCtx::from_mm_ctx(&ctx).unwrap().mark_activated(Chain::Eth);
    assert_error(
        &failure(&ctx, nft("NFT_ETH", provider("https://indexer.example.com/"))),
        "TokenIsAlreadyActivated",
        Some(json!("NFT_ETH")),
        StatusCode::BAD_REQUEST,
    );
}

#[test]
fn enable_nft_platform_failures() {
    // A platform that is not an EVM coin.
    let ctx = iguana_ctx();
    block_on(lp_register_coin(&ctx, TestCoin::default().into(), RegisterCoinParams {
        ticker: "KMD".to_owned(),
        tx_history: false,
    }))
    .unwrap();
    let inline = |ticker: &str, platform: &str| {
        token_request(json!({
            "ticker": ticker,
            "protocol": { "type": "NFT", "protocol_data": { "platform": platform } },
            "activation_params": provider("https://indexer.example.com/"),
        }))
    };
    assert_error(
        &failure(&ctx, inline("NFT_KMD", "KMD")),
        "UnsupportedPlatformCoin",
        Some(json!({ "platform_coin_ticker": "KMD", "token_ticker": "NFT_KMD" })),
        StatusCode::INTERNAL_SERVER_ERROR,
    );

    // An EVM platform without an NFT chain.
    let node = node_with(Arc::default());
    let ctx = iguana_ctx();
    let ctx = {
        let request: EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest> = serde_json::from_value(json!({
            "ticker": "ETHW",
            "nodes": [{ "url": node.url }],
            "erc20_tokens_requests": [],
        }))
        .unwrap();
        block_on(enable_platform_coin_with_tokens::<EthCoin>(ctx.clone(), request)).unwrap();
        ctx
    };
    string_error(
        &failure(&ctx, inline("NFT_ETHW", "ETHW")),
        "Internal",
        StatusCode::INTERNAL_SERVER_ERROR,
    );

    // An HD platform.
    let hd_node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 0),
    });
    let ctx = ctx_with_platform(hd_ctx(), &hd_node);
    assert_error(
        &failure(&ctx, nft("NFT_ETH", provider("https://indexer.example.com/"))),
        "UnexpectedDerivationMethod",
        Some(json!("ExpectedSingleAddress")),
        StatusCode::INTERNAL_SERVER_ERROR,
    );
}

// ---------------------------------------------------------------------------
// T35.16: `task::enable_erc20`
// ---------------------------------------------------------------------------

fn task_request(extra: Json) -> crate::init_token::InitTokenReq<EvmTokenActivationParams> {
    serde_json::from_value(extra).unwrap_or_else(|e| panic!("bad request: {}", e))
}

fn init_error(ctx: &MmArc, extra: Json) -> (Json, StatusCode) {
    match block_on(init_token::<EthCoin>(ctx.clone(), task_request(extra))) {
        Ok(response) => panic!("expected an init failure, got task {}", response.task_id),
        Err(e) => wire(&e),
    }
}

fn poll_terminal(ctx: &MmArc, task_id: u64) -> Json {
    for _ in 0..400 {
        let status = block_on(init_token_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": task_id, "forget_if_finished": false }))
                .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {}", e));
        let status = serde_json::to_value(&status).unwrap();
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            return status;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the task did not finish");
}

fn run_task(ctx: &MmArc, extra: Json) -> Json {
    let response = block_on(init_token::<EthCoin>(ctx.clone(), task_request(extra)))
        .unwrap_or_else(|e| panic!("init failed: {}", e));
    poll_terminal(ctx, response.task_id)
}

#[test]
fn task_init_errors_are_checked_in_the_bound_order_with_their_shapes() {
    let node = node_with(Arc::default());
    let ctx = iguana_ctx();
    let params = |ticker: &str| json!({ "ticker": ticker, "activation_params": {} });

    // 2. The configuration: a token that is not configured; 400 for a protocol parse error on this family.
    assert_error(
        &init_error(&ctx, params("NOSUCH")),
        "TokenConfigIsNotFound",
        Some(json!("NOSUCH")),
        StatusCode::BAD_REQUEST,
    );
    let (wire_json, status) = init_error(&ctx, params("BAD-ERC20"));
    assert_eq!(wire_json["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(wire_json["error_data"]["ticker"], "BAD-ERC20");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut conf = ctx.conf.clone();
    conf["coins"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "coin": "BROKEN", "protocol": { "type": "ERC20" } }));
    let broken_ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(broken_ctx.clone(), "evm activation test passphrase").unwrap();
    let (wire_json, status) = init_error(&broken_ctx, params("BROKEN"));
    assert_eq!(wire_json["error_type"], "TokenProtocolParseError");
    assert_eq!(wire_json["error_data"]["ticker"], "BROKEN");
    assert_eq!(status, StatusCode::BAD_REQUEST, "400 on the task family");

    // The custom token rules.
    let custom = json!({
        "ticker": "USDT-ERC20",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": DAI_CONTRACT } },
        "activation_params": {},
    });
    assert_error(
        &init_error(&ctx, custom),
        "CustomTokenError",
        Some(json!({ "DuplicateTickerInConfig": { "ticker_in_config": "USDT-ERC20" } })),
        StatusCode::BAD_REQUEST,
    );

    // 3. The platform is not active.
    assert_error(
        &init_error(&ctx, params("USDT-ERC20")),
        "PlatformCoinIsNotActivated",
        Some(json!(PLATFORM)),
        StatusCode::BAD_REQUEST,
    );

    // 1. The token is active: an object, unlike the string of the one-shot call.
    let ctx = ctx_with_platform(ctx, &node);
    enable(&ctx, erc20("USDT-ERC20")).unwrap();
    assert_error(
        &init_error(&ctx, params("USDT-ERC20")),
        "TokenIsAlreadyActivated",
        Some(json!({ "ticker": "USDT-ERC20" })),
        StatusCode::BAD_REQUEST,
    );
}

#[test]
fn task_init_on_a_non_evm_platform_is_unsupported_platform_coin() {
    let mut conf = iguana_ctx().conf.clone();
    conf["coins"].as_array_mut().unwrap().push(json!({
        "coin": "ODD-ERC20",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": "KMD", "contract_address": USDT_CONTRACT } },
        "decimals": 6,
    }));
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "evm activation test passphrase").unwrap();
    block_on(lp_register_coin(&ctx, TestCoin::default().into(), RegisterCoinParams {
        ticker: "KMD".to_owned(),
        tx_history: false,
    }))
    .unwrap();
    assert_error(
        &init_error(&ctx, json!({ "ticker": "ODD-ERC20", "activation_params": {} })),
        "UnsupportedPlatformCoin",
        Some(json!({ "platform_coin_ticker": "KMD", "token_ticker": "ODD-ERC20" })),
        StatusCode::INTERNAL_SERVER_ERROR,
    );
}

#[test]
fn task_success_object_for_a_single_address_platform() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let status = run_task(
        &ctx,
        json!({ "ticker": "USDT-ERC20", "activation_params": {}, "client_id": 3 }),
    );
    assert_eq!(status["status"], "Ok", "{}", status);
    let details = &status["details"];
    assert_eq!(details["ticker"], "USDT-ERC20");
    assert_eq!(details["platform_coin"], PLATFORM);
    assert_eq!(details["token_contract_address"], USDT_CONTRACT, "the EIP-55 form");
    assert_eq!(details["current_block"], 42);
    assert_eq!(details["required_confirmations"], 4);
    let wallet = &details["wallet_balance"];
    assert_eq!(wallet["wallet_type"], "Iguana");
    assert_eq!(wallet["address"], platform_coin(&ctx).my_address().unwrap());
    assert_eq!(wallet["balance"]["USDT-ERC20"]["spendable"], "2");
    assert_eq!(wallet["balance"].as_object().unwrap().len(), 1, "the token ticker only");

    // Registered and recorded.
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_some());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().contains_key("USDT-ERC20"));
}

#[test]
fn task_status_errors_are_transport_and_could_not_fetch_balance() {
    let failures = Arc::new(Failures::default());
    let node = node_with(failures.clone());
    let ctx = ctx_with_platform(iguana_ctx(), &node);

    failures.decimals.store(true, Ordering::SeqCst);
    let status = run_task(&ctx, json!({ "ticker": "DAI-ERC20", "activation_params": {} }));
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "Transport");
    assert!(status["details"]["error_data"].is_string());
    failures.decimals.store(false, Ordering::SeqCst);

    failures.balance_of.store(true, Ordering::SeqCst);
    let status = run_task(&ctx, json!({ "ticker": "USDT-ERC20", "activation_params": {} }));
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "CouldNotFetchBalance");
    assert!(status["details"]["error_data"].is_string());
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_none());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().is_empty());
}

#[test]
fn a_custom_token_duplicate_activation_check_is_a_status_level_error() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let free_contract = "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56";
    let custom = |ticker: &str| {
        json!({
            "ticker": ticker,
            "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": free_contract } },
            "activation_params": {},
        })
    };
    assert_eq!(run_task(&ctx, custom("MINE"))["status"], "Ok");
    let status = run_task(&ctx, custom("YOURS"));
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "CustomTokenError");
    assert!(status["details"]["error_data"]["TokenWithSameContractAlreadyActivated"].is_object());
}

#[test]
fn a_cancelled_task_leaves_the_token_unregistered() {
    let failures = Arc::new(Failures::default());
    let node = node_with(failures.clone());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    failures.slow_balance_of.store(true, Ordering::SeqCst);

    let response = block_on(init_token::<EthCoin>(
        ctx.clone(),
        task_request(json!({ "ticker": "USDT-ERC20", "activation_params": {} })),
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    block_on(cancel_init_token::<EthCoin>(
        ctx.clone(),
        serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": response.task_id })).unwrap(),
    ))
    .unwrap_or_else(|e| panic!("cancel failed: {}", e));

    // A cancelled task is not observable.
    let error = error_of(
        block_on(init_token_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": response.task_id })).unwrap(),
        )),
        "the task is gone",
    );
    assert_eq!(
        serde_json::to_value(error.get_inner()).unwrap(),
        json!({ "error_type": "NoSuchTask", "error_data": response.task_id })
    );
    std::thread::sleep(Duration::from_millis(600));
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_none());
    assert!(platform_coin(&ctx).get_erc20_tokens_infos().is_empty());
    // The token can be activated afterwards.
    failures.slow_balance_of.store(false, Ordering::SeqCst);
    assert_eq!(
        run_task(&ctx, json!({ "ticker": "USDT-ERC20", "activation_params": {} }))["status"],
        "Ok"
    );
}

#[test]
fn cancel_of_a_finished_token_task_is_task_finished_409_and_keeps_the_token() {
    let node = node_with(Arc::default());
    let ctx = ctx_with_platform(iguana_ctx(), &node);
    let response = block_on(init_token::<EthCoin>(
        ctx.clone(),
        task_request(json!({ "ticker": "USDT-ERC20", "activation_params": {} })),
    ))
    .unwrap();
    assert_eq!(poll_terminal(&ctx, response.task_id)["status"], "Ok");
    let error = error_of(
        block_on(cancel_init_token::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": response.task_id })).unwrap(),
        )),
        "a finished task",
    );
    assert_error(
        &wire(&error),
        "TaskFinished",
        Some(json!(response.task_id)),
        StatusCode::CONFLICT,
    );
    assert!(block_on(coins::lp_coinfind(&ctx, "USDT-ERC20")).unwrap().is_some());
}

#[test]
fn the_token_task_works_on_an_hd_platform_and_reports_the_hd_form() {
    let node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 7_000_000),
    });
    let ctx = ctx_with_platform(hd_ctx(), &node);
    let status = run_task(&ctx, json!({ "ticker": "USDT-ERC20", "activation_params": {} }));
    assert_eq!(status["status"], "Ok", "{}", status);
    let wallet = &status["details"]["wallet_balance"];
    assert_eq!(wallet["wallet_type"], "HD");
    let account = &wallet["accounts"][0];
    assert_eq!(account["account_index"], 0);
    let address = &account["addresses"][0];
    assert!(address["balance"]["USDT-ERC20"].is_object());
    assert_eq!(
        address["balance"].as_object().unwrap().len(),
        1,
        "the token ticker only"
    );
    assert_eq!(address["address"], platform_coin(&ctx).my_address().unwrap());
    assert_eq!(account["total_balance"].as_object().unwrap().len(), 1);
}

#[test]
fn a_token_can_be_enabled_only_on_the_enabled_address_of_its_platform() {
    let node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 1),
    });
    let ctx = ctx_with_platform(hd_ctx(), &node);
    let other = json!({ "ticker": "USDT-ERC20", "activation_params": { "path_to_address": { "account_id": 0, "chain": "External", "address_id": 5 } } });
    let status = run_task(&ctx, other);
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "Internal");
    let same = json!({ "ticker": "USDT-ERC20", "activation_params": { "path_to_address": { "account_id": 0, "chain": "External", "address_id": 0 } } });
    assert_eq!(run_task(&ctx, same)["status"], "Ok");
}

#[test]
fn the_token_task_error_set_serializes_as_bound() {
    use crate::init_token::InitTokenError as E;
    let cases: Vec<(E, &str, Option<Json>, StatusCode)> = vec![
        (E::NoSuchTask(3), "NoSuchTask", Some(json!(3)), StatusCode::BAD_REQUEST),
        (
            E::TaskTimedOut {
                duration: Duration::new(300, 0),
            },
            "TaskTimedOut",
            Some(json!({ "duration": { "secs": 300, "nanos": 0 } })),
            StatusCode::REQUEST_TIMEOUT,
        ),
        (
            E::TokenIsAlreadyActivated { ticker: "T".into() },
            "TokenIsAlreadyActivated",
            Some(json!({ "ticker": "T" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::TokenConfigIsNotFound("T".into()),
            "TokenConfigIsNotFound",
            Some(json!("T")),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::TokenProtocolParseError {
                ticker: "T".into(),
                error: "e".into(),
            },
            "TokenProtocolParseError",
            Some(json!({ "ticker": "T", "error": "e" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::TokenCreationError {
                ticker: "T".into(),
                error: "e".into(),
            },
            "TokenCreationError",
            Some(json!({ "ticker": "T", "error": "e" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::CouldNotFetchBalance("e".into()),
            "CouldNotFetchBalance",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            E::PlatformCoinIsNotActivated("ETH".into()),
            "PlatformCoinIsNotActivated",
            Some(json!("ETH")),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::UnsupportedPlatformCoin {
                platform_coin_ticker: "KMD".into(),
                token_ticker: "T".into(),
            },
            "UnsupportedPlatformCoin",
            Some(json!({ "platform_coin_ticker": "KMD", "token_ticker": "T" })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            E::PlatformCoinMismatch,
            "PlatformCoinMismatch",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            E::CustomTokenError(CustomTokenError::InvalidTokenAddress),
            "CustomTokenError",
            Some(json!("InvalidTokenAddress")),
            StatusCode::BAD_REQUEST,
        ),
        (
            E::HwError("NotInitialized".into()),
            "HwError",
            Some(json!("NotInitialized")),
            StatusCode::GONE,
        ),
        (
            E::Transport("e".into()),
            "Transport",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            E::Internal("e".into()),
            "Internal",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];
    for (error, error_type, error_data, status) in cases {
        let wire = (serde_json::to_value(&error).unwrap(), error.status_code());
        assert_error(&wire, error_type, error_data, status);
    }
}
