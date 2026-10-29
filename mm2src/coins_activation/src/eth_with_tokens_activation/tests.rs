//! Wire-shape tests of `enable_eth_with_tokens` and `task::enable_eth` (CRD ch. 35 §35.1, §35.3; T35.1 -- T35.11,
//! T35.14, T35.15).
//!
//! Every test serializes the response or the error as the dispatcher would and asserts the `error_type`, the JSON
//! type of `error_data` (or its absence) and the HTTP status. The activation runs end to end against a scripted
//! EVM node on a local socket.

use super::*;
use crate::evm_test_support::*;
use crate::init_platform_coin_with_tokens::{cancel_init_platform_coin_with_tokens, init_platform_coin_with_tokens,
                                            init_platform_coin_with_tokens_status};
use coins::{lp_coinfind, MmCoin as _};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmCtxBuilder;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::json;

type Req = EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest>;

fn request_json(node_urls: &[String], extra: Json) -> Json {
    let nodes: Vec<Json> = node_urls.iter().map(|url| json!({ "url": url })).collect();
    let mut request = json!({
        "ticker": PLATFORM,
        "nodes": nodes,
        "erc20_tokens_requests": [],
        "swap_contract_address": SWAP_CONTRACT,
    });
    for (key, value) in extra.as_object().expect("an object").iter() {
        if value.is_null() {
            request.as_object_mut().expect("an object").remove(key);
        } else {
            request[key] = value.clone();
        }
    }
    request
}

fn request(node_urls: &[String], extra: Json) -> Req {
    serde_json::from_value(request_json(node_urls, extra)).unwrap_or_else(|e| panic!("bad request: {}", e))
}

fn activate(
    ctx: &MmArc,
    req: Req,
) -> Result<EthWithTokensActivationResult, MmError<EnablePlatformCoinWithTokensError>> {
    block_on(enable_platform_coin_with_tokens::<EthCoin>(ctx.clone(), req))
}

/// The error as the dispatcher serializes it: the adjacently tagged pair, and the HTTP status.
fn wire(error: &MmError<EnablePlatformCoinWithTokensError>) -> (Json, StatusCode) {
    (
        serde_json::to_value(error.get_inner()).expect("a serializable error"),
        error.get_inner().status_code(),
    )
}

fn failure(ctx: &MmArc, req: Req) -> (Json, StatusCode) {
    match activate(ctx, req) {
        Ok(result) => panic!(
            "expected a failure, activated: {}",
            serde_json::to_value(&result).unwrap()
        ),
        Err(e) => wire(&e),
    }
}

fn assert_string_error(wire: &(Json, StatusCode), error_type: &str, status: StatusCode) {
    assert_eq!(wire.0["error_type"], error_type, "{:?}", wire.0);
    assert!(
        wire.0["error_data"].is_string(),
        "error_data must be a string: {:?}",
        wire.0
    );
    assert_eq!(wire.1, status, "{:?}", wire.0);
}

/// A node healthy for the probe and the chain height whose other answers are scripted by `rest`.
fn node_with(rest: impl Fn(&str, &Json) -> Option<Reply> + Send + Sync + 'static) -> MockEvmNode {
    spawn_node(move |method, params| {
        rest(method, params).unwrap_or_else(|| healthy_node_reply(method, params, 1_500_000_000_000_000_000, 2_000_000))
    })
}

fn tokens(tickers: &[&str]) -> Json { Json::Array(tickers.iter().map(|ticker| json!({ "ticker": ticker })).collect()) }

// ---------------------------------------------------------------------------
// T35.1 -- T35.3: backend failures (R35.1.6)
// ---------------------------------------------------------------------------

/// CRD ch.55 R55.1.12: the activation locks the tickers of the tokens the request registers beside the platform.
#[test]
fn the_requested_token_tickers_are_the_erc20_tickers_of_the_request() {
    let nodes = vec![refusing_url()];
    let req = request(
        &nodes,
        json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20", "DAI-ERC20"]) }),
    );
    assert_eq!(
        <EthCoin as PlatformWithTokensActivationOps>::requested_token_tickers(&req.request),
        ["USDT-ERC20", "DAI-ERC20"]
    );
    let none = request(&nodes, json!({}));
    assert!(<EthCoin as PlatformWithTokensActivationOps>::requested_token_tickers(&none.request).is_empty());
}

#[test]
fn every_node_answering_502_or_refusing_connections_is_transport_string_502() {
    let ctx = iguana_ctx();
    let gateway = bad_gateway_node();
    for node_urls in [vec![gateway.url.clone()], vec![refusing_url()], vec![
        gateway.url.clone(),
        refusing_url(),
    ]] {
        // All token decimals are in the config.
        let configured = request(&node_urls, json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20"]) }));
        assert_string_error(&failure(&ctx, configured), "Transport", StatusCode::BAD_GATEWAY);
        // A token whose decimals must be read on-chain.
        let on_chain = request(&node_urls, json!({ "erc20_tokens_requests": tokens(&["DAI-ERC20"]) }));
        assert_string_error(&failure(&ctx, on_chain), "Transport", StatusCode::BAD_GATEWAY);
        // And without any token.
        assert_string_error(
            &failure(&ctx, request(&node_urls, json!({}))),
            "Transport",
            StatusCode::BAD_GATEWAY,
        );
    }
    // Nothing was registered by the failures.
    assert!(block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none());
}

#[test]
fn the_decimals_read_failing_after_a_successful_probe_is_transport_string_502() {
    let ctx = iguana_ctx();
    let node = node_with(|method, _| (method == "eth_call").then(|| Reply::HttpStatus(502)));
    let req = request(
        std::slice::from_ref(&node.url),
        json!({ "erc20_tokens_requests": tokens(&["DAI-ERC20"]) }),
    );
    assert_string_error(&failure(&ctx, req), "Transport", StatusCode::BAD_GATEWAY);
    assert!(block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none());
}

#[test]
fn the_chain_height_read_failing_is_transport_string_502() {
    let ctx = iguana_ctx();
    let node = node_with(|method, _| (method == "eth_blockNumber").then(|| Reply::HttpStatus(502)));
    assert_string_error(
        &failure(&ctx, request(std::slice::from_ref(&node.url), json!({}))),
        "Transport",
        StatusCode::BAD_GATEWAY,
    );
    let node = node_with(|method, _| (method == "eth_blockNumber").then_some(Reply::RpcError));
    assert_string_error(
        &failure(&ctx, request(std::slice::from_ref(&node.url), json!({}))),
        "Transport",
        StatusCode::BAD_GATEWAY,
    );
}

#[test]
fn a_balance_read_failing_is_transport_string_502() {
    let ctx = iguana_ctx();
    let node = node_with(|method, _| (method == "eth_getBalance").then_some(Reply::RpcError));
    assert_string_error(
        &failure(&ctx, request(std::slice::from_ref(&node.url), json!({}))),
        "Transport",
        StatusCode::BAD_GATEWAY,
    );
    // A token balance (`balanceOf`) failing.
    let node = node_with(|method, params| {
        let is_balance_of =
            method == "eth_call" && params[0]["data"].as_str().is_some_and(|d| d.starts_with("0x70a08231"));
        is_balance_of.then_some(Reply::RpcError)
    });
    let req = request(
        std::slice::from_ref(&node.url),
        json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20"]) }),
    );
    assert_string_error(&failure(&ctx, req), "Transport", StatusCode::BAD_GATEWAY);
}

#[test]
fn some_unresponsive_nodes_are_not_an_error() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let req = request(&[refusing_url(), node.url.clone()], json!({}));
    let result = activate(&ctx, req).unwrap_or_else(|e| panic!("activation failed: {}", e));
    assert_eq!(result.current_block(), 42);
}

