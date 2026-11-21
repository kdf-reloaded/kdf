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

// The passive state (CRD ch.43 R43.1E.5 item 3 and R43.1E.6)

fn passive_platform() -> (MmArc, Arc<CoinsContext>) {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1"), test_coin("T2")])).unwrap();
    assert!(block_on(coins_ctx.make_passive("PLAT")));
    (ctx, coins_ctx)
}

#[test]
fn only_a_registered_platform_with_a_dependent_can_become_passive() {
    let (_ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("LONE"))).unwrap();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1")])).unwrap();

    assert!(!block_on(coins_ctx.make_passive("NOT-REGISTERED")));
    assert!(
        !block_on(coins_ctx.make_passive("LONE")),
        "a coin without a dependent is never passive"
    );
    assert!(!block_on(coins_ctx.make_passive("T1")), "a token is never passive");
    assert!(!block_on(coins_ctx.is_passive("LONE")) && !block_on(coins_ctx.is_passive("T1")));

    assert!(block_on(coins_ctx.make_passive("PLAT")));
    assert!(block_on(coins_ctx.is_passive("PLAT")));
}

#[test]
fn a_passive_platform_is_registered_but_not_active() {
    let (ctx, coins_ctx) = passive_platform();

    assert!(block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_none(), "not active");
    assert!(
        block_on(lp_coinfind_registered(&ctx, "PLAT")).unwrap().is_some(),
        "registered"
    );
    assert!(block_on(find_pair(&ctx, "PLAT", "T1")).unwrap().is_none());
    assert!(
        block_on(find_pair(&ctx, "T1", "T2")).unwrap().is_some(),
        "the tokens stay active"
    );
    assert!(block_on(lp_coinfind_or_err(&ctx, "PLAT")).is_err());
    assert!(block_on(lp_coinfind(&ctx, "T1")).unwrap().is_some());
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["T1", "T2"]));
}

#[test]
fn get_enabled_coins_lists_a_passive_platform() {
    let (ctx, _coins_ctx) = passive_platform();

    let answer = block_on(crate::rpc_command::get_enabled_coins::get_enabled_coins_rpc(ctx, None)).unwrap();

    let mut listed: Vec<String> = json::to_value(&answer).unwrap()["coins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|coin| coin["ticker"].as_str().unwrap().to_owned())
        .collect();
    listed.sort();
    assert_eq!(listed, vec!["PLAT".to_owned(), "T1".to_owned(), "T2".to_owned()]);
}

#[test]
fn the_registration_checks_count_a_passive_platform_as_registered() {
    let (ctx, coins_ctx) = passive_platform();

    assert!(block_on(coins_ctx.add_coin(test_coin("PLAT"))).is_err());
    assert_eq!(
        block_on(coins_ctx.add_token("OTHER", test_coin("PLAT")))
            .unwrap_err()
            .get_inner()
            .ticker,
        "PLAT"
    );
    let refused = block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("NEW")])).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "PLAT");
    assert!(
        block_on(lp_coinfind_registered(&ctx, "NEW")).unwrap().is_none(),
        "nothing was registered"
    );
    let refused = block_on(lp_register_coin(&ctx, test_coin("PLAT"), RegisterCoinParams {
        ticker: "PLAT".to_owned(),
        tx_history: false,
    }))
    .unwrap_err();
    assert!(matches!(
        refused.get_inner(),
        RegisterCoinError::CoinIsInitializedAlready { .. }
    ));
    // The legacy `enable` and `electrum` end here before they read the coin's configuration.
    let refused = block_on(lp_coininit(&ctx, "PLAT", &json!({}))).unwrap_err();
    assert!(refused.contains("already initialized"));
    assert!(block_on(coins_ctx.is_passive("PLAT")), "the refusals change nothing");
}

#[test]
fn a_passive_platform_stays_passive_when_its_last_dependent_leaves() {
    let (ctx, coins_ctx) = passive_platform();

    block_on(disable_coin(&ctx, "T1")).unwrap();
    block_on(disable_coin(&ctx, "T2")).unwrap();

    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
    assert!(
        block_on(coins_ctx.is_passive("PLAT")),
        "an empty set does not drop the flag"
    );
    assert!(block_on(lp_coinfind_registered(&ctx, "PLAT")).unwrap().is_some());
    assert!(block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_none());
}

