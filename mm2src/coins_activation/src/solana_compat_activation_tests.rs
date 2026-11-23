//! The compatibility activation methods `experimental::enable_solana_with_assets` and
//! `experimental::enable_solana_token` (CRD ch.40 §40.1-§40.3, T40.2-T40.7, R40.1.5, R40.1A.5, R40.1A.6, R40.6B.6),
//! against a scripted loopback node. The activation entry points that skip the beta switch are called directly. No
//! network is used. No assertion message carries a key or an address: they are compared with
//! `assert!(a == b, "<fixed text>")`.

use crate::evm_test_support::{spawn_node, MockEvmNode, Reply};
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq};
use crate::solana_compat_activation::{activate_solana_token, activate_solana_with_assets, SolanaCompatPlatformRequest,
                                      SolanaCompatTokenRequest};
use crate::solana_with_tokens_activation::SolanaWithTokensActivationRequest;
use crate::spl_token_activation::SplActivationRequest;
use crate::token::{enable_token, EnableTokenError, EnableTokenRequest};
use coins::solana::spl::associated_token_account;
use coins::{lp_coinfind, lp_coinfind_or_err, lp_register_coin, solana_coin_from_conf_and_params, MarketCoinOps,
            MmCoinEnum, RegisterCoinParams, SolanaActivationParams, SolanaCoin, SplToken};
use common::mm_number::BigDecimal;
use common::{block_on, HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};
use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const MINT2: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
/// The swap program of the devnet lab (CRD ch.40 F40.6B.5).
const SWAP_PROGRAM: &str = "54mDjQz6jS9sggCV1P5GoUzqHaBEueGVZqmaMPLt2tDx";
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
/// A node that refuses the connection. The path and the query stand in for a credential: no error text may carry
/// them (R40.5A.1).
const CLOSED_NODE: &str = "http://127.0.0.1:1/v1/PATH-SECRET?token=QUERY-SECRET";
const HD_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const BIP44_CHANGE_ACCOUNT_0: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
const BIP44_ACCOUNT_0: &str = "GjJyeC1r2RgkuoCWMyPYkCWSGSGLcz266EaAkLA27AhL";

fn solanatoken(platform: &str, mint: &str, program: &str) -> Json {
    json!({ "type": "SOLANATOKEN", "protocol_data": {
        "platform": platform, "decimals": 6, "mint_address": mint, "program_id": program } })
}

fn coins() -> Vec<Json> {
    vec![
        json!({ "coin": "SOL", "mm2": 1, "protocol": { "type": "SOLANA", "protocol_data": {} } }),
        json!({ "coin": "TOK-SOL", "mm2": 1, "protocol": solanatoken("SOL", MINT, SPL_TOKEN_PROGRAM) }),
        json!({ "coin": "OTHER-TOK", "mm2": 1, "protocol": solanatoken("OTHER", MINT, SPL_TOKEN_PROGRAM) }),
        json!({ "coin": "BAD-TOK", "mm2": 1, "protocol": solanatoken("SOL", "not a base58 key", SPL_TOKEN_PROGRAM) }),
        json!({ "coin": "UTXO-TOK", "mm2": 1, "protocol": { "type": "UTXO" } }),
        json!({ "coin": "BADPROTO", "mm2": 1, "protocol": { "type": "NO_SUCH_PROTOCOL" } }),
        json!({ "coin": "UTXOCOIN", "mm2": 1, "protocol": { "type": "UTXO" } }),
        json!({ "coin": "SUB-TOK", "mm2": 1, "protocol": solanatoken("TOK-SOL", MINT2, SPL_TOKEN_PROGRAM) }),
        json!({ "coin": "SOLPROG", "mm2": 1, "protocol": { "type": "SOLANA",
            "protocol_data": { "swap_program_id": SWAP_PROGRAM } } }),
        json!({ "coin": "SOLBADPROG", "mm2": 1, "protocol": { "type": "SOLANA",
            "protocol_data": { "swap_program_id": "not base58 0OIl" } } }),
    ]
}

fn ctx() -> MmArc {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins() })).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "solana compat test passphrase")
        .expect("initialize the crypto context");
    ctx
}

/// A single-key context whose `MM2.json` holds `solana_node_order`.
fn ctx_with_order(order: &str) -> MmArc {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "coins": coins(), "solana_node_order": order }))
        .into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "solana compat test passphrase")
        .expect("initialize the crypto context");
    ctx
}

fn hd_ctx() -> MmArc {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins() })).into_mm_arc();
    crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).expect("initialize the HD crypto context");
    ctx
}

/// What the scripted node holds.
#[derive(Clone, Copy)]
struct NodeSpec {
    /// The owner of the mint accounts; `None`: they do not exist.
    mint_owner: Option<&'static str>,
    /// Whether the wallet has a token account holding 2.5 tokens.
    token_account: bool,
    /// Whether `getHealth` answers `ok`; otherwise it answers an error.
    health_ok: bool,
}

const HEALTHY: NodeSpec = NodeSpec {
    mint_owner: Some(SPL_TOKEN_PROGRAM),
    token_account: false,
    health_ok: true,
};

const UNHEALTHY: NodeSpec = NodeSpec {
    health_ok: false,
    ..HEALTHY
};

/// A scripted node that records every request as `<method> <params>`.
struct Node {
    node: MockEvmNode,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Node {
    fn url(&self) -> &str { &self.node.url }

    fn requests(&self) -> Vec<String> { self.seen.lock().expect("the request log").clone() }

    fn count(&self) -> usize { self.requests().len() }

    fn count_of(&self, method: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.starts_with(&format!("{method} ")))
            .count()
    }
}

fn node(spec: NodeSpec) -> Node {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let node = spawn_node(move |method, params| {
        log.lock().expect("the request log").push(format!("{method} {params}"));
        match method {
            "getHealth" if spec.health_ok => Reply::Result(json!("ok")),
            "getHealth" => Reply::RpcError,
            "getAccountInfo" => {
                let key = params[0].as_str().unwrap_or_default();
                let is_mint = key == MINT || key == MINT2;
                let present = if is_mint {
                    spec.mint_owner.is_some()
                } else {
                    spec.token_account
                };
                let owner = if is_mint {
                    spec.mint_owner.unwrap_or(SPL_TOKEN_PROGRAM)
                } else {
                    SPL_TOKEN_PROGRAM
                };
                let value = if present {
                    json!({ "owner": owner, "lamports": 1, "data": ["", "base64"], "executable": false,
                        "rentEpoch": 0 })
                } else {
                    Json::Null
                };
                Reply::Result(json!({ "context": { "slot": 1 }, "value": value }))
            },
            "getBalance" => Reply::Result(json!({ "context": { "slot": 1 }, "value": 1_500_000_000u64 })),
            "getBlockHeight" => Reply::Result(json!(42)),
            "getTokenAccountBalance" => Reply::Result(json!({ "context": { "slot": 1 }, "value": {
                "amount": "2500000", "decimals": 6, "uiAmount": 2.5, "uiAmountString": "2.5" } })),
            _ => Reply::RpcError,
        }
    });
    Node { node, seen }
}