// ---------------------------------------------------------------------------
// T35.4, T35.5: request validation
// ---------------------------------------------------------------------------

#[test]
fn swap_contracts_and_node_urls_are_validated_as_invalid_payload() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let zero = "0x0000000000000000000000000000000000000000";
    for extra in [
        // Absent on a coin that can trade.
        json!({ "swap_contract_address": null }),
        // The zero swap contract.
        json!({ "swap_contract_address": zero }),
        // The zero fallback contract.
        json!({ "fallback_swap_contract": zero }),
    ] {
        let wire = failure(&ctx, request(std::slice::from_ref(&node.url), extra.clone()));
        assert_string_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    }
    // An unparseable URL and unsupported schemes.
    for url in [
        "not a url",
        "ftp://node.example",
        "ws://node.example",
        "wss://node.example",
        "node.example",
    ] {
        let wire = failure(&ctx, request(&[url.to_owned()], json!({})));
        assert_string_error(&wire, "InvalidPayload", StatusCode::BAD_REQUEST);
    }
    // Trading protocol v2 needs the v2 contracts.
    let mut v2_conf = ctx.conf.clone();
    v2_conf["use_trading_proto_v2"] = json!(true);
    let v2_ctx = MmCtxBuilder::new().with_conf(v2_conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(v2_ctx.clone(), "evm activation test passphrase").unwrap();
    assert_string_error(
        &failure(&v2_ctx, request(std::slice::from_ref(&node.url), json!({}))),
        "InvalidPayload",
        StatusCode::BAD_REQUEST,
    );
    let v2_contracts = json!({
        "maker_swap_v2_contract": SWAP_CONTRACT,
        "taker_swap_v2_contract": SWAP_CONTRACT,
        "nft_maker_swap_v2_contract": zero,
    });
    assert_string_error(
        &failure(
            &v2_ctx,
            request(
                std::slice::from_ref(&node.url),
                json!({ "swap_v2_contracts": v2_contracts }),
            ),
        ),
        "InvalidPayload",
        StatusCode::BAD_REQUEST,
    );
}

#[test]
fn a_wallet_only_coin_needs_no_swap_contract() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let mut req = request_json(
        std::slice::from_ref(&node.url),
        json!({ "swap_contract_address": null }),
    );
    req["ticker"] = json!("ETHW");
    let req: Req = serde_json::from_value(req).unwrap();
    activate(&ctx, req).unwrap_or_else(|e| panic!("a wallet-only coin must activate without a swap contract: {}", e));
    assert!(block_on(lp_coinfind(&ctx, "ETHW")).unwrap().is_some());
}

#[test]
fn a_v2_activation_without_a_configured_value_waits_for_three_confirmations() {
    // CRD ch.35 R35.1.1, erratum (g): the mmrpc 2.0 default is 3 (the legacy `enable` keeps 1).
    let ctx = iguana_ctx();
    let node = healthy_node();
    let mut req = request_json(
        std::slice::from_ref(&node.url),
        json!({ "swap_contract_address": null }),
    );
    req["ticker"] = json!("ETHW");
    let req: Req = serde_json::from_value(req).unwrap();
    activate(&ctx, req).unwrap_or_else(|e| panic!("activation failed: {}", e));
    match block_on(lp_coinfind(&ctx, "ETHW")).unwrap().unwrap() {
        coins::MmCoinEnum::EthCoin(coin) => assert_eq!(coin.required_confirmations(), 3),
        other => panic!("expected an EVM coin, got {other:?}"),
    }
}

#[test]
fn missing_and_empty_nodes_are_at_least_one_node_required() {
    let ctx = iguana_ctx();
    let empty = request(&[], json!({}));
    assert_string_error(&failure(&ctx, empty), "AtLeastOneNodeRequired", StatusCode::BAD_REQUEST);
    let mut absent = request_json(&[], json!({}));
    absent.as_object_mut().unwrap().remove("nodes");
    let absent: Req = serde_json::from_value(absent).unwrap();
    assert_string_error(
        &failure(&ctx, absent),
        "AtLeastOneNodeRequired",
        StatusCode::BAD_REQUEST,
    );
}

#[test]
fn absent_token_requests_and_malformed_fields_fail_to_parse() {
    let mut absent = request_json(&[], json!({}));
    absent.as_object_mut().unwrap().remove("erc20_tokens_requests");
    assert!(serde_json::from_value::<Req>(absent).is_err());
    for extra in [
        json!({ "mm2": "one" }),
        json!({ "mm2": 300 }),
        json!({ "swap_contract_address": "0xnothex" }),
        json!({ "swap_gas_fee_policy": "Ludicrous" }),
        json!({ "required_confirmations": "three" }),
        json!({ "swap_v2_contracts": { "maker_swap_v2_contract": SWAP_CONTRACT } }),
    ] {
        let parsed = serde_json::from_value::<Req>(request_json(&[], extra.clone()));
        assert!(parsed.is_err(), "{:?} must be a request-parse failure", extra);
    }
}

