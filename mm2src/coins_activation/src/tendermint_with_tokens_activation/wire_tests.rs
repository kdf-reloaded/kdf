//! Wire-shape tests of `enable_tendermint_with_assets`, `enable_tendermint_token` and
//! `task::enable_tendermint` (CRD ch. 36 §36.1, §36.2, §36.6; T36.1 -- T36.7, T36.10 -- T36.13).
//!
//! Every test serializes the error as the dispatcher would and asserts the `error_type`, the JSON shape of
//! `error_data` (or its absence) and the HTTP status. The activations run end to end; the nodes are ports nothing
//! listens on (or, for the in-progress test, a port that never answers).

use super::*;
use crate::evm_test_support::{coins_conf, refusing_url, spawn_node, Reply};
use crate::init_platform_coin_with_tokens::{cancel_init_platform_coin_with_tokens, init_platform_coin_with_tokens,
                                            init_platform_coin_with_tokens_status};
use crate::tendermint_token_activation::{enable_tendermint_token, EnableTendermintTokenRequest};
use crate::token::EnableTokenError;
use coins::{lp_coinfind, CoinsContext, MmCoin, MmCoinEnum};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmCtxBuilder;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use serde_json::json;
use std::net::TcpListener;
use std::str::FromStr;
use std::time::Duration;

const HD_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

type Req = EnablePlatformCoinWithTokensReq<TendermintActivationParams>;

const PLATFORM: &str = "IRIS";

fn tendermint_coins() -> Vec<Json> {
    vec![
        json!({
            "coin": PLATFORM,
            "avg_blocktime": 5,
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uiris", "decimals": 6, "account_prefix": "iaa", "chain_id": "irishub-1" } },
            "mm2": 1
        }),
        json!({
            "coin": "OSMO",
            "avg_blocktime": 6,
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uosmo", "decimals": 6, "account_prefix": "osmo", "chain_id": "osmosis-1" } },
            "mm2": 1
        }),
        json!({
            "coin": "HIGHDEC",
            "avg_blocktime": 5,
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uhigh", "decimals": 19, "account_prefix": "high", "chain_id": "high-1" } },
            "mm2": 1
        }),
        json!({
            "coin": "NOAVG",
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uno", "decimals": 6, "account_prefix": "no", "chain_id": "no-1" } },
            "mm2": 1
        }),
        json!({
            "coin": "ATOM",
            "avg_blocktime": 6,
            "derivation_path": "m/44'/118'",
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uatom", "decimals": 6, "account_prefix": "cosmos", "chain_id": "cosmoshub-4" } },
            "mm2": 1
        }),
        json!({
            "coin": "ATOM-NODERIV",
            "avg_blocktime": 6,
            "protocol": { "type": "TENDERMINT", "protocol_data": {
                "denom": "uatom", "decimals": 6, "account_prefix": "cosmos", "chain_id": "cosmoshub-4" } },
            "mm2": 1
        }),
        json!({
            "coin": "IRIS-NYAN",
            "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                "platform": PLATFORM, "decimals": 6, "denom": "unyan" } },
            "mm2": 1
        }),
        json!({
            "coin": "OSMO-TOKEN",
            "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                "platform": "OSMO", "decimals": 6, "denom": "uion" } },
            "mm2": 1
        }),
        json!({
            "coin": "ETH-TOKEN",
            "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                "platform": "ETH", "decimals": 6, "denom": "ueth" } },
            "mm2": 1
        }),
    ]
}

fn ctx() -> MmArc { ctx_with(tendermint_coins()) }

fn ctx_with(tendermint: Vec<Json>) -> MmArc {
    let mut coins = coins_conf().as_array().cloned().unwrap_or_default();
    coins.extend(tendermint);
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "tendermint activation test passphrase")
        .expect("initialize the crypto context");
    ctx
}

