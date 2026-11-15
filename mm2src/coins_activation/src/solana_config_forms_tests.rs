//! The Solana coin-configuration forms at the activation layer (CRD ch.40 R40.1.2, R40.1.3, T40.2).
//!
//! The generic activation entry points are called directly, so the beta switch that makes the routed methods
//! refuse every request is bypassed. Nothing here touches the network.

use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq};
use crate::prelude::TryFromCoinProtocol;
use crate::solana_with_tokens_activation::SolanaWithTokensActivationRequest;
use crate::spl_token_activation::SplActivationRequest;
use crate::token::{enable_token, EnableTokenError, EnableTokenRequest};
use coins::solana::spl::SplProtocolConf;
use coins::{lp_register_coin, solana_coin_from_conf_and_params, CoinProtocol, MarketCoinOps, MmCoin, MmCoinEnum,
            RegisterCoinParams, SolanaActivationParams, SolanaCoin, SplToken};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};
use std::collections::BTreeSet;

const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

fn solanatoken(mint: &str, program: &str) -> Json {
    json!({ "type": "SOLANATOKEN", "protocol_data": {
        "platform": "SOL", "decimals": 6, "mint_address": mint, "program_id": program } })
}

fn ctx_with(coins: Vec<Json>) -> MmArc {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "solana config forms test passphrase")
        .expect("initialize the crypto context");
    ctx
}

fn token_request(ticker: &str) -> EnableTokenRequest<SplActivationRequest> {
    serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).expect("a well-formed request")
}

#[test]
fn spl_protocol_conf_from_spltoken_uses_the_spl_token_program() {
    let proto = CoinProtocol::SPLTOKEN {
        platform: "SOL".to_owned(),
        token_contract_address: MINT.parse().unwrap(),
        decimals: 6,
    };
    let conf = SplProtocolConf::try_from_coin_protocol(proto).unwrap();
    assert_eq!(conf.platform_coin_ticker, "SOL");
    assert_eq!(conf.decimals, 6);
    assert_eq!(conf.token_contract_address.to_string(), MINT);
    assert_eq!(conf.program_id.to_string(), SPL_TOKEN_PROGRAM);
}

#[test]
fn spl_protocol_conf_from_solanatoken_keeps_the_token_2022_program() {
    let proto = CoinProtocol::from_conf_json(solanatoken(MINT, TOKEN_2022_PROGRAM)).unwrap();
    let conf = SplProtocolConf::try_from_coin_protocol(proto).unwrap();
    assert_eq!(conf.platform_coin_ticker, "SOL");
    assert_eq!(conf.decimals, 6);
    assert_eq!(conf.token_contract_address.to_string(), MINT);
    assert_eq!(conf.program_id.to_string(), TOKEN_2022_PROGRAM);
}

#[test]
fn spl_protocol_conf_refuses_other_protocols() {
    assert!(SplProtocolConf::try_from_coin_protocol(CoinProtocol::SOLANA).is_err());
    assert!(SplProtocolConf::try_from_coin_protocol(CoinProtocol::UTXO).is_err());
}