#[test]
fn malformed_unconsumed_fields_are_treated_as_absent() {
    for extra in [
        json!({ "tx_history": "yes" }),
        json!({ "requires_notarization": 7 }),
        json!({ "gas_station_url": 5 }),
        json!({ "gas_station_decimals": "eight" }),
        json!({ "gas_station_policy": { "policy": "Cheapest" } }),
    ] {
        let parsed = serde_json::from_value::<Req>(request_json(&[], extra.clone()));
        let parsed = parsed.unwrap_or_else(|e| panic!("{:?} must be ignored: {}", extra, e));
        assert!(!parsed.request.tx_history());
    }
    let well_formed = serde_json::from_value::<Req>(request_json(
        &[],
        json!({
            "tx_history": true,
            "gas_station_url": "https://gas.example/api",
            "gas_station_decimals": 9,
            "gas_station_policy": { "policy": "Average" },
        }),
    ))
    .unwrap();
    assert!(well_formed.request.tx_history());
    assert_eq!(well_formed.request.gas_station_decimals, Some(9));
    assert!(well_formed.request.gas_station_policy.is_some());
}

// ---------------------------------------------------------------------------
// T35.6, T35.9: the signing policies
// ---------------------------------------------------------------------------

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn the_trezor_policy_on_the_one_shot_call_is_unexpected_device_activation_policy() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let req = request(
        std::slice::from_ref(&node.url),
        json!({ "priv_key_policy": { "type": "Trezor" } }),
    );
    let (wire, status) = failure(&ctx, req);
    assert_eq!(wire["error_type"], "UnexpectedDeviceActivationPolicy");
    assert!(wire.get("error_data").is_none(), "no payload: {:?}", wire);
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn metamask_and_string_form_policies_do_not_parse_on_native() {
    assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "rpc_mode": "Metamask" }))).is_err());
    assert!(
        serde_json::from_value::<Req>(request_json(&[], json!({ "priv_key_policy": { "type": "Metamask" } }))).is_err()
    );
    // A bare string is not a policy.
    assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "priv_key_policy": "Trezor" }))).is_err());
    assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "priv_key_policy": "ContextPrivKey" }))).is_err());
    // The default, the object forms, the aliases and the payload form are accepted.
    for policy in [
        json!({ "type": "ContextPrivKey" }),
        json!({ "type": "Iguana" }),
        json!({ "type": "IguanaPrivKey" }),
        json!({ "type": "WalletConnect", "params": { "session_topic": "ab12" } }),
    ] {
        let parsed = serde_json::from_value::<Req>(request_json(&[], json!({ "priv_key_policy": policy })));
        assert!(parsed.is_ok(), "{:?}: {:?}", policy, parsed.err());
    }
    assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "rpc_mode": "Default" }))).is_ok());
    // A WalletConnect session needs its topic.
    assert!(serde_json::from_value::<Req>(request_json(
        &[],
        json!({ "priv_key_policy": { "type": "WalletConnect" } })
    ))
    .is_err());
}

#[test]
fn the_wallet_connect_policy_is_a_wallet_connect_error() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let policy = json!({ "type": "WalletConnect", "params": { "session_topic": "ab12" } });
    let wire = failure(
        &ctx,
        request(std::slice::from_ref(&node.url), json!({ "priv_key_policy": policy })),
    );
    assert_string_error(&wire, "WalletConnectError", StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// The platform and token errors (R35.1.5)
// ---------------------------------------------------------------------------

#[test]
fn platform_configuration_errors_serialize_as_bound() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let unknown = {
        let mut req = request_json(std::slice::from_ref(&node.url), json!({}));
        req["ticker"] = json!("NOSUCH");
        serde_json::from_value::<Req>(req).unwrap()
    };
    assert_string_error(
        &failure(&ctx, unknown),
        "PlatformConfigIsNotFound",
        StatusCode::BAD_REQUEST,
    );

    // A platform that is not an EVM platform.
    let kmd = {
        let mut req = request_json(std::slice::from_ref(&node.url), json!({}));
        req["ticker"] = json!("KMD");
        serde_json::from_value::<Req>(req).unwrap()
    };
    let (wire, status) = failure(&ctx, kmd);
    assert_eq!(wire["error_type"], "UnexpectedPlatformProtocol");
    assert_eq!(wire["error_data"]["ticker"], "KMD");
    assert_eq!(wire["error_data"]["protocol"]["type"], "UTXO");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An EVM protocol without an integer chain id.
    let no_chain = {
        let mut req = request_json(std::slice::from_ref(&node.url), json!({}));
        req["ticker"] = json!("NOCHAIN");
        serde_json::from_value::<Req>(req).unwrap()
    };
    let (wire, status) = failure(&ctx, no_chain);
    assert_eq!(wire["error_type"], "CoinProtocolParseError");
    assert_eq!(wire["error_data"]["ticker"], "NOCHAIN");
    assert!(wire["error_data"]["error"].is_string());
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn an_active_ticker_is_platform_is_already_activated() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    activate(&ctx, request(std::slice::from_ref(&node.url), json!({}))).unwrap();
    let wire = failure(&ctx, request(std::slice::from_ref(&node.url), json!({})));
    assert_string_error(&wire, "PlatformIsAlreadyActivated", StatusCode::BAD_REQUEST);
    assert_eq!(wire.0["error_data"], PLATFORM);
}