#[test]
fn removing_a_passive_platform_clears_its_flag() {
    let (ctx, coins_ctx) = passive_platform();

    block_on(disable_coin(&ctx, "PLAT")).unwrap();

    assert!(!block_on(coins_ctx.is_passive("PLAT")));
    assert!(block_on(lp_coinfind_registered(&ctx, "PLAT")).unwrap().is_none());
    // A platform of the same ticker registers again, and is active.
    block_on(coins_ctx.add_coin(test_coin("PLAT"))).unwrap();
    assert!(block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_some());
}

#[test]
fn a_forced_removal_takes_the_platform_and_every_dependent_in_one_step() {
    let (ctx, coins_ctx) = passive_platform();
    block_on(coins_ctx.add_coin(test_coin("LEGACY"))).unwrap();

    let removed = block_on(disable_platform_with_dependents(&ctx, "PLAT")).unwrap();

    assert_eq!(removed, vec!["T1".to_owned(), "T2".to_owned(), "PLAT".to_owned()]);
    for ticker in ["PLAT", "T1", "T2"] {
        assert!(block_on(lp_coinfind_registered(&ctx, ticker)).unwrap().is_none());
    }
    assert!(!block_on(coins_ctx.is_passive("PLAT")));
    assert!(block_on(coins_ctx.dependents_of("PLAT")).is_empty());
    assert!(
        block_on(lp_coinfind(&ctx, "LEGACY")).unwrap().is_some(),
        "a coin outside the set stays"
    );
    assert!(
        block_on(disable_platform_with_dependents(&ctx, "PLAT")).is_err(),
        "disabled already"
    );
}

// Re-activation and the requested-token check (CRD ch.43 R43.1F.3 -- R43.1F.5, R43.1F.8, D43.1F.1)

fn names(tickers: &[&str]) -> Vec<String> { tickers.iter().map(|t| (*t).to_owned()).collect() }

#[test]
fn the_requested_token_check_refuses_only_a_registered_token_of_another_platform() {
    let (_ctx, coins_ctx) = passive_platform();
    block_on(coins_ctx.add_coin(test_coin("LEGACY"))).unwrap();
    let check = |platform: &str, tickers: &[&str], policy| {
        block_on(coins_ctx.check_requested_tokens(platform, &names(tickers), policy))
    };

    // A dependent of the very platform and an unregistered ticker pass.
    check("PLAT", &["T1", "NEW"], RegisteredTokenPolicy::Refuse).unwrap();
    // A registered ticker of another platform is refused, the first one named.
    let refused = check("PLAT", &["T1", "LEGACY", "T2"], RegisteredTokenPolicy::Refuse).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "LEGACY");
    // A dependent of another platform is no dependent of this one.
    let refused = check("OTHER", &["T1"], RegisteredTokenPolicy::Refuse).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "T1");
    // A registered token whose instance belongs to the platform rejoins it; one of another platform is refused.
    let bch = bch_coin_without_network_for_test();
    block_on(coins_ctx.add_coin(bch.clone().into())).unwrap();
    block_on(coins_ctx.add_coin(slp_token(&bch, "FUSD").into())).unwrap();
    check("BCH", &["FUSD"], RegisteredTokenPolicy::Refuse).unwrap();
    let refused = check("PLAT", &["FUSD"], RegisteredTokenPolicy::Refuse).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "FUSD");
    // The keep policy refuses nothing.
    check("PLAT", &["T1", "LEGACY"], RegisteredTokenPolicy::Keep).unwrap();
    check("OTHER", &["T1"], RegisteredTokenPolicy::Keep).unwrap();
    assert_eq!(RegisteredTokenPolicy::DEFAULT, RegisteredTokenPolicy::Refuse);
}