fn merged(mut body: Json, extra: Json) -> Json {
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    body
}

fn platform_req(nodes: &[&str], tokens: Json, extra: Json) -> SolanaCompatPlatformRequest {
    let nodes: Vec<Json> = nodes.iter().map(|url| json!({ "url": url })).collect();
    let body = merged(
        json!({ "ticker": "SOL", "nodes": nodes, "tokens_params": tokens }),
        extra,
    );
    serde_json::from_value(body).expect("a well-formed request")
}

fn token_req(ticker: &str, protocol: Option<Json>) -> SolanaCompatTokenRequest {
    let mut body = json!({ "ticker": ticker, "activation_params": {} });
    if let Some(protocol) = protocol {
        body["protocol"] = protocol;
    }
    serde_json::from_value(body).expect("a well-formed request")
}

fn primary_platform_req(
    urls: &[&str],
    tokens: &[&str],
) -> EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest> {
    let tokens: Vec<Json> = tokens.iter().map(|ticker| json!({ "ticker": ticker })).collect();
    serde_json::from_value(json!({ "ticker": "SOL", "confirmation_commitment": "finalized",
        "client_urls": urls, "spl_tokens_requests": tokens }))
    .expect("a well-formed request")
}

fn primary_token_req(ticker: &str) -> EnableTokenRequest<SplActivationRequest> {
    serde_json::from_value(json!({ "ticker": ticker, "activation_params": {} })).expect("a well-formed request")
}

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

fn keys_of(value: &Json) -> BTreeSet<String> { value.as_object().expect("an object").keys().cloned().collect() }

fn keys(names: &[&str]) -> BTreeSet<String> { names.iter().map(|name| (*name).to_owned()).collect() }

fn assert_no_node_secrets(text: &str) {
    for secret in ["PATH-SECRET", "QUERY-SECRET", "/v1/", "token="] {
        assert!(!text.contains(secret), "an error text leaked a part of the node URL");
    }
}

fn registered(ctx: &MmArc, ticker: &str) -> bool {
    block_on(lp_coinfind(ctx, ticker)).map_or(true, |coin| coin.is_some())
}

fn decimal(value: &Json) -> BigDecimal {
    BigDecimal::from_str(value.as_str().expect("a decimal string")).expect("a decimal")
}

fn platform_failure(ctx: &MmArc, req: SolanaCompatPlatformRequest) -> (Json, StatusCode) {
    let err = block_on(activate_solana_with_assets(ctx.clone(), req))
        .map(|_| ())
        .expect_err("the activation fails");
    platform_wire(err.get_inner())
}

fn token_failure(ctx: &MmArc, req: SolanaCompatTokenRequest) -> (Json, StatusCode) {
    let err = block_on(activate_solana_token(ctx.clone(), req))
        .map(|_| ())
        .expect_err("the activation fails");
    token_wire(err.get_inner())
}

/// Activates the platform through the compatibility method on `url`, no tokens.
fn activate_platform(ctx: &MmArc, url: &str) {
    block_on(activate_solana_with_assets(
        ctx.clone(),
        platform_req(&[url], json!([]), json!({})),
    ))
    .map_err(|e| e.into_inner())
    .expect("the platform activates");
}

/// Registers a platform coin on `url` directly.
fn register_platform(ctx: &MmArc, url: &str) -> SolanaCoin {
    let params: SolanaActivationParams =
        serde_json::from_value(json!({ "confirmation_commitment": "finalized", "client_url": url }))
            .expect("well-formed activation parameters");
    let priv_key = &*ctx.legacy_passphrase_key_pair().private().secret;
    let platform = block_on(solana_coin_from_conf_and_params("SOL", &coins()[0], params, priv_key))
        .expect("the platform coin is built");
    block_on(lp_register_coin(
        ctx,
        MmCoinEnum::SolanaCoin(platform.clone()),
        RegisterCoinParams {
            ticker: "SOL".to_owned(),
            tx_history: false,
        },
    ))
    .expect("the platform coin is registered");
    platform
}

// ---------------------------------------------------------------------------
// T40.6, T40.7, results
// ---------------------------------------------------------------------------

/// T40.6, R40.2.3: the platform result carries exactly its five members, balances as decimal strings.
#[test]
fn t40_6_platform_result_has_exactly_the_documented_members() {
    let node = node(NodeSpec {
        token_account: true,
        ..HEALTHY
    });
    let ctx = ctx();
    let req = platform_req(&[node.url()], json!([{ "ticker": "TOK-SOL" }]), json!({}));
    let result = block_on(activate_solana_with_assets(ctx.clone(), req))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds");
    let wire = serde_json::to_value(&result).expect("serializable result");
    assert!(
        keys_of(&wire) == keys(&["ticker", "address", "current_block", "balance", "tokens_balances"]),
        "the members of the result"
    );
    assert!(wire["ticker"] == "SOL", "the ticker");
    assert!(
        wire["current_block"] == json!(42) && wire["current_block"].is_u64(),
        "an integer block"
    );
    assert!(wire["address"].is_string(), "the address is a string");
    assert!(
        keys_of(&wire["balance"]) == keys(&["spendable", "unspendable"]),
        "the balance members"
    );
    assert!(
        decimal(&wire["balance"]["spendable"]) == BigDecimal::from_str("1.5").expect("a decimal"),
        "SOL"
    );
    assert!(
        decimal(&wire["balance"]["unspendable"]) == BigDecimal::from(0),
        "unspendable is zero"
    );
    assert!(
        keys_of(&wire["tokens_balances"]) == keys(&["TOK-SOL"]),
        "one entry per token"
    );
    let token = &wire["tokens_balances"]["TOK-SOL"];
    assert!(
        keys_of(token) == keys(&["spendable", "unspendable"]),
        "the token balance members"
    );
    assert!(
        decimal(&token["spendable"]) == BigDecimal::from_str("2.5").expect("a decimal"),
        "token amount"
    );
    let platform = block_on(lp_coinfind_or_err(&ctx, "SOL")).expect("the platform is registered");
    assert!(
        platform.my_address().expect("an address") == wire["address"].as_str().expect("a string"),
        "the platform address"
    );
}