/// T40.2: a `SOLANATOKEN` with an invalid `mint_address` is a protocol parse failure on the token path.
#[test]
fn t40_2_token_activation_reports_an_invalid_mint_as_a_protocol_parse_error() {
    let ctx = ctx_with(vec![json!({ "coin": "BAD-SOL", "mm2": 1,
        "protocol": solanatoken("not a base58 key", SPL_TOKEN_PROGRAM) })]);
    let err = block_on(enable_token::<SplToken>(ctx, token_request("BAD-SOL"))).unwrap_err();
    assert!(
        matches!(err.get_inner(), EnableTokenError::TokenProtocolParseError { ticker, .. } if ticker == "BAD-SOL"),
        "{err:?}"
    );
    assert_eq!(err.get_inner().status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// T40.2: a valid `SOLANATOKEN` parses; with no active platform the answer is `PlatformCoinIsNotActivated`.
#[test]
fn t40_2_token_activation_parses_a_valid_solanatoken_then_needs_its_platform() {
    let ctx = ctx_with(vec![json!({ "coin": "OK-SOL", "mm2": 1,
        "protocol": solanatoken(MINT, TOKEN_2022_PROGRAM) })]);
    let err = block_on(enable_token::<SplToken>(ctx, token_request("OK-SOL"))).unwrap_err();
    assert!(
        matches!(err.get_inner(), EnableTokenError::PlatformCoinIsNotActivated(platform) if platform == "SOL"),
        "{err:?}"
    );
    assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
}

/// T40.2: the reference platform form `{"type":"SOLANA","protocol_data":{}}` parses; with no node URL the build
/// fails with the endpoint error, not a protocol error.
#[test]
fn t40_2_platform_activation_accepts_the_reference_platform_form() {
    let ctx = ctx_with(vec![json!({ "coin": "SOL", "mm2": 1,
        "protocol": { "type": "SOLANA", "protocol_data": {} } })]);
    let req: EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest> = serde_json::from_value(json!({
        "ticker": "SOL",
        "confirmation_commitment": "finalized",
        "spl_tokens_requests": [],
    }))
    .expect("a well-formed request");
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    match err.get_inner() {
        EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error } => {
            assert!(ticker.as_str() == "SOL", "unexpected ticker");
            assert!(
                error.contains("at least one RPC endpoint"),
                "unexpected no-node error text"
            );
        },
        _ => panic!("expected PlatformCoinCreationError"),
    }
}

// ---------------------------------------------------------------------------
// The primary activation surface (CRD ch.40 §40.2.4, §40.3.4, R40.6B.2, R40.6B.3, R40.6B.8, T40.6B.1-5, T40.6B.21)
// ---------------------------------------------------------------------------

/// A node that refuses the connection at once. The path and the query stand in for a credential: no error text
/// may carry them (R40.5A.1).
const CLOSED_NODE: &str = "http://127.0.0.1:1/v1/PATH-SECRET?token=QUERY-SECRET";

fn platform_entry() -> Json {
    json!({ "coin": "SOL", "mm2": 1, "protocol": { "type": "SOLANA", "protocol_data": {} } })
}

fn platform_request(
    client_url: Option<&str>,
    tokens: &[&str],
    extra: Json,
) -> EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest> {
    let tokens: Vec<Json> = tokens.iter().map(|ticker| json!({ "ticker": ticker })).collect();
    let mut body = json!({ "ticker": "SOL", "confirmation_commitment": "finalized", "spl_tokens_requests": tokens });
    if let Some(url) = client_url {
        body["client_url"] = json!(url);
    }
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    serde_json::from_value(body).expect("a well-formed request")
}

/// The error as the RPC layer serializes it (`error_type` plus `error_data`) with its HTTP status.
fn platform_wire(err: &EnablePlatformCoinWithTokensError) -> (Json, StatusCode) {
    (
        serde_json::to_value(err).expect("serializable error"),
        err.status_code(),
    )
}

fn token_wire(err: &EnableTokenError) -> (Json, StatusCode) {
    (
        serde_json::to_value(err).expect("serializable error"),
        err.status_code(),
    )
}

fn data_keys(wire: &Json) -> BTreeSet<String> {
    wire["error_data"]
        .as_object()
        .expect("error_data is an object")
        .keys()
        .cloned()
        .collect()
}

fn keys(names: &[&str]) -> BTreeSet<String> { names.iter().map(|name| (*name).to_owned()).collect() }

fn assert_no_node_secrets(text: &str) {
    for secret in ["PATH-SECRET", "QUERY-SECRET", "/v1/", "token="] {
        assert!(!text.contains(secret), "the error text leaked a part of the node URL");
    }
}

fn platform_with_closed_node(ctx: &MmArc, conf: &Json) -> SolanaCoin {
    let params: SolanaActivationParams = serde_json::from_value(json!({
        "confirmation_commitment": "finalized",
        "client_url": CLOSED_NODE,
    }))
    .expect("well-formed activation parameters");
    let priv_key = &*ctx.legacy_passphrase_key_pair().private().secret;
    block_on(solana_coin_from_conf_and_params("SOL", conf, params, priv_key)).expect("the platform coin is built")
}

fn spltoken(mint: &str, decimals: u32) -> Json {
    json!({ "type": "SPLTOKEN", "protocol_data": {
        "platform": "SOL", "token_contract_address": mint, "decimals": decimals } })
}

/// T40.6B.3: a platform entry with a top-level `decimals` still has 9 decimals.
#[test]
fn t40_6b_3_platform_decimals_are_nine_whatever_the_entry_says() {
    let ctx = ctx_with(vec![platform_entry()]);
    let mut entry = platform_entry();
    entry["decimals"] = json!(6);
    let coin = platform_with_closed_node(&ctx, &entry);
    assert_eq!(coin.decimals(), 9);
}

/// T40.6B.3: a token's decimals are `protocol_data.decimals`; a top-level `decimals` is ignored.
#[test]
fn t40_6b_3_token_decimals_are_the_protocol_data_ones() {
    let ctx = ctx_with(vec![platform_entry()]);
    let platform = platform_with_closed_node(&ctx, &platform_entry());
    let mut entry = json!({ "coin": "TOK-SOL", "mm2": 1, "decimals": 9,
        "protocol": solanatoken(MINT, SPL_TOKEN_PROGRAM) });
    entry["protocol"]["protocol_data"]["decimals"] = json!(6);
    let proto = CoinProtocol::from_conf_json(entry["protocol"].clone()).expect("a parsable protocol");
    let conf = SplProtocolConf::try_from_coin_protocol(proto).expect("a Solana token protocol");
    let token = SplToken::new(
        conf.decimals,
        "TOK-SOL".to_owned(),
        conf.token_contract_address,
        conf.program_id,
        platform,
    );
    assert_eq!(token.decimals(), 6);
}

/// T40.6B.4: under a single-key context the address does not depend on `derivation_path`.
#[test]
fn t40_6b_4_derivation_path_does_not_change_the_platform_address() {
    let ctx = ctx_with(vec![platform_entry()]);
    let absent = platform_with_closed_node(&ctx, &platform_entry());
    let mut present = platform_entry();
    present["derivation_path"] = json!("m/44'/501'/0'/0'");
    let present = platform_with_closed_node(&ctx, &present);
    let mut malformed = platform_entry();
    malformed["derivation_path"] = json!("this is not a path");
    let malformed = platform_with_closed_node(&ctx, &malformed);
    let expected = absent.my_address().expect("an address");
    assert!(
        present.my_address().expect("an address") == expected,
        "a derivation_path changed the address"
    );
    assert!(
        malformed.my_address().expect("an address") == expected,
        "a malformed derivation_path changed the address"
    );
}

/// T40.6B.5 / R40.6B.8: the plain passphrase of the twelve-word test mnemonic gives the fixed secret and address.
#[test]
fn t40_6b_5_single_key_vector_of_the_test_mnemonic() {
    let passphrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let key_pair = crypto::privkey::key_pair_from_seed(passphrase).expect("a key pair");
    let secret = &*key_pair.private().secret;
    assert!(
        hex::encode(secret) == "c057eec878dfd852ba3f88087c4f350f09c55537ab5e549c3cd14320ec3cef78",
        "the secret of the test passphrase differs from the vector"
    );
    let params: SolanaActivationParams = serde_json::from_value(json!({
        "confirmation_commitment": "finalized",
        "client_url": CLOSED_NODE,
    }))
    .expect("well-formed activation parameters");
    let coin = block_on(solana_coin_from_conf_and_params(
        "SOL",
        &platform_entry(),
        params,
        secret,
    ))
    .expect("the platform coin is built");
    assert!(
        coin.my_address().expect("an address") == "7zuqksARN8uXHz2yvrkiZZDPeALBSjzK12nkMPNzKSCJ",
        "the address of the test passphrase differs from the vector"
    );
}

/// T40.6B.1: on the platform method an inline token whose `SOLANATOKEN` has no `program_id`, or has `decimals`
/// 256, is a `TokenProtocolParseError` (HTTP 500, `{ticker, error}`); the node is never contacted.
#[test]
fn t40_6b_1_platform_method_reports_unparsable_solanatoken_forms() {
    let mut no_program = solanatoken(MINT, SPL_TOKEN_PROGRAM);
    no_program["protocol_data"]
        .as_object_mut()
        .unwrap()
        .remove("program_id");
    let mut decimals_256 = solanatoken(MINT, SPL_TOKEN_PROGRAM);
    decimals_256["protocol_data"]["decimals"] = json!(256);
    for (ticker, protocol) in [("NOPROG-SOL", no_program), ("DEC256-SOL", decimals_256)] {
        let ctx = ctx_with(vec![
            platform_entry(),
            json!({ "coin": ticker, "mm2": 1, "protocol": protocol }),
        ]);
        let req = platform_request(Some(CLOSED_NODE), &[ticker], json!({}));
        let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
        let (wire, status) = platform_wire(err.get_inner());
        assert!(
            wire["error_type"] == "TokenProtocolParseError",
            "unexpected error wire shape"
        );
        assert!(wire["error_data"]["ticker"] == ticker, "unexpected error wire shape");
        assert!(
            data_keys(&wire) == keys(&["ticker", "error"]),
            "unexpected error wire shape"
        );
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected error wire shape"
        );
    }
}

