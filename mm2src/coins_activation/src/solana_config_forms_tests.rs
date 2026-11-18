//! The Solana coin-configuration forms at the activation layer (CRD ch.40 R40.1.2, R40.1.3, T40.2).
//!
//! The generic activation entry points are called directly, so the beta switch that makes the routed methods
//! refuse every request is bypassed. Nothing here touches the network.

use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq, PlatformWithTokensActivationOps};
use crate::prelude::TryFromCoinProtocol;
use crate::solana_with_tokens_activation::{derivation_method_of, SolanaProtocolInfo, SolanaWithTokensActivationError,
                                           SolanaWithTokensActivationRequest};
use crate::spl_token_activation::SplActivationRequest;
use crate::token::{enable_token, EnableTokenError, EnableTokenRequest};
use coins::solana::spl::SplProtocolConf;
use coins::solana::{SOLANA_HD_LAYOUT_BIP44, SOLANA_HD_LAYOUT_BIP44_CHANGE};
use coins::{lp_register_coin, solana_coin_from_conf_and_params, CoinProtocol, MarketCoinOps, MmCoin, MmCoinEnum,
            RegisterCoinParams, SolanaActivationParams, SolanaCoin, SplToken};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_err_handle::prelude::MmError;
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

// ---------------------------------------------------------------------------
// Keys under each login (CRD ch.40 §40.1A, R40.1A.1-R40.1A.7, T40.1A.1-T40.1A.3)
//
// No assertion message below carries a key, a seed, a mnemonic or an address derived from one (CodeQL rule):
// values are compared with `assert!(a == b, "<fixed text>")`. The mnemonic and the addresses appear only in
// fixtures and expected-value constants.
// ---------------------------------------------------------------------------

const HD_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const PASSPHRASE_ADDRESS: &str = "7zuqksARN8uXHz2yvrkiZZDPeALBSjzK12nkMPNzKSCJ";
const BIP44_CHANGE_ACCOUNT_0: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
const BIP44_CHANGE_ACCOUNT_1: &str = "Hh8QwFUA6MtVu1qAoq12ucvFHNwCcVTV7hpWjeY1Hztb";
const BIP44_ACCOUNT_0: &str = "GjJyeC1r2RgkuoCWMyPYkCWSGSGLcz266EaAkLA27AhL";
const BIP44_ACCOUNT_1: &str = "ANf3TEKFL6jPWjzkndo4CbnNdUNkBk4KHPggJs2nu8Xi";

/// A context whose login is a global-HD wallet of the test mnemonic (the passphrase-hash key of the same words is
/// still held by the context, which is what the HD address must never equal).
fn hd_ctx_with(coins: Vec<Json>) -> MmArc {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).expect("initialize the HD crypto context");
    ctx
}

/// A single-key (passphrase) context of the same words.
fn single_key_ctx_with(coins: Vec<Json>) -> MmArc {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), HD_MNEMONIC).expect("initialize the crypto context");
    ctx
}

fn entry_with_path(derivation_path: Option<Json>) -> Json {
    let mut entry = platform_entry();
    if let Some(path) = derivation_path {
        entry["derivation_path"] = path;
    }
    entry
}

/// Builds the platform coin the way the activation does (key choice included), with a closed-port node that is
/// never contacted: the build reads nothing from the network.
fn built_platform(
    ctx: &MmArc,
    entry: &Json,
    extra: Json,
) -> Result<SolanaCoin, MmError<SolanaWithTokensActivationError>> {
    let req = platform_request(Some(CLOSED_NODE), &[], extra);
    let priv_key = *ctx.legacy_passphrase_key_pair().private().secret;
    block_on(<SolanaCoin as PlatformWithTokensActivationOps>::enable_platform_coin(
        ctx.clone(),
        "SOL".to_owned(),
        entry.clone(),
        req.request,
        SolanaProtocolInfo {},
        &priv_key,
    ))
}