/// R40.2.3: no token gives an empty map; a wallet without a token account reads zero (D40.6B.2 (a)).
#[test]
fn platform_result_with_no_tokens_or_no_token_account() {
    let node = node(HEALTHY);
    let ctx = ctx();
    let wire = serde_json::to_value(
        block_on(activate_solana_with_assets(
            ctx.clone(),
            platform_req(&[node.url()], json!([]), json!({})),
        ))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds"),
    )
    .expect("serializable result");
    assert!(wire["tokens_balances"] == json!({}), "an empty map");

    let ctx = self::ctx();
    let wire = serde_json::to_value(
        block_on(activate_solana_with_assets(
            ctx,
            platform_req(&[node.url()], json!([{ "ticker": "TOK-SOL" }]), json!({})),
        ))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds"),
    )
    .expect("serializable result");
    assert!(
        decimal(&wire["tokens_balances"]["TOK-SOL"]["spendable"]) == BigDecimal::from(0),
        "zero"
    );
}

/// T40.7 and T40.6C.1 (R40.6C.1): `tx_history` never changes the activation answer and never aborts; `true` enables
/// the history of the registered platform, `false` or absent does not; a value that is not a boolean is a
/// request-parse failure (`InvalidRequest`, HTTP 400 at the RPC layer).
#[test]
fn t40_7_tx_history_changes_no_answer_and_enables_the_history_only_when_true() {
    let node = node(HEALTHY);
    let mut results = Vec::new();
    for (extra, enabled) in [
        (json!({}), false),
        (json!({ "tx_history": false }), false),
        (json!({ "tx_history": true }), true),
    ] {
        let ctx = ctx();
        let result = block_on(activate_solana_with_assets(
            ctx.clone(),
            platform_req(&[node.url()], json!([{ "ticker": "TOK-SOL" }]), extra),
        ))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds");
        results.push(serde_json::to_value(&result).expect("serializable result"));
        assert!(
            history_enabled(&ctx) == enabled,
            "the history is enabled exactly when asked"
        );
    }
    assert!(
        results.windows(2).all(|pair| pair[0] == pair[1]),
        "tx_history changed the result"
    );
    for bad in [json!("yes"), json!(1), json!(null)] {
        let body = json!({ "ticker": "SOL", "nodes": [{ "url": node.url() }], "tokens_params": [], "tx_history": bad });
        assert!(
            serde_json::from_value::<SolanaCompatPlatformRequest>(body).is_err(),
            "a non-boolean parsed"
        );
    }
}

/// T40.6C.1: the primary surface honors `tx_history` too, and a second activation of the same platform starts no
/// second history (the decision is a pure rule, tested with the activation hook).
#[test]
fn t40_6c_1_the_primary_surface_honors_tx_history_and_a_reactivation_starts_none() {
    use crate::solana_with_tokens_activation::starts_solana_history;
    assert!(starts_solana_history(true, false), "asked and new: starts");
    assert!(!starts_solana_history(false, false), "not asked: no history");
    assert!(!starts_solana_history(true, true), "a re-activation starts none");
    assert!(
        !starts_solana_history(false, true),
        "neither does it without the member"
    );

    let node = node(HEALTHY);
    for (asked, enabled) in [(false, false), (true, true)] {
        let ctx = self::ctx();
        let request: EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest> =
            serde_json::from_value(json!({ "ticker": "SOL", "confirmation_commitment": "finalized",
                "client_urls": [node.url()], "spl_tokens_requests": [], "tx_history": asked }))
            .expect("a well-formed request");
        block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx.clone(), request))
            .map_err(|e| e.into_inner())
            .expect("the primary platform activates");
        assert!(
            history_enabled(&ctx) == enabled,
            "the history is enabled exactly when asked"
        );
    }
}

/// Whether the registered platform `SOL` has its history enabled.
fn history_enabled(ctx: &MmArc) -> bool {
    match block_on(lp_coinfind_or_err(ctx, "SOL")).expect("registered") {
        MmCoinEnum::SolanaCoin(coin) => coin.history.is_enabled(),
        _ => panic!("not a Solana platform"),
    }
}

/// The primary request still requires `confirmation_commitment`, and an unknown level fails parsing too.
#[test]
fn primary_request_still_requires_a_known_confirmation_commitment() {
    let body = |commitment: Option<Json>| {
        let mut body = json!({ "ticker": "SOL", "client_url": CLOSED_NODE, "spl_tokens_requests": [] });
        if let Some(commitment) = commitment {
            body["confirmation_commitment"] = commitment;
        }
        body
    };
    type Req = EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest>;
    assert!(
        serde_json::from_value::<Req>(body(None)).is_err(),
        "a missing commitment parsed"
    );
    assert!(
        serde_json::from_value::<Req>(body(Some(json!("sometimes")))).is_err(),
        "an unknown level parsed"
    );
    assert!(
        serde_json::from_value::<Req>(body(Some(json!(null)))).is_err(),
        "a null level parsed"
    );
    for level in ["processed", "confirmed", "finalized"] {
        assert!(
            serde_json::from_value::<Req>(body(Some(json!(level)))).is_ok(),
            "a known level failed"
        );
    }
}

/// R40.3.3: the token result is `{ticker, address, current_block, balance}`; the address is the associated token
/// account of the wallet for the mint and the token's program.
#[test]
fn token_result_members_and_address() {
    let node = node(NodeSpec {
        token_account: true,
        ..HEALTHY
    });
    let ctx = ctx();
    let platform = register_platform(&ctx, node.url());
    let result = block_on(activate_solana_token(ctx.clone(), token_req("TOK-SOL", None)))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds");
    let wire = serde_json::to_value(&result).expect("serializable result");
    assert!(
        keys_of(&wire) == keys(&["ticker", "address", "current_block", "balance"]),
        "the members of the result"
    );
    assert!(wire["ticker"] == "TOK-SOL", "the ticker");
    assert!(wire["current_block"] == json!(42), "the block height");
    assert!(
        decimal(&wire["balance"]["spendable"]) == BigDecimal::from_str("2.5").expect("a decimal"),
        "balance"
    );
    assert!(
        decimal(&wire["balance"]["unspendable"]) == BigDecimal::from(0),
        "unspendable"
    );
    let wallet = platform
        .my_address()
        .expect("an address")
        .parse()
        .expect("a public key");
    let expected = associated_token_account(
        &wallet,
        &MINT.parse().expect("a mint"),
        &SPL_TOKEN_PROGRAM.parse().expect("a program"),
    );
    assert!(wire["address"] == expected.to_string(), "the token account address");
    assert!(registered(&ctx, "TOK-SOL"), "registered");
    assert!(
        platform.get_spl_tokens_infos().contains_key("TOK-SOL"),
        "in the platform's token set"
    );
}

// ---------------------------------------------------------------------------
// Errors (T40.2-T40.5, R40.2.4, R40.3.4)
// ---------------------------------------------------------------------------

