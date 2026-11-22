//! The `fee_limit` of a swap contract call (CRD ch.21 D21.B5, §21.11C R21.11C.7).

use super::trigger::{dry_run_energy, tron_api};
use super::TronSwapError;
use crate::eth::tron::fee::{fee_limit_sun_with_margin, TronChainPrices};
use crate::eth::tron::TronAddress;
use crate::eth::EthCoin;

/// The margin on the cost of the dry-run energy, in percent: the `fee_limit` of a swap contract call is that cost
/// times 1.5 (maintainer decision of 2026-11-08).
pub const SWAP_FEE_LIMIT_MARGIN_PERCENT: u64 = 50;

/// The `fee_limit` of a swap contract call whose dry run used `energy`: the cost of that energy at the energy price
/// plus [`SWAP_FEE_LIMIT_MARGIN_PERCENT`] of it, rounded up to whole SUN, and never above the maximum of the wallet
/// path ([`TRC20_FEE_LIMIT_MAX_SUN`](crate::eth::tron::fee::TRC20_FEE_LIMIT_MAX_SUN)).
///
/// # Errors
///
/// [`TronSwapError::FeeLimit`] with
/// [`EnergyCostAboveMaximum`](crate::eth::tron::fee::TronFeeError::EnergyCostAboveMaximum) when the cost of the
/// energy alone is above the maximum: a limit below the need would make the call fail on chain and still charge
/// the limit, so such a call is not built. [`TronSwapError::FeeLimit`] with `Overflow` for a cost beyond 64 bits.
pub fn swap_fee_limit_sun(prices: &TronChainPrices, energy: u64) -> Result<u64, TronSwapError> {
    fee_limit_sun_with_margin(prices, energy, SWAP_FEE_LIMIT_MARGIN_PERCENT).map_err(TronSwapError::FeeLimit)
}

/// The dry-run energy and the `fee_limit` (SUN) of the call of `contract` with the call data `data` and the call
/// value `call_value` (SUN): the dry run of [`dry_run_energy`], the chain's prices, then [`swap_fee_limit_sun`].
///
/// # Errors
///
/// The errors of [`dry_run_energy`]; [`TronSwapError::Node`] when no node gives chain parameters with a positive
/// energy price (the node client takes an answer without one for a bad answer); [`TronSwapError::FeeLimit`] as
/// [`swap_fee_limit_sun`], and for a price the estimate cannot use.
pub async fn swap_call_fee_limit(
    coin: &EthCoin,
    contract: &TronAddress,
    data: &[u8],
    call_value: i64,
) -> Result<(u64, u64), TronSwapError> {
    let energy = dry_run_energy(coin, contract, data, call_value).await?;
    let parameters = tron_api(coin)?
        .get_chain_parameters()
        .await
        .map_err(TronSwapError::Node)?;
    let prices = TronChainPrices::from_chain_parameters(&parameters).map_err(TronSwapError::FeeLimit)?;
    Ok((energy, swap_fee_limit_sun(&prices, energy)?))
}
