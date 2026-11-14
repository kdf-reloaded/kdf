//! Tests of the Tendermint priority gas prices (CRD ch.36 §36.3.1A, ch.18 §18.6B, ch.49 §49.2A and ch.35 §35.6.1:
//! T36.16, T36.17, T18.6B.1, T18.6B.2, T49.28 -- T49.30, T35.38).
//!
//! The coin talks to the scripted node of the swap-flow tests ([`script_fee_estimation`]), which simulates every
//! transaction with a fixed gas use. At 61,234 gas units, the 6 decimals of the test chain and
//! `priority_gas_prices: [0.015, 0.025, 0.04]` with `gas_price` 0.03 the fee is `ceil(61,234 * 1.5 * price)` base
//! units: 3,675 at the high price, 2,297 at the average, 1,378 at the low and 2,756 at the default price.

use super::super::ibc::IBC_GAS_LIMIT_DEFAULT;
use super::super::tendermint_types::{FeeIntent, GAS_LIMIT_DEFAULT, STAKING_GAS_LIMIT_DEFAULT,
                                     UNDELEGATE_GAS_LIMIT_DEFAULT};
use super::*;
use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
use crate::eth::SwapGasFeePolicy;
use crate::rpc_command::swap_gas_fee_policy::{get_swap_gas_fee_policy, set_swap_gas_fee_policy,
                                              GetSwapGasFeePolicyRequest, SetSwapGasFeePolicyRequest,
                                              SwapGasFeePolicyError};
use crate::rpc_command::tendermint::staking::DelegationPayload;
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use crate::{CoinsContext, CosmosGasPriceOption, TransactionDetails, TxFeeDetails, WithdrawFee, WithdrawRequest};

const GAS_USED: u64 = 61_234;
const PRIORITY: [f64; 3] = [0.015, 0.025, 0.04];
/// The fee in base units at each price, for `GAS_USED`.
const FEE_HIGH: u64 = 3_675;
const FEE_AVERAGE: u64 = 2_297;
const FEE_LOW: u64 = 1_378;
const FEE_DEFAULT: u64 = 2_756;
const FEE_EXPLICIT_005: u64 = 4_593;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Builds the IRIS test coin with the given members added to its `protocol_data`.
fn try_build(ctx: &MmArc, node: &FakeNode, protocol_data: Json) -> Result<TendermintCoin, String> {
    let mut conf = Chain::Iris.conf();
    for (key, value) in protocol_data.as_object().expect("an object") {
        conf["protocol"]["protocol_data"][key.as_str()] = value.clone();
    }
    common::block_on(tendermint_coin_from_conf_and_request(
        ctx,
        "IRIS".to_owned(),
        &conf,
        vec![RpcNode::for_test(&node.url())],
        &OWN_KEY,
        false,
    ))
    .map_err(|e| e.to_string())
}

fn fixture_with(protocol_data: Json) -> Fixture {
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }))
        .into_mm_arc();
    let coin = try_build(&ctx, &node, protocol_data).unwrap_or_else(|e| panic!("cannot build the test coin: {e}"));
    Fixture { coin, node, ctx }
}

/// A coin with the array of priority prices and the default price 0.03, on a node that simulates `GAS_USED`.
fn priced() -> Fixture {
    let fx = fixture_with(json!({ "gas_price": 0.03, "priority_gas_prices": PRIORITY }));
    script_fee_estimation(&fx, GAS_USED);
    fx
}

/// A coin without the array, with the default price 0.03.
fn unpriced() -> Fixture {
    let fx = fixture_with(json!({ "gas_price": 0.03 }));
    script_fee_estimation(&fx, GAS_USED);
    fx
}

fn priority(option: &str, gas_limit: u64) -> Json {
    json!({ "type": "CosmosGasPriority", "gas_limit": gas_limit, "gas_price_option": option })
}

fn explicit(gas_price: f64, gas_limit: u64) -> Json {
    json!({ "type": "CosmosGas", "gas_limit": gas_limit, "gas_price": gas_price })
}

