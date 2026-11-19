//! The mint-owner check of Solana token activation on both surfaces (CRD ch.40 R40.2.2, R40.2.4, R40.3.4,
//! R40.3A.3, R40.3A.4, T40.6B.7), against a scripted loopback node. No network is used and the node stops when it
//! is dropped. No assertion message carries a key or an address.

use crate::evm_test_support::{spawn_node, MockEvmNode, Reply};
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq};
use crate::spl_token_activation::SplActivationRequest;
use crate::token::{enable_token, EnableTokenError, EnableTokenRequest};
use coins::{lp_coinfind, lp_register_coin, solana_coin_from_conf_and_params, MmCoinEnum, RegisterCoinParams,
            SolanaActivationParams, SolanaCoin, SplToken};
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

fn platform_entry() -> Json {
    json!({ "coin": "SOL", "mm2": 1, "protocol": { "type": "SOLANA", "protocol_data": {} } })
}

fn token_entry() -> Json {
    json!({ "coin": "TOK-SOL", "mm2": 1, "protocol": { "type": "SOLANATOKEN", "protocol_data": {
        "platform": "SOL", "decimals": 6, "mint_address": MINT, "program_id": SPL_TOKEN_PROGRAM } } })
}

fn ctx() -> MmArc {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "coins": [platform_entry(), token_entry()] }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "solana mint check test passphrase")
        .expect("initialize the crypto context");
    ctx
}

/// A node that holds a SOL balance and a block height, and whose mint account is owned by `mint_owner` (`None`: the
/// mint account does not exist). No token account of the wallet exists.
fn node(mint_owner: Option<&'static str>) -> CountingNode {
    let count = Arc::new(AtomicUsize::new(0));
    let counted = count.clone();
    let inner = spawn_node(move |method, params| {
        counted.fetch_add(1, Ordering::SeqCst);
        match method {
            "getHealth" => Reply::Result(json!("ok")),
            "getAccountInfo" => {
                let value = match (params[0].as_str(), mint_owner) {
                    (Some(MINT), Some(owner)) => json!({ "owner": owner, "lamports": 1, "data": ["", "base64"],
                    "executable": false, "rentEpoch": 0 }),
                    _ => Json::Null,
                };
                Reply::Result(json!({ "context": { "slot": 1 }, "value": value }))
            },
            "getBalance" => Reply::Result(json!({ "context": { "slot": 1 }, "value": 1_000_000_000u64 })),
            "getBlockHeight" => Reply::Result(json!(42)),
            _ => Reply::RpcError,
        }
    });
    CountingNode { inner, count }
}

/// The scripted node and the number of requests it has served.
struct CountingNode {
    inner: MockEvmNode,
    count: Arc<AtomicUsize>,
}

impl CountingNode {
    fn requests_seen(&self) -> usize { self.count.load(Ordering::SeqCst) }
}

impl std::ops::Deref for CountingNode {
    type Target = MockEvmNode;
    fn deref(&self) -> &MockEvmNode { &self.inner }
}

fn platform_request(
    url: &str,
) -> EnablePlatformCoinWithTokensReq<crate::solana_with_tokens_activation::SolanaWithTokensActivationRequest> {
    serde_json::from_value(json!({
        "ticker": "SOL",
        "confirmation_commitment": "finalized",
        "client_url": url,
        "spl_tokens_requests": [{ "ticker": "TOK-SOL" }],
    }))
    .expect("a well-formed request")
}

fn token_request() -> EnableTokenRequest<SplActivationRequest> {
    serde_json::from_value(json!({ "ticker": "TOK-SOL", "activation_params": {} })).expect("a well-formed request")
}

fn registered(ctx: &MmArc, ticker: &str) -> bool {
    block_on(lp_coinfind(ctx, ticker)).map_or(true, |coin| coin.is_some())
}