/// T40.3: unresponsive nodes with one inline token: `Transport`, a string, HTTP 502; with none:
/// `PlatformCoinCreationError`, `{ticker, error}`, HTTP 500. Nodes are named host-only; nothing is registered.
#[test]
fn t40_3_unresponsive_nodes_split_on_inline_tokens() {
    let ctx = ctx();
    let (wire, status) = platform_failure(
        &ctx,
        platform_req(&[CLOSED_NODE], json!([{ "ticker": "TOK-SOL" }]), json!({})),
    );
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "the wire shape"
    );
    assert!(status == StatusCode::BAD_GATEWAY, "HTTP 502");
    assert_no_node_secrets(&wire.to_string());
    assert!(
        !registered(&ctx, "SOL") && !registered(&ctx, "TOK-SOL"),
        "nothing is registered"
    );

    let (wire, status) = platform_failure(&ctx, platform_req(&[CLOSED_NODE], json!([]), json!({})));
    assert!(wire["error_type"] == "PlatformCoinCreationError", "the error type");
    assert!(wire["error_data"]["ticker"] == "SOL", "the platform ticker");
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
        "the members"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
    assert_no_node_secrets(&wire.to_string());
    assert!(!registered(&ctx, "SOL"), "nothing is registered");
}

/// T40.4: the token method with unresponsive nodes: `Transport`, a string, HTTP 500.
#[test]
fn t40_4_token_method_with_unresponsive_nodes_is_a_transport_error() {
    let ctx = ctx();
    register_platform(&ctx, CLOSED_NODE);
    let (wire, status) = token_failure(&ctx, token_req("TOK-SOL", None));
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "the wire shape"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
    assert_no_node_secrets(&wire.to_string());
    assert!(!registered(&ctx, "TOK-SOL"), "nothing is registered");
}

/// T40.5: `PlatformCoinMismatch`, HTTP 400, no `error_data` member, before any node is contacted; for a configured
/// token and for a custom one.
#[test]
fn t40_5_platform_coin_mismatch_has_no_error_data() {
    let node = node(HEALTHY);
    for tokens in [
        json!([{ "ticker": "OTHER-TOK" }]),
        json!([{ "ticker": "CUSTOM-TOK", "protocol": solanatoken("OTHER", MINT2, SPL_TOKEN_PROGRAM) }]),
    ] {
        let ctx = ctx();
        let (wire, status) = platform_failure(&ctx, platform_req(&[node.url()], tokens, json!({})));
        assert!(wire["error_type"] == "PlatformCoinMismatch", "the error type");
        assert!(wire.get("error_data").is_none(), "no error_data member");
        assert!(status == StatusCode::BAD_REQUEST, "HTTP 400");
        assert!(!registered(&ctx, "SOL"), "nothing is registered");
    }
    assert!(node.count() == 0, "no node was contacted");
}

/// T40.2 on both methods: a `SOLANATOKEN` with an invalid key, configured or request-supplied, is a
/// `TokenProtocolParseError` (HTTP 500, `{ticker, error}`); an unparsable inline protocol too.
#[test]
fn t40_2_invalid_solanatoken_is_a_protocol_parse_error_on_both_methods() {
    let node = node(HEALTHY);
    let bad_inline = solanatoken("SOL", "not a base58 key", SPL_TOKEN_PROGRAM);
    for (ticker, protocol) in [
        ("BAD-TOK", None),
        ("CUSTOM-BAD", Some(bad_inline.clone())),
        ("CUSTOM-JUNK", Some(json!("junk"))),
    ] {
        let ctx = ctx();
        let tokens = match &protocol {
            Some(protocol) => json!([{ "ticker": ticker, "protocol": protocol }]),
            None => json!([{ "ticker": ticker }]),
        };
        let (wire, status) = platform_failure(&ctx, platform_req(&[node.url()], tokens, json!({})));
        assert!(
            wire["error_type"] == "TokenProtocolParseError",
            "platform: the error type"
        );
        assert!(wire["error_data"]["ticker"] == ticker, "platform: the ticker");
        assert!(
            keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
            "platform: the members"
        );
        assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "platform: HTTP 500");

        register_platform(&ctx, node.url());
        let (wire, status) = token_failure(&ctx, token_req(ticker, protocol));
        assert!(wire["error_type"] == "TokenProtocolParseError", "token: the error type");
        assert!(
            keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
            "token: the members"
        );
        assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "token: HTTP 500");
    }
}

/// Every R40.2.4 row that is reachable without a node, as the RPC layer serializes it. The nodes are a closed port:
/// a check that came after the first node request would show as `Transport`.
#[test]
fn r40_2_4_rows_without_a_node() {
    let ctx = ctx();
    let nodes = [CLOSED_NODE];

    // Requests with another ticker are built from JSON directly.
    let request = |ticker: &str, tokens: Json| -> SolanaCompatPlatformRequest {
        serde_json::from_value(json!({ "ticker": ticker, "nodes": [{ "url": nodes[0] }], "tokens_params": tokens }))
            .expect("a well-formed request")
    };
    let expect = |ctx: &MmArc, req: SolanaCompatPlatformRequest, error_type: &str, code: StatusCode| -> Json {
        let (wire, status) = platform_failure(ctx, req);
        assert!(wire["error_type"] == error_type, "the error type");
        assert!(status == code, "the status");
        wire
    };

    let wire = expect(
        &ctx,
        request("NOPE", json!([])),
        "PlatformConfigIsNotFound",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "NOPE", "a string with the ticker");

    let wire = expect(
        &ctx,
        request("BADPROTO", json!([])),
        "CoinProtocolParseError",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
        "the members"
    );

    let wire = expect(
        &ctx,
        request("UTXOCOIN", json!([])),
        "UnexpectedPlatformProtocol",
        StatusCode::BAD_REQUEST,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "protocol"]),
        "the members"
    );
    assert!(wire["error_data"]["protocol"]["type"] == "UTXO", "the protocol object");

    let wire = expect(
        &ctx,
        request("SOL", json!([{ "ticker": "NO-SUCH-TOKEN" }])),
        "TokenConfigIsNotFound",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "NO-SUCH-TOKEN", "a string with the token ticker");

    let wire = expect(
        &ctx,
        request("SOL", json!([{ "ticker": "UTXO-TOK" }])),
        "UnexpectedTokenProtocol",
        StatusCode::BAD_REQUEST,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "protocol"]),
        "the members"
    );

    let wire = expect(
        &ctx,
        request(
            "SOL",
            json!([{ "ticker": "TOK-SOL", "protocol": solanatoken("SOL", MINT2, SPL_TOKEN_PROGRAM) }]),
        ),
        "CustomTokenError",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    assert!(
        wire["error_data"] == json!({ "DuplicateTickerInConfig": { "ticker_in_config": "TOK-SOL" } }),
        "the externally tagged data"
    );

    // An empty node list parses and then fails as a platform-coin creation error.
    let empty: SolanaCompatPlatformRequest =
        serde_json::from_value(json!({ "ticker": "SOL", "nodes": [], "tokens_params": [] }))
            .expect("an empty node list parses");
    let wire = expect(
        &ctx,
        empty,
        "PlatformCoinCreationError",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
        "the members"
    );

    // Already activated.
    register_platform(&ctx, CLOSED_NODE);
    let wire = expect(
        &ctx,
        request("SOL", json!([])),
        "PlatformIsAlreadyActivated",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "SOL", "a string with the ticker");

    // An unparsable node URL is a request-parse failure.
    for bad in [json!("not a url"), json!("localhost:8899"), json!(5)] {
        let parsed = serde_json::from_value::<SolanaCompatPlatformRequest>(
            json!({ "ticker": "SOL", "nodes": [{ "url": bad }], "tokens_params": [] }),
        );
        assert!(parsed.is_err(), "an unparsable URL parsed");
    }
    // Required members.
    for body in [
        json!({ "ticker": "SOL", "tokens_params": [] }),
        json!({ "ticker": "SOL", "nodes": [] }),
    ] {
        assert!(
            serde_json::from_value::<SolanaCompatPlatformRequest>(body).is_err(),
            "a missing member parsed"
        );
    }
}