/// Withdraws 1 coin of `ticker` to a fixed recipient with the fee descriptor `fee`.
fn withdraw_with(coin: &impl MmCoin, ticker: &str, fee: Option<Json>) -> Result<TransactionDetails, String> {
    let to = account(Chain::Iris, &pubkey(&PEER_KEY)).to_string();
    let mut request = json!({ "coin": ticker, "to": to, "amount": "1" });
    if let Some(fee) = fee {
        request["fee"] = fee;
    }
    let request: WithdrawRequest = serde_json::from_value(request).map_err(|e| e.to_string())?;
    common::block_on(coin.withdraw(request).compat()).map_err(|e| e.to_string())
}

/// The fee as reported by `fee_details` and as signed into the transaction: `(reported amount, reported gas limit,
/// signed amount, signed denom, signed gas limit)`.
fn paid(details: &TransactionDetails) -> (BigDecimal, u64, String, String, u64) {
    let (amount, gas_limit) = match &details.fee_details {
        Some(TxFeeDetails::Tendermint(d)) => (d.amount.clone(), d.gas_limit),
        other => panic!("not a Tendermint fee: {other:?}"),
    };
    let tx = Tx::decode(details.tx_hex.as_slice()).expect("a signed transaction");
    let fee = tx.auth_info.expect("auth info").fee.expect("a fee");
    let coin = fee.amount.first().expect("a fee coin");
    (
        amount,
        gas_limit,
        coin.amount.clone(),
        coin.denom.clone(),
        fee.gas_limit,
    )
}

/// Asserts that the withdraw paid `sat` base units of the platform denom with the gas limit `gas_limit`, both in
/// `fee_details` and in the signed transaction.
fn assert_paid(details: &TransactionDetails, sat: u64, gas_limit: u64, what: &str) {
    let (amount, reported_limit, signed_amount, denom, signed_limit) = paid(details);
    assert_eq!(
        amount,
        big_decimal_from_sat_unsigned(sat, 6),
        "{what}: fee_details.amount"
    );
    assert_eq!(signed_amount, sat.to_string(), "{what}: the signed fee amount");
    assert_eq!(denom, "uiris", "{what}: the signed fee denom");
    assert_eq!(reported_limit, gas_limit, "{what}: fee_details.gas_limit");
    assert_eq!(signed_limit, gas_limit, "{what}: the signed gas limit");
}

/// `(fee amount in base units, gas limit)` that the fee computation of a request or swap transaction yields, for
/// any message (the node simulates every one the same).
fn fee_of(fx: &Fixture, intent: FeeIntent) -> (u128, u64) {
    let msg = msg_send_any(
        &account(Chain::Iris, &pubkey(&OWN_KEY)),
        &account(Chain::Iris, &pubkey(&PEER_KEY)),
        vec![coin_proto("uiris", 1)],
    );
    let fee = common::block_on(fx.coin.calculate_fee(msg, 200, "", &intent)).unwrap_or_else(|e| panic!("{e}"));
    (fee.amount[0].amount, fee.gas_limit)
}

fn token_of(fx: &Fixture) -> TendermintToken {
    TendermintToken::new(
        "IRIS-NYAN".to_owned(),
        fx.coin.clone(),
        6,
        "unyan".parse().expect("a denom"),
    )
}

/// The base units of `sat` in the platform coin's 6 decimals.
fn base_units(sat: u64) -> BigDecimal { big_decimal_from_sat_unsigned(sat, 6) }

/// The fee of the simulated dex-fee send and of the simulated create-HTLC of the coin, which are priced alike.
fn trade_fee(coin: &impl MmCoin) -> BigDecimal {
    let send = common::block_on(coin.get_fee_to_send_taker_fee(
        BigDecimal::from_str("0.3").expect("a decimal amount"),
        FeeApproxStage::OrderIssue,
    ))
    .unwrap_or_else(|e| panic!("the fee to send the dex fee: {e}"));
    let create = common::block_on(coin.get_sender_trade_fee(
        TradePreimageValue::Exact(BigDecimal::from_str("1.5").expect("a decimal amount")),
        FeeApproxStage::OrderIssue,
    ))
    .unwrap_or_else(|e| panic!("the sender trade fee: {e}"));
    assert_eq!(
        send.amount.to_decimal(),
        create.amount.to_decimal(),
        "both simulations are priced alike"
    );
    send.amount.to_decimal()
}