fn address_of(coin: &SolanaCoin) -> String { coin.my_address().expect("an address") }

/// The error text of a platform-coin creation error, or a failed assertion saying the variant was another one.
fn creation_error_text(err: &SolanaWithTokensActivationError) -> String {
    match err {
        SolanaWithTokensActivationError::PlatformCoinCreationError { ticker, error } => {
            assert!(ticker.as_str() == "SOL", "unexpected ticker");
            error.clone()
        },
        _ => panic!("expected PlatformCoinCreationError"),
    }
}

fn hd_address(layout: Option<&str>, account_id: u32) -> String {
    let ctx = hd_ctx_with(vec![platform_entry()]);
    let mut extra = json!({ "path_to_address": { "account_id": account_id } });
    if let Some(layout) = layout {
        extra["hd_path_layout"] = json!(layout);
    }
    address_of(&built_platform(&ctx, &platform_entry(), extra).expect("the platform coin is built"))
}

/// The two layout names are written once, in the constants; the request type reads exactly those.
#[test]
fn t40_1a_layout_values_are_the_constants() {
    assert!(
        SOLANA_HD_LAYOUT_BIP44_CHANGE == "bip44_change",
        "unexpected layout name"
    );
    assert!(SOLANA_HD_LAYOUT_BIP44 == "bip44", "unexpected layout name");
    for (name, expected) in [
        (SOLANA_HD_LAYOUT_BIP44_CHANGE, "bip44_change"),
        (SOLANA_HD_LAYOUT_BIP44, "bip44"),
    ] {
        let layout: coins::solana::SolanaHdLayout = serde_json::from_value(json!(name)).expect("a known layout name");
        assert!(layout.as_str() == expected, "the parsed layout does not round-trip");
    }
}

/// T40.1A.1: the four published vectors.
#[test]
fn t40_1a_1_hd_vectors_of_the_test_mnemonic() {
    assert!(
        hd_address(Some(SOLANA_HD_LAYOUT_BIP44_CHANGE), 0) == BIP44_CHANGE_ACCOUNT_0,
        "vector mismatch"
    );
    assert!(
        hd_address(Some(SOLANA_HD_LAYOUT_BIP44_CHANGE), 1) == BIP44_CHANGE_ACCOUNT_1,
        "vector mismatch"
    );
    assert!(
        hd_address(Some(SOLANA_HD_LAYOUT_BIP44), 0) == BIP44_ACCOUNT_0,
        "vector mismatch"
    );
    assert!(
        hd_address(Some(SOLANA_HD_LAYOUT_BIP44), 1) == BIP44_ACCOUNT_1,
        "vector mismatch"
    );
}

/// R40.1A.1, R40.1A.4: the HD address never equals the passphrase-hash address of the same words, and the
/// single-key address of those words is the S1 vector.
#[test]
fn t40_1a_1_hd_address_is_never_the_passphrase_hash_address() {
    for (layout, account_id) in [
        (Some(SOLANA_HD_LAYOUT_BIP44_CHANGE), 0),
        (Some(SOLANA_HD_LAYOUT_BIP44), 0),
        (None, 1),
    ] {
        assert!(
            hd_address(layout, account_id) != PASSPHRASE_ADDRESS,
            "an HD address equals the passphrase-hash address"
        );
    }
    let ctx = single_key_ctx_with(vec![platform_entry()]);
    let coin = built_platform(&ctx, &platform_entry(), json!({})).expect("the platform coin is built");
    assert!(
        address_of(&coin) == PASSPHRASE_ADDRESS,
        "the single-key address differs from the vector"
    );
}