/// Every R40.3.4 row that is reachable without a node. `CustomTokenError` is HTTP 400 on this method.
#[test]
fn r40_3_4_rows_without_a_node() {
    let ctx = ctx();
    let expect = |ctx: &MmArc, req: SolanaCompatTokenRequest, error_type: &str, code: StatusCode| -> Json {
        let (wire, status) = token_failure(ctx, req);
        assert!(wire["error_type"] == error_type, "the error type");
        assert!(status == code, "the status");
        wire
    };
    let wire = expect(
        &ctx,
        token_req("NOPE", None),
        "TokenConfigIsNotFound",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "NOPE", "a string");
    let wire = expect(
        &ctx,
        token_req("UTXO-TOK", None),
        "UnexpectedTokenProtocol",
        StatusCode::BAD_REQUEST,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "protocol"]),
        "the members"
    );
    let wire = expect(
        &ctx,
        token_req("NEW-TOK", Some(json!({ "type": "UTXO" }))),
        "UnexpectedTokenProtocol",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"]["ticker"] == "NEW-TOK", "the ticker");
    let wire = expect(
        &ctx,
        token_req("TOK-SOL", Some(solanatoken("SOL", MINT2, SPL_TOKEN_PROGRAM))),
        "CustomTokenError",
        StatusCode::BAD_REQUEST,
    );
    assert!(
        wire["error_data"] == json!({ "DuplicateTickerInConfig": { "ticker_in_config": "TOK-SOL" } }),
        "the externally tagged data"
    );
    // The platform is not active.
    let wire = expect(
        &ctx,
        token_req("TOK-SOL", None),
        "PlatformCoinIsNotActivated",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "SOL", "the platform ticker");

    // The platform is active but is not a Solana platform: an active token stands in for it.
    let platform = register_platform(&ctx, CLOSED_NODE);
    let token = SplToken::new(
        6,
        "TOK-SOL".to_owned(),
        MINT.parse().expect("a mint"),
        SPL_TOKEN_PROGRAM.parse().expect("a program"),
        platform,
    );
    block_on(lp_register_coin(
        &ctx,
        MmCoinEnum::SplToken(token),
        RegisterCoinParams {
            ticker: "TOK-SOL".to_owned(),
            tx_history: false,
        },
    ))
    .expect("the token is registered");
    let wire = expect(
        &ctx,
        token_req("SUB-TOK", None),
        "UnsupportedPlatformCoin",
        StatusCode::INTERNAL_SERVER_ERROR,
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["platform_coin_ticker", "token_ticker"]),
        "the members"
    );
    // Already activated.
    let wire = expect(
        &ctx,
        token_req("TOK-SOL", None),
        "TokenIsAlreadyActivated",
        StatusCode::BAD_REQUEST,
    );
    assert!(wire["error_data"] == "TOK-SOL", "a string with the token ticker");
}

// ---------------------------------------------------------------------------
// Custom tokens (R40.6B.6), Token-2022, mint owner
// ---------------------------------------------------------------------------

/// A custom token on both methods activates from the request's protocol and is wallet-only; a configured token is
/// not.
#[test]
fn custom_tokens_activate_from_the_request_and_are_wallet_only() {
    let node = node(HEALTHY);
    let ctx = ctx();
    let custom = solanatoken("SOL", MINT2, SPL_TOKEN_PROGRAM);
    let wire = serde_json::to_value(
        block_on(activate_solana_with_assets(
            ctx.clone(),
            platform_req(
                &[node.url()],
                json!([{ "ticker": "TOK-SOL" }, { "ticker": "CUSTOM-ONE", "protocol": custom }]),
                json!({}),
            ),
        ))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds"),
    )
    .expect("serializable result");
    assert!(
        keys_of(&wire["tokens_balances"]) == keys(&["TOK-SOL", "CUSTOM-ONE"]),
        "both tokens"
    );
    let wallet_only = |ticker: &str| {
        let coin = block_on(lp_coinfind_or_err(&ctx, ticker)).expect("registered");
        coin.wallet_only(&ctx)
    };
    assert!(wallet_only("CUSTOM-ONE"), "a custom token is wallet-only");
    assert!(!wallet_only("TOK-SOL"), "a configured token follows its entry");

    // The token method, with another mint.
    let result = block_on(activate_solana_token(
        ctx.clone(),
        token_req("CUSTOM-TWO", Some(solanatoken("SOL", MINT, SPL_TOKEN_PROGRAM))),
    ))
    .map_err(|e| e.into_inner())
    .expect("the activation succeeds");
    assert!(
        serde_json::to_value(&result).expect("serializable")["ticker"] == "CUSTOM-TWO",
        "the ticker"
    );
    assert!(wallet_only("CUSTOM-TWO"), "a custom token is wallet-only");
}