// ---------------------------------------------------------------------------
// T36.16, T36.17: the member and its refusal at activation (CRD ch.36 R36.3.5)
// ---------------------------------------------------------------------------

#[test]
fn activation_accepts_the_priority_gas_prices_shapes_and_any_order() {
    // T36.16: the array of three numbers, integers included, in any order (D36.4); `null` and absent are the same.
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for accepted in [
        json!({}),
        json!({ "priority_gas_prices": null }),
        json!({ "priority_gas_prices": [0.015, 0.025, 0.04] }),
        json!({ "priority_gas_prices": [0.04, 0.025, 0.015] }),
        json!({ "priority_gas_prices": [1, 2, 3] }),
        json!({ "priority_gas_prices": [0.001, 0.001, 0.001] }),
    ] {
        assert!(try_build(&ctx, &node, accepted.clone()).is_ok(), "{accepted}");
    }
}

#[test]
fn activation_refuses_a_non_positive_priority_gas_price() {
    // T36.17 (R36.3.5 divergence note): a figure of zero or less fails the activation and names the member and the
    // coin; nothing is built.
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for refused in [
        [0.0, 0.025, 0.04],
        [0.015, 0.0, 0.04],
        [0.015, 0.025, 0.0],
        [0.015, -0.025, 0.04],
        [-1.0, -1.0, -1.0],
    ] {
        let err = try_build(&ctx, &node, json!({ "priority_gas_prices": refused }))
            .expect_err(&format!("{refused:?} is accepted"));
        assert!(err.contains("priority_gas_prices"), "{err}");
        assert!(err.contains("IRIS"), "{err}");
    }
}

#[test]
fn activation_refuses_a_malformed_priority_gas_prices_value_too() {
    // The shapes that fail parsing of the protocol descriptor (T36.16) never build a coin either, should one
    // reach the coin builder.
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for refused in [
        json!([0.015, 0.025]),
        json!([0.015, 0.025, 0.04, 0.05]),
        json!([0.015, "0.025", 0.04]),
        json!({ "low": 0.015, "average": 0.025, "high": 0.04 }),
        json!("0.015"),
    ] {
        assert!(
            try_build(&ctx, &node, json!({ "priority_gas_prices": refused.clone() })).is_err(),
            "{refused}"
        );
    }
}

// ---------------------------------------------------------------------------
// T49.28, T49.29: the two fee forms on the request
// ---------------------------------------------------------------------------

#[test]
fn the_cosmos_fee_forms_parse_with_exact_spellings() {
    // R49.28, R49.29: both members of each form are required, the tags and the four level strings are
    // case-sensitive.
    let fee = |json: Json| serde_json::from_value::<WithdrawFee>(json);
    assert_eq!(fee(explicit(0.03, 125_000)).unwrap(), WithdrawFee::CosmosGas {
        gas_limit: 125_000,
        gas_price: 0.03
    });
    for (text, option) in [
        ("Low", CosmosGasPriceOption::Low),
        ("Average", CosmosGasPriceOption::Average),
        ("High", CosmosGasPriceOption::High),
        ("Default", CosmosGasPriceOption::Default),
    ] {
        assert_eq!(fee(priority(text, 125_000)).unwrap(), WithdrawFee::CosmosGasPriority {
            gas_limit: 125_000,
            gas_price_option: option
        });
    }
    for refused in [
        json!({ "type": "CosmosGas", "gas_limit": 125000 }),
        json!({ "type": "CosmosGas", "gas_price": 0.03 }),
        json!({ "type": "CosmosGasPriority", "gas_limit": 125000 }),
        json!({ "type": "CosmosGasPriority", "gas_price_option": "High" }),
        json!({ "type": "cosmosgas", "gas_limit": 125000, "gas_price": 0.03 }),
        priority("high", 125_000),
        priority("HIGH", 125_000),
        priority("Medium", 125_000),
        priority("", 125_000),
    ] {
        assert!(fee(refused.clone()).is_err(), "{refused}");
    }
    // The same descriptor is accepted and refused alike on a withdraw request and on a staking request.
    let request = |fee: Json| {
        serde_json::from_value::<WithdrawRequest>(json!({ "coin": "IRIS", "to": "x", "amount": "1", "fee": fee }))
    };
    let staking =
        |fee: Json| serde_json::from_value::<DelegationPayload>(json!({ "validator_address": "x", "fee": fee }));
    for accepted in [explicit(0.05, 150_000), priority("Low", 150_000)] {
        assert!(request(accepted.clone()).is_ok(), "{accepted}");
        assert!(staking(accepted.clone()).is_ok(), "{accepted}");
    }
    for refused in [
        priority("high", 150_000),
        priority("Medium", 150_000),
        json!({ "type": "CosmosGas" }),
    ] {
        assert!(request(refused.clone()).is_err(), "{refused}");
        assert!(staking(refused.clone()).is_err(), "{refused}");
    }
}