#[test]
fn token_errors_serialize_as_bound() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let urls = [node.url.clone()];

    let missing = request(&urls, json!({ "erc20_tokens_requests": tokens(&["NOSUCH-ERC20"]) }));
    let wire = failure(&ctx, missing);
    assert_string_error(&wire, "TokenConfigIsNotFound", StatusCode::BAD_REQUEST);
    assert_eq!(wire.0["error_data"], "NOSUCH-ERC20");

    // A token of another platform.
    let mismatch = request(&urls, json!({ "erc20_tokens_requests": tokens(&["OTHER-ERC20"]) }));
    let (wire, status) = failure(&ctx, mismatch);
    assert_eq!(wire["error_type"], "PlatformCoinMismatch");
    assert!(wire.get("error_data").is_none(), "no payload: {:?}", wire);
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A configured contract address that is not in the exact EIP-55 form.
    let unchecked = request(&urls, json!({ "erc20_tokens_requests": tokens(&["BAD-ERC20"]) }));
    let (wire, status) = failure(&ctx, unchecked);
    assert_eq!(wire["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(wire["error_data"]["ticker"], "BAD-ERC20");
    assert_eq!(wire["error_data"]["protocol"]["type"], "ERC20");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A token that is not an ERC-20 at all.
    let not_erc20 = request(&urls, json!({ "erc20_tokens_requests": tokens(&["KMD"]) }));
    assert_eq!(failure(&ctx, not_erc20).0["error_type"], "UnexpectedTokenProtocol");
}

#[test]
fn the_first_failing_token_of_the_earliest_failing_pass_decides() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    // Pass (a) fails on the second token although the first token would fail pass (b).
    let req = request(
        std::slice::from_ref(&node.url),
        json!({ "erc20_tokens_requests": tokens(&["OTHER-ERC20", "NOSUCH-ERC20"]) }),
    );
    assert_eq!(failure(&ctx, req).0["error_type"], "TokenConfigIsNotFound");
    // Pass (b) fails before pass (c) can read the decimals of the mismatching token on-chain.
    let req = request(
        std::slice::from_ref(&node.url),
        json!({ "erc20_tokens_requests": tokens(&["DAI-ERC20", "OTHER-ERC20"]) }),
    );
    assert_eq!(failure(&ctx, req).0["error_type"], "PlatformCoinMismatch");
}

#[test]
fn custom_token_rejections_serialize_externally_tagged() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let custom = |ticker: &str, contract: &str| {
        json!({
            "ticker": ticker,
            "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": contract } },
        })
    };
    let cases = [
        (
            custom("USDT-ERC20", DAI_CONTRACT),
            json!({ "DuplicateTickerInConfig": { "ticker_in_config": "USDT-ERC20" } }),
        ),
        (
            custom("NEW-ERC20", USDT_CONTRACT),
            json!({ "DuplicateContractInConfig": { "ticker_in_config": "USDT-ERC20" } }),
        ),
        (custom("NEW-ERC20", "0xnothex"), json!("InvalidTokenAddress")),
    ];
    for (entry, expected) in cases {
        let req = request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": [entry] }),
        );
        let (wire, status) = failure(&ctx, req);
        assert_eq!(wire["error_type"], "CustomTokenError");
        assert_eq!(wire["error_data"], expected);
        // The platform set answers 500 for a custom token rejection.
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[test]
fn a_custom_token_activates_and_a_second_one_with_the_same_contract_is_rejected() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let custom = json!({
        "ticker": "CUSTOM-ERC20",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56" } },
    });
    let result = activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": [custom.clone()] }),
        ),
    )
    .unwrap_or_else(|e| panic!("the custom token must activate: {}", e));
    let result = serde_json::to_value(&result).unwrap();
    let balances = &result["erc20_addresses_infos"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["balances"];
    assert!(balances["CUSTOM-ERC20"].is_object(), "{}", result);

    // Another platform activation cannot reuse the contract: ETHW has no tokens, so use the same platform
    // through a second custom token on the active coin set instead.
    let again = find_error_for_second_custom_token(&ctx, &node);
    assert_eq!(again["error_type"], "CustomTokenError");
    assert!(
        again["error_data"]["TokenWithSameContractAlreadyActivated"].is_object(),
        "{}",
        again
    );
}

/// CRD ch.35 R35.2.7: a custom token activated through `enable_eth_with_tokens` is a wallet-only coin although
/// it has no coins-config entry; a coins-config token keeps the wallet-only setting of its entry.
#[test]
fn a_custom_token_of_the_platform_activation_is_wallet_only_and_a_config_token_is_not() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let custom = json!({
        "ticker": "CUSTOM-ERC20",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56" } },
    });
    activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": [custom, { "ticker": "USDT-ERC20" }] }),
        ),
    )
    .unwrap_or_else(|e| panic!("the activation failed: {}", e));

    let wallet_only = |ticker: &str| {
        block_on(lp_coinfind(&ctx, ticker))
            .unwrap()
            .unwrap_or_else(|| panic!("{} is not active", ticker))
            .wallet_only(&ctx)
    };
    assert!(wallet_only("CUSTOM-ERC20"), "a custom token is wallet-only");
    assert!(!wallet_only("USDT-ERC20"), "a config token follows its config entry");
}

fn find_error_for_second_custom_token(ctx: &MmArc, node: &MockEvmNode) -> Json {
    let mut req = request_json(
        std::slice::from_ref(&node.url),
        json!({
            "erc20_tokens_requests": [{
                "ticker": "OTHER-CUSTOM",
                "protocol": { "type": "ERC20", "protocol_data": { "platform": "ETHW", "contract_address": "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56" } },
            }],
            "swap_contract_address": null,
        }),
    );
    req["ticker"] = json!("ETHW");
    let req: Req = serde_json::from_value(req).unwrap();
    // The active token lives on ETH; the contract is looked up per platform, so the same contract on ETHW is a
    // different token and activates. The duplicate check is exercised on the same platform below.
    let _ = req;
    let token_request = serde_json::from_value::<crate::erc20_token_activation::EnableEvmTokenRequest>(json!({
        "ticker": "SECOND-CUSTOM",
        "protocol": { "type": "ERC20", "protocol_data": { "platform": PLATFORM, "contract_address": "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56" } },
        "activation_params": {},
    }))
    .unwrap();
    let error = error_of(
        block_on(crate::erc20_token_activation::enable_evm_token(
            ctx.clone(),
            token_request,
        )),
        "the same contract is already active",
    );
    serde_json::to_value(error.get_inner()).unwrap()
}

// ---------------------------------------------------------------------------
// T35.8: the reserved private-key policy error
// ---------------------------------------------------------------------------