/// Token-2022 is refused before any node on both methods (the shapes of D40.6B.4), custom tokens included.
#[test]
fn custom_token_2022_is_refused_before_any_node() {
    let node = node(HEALTHY);
    let ctx = ctx();
    let protocol = solanatoken("SOL", MINT2, TOKEN_2022_PROGRAM);
    let (wire, status) = platform_failure(
        &ctx,
        platform_req(
            &[node.url()],
            json!([{ "ticker": "CUSTOM-22", "protocol": protocol }]),
            json!({}),
        ),
    );
    assert!(wire["error_type"] == "PlatformCoinCreationError", "the error type");
    assert!(wire["error_data"]["ticker"] == "SOL", "the platform ticker");
    assert!(
        wire["error_data"]["error"] == "CUSTOM-22: Token-2022 tokens are not supported yet",
        "the text"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
    assert!(node.count() == 0, "no node contact");

    register_platform(&ctx, node.url());
    let before = node.count();
    let (wire, status) = token_failure(&ctx, token_req("CUSTOM-22", Some(protocol)));
    assert!(wire["error_type"] == "Internal", "the error type");
    assert!(
        wire["error_data"] == "CUSTOM-22: Token-2022 tokens are not supported yet",
        "the text"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
    assert!(node.count() == before, "no node contact");
    assert!(!registered(&ctx, "CUSTOM-22"), "nothing is registered");
}

/// A mint owned by another program is `Transport` on both methods (502 and 500) and nothing is registered.
#[test]
fn foreign_mint_owner_is_a_transport_error_and_registers_nothing() {
    let node = node(NodeSpec {
        mint_owner: Some(TOKEN_2022_PROGRAM),
        ..HEALTHY
    });
    let custom = solanatoken("SOL", MINT2, SPL_TOKEN_PROGRAM);
    let ctx = ctx();
    let (wire, status) = platform_failure(
        &ctx,
        platform_req(
            &[node.url()],
            json!([{ "ticker": "CUSTOM-X", "protocol": custom }]),
            json!({}),
        ),
    );
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "platform: the wire shape"
    );
    assert!(status == StatusCode::BAD_GATEWAY, "platform: HTTP 502");
    assert!(
        !registered(&ctx, "SOL") && !registered(&ctx, "CUSTOM-X"),
        "platform: nothing is registered"
    );

    let platform = register_platform(&ctx, node.url());
    let (wire, status) = token_failure(&ctx, token_req("CUSTOM-X", Some(custom)));
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "token: the wire shape"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "token: HTTP 500");
    assert!(!registered(&ctx, "CUSTOM-X"), "token: not registered");
    assert!(
        !platform.get_spl_tokens_infos().contains_key("CUSTOM-X"),
        "token: not in the token set"
    );
}

// ---------------------------------------------------------------------------
// One registry
// ---------------------------------------------------------------------------

/// A platform activated through either surface takes a token through the other surface's token method.
#[test]
fn both_surfaces_share_one_registry() {
    let node = node(HEALTHY);
    // Compatibility platform, primary token.
    let ctx = ctx();
    activate_platform(&ctx, node.url());
    block_on(enable_token::<SplToken>(ctx.clone(), primary_token_req("TOK-SOL")))
        .map_err(|e| e.into_inner())
        .expect("enable_spl takes a token on a compatibility platform");
    let platform = match block_on(lp_coinfind_or_err(&ctx, "SOL")).expect("registered") {
        MmCoinEnum::SolanaCoin(coin) => coin,
        _ => panic!("not a Solana platform"),
    };
    assert!(
        platform.get_spl_tokens_infos().contains_key("TOK-SOL"),
        "in the token set"
    );

    // Primary platform, compatibility token.
    let ctx = self::ctx();
    block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx.clone(),
        primary_platform_req(&[node.url()], &[]),
    ))
    .map_err(|e| e.into_inner())
    .expect("the primary platform activates");
    block_on(activate_solana_token(ctx.clone(), token_req("TOK-SOL", None)))
        .map_err(|e| e.into_inner())
        .expect("the compatibility token method takes a token on a primary platform");
    assert!(registered(&ctx, "TOK-SOL"), "registered");
}

// ---------------------------------------------------------------------------
// Keys (R40.1A.5, R40.1A.6)
// ---------------------------------------------------------------------------

/// A refused `priv_key_policy` is answered before any node is contacted; an unknown layout and the WalletConnect
/// form fail request parsing.
#[test]
fn r40_1a_policy_and_layout_rows_on_the_compatibility_method() {
    let node = node(HEALTHY);
    for form in [json!("Trezor"), json!({ "type": "Trezor" })] {
        let ctx = ctx();
        let (wire, status) = platform_failure(
            &ctx,
            platform_req(&[node.url()], json!([]), json!({ "priv_key_policy": form })),
        );
        assert!(wire["error_type"] == "PlatformCoinCreationError", "the error type");
        assert!(
            keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
            "the members"
        );
        assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
    }
    assert!(node.count() == 0, "a node was contacted");
    for extra in [
        json!({ "hd_path_layout": "bip44_extra" }),
        json!({ "priv_key_policy": { "type": "WalletConnect" } }),
        json!({ "priv_key_policy": "NoSuchPolicy" }),
    ] {
        let body = merged(
            json!({ "ticker": "SOL", "nodes": [{ "url": CLOSED_NODE }], "tokens_params": [] }),
            extra,
        );
        assert!(
            serde_json::from_value::<SolanaCompatPlatformRequest>(body).is_err(),
            "an unaccepted form parsed"
        );
    }
}

/// Under HD the layout and `path_to_address` apply and refusals are creation errors; under a single key they are
/// ignored.
#[test]
fn r40_1a_hd_and_single_key_on_the_compatibility_method() {
    let node = node(HEALTHY);
    let address = |ctx: &MmArc, extra: Json| {
        let wire = serde_json::to_value(
            block_on(activate_solana_with_assets(
                ctx.clone(),
                platform_req(&[node.url()], json!([]), extra),
            ))
            .map_err(|e| e.into_inner())
            .expect("the activation succeeds"),
        )
        .expect("serializable result");
        wire["address"].as_str().expect("an address").to_owned()
    };
    assert!(
        address(&hd_ctx(), json!({})) == BIP44_CHANGE_ACCOUNT_0,
        "the default HD address"
    );
    assert!(
        address(&hd_ctx(), json!({ "hd_path_layout": "bip44" })) == BIP44_ACCOUNT_0,
        "the bip44 address"
    );
    let plain = address(&ctx(), json!({}));
    assert!(
        address(
            &ctx(),
            json!({ "hd_path_layout": "bip44", "path_to_address": { "account_id": 5 } })
        ) == plain,
        "a single-key login ignores the members"
    );
    let (wire, status) = platform_failure(
        &hd_ctx(),
        platform_req(
            &[node.url()],
            json!([]),
            json!({ "path_to_address": { "chain": "Internal" } }),
        ),
    );
    assert!(wire["error_type"] == "PlatformCoinCreationError", "the error type");
    assert!(
        wire["error_data"]["error"]
            .as_str()
            .is_some_and(|t| t.contains("path_to_address")),
        "names the member"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
}

// ---------------------------------------------------------------------------
// Commitment (R40.6B.4)
// ---------------------------------------------------------------------------

/// A coin activated through the compatibility surface sends no commitment member; the primary surface still does.
#[test]
fn compatibility_coins_send_no_commitment_and_the_primary_surface_does() {
    let node = node(NodeSpec {
        token_account: true,
        ..HEALTHY
    });
    let ctx = ctx();
    block_on(activate_solana_with_assets(
        ctx.clone(),
        platform_req(&[node.url()], json!([{ "ticker": "TOK-SOL" }]), json!({})),
    ))
    .map_err(|e| e.into_inner())
    .expect("the activation succeeds");
    block_on(activate_solana_token(
        ctx.clone(),
        token_req("CUSTOM-ONE", Some(solanatoken("SOL", MINT2, SPL_TOKEN_PROGRAM))),
    ))
    .map_err(|e| e.into_inner())
    .expect("the activation succeeds");
    let seen = node.requests();
    assert!(seen.iter().any(|r| r.starts_with("getBalance ")), "the node was used");
    assert!(
        seen.iter().all(|r| !r.contains("ommitment")),
        "a request carried a commitment member"
    );

    let primary = self::node(HEALTHY);
    let ctx = self::ctx();
    block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx,
        primary_platform_req(&[primary.url()], &[]),
    ))
    .map_err(|e| e.into_inner())
    .expect("the primary activation succeeds");
    assert!(
        primary
            .requests()
            .iter()
            .any(|r| r.contains("\"commitment\":\"finalized\"")),
        "the primary surface sends its commitment"
    );
}