// ---------------------------------------------------------------------------
// T18.6B.1: the selected price in the amount
// ---------------------------------------------------------------------------

#[test]
fn a_withdraw_is_priced_with_the_selected_gas_price() {
    // T18.6B.1, T49.28, T49.29 on the direct method; the task route reaches the same `withdraw`.
    let fx = priced();
    let coin = &fx.coin;
    for (fee, sat, what) in [
        (priority("High", 125_000), FEE_HIGH, "level High"),
        (priority("Average", 125_000), FEE_AVERAGE, "level Average"),
        (priority("Low", 125_000), FEE_LOW, "level Low"),
        (priority("Default", 125_000), FEE_DEFAULT, "level Default"),
        (explicit(0.05, 125_000), FEE_EXPLICIT_005, "the explicit price 0.05"),
    ] {
        let details = withdraw_with(coin, "IRIS", Some(fee)).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_paid(&details, sat, 125_000, what);
    }
    // Without a descriptor: the default price and the default gas limit of a bank send.
    let details = withdraw_with(coin, "IRIS", None).expect("a withdraw");
    assert_paid(&details, FEE_DEFAULT, GAS_LIMIT_DEFAULT, "no descriptor");

    // The figures as the decimal amounts of T18.6B.1.
    for (option, text) in [("High", "0.003675"), ("Low", "0.001378"), ("Default", "0.002756")] {
        let details = withdraw_with(coin, "IRIS", Some(priority(option, 125_000))).expect("a withdraw");
        assert_eq!(paid(&details).0, BigDecimal::from_str(text).unwrap(), "{option}");
    }
}

#[test]
fn every_level_selects_the_default_price_without_the_array() {
    // T18.6B.1: a coin without `priority_gas_prices` honors no level and does not fail.
    let fx = unpriced();
    for option in ["Low", "Average", "High", "Default"] {
        let details = withdraw_with(&fx.coin, "IRIS", Some(priority(option, 125_000)))
            .unwrap_or_else(|e| panic!("{option}: {e}"));
        assert_paid(&details, FEE_DEFAULT, 125_000, option);
    }
    // The explicit form still wins over the default.
    let details = withdraw_with(&fx.coin, "IRIS", Some(explicit(0.05, 125_000))).expect("a withdraw");
    assert_paid(&details, FEE_EXPLICIT_005, 125_000, "explicit");
}

