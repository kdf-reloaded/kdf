//! Tests of the typed cores of the wallet and coin RPC methods of group D (CRD ch.43 §43.1R; plan
//! `mmrpc-v2-completion`, phase 3): `my_balance`, `disable_coin`, `send_raw_transaction`, `validateaddress`,
//! `convertaddress`, `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
//! `set_required_confirmations` and `set_requires_notarization`.
//!
//! * The legacy arms keep their answers. Every expected legacy body below is the body that the handler produced as it
//!   stood on dev at `afb8b1bb8`, derived from its code and marked with the handler that produced it; the expected
//!   error texts are those of its `ERR!`/`try_s!`/`ERRL!` sites. The `file:line] ` tokens in front of a legacy error
//!   text change with every edit of the source, so they are split off, checked to name the file of the handler, and the
//!   text behind them is compared whole ([`legacy_refusal`]). The expected bodies were compared with the answers of a
//!   release build in the lab row of PR #379.
//! * Every variant of every error enum has its serialized `error_type` token, its `error_data` JSON type, its HTTP
//!   status and its `Display` text checked (CRD ch.43 R43.8.6, the form of the tests of PR #312).
//! * The ten methods are registered on the versioned surface. Requests run through the whole server path
//!   (`process_single_request`: the legacy table, the fallback, the versioned dispatcher) on the test thread, so that
//!   the coin mocks of a test apply.
//! * No assertion message carries an error payload, a body or a `Display` text: only tokens, method names and labels.

use super::*;
use crate::mm2::lp_ordermatch::order_rpc_tests::Node;
use crate::mm2::rpc::lp_commands_legacy::legacy_disable_coin_refusal;
use coins::{BalanceError, CoinBalance, CoinSettingError, CoinsContext, CoinsNeededForKickStartError,
            ConvertAddressError, ConvertUtxoAddressError, DisableCoinError, FeeCapBelowBaseFee, KmdRewardsInfoError,
            MarketCoinOps, MmCoin, MmCoinEnum, MyBalanceRpcError, SendRawTransactionError, TestCoin,
            UnexpectedDerivationMethod, ValidateAddressError, ValidateAddressResult};
use common::block_on;
use common::mm_number::BigDecimal;
use mm2_core::mm_ctx::MmCtxBuilder;
use mocktopus::mocking::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

fn bd(s: &str) -> BigDecimal { s.parse().unwrap() }

/// `actual` is `expected`; the message names `what` only (the values are bodies or texts).
fn same<T: PartialEq>(actual: &T, expected: &T, what: &str) {
    assert!(actual == expected, "{what}: the values differ");
}

/// CRD ch.43 R43.8.6: the serialized `error_type` token, the JSON type of `error_data` (`None`: the key is absent), the
/// HTTP status and the `Display` text of an error.
fn assert_wire<E>(error: &E, token: &str, data: Option<&str>, status: u16, text: &str)
where
    E: serde::Serialize + HttpStatusCode + std::fmt::Display,
{
    // No assertion message below carries the error, its payload or its text: only the token.
    let wire = json::to_value(error).unwrap();
    assert_eq!(wire["error_type"], token, "{token}: error_type");
    match data {
        Some("object") => assert!(wire["error_data"].is_object(), "{token}: error_data kind"),
        Some("string") => assert!(wire["error_data"].is_string(), "{token}: error_data kind"),
        Some(other) => panic!("unknown kind of error_data: {other}"),
        None => assert!(wire.get("error_data").is_none(), "{token}: error_data kind"),
    }
    assert_eq!(error.status_code().as_u16(), status, "{token}: status");
    assert!(error.to_string() == text, "{token}: Display text");
}

/// Splits the `file:line] ` tokens that `try_s!` and `ERR!` put in front of a legacy error text.
fn split_location(error: &str) -> (Vec<String>, String) {
    let token = Regex::new(r"^([A-Za-z0-9_]+):\d+\] ").unwrap();
    let mut files = Vec::new();
    let mut rest = error;
    while let Some(found) = token.captures(rest) {
        files.push(found[1].to_owned());
        rest = &rest[found[0].len()..];
    }
    (files, rest.to_owned())
}

fn client() -> SocketAddr { SocketAddr::from(([127, 0, 0, 1], 4242)) }

/// Runs `request` through the whole server path on the test thread.
fn run(ctx: &MmArc, request: Json) -> Result<Response<Vec<u8>>, String> {
    block_on(process_single_request(ctx.clone(), request, client()))
}

/// A legacy request: no `mmrpc` key, the members inline next to `method`, the server password.
fn legacy_request(method: &str, params: Json) -> Json {
    let mut request = json!({ "userpass": "pw", "method": method });
    for (key, value) in params.as_object().expect("the inline parameters are an object") {
        request[key.as_str()] = value.clone();
    }
    request
}

/// A legacy success: HTTP 200. Returns the body text exactly as the client receives it.
fn legacy_success(ctx: &MmArc, method: &str, params: Json) -> String {
    let answer = match run(ctx, legacy_request(method, params)) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: a legacy success was expected"),
    };
    assert_eq!(answer.status().as_u16(), 200, "{method}: legacy success status");
    String::from_utf8(answer.into_body()).unwrap()
}

/// A legacy refusal: the legacy surface answers it as `{"error": <text>}` with HTTP 500, which is what an `Err(String)`
/// of the handler becomes. Returns the text without its `file:line] ` tokens; the innermost token names `handler_file`
/// (the file name without its extension).
fn legacy_refusal(ctx: &MmArc, method: &str, params: Json, handler_file: &str) -> String {
    let error = match run(ctx, legacy_request(method, params)) {
        Ok(_) => panic!("{method}: a legacy refusal was expected"),
        Err(error) => error,
    };
    let (files, rest) = split_location(&error);
    assert!(
        files.last().map(String::as_str) == Some(handler_file),
        "{method}: the legacy error names its handler file"
    );
    rest
}

/// A versioned request. `params` is left out when `None`.
fn versioned(ctx: &MmArc, method: &str, params: Option<Json>, id: usize) -> (u16, Json) {
    let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": id });
    if let Some(params) = params {
        request["params"] = params;
    }
    let answer = match run(ctx, request) {
        Ok(answer) => answer,
        Err(_) => panic!("{method}: the versioned surface answers every request"),
    };
    let body = json::from_slice(answer.body()).unwrap_or_else(|_| panic!("the answer is JSON"));
    (answer.status().as_u16(), body)
}

/// A versioned success: HTTP 200, the 2.0 envelope with the `id` echoed and no error member. Returns `result`.
fn versioned_result(answer: (u16, Json), id: usize) -> Json {
    let (status, body) = answer;
    // No message below carries the body: a refused request would put an error payload into it.
    assert_eq!(status, 200, "versioned success: status");
    assert_eq!(body["mmrpc"], "2.0", "versioned success: mmrpc version");
    assert_eq!(body["id"], id, "versioned success: id");
    assert!(
        body.get("error").is_none() && body.get("error_type").is_none(),
        "versioned success: no error member"
    );
    body["result"].clone()
}

/// The typed refusal: HTTP status, `error_type` token, the JSON type of `error_data` (`None`: the key is absent) and
/// the echoed `id`, in the mmrpc 2.0 error envelope.
fn assert_typed_refusal(answer: &(u16, Json), status: u16, token: &str, data: Option<&str>, id: usize) {
    let (code, body) = answer;
    assert_eq!(*code, status, "{token}: status");
    assert_eq!(body["mmrpc"], "2.0", "{token}: mmrpc version");
    assert_eq!(body["error_type"], token, "{token}: error_type");
    assert_eq!(body.get("id"), Some(&json!(id)), "{token}: id");
    match data {
        Some("object") => assert!(body["error_data"].is_object(), "{token}: error_data kind"),
        Some("string") => assert!(body["error_data"].is_string(), "{token}: error_data kind"),
        Some(other) => panic!("unknown kind of error_data: {other}"),
        None => assert!(body.get("error_data").is_none(), "{token}: error_data kind"),
    }
    assert!(
        body["error"].is_string() && body.get("result").is_none(),
        "{token}: error envelope"
    );
}