fn request_json(extra: Json) -> Json {
    let mut request = json!({
        "ticker": PLATFORM,
        "nodes": [{ "url": refusing_url() }],
        "tokens_params": [],
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

fn request(extra: Json) -> Req {
    serde_json::from_value(request_json(extra)).unwrap_or_else(|e| panic!("bad request: {}", e))
}

/// The error as the dispatcher serializes it: the adjacently tagged pair, and the HTTP status.
fn wire(error: &MmError<EnablePlatformCoinWithTokensError>) -> (Json, StatusCode) {
    (
        serde_json::to_value(error.get_inner()).expect("a serializable error"),
        error.get_inner().status_code(),
    )
}

fn failure(ctx: &MmArc, req: Req) -> (Json, StatusCode) {
    match block_on(enable_platform_coin_with_tokens::<TendermintCoin>(ctx.clone(), req)) {
        Ok(result) => panic!(
            "expected a failure, activated: {}",
            serde_json::to_value(&result).unwrap()
        ),
        Err(e) => wire(&e),
    }
}

/// A `PlatformCoinCreationError` whose `error_data` has exactly the string members `ticker` and `error`, with
/// HTTP 500.
fn assert_creation_error(wire: &(Json, StatusCode), ticker: &str) {
    assert_eq!(wire.0["error_type"], "PlatformCoinCreationError", "{:?}", wire.0);
    let data = wire.0["error_data"]
        .as_object()
        .unwrap_or_else(|| panic!("{:?}", wire.0));
    assert_eq!(data.len(), 2, "{:?}", wire.0);
    assert_eq!(data["ticker"], ticker);
    assert!(data["error"].is_string(), "{:?}", wire.0);
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
}

/// A unit variant: no `error_data` member at all.
fn assert_unit_error(wire: &(Json, StatusCode), error_type: &str, status: StatusCode) {
    assert_eq!(wire.0["error_type"], error_type, "{:?}", wire.0);
    assert!(wire.0.get("error_data").is_none(), "{:?}", wire.0);
    assert_eq!(wire.1, status);
}

fn assert_nothing_registered(ctx: &MmArc, tickers: &[&str]) {
    for ticker in tickers {
        assert!(
            block_on(lp_coinfind(ctx, ticker)).unwrap().is_none(),
            "{} registered",
            ticker
        );
    }
}

/// Registers the platform without contacting a node: building the coin needs none.
fn register_platform(ctx: &MmArc) -> TendermintCoin { register_platform_on(ctx, &refusing_url()) }

/// [`register_platform`] with the node URL given.
fn register_platform_on(ctx: &MmArc, node_url: &str) -> TendermintCoin {
    let conf = coins::coin_conf(ctx, PLATFORM);
    let priv_key = *ctx.legacy_passphrase_key_pair().private().secret;
    let coin = block_on(coins::tendermint::tendermint_coin_from_conf_and_request(
        ctx,
        PLATFORM.to_owned(),
        &conf,
        vec![serde_json::from_value(json!({ "url": node_url })).unwrap()],
        &priv_key,
        true,
    ))
    .unwrap_or_else(|e| panic!("cannot build the platform: {}", e));
    let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
    block_on(coins_ctx.add_coin(coin.clone().into())).unwrap();
    coin
}

// ---------------------------------------------------------------------------
// T36.1 -- T36.3: creation errors (R36.1.6)
// ---------------------------------------------------------------------------

#[test]
fn every_node_refusing_connections_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    let wire = failure(
        &ctx,
        request(json!({ "nodes": [{ "url": refusing_url() }, { "url": refusing_url() }] })),
    );
    assert_creation_error(&wire, PLATFORM);
    // Also without balances, and with an inline token: the chain height is read either way.
    let wire = failure(
        &ctx,
        request(json!({ "get_balances": false, "tokens_params": [{ "ticker": "IRIS-NYAN" }] })),
    );
    assert_creation_error(&wire, PLATFORM);
    assert_nothing_registered(&ctx, &[PLATFORM, "IRIS-NYAN"]);
}

#[test]
fn empty_nodes_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    assert_creation_error(&failure(&ctx, request(json!({ "nodes": [] }))), PLATFORM);
    assert_nothing_registered(&ctx, &[PLATFORM]);
}

#[test]
fn an_unusable_node_url_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    assert_creation_error(
        &failure(&ctx, request(json!({ "nodes": [{ "url": "this is not a url" }] }))),
        PLATFORM,
    );
}

#[test]
fn decimals_above_18_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    let wire = failure(&ctx, request(json!({ "ticker": "HIGHDEC" })));
    assert_creation_error(&wire, "HIGHDEC");
    assert_nothing_registered(&ctx, &["HIGHDEC"]);
}

#[test]
fn a_missing_avg_blocktime_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    assert_creation_error(&failure(&ctx, request(json!({ "ticker": "NOAVG" }))), "NOAVG");
}

#[test]
fn a_non_null_activation_params_is_a_platform_coin_creation_error() {
    let ctx = ctx();
    for activation_params in [
        json!({ "type": "WithPubkey", "params": { "pubkey": { "type": "secp256k1", "value": [] }, "is_ledger_connection": false } }),
        json!({ "type": "WalletConnect", "params": { "session_topic": "topic" } }),
        json!({ "priv_key_policy": { "type": "WalletConnect", "data": "topic" } }),
        json!("anything"),
    ] {
        let wire = failure(&ctx, request(json!({ "activation_params": activation_params })));
        assert_creation_error(&wire, PLATFORM);
        // The refusal is the signing-policy one, not a node failure.
        assert!(
            wire.0["error_data"]["error"]
                .as_str()
                .unwrap()
                .contains("activation_params"),
            "{:?}",
            wire.0
        );
    }
    assert_nothing_registered(&ctx, &[PLATFORM]);
}