/// R40.1A.2: an absent `hd_path_layout` is `bip44_change`; an absent `path_to_address` is account 0.
#[test]
fn t40_1a_2_absent_layout_is_bip44_change() {
    assert!(
        hd_address(None, 0) == BIP44_CHANGE_ACCOUNT_0,
        "the default layout differs from bip44_change"
    );
    let ctx = hd_ctx_with(vec![platform_entry()]);
    let coin = built_platform(&ctx, &platform_entry(), json!({})).expect("the platform coin is built");
    assert!(
        address_of(&coin) == BIP44_CHANGE_ACCOUNT_0,
        "the default account differs from account 0"
    );
    let coin = built_platform(&ctx, &platform_entry(), json!({ "path_to_address": null }))
        .expect("the platform coin is built");
    assert!(
        address_of(&coin) == BIP44_CHANGE_ACCOUNT_0,
        "a null path_to_address differs from the default"
    );
}

/// R40.1A.6: an `hd_path_layout` other than the two values fails request parsing (`InvalidRequest`, HTTP 400 at
/// the RPC layer, which maps every request-parse failure that way).
#[test]
fn t40_1a_6_unknown_layout_fails_request_parsing() {
    for bad in [json!("bip44_extra"), json!("BIP44"), json!(""), json!(44)] {
        let parsed =
            serde_json::from_value::<EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest>>(json!({
                "ticker": "SOL",
                "confirmation_commitment": "finalized",
                "client_url": CLOSED_NODE,
                "spl_tokens_requests": [],
                "hd_path_layout": bad,
            }));
        assert!(parsed.is_err(), "an unknown layout parsed");
    }
}

/// T40.1A.3 / R40.1A.1: under a single-key login the layout and `path_to_address` change nothing, whatever they
/// hold.
#[test]
fn t40_1a_3_single_key_login_ignores_layout_and_path_to_address() {
    let ctx = single_key_ctx_with(vec![platform_entry()]);
    let plain = address_of(&built_platform(&ctx, &platform_entry(), json!({})).expect("a coin"));
    for extra in [
        json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44 }),
        json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44_CHANGE, "path_to_address": { "account_id": 5 } }),
        json!({ "path_to_address": { "chain": "Internal", "address_id": 9 } }),
        json!({ "path_to_address": "not an object" }),
    ] {
        let coin = built_platform(&ctx, &platform_entry(), extra).expect("a coin");
        assert!(
            address_of(&coin) == plain,
            "a layout or path_to_address changed the single-key address"
        );
    }
}

/// R40.1A.2 / R40.1A.6: `derivation_path` absent gives the default `m/44'/501'`; a different base gives another
/// address; a value that is not a two-level hardened path is a `PlatformCoinCreationError` naming it.
#[test]
fn t40_1a_2_derivation_path_default_and_malformed() {
    let ctx = hd_ctx_with(vec![platform_entry()]);
    let default_entry = entry_with_path(Some(json!("m/44'/501'")));
    let coin = built_platform(&ctx, &default_entry, json!({})).expect("a coin");
    assert!(
        address_of(&coin) == BIP44_CHANGE_ACCOUNT_0,
        "the explicit default base differs from the absent one"
    );
    let other = entry_with_path(Some(json!("m/44'/502'")));
    let coin = built_platform(&ctx, &other, json!({})).expect("a coin");
    assert!(
        address_of(&coin) != BIP44_CHANGE_ACCOUNT_0,
        "another base gave the default address"
    );
    for bad in [
        json!("this is not a path"),
        json!("m/44'"),
        json!("m/44'/501'/0'"),
        json!("m/44/501'"),
        json!("m/44'/501"),
        json!("44'/501'"),
        json!("m/44'/2147483648'"),
        json!(501),
    ] {
        let err = built_platform(&ctx, &entry_with_path(Some(bad)), json!({})).unwrap_err();
        let text = creation_error_text(err.get_inner());
        assert!(
            text.contains("derivation_path"),
            "the error does not name derivation_path"
        );
    }
}