/// The member `member` of the `error_data` record of a typed refusal is `expected`; the message names the member only.
fn assert_data_member(answer: &(u16, Json), member: &str, expected: Json) {
    assert!(answer.1["error_data"][member] == expected, "error_data member {member}");
}

/// A node without coins, orders or swaps.
fn bare_node() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({ "rpc_password": "pw" }))
        .into_mm_arc()
}

// ----- The wire forms of the error enums -----

#[test]
fn my_balance_error_wire_forms() {
    let rejection = FeeCapBelowBaseFee {
        max_fee_per_gas: None,
        base_fee: None,
        node: None,
    };
    let rejection_text = rejection.to_string();
    assert_wire(
        &MyBalanceRpcError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin: BTC",
    );
    assert_wire(
        &MyBalanceRpcError::Transport("node down".into()),
        "Transport",
        Some("string"),
        502,
        "node down",
    );
    assert_wire(
        &MyBalanceRpcError::FeeCapBelowBaseFee(rejection),
        "FeeCapBelowBaseFee",
        Some("object"),
        500,
        &rejection_text,
    );
    assert_wire(
        &MyBalanceRpcError::InternalError("no address".into()),
        "InternalError",
        Some("string"),
        500,
        "no address",
    );
}

/// The balance errors of a coin land in the variants of the nearest sibling, `WithdrawError` (its conversion of the
/// same type), with the legacy text of the balance error.
#[test]
fn my_balance_error_classifies_the_balance_errors_of_a_coin() {
    let token = |error: BalanceError| json::to_value(MyBalanceRpcError::from(error)).unwrap()["error_type"].clone();
    assert_eq!(token(BalanceError::Transport("t".into())), "Transport");
    assert_eq!(token(BalanceError::InvalidResponse("r".into())), "Transport");
    assert_eq!(token(BalanceError::WalletStorageError("s".into())), "InternalError");
    assert_eq!(token(BalanceError::Internal("i".into())), "InternalError");
    assert_eq!(
        token(BalanceError::UnexpectedDerivationMethod(
            UnexpectedDerivationMethod::IguanaPrivKeyUnavailable
        )),
        "InternalError"
    );
    assert_eq!(
        token(BalanceError::FeeCapBelowBaseFee(FeeCapBelowBaseFee {
            max_fee_per_gas: None,
            base_fee: None,
            node: None,
        })),
        "FeeCapBelowBaseFee"
    );
    assert!(
        MyBalanceRpcError::from(BalanceError::Transport("t".into())).to_string() == "Transport: t",
        "the text of a balance error is the legacy text"
    );
}

#[test]
fn disable_coin_error_wire_forms() {
    let uuid = uuid::Uuid::nil();
    assert_wire(
        &DisableCoinError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin: BTC",
    );
    assert_wire(
        &DisableCoinError::ActiveSwapsBlockDeactivation {
            coin: "KMD".into(),
            swaps: vec![uuid.to_string()],
        },
        "ActiveSwapsBlockDeactivation",
        Some("object"),
        400,
        "There're active swaps using KMD",
    );
    assert_wire(
        &DisableCoinError::MatchingOrdersBlockDeactivation {
            coin: "KMD".into(),
            matching: vec![uuid.to_string()],
            cancelled: vec![],
        },
        "MatchingOrdersBlockDeactivation",
        Some("object"),
        400,
        "There're currently matching orders using KMD",
    );
    assert_wire(
        &DisableCoinError::InternalError("KMD is disabled already".into()),
        "InternalError",
        Some("string"),
        500,
        "KMD is disabled already",
    );
    // The records carry the lists, as the legacy error body does beside `error`.
    let wire = json::to_value(DisableCoinError::MatchingOrdersBlockDeactivation {
        coin: "KMD".into(),
        matching: vec![uuid.to_string()],
        cancelled: vec![uuid.to_string()],
    })
    .unwrap();
    assert_eq!(wire["error_data"]["coin"], "KMD");
    assert!(wire["error_data"]["matching"].is_array() && wire["error_data"]["cancelled"].is_array());
    let wire = json::to_value(DisableCoinError::ActiveSwapsBlockDeactivation {
        coin: "KMD".into(),
        swaps: vec![uuid.to_string()],
    })
    .unwrap();
    assert!(wire["error_data"]["swaps"].is_array());
}

#[test]
fn send_raw_transaction_error_wire_forms() {
    assert_wire(
        &SendRawTransactionError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin: BTC",
    );
    assert_wire(
        &SendRawTransactionError::InvalidParam("'tx_hex' must be a string".into()),
        "InvalidParam",
        Some("string"),
        400,
        "'tx_hex' must be a string",
    );
    assert_wire(
        &SendRawTransactionError::Transport("refused".into()),
        "Transport",
        Some("string"),
        502,
        "refused",
    );
    assert_wire(
        &SendRawTransactionError::InternalError("lp_coinfind".into()),
        "InternalError",
        Some("string"),
        500,
        "lp_coinfind",
    );
}

#[test]
fn address_error_wire_forms() {
    assert_wire(
        &ValidateAddressError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin: BTC",
    );
    assert_wire(
        &ValidateAddressError::InternalError("x".into()),
        "InternalError",
        Some("string"),
        500,
        "x",
    );
    assert_wire(
        &ConvertAddressError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin: BTC",
    );
    assert_wire(
        &ConvertAddressError::InvalidParam("conversion refused".into()),
        "InvalidParam",
        Some("string"),
        400,
        "conversion refused",
    );
    assert_wire(
        &ConvertAddressError::InternalError("x".into()),
        "InternalError",
        Some("string"),
        500,
        "x",
    );
    assert_wire(
        &ConvertUtxoAddressError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "Coin BTC is not activated",
    );
    assert_wire(
        &ConvertUtxoAddressError::InvalidAddress("bad".into()),
        "InvalidAddress",
        Some("string"),
        400,
        "bad",
    );
    assert_wire(
        &ConvertUtxoAddressError::UnsupportedError("Coin ETH is not utxo".into()),
        "UnsupportedError",
        Some("string"),
        400,
        "Coin ETH is not utxo",
    );
}