#[test]
fn an_absent_or_null_activation_params_is_not_refused() {
    assert!(reject_unsupported_activation_params(PLATFORM, &None).is_ok());
    assert!(reject_unsupported_activation_params(PLATFORM, &Some(Json::Null)).is_ok());
    // End to end the request proceeds to the nodes, so the error is the node failure, not the policy refusal.
    let ctx = ctx();
    let wire = failure(&ctx, request(json!({ "activation_params": null })));
    assert_creation_error(&wire, PLATFORM);
    assert!(!wire.0["error_data"]["error"]
        .as_str()
        .unwrap()
        .contains("activation_params"));
}

// ---------------------------------------------------------------------------
// T36.4: tokens_params is required
// ---------------------------------------------------------------------------

#[test]
fn tokens_params_is_required() {
    let err = serde_json::from_value::<Req>(json!({ "ticker": PLATFORM, "nodes": [{ "url": "https://rpc.example" }] }))
        .err()
        .expect("a request without tokens_params must not parse");
    assert!(err.to_string().contains("tokens_params"), "{}", err);
    // An empty list is accepted.
    assert!(serde_json::from_value::<Req>(request_json(json!({ "tokens_params": [] }))).is_ok());
}

// ---------------------------------------------------------------------------
// T36.5, T36.7: token platform and protocol errors
// ---------------------------------------------------------------------------

#[test]
fn a_token_of_another_platform_is_platform_coin_mismatch() {
    let ctx = ctx();
    let wire = failure(&ctx, request(json!({ "tokens_params": [{ "ticker": "OSMO-TOKEN" }] })));
    assert_unit_error(&wire, "PlatformCoinMismatch", StatusCode::BAD_REQUEST);
    // A matching token next to the mismatching one does not let the request through either.
    let wire = failure(
        &ctx,
        request(json!({ "tokens_params": [{ "ticker": "IRIS-NYAN" }, { "ticker": "OSMO-TOKEN" }] })),
    );
    assert_unit_error(&wire, "PlatformCoinMismatch", StatusCode::BAD_REQUEST);
    assert_nothing_registered(&ctx, &[PLATFORM, "IRIS-NYAN", "OSMO-TOKEN"]);
}

#[test]
fn unexpected_platform_protocol_carries_the_canonical_protocol_form() {
    let ctx = ctx();
    // A protocol without protocol data omits the member.
    let wire = failure(&ctx, request(json!({ "ticker": "KMD" })));
    assert_eq!(wire.0["error_type"], "UnexpectedPlatformProtocol");
    assert_eq!(wire.0["error_data"]["ticker"], "KMD");
    assert_eq!(wire.0["error_data"]["protocol"], json!({ "type": "UTXO" }));
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
    // An ERC20 protocol carries its protocol data.
    let wire = failure(&ctx, request(json!({ "ticker": "USDT-ERC20" })));
    assert_eq!(wire.0["error_data"]["protocol"]["type"], "ERC20");
    assert_eq!(
        wire.0["error_data"]["protocol"]["protocol_data"]["platform"],
        json!("ETH")
    );
    assert!(wire.0["error_data"]["protocol"]["protocol_data"]["contract_address"].is_string());
    // A TENDERMINTTOKEN protocol carries its protocol data.
    let wire = failure(&ctx, request(json!({ "ticker": "IRIS-NYAN" })));
    assert_eq!(
        wire.0["error_data"]["protocol"],
        json!({ "type": "TENDERMINTTOKEN", "protocol_data": { "platform": PLATFORM, "denom": "unyan", "decimals": 6 } })
    );
}

