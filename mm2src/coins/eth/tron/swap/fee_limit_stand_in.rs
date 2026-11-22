//! The `fee_limit` of a swap-contract call (CRD ch.21 D21.B5, §21.11C): a stand-in for the shared fee-limit
//! module of the TRON swap code, with the names and signatures of that module, so that a caller changes one
//! `use` line when the shared module replaces this file.
//!
//! # Invariants
//!
//! - A limit is never below the cost of the dry-run energy: a call whose limit is below its need fails on chain
//!   and is still charged the limit, so such a call is refused here and nothing is built for it.
//! - A limit is never above [`TRC20_FEE_LIMIT_MAX_SUN`].

use super::trigger::dry_run_energy;
use super::TronSwapError;
use crate::eth::tron::api::TronApiError;
use crate::eth::tron::fee::{TronChainPrices, TRC20_FEE_LIMIT_MAX_SUN};
use crate::eth::tron::TronAddress;
use crate::eth::EthCoin;

/// The margin the `fee_limit` of a swap call adds to the cost of its dry-run energy, in percent of that cost.
pub const SWAP_FEE_LIMIT_MARGIN_PERCENT: u64 = 50;

/// The `fee_limit` of a swap call whose dry run used `energy`: the cost of that energy at the energy price plus
/// [`SWAP_FEE_LIMIT_MARGIN_PERCENT`] of it, rounded up to whole SUN, and never above [`TRC20_FEE_LIMIT_MAX_SUN`].
///
/// # Errors
///
/// [`TronSwapError::FeeLimit`] when the cost of the energy alone is above the maximum, or does not fit 64 bits.
pub fn swap_fee_limit_sun(prices: &TronChainPrices, energy: u64) -> Result<u64, TronSwapError> {
    let estimate_sun = energy
        .checked_mul(prices.energy_price_sun)
        .ok_or_else(|| TronSwapError::FeeLimit("the cost of the energy exceeds the 64-bit range".to_owned()))?;
    if estimate_sun > TRC20_FEE_LIMIT_MAX_SUN {
        return Err(TronSwapError::FeeLimit(format!(
            "the estimated energy cost of {estimate_sun} SUN is above the maximum fee limit of \
             {TRC20_FEE_LIMIT_MAX_SUN} SUN"
        )));
    }
    // `estimate_sun` is at most the maximum here, so neither the product nor the sum can overflow.
    let margin_sun = (estimate_sun * SWAP_FEE_LIMIT_MARGIN_PERCENT).div_ceil(100);
    Ok((estimate_sun + margin_sun).min(TRC20_FEE_LIMIT_MAX_SUN))
}

/// The dry run of the call of `contract` with the call data `data` and the call value `call_value` (SUN), and
/// the `fee_limit` that follows from it: the energy the dry run used and [`swap_fee_limit_sun`] of it at the
/// chain's current energy price.
///
/// # Errors
///
/// The errors of [`dry_run_energy`] (a reverting dry run is [`TronSwapError::Reverted`]);
/// [`TronSwapError::Node`] when the chain's prices cannot be read; [`TronSwapError::FeeLimit`].
pub async fn swap_call_fee_limit(
    coin: &EthCoin,
    contract: &TronAddress,
    data: &[u8],
    call_value: i64,
) -> Result<(u64, u64), TronSwapError> {
    let energy = dry_run_energy(coin, contract, data, call_value).await?;
    let api = coin.tron_api.as_ref().ok_or(TronSwapError::NotTronCoin)?;
    let parameters = api.get_chain_parameters().await.map_err(TronSwapError::Node)?;
    let prices = TronChainPrices::from_chain_parameters(&parameters)
        .map_err(|e| TronSwapError::Node(TronApiError::InvalidResponse(e.to_string())))?;
    Ok((energy, swap_fee_limit_sun(&prices, energy)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices(energy_price_sun: u64) -> TronChainPrices {
        TronChainPrices {
            bandwidth_price_sun: 1000,
            energy_price_sun,
            account_creation_fee_sun: None,
            create_account_fee_sun: None,
            create_account_bandwidth_rate: 1,
        }
    }

    /// The margin is half of the energy cost, rounded up to whole SUN.
    #[test]
    fn the_limit_is_the_energy_cost_plus_half_of_it() {
        // 14 000 x 420 = 5 880 000; half of it is 2 940 000.
        assert_eq!(swap_fee_limit_sun(&prices(420), 14_000).unwrap(), 8_820_000);
        // 3 x 7 = 21; half is 10.5, rounded up to 11.
        assert_eq!(swap_fee_limit_sun(&prices(7), 3).unwrap(), 32);
    }

    /// A limit above the maximum is held at it, the cost alone at the maximum included.
    #[test]
    fn the_limit_is_held_at_the_maximum() {
        // 200 000 x 420 = 84 000 000; with the margin 126 000 000.
        assert_eq!(
            swap_fee_limit_sun(&prices(420), 200_000).unwrap(),
            TRC20_FEE_LIMIT_MAX_SUN
        );
        assert_eq!(
            swap_fee_limit_sun(&prices(1000), 100_000).unwrap(),
            TRC20_FEE_LIMIT_MAX_SUN
        );
    }

    /// An energy cost above the maximum is refused, and so is one beyond 64 bits.
    #[test]
    fn an_energy_cost_above_the_maximum_is_refused() {
        for energy in [100_001, u64::MAX] {
            let error = swap_fee_limit_sun(&prices(1000), energy).unwrap_err();
            assert!(matches!(error, TronSwapError::FeeLimit(_)), "{error}");
        }
    }
}