#[test]
fn kmd_rewards_info_and_kick_start_and_setting_error_wire_forms() {
    assert_wire(
        &KmdRewardsInfoError::NoSuchCoin { coin: "KMD".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "KMD is not activated",
    );
    assert_wire(
        &KmdRewardsInfoError::Transport("node down".into()),
        "Transport",
        Some("string"),
        502,
        "node down",
    );
    assert_wire(
        &KmdRewardsInfoError::InternalError("KMD was expected to be UTXO".into()),
        "InternalError",
        Some("string"),
        500,
        "KMD was expected to be UTXO",
    );
    assert_wire(
        &CoinsNeededForKickStartError::InternalError("poisoned".into()),
        "InternalError",
        Some("string"),
        500,
        "poisoned",
    );
    assert_wire(
        &CoinSettingError::NoSuchCoin { coin: "BTC".into() },
        "NoSuchCoin",
        Some("object"),
        404,
        "No such coin BTC",
    );
    assert_wire(
        &CoinSettingError::InternalError("x".into()),
        "InternalError",
        Some("string"),
        500,
        "x",
    );
}

// ----- The legacy answer of a refused `disable_coin` -----

/// CRD ch.43 R43.1E.3 with D43.1E.1 option (c): every refusal for a coin in use and the refusal for a coin that is not
/// active answer HTTP 500 and the extended body with the four members `error`, `orders` (`matching`, `cancelled`),
/// `active_swaps` and `swaps`, in this order, all always present; `active_swaps` and `swaps` hold the same list.
#[test]
fn disable_coin_legacy_refusals_are_the_extended_body() {
    let uuid = uuid::Uuid::nil();
    let answer = legacy_disable_coin_refusal(MmError::new(DisableCoinError::ActiveSwapsBlockDeactivation {
        coin: "KMD".into(),
        swaps: vec![uuid.to_string()],
    }))
    .unwrap_or_else(|_| panic!("an answer"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"error":"There're active swaps using KMD","orders":{"matching":[],"cancelled":[]},"active_swaps":["00000000-0000-0000-0000-000000000000"],"swaps":["00000000-0000-0000-0000-000000000000"]}"#
            .to_owned(),
        "the active-swaps body",
    );

    let answer = legacy_disable_coin_refusal(MmError::new(DisableCoinError::MatchingOrdersBlockDeactivation {
        coin: "KMD".into(),
        matching: vec![uuid.to_string()],
        cancelled: vec![],
    }))
    .unwrap_or_else(|_| panic!("an answer"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"error":"There're currently matching orders using KMD","orders":{"matching":["00000000-0000-0000-0000-000000000000"],"cancelled":[]},"active_swaps":[],"swaps":[]}"#
            .to_owned(),
        "the matching-orders body",
    );

    // The orders that this call cancelled before the refusal are listed.
    let answer = legacy_disable_coin_refusal(MmError::new(DisableCoinError::MatchingOrdersBlockDeactivation {
        coin: "KMD".into(),
        matching: vec![uuid.to_string()],
        cancelled: vec![uuid.to_string()],
    }))
    .unwrap_or_else(|_| panic!("an answer"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"error":"There're currently matching orders using KMD","orders":{"matching":["00000000-0000-0000-0000-000000000000"],"cancelled":["00000000-0000-0000-0000-000000000000"]},"active_swaps":[],"swaps":[]}"#
            .to_owned(),
        "the matching-orders body with cancelled orders",
    );

    // A coin that is not active: the extended body with empty lists.
    let answer = legacy_disable_coin_refusal(MmError::new(DisableCoinError::NoSuchCoin { coin: "KMD".into() }))
        .unwrap_or_else(|_| panic!("an answer"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"error":"No such coin: KMD","orders":{"matching":[],"cancelled":[]},"active_swaps":[],"swaps":[]}"#
            .to_owned(),
        "the not-active body",
    );

    // A node failure that has no lists is the legacy error text.
    let refusal = legacy_disable_coin_refusal(MmError::new(DisableCoinError::InternalError("registry".into())));
    match refusal {
        Err(error) => {
            let (files, rest) = split_location(&error);
            // The location is that of the caller of the helper, which is this test.
            assert!(
                files == vec!["wallet_rpc_tests".to_owned()],
                "the refusal names its caller"
            );
            assert!(rest == "registry", "the refusal text");
        },
        Ok(_) => panic!("a refusal without lists is a legacy error"),
    }
}

// ----- `my_balance` -----

#[test]
fn my_balance_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    TestCoin::my_balance.mock_safe(|_| {
        MockResult::Return(Box::new(futures01::future::ok(CoinBalance {
            spendable: bd("12.5"),
            unspendable: bd("0.5"),
        })))
    });
    TestCoin::my_address.mock_safe(|_| MockResult::Return(Ok("dyn-address".to_owned())));

    // The body of the dev `lp_commands_legacy::my_balance`: the bare object `json!({ "coin": ticker, "balance":
    // spendable, "unspendable_balance": unspendable, "address": address })`.
    let dev_body = json::to_string(&json!({
        "coin": "DYN",
        "balance": bd("12.5"),
        "unspendable_balance": bd("0.5"),
        "address": "dyn-address",
    }))
    .unwrap();
    let legacy = legacy_success(ctx, "my_balance", json!({ "coin": "DYN" }));
    same(&legacy, &dev_body, "the legacy body");

    // The versioned result is the bare legacy body, unchanged; an unknown member of `params` is ignored.
    let result = versioned_result(
        versioned(
            ctx,
            "my_balance",
            Some(json!({ "coin": "DYN", "unknown_member": true })),
            11,
        ),
        11,
    );
    same(
        &result,
        &json::from_str::<Json>(&dev_body).unwrap(),
        "the versioned result",
    );

    // Refusals: legacy text and versioned token.
    same(
        &legacy_refusal(ctx, "my_balance", json!({ "coin": "NOPE" }), "lp_commands_legacy"),
        &"No such coin: NOPE".to_owned(),
        "the unknown-coin text",
    );
    same(
        &legacy_refusal(ctx, "my_balance", json!({}), "lp_commands_legacy"),
        &"No 'coin' field".to_owned(),
        "the missing-coin text",
    );
    let answer = versioned(ctx, "my_balance", Some(json!({ "coin": "NOPE" })), 12);
    assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), 12);
    assert_data_member(&answer, "coin", json!("NOPE"));
    // A request without the member the legacy form needs is undecodable on the versioned surface.
    assert_typed_refusal(
        &versioned(ctx, "my_balance", Some(json!({})), 13),
        400,
        "InvalidRequest",
        Some("string"),
        13,
    );
}

