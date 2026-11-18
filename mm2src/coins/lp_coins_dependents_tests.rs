//! Offline tests of the registry's dependent sets and of the token-record hook of `disable_coin` (CRD ch.43
//! R43.1E.5 items 1-2 and the compatibility note under R43.1E.8). The coins are the test coin and the BCH coin over
//! an unconnected client, registered as the activation paths do.

use super::*;
use crate::lp_coins_ops::forget_token_records;
use crate::utxo::bch::bch_coin_without_network_for_test;
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use primitives::hash::H256;
use std::collections::BTreeSet;

fn test_coin(ticker: &str) -> MmCoinEnum { MmCoinEnum::Test(TestCoin::new(ticker)) }

fn ctx_and_coins() -> (MmArc, Arc<CoinsContext>) {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    (ctx, coins_ctx)
}

fn set(tickers: &[&str]) -> BTreeSet<String> { tickers.iter().map(|t| (*t).to_owned()).collect() }

#[test]
fn a_platform_with_tokens_activation_records_its_tokens_as_dependents() {
    let (_ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1"), test_coin("T2")])).unwrap();

    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["T1", "T2"]));
    assert!(block_on(coins_ctx.dependents_of("T1")).is_empty());
    assert!(block_on(coins_ctx.dependents_of("OTHER")).is_empty());
}

#[test]
fn add_token_registers_the_coin_and_records_it_as_a_dependent() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("PLAT"))).unwrap();

    block_on(coins_ctx.add_token("PLAT", test_coin("T1"))).unwrap();

    assert!(block_on(lp_coinfind(&ctx, "T1")).unwrap().is_some());
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["T1"]));
}

#[test]
fn add_token_refuses_a_registered_ticker_and_records_nothing() {
    let (_ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("PLAT"))).unwrap();
    block_on(coins_ctx.add_coin(test_coin("T1"))).unwrap();

    let err = block_on(coins_ctx.add_token("PLAT", test_coin("T1"))).unwrap_err();

    assert_eq!(err.get_inner().ticker, "T1");
    assert!(
        block_on(coins_ctx.dependents_of("PLAT")).is_empty(),
        "the refused coin does not join"
    );
}

#[test]
fn a_token_registered_by_the_legacy_path_is_not_a_dependent() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("PLAT"))).unwrap();
    let params = RegisterCoinParams {
        ticker: "LEGACY".to_owned(),
        tx_history: false,
    };
    block_on(lp_register_coin(&ctx, test_coin("LEGACY"), params)).unwrap();

    assert!(block_on(lp_coinfind(&ctx, "LEGACY")).unwrap().is_some());
    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
}

#[test]
fn disabling_a_token_takes_it_out_of_the_set_and_the_last_one_drops_the_entry() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1"), test_coin("T2")])).unwrap();

    block_on(disable_coin(&ctx, "T1")).unwrap();
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["T2"]));
    assert!(
        block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_some(),
        "the platform stays"
    );

    block_on(disable_coin(&ctx, "T2")).unwrap();
    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
}

#[test]
fn disabling_a_platform_drops_its_set_and_its_tokens_stay_registered() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1")])).unwrap();

    block_on(disable_coin(&ctx, "PLAT")).unwrap();

    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
    assert!(block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_none());
    assert!(
        block_on(lp_coinfind(&ctx, "T1")).unwrap().is_some(),
        "the token stays registered"
    );
    // The token can be disabled afterwards; its platform is not registered, so no record changes.
    block_on(disable_coin(&ctx, "T1")).unwrap();
}

#[test]
fn a_disabled_platform_can_be_activated_again_with_an_empty_set() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1")])).unwrap();
    block_on(disable_coin(&ctx, "PLAT")).unwrap();

    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![])).unwrap();

    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
}

fn slp_token(bch: &BchCoin, ticker: &str) -> SlpToken {
    let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
    SlpToken::new(4, ticker.into(), token_id, bch.clone(), 0)
}

/// Registers an SLP token as `enable_slp` does: in the registry as a dependent, then in the platform's records.
fn register_slp(coins_ctx: &CoinsContext, bch: &BchCoin, token: &SlpToken) {
    block_on(coins_ctx.add_token(bch.ticker(), token.clone().into())).unwrap();
    bch.add_slp_token_info(token.ticker().into(), token.get_info());
}

#[test]
fn disabling_an_slp_token_takes_it_out_of_the_set_and_the_platform_records() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let bch = bch_coin_without_network_for_test();
    block_on(coins_ctx.add_coin(bch.clone().into())).unwrap();
    let (fusd, other) = (slp_token(&bch, "FUSD"), slp_token(&bch, "OTHER"));
    register_slp(&coins_ctx, &bch, &fusd);
    register_slp(&coins_ctx, &bch, &other);
    assert_eq!(block_on(coins_ctx.dependents_of("BCH")), set(&["FUSD", "OTHER"]));
    assert_eq!(bch.get_slp_tokens_infos().len(), 2);

    block_on(disable_coin(&ctx, "FUSD")).unwrap();

    assert_eq!(block_on(coins_ctx.dependents_of("BCH")), set(&["OTHER"]));
    let infos = bch.get_slp_tokens_infos();
    assert!(infos.contains_key("OTHER") && !infos.contains_key("FUSD"));
}

#[test]
fn the_hook_is_idempotent() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let bch = bch_coin_without_network_for_test();
    block_on(coins_ctx.add_coin(bch.clone().into())).unwrap();
    let (fusd, other) = (slp_token(&bch, "FUSD"), slp_token(&bch, "OTHER"));
    register_slp(&coins_ctx, &bch, &fusd);
    register_slp(&coins_ctx, &bch, &other);
    block_on(disable_coin(&ctx, "FUSD")).unwrap();

    // A second call for the same token, and a call after the platform left the registry, change nothing.
    let registry = block_on(coins_ctx.coins.lock());
    forget_token_records(&registry, &fusd.clone().into(), "FUSD");
    assert_eq!(bch.get_slp_tokens_infos().len(), 1);
    forget_token_records(&registry, &test_coin("NOT-A-TOKEN"), "NOT-A-TOKEN");
    drop(registry);
    assert!(block_on(disable_coin(&ctx, "FUSD")).is_err(), "disabled already");
    assert_eq!(bch.get_slp_tokens_infos().len(), 1);
    assert_eq!(block_on(coins_ctx.dependents_of("BCH")), set(&["OTHER"]));
}