/// T40.6B.1: the same two forms on the token method.
#[test]
fn t40_6b_1_token_method_reports_unparsable_solanatoken_forms() {
    let mut no_program = solanatoken(MINT, SPL_TOKEN_PROGRAM);
    no_program["protocol_data"]
        .as_object_mut()
        .unwrap()
        .remove("program_id");
    let mut decimals_256 = solanatoken(MINT, SPL_TOKEN_PROGRAM);
    decimals_256["protocol_data"]["decimals"] = json!(256);
    for (ticker, protocol) in [("NOPROG-SOL", no_program), ("DEC256-SOL", decimals_256)] {
        let ctx = ctx_with(vec![json!({ "coin": ticker, "mm2": 1, "protocol": protocol })]);
        let err = block_on(enable_token::<SplToken>(ctx, token_request(ticker))).unwrap_err();
        let (wire, status) = token_wire(err.get_inner());
        assert!(
            wire["error_type"] == "TokenProtocolParseError",
            "unexpected error wire shape"
        );
        assert!(
            data_keys(&wire) == keys(&["ticker", "error"]),
            "unexpected error wire shape"
        );
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected error wire shape"
        );
    }
}

/// `SPLTOKEN.token_contract_address` is checked at parse: an invalid key is a `TokenProtocolParseError` on the
/// token method, with the `{ticker, error}` wire shape.
#[test]
fn spltoken_invalid_contract_address_is_a_protocol_parse_error_on_the_token_method() {
    let ctx = ctx_with(vec![
        json!({ "coin": "BAD-SPL", "mm2": 1, "protocol": spltoken("not a base58 key", 6) }),
    ]);
    let err = block_on(enable_token::<SplToken>(ctx, token_request("BAD-SPL"))).unwrap_err();
    let (wire, status) = token_wire(err.get_inner());
    assert!(
        wire["error_type"] == "TokenProtocolParseError",
        "unexpected error wire shape"
    );
    assert!(wire["error_data"]["ticker"] == "BAD-SPL", "unexpected error wire shape");
    assert!(
        data_keys(&wire) == keys(&["ticker", "error"]),
        "unexpected error wire shape"
    );
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
}

