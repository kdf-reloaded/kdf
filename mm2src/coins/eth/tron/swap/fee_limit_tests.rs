//! Tests of the `fee_limit` of a swap contract call (CRD ch.21 D21.B5, §21.11C R21.11C.7).

use super::fee_limit::{swap_call_fee_limit, swap_fee_limit_sun, SWAP_FEE_LIMIT_MARGIN_PERCENT};
use super::ids_tests::addr;
use super::TronSwapError;
use crate::eth::tron::fee::{TronChainPrices, TronFeeError, TRC20_FEE_LIMIT_MAX_SUN};
use crate::eth::tron::test_node::{Reply, StubNode};
use crate::eth::tron::wallet_path_tests::trx_coin;
use common::block_on;
use serde_json::{json, Value as Json};

/// Chain prices with the energy price `energy_price_sun`.
fn prices(energy_price_sun: u64) -> TronChainPrices {
    TronChainPrices {
        bandwidth_price_sun: 1000,
        energy_price_sun,
        account_creation_fee_sun: None,
        create_account_fee_sun: None,
        create_account_bandwidth_rate: 1,
    }
}

/// The cost of the dry-run energy times 1.5, rounded up to whole SUN.
#[test]
fn the_fee_limit_is_the_energy_cost_plus_half_of_it_rounded_up() {
    assert_eq!(SWAP_FEE_LIMIT_MARGIN_PERCENT, 50);
    // 14 000 x 420 = 5 880 000; half of it is 2 940 000.
    assert_eq!(swap_fee_limit_sun(&prices(420), 14_000).unwrap(), 8_820_000);
    // 1 x 420 = 420; half is 210.
    assert_eq!(swap_fee_limit_sun(&prices(420), 1).unwrap(), 630);
    // Rounding up: 3 x 7 = 21, half is 10.5, rounded up to 11.
    assert_eq!(swap_fee_limit_sun(&prices(7), 3).unwrap(), 32);
    // No energy, no limit.
    assert_eq!(swap_fee_limit_sun(&prices(420), 0).unwrap(), 0);
}

/// The limit is held at the maximum of the wallet path, 100 TRX; an energy cost above it is refused, typed.
#[test]
fn the_fee_limit_is_capped_and_a_cost_above_the_cap_is_refused() {
    assert_eq!(TRC20_FEE_LIMIT_MAX_SUN, 100_000_000);
    // 200 000 x 420 = 84 000 000; times 1.5 is 126 000 000, held at the maximum.
    assert_eq!(swap_fee_limit_sun(&prices(420), 200_000).unwrap(), 100_000_000);
    // 66 666 667 x 1.5 = 100 000 000.5, rounded up and held at the maximum; one SUN less stays below it.
    assert_eq!(swap_fee_limit_sun(&prices(1), 66_666_667).unwrap(), 100_000_000);
    assert_eq!(swap_fee_limit_sun(&prices(1), 66_666_666).unwrap(), 99_999_999);
    // The cost alone at the maximum is still accepted.
    assert_eq!(swap_fee_limit_sun(&prices(1000), 100_000).unwrap(), 100_000_000);
    // One energy unit more is above the maximum.
    let refused = swap_fee_limit_sun(&prices(1000), 100_001);
    assert!(
        matches!(
            &refused,
            Err(TronSwapError::FeeLimit(TronFeeError::EnergyCostAboveMaximum {
                estimate_sun: 100_001_000,
                maximum_sun: 100_000_000,
            }))
        ),
        "{refused:?}"
    );
}

/// A cost beyond 64 bits is a typed error.
#[test]
fn an_energy_cost_beyond_64_bits_is_an_overflow() {
    for (price, energy) in [(420, u64::MAX), (u64::MAX, 2)] {
        assert!(matches!(
            swap_fee_limit_sun(&prices(price), energy),
            Err(TronSwapError::FeeLimit(TronFeeError::Overflow))
        ));
    }
}

/// A node whose dry run answers `dry_run` and whose chain parameters are `parameters` (HTTP 500 when `None`).
fn node(dry_run: Json, parameters: Option<Json>) -> StubNode {
    StubNode::start(move |path, _| match (path, &parameters) {
        ("/wallet/triggerconstantcontract", _) => Reply::ok(&dry_run.to_string()),
        ("/wallet/getchainparameters", Some(parameters)) => Reply::ok(&parameters.to_string()),
        ("/wallet/getchainparameters", None) => Reply::status(500, "stub failure"),
        (other, _) => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

fn successful_dry_run(energy: u64) -> Json {
    json!({
        "result": { "result": true },
        "energy_used": energy,
        "constant_result": [""],
        "transaction": { "ret": [{}] },
    })
}

fn parameters(energy_price_sun: u64) -> Json {
    json!({ "chainParameter": [
        { "key": "getTransactionFee", "value": 1000 },
        { "key": "getEnergyFee", "value": energy_price_sun },
    ] })
}

fn call_fee_limit(node: &StubNode) -> Result<(u64, u64), TronSwapError> {
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    block_on(swap_call_fee_limit(
        &coin,
        &addr(0x10),
        &[0xde, 0xad, 0xbe, 0xef],
        5_000_000,
    ))
}

/// The dry-run energy and the chain's energy price give the limit: 31 000 x 420 = 13 020 000, times 1.5.
#[test]
fn the_call_fee_limit_comes_from_the_dry_run_and_the_chain_prices() {
    let node = node(successful_dry_run(31_000), Some(parameters(420)));
    assert_eq!(call_fee_limit(&node).unwrap(), (31_000, 19_530_000));
    let dry_runs = node.bodies_for("/wallet/triggerconstantcontract");
    assert_eq!(dry_runs.len(), 1);
    assert_eq!(dry_runs[0]["data"], json!("deadbeef"));
    assert_eq!(dry_runs[0]["call_value"], json!(5_000_000));
    assert_eq!(node.bodies_for("/wallet/getchainparameters").len(), 1);
}

/// Nothing is assumed: a reverting dry run, unreadable or unusable chain parameters and a cost above the maximum
/// are each a typed error.
#[test]
fn the_call_fee_limit_fails_typed_without_a_usable_figure() {
    let reverting = node(
        json!({ "result": { "result": true, "message": "REVERT opcode executed" }, "energy_used": 500 }),
        Some(parameters(420)),
    );
    assert!(matches!(
        call_fee_limit(&reverting),
        Err(TronSwapError::Reverted { .. })
    ));
    assert!(reverting.bodies_for("/wallet/getchainparameters").is_empty());

    let unreadable = node(successful_dry_run(31_000), None);
    assert!(matches!(call_fee_limit(&unreadable), Err(TronSwapError::Node(_))));

    let no_energy_price = node(
        successful_dry_run(31_000),
        Some(json!({ "chainParameter": [{ "key": "getTransactionFee", "value": 1000 }] })),
    );
    // The node client takes an answer without a positive energy price for a bad answer of that node.
    assert!(matches!(call_fee_limit(&no_energy_price), Err(TronSwapError::Node(_))));

    // 300 000 x 420 = 126 000 000, above the maximum of 100 000 000.
    let costly = node(successful_dry_run(300_000), Some(parameters(420)));
    assert!(matches!(
        call_fee_limit(&costly),
        Err(TronSwapError::FeeLimit(TronFeeError::EnergyCostAboveMaximum { .. }))
    ));
}