#[test]
fn the_swap_policy_prices_swap_simulations_and_never_a_withdraw() {
    // T18.6B.1 (last sentence), R18.6B.4: policy High prices the dex-fee send and the create-HTLC simulations at the
    // high price; a withdraw (with or without a level) is not affected.
    let fx = priced();
    let coin = &fx.coin;
    assert_eq!(trade_fee(coin), base_units(FEE_DEFAULT), "policy Legacy");
    for (policy, fee_sat) in [
        (SwapGasFeePolicy::High, FEE_HIGH),
        (SwapGasFeePolicy::Medium, FEE_AVERAGE),
        (SwapGasFeePolicy::Low, FEE_LOW),
        (SwapGasFeePolicy::Legacy, FEE_DEFAULT),
    ] {
        coin.set_swap_gas_fee_policy(policy);
        assert_eq!(coin.swap_gas_fee_policy(), policy);
        assert_eq!(
            trade_fee(coin),
            base_units(fee_sat),
            "swap simulations under {policy:?}"
        );
        assert_eq!(
            fee_of(&fx, FeeIntent::Swap),
            (fee_sat as u128, GAS_LIMIT_DEFAULT),
            "{policy:?}"
        );
        // A withdraw is priced as before whatever the policy.
        let plain = withdraw_with(coin, "IRIS", None).expect("a withdraw");
        assert_paid(
            &plain,
            FEE_DEFAULT,
            GAS_LIMIT_DEFAULT,
            "a withdraw without a descriptor",
        );
        let low = withdraw_with(coin, "IRIS", Some(priority("Low", 125_000))).expect("a withdraw");
        assert_paid(&low, FEE_LOW, 125_000, "a withdraw at level Low");
    }
}

#[test]
fn a_token_is_priced_with_its_platform_coin_in_the_platform_denom() {
    // R36.3.6, R18.6B.1 last sentence, T49.28: the token's withdraw selects from the platform's array and pays in
    // the platform denom; its swap simulations follow the platform's policy.
    let fx = priced();
    let token = token_of(&fx);
    let details = withdraw_with(&token, "IRIS-NYAN", Some(priority("High", 125_000))).expect("a token withdraw");
    assert_paid(&details, FEE_HIGH, 125_000, "the token at level High");
    assert_eq!(details.coin, "IRIS-NYAN");
    let details = withdraw_with(&token, "IRIS-NYAN", Some(explicit(0.05, 150_000))).expect("a token withdraw");
    assert_paid(&details, FEE_EXPLICIT_005, 150_000, "the token with the explicit price");
    let details = withdraw_with(&token, "IRIS-NYAN", None).expect("a token withdraw");
    assert_paid(
        &details,
        FEE_DEFAULT,
        GAS_LIMIT_DEFAULT,
        "the token without a descriptor",
    );

    fx.coin.set_swap_gas_fee_policy(SwapGasFeePolicy::High);
    assert_eq!(trade_fee(&token), base_units(FEE_HIGH));
}

// ---------------------------------------------------------------------------
// T18.6B.2: the gas limit
// ---------------------------------------------------------------------------

#[test]
fn the_gas_limit_is_independent_of_the_price() {
    let fx = priced();
    // The descriptor's limit is signed; the amount is the same at 125,000 and 150,000 (R18.6B.3, R18.6B.2).
    for gas_limit in [125_000, 150_000, 300_000] {
        let details = withdraw_with(&fx.coin, "IRIS", Some(priority("High", gas_limit))).expect("a withdraw");
        assert_paid(&details, FEE_HIGH, gas_limit, "level High");
        let details = withdraw_with(&fx.coin, "IRIS", Some(explicit(0.05, gas_limit))).expect("a withdraw");
        assert_paid(&details, FEE_EXPLICIT_005, gas_limit, "the explicit price");
    }
}

#[test]
fn the_operations_default_gas_limits() {
    // R18.6B.3: 125,000 for a bank send, 187,500 for a delegation and a reward claim, 250,000 for an undelegation,
    // 150,000 for an IBC transfer. The staking and IBC figures are the defaults the callers pass in.
    assert_eq!(GAS_LIMIT_DEFAULT, 125_000);
    assert_eq!(STAKING_GAS_LIMIT_DEFAULT, 187_500);
    assert_eq!(UNDELEGATE_GAS_LIMIT_DEFAULT, 250_000);
    assert_eq!(IBC_GAS_LIMIT_DEFAULT, 150_000);

    let fx = priced();
    for (default, label) in [
        (GAS_LIMIT_DEFAULT, "a bank send"),
        (STAKING_GAS_LIMIT_DEFAULT, "a delegation or a reward claim"),
        (UNDELEGATE_GAS_LIMIT_DEFAULT, "an undelegation"),
        (IBC_GAS_LIMIT_DEFAULT, "an IBC transfer"),
    ] {
        // No descriptor: the operation's default limit and the default price.
        assert_eq!(
            fee_of(&fx, FeeIntent::request(None, default)),
            (FEE_DEFAULT as u128, default),
            "{label}"
        );
        // A descriptor's limit replaces the default; its level selects the price.
        let fee: WithdrawFee = serde_json::from_value(priority("High", 300_000)).unwrap();
        assert_eq!(
            fee_of(&fx, FeeIntent::request(Some(fee), default)),
            (FEE_HIGH as u128, 300_000),
            "{label}"
        );
        let fee: WithdrawFee = serde_json::from_value(explicit(0.05, 300_000)).unwrap();
        assert_eq!(
            fee_of(&fx, FeeIntent::request(Some(fee), default)),
            (FEE_EXPLICIT_005 as u128, 300_000),
            "{label}"
        );
        // The swap policy never prices a request.
        fx.coin.set_swap_gas_fee_policy(SwapGasFeePolicy::High);
        assert_eq!(
            fee_of(&fx, FeeIntent::request(None, default)),
            (FEE_DEFAULT as u128, default),
            "{label}"
        );
        fx.coin.set_swap_gas_fee_policy(SwapGasFeePolicy::Legacy);
    }
}