/// The same entry on the platform method.
#[test]
fn spltoken_invalid_contract_address_is_a_protocol_parse_error_on_the_platform_method() {
    let ctx = ctx_with(vec![
        platform_entry(),
        json!({ "coin": "BAD-SPL", "mm2": 1, "protocol": spltoken("not a base58 key", 6) }),
    ]);
    let req = platform_request(Some(CLOSED_NODE), &["BAD-SPL"], json!({}));
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "TokenProtocolParseError",
        "unexpected error wire shape"
    );
    assert!(wire["error_data"]["ticker"] == "BAD-SPL", "unexpected error wire shape");
    assert!(
        data_keys(&wire) == keys(&["ticker", "error"]),
        "unexpected error wire shape"
    );
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
}

/// A valid `SPLTOKEN` address keeps its base58 form through the JSON round trip.
#[test]
fn spltoken_contract_address_round_trips_as_base58() {
    let proto = CoinProtocol::from_conf_json(spltoken(MINT, 6)).expect("a parsable protocol");
    let json = serde_json::to_value(&proto).expect("a serializable protocol");
    assert_eq!(json["protocol_data"]["token_contract_address"], MINT);
}

/// §40.2.4: a node failure with no inline token fails at the balance read with `PlatformCoinCreationError`
/// (HTTP 500, `{ticker, error}`, the platform ticker), and the text names the node host-only.
#[test]
fn platform_method_node_failure_is_a_platform_coin_creation_error() {
    let ctx = ctx_with(vec![platform_entry()]);
    let req = platform_request(Some(CLOSED_NODE), &[], json!({}));
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "PlatformCoinCreationError",
        "unexpected error wire shape"
    );
    assert!(wire["error_data"]["ticker"] == "SOL", "unexpected error wire shape");
    assert!(
        data_keys(&wire) == keys(&["ticker", "error"]),
        "unexpected error wire shape"
    );
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
    assert_no_node_secrets(&wire.to_string());
}

