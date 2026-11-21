//! Browser tests of the Solana code on the WASM target (CRD ch.40 package S8; run with
//! `wasm-pack test --firefox --headless mm2src/coins`). They need no network and no stub node: the coins-file forms
//! (R40.1.2, R40.1.3), the node pool's memory of a failing node (R40.1.5) with an injected clock, and the host-only
//! naming of a node in every error text and debug form (R40.5A.1, T40.10). The node answers and the pool's request
//! handling are covered natively by the stub-node tests; the code that sends is the same on both targets. No assertion
//! message carries a key or an address.

use crate::solana::rpc_client::SolanaRpcClient;
use crate::solana::rpc_pool::{PoolClock, SolanaNodeOrder, SolanaRpcPool, SET_ASIDE_FIRST_MS, SET_ASIDE_MAX_MS};
use crate::CoinProtocol;
use rand::rngs::SmallRng;
use rand::SeedableRng;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// A clock the test moves by hand.
#[derive(Debug, Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn advance(&self, ms: u64) { self.0.fetch_add(ms, Ordering::SeqCst); }
}

impl PoolClock for TestClock {
    fn now_ms(&self) -> u64 { self.0.load(Ordering::SeqCst) + 1_000 }
}

/// T40.2 on WASM: the platform descriptor parses in its three forms, and both token forms parse with their keys
/// checked when the protocol is parsed.
#[wasm_bindgen_test]
fn coins_file_forms_parse_on_wasm() {
    for protocol in [
        json!({"type": "SOLANA"}),
        json!({"type": "SOLANA", "protocol_data": {}}),
        json!({"type": "SOLANA", "protocol_data": null}),
    ] {
        assert!(
            matches!(CoinProtocol::from_conf_json(protocol), Ok(CoinProtocol::SOLANA)),
            "a platform form is refused"
        );
    }

    let legacy = CoinProtocol::from_conf_json(json!({
        "type": "SPLTOKEN",
        "protocol_data": {"platform": "SOL", "token_contract_address": MINT, "decimals": 6}
    }));
    assert!(
        matches!(&legacy, Ok(CoinProtocol::SPLTOKEN { platform, decimals: 6, .. }) if platform == "SOL"),
        "the SPLTOKEN form is not read"
    );

    let reference = json!({
        "type": "SOLANATOKEN",
        "protocol_data": {"platform": "SOL", "decimals": 6, "mint_address": MINT, "program_id": SPL_TOKEN_PROGRAM}
    });
    let parsed = CoinProtocol::from_conf_json(reference.clone());
    assert!(
        matches!(&parsed, Ok(CoinProtocol::SOLANATOKEN { platform, decimals: 6, .. }) if platform == "SOL"),
        "the SOLANATOKEN form is not read"
    );
    let Ok(protocol) = parsed else {
        panic!("the SOLANATOKEN form is not read");
    };
    assert!(
        serde_json::to_value(protocol).ok().as_ref() == Some(&reference),
        "the SOLANATOKEN form does not round-trip"
    );

    // An invalid key makes the protocol unparsable.
    let mut bad = reference;
    bad["protocol_data"]["mint_address"] = json!("not base58 0OIl");
    assert!(CoinProtocol::from_conf_json(bad).is_err(), "an invalid key is accepted");
}

/// R40.1.5 on WASM: a failing node is set aside for 20 s, the time doubles while it keeps failing up to 12 h, with
/// the injected clock (the pool reads `common::now_ms`, which is the browser clock, in production).
#[wasm_bindgen_test]
fn pool_set_aside_doubles_up_to_the_cap_on_wasm() {
    let clock = Arc::new(TestClock::default());
    let pool = SolanaRpcPool::with_parts(
        vec!["http://127.0.0.1:1".to_owned()],
        None,
        SolanaNodeOrder::Listed,
        clock.clone(),
        &mut SmallRng::seed_from_u64(1),
    )
    .expect("a non-empty pool");

    let mut expected = SET_ASIDE_FIRST_MS;
    for _ in 0..30 {
        pool.report_failure_for_test(0);
        assert!(
            pool.set_aside_remaining_ms_for_test(0) == expected,
            "the set-aside time is not the expected one"
        );
        clock.advance(expected);
        expected = (expected * 2).min(SET_ASIDE_MAX_MS);
    }
    assert!(expected == SET_ASIDE_MAX_MS, "the cap is not reached");
}

/// T40.10 on WASM: a request to a node whose address carries credentials fails, and the error texts and the debug
/// forms of the client and the pool name the node host-only. (A browser refuses such an address before any network
/// access; the text it gives is scrubbed of the address.)
#[wasm_bindgen_test]
async fn a_node_is_named_host_only_in_errors_and_debug_forms_on_wasm() {
    let url = "http://wasmuser:wasmsecret@127.0.0.1:1/v1/WASMAPIKEY?token=WASMTOKEN";
    let client = SolanaRpcClient::new(url);
    let pool = SolanaRpcPool::from_urls([url]).expect("a non-empty pool");

    let error = client.get_health().await.err().map(|e| e.to_string());
    let pool_error = pool.get_health().await.err().map(|e| e.to_string());
    assert!(
        error.is_some() && pool_error.is_some(),
        "a request to the node must fail"
    );

    let client_debug = format!("{client:?}");
    let pool_debug = format!("{pool:?}");
    assert!(
        client_debug.contains("http://127.0.0.1:1"),
        "the client names the node host-only"
    );
    assert!(
        pool_debug.contains("http://127.0.0.1:1"),
        "the pool names the node host-only"
    );

    for text in [error, pool_error, Some(client_debug), Some(pool_debug)]
        .into_iter()
        .flatten()
    {
        for secret in [
            "wasmuser",
            "wasmsecret",
            "/v1/",
            "WASMAPIKEY",
            "token=WASMTOKEN",
            "WASMTOKEN",
        ] {
            assert!(!text.contains(secret), "a credential part reaches a text");
        }
    }
}