/// R40.1A.6: under HD, `path_to_address` that does not parse, a `chain` other than `External`, and an
/// `address_id` other than 0 are `PlatformCoinCreationError` naming `path_to_address`.
#[test]
fn t40_1a_6_hd_path_to_address_refusals() {
    let ctx = hd_ctx_with(vec![platform_entry()]);
    for extra in [
        json!({ "path_to_address": { "chain": "Internal" } }),
        json!({ "path_to_address": { "address_id": 1 } }),
        json!({ "path_to_address": { "account_id": 1, "chain": "Internal", "address_id": 1 } }),
        json!({ "path_to_address": "not an object" }),
        json!({ "path_to_address": { "account_id": "zero" } }),
    ] {
        let err = built_platform(&ctx, &platform_entry(), extra).unwrap_err();
        let text = creation_error_text(err.get_inner());
        assert!(
            text.contains("path_to_address"),
            "the error does not name path_to_address"
        );
    }
}

/// R40.1A.6: a hardware `priv_key_policy` is refused before any node is contacted, under either login. The
/// request carries no node URL, so a build that got as far as the node check would answer with the endpoint error.
#[test]
fn t40_1a_6_trezor_policy_is_refused_before_any_node() {
    for ctx in [
        hd_ctx_with(vec![platform_entry()]),
        single_key_ctx_with(vec![platform_entry()]),
    ] {
        for form in [json!("Trezor"), json!({ "type": "Trezor" })] {
            let req = platform_request(None, &[], json!({ "priv_key_policy": form }));
            let priv_key = *ctx.legacy_passphrase_key_pair().private().secret;
            let err = block_on(<SolanaCoin as PlatformWithTokensActivationOps>::enable_platform_coin(
                ctx.clone(),
                "SOL".to_owned(),
                platform_entry(),
                req.request,
                SolanaProtocolInfo {},
                &priv_key,
            ))
            .unwrap_err();
            let text = creation_error_text(err.get_inner());
            assert!(text.contains("hardware key policy"), "unexpected refusal text");
            assert!(!text.contains("RPC endpoint"), "the refusal came after the node check");
        }
    }
}