/// A loopback TRON node on the Nile network for the `my_balance` test of CRD ch.63: an account of 2.5 TRX, a 6-decimal
/// token balance of 1.234567 at every address, and a GasFree controller contract that answers `getGasFreeAddress`
/// with the address `coins` derives for the asked user. `controller_reads` counts those reads. It stops when `stop`
/// is set.
#[cfg(not(target_arch = "wasm32"))]
fn tron_stub_node(stop: std::sync::Arc<AtomicBool>, controller_reads: std::sync::Arc<AtomicU64>) -> String {
    use coins::eth::tron::{gasfree_custody_address, Network, TronAddress};
    use std::io::{Read, Write};

    fn word(value: u64) -> String { format!("{value:064x}") }

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                },
            };
            stream.set_nonblocking(false).ok();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let (path, body) = loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break (String::new(), Json::Null),
                    Ok(n) => n,
                };
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .to_lowercase()
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if rest.len() >= len {
                        let path = head.split_whitespace().nth(1).unwrap_or_default().to_owned();
                        break (path, json::from_str(rest).unwrap_or(Json::Null));
                    }
                }
            };
            let answer = match (path.as_str(), body["function_selector"].as_str()) {
                ("/wallet/getaccount", _) => Some(json!({ "balance": 2_500_000, "create_time": 1 })),
                ("/wallet/triggerconstantcontract", Some("decimals()")) => {
                    Some(json!({ "constant_result": [word(6)] }))
                },
                ("/wallet/triggerconstantcontract", Some("balanceOf(address)")) => {
                    Some(json!({ "constant_result": [word(1_234_567)] }))
                },
                ("/wallet/triggerconstantcontract", Some("getGasFreeAddress(address)")) => {
                    controller_reads.fetch_add(1, Ordering::SeqCst);
                    // The parameter is one ABI word: 12 zero bytes and the 20-byte user.
                    body["parameter"]
                        .as_str()
                        .and_then(|parameter| parameter.get(24..))
                        .and_then(|payload| TronAddress::parse(&format!("41{payload}")).ok())
                        .map(|user| {
                            let custody = gasfree_custody_address(&Network::Nile, &user);
                            json!({ "constant_result": [format!("{:0>64}", hex::encode(custody.to_evm_address()))] })
                        })
                },
                _ => None,
            };
            let (status, reply) = match answer {
                Some(answer) => (200, answer.to_string()),
                None => (404, "{}".to_owned()),
            };
            let response = format!(
                "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// CRD ch.63 T10 (§63.3.3, R18) for `my_balance`: a TRX coin whose activation gave a GasFree provider, and its TRC20
/// token, answer `gasfree_address` beside `address`, equal to the local derivation for that address, on the legacy
/// and on the versioned surface; the platform and the token share one controller confirmation (R29). Without a
/// provider the member is absent from the body text, not `null`. The coins are built with the coin builder the
/// activation routes use.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn my_balance_of_a_tron_coin_carries_the_gasfree_address_only_with_a_provider() {
    use coins::eth::tron::nodes::TronNode;
    use coins::eth::tron::{gasfree_custody_address, parse_trc20_contract, tron_coin_from_params, GasFreeBinding,
                           Network, TronAddress, TronCoinParams};
    use coins::CoinProtocol;

    const API_KEY: &str = "key-0f3a9c";
    const API_SECRET: &str = "secret-7b1d4e";
    const CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let controller_reads = std::sync::Arc::new(AtomicU64::new(0));
    let url = tron_stub_node(stop.clone(), controller_reads.clone());
    let mut key = [0u8; 32];
    key[31] = 1;

    // Registers a TRX platform coin with the provider binding `gasfree` and its 6-decimal token; returns the address.
    let node_with_coins = |gasfree: Option<GasFreeBinding>| -> (MmArc, String) {
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "rpc_password": "pw", "coins": [
                { "coin": "TRX", "mm2": 1, "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } } },
                { "coin": "USDT-TRC20", "mm2": 1, "decimals": 6, "protocol": { "type": "TRC20", "protocol_data": {
                    "platform": "TRX", "contract_address": CONTRACT } } },
            ] }))
            .into_mm_arc();
        let params = TronCoinParams {
            nodes: vec![TronNode::plain(url.clone())],
            required_confirmations: 1,
            tx_history: false,
            gasfree,
        };
        let platform = block_on(tron_coin_from_params(
            &ctx,
            "TRX",
            &coins::coin_conf(&ctx, "TRX"),
            params,
            &key,
            CoinProtocol::TRX { network: Network::Nile },
        ))
        .unwrap_or_else(|_| panic!("the platform coin builds"));
        let contract = parse_trc20_contract(CONTRACT).expect("a contract");
        let token = block_on(platform.trc20_token_from_conf_or_contract("USDT-TRC20".to_owned(), contract, 1, None))
            .unwrap_or_else(|_| panic!("the token builds"));
        let address = platform.my_address().expect("an address");
        register_in(&ctx, platform.into());
        register_in(&ctx, token.into());
        (ctx, address)
    };
    let provider = json::from_value(json!({
        "base_url": "https://open.gasfree.io",
        "api_key": API_KEY,
        "api_secret": API_SECRET,
        "service_provider": CONTRACT,
    }))
    .expect("a provider object");
    let binding = GasFreeBinding::from_request(&Network::Nile, &provider).expect("a valid provider object");

    // With a provider on the platform.
    let (ctx, address) = node_with_coins(Some(binding));
    let user = TronAddress::from_base58(&address).expect("a valid address");
    let custody = gasfree_custody_address(&Network::Nile, &user).to_base58();
    for (ticker, balance, id) in [("TRX", "2.5", 31), ("USDT-TRC20", "1.234567", 32)] {
        let expected = json!({
            "coin": ticker,
            "balance": balance,
            "unspendable_balance": "0",
            "address": address,
            "gasfree_address": custody,
        });
        let legacy = legacy_success(&ctx, "my_balance", json!({ "coin": ticker }));
        assert!(
            !legacy.contains(API_KEY) && !legacy.contains(API_SECRET),
            "{ticker}: a credential in the body"
        );
        same(
            &json::from_str::<Json>(&legacy).unwrap(),
            &expected,
            "the legacy body with the custody address",
        );
        // The member stands beside `address`, after it.
        assert!(
            legacy.find("\"address\"") < legacy.find("\"gasfree_address\""),
            "{ticker}: the member order"
        );
        let result = versioned_result(versioned(&ctx, "my_balance", Some(json!({ "coin": ticker })), id), id);
        same(&result, &expected, "the versioned result with the custody address");
    }
    assert_eq!(
        controller_reads.load(Ordering::SeqCst),
        1,
        "one confirmation serves the platform, its token and the later reports"
    );

    // Without a provider: the legacy body is the one it always was.
    let (ctx, address) = node_with_coins(None);
    for (ticker, balance, id) in [("TRX", "2.5", 33), ("USDT-TRC20", "1.234567", 34)] {
        let expected = json!({
            "coin": ticker,
            "balance": balance,
            "unspendable_balance": "0",
            "address": address,
        });
        let legacy = legacy_success(&ctx, "my_balance", json!({ "coin": ticker }));
        assert!(
            !legacy.contains("gasfree_address") && !legacy.contains("null"),
            "{ticker}: the member is absent"
        );
        same(
            &json::from_str::<Json>(&legacy).unwrap(),
            &expected,
            "the legacy body without a provider",
        );
        let result = versioned_result(versioned(&ctx, "my_balance", Some(json!({ "coin": ticker })), id), id);
        same(&result, &expected, "the versioned result without a provider");
    }
    assert_eq!(
        controller_reads.load(Ordering::SeqCst),
        1,
        "no provider, no controller read"
    );
    stop.store(true, Ordering::SeqCst);
}

#[test]
fn my_balance_classifies_the_failures_of_the_coin() {
    let node = Node::new();
    let ctx = &node.ctx;
    TestCoin::my_balance.mock_safe(|_| {
        MockResult::Return(Box::new(futures01::future::err::<CoinBalance, MmError<BalanceError>>(
            MmError::new(BalanceError::Transport("node down".into())),
        )))
    });
    same(
        &legacy_refusal(ctx, "my_balance", json!({ "coin": "DYN" }), "lp_commands_legacy"),
        &"Transport: node down".to_owned(),
        "the transport text",
    );
    assert_typed_refusal(
        &versioned(ctx, "my_balance", Some(json!({ "coin": "DYN" })), 21),
        502,
        "Transport",
        Some("string"),
        21,
    );

    TestCoin::my_balance
        .mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(CoinBalance::new(BigDecimal::from(1))))));
    TestCoin::my_address.mock_safe(|_| MockResult::Return(Err("no address".to_owned())));
    same(
        &legacy_refusal(ctx, "my_balance", json!({ "coin": "DYN" }), "lp_commands_legacy"),
        &"no address".to_owned(),
        "the address text",
    );
    assert_typed_refusal(
        &versioned(ctx, "my_balance", Some(json!({ "coin": "DYN" })), 22),
        500,
        "InternalError",
        Some("string"),
        22,
    );
}

// ----- `validateaddress` and `convertaddress` -----

fn mock_address_checks() {
    TestCoin::validate_address.mock_safe(|_, address| {
        MockResult::Return(if address == "good" {
            ValidateAddressResult {
                is_valid: true,
                reason: None,
            }
        } else {
            ValidateAddressResult {
                is_valid: false,
                reason: Some("not an address".to_owned()),
            }
        })
    });
    TestCoin::convert_to_address.mock_safe(|_, from, _format| {
        MockResult::Return(if from == "bad" {
            Err("conversion refused".to_owned())
        } else {
            Ok(format!("converted-{from}"))
        })
    });
}

#[test]
fn validate_address_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    mock_address_checks();

    // The body of the dev `coins::validate_address`: `json!({ "result": ValidateAddressResult })`, `reason` absent
    // for a valid address.
    same(
        &legacy_success(ctx, "validateaddress", json!({ "coin": "DYN", "address": "good" })),
        &r#"{"result":{"is_valid":true}}"#.to_owned(),
        "the valid body",
    );
    same(
        &legacy_success(ctx, "validateaddress", json!({ "coin": "DYN", "address": "bad" })),
        &r#"{"result":{"is_valid":false,"reason":"not an address"}}"#.to_owned(),
        "the invalid body",
    );

    // An invalid address is a success on the versioned surface too, never an error.
    let valid = versioned_result(
        versioned(
            ctx,
            "validateaddress",
            Some(json!({ "coin": "DYN", "address": "good", "unknown_member": 1 })),
            31,
        ),
        31,
    );
    same(&valid, &json!({ "is_valid": true }), "the valid result");
    let invalid = versioned_result(
        versioned(
            ctx,
            "validateaddress",
            Some(json!({ "coin": "DYN", "address": "bad" })),
            32,
        ),
        32,
    );
    same(
        &invalid,
        &json!({ "is_valid": false, "reason": "not an address" }),
        "the invalid result",
    );

    same(
        &legacy_refusal(
            ctx,
            "validateaddress",
            json!({ "coin": "NOPE", "address": "good" }),
            "dispatcher_legacy",
        ),
        &"No such coin: NOPE".to_owned(),
        "the unknown-coin text",
    );
    assert_typed_refusal(
        &versioned(
            ctx,
            "validateaddress",
            Some(json!({ "coin": "NOPE", "address": "good" })),
            33,
        ),
        404,
        "NoSuchCoin",
        Some("object"),
        33,
    );
}