#[test]
fn unexpected_token_protocol_carries_the_canonical_protocol_form() {
    let ctx = ctx();
    let wire = failure(&ctx, request(json!({ "tokens_params": [{ "ticker": "KMD" }] })));
    assert_eq!(wire.0["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(wire.0["error_data"]["ticker"], "KMD");
    assert_eq!(wire.0["error_data"]["protocol"], json!({ "type": "UTXO" }));
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
    let wire = failure(&ctx, request(json!({ "tokens_params": [{ "ticker": "OSMO" }] })));
    assert_eq!(wire.0["error_data"]["protocol"]["type"], "TENDERMINT");
    assert_eq!(
        wire.0["error_data"]["protocol"]["protocol_data"]["account_prefix"],
        json!("osmo")
    );
}

// ---------------------------------------------------------------------------
// T36.10, T36.11: enable_tendermint_token
// ---------------------------------------------------------------------------

fn token_request(ticker: &str) -> EnableTendermintTokenRequest {
    serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).unwrap()
}

fn token_request_with(ticker: &str, protocol: Json) -> EnableTendermintTokenRequest {
    serde_json::from_value(json!({ "ticker": ticker, "activation_params": {}, "protocol": protocol })).unwrap()
}

fn token_wire(error: &MmError<EnableTokenError>) -> (Json, StatusCode) {
    let e: &EnableTokenError = error.get_inner();
    (serde_json::to_value(e).expect("a serializable error"), e.status_code())
}

fn token_failure_of(ctx: &MmArc, req: EnableTendermintTokenRequest) -> (Json, StatusCode) {
    match block_on(enable_tendermint_token(ctx.clone(), req)) {
        Ok(_) => panic!("expected a failure"),
        Err(e) => token_wire(&e),
    }
}

fn token_failure(ctx: &MmArc, ticker: &str) -> (Json, StatusCode) { token_failure_of(ctx, token_request(ticker)) }

#[test]
fn a_token_balance_failure_is_could_not_fetch_balance_and_leaves_the_token_set_unchanged() {
    let ctx = ctx();
    let platform = register_platform(&ctx);
    let wire = token_failure(&ctx, "IRIS-NYAN");
    assert_eq!(wire.0["error_type"], "CouldNotFetchBalance", "{:?}", wire.0);
    assert!(wire.0["error_data"].is_string(), "{:?}", wire.0);
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(platform.activated_token_tickers().is_empty());
    assert_nothing_registered(&ctx, &["IRIS-NYAN"]);
}

#[test]
fn a_token_of_an_inactive_platform_is_platform_coin_is_not_activated() {
    let ctx = ctx();
    let wire = token_failure(&ctx, "IRIS-NYAN");
    assert_eq!(wire.0["error_type"], "PlatformCoinIsNotActivated");
    assert_eq!(wire.0["error_data"], json!(PLATFORM));
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
}

#[test]
fn a_token_of_a_non_tendermint_platform_is_unsupported_platform_coin() {
    let ctx = ctx();
    // An active coin that is not a Tendermint platform.
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_coin(MmCoinEnum::Test(coins::TestCoin::new("ETH")))).unwrap();
    let wire = token_failure(&ctx, "ETH-TOKEN");
    assert_eq!(wire.0["error_type"], "UnsupportedPlatformCoin");
    assert_eq!(
        wire.0["error_data"],
        json!({ "platform_coin_ticker": "ETH", "token_ticker": "ETH-TOKEN" })
    );
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn a_token_registered_after_a_successful_activation_is_recorded_on_the_platform() {
    let ctx = ctx();
    let platform = register_platform(&ctx);
    let token = TendermintToken::from_protocol("IRIS-NYAN".to_owned(), platform.clone(), 6, "unyan").unwrap();
    <TendermintToken as crate::token::TokenActivationOps>::on_token_registered(&platform, &token);
    assert!(platform.activated_token_tickers().contains("IRIS-NYAN"));
}

/// Tendermint swaps are postponed for this beta (`coins::TENDERMINT_SWAPS_AVAILABLE`): a platform coin and a
/// token report `wallet_only` true and decline every match, whatever their coins-config entries say.
#[test]
fn a_tendermint_platform_and_token_are_wallet_only_whatever_the_config_says() {
    for flag in [None, Some(false), Some(true)] {
        let mut confs = tendermint_coins();
        for conf in confs.iter_mut() {
            if let Some(flag) = flag {
                conf["wallet_only"] = json!(flag);
            }
        }
        let ctx = ctx_with(confs);
        let platform = register_platform(&ctx);
        let token = TendermintToken::from_protocol("IRIS-NYAN".to_owned(), platform.clone(), 6, "unyan").unwrap();

        assert!(platform.wallet_only(&ctx), "platform, flag {flag:?}");
        assert!(token.wallet_only(&ctx), "token, flag {flag:?}");
        assert!(!platform.is_coin_protocol_supported(&None), "platform, flag {flag:?}");
        assert!(!token.is_coin_protocol_supported(&None), "token, flag {flag:?}");
    }
}

// ---------------------------------------------------------------------------
// T36.12, T36.13: task placement and in-progress value
// ---------------------------------------------------------------------------

fn init(ctx: &MmArc, req: Req) -> Result<u64, (Json, StatusCode)> {
    match block_on(init_platform_coin_with_tokens::<TendermintCoin>(ctx.clone(), req)) {
        Ok(response) => Ok(serde_json::to_value(response).unwrap()["task_id"].as_u64().unwrap()),
        Err(e) => Err(wire(&e)),
    }
}

fn status(ctx: &MmArc, task_id: u64) -> Json {
    let status = block_on(init_platform_coin_with_tokens_status::<TendermintCoin>(
        ctx.clone(),
        RpcTaskStatusRequest {
            task_id: task_id as _,
            forget_if_finished: false,
        },
    ))
    .unwrap_or_else(|e| panic!("status failed: {}", e));
    serde_json::to_value(status).unwrap()
}

#[test]
fn task_init_rejects_an_active_platform_at_mmrpc_level() {
    let ctx = ctx();
    register_platform(&ctx);
    let (body, status) = init(&ctx, request(json!({}))).expect_err("the platform is active");
    assert_eq!(body["error_type"], "PlatformIsAlreadyActivated");
    assert_eq!(body["error_data"], json!(PLATFORM));
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn task_reports_the_other_errors_at_status_level() {
    let ctx = ctx();
    let task_id = init(&ctx, request(json!({ "ticker": "UNKNOWN-COIN" }))).expect("init accepts the request");
    let mut status = status(&ctx, task_id);
    for _ in 0..100 {
        if status["status"] != "InProgress" {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        status = self::status(&ctx, task_id);
    }
    assert_eq!(status["status"], "Error", "{:?}", status);
    assert_eq!(status["details"]["error_type"], "PlatformConfigIsNotFound");
    assert_eq!(status["details"]["error_data"], "UNKNOWN-COIN");
}

#[test]
fn a_running_task_reports_activating_coin_only() {
    let ctx = ctx();
    // A node that accepts connections and never answers keeps the activation in its node-contacting step.
    let silent_node = TcpListener::bind("127.0.0.1:0").expect("bind the silent node");
    let url = format!("http://127.0.0.1:{}", silent_node.local_addr().unwrap().port());
    let task_id = init(&ctx, request(json!({ "nodes": [{ "url": url }] }))).expect("init accepts the request");
    for _ in 0..15 {
        let status = status(&ctx, task_id);
        assert_eq!(
            status,
            json!({ "status": "InProgress", "details": "ActivatingCoin" }),
            "{:?}",
            status
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    block_on(cancel_init_platform_coin_with_tokens::<TendermintCoin>(
        ctx.clone(),
        RpcTaskStatusRequest {
            task_id: task_id as _,
            forget_if_finished: false,
        },
    ))
    .unwrap_or_else(|e| panic!("cancel failed: {}", e));
    drop(silent_node);
}

// ---------------------------------------------------------------------------
// Custom tokens (R36.1.2 `tokens_params[].protocol`, R36.1.4 step 4, R36.1.6, R36.2.1, R36.2.4)
// ---------------------------------------------------------------------------

fn custom_protocol(platform: &str) -> Json {
    json!({ "type": "TENDERMINTTOKEN", "protocol_data": { "platform": platform, "decimals": 6, "denom": "ucustom" } })
}

/// A `tokens_params` entry that carries an inline protocol.
fn inline_token(ticker: &str, platform: &str) -> Json {
    json!({ "ticker": ticker, "protocol": custom_protocol(platform) })
}

/// The standard base64 of `bytes`.
fn base64_standard(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let group = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[(group >> 18) as usize & 63] as char);
        encoded.push(ALPHABET[(group >> 12) as usize & 63] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6) as usize & 63] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[group as usize & 63] as char
        } else {
            '='
        });
    }
    encoded
}