// ---------------------------------------------------------------------------
// T49.30: a foreign fee form is ignored
// ---------------------------------------------------------------------------

#[test]
fn a_foreign_fee_form_is_ignored_on_every_cosmos_request() {
    let fx = priced();
    let foreign = [
        json!({ "type": "EthGas", "gas_price": 1, "gas": 21000 }),
        json!({ "type": "UtxoFixed", "amount": "0.1" }),
        json!({ "type": "UtxoPerKbyte", "amount": "0.1" }),
        json!({ "type": "Qrc20Gas", "gas_limit": 2_500_000, "gas_price": 40 }),
    ];
    let token = token_of(&fx);
    for descriptor in foreign {
        // Withdraw: the default price and the bank-send default limit, as without a descriptor.
        let details =
            withdraw_with(&fx.coin, "IRIS", Some(descriptor.clone())).unwrap_or_else(|e| panic!("{descriptor}: {e}"));
        assert_paid(&details, FEE_DEFAULT, GAS_LIMIT_DEFAULT, "a foreign form on a withdraw");
        let details = withdraw_with(&token, "IRIS-NYAN", Some(descriptor.clone()))
            .unwrap_or_else(|e| panic!("{descriptor}: {e}"));
        assert_paid(
            &details,
            FEE_DEFAULT,
            GAS_LIMIT_DEFAULT,
            "a foreign form on a token withdraw",
        );
        // Staking: the delegation's default limit.
        let fee: WithdrawFee = serde_json::from_value(descriptor.clone()).unwrap();
        assert_eq!(
            fee_of(&fx, FeeIntent::request(Some(fee), STAKING_GAS_LIMIT_DEFAULT)),
            (FEE_DEFAULT as u128, STAKING_GAS_LIMIT_DEFAULT),
            "{descriptor}"
        );
    }
}

// ---------------------------------------------------------------------------
// T35.38: the swap fee policy RPCs
// ---------------------------------------------------------------------------

fn get_policy(ctx: &MmArc, coin: &str) -> Result<SwapGasFeePolicy, SwapGasFeePolicyError> {
    let request = GetSwapGasFeePolicyRequest { coin: coin.to_owned() };
    common::block_on(get_swap_gas_fee_policy(ctx.clone(), request)).map_err(|e| e.into_inner())
}

fn set_policy(ctx: &MmArc, coin: &str, policy: Json) -> Result<SwapGasFeePolicy, SwapGasFeePolicyError> {
    let request: SetSwapGasFeePolicyRequest =
        serde_json::from_value(json!({ "coin": coin, "swap_gas_fee_policy": policy })).expect("a request");
    common::block_on(set_swap_gas_fee_policy(ctx.clone(), request)).map_err(|e| e.into_inner())
}

fn register(fx: &Fixture) {
    let coins_ctx = CoinsContext::from_ctx(&fx.ctx).expect("the coins context");
    common::block_on(coins_ctx.add_coin(fx.coin.clone().into())).expect("register the platform");
    common::block_on(coins_ctx.add_coin(token_of(fx).into())).expect("register the token");
}