/// CRD ch.43 D43.1F.1: the `reactivation_registered_token` member is optional (absent or `null` is the default `"refuse"`), takes `"refuse"` or `"keep"`, and
/// any other value is an error that names the member and both values.
#[test]
fn the_registered_token_setting_is_read_from_the_configuration() {
    use serde_json::json;
    assert_eq!(
        RegisteredTokenPolicy::from_conf(&json!({})).unwrap(),
        RegisteredTokenPolicy::Refuse
    );
    assert_eq!(
        RegisteredTokenPolicy::from_conf(&json!({ "reactivation_registered_token": "refuse" })).unwrap(),
        RegisteredTokenPolicy::Refuse
    );
    assert_eq!(
        RegisteredTokenPolicy::from_conf(&json!({ "reactivation_registered_token": "keep" })).unwrap(),
        RegisteredTokenPolicy::Keep
    );
    assert_eq!(
        RegisteredTokenPolicy::from_conf(&json!({ "reactivation_registered_token": null })).unwrap(),
        RegisteredTokenPolicy::DEFAULT
    );
    for bad in [json!("Keep"), json!("none"), json!(true), json!(0)] {
        let err = RegisteredTokenPolicy::from_conf(&json!({ "reactivation_registered_token": bad })).unwrap_err();
        assert!(
            err.contains("reactivation_registered_token") && err.contains("refuse") && err.contains("keep"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn a_re_activation_makes_the_platform_active_and_registers_the_new_tokens_once() {
    let (ctx, coins_ctx) = passive_platform();

    // The same ticker twice, and a token that is registered already: kept, and in the set once.
    block_on(coins_ctx.reactivate_platform_with_tokens("PLAT", vec![
        test_coin("NEW"),
        test_coin("NEW"),
        test_coin("T1"),
    ]))
    .unwrap();

    assert!(!block_on(coins_ctx.is_passive("PLAT")));
    assert!(block_on(lp_coinfind(&ctx, "PLAT")).unwrap().is_some());
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["NEW", "T1", "T2"]));
    for ticker in ["NEW", "T1", "T2"] {
        assert!(block_on(lp_coinfind(&ctx, ticker)).unwrap().is_some(), "{}", ticker);
    }
}

#[test]
fn a_re_activation_of_an_entry_that_is_not_passive_changes_nothing() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![test_coin("T1")])).unwrap();

    let refused = block_on(coins_ctx.reactivate_platform_with_tokens("PLAT", vec![test_coin("NEW")])).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "PLAT");
    let refused = block_on(coins_ctx.reactivate_platform_with_tokens("ABSENT", vec![test_coin("NEW")])).unwrap_err();
    assert_eq!(refused.get_inner().ticker, "ABSENT");

    assert!(block_on(lp_coinfind_registered(&ctx, "NEW")).unwrap().is_none());
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["T1"]));
    assert!(block_on(lp_coinfind_registered(&ctx, "ABSENT")).unwrap().is_none());
}

#[test]
fn the_nft_chain_is_a_dependent_with_no_coin_entry() {
    let (ctx, coins_ctx) = ctx_and_coins();
    assert!(
        !block_on(coins_ctx.add_nft_dependent("PLAT", "NFT_PLAT")),
        "an unregistered platform has no NFT chain"
    );
    block_on(coins_ctx.add_coin(test_coin("PLAT"))).unwrap();
    assert!(!block_on(coins_ctx.make_passive("PLAT")), "no dependent yet");

    assert!(block_on(coins_ctx.add_nft_dependent("PLAT", "NFT_PLAT")));
    assert!(block_on(coins_ctx.add_nft_dependent("PLAT", "NFT_PLAT")), "idempotent");

    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["NFT_PLAT"]));
    assert!(block_on(lp_coinfind_registered(&ctx, "NFT_PLAT")).unwrap().is_none());
    assert!(
        block_on(coins_ctx.make_passive("PLAT")),
        "a platform whose only dependent is its NFT chain can be passive"
    );
    // A re-activation keeps the chain in the set.
    block_on(coins_ctx.reactivate_platform_with_tokens("PLAT", vec![])).unwrap();
    assert_eq!(block_on(coins_ctx.dependents_of("PLAT")), set(&["NFT_PLAT"]));
}