#[test]
fn convert_address_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    mock_address_checks();
    let params = json!({ "coin": "DYN", "from": "addr", "to_address_format": { "format": "mixedcase" } });

    // The body of the dev `coins::convert_address`: `json!({ "result": { "address": <converted> } })`.
    same(
        &legacy_success(ctx, "convertaddress", params.clone()),
        &r#"{"result":{"address":"converted-addr"}}"#.to_owned(),
        "the legacy body",
    );
    let mut versioned_params = params.clone();
    versioned_params["unknown_member"] = json!(true);
    let result = versioned_result(versioned(ctx, "convertaddress", Some(versioned_params), 41), 41);
    same(&result, &json!({ "address": "converted-addr" }), "the versioned result");

    // The coin's conversion refuses the address: caller-side, with the coin's text.
    let refused = json!({ "coin": "DYN", "from": "bad", "to_address_format": {} });
    same(
        &legacy_refusal(ctx, "convertaddress", refused.clone(), "dispatcher_legacy"),
        &"conversion refused".to_owned(),
        "the conversion text",
    );
    assert_typed_refusal(
        &versioned(ctx, "convertaddress", Some(refused), 42),
        400,
        "InvalidParam",
        Some("string"),
        42,
    );

    let unknown = json!({ "coin": "NOPE", "from": "addr", "to_address_format": {} });
    same(
        &legacy_refusal(ctx, "convertaddress", unknown.clone(), "dispatcher_legacy"),
        &"No such coin: NOPE".to_owned(),
        "the unknown-coin text",
    );
    assert_typed_refusal(
        &versioned(ctx, "convertaddress", Some(unknown), 43),
        404,
        "NoSuchCoin",
        Some("object"),
        43,
    );

    // A body that does not decode keeps the text of the decoder on the legacy surface.
    same(
        &legacy_refusal(ctx, "convertaddress", json!({ "coin": "DYN" }), "dispatcher_legacy"),
        &"missing field `from`".to_owned(),
        "the decoder text",
    );
}

/// A UTXO coin is not activated on a test node, so the success of `convert_utxo_address` is not reachable here; the
/// three refusals are. The address is a valid transparent address of a UTXO chain (it is the one that the history
/// storage tests use).
#[test]
fn convert_utxo_address_refusals() {
    let node = Node::new();
    let ctx = &node.ctx;
    let address = "RRnMcSeKiLrNdbp91qNVQwwXx5azD4S4CD";

    // The address does not parse: the dev handler's `try_s!(req.address.parse())` came before the coin lookup.
    let params = json!({ "address": "not an address", "to_coin": "DYN" });
    let (files, _text) = split_location(
        &match run(ctx, legacy_request("convert_utxo_address", params.clone())) {
            Ok(_) => panic!("a legacy refusal was expected"),
            Err(error) => error,
        },
    );
    assert!(
        files.last().map(String::as_str) == Some("dispatcher_legacy"),
        "the legacy error names its handler file"
    );
    assert_typed_refusal(
        &versioned(ctx, "convert_utxo_address", Some(params), 51),
        400,
        "InvalidAddress",
        Some("string"),
        51,
    );

    // The target coin is not activated (one text for an unknown coin and for a registry that cannot be read).
    let params = json!({ "address": address, "to_coin": "NOPE" });
    same(
        &legacy_refusal(ctx, "convert_utxo_address", params.clone(), "dispatcher_legacy"),
        &"Coin NOPE is not activated".to_owned(),
        "the not-activated text",
    );
    let answer = versioned(ctx, "convert_utxo_address", Some(params), 52);
    assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), 52);
    assert_data_member(&answer, "coin", json!("NOPE"));

    // The target coin is not a UTXO coin.
    let params = json!({ "address": address, "to_coin": "DYN", "unknown_member": 1 });
    same(
        &legacy_refusal(ctx, "convert_utxo_address", params.clone(), "dispatcher_legacy"),
        &"Coin DYN is not utxo".to_owned(),
        "the not-utxo text",
    );
    assert_typed_refusal(
        &versioned(ctx, "convert_utxo_address", Some(params), 53),
        400,
        "UnsupportedError",
        Some("string"),
        53,
    );
}

// ----- `send_raw_transaction` -----

#[test]
fn send_raw_transaction_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    TestCoin::send_raw_tx.mock_safe(|_, tx| {
        let answer: Result<String, String> = if tx == "dead" {
            Err("broadcast refused".to_owned())
        } else {
            Ok(format!("hash:{tx}"))
        };
        MockResult::Return(Box::new(futures01::future::result(answer)))
    });
    TestCoin::send_raw_tx_bytes.mock_safe(|_, tx| {
        MockResult::Return(Box::new(futures01::future::ok::<String, String>(format!(
            "bytes:{}",
            tx.len()
        ))))
    });

    // The body of the dev `coins::send_raw_transaction`: the bare object `json!({ "tx_hash": hash })`.
    same(
        &legacy_success(ctx, "send_raw_transaction", json!({ "coin": "DYN", "tx_hex": "00ff" })),
        &r#"{"tx_hash":"hash:00ff"}"#.to_owned(),
        "the hex body",
    );
    // The JSON carrier goes to the bytes-taking half of the coin: `{"a":1}` is seven bytes.
    same(
        &legacy_success(
            ctx,
            "send_raw_transaction",
            json!({ "coin": "DYN", "tx_json": { "a": 1 } }),
        ),
        &r#"{"tx_hash":"bytes:7"}"#.to_owned(),
        "the JSON-carrier body",
    );
    // `tx_hex` wins when both carriers are present; a `null` carrier counts as absent.
    same(
        &legacy_success(
            ctx,
            "send_raw_transaction",
            json!({ "coin": "DYN", "tx_hex": "00ff", "tx_json": { "a": 1 } }),
        ),
        &r#"{"tx_hash":"hash:00ff"}"#.to_owned(),
        "both carriers",
    );
    same(
        &legacy_success(
            ctx,
            "send_raw_transaction",
            json!({ "coin": "DYN", "tx_hex": null, "tx_json": { "a": 1 } }),
        ),
        &r#"{"tx_hash":"bytes:7"}"#.to_owned(),
        "a null carrier",
    );

    // The versioned result is the bare legacy body, unchanged.
    let result = versioned_result(
        versioned(
            ctx,
            "send_raw_transaction",
            Some(json!({ "coin": "DYN", "tx_hex": "00ff", "unknown_member": 1 })),
            61,
        ),
        61,
    );
    same(&result, &json!({ "tx_hash": "hash:00ff" }), "the versioned result");

    // A broadcast the coin refuses.
    let refused = json!({ "coin": "DYN", "tx_hex": "dead" });
    same(
        &legacy_refusal(ctx, "send_raw_transaction", refused.clone(), "dispatcher_legacy"),
        &"broadcast refused".to_owned(),
        "the broadcast text",
    );
    assert_typed_refusal(
        &versioned(ctx, "send_raw_transaction", Some(refused), 62),
        502,
        "Transport",
        Some("string"),
        62,
    );

    // The carrier checks.
    for (params, text) in [
        (
            json!({ "coin": "DYN" }),
            "Request must carry either a 'tx_hex' or a 'tx_json' field",
        ),
        (json!({ "coin": "DYN", "tx_hex": 42 }), "'tx_hex' must be a string"),
        (
            json!({ "coin": "DYN", "tx_json": "text" }),
            "'tx_json' must be an object",
        ),
    ] {
        same(
            &legacy_refusal(ctx, "send_raw_transaction", params.clone(), "dispatcher_legacy"),
            &text.to_owned(),
            "the carrier text",
        );
        assert_typed_refusal(
            &versioned(ctx, "send_raw_transaction", Some(params), 63),
            400,
            "InvalidParam",
            Some("string"),
            63,
        );
    }

    // An unknown coin, and the legacy text of a request without `coin`.
    same(
        &legacy_refusal(
            ctx,
            "send_raw_transaction",
            json!({ "coin": "NOPE", "tx_hex": "00ff" }),
            "dispatcher_legacy",
        ),
        &"No such coin: NOPE".to_owned(),
        "the unknown-coin text",
    );
    same(
        &legacy_refusal(
            ctx,
            "send_raw_transaction",
            json!({ "tx_hex": "00ff" }),
            "dispatcher_legacy",
        ),
        &"No 'coin' field".to_owned(),
        "the missing-coin text",
    );
    assert_typed_refusal(
        &versioned(
            ctx,
            "send_raw_transaction",
            Some(json!({ "coin": "NOPE", "tx_hex": "00ff" })),
            64,
        ),
        404,
        "NoSuchCoin",
        Some("object"),
        64,
    );
}