/// R40.1A.6: through the generic activation the Trezor refusal has the `PlatformCoinCreationError` wire shape
/// (HTTP 500, `{ticker, error}`) and registers nothing.
#[test]
fn t40_1a_6_trezor_policy_wire_shape_through_the_generic_activation() {
    let ctx = hd_ctx_with(vec![platform_entry()]);
    let req = platform_request(Some(CLOSED_NODE), &[], json!({ "priv_key_policy": "Trezor" }));
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

/// R40.1A.6: the WalletConnect form, and any other unaccepted form, fails request parsing.
#[test]
fn t40_1a_6_unaccepted_priv_key_policy_forms_fail_request_parsing() {
    for form in [
        json!({ "type": "WalletConnect", "data": { "session_topic": "topic" } }),
        json!({ "type": "WalletConnect" }),
        json!("WalletConnect"),
        json!("NoSuchPolicy"),
        json!(7),
    ] {
        let parsed =
            serde_json::from_value::<EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest>>(json!({
                "ticker": "SOL",
                "confirmation_commitment": "finalized",
                "client_url": CLOSED_NODE,
                "spl_tokens_requests": [],
                "priv_key_policy": form,
            }));
        assert!(parsed.is_err(), "an unaccepted priv_key_policy form parsed");
    }
}

/// R40.1A.1, R40.1A.6: `ContextPrivKey` and an absent policy activate under either login with the login's own key;
/// `IguanaPrivKey` is honoured under either login: under a single-key login the passphrase-hash address, under HD
/// the address of the context's internal key (the one the UTXO family uses), whatever `hd_path_layout` and
/// `path_to_address` hold, and `derivation_method` `Iguana`.
#[test]
fn t40_1a_6_context_and_iguana_policies() {
    let hd = hd_ctx_with(vec![platform_entry()]);
    for form in [json!("ContextPrivKey"), json!({ "type": "ContextPrivKey" })] {
        let coin = built_platform(&hd, &platform_entry(), json!({ "priv_key_policy": form })).expect("a coin");
        assert!(
            address_of(&coin) == BIP44_CHANGE_ACCOUNT_0,
            "ContextPrivKey changed the HD address"
        );
    }
    let single = single_key_ctx_with(vec![platform_entry()]);
    for policy in ["ContextPrivKey", "IguanaPrivKey"] {
        let coin = built_platform(&single, &platform_entry(), json!({ "priv_key_policy": policy })).expect("a coin");
        assert!(
            address_of(&coin) == PASSPHRASE_ADDRESS,
            "a policy changed the single-key address"
        );
    }
    // Under an HD login the request is honoured: the context's internal key, with or without HD members.
    let internal_address = {
        let secret = crypto::CryptoCtx::from_ctx(&hd)
            .expect("the crypto context")
            .mm2_internal_privkey_secret();
        let params: SolanaActivationParams = serde_json::from_value(json!({
            "confirmation_commitment": "finalized",
            "client_url": CLOSED_NODE,
        }))
        .expect("well-formed activation parameters");
        let coin = block_on(solana_coin_from_conf_and_params(
            "SOL",
            &platform_entry(),
            params,
            secret.as_slice(),
        ))
        .expect("the platform coin is built");
        address_of(&coin)
    };
    assert!(
        internal_address != PASSPHRASE_ADDRESS && internal_address != BIP44_CHANGE_ACCOUNT_0,
        "the internal-key address equals another vector"
    );
    for extra in [
        json!({ "priv_key_policy": "IguanaPrivKey" }),
        json!({ "priv_key_policy": { "type": "IguanaPrivKey" } }),
        json!({
            "priv_key_policy": "IguanaPrivKey",
            "hd_path_layout": SOLANA_HD_LAYOUT_BIP44,
            "path_to_address": { "account_id": 1, "chain": "Internal", "address_id": 3 },
        }),
    ] {
        let coin = built_platform(&hd, &platform_entry(), extra.clone()).expect("a coin");
        assert!(
            address_of(&coin) == internal_address,
            "IguanaPrivKey under HD did not give the internal-key address"
        );
        let request = platform_request(None, &[], extra).request;
        let method = derivation_method_of(&hd, "SOL", &request).expect("a derivation method");
        assert!(
            serde_json::to_value(method).expect("serializable") == json!({ "type": "Iguana" }),
            "IguanaPrivKey under HD did not give the Iguana derivation method"
        );
    }
    // Under a single-key login the method is Iguana as well.
    let request = platform_request(None, &[], json!({ "priv_key_policy": "IguanaPrivKey" })).request;
    let method = derivation_method_of(&single, "SOL", &request).expect("a derivation method");
    assert!(
        serde_json::to_value(method).expect("serializable") == json!({ "type": "Iguana" }),
        "unexpected derivation method"
    );
}

/// R40.1A.7: the `derivation_method` wire shape under HD (full path, both layouts) and under a single-key login.
#[test]
fn t40_1a_7_derivation_method_wire_shape() {
    let hd = hd_ctx_with(vec![platform_entry()]);
    let request = |extra: Json| platform_request(None, &[], extra).request;
    let wire = |ctx: &MmArc, extra: Json| {
        let method = derivation_method_of(ctx, "SOL", &request(extra)).expect("a derivation method");
        serde_json::to_value(method).expect("serializable")
    };
    assert!(
        wire(&hd, json!({})) == json!({ "type": "HDWallet", "data": "m/44'/501'/0'/0'" }),
        "unexpected wire"
    );
    assert!(
        wire(
            &hd,
            json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44, "path_to_address": { "account_id": 1 } })
        ) == json!({ "type": "HDWallet", "data": "m/44'/501'/1'" }),
        "unexpected wire"
    );
    assert!(
        wire(
            &hd,
            json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44_CHANGE, "path_to_address": { "account_id": 2 } })
        ) == json!({ "type": "HDWallet", "data": "m/44'/501'/2'/0'" }),
        "unexpected wire"
    );
    let single = single_key_ctx_with(vec![platform_entry()]);
    assert!(
        wire(&single, json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44 })) == json!({ "type": "Iguana" }),
        "unexpected wire"
    );
}