/// The protobuf of a bank `QueryBalanceResponse` holding `amount` of `denom` (both short enough for one-byte
/// lengths).
fn balance_response(denom: &str, amount: u64) -> Vec<u8> {
    let amount = amount.to_string();
    let mut coin = vec![0x0a, denom.len() as u8];
    coin.extend_from_slice(denom.as_bytes());
    coin.push(0x12);
    coin.push(amount.len() as u8);
    coin.extend_from_slice(amount.as_bytes());
    let mut response = vec![0x0a, coin.len() as u8];
    response.extend_from_slice(&coin);
    response
}

/// A node that answers the health check and every balance query with `amount` of `denom`.
fn balance_node(denom: &str, amount: u64) -> crate::evm_test_support::MockEvmNode {
    let value = base64_standard(&balance_response(denom, amount));
    spawn_node(move |method, _params| match method {
        "health" => Reply::Result(json!({})),
        "abci_query" => Reply::Result(json!({ "response": { "code": 0, "value": value } })),
        _ => Reply::RpcError,
    })
}

fn token_of(ctx: &MmArc, ticker: &str) -> TendermintToken {
    match block_on(lp_coinfind(ctx, ticker)) {
        Ok(Some(MmCoinEnum::TendermintToken(token))) => token,
        _ => panic!("{} is not an active Tendermint token", ticker),
    }
}