/// Registers a platform coin on `url` the way the token method finds it.
fn register_platform(ctx: &MmArc, url: &str) {
    let params: SolanaActivationParams = serde_json::from_value(json!({
        "confirmation_commitment": "finalized",
        "client_url": url,
    }))
    .expect("well-formed activation parameters");
    let priv_key = &*ctx.legacy_passphrase_key_pair().private().secret;
    let platform = block_on(solana_coin_from_conf_and_params(
        "SOL",
        &platform_entry(),
        params,
        priv_key,
    ))
    .expect("the platform coin is built");
    block_on(lp_register_coin(
        ctx,
        MmCoinEnum::SolanaCoin(platform),
        RegisterCoinParams {
            ticker: "SOL".to_owned(),
            tx_history: false,
        },
    ))
    .expect("the platform coin is registered");
}

/// R40.2.4: an inline token whose mint is owned by another program fails token initialisation with `Transport`
/// (a string, HTTP 502) and nothing is registered.
#[test]
fn inline_token_with_a_foreign_mint_owner_is_a_transport_error() {
    for owner in [TOKEN_2022_PROGRAM, "11111111111111111111111111111111"] {
        let node = node(Some(owner));
        let ctx = ctx();
        let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
            ctx.clone(),
            platform_request(&node.url),
        ))
        .map(|_| ())
        .expect_err("the mint owner is refused");
        assert!(
            matches!(err.get_inner(), EnablePlatformCoinWithTokensError::Transport(_)),
            "{err:?}"
        );
        assert!(err.get_inner().status_code() == StatusCode::BAD_GATEWAY, "HTTP 502");
        let wire = serde_json::to_value(err.get_inner()).expect("serializable error");
        assert!(
            wire["error_type"] == "Transport" && wire["error_data"].is_string(),
            "the wire shape"
        );
        assert!(
            !registered(&ctx, "SOL") && !registered(&ctx, "TOK-SOL"),
            "nothing is registered"
        );
    }
}

/// R40.2.2, T40.6B.7: a mint account that does not exist is accepted, and a wallet without a token account
/// activates with balance zero.
#[test]
fn inline_token_with_no_mint_account_and_no_token_account_activates_with_zero() {
    let node = node(None);
    let ctx = ctx();
    let result = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx.clone(),
        platform_request(&node.url),
    ))
    .map_err(|e| e.into_inner())
    .expect("the activation succeeds");
    let wire = serde_json::to_value(&result).expect("serializable result");
    let balances = wire["spl_addresses_infos"]
        .as_object()
        .and_then(|infos| infos.values().next())
        .map(|info| info["balances"]["TOK-SOL"].clone())
        .expect("one address entry");
    assert!(
        balances["spendable"] == "0" && balances["unspendable"] == "0",
        "balance zero"
    );
    assert!(
        registered(&ctx, "SOL") && registered(&ctx, "TOK-SOL"),
        "both are registered"
    );
}

/// A mint owned by the token's own program passes.
#[test]
fn inline_token_with_the_right_mint_owner_activates() {
    let node = node(Some(SPL_TOKEN_PROGRAM));
    let ctx = ctx();
    assert!(
        block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
            ctx.clone(),
            platform_request(&node.url)
        ))
        .is_ok(),
        "the right owner activates"
    );
    assert!(registered(&ctx, "TOK-SOL"), "the token is registered");
}

/// R40.3.4, R40.3A.4: on the token method a mint owned by another program is `Transport` (a string, HTTP 500) and
/// nothing is registered.
#[test]
fn token_method_with_a_foreign_mint_owner_is_a_transport_error() {
    let node = node(Some(TOKEN_2022_PROGRAM));
    let ctx = ctx();
    register_platform(&ctx, &node.url);
    let err = block_on(enable_token::<SplToken>(ctx.clone(), token_request()))
        .map(|_| ())
        .expect_err("the mint owner is refused");
    assert!(matches!(err.get_inner(), EnableTokenError::Transport(_)), "{err:?}");
    assert!(
        err.get_inner().status_code() == StatusCode::INTERNAL_SERVER_ERROR,
        "HTTP 500"
    );
    let wire = serde_json::to_value(err.get_inner()).expect("serializable error");
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "the wire shape"
    );
    assert!(!registered(&ctx, "TOK-SOL"), "the token is not registered");
}