#[test]
fn priv_key_policy_not_allowed_serializes_under_its_exact_name() {
    let error = EnablePlatformCoinWithTokensError::PrivKeyPolicyNotAllowed("hardware wallet".into());
    let wire = serde_json::to_value(&error).unwrap();
    assert_eq!(wire["error_type"], "PrivKeyPolicyNotAllowed");
    assert!(wire["error_data"].is_string());
    assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn every_platform_error_serializes_with_the_bound_shape_and_status() {
    use CustomTokenError as Custom;
    let cases: Vec<(EnablePlatformCoinWithTokensError, &str, Option<Json>, StatusCode)> = vec![
        (
            ActivationErr::PlatformIsAlreadyActivated("ETH".into()),
            "PlatformIsAlreadyActivated",
            Some(json!("ETH")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::PlatformConfigIsNotFound("ETH".into()),
            "PlatformConfigIsNotFound",
            Some(json!("ETH")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::CoinProtocolParseError {
                ticker: "ETH".into(),
                error: "e".into(),
            },
            "CoinProtocolParseError",
            Some(json!({ "ticker": "ETH", "error": "e" })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::TokenConfigIsNotFound("T".into()),
            "TokenConfigIsNotFound",
            Some(json!("T")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::TokenProtocolParseError {
                ticker: "T".into(),
                error: "e".into(),
            },
            "TokenProtocolParseError",
            Some(json!({ "ticker": "T", "error": "e" })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::PlatformCoinMismatch,
            "PlatformCoinMismatch",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::CustomTokenError(Custom::InvalidTokenAddress),
            "CustomTokenError",
            Some(json!("InvalidTokenAddress")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::AtLeastOneNodeRequired("no nodes".into()),
            "AtLeastOneNodeRequired",
            Some(json!("no nodes")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::InvalidPayload("e".into()),
            "InvalidPayload",
            Some(json!("e")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::UnexpectedDeviceActivationPolicy,
            "UnexpectedDeviceActivationPolicy",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::WalletConnectError("e".into()),
            "WalletConnectError",
            Some(json!("e")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::PlatformCoinCreationError {
                ticker: "ETH".into(),
                error: "e".into(),
            },
            "PlatformCoinCreationError",
            Some(json!({ "ticker": "ETH", "error": "e" })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::UnexpectedDerivationMethod("e".into()),
            "UnexpectedDerivationMethod",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::PrivKeyPolicyNotAllowed("e".into()),
            "PrivKeyPolicyNotAllowed",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::FailedSpawningBalanceEvents("e".into()),
            "FailedSpawningBalanceEvents",
            Some(json!("e")),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::Transport("e".into()),
            "Transport",
            Some(json!("e")),
            StatusCode::BAD_GATEWAY,
        ),
        (
            ActivationErr::Internal("e".into()),
            "Internal",
            Some(json!("e")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            ActivationErr::NoSuchTask(7),
            "NoSuchTask",
            Some(json!(7)),
            StatusCode::BAD_REQUEST,
        ),
        (
            ActivationErr::TaskTimedOut {
                duration: std::time::Duration::new(300, 5),
            },
            "TaskTimedOut",
            Some(json!({ "duration": { "secs": 300, "nanos": 5 } })),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];
    for (error, error_type, error_data, status) in cases {
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(wire["error_type"], error_type);
        match error_data {
            Some(expected) => assert_eq!(wire["error_data"], expected, "{}", error_type),
            None => assert!(
                wire.get("error_data").is_none(),
                "{} carries no payload: {:?}",
                error_type,
                wire
            ),
        }
        assert_eq!(error.status_code(), status, "{}", error_type);
    }
}

// ---------------------------------------------------------------------------
// T35.10: the success shapes (R35.1.3)
// ---------------------------------------------------------------------------

#[test]
fn single_address_result_with_balances() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let result = activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20", "DAI-ERC20"]) }),
        ),
    )
    .unwrap_or_else(|e| panic!("activation failed: {}", e));
    let result = serde_json::to_value(&result).unwrap();

    assert_eq!(result["current_block"], 42);
    assert_eq!(result["nfts_infos"], json!({}));
    let eth_infos = result["eth_addresses_infos"].as_object().unwrap();
    assert_eq!(eth_infos.len(), 1);
    let (address, eth_info) = eth_infos.iter().next().unwrap();
    assert!(address.starts_with("0x") && address.len() == 42);
    assert_eq!(eth_info["derivation_method"], json!({ "type": "Iguana" }));
    let pubkey = eth_info["pubkey"].as_str().unwrap();
    assert_eq!(pubkey.len(), 130, "04 + the 64-byte key in hex");
    assert!(pubkey.starts_with("04") && !pubkey.starts_with("0x"));
    assert_eq!(pubkey, pubkey.to_lowercase());
    assert_eq!(eth_info["balances"]["spendable"], "1.5");
    assert_eq!(eth_info["balances"]["unspendable"], "0");
    assert!(eth_info.get("tickers").is_none());

    let erc20_info = &result["erc20_addresses_infos"][address];
    assert_eq!(erc20_info["pubkey"], pubkey);
    let balances = erc20_info["balances"].as_object().unwrap();
    assert_eq!(balances["USDT-ERC20"]["spendable"], "2");
    assert!(balances.contains_key("DAI-ERC20"));
    assert!(erc20_info.get("tickers").is_none());
}

#[test]
fn single_address_result_without_balances_lists_the_token_tickers() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let result = activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({
                "erc20_tokens_requests": tokens(&["USDT-ERC20", "DAI-ERC20"]),
                "get_balances": false,
            }),
        ),
    )
    .unwrap_or_else(|e| panic!("activation failed: {}", e));
    let result = serde_json::to_value(&result).unwrap();

    assert_eq!(result["nfts_infos"], json!({}));
    let (address, eth_info) = result["eth_addresses_infos"]
        .as_object()
        .unwrap()
        .iter()
        .next()
        .unwrap();
    assert!(eth_info.get("balances").is_none() && eth_info.get("tickers").is_none());
    let erc20_info = &result["erc20_addresses_infos"][address];
    assert!(erc20_info.get("balances").is_none());
    let mut tickers: Vec<String> = serde_json::from_value(erc20_info["tickers"].clone()).unwrap();
    tickers.sort();
    assert_eq!(tickers, vec!["DAI-ERC20".to_owned(), "USDT-ERC20".to_owned()]);
}

#[test]
fn token_confirmations_come_from_the_request_then_the_token_config_then_the_platform() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let token_requests = json!([
        { "ticker": "USDT-ERC20" },
        { "ticker": "DAI-ERC20" },
    ]);
    activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": token_requests }),
        ),
    )
    .unwrap();
    let confirmations = |ticker: &str| match block_on(lp_coinfind(&ctx, ticker)).unwrap().unwrap() {
        coins::MmCoinEnum::EthCoin(coin) => coin.required_confirmations(),
        other => panic!("not an EVM coin: {}", other.ticker()),
    };
    assert_eq!(confirmations(PLATFORM), 2, "the platform config");
    assert_eq!(confirmations("USDT-ERC20"), 4, "the token config");
    assert_eq!(confirmations("DAI-ERC20"), 2, "the platform's effective value");

    let ctx = iguana_ctx();
    let token_requests = json!([{ "ticker": "USDT-ERC20", "required_confirmations": 9 }]);
    activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": token_requests, "required_confirmations": 7 }),
        ),
    )
    .unwrap();
    let confirmations = |ticker: &str| match block_on(lp_coinfind(&ctx, ticker)).unwrap().unwrap() {
        coins::MmCoinEnum::EthCoin(coin) => coin.required_confirmations(),
        other => panic!("not an EVM coin: {}", other.ticker()),
    };
    assert_eq!(confirmations(PLATFORM), 7);
    assert_eq!(confirmations("USDT-ERC20"), 9);
}

#[test]
fn an_hd_daemon_activates_the_address_at_the_coin_derivation_path() {
    use coins::eth::derive_eth_hd_activation_key;
    use crypto::KeyPairPolicy;

    let ctx = hd_ctx();
    let crypto_ctx = CryptoCtx::from_ctx(&ctx).unwrap();
    let global_hd = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        KeyPairPolicy::Iguana => panic!("an HD context is expected"),
    };

    // The address the coin's derivation path and `path_to_address` select, and the address of the
    // daemon-internal identity key that the activation used to bind instead.
    let conf = coins::coin_conf(&ctx, PLATFORM);
    let selected = |path: PathToAddress| {
        derive_eth_hd_activation_key(&global_hd, &conf, &path)
            .unwrap()
            .address()
    };
    let path_address = selected(PathToAddress::default());
    let identity_address = {
        // The same mnemonic as an Iguana passphrase yields the daemon-internal identity key.
        let identity_ctx = MmCtxBuilder::new()
            .with_conf(json!({ "coins": coins_conf() }))
            .into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(identity_ctx.clone(), TEST_MNEMONIC).unwrap();
        assert!(
            identity_ctx.secp256k1_key_pair().private().secret == ctx.secp256k1_key_pair().private().secret,
            "the key the activation used to bind is the identity key of the HD daemon"
        );
        let request = serde_json::from_value(json!({ "coin": PLATFORM })).unwrap();
        let response = block_on(coins::rpc_command::get_my_address::get_my_address_rpc(
            identity_ctx,
            request,
        ))
        .unwrap_or_else(|e| panic!("get_my_address failed: {}", e));
        response.wallet_address
    };
    let path_address_str = coins::eth::checksum_address(&format!("{:#x}", path_address));
    assert_ne!(
        identity_address, path_address_str,
        "the identity key is not the key at the coin derivation path, so binding it was a defect"
    );

    // The node reports a balance for the first address only, so the HD scan stops at the gap limit.
    let funded = format!("{:#x}", path_address);
    let node = spawn_node(move |method, params| {
        let queried = params[0].as_str().map(str::to_lowercase);
        match method {
            "eth_getBalance" if queried.as_deref() == Some(funded.as_str()) => {
                Reply::Result(json!("0xde0b6b3a7640000"))
            },
            "eth_getBalance" => Reply::Result(json!("0x0")),
            _ => healthy_node_reply(method, params, 0, 5_000_000),
        }
    });

    let result = activate(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20"]) }),
        ),
    )
    .unwrap_or_else(|e| panic!("the HD activation failed: {}", e));
    let result = serde_json::to_value(&result).unwrap();

    // The HD shape (R35.1.3(b)).
    assert_eq!(result["current_block"], 42);
    assert_eq!(result["ticker"], PLATFORM);
    assert_eq!(result["nfts_infos"], json!({}));
    assert!(result.get("eth_addresses_infos").is_none());
    assert_eq!(result["wallet_balance"]["wallet_type"], "HD");
    let account = &result["wallet_balance"]["accounts"][0];
    assert_eq!(account["account_index"], 0);
    assert_eq!(account["derivation_path"], "m/44'/60'/0'");
    let first = &account["addresses"][0];
    assert_eq!(
        first["address"],
        coins::eth::checksum_address(&format!("{:#x}", path_address))
    );
    assert_eq!(first["derivation_path"], "m/44'/60'/0'/0/0");
    assert_eq!(first["chain"], "External");
    // Every balance map covers the platform ticker and every activated token ticker.
    let spendable = |ticker: &str| {
        use std::str::FromStr;
        BigDecimal::from_str(
            first["balance"][ticker]["spendable"]
                .as_str()
                .expect("a decimal string"),
        )
        .unwrap()
    };
    assert_eq!(spendable("ETH"), BigDecimal::from(1));
    assert_eq!(spendable("USDT-ERC20"), BigDecimal::from(5));
    assert!(account["total_balance"]["ETH"].is_object() && account["total_balance"]["USDT-ERC20"].is_object());

    // The registered coin is bound to the address at the derivation path.
    match block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().unwrap() {
        coins::MmCoinEnum::EthCoin(coin) => {
            assert_eq!(coin.enabled_address(), path_address);
            assert!(matches!(coin.derivation_method, coins::DerivationMethod::HDWallet(_)));
        },
        other => panic!("not an EVM coin: {}", other.ticker()),
    }
}