// ---------------------------------------------------------------------------
// Node pool at the activation level (R40.1.5), both surfaces
// ---------------------------------------------------------------------------

/// A node URL with a credential-like path and query; the stub ignores both.
fn secret_url(node: &Node) -> String { format!("{}/v1/PATH-SECRET?token=QUERY-SECRET", node.url()) }

/// A first node whose `getHealth` answers an error is skipped on both surfaces: the activation succeeds on the
/// second node and the first gets no request other than `getHealth`. The listed order keeps the bad node first.
#[test]
fn an_unhealthy_first_node_is_skipped_by_the_activation_on_both_surfaces() {
    // Compatibility surface.
    let (bad, good) = (node(UNHEALTHY), node(HEALTHY));
    let ctx = ctx_with_order("listed");
    block_on(activate_solana_with_assets(
        ctx,
        platform_req(&[bad.url(), good.url()], json!([{ "ticker": "TOK-SOL" }]), json!({})),
    ))
    .map_err(|e| e.into_inner())
    .expect("the compatibility activation succeeds on the second node");
    assert!(
        bad.count() > 0 && bad.count() == bad.count_of("getHealth"),
        "compat: only a health probe"
    );
    assert!(good.count_of("getBalance") > 0, "compat: the second node served");

    // Primary surface, through the generic activation the routed method runs.
    let (bad, good) = (node(UNHEALTHY), node(HEALTHY));
    let ctx = ctx_with_order("listed");
    block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx,
        primary_platform_req(&[bad.url(), good.url()], &["TOK-SOL"]),
    ))
    .map_err(|e| e.into_inner())
    .expect("the primary activation succeeds on the second node");
    assert!(
        bad.count() > 0 && bad.count() == bad.count_of("getHealth"),
        "primary: only a health probe"
    );
    assert!(good.count_of("getBalance") > 0, "primary: the second node served");
}

/// Every node answers `getHealth` with an error: the bound rows on both surfaces, nodes named host-only.
#[test]
fn no_healthy_node_gives_the_bound_rows_on_both_surfaces() {
    let bad = node(UNHEALTHY);
    let url = secret_url(&bad);
    let check_platform = |wire: &Json, status: StatusCode, with_token: bool| {
        if with_token {
            assert!(
                wire["error_type"] == "Transport" && wire["error_data"].is_string(),
                "the wire shape"
            );
            assert!(status == StatusCode::BAD_GATEWAY, "HTTP 502");
        } else {
            assert!(wire["error_type"] == "PlatformCoinCreationError", "the error type");
            assert!(wire["error_data"]["ticker"] == "SOL", "the platform ticker");
            assert!(
                keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
                "the members"
            );
            assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "HTTP 500");
        }
        assert_no_node_secrets(&wire.to_string());
    };
    for with_token in [true, false] {
        let tokens: Vec<&str> = if with_token { vec!["TOK-SOL"] } else { vec![] };
        // Compatibility platform method.
        let ctx = ctx_with_order("listed");
        let compat_tokens: Vec<Json> = tokens.iter().map(|t| json!({ "ticker": t })).collect();
        let (wire, status) = platform_failure(&ctx, platform_req(&[url.as_str()], json!(compat_tokens), json!({})));
        check_platform(&wire, status, with_token);
        assert!(!registered(&ctx, "SOL"), "compat: nothing is registered");
        // Primary platform method.
        let ctx = ctx_with_order("listed");
        let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
            ctx.clone(),
            primary_platform_req(&[url.as_str()], &tokens),
        ))
        .map(|_| ())
        .expect_err("no node is healthy");
        let (wire, status) = platform_wire(err.get_inner());
        check_platform(&wire, status, with_token);
        assert!(!registered(&ctx, "SOL"), "primary: nothing is registered");
    }

    // Token methods on an active platform whose only node is unhealthy.
    let ctx = ctx_with_order("listed");
    register_platform(&ctx, &url);
    let err = block_on(enable_token::<SplToken>(ctx.clone(), primary_token_req("TOK-SOL")))
        .map(|_| ())
        .expect_err("no node is healthy");
    let (wire, status) = token_wire(err.get_inner());
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "enable_spl: the wire shape"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "enable_spl: HTTP 500");
    assert_no_node_secrets(&wire.to_string());
    let (wire, status) = token_failure(&ctx, token_req("TOK-SOL", None));
    assert!(
        wire["error_type"] == "Transport" && wire["error_data"].is_string(),
        "compat token: the wire shape"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "compat token: HTTP 500");
    assert_no_node_secrets(&wire.to_string());
    assert!(!registered(&ctx, "TOK-SOL"), "nothing is registered");
    assert!(
        bad.count() == bad.count_of("getHealth"),
        "only health probes reached the node"
    );
}

/// An unknown `solana_node_order` in the context configuration does not stop an activation, on either surface. The
/// repository has no log capture reachable from this crate (the one in `coins` is crate-private and installs a
/// process-wide logger), so the warning is tested at the function the activation calls: `solana_node_order` is called
/// once per activation (in `enable_platform_coin`) and `parse_node_order` returns exactly one warning, naming the key
/// and the value, with the random order.
#[test]
fn an_unknown_solana_node_order_activates_with_random_order_and_one_warning() {
    use coins::solana::rpc_pool::{parse_node_order, solana_node_order, SolanaNodeOrder};
    let ctx = ctx_with_order("mixed");
    let (order, warning) = parse_node_order(&ctx.conf);
    assert!(order == SolanaNodeOrder::Random, "random order");
    let warning = warning.expect("exactly one warning");
    assert!(
        warning.contains("solana_node_order") && warning.contains("mixed"),
        "names key and value"
    );
    assert!(
        solana_node_order(&ctx) == SolanaNodeOrder::Random,
        "the activation's function gives random"
    );

    let node = node(HEALTHY);
    block_on(activate_solana_with_assets(
        ctx_with_order("mixed"),
        platform_req(&[node.url()], json!([{ "ticker": "TOK-SOL" }]), json!({})),
    ))
    .map_err(|e| e.into_inner())
    .expect("the compatibility activation succeeds");
    block_on(enable_platform_coin_with_tokens::<SolanaCoin>(
        ctx_with_order("mixed"),
        primary_platform_req(&[node.url()], &["TOK-SOL"]),
    ))
    .map_err(|e| e.into_inner())
    .expect("the primary activation succeeds");
}