#[test]
fn the_tendermint_swap_fee_policy_round_trip() {
    // T35.38: the policy starts at `Legacy`, `set` echoes and `get` returns it, swap simulations follow it and a
    // withdraw does not; a token ticker is `NotSupported`; an unknown coin is `NoSuchCoin`.
    let fx = priced();
    register(&fx);
    assert_eq!(get_policy(&fx.ctx, "IRIS").unwrap(), SwapGasFeePolicy::Legacy);
    assert_eq!(
        set_policy(&fx.ctx, "IRIS", json!("High")).unwrap(),
        SwapGasFeePolicy::High
    );
    assert_eq!(get_policy(&fx.ctx, "IRIS").unwrap(), SwapGasFeePolicy::High);
    assert_eq!(fx.coin.swap_gas_fee_policy(), SwapGasFeePolicy::High);

    assert_eq!(
        trade_fee(&fx.coin),
        base_units(FEE_HIGH),
        "the simulated dex-fee send and create-HTLC"
    );
    let details = withdraw_with(&fx.coin, "IRIS", None).expect("a withdraw");
    assert_paid(&details, FEE_DEFAULT, GAS_LIMIT_DEFAULT, "a withdraw in the same state");

    for policy in ["Low", "Medium", "Legacy"] {
        let echoed = set_policy(&fx.ctx, "IRIS", json!(policy)).unwrap();
        assert_eq!(serde_json::to_value(echoed).unwrap(), json!(policy));
    }

    // A token has no policy of its own.
    assert!(matches!(
        get_policy(&fx.ctx, "IRIS-NYAN"),
        Err(SwapGasFeePolicyError::NotSupported(t)) if t == "IRIS-NYAN"
    ));
    assert!(matches!(
        set_policy(&fx.ctx, "IRIS-NYAN", json!("High")),
        Err(SwapGasFeePolicyError::NotSupported(t)) if t == "IRIS-NYAN"
    ));
    assert!(matches!(
        get_policy(&fx.ctx, "NOSUCH"),
        Err(SwapGasFeePolicyError::NoSuchCoin(_))
    ));
    // A value outside the four fails parsing of the request.
    assert!(serde_json::from_value::<SetSwapGasFeePolicyRequest>(
        json!({ "coin": "IRIS", "swap_gas_fee_policy": "Internal" })
    )
    .is_err());
}

#[test]
fn a_policy_without_the_array_is_stored_with_one_warning_and_priced_at_the_default() {
    // T35.38, R35.6.9 item 2: `Medium` on a coin without `priority_gas_prices` is accepted, stored and returned;
    // one warning is logged; swap simulations are priced at the default price. `Legacy` is a value like any other
    // here and logs its own warning. Only this test sets a policy on a coin without the array, so the count is its own.
    let capturing = install_log_capture();
    let warning = "has no `priority_gas_prices`";
    let count = || {
        captured_log_lines()
            .iter()
            .filter(|line| line.contains(warning))
            .count()
    };

    let fx = unpriced();
    register(&fx);
    let before = count();
    assert_eq!(
        set_policy(&fx.ctx, "IRIS", json!("Legacy")).unwrap(),
        SwapGasFeePolicy::Legacy
    );
    if capturing {
        assert_eq!(count(), before + 1, "Legacy logs exactly one warning");
    }
    assert_eq!(
        set_policy(&fx.ctx, "IRIS", json!("Medium")).unwrap(),
        SwapGasFeePolicy::Medium
    );
    assert_eq!(get_policy(&fx.ctx, "IRIS").unwrap(), SwapGasFeePolicy::Medium);
    if capturing {
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|l| l.contains(warning))
            .collect();
        assert_eq!(lines.len(), before + 2, "one warning per set, two sets");
        assert!(lines[before].starts_with("WARN"), "{}", lines[before]);
        assert!(lines[before + 1].starts_with("WARN"), "{}", lines[before + 1]);
    }
    assert_eq!(
        trade_fee(&fx.coin),
        base_units(FEE_DEFAULT),
        "a simulated dex-fee send is priced at the default price"
    );
}