#[test]
fn path_to_address_selects_the_enabled_address_of_an_hd_daemon() {
    use coins::eth::derive_eth_hd_activation_key;
    use crypto::KeyPairPolicy;

    let ctx = hd_ctx();
    let crypto_ctx = CryptoCtx::from_ctx(&ctx).unwrap();
    let global_hd = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        KeyPairPolicy::Iguana => panic!("an HD context is expected"),
    };
    let conf = coins::coin_conf(&ctx, PLATFORM);
    let path = PathToAddress {
        account_id: 0,
        chain: crypto::Bip44Chain::External,
        address_id: 3,
    };
    let expected = derive_eth_hd_activation_key(&global_hd, &conf, &path)
        .unwrap()
        .address();

    let node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 0),
    });
    let req = request(
        std::slice::from_ref(&node.url),
        json!({
            "path_to_address": { "account_id": 0, "chain": "External", "address_id": 3 },
            "scan_policy": "do_not_scan",
            "min_addresses_number": 4,
        }),
    );
    let result = activate(&ctx, req).unwrap_or_else(|e| panic!("the HD activation failed: {}", e));
    // The report covers the enabled address, and every address is the full EIP-55 string.
    let result = serde_json::to_value(&result).unwrap();
    let addresses = result["wallet_balance"]["accounts"][0]["addresses"].as_array().unwrap();
    assert_eq!(
        addresses.len(),
        4,
        "the requested minimum, which also covers the enabled address"
    );
    for address in addresses {
        let address = address["address"].as_str().unwrap();
        assert_eq!(address.len(), 42, "{}", address);
        assert_eq!(address, coins::eth::checksum_address(address));
    }
    assert_eq!(
        addresses[3]["address"],
        coins::eth::checksum_address(&format!("{:#x}", expected))
    );
    match block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().unwrap() {
        coins::MmCoinEnum::EthCoin(coin) => assert_eq!(coin.enabled_address(), expected),
        other => panic!("not an EVM coin: {}", other.ticker()),
    }
}