#[test]
fn an_inline_token_is_enabled_beside_the_platform_and_is_wallet_only() {
    // R36.1.2, R36.1.4 step 4: a token entry with a `protocol` is built from it; the entry of a configured token
    // is built from the coins configuration and is not marked custom (R-b: a custom token is wallet-only).
    let ctx = ctx();
    let platform = register_platform(&ctx);
    let initializer = TendermintTokenInitializer {
        platform_coin: platform.clone(),
    };
    let req = request(json!({ "tokens_params": [{ "ticker": "IRIS-NYAN" }, inline_token("CUSTOM-TOKEN", PLATFORM)] }));

    let coins = match block_on(initializer.enable_tokens_as_mm_coins(ctx.clone(), &req.request)) {
        Ok(coins) => coins,
        Err(_) => panic!("the tokens must be enabled"),
    };
    assert_eq!(coins.len(), 2);
    let tokens: Vec<TendermintToken> = coins
        .into_iter()
        .map(|coin| match coin {
            MmCoinEnum::TendermintToken(token) => token,
            _ => panic!("a Tendermint token"),
        })
        .collect();
    let custom = tokens
        .iter()
        .find(|token| token.ticker == "CUSTOM-TOKEN")
        .expect("the custom token");
    let configured = tokens
        .iter()
        .find(|token| token.ticker == "IRIS-NYAN")
        .expect("the configured token");

    assert!(custom.custom_token);
    assert_eq!(custom.decimals, 6);
    assert_eq!(custom.denom.to_string(), "ucustom");
    assert!(custom.wallet_only(&ctx));
    assert!(!configured.custom_token);
    assert_eq!(configured.denom.to_string(), "unyan");

    let recorded = platform.activated_token_tickers();
    assert!(recorded.contains("CUSTOM-TOKEN") && recorded.contains("IRIS-NYAN"));
}

#[test]
fn an_inline_protocol_for_a_configured_ticker_is_a_custom_token_error() {
    // T36.6, R36.1.6: a `protocol` for a ticker of the coins configuration is rejected, HTTP 500.
    let ctx = ctx();
    let wire = failure(
        &ctx,
        request(json!({ "tokens_params": [inline_token("IRIS-NYAN", PLATFORM)] })),
    );
    assert_eq!(wire.0["error_type"], "CustomTokenError", "{:?}", wire.0);
    assert_eq!(
        wire.0["error_data"],
        json!({ "DuplicateTickerInConfig": { "ticker_in_config": "IRIS-NYAN" } })
    );
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
    assert_nothing_registered(&ctx, &[PLATFORM, "IRIS-NYAN"]);
}

#[test]
fn an_inline_token_of_another_platform_is_platform_coin_mismatch() {
    // R36.1.4 step 4: a custom token names the platform being activated like a configured one.
    let ctx = ctx();
    let wire = failure(
        &ctx,
        request(json!({ "tokens_params": [inline_token("CUSTOM-TOKEN", "OSMO")] })),
    );
    assert_unit_error(&wire, "PlatformCoinMismatch", StatusCode::BAD_REQUEST);
    assert_nothing_registered(&ctx, &[PLATFORM, "CUSTOM-TOKEN"]);
}

#[test]
fn an_inline_protocol_of_another_type_is_unexpected_token_protocol() {
    // R36.1.6: the inline protocol must be `TENDERMINTTOKEN`; the answer carries the canonical protocol form.
    let ctx = ctx();
    let protocol = json!({ "type": "TENDERMINT", "protocol_data": {
        "denom": "uatom", "decimals": 6, "account_prefix": "cosmos", "chain_id": "cosmoshub-4" } });
    let wire = failure(
        &ctx,
        request(json!({ "tokens_params": [{ "ticker": "CUSTOM-TOKEN", "protocol": protocol }] })),
    );
    assert_eq!(wire.0["error_type"], "UnexpectedTokenProtocol", "{:?}", wire.0);
    assert_eq!(wire.0["error_data"]["ticker"], "CUSTOM-TOKEN");
    assert_eq!(wire.0["error_data"]["protocol"]["type"], "TENDERMINT");
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
}

#[test]
fn a_custom_token_is_activated_on_its_own_and_is_wallet_only() {
    // R36.2.1: `enable_tendermint_token` with a `protocol` activates the token from it; R-b: it is wallet-only.
    let ctx = ctx();
    let node = balance_node("ucustom", 12_345_678);
    let platform = register_platform_on(&ctx, &node.url);

    let result = match block_on(enable_tendermint_token(
        ctx.clone(),
        token_request_with("CUSTOM-TOKEN", custom_protocol(PLATFORM)),
    )) {
        Ok(result) => serde_json::to_value(&result).expect("a serializable result"),
        Err(e) => panic!("the custom token must activate: {:?}", token_wire(&e).0),
    };
    assert_eq!(result["platform_coin"], PLATFORM);
    let balances = result["balances"].as_object().expect("a balances map");
    assert_eq!(balances.len(), 1, "{:?}", result);
    assert_eq!(balances.values().next().unwrap()["spendable"], "12.345678");

    let token = token_of(&ctx, "CUSTOM-TOKEN");
    assert!(token.custom_token);
    assert!(token.wallet_only(&ctx));
    assert!(platform.activated_token_tickers().contains("CUSTOM-TOKEN"));

    // A second activation of the same ticker is refused.
    let wire = token_failure_of(&ctx, token_request_with("CUSTOM-TOKEN", custom_protocol(PLATFORM)));
    assert_eq!(wire.0["error_type"], "TokenIsAlreadyActivated");
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
}