// ----- `set_required_confirmations` and `set_requires_notarization` -----

#[test]
fn set_required_confirmations_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    static SET_TO: AtomicU64 = AtomicU64::new(0);
    TestCoin::set_required_confirmations.mock_safe(|_, confirmations| {
        SET_TO.store(confirmations, Ordering::SeqCst);
        MockResult::Return(())
    });
    // The answer carries the setting as the coin reports it after the change.
    TestCoin::required_confirmations.mock_safe(|_| MockResult::Return(5));

    // The body of the dev `coins::set_required_confirmations`: `json!({ "result": { "coin": ..., "confirmations":
    // coin.required_confirmations() } })`.
    same(
        &legacy_success(
            ctx,
            "set_required_confirmations",
            json!({ "coin": "DYN", "confirmations": 5 }),
        ),
        &r#"{"result":{"coin":"DYN","confirmations":5}}"#.to_owned(),
        "the legacy body",
    );
    assert_eq!(SET_TO.load(Ordering::SeqCst), 5, "the coin is told the number");

    let result = versioned_result(
        versioned(
            ctx,
            "set_required_confirmations",
            Some(json!({ "coin": "DYN", "confirmations": 7, "unknown_member": 1 })),
            71,
        ),
        71,
    );
    same(
        &result,
        &json!({ "coin": "DYN", "confirmations": 5 }),
        "the versioned result",
    );
    assert_eq!(SET_TO.load(Ordering::SeqCst), 7, "the coin is told the number");

    same(
        &legacy_refusal(
            ctx,
            "set_required_confirmations",
            json!({ "coin": "NOPE", "confirmations": 1 }),
            "dispatcher_legacy",
        ),
        &"No such coin NOPE".to_owned(),
        "the unknown-coin text",
    );
    let answer = versioned(
        ctx,
        "set_required_confirmations",
        Some(json!({ "coin": "NOPE", "confirmations": 1 })),
        72,
    );
    assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), 72);
    assert_data_member(&answer, "coin", json!("NOPE"));

    // A body that does not decode keeps the text of the decoder on the legacy surface and is `InvalidRequest` on the
    // versioned one.
    same(
        &legacy_refusal(
            ctx,
            "set_required_confirmations",
            json!({ "coin": "DYN" }),
            "dispatcher_legacy",
        ),
        &"missing field `confirmations`".to_owned(),
        "the decoder text",
    );
    assert_typed_refusal(
        &versioned(ctx, "set_required_confirmations", Some(json!({ "coin": "DYN" })), 73),
        400,
        "InvalidRequest",
        Some("string"),
        73,
    );
}

#[test]
fn set_requires_notarization_answers_the_legacy_body_and_the_versioned_result() {
    let node = Node::new();
    let ctx = &node.ctx;
    static SET_TO: AtomicBool = AtomicBool::new(false);
    TestCoin::set_requires_notarization.mock_safe(|_, requires_nota| {
        SET_TO.store(requires_nota, Ordering::SeqCst);
        MockResult::Return(())
    });
    TestCoin::requires_notarization.mock_safe(|_| MockResult::Return(true));

    // The body of the dev `coins::set_requires_notarization`: `json!({ "result": { "coin": ...,
    // "requires_notarization": coin.requires_notarization() } })`.
    same(
        &legacy_success(
            ctx,
            "set_requires_notarization",
            json!({ "coin": "DYN", "requires_notarization": true }),
        ),
        &r#"{"result":{"coin":"DYN","requires_notarization":true}}"#.to_owned(),
        "the legacy body",
    );
    assert!(SET_TO.load(Ordering::SeqCst), "the coin is told the setting");

    let result = versioned_result(
        versioned(
            ctx,
            "set_requires_notarization",
            Some(json!({ "coin": "DYN", "requires_notarization": true, "unknown_member": 1 })),
            81,
        ),
        81,
    );
    same(
        &result,
        &json!({ "coin": "DYN", "requires_notarization": true }),
        "the versioned result",
    );

    same(
        &legacy_refusal(
            ctx,
            "set_requires_notarization",
            json!({ "coin": "NOPE", "requires_notarization": true }),
            "dispatcher_legacy",
        ),
        &"No such coin NOPE".to_owned(),
        "the unknown-coin text",
    );
    assert_typed_refusal(
        &versioned(
            ctx,
            "set_requires_notarization",
            Some(json!({ "coin": "NOPE", "requires_notarization": true })),
            82,
        ),
        404,
        "NoSuchCoin",
        Some("object"),
        82,
    );
}

// ----- `kmd_rewards_info` and `coins_needed_for_kick_start` -----

/// KMD is not activated: the same refusal whatever form `params` takes (absent, `null`, `{}`, or an object with
/// members, which are ignored). The success needs a UTXO coin and a node and is not reachable on a test node.
#[test]
fn kmd_rewards_info_refuses_a_node_without_kmd_for_every_form_of_params() {
    let ctx = bare_node();
    same(
        &legacy_refusal(&ctx, "kmd_rewards_info", json!({}), "dispatcher_legacy"),
        &"KMD is not activated".to_owned(),
        "the not-activated text",
    );
    for (params, id) in [
        (None, 91),
        (Some(Json::Null), 92),
        (Some(json!({})), 93),
        (Some(json!({ "unknown_member": 1 })), 94),
    ] {
        let answer = versioned(&ctx, "kmd_rewards_info", params, id);
        assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), id);
        assert_data_member(&answer, "coin", json!("KMD"));
    }
}

/// KMD is activated as a coin that is not a UTXO coin (the test coin): the dev handler's `ERR!("KMD was expected to
/// be UTXO")`.
#[test]
fn kmd_rewards_info_refuses_a_kmd_that_is_not_utxo() {
    let node = Node::new();
    let ctx = &node.ctx;
    same(
        &legacy_refusal(ctx, "kmd_rewards_info", json!({}), "dispatcher_legacy"),
        &"KMD was expected to be UTXO".to_owned(),
        "the not-utxo text",
    );
    assert_typed_refusal(
        &versioned(ctx, "kmd_rewards_info", None, 95),
        500,
        "InternalError",
        Some("string"),
        95,
    );
}

#[test]
fn coins_needed_for_kick_start_answers_the_legacy_body_and_the_versioned_result() {
    let ctx = bare_node();

    // The body of the dev `swap_rpc::coins_needed_for_kick_start`: `json!({ "result": <the set of tickers> })`.
    same(
        &legacy_success(&ctx, "coins_needed_for_kick_start", json!({})),
        &r#"{"result":[]}"#.to_owned(),
        "the empty body",
    );
    ctx.coins_needed_for_kick_start.lock().unwrap().insert("KMD".to_owned());
    same(
        &legacy_success(&ctx, "coins_needed_for_kick_start", json!({})),
        &r#"{"result":["KMD"]}"#.to_owned(),
        "the legacy body",
    );

    // `params` absent, `null`, `{}` and an object with members all succeed with the list under `result`.
    for (params, id) in [
        (None, 101),
        (Some(Json::Null), 102),
        (Some(json!({})), 103),
        (Some(json!({ "unknown_member": 1 })), 104),
    ] {
        let result = versioned_result(versioned(&ctx, "coins_needed_for_kick_start", params, id), id);
        same(&result, &json!(["KMD"]), "the versioned result");
    }
}

// ----- `disable_coin` -----