// ---------------------------------------------------------------------------
// T35.11: the inline NFT activation (R35.1.4b)
// ---------------------------------------------------------------------------

#[test]
fn nft_req_is_accepted_bare_and_wrapped() {
    let provider = json!({ "type": "Moralis", "info": { "url": "https://indexer.example.com/" } });
    for nft_req in [provider.clone(), json!({ "provider": provider.clone() })] {
        let parsed = serde_json::from_value::<Req>(request_json(&[], json!({ "nft_req": nft_req.clone() })));
        let parsed = parsed.unwrap_or_else(|e| panic!("{} must parse: {}", nft_req, e));
        assert!(parsed.request.nft_req.is_some());
    }
    // An unknown provider type or an invalid URL is a request-parse failure.
    for bad in [
        json!({ "type": "Unknown", "info": { "url": "https://indexer.example.com/" } }),
        json!({ "type": "Moralis", "info": { "url": "not a url" } }),
        json!({ "provider": { "type": "Unknown", "info": { "url": "https://indexer.example.com/" } } }),
    ] {
        assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "nft_req": bad }))).is_err());
    }
}

#[test]
fn the_inline_nft_activation_is_unavailable_for_an_hd_platform() {
    let ctx = hd_ctx();
    let node = spawn_node(|method, params| match method {
        "eth_getBalance" => Reply::Result(json!("0x0")),
        _ => healthy_node_reply(method, params, 0, 0),
    });
    let provider = json!({ "type": "Moralis", "info": { "url": "https://indexer.example.com/" } });
    let (wire, status) = failure(
        &ctx,
        request(std::slice::from_ref(&node.url), json!({ "nft_req": provider })),
    );
    assert_eq!(wire["error_type"], "UnexpectedDerivationMethod");
    assert!(wire["error_data"].is_string());
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none(),
        "a failure leaves nothing registered"
    );
}

#[test]
fn an_unreachable_nft_provider_fails_the_whole_activation_with_transport() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let provider = json!({ "type": "Moralis", "info": { "url": refusing_url() } });
    let wire = failure(
        &ctx,
        request(
            std::slice::from_ref(&node.url),
            json!({ "nft_req": { "provider": provider } }),
        ),
    );
    assert_string_error(&wire, "Transport", StatusCode::BAD_GATEWAY);
    assert!(
        block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none(),
        "a failure leaves nothing registered"
    );
}

// ---------------------------------------------------------------------------
// T35.14, T35.15: the task variant (R35.3)
// ---------------------------------------------------------------------------

fn poll_terminal(ctx: &MmArc, task_id: u64) -> Json {
    for _ in 0..400 {
        let status = block_on(init_platform_coin_with_tokens_status::<EthCoin>(
            ctx.clone(),
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": task_id, "forget_if_finished": false }))
                .unwrap(),
        ))
        .unwrap_or_else(|e| panic!("status failed: {}", e));
        let status = serde_json::to_value(&status).unwrap();
        if matches!(status["status"].as_str(), Some("Ok") | Some("Error")) {
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("the task did not finish");
}

#[test]
fn task_init_on_an_active_ticker_is_an_mmrpc_level_platform_is_already_activated() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    activate(&ctx, request(std::slice::from_ref(&node.url), json!({}))).unwrap();
    let error = error_of(
        block_on(init_platform_coin_with_tokens::<EthCoin>(
            ctx.clone(),
            request(std::slice::from_ref(&node.url), json!({})),
        )),
        "init must fail synchronously",
    );
    let wire = wire(&error);
    assert_string_error(&wire, "PlatformIsAlreadyActivated", StatusCode::BAD_REQUEST);
    assert_eq!(wire.0["error_data"], PLATFORM);
}

#[test]
fn task_configuration_failures_are_status_level() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let mut req = request_json(std::slice::from_ref(&node.url), json!({}));
    req["ticker"] = json!("NOSUCH");
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        serde_json::from_value::<Req>(req).unwrap(),
    ))
    .unwrap_or_else(|e| panic!("init must hand out a task id: {}", e));

    let status = poll_terminal(&ctx, response.task_id);
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "PlatformConfigIsNotFound");
    assert_eq!(status["details"]["error_data"], "NOSUCH");
    // The error object sits directly in `details`.
    assert!(status["details"].get("result").is_none());
}