#[test]
fn a_configured_token_is_activated_as_before_and_is_not_custom() {
    let ctx = ctx();
    let node = balance_node("unyan", 5_000_000);
    let platform = register_platform_on(&ctx, &node.url);

    match block_on(enable_tendermint_token(ctx.clone(), token_request("IRIS-NYAN"))) {
        Ok(_) => {},
        Err(e) => panic!("the configured token must activate: {:?}", token_wire(&e).0),
    }
    assert!(!token_of(&ctx, "IRIS-NYAN").custom_token);
    assert!(platform.activated_token_tickers().contains("IRIS-NYAN"));
}

#[test]
fn a_custom_token_for_a_configured_ticker_on_its_own_is_a_custom_token_error() {
    // T36.6, R36.2.4: the same on `enable_tendermint_token` is HTTP 400.
    let ctx = ctx();
    let wire = token_failure_of(&ctx, token_request_with("IRIS-NYAN", custom_protocol(PLATFORM)));
    assert_eq!(wire.0["error_type"], "CustomTokenError", "{:?}", wire.0);
    assert_eq!(
        wire.0["error_data"],
        json!({ "DuplicateTickerInConfig": { "ticker_in_config": "IRIS-NYAN" } })
    );
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
}

#[test]
fn a_custom_token_on_its_own_follows_the_platform_and_balance_errors() {
    let ctx = ctx();
    // The platform is not active.
    let wire = token_failure_of(&ctx, token_request_with("CUSTOM-TOKEN", custom_protocol(PLATFORM)));
    assert_eq!(wire.0["error_type"], "PlatformCoinIsNotActivated");
    assert_eq!(wire.0["error_data"], json!(PLATFORM));
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);

    // The platform is active and its nodes refuse: the protocol was accepted, the balance read failed, and the
    // platform's token set is unchanged.
    let platform = register_platform(&ctx);
    let wire = token_failure_of(&ctx, token_request_with("CUSTOM-TOKEN", custom_protocol(PLATFORM)));
    assert_eq!(wire.0["error_type"], "CouldNotFetchBalance", "{:?}", wire.0);
    assert_eq!(wire.1, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(platform.activated_token_tickers().is_empty());
    assert_nothing_registered(&ctx, &["CUSTOM-TOKEN"]);

    // A protocol of another type is refused with the canonical protocol form.
    let other = json!({ "type": "TENDERMINT", "protocol_data": {
        "denom": "uatom", "decimals": 6, "account_prefix": "cosmos", "chain_id": "cosmoshub-4" } });
    let wire = token_failure_of(&ctx, token_request_with("CUSTOM-TOKEN", other));
    assert_eq!(wire.0["error_type"], "UnexpectedTokenProtocol");
    assert_eq!(wire.0["error_data"]["protocol"]["type"], "TENDERMINT");
}

// ---------------------------------------------------------------------------
// The key of the platform coin (R36.1.2 `path_to_address`, R36.1.4 step 3)
// ---------------------------------------------------------------------------

fn hd_ctx() -> MmArc {
    let mut coins = coins_conf().as_array().cloned().unwrap_or_default();
    coins.extend(tendermint_coins());
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).expect("initialize the HD crypto context");
    ctx
}

/// Builds the platform coin the way the activation does, without contacting a node.
fn built_platform(
    ctx: &MmArc,
    ticker: &str,
    extra: Json,
) -> Result<TendermintCoin, MmError<TendermintActivationError>> {
    let (conf, protocol) = match coin_conf_with_protocol::<TendermintPlatformProtocolInfo>(ctx, ticker) {
        Ok(found) => found,
        Err(_) => panic!("the configuration of {}", ticker),
    };
    let mut extra = extra;
    extra["ticker"] = json!(ticker);
    let priv_key = *ctx.legacy_passphrase_key_pair().private().secret;
    block_on(
        <TendermintCoin as PlatformWithTokensActivationOps>::enable_platform_coin(
            ctx.clone(),
            ticker.to_owned(),
            conf,
            request(extra).request,
            protocol,
            &priv_key,
        ),
    )
}

fn address_of_secret(secret: &[u8]) -> String {
    let key_pair = crypto::privkey::key_pair_from_secret(secret).unwrap();
    coins::tendermint::account_id_from_pubkey_hex("cosmos", &hex::encode(key_pair.public_slice()))
        .unwrap()
        .to_string()
}