// ---------------------------------------------------------------------------------------------------------------
// CRD ch.40 §40.6D: the swap program member, its check at activation, and the evidence node (S11b)
// ---------------------------------------------------------------------------------------------------------------

/// A scripted node whose program account of `SWAP_PROGRAM` is owned by `owner` (executable as given); `None`: the
/// account does not exist.
fn program_node(program: Option<(&'static str, bool)>) -> Node {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let node = spawn_node(move |method, params| {
        log.lock().expect("the request log").push(format!("{method} {params}"));
        match method {
            "getHealth" => Reply::Result(json!("ok")),
            "getAccountInfo" => {
                let value = match (params[0].as_str() == Some(SWAP_PROGRAM), program) {
                    (true, Some((owner, executable))) => json!({ "owner": owner, "lamports": 1,
                        "data": ["", "base64"], "executable": executable, "rentEpoch": 0 }),
                    _ => Json::Null,
                };
                Reply::Result(json!({ "context": { "slot": 1 }, "value": value }))
            },
            "getBalance" => Reply::Result(json!({ "context": { "slot": 1 }, "value": 1_500_000_000u64 })),
            "getBlockHeight" => Reply::Result(json!(42)),
            _ => Reply::RpcError,
        }
    });
    Node { node, seen }
}

fn registered_solana(ctx: &MmArc, ticker: &str) -> SolanaCoin {
    match block_on(lp_coinfind(ctx, ticker))
        .expect("the lookup")
        .expect("the coin is registered")
    {
        MmCoinEnum::SolanaCoin(coin) => coin,
        _ => panic!("a Solana platform coin"),
    }
}

/// R40.6B.3 item 1: an invalid `swap_program_id` makes the entry unparsable, on both surfaces, before any node is
/// contacted; the answer is the `CoinProtocolParseError` of HTTP 500 with `{ticker, error}`.
#[test]
fn t40_6d_1_an_invalid_swap_program_id_is_a_protocol_parse_error_on_both_surfaces() {
    let ctx = ctx();
    let (wire, status) = platform_failure(
        &ctx,
        platform_req(&[CLOSED_NODE], json!([]), json!({ "ticker": "SOLBADPROG" })),
    );
    assert!(wire["error_type"] == "CoinProtocolParseError", "compat: the error type");
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
        "compat: the members"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "compat: HTTP 500");
    assert_no_node_secrets(&wire.to_string());

    let mut request = primary_platform_req(&[CLOSED_NODE], &[]);
    request.ticker = "SOLBADPROG".to_owned();
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx.clone(), request))
        .map(|_| ())
        .expect_err("the activation fails");
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "CoinProtocolParseError",
        "primary: the error type"
    );
    assert!(
        keys_of(&wire["error_data"]) == keys(&["ticker", "error"]),
        "primary: the members"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "primary: HTTP 500");
}

/// R40.6D.2: the activation reads the program once; a program that passes makes the coin tradable, one that is
/// missing or not executable leaves it wallet-only with the matching reason, and the activation succeeds either way.
#[test]
fn t40_6d_1_the_activation_checks_the_program_and_succeeds_either_way() {
    for (program, tradable, reason) in [
        (Some(("BPFLoader2111111111111111111111111111111111", true)), true, None),
        (None, false, Some(coins::solana::WalletOnlyReason::ProgramMissing)),
        (
            Some(("BPFLoader2111111111111111111111111111111111", false)),
            false,
            Some(coins::solana::WalletOnlyReason::ProgramNotExecutable),
        ),
        (
            Some(("LoaderV411111111111111111111111111111111111", true)),
            false,
            Some(coins::solana::WalletOnlyReason::ProgramLoaderUnsupported),
        ),
    ] {
        let ctx = ctx();
        let node = program_node(program);
        block_on(activate_solana_with_assets(
            ctx.clone(),
            platform_req(&[node.url()], json!([]), json!({ "ticker": "SOLPROG" })),
        ))
        .map_err(|e| e.into_inner())
        .expect("the activation succeeds");
        let coin = registered_solana(&ctx, "SOLPROG");
        assert!(
            coin.swap_program().is_some() == tradable,
            "the coin trades only when the program passes"
        );
        assert!(
            coin.swap_wallet_only_reason() == reason,
            "the reason names which case applies"
        );
        assert!(node.count_of("getAccountInfo") == 1, "the program account is read once");
    }
    // Without the member the coin stays wallet-only and no program is read.
    let ctx = ctx();
    let node = program_node(None);
    activate_platform(&ctx, node.url());
    let coin = registered_solana(&ctx, "SOL");
    assert!(
        coin.swap_wallet_only_reason() == Some(coins::solana::WalletOnlyReason::NoProgramConfigured),
        "no member, no program"
    );
    assert!(node.count_of("getAccountInfo") == 0, "no program account is read");
}

/// R40.6D.16: `swap_evidence_node` must be one of the coin's nodes, on both surfaces; a URL that is not refuses the
/// activation before any node is contacted; one that is, is recorded.
#[test]
fn t40_6d_13_the_evidence_node_must_be_in_the_node_list_on_both_surfaces() {
    let node = node(HEALTHY);
    let ctx = ctx();
    let (wire, status) = platform_failure(
        &ctx,
        platform_req(
            &[node.url()],
            json!([]),
            json!({ "swap_evidence_node": "http://127.0.0.1:9/other" }),
        ),
    );
    assert!(
        wire["error_type"] == "SwapEvidenceNodeNotInList",
        "compat: the error type"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "compat: HTTP 500");
    assert!(node.count() == 0, "compat: no node was contacted");

    let request: EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest> = serde_json::from_value(json!({
        "ticker": "SOL", "confirmation_commitment": "finalized", "client_urls": [node.url()],
        "spl_tokens_requests": [], "swap_evidence_node": "http://127.0.0.1:9/other" }))
    .expect("a well-formed request");
    let err = block_on(enable_platform_coin_with_tokens::<SolanaCoin>(ctx.clone(), request))
        .map(|_| ())
        .expect_err("the activation fails");
    let (wire, status) = platform_wire(err.get_inner());
    assert!(
        wire["error_type"] == "SwapEvidenceNodeNotInList",
        "primary: the error type"
    );
    assert!(status == StatusCode::INTERNAL_SERVER_ERROR, "primary: HTTP 500");
    assert!(node.count() == 0, "primary: no node was contacted");

    // A node of the list is accepted on the compatibility surface (additive member).
    block_on(activate_solana_with_assets(
        ctx.clone(),
        platform_req(&[node.url()], json!([]), json!({ "swap_evidence_node": node.url() })),
    ))
    .map_err(|e| e.into_inner())
    .expect("the activation succeeds");
}
