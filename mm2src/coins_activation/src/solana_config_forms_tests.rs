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
use coins::{CoinProtocol, SolanaCoin, SplToken};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};

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
        token_contract_address: MINT.to_owned(),
        decimals: 6,
    };
    let conf = SplProtocolConf::try_from_coin_protocol(proto).unwrap();
    assert_eq!(conf.platform_coin_ticker, "SOL");
    assert_eq!(conf.decimals, 6);
    assert_eq!(conf.token_contract_address, MINT);
    assert_eq!(conf.program_id.to_string(), SPL_TOKEN_PROGRAM);
}

#[test]
fn spl_protocol_conf_from_solanatoken_keeps_the_token_2022_program() {
    let proto = CoinProtocol::from_conf_json(solanatoken(MINT, TOKEN_2022_PROGRAM)).unwrap();
    let conf = SplProtocolConf::try_from_coin_protocol(proto).unwrap();
    assert_eq!(conf.platform_coin_ticker, "SOL");
    assert_eq!(conf.decimals, 6);
    assert_eq!(conf.token_contract_address, MINT);
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
            assert_eq!(ticker, "SOL");
            assert!(error.contains("at least one RPC endpoint"), "{error}");
        },
        other => panic!("expected PlatformCoinCreationError, got {other:?}"),
    }
}