/// R40.1A.7: the path in the result follows the coin entry's `derivation_path`.
#[test]
fn t40_1a_7_derivation_method_follows_the_entry_base_path() {
    let entry = entry_with_path(Some(json!("m/44'/502'")));
    let hd = hd_ctx_with(vec![entry]);
    let request = platform_request(None, &[], json!({})).request;
    let method = derivation_method_of(&hd, "SOL", &request).expect("a derivation method");
    assert!(
        serde_json::to_value(method).expect("serializable")
            == json!({ "type": "HDWallet", "data": "m/44'/502'/0'/0'" }),
        "unexpected wire"
    );
}

/// R40.1A.1 / brief item 5: a token uses the platform's key, so under HD its address (the balance key of the
/// inline-token result and of `enable_spl`) is the HD platform address.
#[test]
fn t40_1a_5_token_under_hd_uses_the_hd_platform_address() {
    let hd = hd_ctx_with(vec![platform_entry()]);
    let platform = built_platform(
        &hd,
        &platform_entry(),
        json!({ "hd_path_layout": SOLANA_HD_LAYOUT_BIP44 }),
    )
    .expect("a coin");
    assert!(address_of(&platform) == BIP44_ACCOUNT_0, "unexpected platform address");
    let token = SplToken::new(
        6,
        "TOK-SOL".to_owned(),
        MINT.parse().expect("a mint"),
        SPL_TOKEN_PROGRAM.parse().expect("a program"),
        platform.clone(),
    );
    assert!(
        token.my_address().expect("an address") == BIP44_ACCOUNT_0,
        "the token address is not the HD one"
    );
}

/// The single-token activation records the token on its platform once it is registered, so the platform reports it
/// and the unregister hook of `disable_coin` has a record to drop (CRD ch.40 T40.6B.20, ch.43 R43.1E.5). It also
/// joins the platform's dependent set, and `disable_coin` takes it out of both.
#[test]
fn enable_spl_registration_records_the_token_and_disable_coin_removes_it_everywhere() {
    use crate::token::TokenActivationOps;

    let ctx = ctx_with(vec![platform_entry()]);
    let platform = platform_with_closed_node(&ctx, &platform_entry());
    block_on(lp_register_coin(
        &ctx,
        MmCoinEnum::SolanaCoin(platform.clone()),
        RegisterCoinParams {
            ticker: "SOL".to_owned(),
            tx_history: false,
        },
    ))
    .expect("the platform coin is registered");
    let proto = CoinProtocol::from_conf_json(solanatoken(MINT, SPL_TOKEN_PROGRAM)).expect("a parsable protocol");
    let conf = SplProtocolConf::try_from_coin_protocol(proto).expect("a Solana token protocol");
    let token = SplToken::new(
        conf.decimals,
        "TOK-SOL".to_owned(),
        conf.token_contract_address,
        conf.program_id,
        platform.clone(),
    );

    let coins_ctx = coins::CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_token("SOL", token.clone().into())).expect("the token is registered");
    <SplToken as TokenActivationOps>::on_token_registered(&platform, &token);

    assert!(platform.get_spl_tokens_infos().contains_key("TOK-SOL"));
    assert_eq!(
        block_on(coins_ctx.dependents_of("SOL")).into_iter().collect::<Vec<_>>(),
        vec!["TOK-SOL".to_owned()]
    );

    block_on(coins::disable_coin(&ctx, "TOK-SOL")).expect("the token is disabled");

    assert!(platform.get_spl_tokens_infos().is_empty());
    assert!(block_on(coins_ctx.dependents_of("SOL")).is_empty());
}