/// A coin without orders or swaps is disabled; the answer lists the orders the call cancelled. The legacy body is the
/// one of the dev `lp_commands_legacy::disable_coin`: `json!({ "result": { "coin": ticker, "cancelled_orders":
/// cancelled } })`.
#[test]
fn disable_coin_answers_the_legacy_body() {
    let node = Node::new();
    let ctx = &node.ctx;
    let order = node.add_order(true, true);
    let expected = json::to_string(&json!({ "result": { "coin": "DYN", "cancelled_orders": [order.uuid] } })).unwrap();
    same(
        &legacy_success(ctx, "disable_coin", json!({ "coin": "DYN" })),
        &expected,
        "the legacy body",
    );
    assert!(!node.in_container(&order.uuid), "the order is cancelled");
    // The coin is gone: CRD ch.43 R43.1E.3 with D43.1E.1 option (c), the extended body with empty lists, HTTP 500.
    let answer = run(ctx, legacy_request("disable_coin", json!({ "coin": "DYN" })))
        .unwrap_or_else(|_| panic!("the legacy not-active answer is a response"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &r#"{"error":"No such coin: DYN","orders":{"matching":[],"cancelled":[]},"active_swaps":[],"swaps":[]}"#
            .to_owned(),
        "the not-active body",
    );
    // No coin named: the plain legacy error.
    same(
        &legacy_refusal(ctx, "disable_coin", json!({}), "lp_commands_legacy"),
        &"No 'coin' field".to_owned(),
        "the missing-coin text",
    );
}

#[test]
fn disable_coin_answers_the_versioned_result_and_its_refusals() {
    let node = Node::new();
    let ctx = &node.ctx;
    let order = node.add_order(true, true);
    let result = versioned_result(
        versioned(
            ctx,
            "disable_coin",
            Some(json!({ "coin": "DYN", "unknown_member": true })),
            111,
        ),
        111,
    );
    same(
        &result,
        &json!({ "coin": "DYN", "cancelled_orders": [order.uuid] }),
        "the versioned result",
    );

    let answer = versioned(ctx, "disable_coin", Some(json!({ "coin": "DYN" })), 112);
    assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), 112);
    assert_data_member(&answer, "coin", json!("DYN"));
}

/// An order of the coin that is being matched blocks the deactivation: the legacy answer is HTTP 500 with the extended
/// body of CRD ch.43 R43.1E.3 with D43.1E.1 option (c), `{"error", "orders": {"matching", "cancelled"}, "active_swaps",
/// "swaps"}`, and the versioned answer is the typed conflict.
/// The coin stays.
#[test]
fn disable_coin_is_blocked_by_an_order_that_is_being_matched() {
    let node = Node::new();
    let ctx = &node.ctx;
    block_on(
        CoinsContext::from_ctx(ctx)
            .unwrap()
            .add_coin(MmCoinEnum::Test(TestCoin::new("ETH"))),
    )
    .unwrap();
    let matching = node.add_order_with_an_ongoing_match();

    let answer = run(ctx, legacy_request("disable_coin", json!({ "coin": "ETH" })))
        .unwrap_or_else(|_| panic!("the legacy blocked answer is a response"));
    assert_eq!(answer.status().as_u16(), 500);
    same(
        &String::from_utf8(answer.into_body()).unwrap(),
        &json::to_string(&json!({
            "error": "There're currently matching orders using ETH",
            "orders": { "matching": [matching.uuid], "cancelled": [] },
            "active_swaps": [],
            "swaps": [],
        }))
        .unwrap(),
        "the legacy blocked body",
    );

    let answer = versioned(ctx, "disable_coin", Some(json!({ "coin": "ETH" })), 121);
    assert_typed_refusal(&answer, 400, "MatchingOrdersBlockDeactivation", Some("object"), 121);
    assert_data_member(&answer, "coin", json!("ETH"));
    assert_data_member(&answer, "matching", json!([matching.uuid]));
    assert_data_member(&answer, "cancelled", json!([]));

    // The coin is still activated, and the order still there.
    assert!(node.in_container(&matching.uuid), "the order stays");
    assert!(
        block_on(coins::lp_coinfind(ctx, "ETH")).unwrap().is_some(),
        "the coin stays activated"
    );
}

// ----- The envelope -----

/// CRD ch.43 R43.1A.2: a legacy-shaped body (parameters inline) with an added `"mmrpc": "2.0"` is undecodable under the
/// versioned envelope and answers `InvalidRequest` (string payload, HTTP 400) before any method lookup, for every
/// method of the group.
#[test]
fn legacy_shaped_body_with_an_mmrpc_key_answers_invalid_request_for_every_method_of_the_group() {
    let ctx = bare_node();
    let cases = [
        ("my_balance", json!({ "coin": "DYN" })),
        ("disable_coin", json!({ "coin": "DYN" })),
        ("send_raw_transaction", json!({ "coin": "DYN", "tx_hex": "00" })),
        ("validateaddress", json!({ "coin": "DYN", "address": "a" })),
        (
            "convertaddress",
            json!({ "coin": "DYN", "from": "a", "to_address_format": {} }),
        ),
        ("convert_utxo_address", json!({ "address": "a", "to_coin": "DYN" })),
        ("kmd_rewards_info", json!({ "unknown": 1 })),
        ("coins_needed_for_kick_start", json!({ "unknown": 1 })),
        (
            "set_required_confirmations",
            json!({ "coin": "DYN", "confirmations": 1 }),
        ),
        (
            "set_requires_notarization",
            json!({ "coin": "DYN", "requires_notarization": true }),
        ),
    ];
    for (method, inline) in cases {
        let mut request = json!({ "mmrpc": "2.0", "userpass": "pw", "method": method, "id": 3 });
        for (key, value) in inline.as_object().unwrap() {
            request[key.as_str()] = value.clone();
        }
        let answer = match run(&ctx, request) {
            Ok(answer) => answer,
            Err(_) => panic!("{method}: the versioned surface answers every request"),
        };
        let body: Json = json::from_slice(answer.body()).unwrap();
        assert_eq!(answer.status().as_u16(), 400, "{method}: status");
        assert_eq!(body["error_type"], "InvalidRequest", "{method}: error_type");
        assert!(body["error_data"].is_string(), "{method}: error_data kind");
        assert_eq!(body["id"], 3, "{method}: id");
    }
}

// ----- `disable_coin` for a Solana coin and an SPL token (CRD ch.40 T40.6B.20, ch.43 T43.1E) -----

/// A Solana platform coin built offline from its activation parameters (the node pool contacts nothing until a request
/// is made), and an SPL token of it.
fn offline_solana_and_token() -> (coins::SolanaCoin, coins::SplToken) {
    let params = json::from_value(json!({
        "confirmation_commitment": "finalized",
        "client_url": "http://127.0.0.1:1",
    }))
    .expect("the activation parameters parse");
    let platform = block_on(coins::solana_coin_from_conf_and_params(
        "SOL",
        &json!({}),
        params,
        &[7_u8; 32],
    ))
    .expect("the offline platform coin is built");
    let mint = coins::solana::spl::SPL_TOKEN_PROGRAM_ID;
    let token = coins::SplToken::new(6, "USDC".to_owned(), mint, mint, platform.clone());
    (platform, token)
}

fn register_in(ctx: &MmArc, coin: MmCoinEnum) {
    block_on(CoinsContext::from_ctx(ctx).unwrap().add_coin(coin)).expect("the coin is registered");
}

fn enabled_tickers(ctx: &MmArc) -> Vec<String> {
    let body: Json = json::from_str(&legacy_success(ctx, "get_enabled_coins", json!({}))).unwrap();
    let mut tickers: Vec<String> = body["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ticker"].as_str().unwrap().to_owned())
        .collect();
    tickers.sort();
    tickers
}