/// The address of the key at `path` of the wallet of an HD login.
fn hd_address_at(ctx: &MmArc, path: &str) -> String {
    let global_hd = match crypto::CryptoCtx::from_ctx(ctx).unwrap().key_pair_policy() {
        crypto::KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        crypto::KeyPairPolicy::Iguana => panic!("expected an HD login"),
    };
    let secret = global_hd
        .derive_secp256k1_secret(&crypto::DerivationPath::from_str(path).unwrap())
        .unwrap();
    address_of_secret(secret.as_slice())
}

fn built_address(ctx: &MmArc, ticker: &str, extra: Json) -> String {
    match built_platform(ctx, ticker, extra) {
        Ok(coin) => coin.account_id.to_string(),
        Err(e) => panic!("the platform must be built: {:?}", e.get_inner()),
    }
}

#[test]
fn an_iguana_login_activates_the_address_of_the_passphrase_key_whatever_path_to_address_holds() {
    // R36.1.2: without HD mode `path_to_address` has no effect, whatever it holds.
    let ctx = ctx();
    let expected = address_of_secret(&*ctx.legacy_passphrase_key_pair().private().secret);
    assert_eq!(built_address(&ctx, "ATOM", json!({})), expected);
    for path_to_address in [
        json!({ "account_id": 3, "chain": "Internal", "address_id": 9 }),
        json!("nonsense"),
        json!({ "chain": "Sideways" }),
        Json::Null,
    ] {
        let extra = json!({ "path_to_address": path_to_address });
        assert_eq!(built_address(&ctx, "ATOM", extra), expected);
    }
    // Also a coin without a `derivation_path`.
    assert_eq!(built_address(&ctx, "ATOM-NODERIV", json!({})), expected);
}

#[test]
fn an_hd_login_activates_the_address_at_the_default_path() {
    // R36.1.4 step 3: the address at account 0, External, address 0 under the coin's `derivation_path`.
    let ctx = hd_ctx();
    let expected = hd_address_at(&ctx, "m/44'/118'/0'/0/0");
    assert_eq!(built_address(&ctx, "ATOM", json!({})), expected);
    assert_eq!(
        built_address(&ctx, "ATOM", json!({ "path_to_address": null })),
        expected
    );
    // It is not the address of the passphrase key that an iguana login (and earlier builds) used.
    let passphrase_key = address_of_secret(&*ctx.legacy_passphrase_key_pair().private().secret);
    assert_ne!(expected, passphrase_key);
}

#[test]
fn an_hd_login_honors_path_to_address() {
    let ctx = hd_ctx();
    let cases = [
        (
            json!({ "account_id": 2, "chain": "Internal", "address_id": 5 }),
            "m/44'/118'/2'/1/5",
        ),
        (json!({ "address_id": 3 }), "m/44'/118'/0'/0/3"),
        (json!({ "account_id": 1, "chain": "External" }), "m/44'/118'/1'/0/0"),
    ];
    for (path_to_address, path) in cases {
        let extra = json!({ "path_to_address": path_to_address });
        assert_eq!(built_address(&ctx, "ATOM", extra), hd_address_at(&ctx, path), "{path}");
    }
}

#[test]
fn an_hd_login_without_a_derivation_path_is_a_platform_coin_creation_error() {
    // R36.1.4 step 3: missing derivation config is `PlatformCoinCreationError`.
    let ctx = hd_ctx();
    match built_platform(&ctx, "ATOM-NODERIV", json!({})) {
        Ok(_) => panic!("the activation needs a derivation path"),
        Err(e) => match e.get_inner() {
            TendermintActivationError::PlatformCoinCreationError { ticker, error } => {
                assert_eq!(ticker, "ATOM-NODERIV");
                assert!(error.contains("derivation_path"), "{}", error);
            },
            other => panic!("unexpected error {:?}", other),
        },
    }
}

#[test]
fn an_hd_login_with_an_invalid_path_to_address_is_a_platform_coin_creation_error() {
    // R36.1.4 step 3: an invalid path is `PlatformCoinCreationError`.
    let ctx = hd_ctx();
    for path_to_address in [
        json!("nonsense"),
        json!({ "account_id": "zero" }),
        json!({ "chain": "Sideways" }),
        // A hardened child number must be below 2^31.
        json!({ "account_id": 2_147_483_648u32 }),
    ] {
        let extra = json!({ "path_to_address": path_to_address.clone() });
        match built_platform(&ctx, "ATOM", extra) {
            Ok(_) => panic!("{} must be refused", path_to_address),
            Err(e) => assert!(
                matches!(
                    e.get_inner(),
                    TendermintActivationError::PlatformCoinCreationError { ticker, .. } if ticker == "ATOM"
                ),
                "{}: {:?}",
                path_to_address,
                e.get_inner()
            ),
        }
    }
}