/// §40.2.4 and T40.6B.1 wire shape: the no-node failure is a `PlatformCoinCreationError` as well.
#[test]
fn platform_method_without_a_node_has_the_platform_coin_creation_error_wire_shape() {
    let ctx = ctx_with(vec![platform_entry()]);
    let req = platform_request(None, &[], json!({}));
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "PlatformCoinCreationError",
        "unexpected error wire shape"
    );
    assert!(wire["error_data"]["ticker"] == "SOL", "unexpected error wire shape");
    assert!(
        data_keys(&wire) == keys(&["ticker", "error"]),
        "unexpected error wire shape"
    );
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
}

/// §40.2.4: a node failure while reading a token balance (inline token, node refuses) is also a
/// `PlatformCoinCreationError` naming the platform ticker, not `Transport` and not `Internal`.
#[test]
fn platform_method_token_balance_failure_is_a_platform_coin_creation_error() {
    let ctx = ctx_with(vec![
        platform_entry(),
        json!({ "coin": "OK-SOL", "mm2": 1, "protocol": solanatoken(MINT, SPL_TOKEN_PROGRAM) }),
    ]);
    let req = platform_request(Some(CLOSED_NODE), &["OK-SOL"], json!({}));
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "PlatformCoinCreationError",
        "unexpected error wire shape"
    );
    assert!(wire["error_data"]["ticker"] == "SOL", "unexpected error wire shape");
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
    assert_no_node_secrets(&wire.to_string());
}

/// §40.3.4: a balance read that fails on the token method is `Transport` (HTTP 500), the node named host-only.
/// The platform coin is built with the coin constructor and registered in the context directly.
#[test]
fn token_method_node_failure_is_a_transport_error() {
    let ctx = ctx_with(vec![
        platform_entry(),
        json!({ "coin": "OK-SOL", "mm2": 1, "protocol": solanatoken(MINT, SPL_TOKEN_PROGRAM) }),
    ]);
    let platform = platform_with_closed_node(&ctx, &platform_entry());
    block_on(lp_register_coin(
        &ctx,
        MmCoinEnum::SolanaCoin(platform),
        RegisterCoinParams {
            ticker: "SOL".to_owned(),
            tx_history: false,
        },
    ))
    .expect("the platform coin is registered");
    let err = block_on(enable_token::<SplToken>(ctx, token_request("OK-SOL"))).unwrap_err();
    let (wire, status) = token_wire(err.get_inner());
    assert!(wire["error_type"] == "Transport", "unexpected error wire shape");
    assert!(wire["error_data"].is_string(), "unexpected error wire shape");
    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected error wire shape"
    );
    assert_no_node_secrets(&wire.to_string());
}

/// T40.6B.21: `tx_history: true` in the platform request does not fail parsing; activation goes on to the no-node
/// `PlatformCoinCreationError`, which shows that the member was ignored, not refused.
#[test]
fn t40_6b_21_tx_history_true_is_ignored_on_the_platform_method() {
    let ctx = ctx_with(vec![platform_entry()]);
    let req = platform_request(None, &[], json!({ "tx_history": true }));
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req)).unwrap_err();
    match err.get_inner() {
        EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error } => {
            assert!(ticker.as_str() == "SOL", "unexpected ticker");
            assert!(
                error.contains("at least one RPC endpoint"),
                "unexpected no-node error text"
            );
        },
        _ => panic!("expected PlatformCoinCreationError"),
    }
}