/// T40.6B.20: a token answers and leaves `get_enabled_coins`, on both surfaces; the platform stays and the token can be
/// registered again.
#[test]
fn t40_6b_20_disable_coin_on_an_spl_token_answers_on_both_surfaces() {
    let (platform, token) = offline_solana_and_token();
    let ctx = bare_node();
    register_in(&ctx, platform.clone().into());
    register_in(&ctx, token.clone().into());
    platform.add_spl_token_info("USDC".to_owned(), token.get_info());
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned(), "USDC".to_owned()]);

    let expected = json::to_string(&json!({ "result": { "coin": "USDC", "cancelled_orders": [] } })).unwrap();
    same(
        &legacy_success(&ctx, "disable_coin", json!({ "coin": "USDC" })),
        &expected,
        "the legacy token body",
    );
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned()]);
    assert!(
        platform.get_spl_tokens_infos().is_empty(),
        "the platform no longer records the token"
    );

    register_in(&ctx, token.clone().into());
    platform.add_spl_token_info("USDC".to_owned(), token.get_info());
    let result = versioned_result(
        versioned(&ctx, "disable_coin", Some(json!({ "coin": "USDC" })), 201),
        201,
    );
    same(
        &result,
        &json!({ "coin": "USDC", "cancelled_orders": [] }),
        "the versioned token result",
    );
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned()]);
}

/// T40.6B.20: a platform without tokens answers and is removed, on both surfaces; it can be registered again.
#[test]
fn t40_6b_20_disable_coin_on_a_solana_platform_without_tokens_answers_on_both_surfaces() {
    let (platform, _) = offline_solana_and_token();
    let ctx = bare_node();
    register_in(&ctx, platform.clone().into());

    let expected = json::to_string(&json!({ "result": { "coin": "SOL", "cancelled_orders": [] } })).unwrap();
    same(
        &legacy_success(&ctx, "disable_coin", json!({ "coin": "SOL" })),
        &expected,
        "the legacy platform body",
    );
    assert!(enabled_tickers(&ctx).is_empty());

    register_in(&ctx, platform.into());
    let result = versioned_result(
        versioned(&ctx, "disable_coin", Some(json!({ "coin": "SOL" })), 202),
        202,
    );
    same(
        &result,
        &json!({ "coin": "SOL", "cancelled_orders": [] }),
        "the versioned platform result",
    );
    assert!(enabled_tickers(&ctx).is_empty());
}

/// T40.6B.20 under the current rule (CRD ch.43 §43.1E "Compatibility" item 2, ch.55 D55.6 open): the platform answers
/// and is removed as for the other families, the token stays registered, and the platform can be registered again.
#[test]
fn t40_6b_20_disable_coin_on_a_solana_platform_with_a_token_follows_the_other_families() {
    let (platform, token) = offline_solana_and_token();
    let ctx = bare_node();
    register_in(&ctx, platform.clone().into());
    register_in(&ctx, token.clone().into());
    platform.add_spl_token_info("USDC".to_owned(), token.get_info());

    let expected = json::to_string(&json!({ "result": { "coin": "SOL", "cancelled_orders": [] } })).unwrap();
    same(
        &legacy_success(&ctx, "disable_coin", json!({ "coin": "SOL" })),
        &expected,
        "the legacy platform-with-token body",
    );
    assert_eq!(enabled_tickers(&ctx), vec!["USDC".to_owned()]);

    register_in(&ctx, platform.into());
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned(), "USDC".to_owned()]);
}

/// A loopback Solana node that answers what an activation reads: a SOL balance, a block height and no account for any
/// address (so a token balance is zero). It stops when `stop` is set.
#[cfg(not(target_arch = "wasm32"))]
fn solana_stub_node(stop: std::sync::Arc<AtomicBool>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                },
            };
            stream.set_nonblocking(false).ok();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let body = loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break Json::Null,
                    Ok(n) => n,
                };
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .to_lowercase()
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break json::from_str(rest).unwrap_or(Json::Null);
                    }
                }
            };
            let result = match body["method"].as_str().unwrap_or_default() {
                "getHealth" => json!("ok"),
                "getBalance" => json!({ "context": { "slot": 1 }, "value": 1_000_000_000u64 }),
                "getBlockHeight" => json!(42),
                "getAccountInfo" => json!({ "context": { "slot": 1 }, "value": null }),
                _ => Json::Null,
            };
            let reply = json!({ "jsonrpc": "2.0", "id": body["id"], "result": result }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// T40.6B.20 for the compatibility surface: a platform activated through the compatibility platform method is
/// disabled with `disable_coin` and leaves `get_enabled_coins`; a token activated through the compatibility token
/// method is removed from its platform's token records. The activation entry points that skip the beta switch are
/// used; the node is a loopback stub.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn t40_6b_20_disable_coin_on_coins_activated_through_the_compatibility_surface() {
    use coins_activation::{activate_solana_token, activate_solana_with_assets, SolanaCompatPlatformRequest,
                           SolanaCompatTokenRequest};
    let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let program = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let url = solana_stub_node(stop.clone());
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "rpc_password": "pw", "coins": [
            { "coin": "SOL", "mm2": 1, "protocol": { "type": "SOLANA", "protocol_data": {} } },
            { "coin": "USDC", "mm2": 1, "protocol": { "type": "SOLANATOKEN", "protocol_data": {
                "platform": "SOL", "decimals": 6, "mint_address": mint, "program_id": program } } },
        ] }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "disable compat test passphrase")
        .expect("initialize the crypto context");

    let platform_request: SolanaCompatPlatformRequest =
        json::from_value(json!({ "ticker": "SOL", "nodes": [{ "url": url }], "tokens_params": [] }))
            .expect("a well-formed request");
    block_on(activate_solana_with_assets(ctx.clone(), platform_request))
        .map_err(|e| e.into_inner())
        .expect("the platform activates");
    let token_request: SolanaCompatTokenRequest =
        json::from_value(json!({ "ticker": "USDC", "activation_params": {} })).expect("a well-formed request");
    block_on(activate_solana_token(ctx.clone(), token_request))
        .map_err(|e| e.into_inner())
        .expect("the token activates");
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned(), "USDC".to_owned()]);
    let platform = match block_on(coins::lp_coinfind(&ctx, "SOL")).unwrap() {
        Some(MmCoinEnum::SolanaCoin(platform)) => platform,
        _ => panic!("the platform is not a Solana platform"),
    };
    assert!(
        platform.get_spl_tokens_infos().contains_key("USDC"),
        "the token is recorded"
    );

    let expected = json::to_string(&json!({ "result": { "coin": "USDC", "cancelled_orders": [] } })).unwrap();
    same(
        &legacy_success(&ctx, "disable_coin", json!({ "coin": "USDC" })),
        &expected,
        "the legacy token body",
    );
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned()]);
    assert!(
        platform.get_spl_tokens_infos().is_empty(),
        "the platform no longer records the token"
    );

    let expected = json::to_string(&json!({ "result": { "coin": "SOL", "cancelled_orders": [] } })).unwrap();
    same(
        &legacy_success(&ctx, "disable_coin", json!({ "coin": "SOL" })),
        &expected,
        "the legacy platform body",
    );
    assert!(enabled_tickers(&ctx).is_empty());
    stop.store(true, Ordering::SeqCst);
}

/// T43.1E (coin not active) for the Solana tickers: the extended refusal body with empty lists, HTTP 500, legacy; the
/// typed `NoSuchCoin`, versioned. A token and a platform that were disabled already answer the same.
#[test]
fn t40_6b_20_disable_coin_on_a_solana_ticker_that_is_not_active_is_the_not_active_refusal() {
    let (platform, token) = offline_solana_and_token();
    let ctx = bare_node();
    register_in(&ctx, platform.into());
    register_in(&ctx, token.into());
    for ticker in ["USDC", "SOL"] {
        legacy_success(&ctx, "disable_coin", json!({ "coin": ticker }));
        let answer = run(&ctx, legacy_request("disable_coin", json!({ "coin": ticker })))
            .unwrap_or_else(|_| panic!("the legacy not-active answer is a response"));
        assert_eq!(answer.status().as_u16(), 500);
        let body: Json = json::from_slice(answer.body()).unwrap();
        assert!(body["error"].is_string(), "the error member");
        assert_eq!(body["orders"], json!({ "matching": [], "cancelled": [] }));
        assert_eq!(body["active_swaps"], json!([]));
        assert_eq!(body["swaps"], json!([]));

        let answer = versioned(&ctx, "disable_coin", Some(json!({ "coin": ticker })), 203);
        assert_typed_refusal(&answer, 404, "NoSuchCoin", Some("object"), 203);
        assert_data_member(&answer, "coin", json!(ticker));
    }
}
