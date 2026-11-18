//! Offline tests of the registry side of `disable_coin` for a Solana coin and an SPL token (CRD ch.40 T40.6B.20,
//! R40.6B.14; ch.43 §43.1E). The coins are built offline and registered as the activation does; removal is the
//! registry operation both `disable_coin` routes call. No test prints or compares key material in a message.

use super::*;
use crate::solana::solana_common_tests::{solana_coin_for_test, spl_coin_for_test, SolanaNet};
use crate::{get_enabled_coins, lp_coinfind, lp_register_coin, MarketCoinOps, MmCoinEnum, RegisterCoinParams, SplToken};
use common::block_on;
use serde_json::Value as Json;

const PASSPHRASE: &str = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
const USDC_MINT: &str = "CpMah17kQEL2wqyMKt3mZBdTnZbkbfx4nqmQMFDP5vwp";

fn sol_and_token() -> (MmArc, SolanaCoin, SplToken) {
    let (ctx, sol) = solana_coin_for_test(PASSPHRASE.to_string(), SolanaNet::Testnet);
    let token = spl_coin_for_test(sol.clone(), "USDC".to_string(), 6, Pubkey::from_str(USDC_MINT).unwrap());
    (ctx, sol, token)
}

fn register(ctx: &MmArc, coin: MmCoinEnum) -> Result<(), String> {
    let params = RegisterCoinParams {
        ticker: coin.ticker().to_owned(),
        tx_history: false,
    };
    block_on(lp_register_coin(ctx, coin, params)).map_err(|_| "registration refused".to_owned())
}

/// Registers a token as the activation does: in the registry and in its platform's record of tokens.
fn register_token(ctx: &MmArc, sol: &SolanaCoin, token: &SplToken) {
    register(ctx, token.clone().into()).unwrap();
    sol.add_spl_token_info(token.ticker().to_owned(), token.get_info());
}

fn is_active(ctx: &MmArc, ticker: &str) -> bool { block_on(lp_coinfind(ctx, ticker)).unwrap().is_some() }

fn enabled_tickers(ctx: &MmArc) -> Vec<String> {
    let answer = block_on(get_enabled_coins(ctx.clone())).unwrap();
    let body: Json = serde_json::from_slice(answer.body()).unwrap();
    let mut tickers: Vec<String> = body["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ticker"].as_str().unwrap().to_owned())
        .collect();
    tickers.sort();
    tickers
}

#[test]
fn t40_6b_20_disable_a_token_removes_it_from_the_registry_and_from_its_platform() {
    let (ctx, sol, token) = sol_and_token();
    register(&ctx, sol.clone().into()).unwrap();
    register_token(&ctx, &sol, &token);
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned(), "USDC".to_owned()]);
    assert!(sol.get_spl_tokens_infos().contains_key("USDC"));

    block_on(crate::disable_coin(&ctx, "USDC")).expect("a token is disabled");

    assert!(!is_active(&ctx, "USDC"), "the token left the registry");
    assert!(is_active(&ctx, "SOL"), "the platform stays");
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned()]);
    assert!(
        sol.get_spl_tokens_infos().is_empty(),
        "the platform no longer records the token"
    );
}

#[test]
fn t40_6b_20_a_disabled_token_can_be_activated_again() {
    let (ctx, sol, token) = sol_and_token();
    register(&ctx, sol.clone().into()).unwrap();
    register_token(&ctx, &sol, &token);
    block_on(crate::disable_coin(&ctx, "USDC")).unwrap();

    register_token(&ctx, &sol, &token);

    assert!(is_active(&ctx, "USDC"), "the token is active again");
    assert_eq!(sol.get_spl_tokens_infos().len(), 1);
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned(), "USDC".to_owned()]);
}

#[test]
fn t40_6b_20_disable_a_token_keeps_the_other_tokens_of_its_platform() {
    let (ctx, sol, usdc) = sol_and_token();
    let other = spl_coin_for_test(
        sol.clone(),
        "OTHER".to_string(),
        9,
        Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap(),
    );
    register(&ctx, sol.clone().into()).unwrap();
    register_token(&ctx, &sol, &usdc);
    register_token(&ctx, &sol, &other);

    block_on(crate::disable_coin(&ctx, "USDC")).unwrap();

    assert!(is_active(&ctx, "OTHER"));
    let infos = sol.get_spl_tokens_infos();
    assert!(infos.contains_key("OTHER") && !infos.contains_key("USDC"));
}

#[test]
fn t40_6b_20_disable_a_platform_without_tokens_removes_it_and_it_can_be_activated_again() {
    let (ctx, sol, _) = sol_and_token();
    register(&ctx, sol.clone().into()).unwrap();

    block_on(crate::disable_coin(&ctx, "SOL")).expect("a platform without tokens is disabled");

    assert!(!is_active(&ctx, "SOL"));
    assert!(enabled_tickers(&ctx).is_empty());
    register(&ctx, sol.into()).expect("the platform is activated again");
    assert_eq!(enabled_tickers(&ctx), vec!["SOL".to_owned()]);
}

/// What the other families do today (ch.43 §43.1E "Compatibility" item 2): the platform is removed and its tokens stay
/// registered; the answer is a success. The passive state of ch.55 D55.6 is not built.
#[test]
fn t40_6b_20_disable_a_platform_with_a_token_removes_the_platform_as_other_families_do() {
    let (ctx, sol, token) = sol_and_token();
    register(&ctx, sol.clone().into()).unwrap();
    register_token(&ctx, &sol, &token);

    block_on(crate::disable_coin(&ctx, "SOL")).expect("the platform is disabled");

    assert!(!is_active(&ctx, "SOL"));
    assert!(is_active(&ctx, "USDC"), "the token stays registered");
    register(&ctx, sol.clone().into()).expect("the platform is activated again");
    // The token can still be disabled afterwards.
    block_on(crate::disable_coin(&ctx, "USDC")).expect("the remaining token is disabled");
    assert!(!is_active(&ctx, "USDC"));
    assert!(is_active(&ctx, "SOL"));
}

#[test]
fn t40_6b_20_disable_a_coin_that_is_not_active_is_an_error_and_changes_nothing() {
    let (ctx, sol, _) = sol_and_token();
    register(&ctx, sol.into()).unwrap();

    assert!(block_on(crate::disable_coin(&ctx, "USDC")).is_err());
    assert!(block_on(crate::disable_coin(&ctx, "SOL")).is_ok());
    assert!(block_on(crate::disable_coin(&ctx, "SOL")).is_err(), "disabled already");
}

/// A token registered through `add_token` joins the platform's dependent set, and `disable_coin` takes it out of the
/// set and of the platform's records (CRD ch.43 R43.1E.5 items 1-2).
#[test]
fn t40_6b_20_a_token_joins_the_dependent_set_and_leaves_it_on_disable() {
    let (ctx, sol, token) = sol_and_token();
    register(&ctx, sol.clone().into()).unwrap();
    let coins_ctx = crate::CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_token("SOL", token.clone().into())).unwrap();
    sol.add_spl_token_info(token.ticker().to_owned(), token.get_info());
    assert_eq!(
        block_on(coins_ctx.dependents_of("SOL")).into_iter().collect::<Vec<_>>(),
        vec!["USDC".to_owned()]
    );

    block_on(crate::disable_coin(&ctx, "USDC")).unwrap();

    assert!(block_on(coins_ctx.dependents_of("SOL")).is_empty());
    assert!(sol.get_spl_tokens_infos().is_empty());
}