#[test]
fn task_result_is_the_one_shot_result() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        request(
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-ERC20"]) }),
        ),
    ))
    .unwrap_or_else(|e| panic!("init failed: {}", e));
    let status = poll_terminal(&ctx, response.task_id);
    assert_eq!(status["status"], "Ok", "{}", status);
    let details = &status["details"];
    assert_eq!(details["current_block"], 42);
    assert_eq!(details["nfts_infos"], json!({}));
    assert!(details["eth_addresses_infos"].is_object() && details["erc20_addresses_infos"].is_object());
}

#[test]
fn task_failures_carry_the_one_shot_error_inside_a_status_answer() {
    let ctx = iguana_ctx();
    let gateway = bad_gateway_node();
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        request(std::slice::from_ref(&gateway.url), json!({})),
    ))
    .unwrap_or_else(|e| panic!("init failed: {}", e));
    let status = poll_terminal(&ctx, response.task_id);
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "Transport");
    assert!(status["details"]["error_data"].is_string());
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn the_trezor_task_without_a_hardware_context_is_a_status_level_internal() {
    let ctx = iguana_ctx();
    let node = healthy_node();
    let response = block_on(init_platform_coin_with_tokens::<EthCoin>(
        ctx.clone(),
        request(
            std::slice::from_ref(&node.url),
            json!({ "priv_key_policy": { "type": "Trezor" } }),
        ),
    ))
    .unwrap_or_else(|e| panic!("init must hand out a task id: {}", e));
    let status = poll_terminal(&ctx, response.task_id);
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "Internal");
    assert!(status["details"]["error_data"].is_string());
    assert!(block_on(lp_coinfind(&ctx, PLATFORM)).unwrap().is_none());
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn the_progress_vocabulary_names_the_device_confirmation_follow_hw_device_instructions() {
    use crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensInProgressStatus as Status;

    for (status, name) in [
        (Status::ActivatingCoin, "ActivatingCoin"),
        (Status::RequestingWalletBalance, "RequestingWalletBalance"),
        (Status::Finishing, "Finishing"),
        (Status::WaitingForTrezorToConnect, "WaitingForTrezorToConnect"),
        (Status::FollowHwDeviceInstructions, "FollowHwDeviceInstructions"),
    ] {
        assert_eq!(serde_json::to_value(&status).unwrap(), json!(name));
    }
    // The device button request maps onto the bound value.
    let statuses = crate::init_platform_coin_with_tokens::xpub_extractor_rpc_statuses();
    assert_eq!(
        serde_json::to_value(&statuses.on_button_request).unwrap(),
        json!("FollowHwDeviceInstructions")
    );
}

#[test]
fn cancel_of_an_unknown_task_is_no_such_task_404() {
    use common::HttpStatusCode as _;
    let ctx = iguana_ctx();
    let error = error_of(
        block_on(cancel_init_platform_coin_with_tokens::<EthCoin>(
            ctx,
            serde_json::from_value::<RpcTaskStatusRequest>(json!({ "task_id": 99 })).unwrap(),
        )),
        "an unknown task",
    );
    assert_eq!(
        serde_json::to_value(error.get_inner()).unwrap(),
        json!({ "error_type": "NoSuchTask", "error_data": 99 })
    );
    assert_eq!(error.get_inner().status_code(), StatusCode::NOT_FOUND);
}

/// The initial swap gas-fee policy of a coin comes from the activation request, else from the
/// platform coin's configuration entry, else it is left unset (the coin then starts as `Legacy`).
/// A coins file that sets `swap_gas_fee_policy` therefore changes the transaction form of every
/// swap send of that platform and its tokens without any RPC parameter naming it (CRD R35.1.1).
#[test]
fn initial_swap_gas_fee_policy_precedence_is_request_then_coin_config() {
    use coins::eth::SwapGasFeePolicy;
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let policy_of = |conf: Json, extra: Json| {
        let req = request(&[], extra).request;
        activation_params(&ctx, &conf, &req)
            .unwrap_or_else(|e| panic!("activation_params: {}", e))
            .swap_gas_fee_policy
    };
    assert_eq!(policy_of(json!({}), json!({})), None);
    assert_eq!(
        policy_of(json!({ "swap_gas_fee_policy": "Medium" }), json!({})),
        Some(SwapGasFeePolicy::Medium)
    );
    assert_eq!(
        policy_of(
            json!({ "swap_gas_fee_policy": "Medium" }),
            json!({ "swap_gas_fee_policy": "Legacy" })
        ),
        Some(SwapGasFeePolicy::Legacy)
    );
}

/// CRD R35.1.1 against R55.4.4: the v2 one-shot and the task route share one request type and **reject**
/// an unrecognised `swap_gas_fee_policy`, where the legacy `enable` route ignores it. The difference is the
/// specified behavior, so the two parses must stay separate.
#[test]
fn the_v2_and_task_routes_reject_an_unrecognised_swap_gas_fee_policy() {
    for bad in [json!("Ludicrous"), json!("medium"), json!(5)] {
        let raw = request_json(&[], json!({ "swap_gas_fee_policy": bad.clone() }));
        assert!(
            serde_json::from_value::<Req>(raw.clone()).is_err(),
            "{bad} must fail to parse on the v2 and task routes"
        );
        let task: serde_json::Result<EnablePlatformCoinWithTokensReq<EthWithTokensActivationRequest>> =
            serde_json::from_value(raw);
        assert!(task.is_err());
    }
    for good in ["Legacy", "Low", "Medium", "High"] {
        assert!(serde_json::from_value::<Req>(request_json(&[], json!({ "swap_gas_fee_policy": good }))).is_ok());
    }
}

/// T35.36: the debug form of the parsed request names every node and the gas station host-only.
#[test]
fn t35_36_the_debug_form_of_the_request_is_host_only() {
    let node = "https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X".to_owned();
    let req: Req = serde_json::from_value(request_json(
        &[node],
        json!({ "gas_station_url": "https://gas.example.org/v3/GASKEY?key=Y" }),
    ))
    .unwrap();
    let rendered = format!("{req:?}");
    assert!(rendered.contains("https://rpc.example.org:8545"), "{rendered}");
    assert!(rendered.contains("https://gas.example.org"), "{rendered}");
    for secret in ["user", "secret", "/v3/", "APIKEY", "GASKEY", "token=", "key=Y"] {
        assert!(!rendered.contains(secret), "a credential part is rendered");
    }
}
