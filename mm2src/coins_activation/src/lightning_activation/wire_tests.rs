//! Wire-shape tests of `task::enable_lightning::init` and of the Lightning task status errors (CRD ch. 41 §41.2;
//! T41.1 -- T41.4, T41.7) and of the shared one-shot `enable_lightning` error.
//!
//! Every test serializes the error as the dispatcher would and asserts the `error_type`, the JSON shape of
//! `error_data` and the HTTP status.

use super::*;
use crate::evm_test_support::error_of;
use crate::l2::{enable_l2, init_l2, EnableL2Request, L2ActivationRequest};
use coins::lightning::ln_errors::EnableLightningError;
use coins::{lp_register_coin, RegisterCoinParams, TestCoin};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;

const LN: &str = "LNBTC";

fn confirmations() -> Json {
    json!({
        "background": { "default_fee_per_kb": 253, "n_blocks": 12 },
        "normal": { "default_fee_per_kb": 2000, "n_blocks": 6 },
        "high_priority": { "default_fee_per_kb": 5000, "n_blocks": 1 }
    })
}

fn ctx() -> MmArc {
    let coins = json!([
        { "coin": LN, "decimals": 11, "protocol": { "type": "LIGHTNING", "protocol_data": {
            "platform": "BTC-segwit", "network": "testnet", "confirmations": confirmations() } } },
        { "coin": "LNNODEC", "protocol": { "type": "LIGHTNING", "protocol_data": {
            "platform": "BTC-segwit", "network": "testnet", "confirmations": confirmations() } } },
        { "coin": "LNOFFLINE", "decimals": 11, "protocol": { "type": "LIGHTNING", "protocol_data": {
            "platform": "OFFLINE-segwit", "network": "testnet", "confirmations": confirmations() } } },
        { "coin": "LNBADPROTO", "decimals": 11, "protocol": { "type": "LIGHTNING" } },
        { "coin": "KMD", "protocol": { "type": "UTXO" } },
        { "coin": "USDT-ERC20", "protocol": { "type": "ERC20", "protocol_data": {
            "platform": "ETH", "contract_address": "0xdAC17F958D2ee523a2206206994597C13D831ec7" } } },
        { "coin": "BTC-segwit", "protocol": { "type": "UTXO" }, "avg_blocktime": 600 },
    ]);
    MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc()
}

fn init_failure(ctx: &MmArc, ticker: &str) -> (Json, StatusCode) {
    let req: L2ActivationRequest<LightningActivationParams> = serde_json::from_value(json!({
        "ticker": ticker,
        "activation_params": { "name": "node" }
    }))
    .unwrap();
    match block_on(init_l2::<LightningCoin>(ctx.clone(), req)) {
        Ok(_) => panic!("expected a failure"),
        Err(e) => (
            serde_json::to_value(e.get_inner()).expect("a serializable error"),
            e.get_inner().status_code(),
        ),
    }
}

fn register_test_coin(ctx: &MmArc, ticker: &str) {
    block_on(lp_register_coin(
        ctx,
        coins::MmCoinEnum::Test(TestCoin::new(ticker)),
        RegisterCoinParams {
            ticker: ticker.to_owned(),
            tx_history: false,
        },
    ))
    .unwrap_or_else(|e| panic!("cannot register {}: {}", ticker, e));
}

fn wire<E: serde::Serialize + HttpStatusCode>(error: &E) -> (Json, StatusCode) {
    (
        serde_json::to_value(error).expect("a serializable error"),
        error.status_code(),
    )
}

// ---------------------------------------------------------------------------
// T41.1, T41.2: init errors, in row order
// ---------------------------------------------------------------------------