/// R40.3.4, T40.6B.7: no mint account and no token account: the token method activates with balance zero.
#[test]
fn token_method_with_no_mint_account_and_no_token_account_activates_with_zero() {
    let node = node(None);
    let ctx = ctx();
    register_platform(&ctx, &node.url);
    let result = block_on(enable_token::<SplToken>(ctx.clone(), token_request()))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds");
    let wire = serde_json::to_value(&result).expect("serializable result");
    let balance = wire["balances"]
        .as_object()
        .and_then(|balances| balances.values().next())
        .cloned()
        .expect("one balance entry");
    assert!(balance["spendable"] == "0", "balance zero");
    assert!(registered(&ctx, "TOK-SOL"), "the token is registered");
}

// ---------------------------------------------------------------------------
// Token-2022 refusal (CRD ch.40 D40.6B.4, the maintainer's decision of 2026-11-18)
// ---------------------------------------------------------------------------

const REFUSAL: &str = "TOK-SOL: Token-2022 tokens are not supported yet";

fn ctx_2022() -> MmArc {
    let mut token = token_entry();
    token["protocol"]["protocol_data"]["program_id"] = json!(TOKEN_2022_PROGRAM);
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "coins": [platform_entry(), token] }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "solana mint check test passphrase")
        .expect("initialize the crypto context");
    ctx
}

/// An inline Token-2022 token: `PlatformCoinCreationError {ticker, error}`, HTTP 500, before any node contact,
/// nothing registered.
#[test]
fn inline_token_2022_is_refused_before_any_node_contact() {
    let node = node(Some(TOKEN_2022_PROGRAM));
    let ctx = ctx_2022();
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx.clone(),
        platform_request(&node.url),
    ))
    .map(|_| ())
    .expect_err("Token-2022 is refused");
    let wire = serde_json::to_value(err.get_inner()).expect("serializable error");
    assert!(wire["error_type"] == "PlatformCoinCreationError", "the error token");
    assert!(wire["error_data"]["ticker"] == "SOL", "the platform ticker");
    assert!(wire["error_data"]["error"] == REFUSAL, "the text");
    assert!(
        wire["error_data"].as_object().map(|o| o.len()) == Some(2),
        "exactly ticker and error"
    );
    assert!(
        err.get_inner().status_code() == StatusCode::INTERNAL_SERVER_ERROR,
        "HTTP 500"
    );
    assert!(
        !registered(&ctx, "SOL") && !registered(&ctx, "TOK-SOL"),
        "nothing is registered"
    );
    assert!(node.requests_seen() == 0, "no request reached the node");
}

/// `enable_spl` for a Token-2022 token: `Internal` with a string, HTTP 500, no node contact, nothing registered.
#[test]
fn enable_spl_token_2022_is_refused_before_any_node_contact() {
    let node = node(Some(TOKEN_2022_PROGRAM));
    let ctx = ctx_2022();
    register_platform(&ctx, &node.url);
    let err = block_on(enable_token::<SplToken>(ctx.clone(), token_request()))
        .map(|_| ())
        .expect_err("Token-2022 is refused");
    let wire = serde_json::to_value(err.get_inner()).expect("serializable error");
    assert!(wire["error_type"] == "Internal", "the error token");
    assert!(wire["error_data"] == REFUSAL, "a string with the text");
    assert!(
        err.get_inner().status_code() == StatusCode::INTERNAL_SERVER_ERROR,
        "HTTP 500"
    );
    assert!(!registered(&ctx, "TOK-SOL"), "the token is not registered");
    assert!(node.requests_seen() == 0, "no request reached the node");
}