#[test]
fn init_errors_have_the_bound_names_shapes_and_statuses() {
    let ctx = ctx();

    // Row 1.
    register_test_coin(&ctx, LN);
    assert_eq!(
        init_failure(&ctx, LN),
        (
            json!({ "error_type": "L2IsAlreadyActivated", "error_data": LN }),
            StatusCode::BAD_REQUEST
        )
    );

    // Row 2.
    assert_eq!(
        init_failure(&ctx, "NOSUCH"),
        (
            json!({ "error_type": "L2ConfigIsNotFound", "error_data": "NOSUCH" }),
            StatusCode::BAD_REQUEST
        )
    );

    // Row 3: object error_data with exactly the members ticker and error.
    let (body, status) = init_failure(&ctx, "LNBADPROTO");
    assert_eq!(body["error_type"], "L2ProtocolParseError");
    assert_eq!(body["error_data"]["ticker"], "LNBADPROTO");
    assert!(body["error_data"]["error"].is_string());
    assert_eq!(body["error_data"].as_object().unwrap().len(), 2);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // Row 4 (T41.2): the protocol in the canonical form, without protocol data for UTXO.
    assert_eq!(
        init_failure(&ctx, "KMD"),
        (
            json!({ "error_type": "UnexpectedL2Protocol", "error_data": { "ticker": "KMD", "protocol": { "type": "UTXO" } } }),
            StatusCode::BAD_REQUEST
        )
    );
    let (body, status) = init_failure(&ctx, "USDT-ERC20");
    assert_eq!(body["error_type"], "UnexpectedL2Protocol");
    assert_eq!(body["error_data"]["protocol"]["type"], "ERC20");
    assert_eq!(body["error_data"]["protocol"]["protocol_data"]["platform"], "ETH");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Row 5: the rest of the entry does not parse (no `decimals`).
    let (body, status) = init_failure(&ctx, "LNNODEC");
    assert_eq!(body["error_type"], "L2ConfigParseError");
    assert!(body["error_data"].is_string());
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn an_inactive_platform_names_the_lightning_ticker() {
    let ctx = ctx();
    // Row 6: the data is the Lightning ticker, not the platform ticker.
    assert_eq!(
        init_failure(&ctx, LN),
        (
            json!({ "error_type": "PlatformCoinIsNotActivated", "error_data": LN }),
            StatusCode::BAD_REQUEST
        )
    );
}

#[test]
fn a_non_utxo_platform_is_unsupported_platform_coin() {
    let ctx = ctx();
    register_test_coin(&ctx, "BTC-segwit");
    // Row 7.
    assert_eq!(
        init_failure(&ctx, LN),
        (
            json!({ "error_type": "UnsupportedPlatformCoin", "error_data": { "platform_coin_ticker": "BTC-segwit", "l2_ticker": LN } }),
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
}

#[test]
fn the_remaining_task_errors_have_the_bound_shapes() {
    // Row 9.
    assert_eq!(
        wire(&L2ActivationError::InvalidPlatformConfiguration {
            platform_coin_ticker: "BTC-segwit".to_owned(),
            err: "avg_blocktime is missing".to_owned(),
        }),
        (
            json!({ "error_type": "InvalidPlatformConfiguration", "error_data": { "platform_coin_ticker": "BTC-segwit", "err": "avg_blocktime is missing" } }),
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
    // Rows 8, 10 and 11 are `Internal` strings; the task-level statuses of R41.2.6.
    assert_eq!(
        wire(&L2ActivationError::Internal("x".to_owned())),
        (
            json!({ "error_type": "Internal", "error_data": "x" }),
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
    assert_eq!(
        wire(&L2ActivationError::TaskTimedOut {
            duration: std::time::Duration::from_secs(3)
        })
        .1,
        StatusCode::REQUEST_TIMEOUT
    );
    assert_eq!(
        wire(&L2ActivationError::Transport("x".to_owned())),
        (
            json!({ "error_type": "Transport", "error_data": "x" }),
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
}

#[test]
fn the_one_shot_enable_keeps_the_same_names() {
    let ctx = ctx();
    let req: EnableL2Request<LightningActivationParams> = serde_json::from_value(json!({
        "ticker": "KMD",
        "activation_params": { "name": "node" }
    }))
    .unwrap();
    let error = error_of(block_on(enable_l2::<LightningCoin>(ctx, req)), "a failure");
    assert_eq!(
        wire(error.get_inner()),
        (
            json!({ "error_type": "UnexpectedL2Protocol", "error_data": { "ticker": "KMD", "protocol": { "type": "UTXO" } } }),
            StatusCode::BAD_REQUEST
        )
    );
    assert_eq!(
        wire(&EnableL2Error::InvalidPlatformConfiguration {
            platform_coin_ticker: "BTC-segwit".to_owned(),
            err: "e".to_owned(),
        }),
        (
            json!({ "error_type": "InvalidPlatformConfiguration", "error_data": { "platform_coin_ticker": "BTC-segwit", "err": "e" } }),
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
}

// ---------------------------------------------------------------------------
// T41.3: the avg_blocktime check
// ---------------------------------------------------------------------------

#[test]
fn the_platform_must_declare_avg_blocktime() {
    let ctx = ctx();
    assert!(check_platform_avg_blocktime(&ctx, "BTC-segwit").is_ok());
    // No entry at all, and an entry without the member.
    let err = error_of(check_platform_avg_blocktime(&ctx, "KMD"), "missing avg_blocktime");
    assert_eq!(err.get_inner().platform_coin_ticker, "KMD");
    assert!(!err.get_inner().err.is_empty());
    assert!(check_platform_avg_blocktime(&ctx, "NOSUCH").is_err());
}

// ---------------------------------------------------------------------------
// T41.7: activation parameters
// ---------------------------------------------------------------------------

#[test]
fn activation_parameters_are_validated_as_bound() {
    let params = |value: Json| -> LightningActivationParams { serde_json::from_value(value).unwrap() };
    let validated =
        <LightningCoin as InitL2ActivationOps>::validate_activation_params(params(json!({ "name": "node" })))
            .unwrap_or_else(|e| panic!("{}", e));
    // `color` absent yields the documented `2b6680` (R41.2.4; the reference's `000000` is not used).
    assert_eq!(validated.node_color, [0x2b, 0x66, 0x80]);
    // The one-shot twin (`enable_lightning`) uses the same default.
    let one_shot = match <LightningCoin as crate::l2::L2ActivationOps>::validate_activation_params(params(
        json!({ "name": "node" }),
    )) {
        Ok(validated) => validated,
        Err(_) => panic!("the one-shot validation accepts a request with a name only"),
    };
    assert_eq!(one_shot.node_color, [0x2b, 0x66, 0x80]);
    // An explicit colour is still honoured.
    let explicit = <LightningCoin as InitL2ActivationOps>::validate_activation_params(params(
        json!({ "name": "node", "color": "ff0001" }),
    ))
    .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(explicit.node_color, [0xff, 0x00, 0x01]);
    assert_eq!(validated.listening_port, 9735);
    assert_eq!(&validated.node_name[..4], b"node");
    assert!(validated.node_name[4..].iter().all(|b| *b == b' '));

    // A 33-byte name and a malformed colour are `Internal` at init.
    for bad in [
        json!({ "name": "n".repeat(33) }),
        json!({ "name": "n", "color": "12345" }),
        json!({ "name": "n", "color": "zzzzzz" }),
    ] {
        let error = error_of(
            <LightningCoin as InitL2ActivationOps>::validate_activation_params(params(bad)),
            "an invalid parameter",
        );
        let mapped: L2ActivationError = error.into_inner().into();
        assert_eq!(wire(&mapped).0["error_type"], "Internal");
        assert_eq!(wire(&mapped).1, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // A listening port that is not an integer is a parse failure.
    assert!(
        serde_json::from_value::<LightningActivationParams>(json!({ "name": "n", "listening_port": "9735" })).is_err()
    );
}

// ---------------------------------------------------------------------------
// T41.4: status-level mapping (R41.2.7)
// ---------------------------------------------------------------------------

fn status_error(error: LightningTaskError) -> (Json, StatusCode) {
    let mapped: L2ActivationError = error.into();
    wire(&mapped)
}

#[test]
fn an_unreachable_platform_backend_is_a_status_level_transport_error() {
    let started: LightningTaskError =
        LightningInitError::EnableLightningError(EnableLightningError::RpcError("down".to_owned())).into();
    let (body, status) = status_error(started);
    assert_eq!(body, json!({ "error_type": "Transport", "error_data": "down" }));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // Also a transport failure while reading the final balance.
    let balance: LightningTaskError =
        LightningInitError::MyBalanceError(BalanceError::Transport("down".to_owned())).into();
    assert_eq!(status_error(balance).0["error_type"], "Transport");
}

#[test]
fn other_start_failures_are_internal() {
    for error in [
        LightningInitError::EnableLightningError(EnableLightningError::IOError("port in use".to_owned())),
        LightningInitError::EnableLightningError(EnableLightningError::DbError("db".to_owned())),
        LightningInitError::EnableLightningError(EnableLightningError::UnsupportedMode(
            "start".to_owned(),
            "iguana".to_owned(),
        )),
        LightningInitError::MyBalanceError(BalanceError::Internal("x".to_owned())),
        LightningInitError::MyBalanceError(BalanceError::InvalidResponse("x".to_owned())),
        LightningInitError::MyAddressError("x".to_owned()),
    ] {
        let (body, status) = status_error(error.into());
        assert_eq!(body["error_type"], "Internal", "{:?}", body);
        assert!(body["error_data"].is_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[test]
fn a_registration_race_is_l2_is_already_activated() {
    let lost: LightningTaskError = RegisterCoinError::CoinIsInitializedAlready { coin: LN.to_owned() }.into();
    assert_eq!(
        status_error(lost),
        (
            json!({ "error_type": "L2IsAlreadyActivated", "error_data": LN }),
            StatusCode::BAD_REQUEST
        )
    );
    let other: LightningTaskError = RegisterCoinError::Internal("x".to_owned()).into();
    assert_eq!(status_error(other).0["error_type"], "Internal");
}

#[test]
fn a_platform_configuration_failure_keeps_its_name_in_the_status() {
    let error: LightningTaskError = LightningInitError::InvalidPlatformConfiguration {
        platform_coin_ticker: "BTC-segwit".to_owned(),
        err: "e".to_owned(),
    }
    .into();
    let (body, status) = status_error(error);
    assert_eq!(body["error_type"], "InvalidPlatformConfiguration");
    assert_eq!(
        body["error_data"],
        json!({ "platform_coin_ticker": "BTC-segwit", "err": "e" })
    );
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
