//! TRON fee estimation (CRD ch.21 §21.8, R21.10B.7 and D21.10B.1).
//!
//! TRON has a dual fee model:
//! - **Bandwidth**: consumed by every transaction, one unit per byte of its size. An account has a free daily quota
//!   and may hold a staked quota. The node draws a transaction's bandwidth as a whole from the staked quota, else
//!   as a whole from the free quota, else it burns `getTransactionFee` SUN for every byte. The two quotas are never
//!   summed.
//! - **Energy**: consumed only by smart-contract calls (a TRC20 transfer). Staked energy is used first and the
//!   shortfall is burned at `getEnergyFee` SUN per unit. The transaction's `fee_limit` bounds what the call may
//!   cost, staked and burned alike.
//!
//! Everything in this module is pure arithmetic on figures the caller read from the node. No figure is assumed: a
//! parameter the estimate needs and the node did not report is a [`TronFeeError`].
//!
//! # Invariants
//!
//! - The bandwidth part of an estimate is never below what the node charges for a transaction of the given size
//!   and the given resources.
//! - Every amount is in SUN and every multiplication and sum is checked.

use bigdecimal::BigDecimal;
use derive_more::Display;
use ethereum_types::U256;
use serde::{Deserialize, Serialize};

use super::api::GetChainParametersResponse;
use super::proto::{Transaction, TransactionRaw};
use super::TRX_DECIMALS;
use crate::eth::u256_to_big_decimal;
use crate::NumConversResult;

use prost::Message as _;

/// The length of the one signature of a transaction, `r || s || v`.
const SIGNATURE_LEN: usize = 65;

/// The bandwidth the node adds for each contract of a transaction, for the result it stores.
const BANDWIDTH_PER_CONTRACT: u64 = 64;

/// The most a TRC20 transfer's `fee_limit` may be, in SUN: 100 TRX (CRD ch.21 D21.10B.1).
pub const TRC20_FEE_LIMIT_MAX_SUN: u64 = 100_000_000;

/// The margin a TRC20 transfer's `fee_limit` adds to the cost of the dry-run energy, in percent of that cost
/// (CRD ch.21 D21.10B.1).
pub const TRC20_FEE_LIMIT_MARGIN_PERCENT: u64 = 20;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a fee could not be estimated.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum TronFeeError {
    /// The node's chain parameters carry no usable value of a parameter every estimate needs.
    #[display(fmt = "the node's chain parameters carry no usable {}", _0)]
    UnusableChainParameter(&'static str),
    /// The transfer creates the recipient's account and the node reports no positive account-creation fee
    /// (`getCreateNewAccountFeeInSystemContract`).
    #[display(fmt = "the node reports no positive getCreateNewAccountFeeInSystemContract")]
    NoAccountCreationFee,
    /// The transfer creates the recipient's account, the sender's staked bandwidth does not cover it and the node
    /// reports no `getCreateAccountFee`.
    #[display(fmt = "the node reports no getCreateAccountFee")]
    NoCreateAccountFee,
    /// The cost of the dry-run energy is above the most a `fee_limit` may be.
    #[display(
        fmt = "the estimated energy cost of {} SUN is above the maximum fee limit of {} SUN",
        estimate_sun,
        maximum_sun
    )]
    EnergyCostAboveMaximum { estimate_sun: u64, maximum_sun: u64 },
    /// A figure does not fit the 64-bit amounts of the protocol.
    #[display(fmt = "a fee figure exceeds the 64-bit range")]
    Overflow,
}

impl std::error::Error for TronFeeError {}

// ---------------------------------------------------------------------------
// Chain price parameters
// ---------------------------------------------------------------------------

/// The chain parameters a fee estimate reads (CRD ch.21 R21.10B.7), from `/wallet/getchainparameters`.
#[derive(Clone, Debug, PartialEq)]
pub struct TronChainPrices {
    /// SUN burned per bandwidth unit (`getTransactionFee`), positive.
    pub bandwidth_price_sun: u64,
    /// SUN burned per energy unit (`getEnergyFee`), positive.
    pub energy_price_sun: u64,
    /// The fee of a transfer that creates the recipient's account (`getCreateNewAccountFeeInSystemContract`), when
    /// the node reports a positive one.
    pub account_creation_fee_sun: Option<u64>,
    /// What replaces the bandwidth of an account-creating transfer that the sender's staked bandwidth does not
    /// cover (`getCreateAccountFee`), when the node reports one.
    pub create_account_fee_sun: Option<u64>,
    /// The factor on the bandwidth of an account-creating transfer (`getCreateNewAccountBandwidthRate`); 1 when
    /// the node does not report it.
    pub create_account_bandwidth_rate: u64,
}

impl TronChainPrices {
    /// Reads the prices from a `/wallet/getchainparameters` answer.
    ///
    /// # Errors
    ///
    /// [`TronFeeError::UnusableChainParameter`] when `getTransactionFee` or `getEnergyFee` is absent or not
    /// positive, or when `getCreateNewAccountBandwidthRate` is reported and not positive.
    pub fn from_chain_parameters(resp: &GetChainParametersResponse) -> Result<Self, TronFeeError> {
        // The node leaves `value` out of an entry whose value is zero, so an entry without it reads as no figure.
        let value_of = |key: &str| {
            resp.chain_parameter
                .iter()
                .find(|parameter| parameter.key == key)
                .and_then(|parameter| parameter.value)
        };
        let positive = |key: &'static str| {
            value_of(key)
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or(TronFeeError::UnusableChainParameter(key))
        };
        let create_account_bandwidth_rate = match value_of("getCreateNewAccountBandwidthRate") {
            Some(_) => positive("getCreateNewAccountBandwidthRate")?,
            // The default the protocol states for a chain that never set the rate.
            None => 1,
        };
        Ok(TronChainPrices {
            bandwidth_price_sun: positive("getTransactionFee")?,
            energy_price_sun: positive("getEnergyFee")?,
            account_creation_fee_sun: positive("getCreateNewAccountFeeInSystemContract").ok(),
            create_account_fee_sun: value_of("getCreateAccountFee").and_then(|value| u64::try_from(value).ok()),
            create_account_bandwidth_rate,
        })
    }
}

// ---------------------------------------------------------------------------
// Account resources
// ---------------------------------------------------------------------------

/// Account bandwidth and energy resource quotas (from `/wallet/getaccountresource`). The node leaves a member out
/// when it is zero, so an empty answer is all zero.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TronAccountResources {
    /// Free bandwidth used.
    #[serde(default, rename = "freeNetUsed")]
    pub free_net_used: i64,
    /// Free bandwidth limit (typically 600/day).
    #[serde(default, rename = "freeNetLimit")]
    pub free_net_limit: i64,
    /// Staked bandwidth used.
    #[serde(default, rename = "NetUsed")]
    pub net_used: i64,
    /// Staked bandwidth limit.
    #[serde(default, rename = "NetLimit")]
    pub net_limit: i64,
    /// Energy used.
    #[serde(default, rename = "EnergyUsed")]
    pub energy_used: i64,
    /// Energy limit.
    #[serde(default, rename = "EnergyLimit")]
    pub energy_limit: i64,
}

impl TronAccountResources {
    /// Remaining free bandwidth.
    pub fn free_bandwidth_remaining(&self) -> u64 { remaining(self.free_net_limit, self.free_net_used) }

    /// Remaining staked bandwidth.
    pub fn staked_bandwidth_remaining(&self) -> u64 { remaining(self.net_limit, self.net_used) }

    /// Remaining staked energy.
    pub fn energy_remaining(&self) -> u64 { remaining(self.energy_limit, self.energy_used) }
}

/// What is left of a quota: `limit` less `used`, and nothing when the use is at or above the limit.
fn remaining(limit: i64, used: i64) -> u64 { limit.saturating_sub(used).max(0).unsigned_abs() }

// ---------------------------------------------------------------------------
// Estimate
// ---------------------------------------------------------------------------

/// The estimated fee of one transfer, in SUN and resource units.
#[derive(Clone, Debug, PartialEq)]
pub struct TronFeeEstimate {
    /// Bandwidth the transaction consumes; 0 when a fixed fee replaces the bandwidth of an account-creating
    /// transfer.
    pub bandwidth_used: u64,
    /// SUN burned for bandwidth (0 when a quota covers the whole transaction).
    pub bandwidth_fee_sun: u64,
    /// Energy the dry run of the contract call used (0 for a TRX transfer).
    pub energy_used: u64,
    /// SUN burned for the energy the sender's staked energy does not cover.
    pub energy_fee_sun: u64,
    /// The fee of creating the recipient's account (0 when the account exists, and for every TRC20 transfer).
    pub account_creation_fee_sun: u64,
}

impl TronFeeEstimate {
    /// The whole fee in SUN.
    ///
    /// # Errors
    ///
    /// [`TronFeeError::Overflow`] when the sum does not fit.
    pub fn total_fee_sun(&self) -> Result<u64, TronFeeError> {
        self.bandwidth_fee_sun
            .checked_add(self.energy_fee_sun)
            .and_then(|sum| sum.checked_add(self.account_creation_fee_sun))
            .ok_or(TronFeeError::Overflow)
    }
}

/// The bandwidth a transaction of `raw` consumes: the serialized size of the transaction with its one signature,
/// plus the node's allowance for each contract's result (CRD ch.21 R21.10B.7).
pub fn transaction_bandwidth(raw: &TransactionRaw) -> u64 {
    let signed = Transaction {
        raw_data: Some(raw.clone()),
        signature: vec![vec![0u8; SIGNATURE_LEN]],
    };
    // `usize` to `u64` never truncates on a supported target.
    signed.encoded_len() as u64 + BANDWIDTH_PER_CONTRACT * raw.contract.len() as u64
}

/// The SUN burned for a transaction of `size` bytes whose recipient exists: nothing when the sender's staked
/// bandwidth covers the whole size, nothing when the free bandwidth covers the whole size, else the whole size at
/// the bandwidth price. The node never sums the two quotas, and neither does this.
fn bandwidth_fee_sun(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    size: u64,
) -> Result<u64, TronFeeError> {
    if size <= resources.staked_bandwidth_remaining() || size <= resources.free_bandwidth_remaining() {
        return Ok(0);
    }
    size.checked_mul(prices.bandwidth_price_sun)
        .ok_or(TronFeeError::Overflow)
}

/// The fee of a native TRX transfer whose transaction consumes `size` bandwidth (see [`transaction_bandwidth`]).
///
/// A transfer to an address the chain has not created (`recipient_exists` false) pays the account-creation fee.
/// Its bandwidth is `size` times the chain's rate and is free only when the sender's staked bandwidth covers it;
/// otherwise the chain's fixed `getCreateAccountFee` replaces it and no bandwidth is consumed.
///
/// # Errors
///
/// [`TronFeeError::NoAccountCreationFee`] and [`TronFeeError::NoCreateAccountFee`] when the transfer creates an
/// account and the node did not report the figure it needs; [`TronFeeError::Overflow`].
pub fn estimate_trx_transfer_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    size: u64,
    recipient_exists: bool,
) -> Result<TronFeeEstimate, TronFeeError> {
    if recipient_exists {
        return Ok(TronFeeEstimate {
            bandwidth_used: size,
            bandwidth_fee_sun: bandwidth_fee_sun(prices, resources, size)?,
            energy_used: 0,
            energy_fee_sun: 0,
            account_creation_fee_sun: 0,
        });
    }
    let account_creation_fee_sun = prices
        .account_creation_fee_sun
        .ok_or(TronFeeError::NoAccountCreationFee)?;
    let creation_bandwidth = size
        .checked_mul(prices.create_account_bandwidth_rate)
        .ok_or(TronFeeError::Overflow)?;
    let (bandwidth_used, bandwidth_fee_sun) = if creation_bandwidth <= resources.staked_bandwidth_remaining() {
        (creation_bandwidth, 0)
    } else {
        let fixed = prices.create_account_fee_sun.ok_or(TronFeeError::NoCreateAccountFee)?;
        (0, fixed)
    };
    Ok(TronFeeEstimate {
        bandwidth_used,
        bandwidth_fee_sun,
        energy_used: 0,
        energy_fee_sun: 0,
        account_creation_fee_sun,
    })
}

/// The fee of a TRC20 transfer whose transaction consumes `size` bandwidth and whose dry run used `energy`.
///
/// The energy fee is the shortfall of the sender's staked energy at the energy price. The whole dry-run energy is
/// the sender's: the contract's own energy share is not read. A TRC20 transfer never pays an account-creation fee.
///
/// # Errors
///
/// [`TronFeeError::Overflow`].
pub fn estimate_trc20_transfer_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    size: u64,
    energy: u64,
) -> Result<TronFeeEstimate, TronFeeError> {
    let energy_fee_sun = energy
        .saturating_sub(resources.energy_remaining())
        .checked_mul(prices.energy_price_sun)
        .ok_or(TronFeeError::Overflow)?;
    Ok(TronFeeEstimate {
        bandwidth_used: size,
        bandwidth_fee_sun: bandwidth_fee_sun(prices, resources, size)?,
        energy_used: energy,
        energy_fee_sun,
        account_creation_fee_sun: 0,
    })
}

/// The `fee_limit` of a TRC20 transfer whose dry run used `energy` (CRD ch.21 D21.10B.1): the cost of that energy
/// at the energy price plus [`TRC20_FEE_LIMIT_MARGIN_PERCENT`] of it, rounded up to whole SUN, and never above
/// [`TRC20_FEE_LIMIT_MAX_SUN`]. The limit counts the whole energy, staked or burned, because the node bounds both
/// with it. It is the most the call can cost the sender in energy.
///
/// # Errors
///
/// [`TronFeeError::EnergyCostAboveMaximum`] when the cost of the energy alone is above the maximum: a limit below
/// the need would make the call fail on chain and still charge the limit, so such a transfer is not built.
/// [`TronFeeError::Overflow`].
pub fn trc20_fee_limit_sun(prices: &TronChainPrices, energy: u64) -> Result<u64, TronFeeError> {
    let estimate_sun = energy
        .checked_mul(prices.energy_price_sun)
        .ok_or(TronFeeError::Overflow)?;
    if estimate_sun > TRC20_FEE_LIMIT_MAX_SUN {
        return Err(TronFeeError::EnergyCostAboveMaximum {
            estimate_sun,
            maximum_sun: TRC20_FEE_LIMIT_MAX_SUN,
        });
    }
    // `estimate_sun` is at most the maximum here, so neither the product nor the sum can overflow.
    let margin_sun = (estimate_sun * TRC20_FEE_LIMIT_MARGIN_PERCENT).div_ceil(100);
    Ok((estimate_sun + margin_sun).min(TRC20_FEE_LIMIT_MAX_SUN))
}

// ---------------------------------------------------------------------------
// Fee details (returned to the user)
// ---------------------------------------------------------------------------

/// The fee of a TRON transaction as a withdraw result reports it (CRD ch.21 R21.10B.7), the `{"type": "Tron"}`
/// member of the fee details. The fees are decimal amounts in TRX.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TronTxFeeDetails {
    /// The coin the fee is paid in: TRX, or for a token the ticker of its platform.
    pub coin: String,
    /// Bandwidth the transaction consumes.
    pub bandwidth_used: u64,
    /// Energy the contract call consumes (0 for a TRX transfer).
    pub energy_used: u64,
    /// TRX burned for bandwidth.
    pub bandwidth_fee: BigDecimal,
    /// TRX burned for energy.
    pub energy_fee: BigDecimal,
    /// The whole fee.
    pub total_fee: BigDecimal,
    /// The fee of creating the recipient's account, present only when it is not zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_creation_fee: Option<BigDecimal>,
}

impl TronTxFeeDetails {
    /// The details of `estimate`, whose whole fee is `total_fee_sun`, paid in `coin`.
    ///
    /// # Errors
    ///
    /// A number-conversion error when a figure does not convert. `total_fee_sun` is the estimate's
    /// [`TronFeeEstimate::total_fee_sun`], which the caller has already computed.
    pub fn new(coin: &str, estimate: &TronFeeEstimate, total_fee_sun: u64) -> NumConversResult<Self> {
        let trx = |sun: u64| u256_to_big_decimal(U256::from(sun), TRX_DECIMALS);
        let account_creation_fee = if estimate.account_creation_fee_sun == 0 {
            None
        } else {
            Some(trx(estimate.account_creation_fee_sun)?)
        };
        Ok(TronTxFeeDetails {
            coin: coin.to_owned(),
            bandwidth_used: estimate.bandwidth_used,
            energy_used: estimate.energy_used,
            bandwidth_fee: trx(estimate.bandwidth_fee_sun)?,
            energy_fee: trx(estimate.energy_fee_sun)?,
            total_fee: trx(total_fee_sun)?,
            account_creation_fee,
        })
    }
}

/// The fixed `fee_method` of [`TronGaslessFeeDetails`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum GaslessFeeMethod {
    #[serde(rename = "gasless")]
    Gasless,
}

/// The fixed `provider_name` of [`TronGaslessFeeDetails`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum GaslessProviderName {
    #[serde(rename = "gasfree")]
    GasFree,
}

/// The fee of a TRC20 withdraw through the gasless rail as a withdraw result reports it (CRD ch.63 §63.3.5), the
/// `{"type": "TronGasless"}` member of the fee details. The fee is paid in the token, not in TRX: every amount is
/// a decimal amount in token units.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TronGaslessFeeDetails {
    /// The coin the fee is paid in: the ticker of the token that is withdrawn.
    pub coin: String,
    pub fee_method: GaslessFeeMethod,
    pub provider_name: GaslessProviderName,
    /// The GasFree custody address of the sender, in base58check: the account the transfer and its fee are taken
    /// from.
    pub gasfree_address: String,
    /// The provider's fee for the transfer.
    pub transfer_fee: BigDecimal,
    /// The provider's fee for activating the custody account; zero for an account that is already activated.
    pub activation_fee: BigDecimal,
    /// `transfer_fee` plus `activation_fee`: the fee the provider quoted.
    pub total_token_fee: BigDecimal,
    /// The highest fee the signed authorization accepts (its `maxFee`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_max_fee: Option<BigDecimal>,
    /// The provider's id of the submitted transfer. Always present; `null` while the authorization is signed
    /// only and not submitted (CRD ch.63 D63.6).
    #[serde(default)]
    pub trace_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::api::ChainParameter;
    use super::*;

    fn prices() -> TronChainPrices {
        TronChainPrices {
            bandwidth_price_sun: 1000,
            energy_price_sun: 420,
            account_creation_fee_sun: Some(1_000_000),
            create_account_fee_sun: Some(100_000),
            create_account_bandwidth_rate: 1,
        }
    }

    /// Resources with `staked` staked and `free` free bandwidth left, and `energy` staked energy left.
    fn resources(staked: i64, free: i64, energy: i64) -> TronAccountResources {
        TronAccountResources {
            free_net_used: 100,
            free_net_limit: free + 100,
            net_used: 50,
            net_limit: staked + 50,
            energy_used: 7,
            energy_limit: energy + 7,
        }
    }

    fn parameters(entries: &[(&str, Option<i64>)]) -> GetChainParametersResponse {
        GetChainParametersResponse {
            chain_parameter: entries
                .iter()
                .map(|(key, value)| ChainParameter {
                    key: (*key).to_owned(),
                    value: *value,
                })
                .collect(),
        }
    }

    /// The whole-draw rule of the node: staked covers the whole size, or free covers the whole size, or the whole
    /// size is burned. 267 bytes at 1000 SUN are 267 000 SUN.
    #[test]
    fn bandwidth_is_drawn_whole_from_one_quota_or_burned_whole() {
        let fee = |staked, free| {
            estimate_trx_transfer_fee(&prices(), &resources(staked, free, 0), 267, true)
                .expect("an estimate")
                .bandwidth_fee_sun
        };
        // Staked covers the whole size (the free quota is empty).
        assert_eq!(fee(267, 0), 0);
        // Staked is short, free covers the whole size.
        assert_eq!(fee(266, 267), 0);
        assert_eq!(fee(0, 600), 0);
        // Neither covers alone: the whole size is burned, even where the two together would cover it.
        assert_eq!(fee(266, 266), 267_000);
        assert_eq!(fee(200, 100), 267_000);
        assert_eq!(fee(0, 0), 267_000);
    }

    #[test]
    fn a_trx_transfer_to_an_existing_account_has_no_energy_and_no_creation_fee() {
        let estimate = estimate_trx_transfer_fee(&prices(), &resources(0, 0, 0), 267, true).expect("an estimate");
        assert_eq!(estimate, TronFeeEstimate {
            bandwidth_used: 267,
            bandwidth_fee_sun: 267_000,
            energy_used: 0,
            energy_fee_sun: 0,
            account_creation_fee_sun: 0,
        });
        assert_eq!(estimate.total_fee_sun(), Ok(267_000));
    }

    /// A new account: the creation fee is added; the bandwidth is the size times the rate, free only from staked
    /// bandwidth, else replaced by the fixed fee with no bandwidth used.
    #[test]
    fn a_trx_transfer_to_a_new_account_adds_the_creation_fee() {
        let mut prices = prices();
        prices.create_account_bandwidth_rate = 2;

        // Staked bandwidth covers 267 x 2 = 534.
        let covered = estimate_trx_transfer_fee(&prices, &resources(534, 0, 0), 267, false).expect("an estimate");
        assert_eq!(covered, TronFeeEstimate {
            bandwidth_used: 534,
            bandwidth_fee_sun: 0,
            energy_used: 0,
            energy_fee_sun: 0,
            account_creation_fee_sun: 1_000_000,
        });
        assert_eq!(covered.total_fee_sun(), Ok(1_000_000));

        // One unit short, and free bandwidth does not count here: the fixed fee, no bandwidth used.
        let fixed = estimate_trx_transfer_fee(&prices, &resources(533, 5000, 0), 267, false).expect("an estimate");
        assert_eq!(fixed, TronFeeEstimate {
            bandwidth_used: 0,
            bandwidth_fee_sun: 100_000,
            energy_used: 0,
            energy_fee_sun: 0,
            account_creation_fee_sun: 1_000_000,
        });
        assert_eq!(fixed.total_fee_sun(), Ok(1_100_000));
    }

    #[test]
    fn a_new_account_without_the_figures_it_needs_is_an_error() {
        let mut no_creation_fee = prices();
        no_creation_fee.account_creation_fee_sun = None;
        assert_eq!(
            estimate_trx_transfer_fee(&no_creation_fee, &resources(600, 600, 0), 267, false),
            Err(TronFeeError::NoAccountCreationFee)
        );
        // The figure is not needed when the account exists.
        assert!(estimate_trx_transfer_fee(&no_creation_fee, &resources(600, 600, 0), 267, true).is_ok());

        let mut no_fixed_fee = prices();
        no_fixed_fee.create_account_fee_sun = None;
        assert_eq!(
            estimate_trx_transfer_fee(&no_fixed_fee, &resources(0, 600, 0), 267, false),
            Err(TronFeeError::NoCreateAccountFee)
        );
        // Not needed either when staked bandwidth covers the transfer.
        assert!(estimate_trx_transfer_fee(&no_fixed_fee, &resources(267, 0, 0), 267, false).is_ok());
    }

    /// Energy: the shortfall over the staked energy at the energy price. 14 000 - 4 000 = 10 000 units at 420 SUN
    /// are 4 200 000 SUN. No creation fee, whatever the recipient.
    #[test]
    fn a_trc20_transfer_pays_the_energy_shortfall() {
        let estimate =
            estimate_trc20_transfer_fee(&prices(), &resources(0, 600, 4_000), 345, 14_000).expect("an estimate");
        assert_eq!(estimate, TronFeeEstimate {
            bandwidth_used: 345,
            bandwidth_fee_sun: 0,
            energy_used: 14_000,
            energy_fee_sun: 4_200_000,
            account_creation_fee_sun: 0,
        });

        // Staked energy covers the call: only the burned bandwidth is left, 345 x 1000.
        let staked =
            estimate_trc20_transfer_fee(&prices(), &resources(0, 0, 14_000), 345, 14_000).expect("an estimate");
        assert_eq!(staked.energy_fee_sun, 0);
        assert_eq!(staked.total_fee_sun(), Ok(345_000));

        // No staked energy: 14 000 x 420 = 5 880 000.
        let burned = estimate_trc20_transfer_fee(&prices(), &resources(0, 0, 0), 345, 14_000).expect("an estimate");
        assert_eq!(burned.energy_fee_sun, 5_880_000);
        assert_eq!(burned.total_fee_sun(), Ok(6_225_000));
    }

    /// D21.10B.1: the cost of the dry-run energy plus 20 %, rounded up to whole SUN, at most 100 TRX; a cost above
    /// 100 TRX is refused.
    #[test]
    fn the_fee_limit_is_the_energy_cost_plus_the_margin_up_to_the_maximum() {
        let prices = prices();
        // 14 000 x 420 = 5 880 000; a fifth of it is 1 176 000.
        assert_eq!(trc20_fee_limit_sun(&prices, 14_000), Ok(7_056_000));
        // 1 x 420 = 420; a fifth is 84.
        assert_eq!(trc20_fee_limit_sun(&prices, 1), Ok(504));

        // Rounding up: 3 x 7 = 21, a fifth is 4.2, rounded up to 5.
        let mut odd = prices.clone();
        odd.energy_price_sun = 7;
        assert_eq!(trc20_fee_limit_sun(&odd, 3), Ok(26));

        // 200 000 x 420 = 84 000 000; with the margin 100 800 000, held at the maximum.
        assert_eq!(trc20_fee_limit_sun(&prices, 200_000), Ok(TRC20_FEE_LIMIT_MAX_SUN));
        // 100 000 000 exactly: the cost alone is the maximum and is still accepted.
        let mut round = prices.clone();
        round.energy_price_sun = 1000;
        assert_eq!(trc20_fee_limit_sun(&round, 100_000), Ok(TRC20_FEE_LIMIT_MAX_SUN));
        // One energy unit more is above the maximum.
        assert_eq!(
            trc20_fee_limit_sun(&round, 100_001),
            Err(TronFeeError::EnergyCostAboveMaximum {
                estimate_sun: 100_001_000,
                maximum_sun: 100_000_000,
            })
        );
        assert_eq!(trc20_fee_limit_sun(&prices, u64::MAX), Err(TronFeeError::Overflow));
    }

    #[test]
    fn chain_prices_are_read_from_the_parameters() {
        let prices = TronChainPrices::from_chain_parameters(&parameters(&[
            ("getTransactionFee", Some(1000)),
            ("getEnergyFee", Some(420)),
            ("getCreateNewAccountFeeInSystemContract", Some(1_000_000)),
            ("getCreateAccountFee", Some(100_000)),
            ("getCreateNewAccountBandwidthRate", Some(3)),
            ("unrelated_key", Some(999)),
        ]))
        .expect("usable parameters");
        assert_eq!(prices, TronChainPrices {
            bandwidth_price_sun: 1000,
            energy_price_sun: 420,
            account_creation_fee_sun: Some(1_000_000),
            create_account_fee_sun: Some(100_000),
            create_account_bandwidth_rate: 3,
        });
    }

    #[test]
    fn the_bandwidth_rate_defaults_to_one_and_the_optional_fees_stay_unknown() {
        let prices = TronChainPrices::from_chain_parameters(&parameters(&[
            ("getTransactionFee", Some(1000)),
            ("getEnergyFee", Some(420)),
            // An entry without a value is how the node lists a zero.
            ("getCreateNewAccountFeeInSystemContract", None),
        ]))
        .expect("usable parameters");
        assert_eq!(prices.create_account_bandwidth_rate, 1);
        assert_eq!(prices.account_creation_fee_sun, None);
        assert_eq!(prices.create_account_fee_sun, None);
    }

    #[test]
    fn chain_prices_without_a_positive_price_are_an_error() {
        for (bandwidth, energy, missing) in [
            (None, Some(420), "getTransactionFee"),
            (Some(0), Some(420), "getTransactionFee"),
            (Some(-1), Some(420), "getTransactionFee"),
            (Some(1000), None, "getEnergyFee"),
            (Some(1000), Some(0), "getEnergyFee"),
        ] {
            let answer = parameters(&[("getTransactionFee", bandwidth), ("getEnergyFee", energy)]);
            assert_eq!(
                TronChainPrices::from_chain_parameters(&answer),
                Err(TronFeeError::UnusableChainParameter(missing))
            );
        }
        // No entry at all.
        assert_eq!(
            TronChainPrices::from_chain_parameters(&parameters(&[])),
            Err(TronFeeError::UnusableChainParameter("getTransactionFee"))
        );
        // A reported rate must be positive.
        let bad_rate = parameters(&[
            ("getTransactionFee", Some(1000)),
            ("getEnergyFee", Some(420)),
            ("getCreateNewAccountBandwidthRate", Some(-2)),
        ]);
        assert_eq!(
            TronChainPrices::from_chain_parameters(&bad_rate),
            Err(TronFeeError::UnusableChainParameter("getCreateNewAccountBandwidthRate"))
        );
    }

    #[test]
    fn remaining_quotas_never_go_below_zero() {
        let res = TronAccountResources {
            free_net_used: 200,
            free_net_limit: 600,
            net_used: 100,
            net_limit: 500,
            energy_used: 5000,
            energy_limit: 10000,
        };
        assert_eq!(res.free_bandwidth_remaining(), 400);
        assert_eq!(res.staked_bandwidth_remaining(), 400);
        assert_eq!(res.energy_remaining(), 5000);

        let overused = TronAccountResources {
            free_net_used: 700,
            free_net_limit: 600,
            net_used: i64::MAX,
            net_limit: i64::MIN,
            energy_used: 1,
            energy_limit: 0,
        };
        assert_eq!(overused.free_bandwidth_remaining(), 0);
        assert_eq!(overused.staked_bandwidth_remaining(), 0);
        assert_eq!(overused.energy_remaining(), 0);
    }

    #[test]
    fn an_empty_resource_answer_is_all_zero() {
        let res: TronAccountResources = serde_json::from_str("{}").unwrap();
        assert_eq!(res.free_bandwidth_remaining(), 0);
        assert_eq!(res.staked_bandwidth_remaining(), 0);
        assert_eq!(res.energy_remaining(), 0);
    }

    /// The bandwidth of a transaction is its serialized size with one 65-byte signature plus 64 per contract.
    #[test]
    fn the_bandwidth_of_a_transaction_is_its_signed_size_plus_the_contract_allowance() {
        use super::super::proto::{ContractType, TransactionContract};

        let raw = TransactionRaw {
            ref_block_bytes: vec![0x01, 0x02],
            ref_block_hash: vec![3; 8],
            expiration: 1_700_000_060_000,
            contract: vec![TransactionContract {
                r#type: ContractType::TransferContract as i32,
                parameter: Some(prost_types::Any {
                    type_url: "type.googleapis.com/protocol.TransferContract".to_owned(),
                    value: vec![0x41; 50],
                }),
            }],
            timestamp: 1_700_000_000_000,
            fee_limit: 0,
        };
        let signed = Transaction {
            raw_data: Some(raw.clone()),
            signature: vec![vec![0xff; 65]],
        };
        assert_eq!(transaction_bandwidth(&raw), signed.encode_to_vec().len() as u64 + 64);
        // By hand: the raw body is 4 + 10 + 7 + 105 + 7 = 133 bytes, framed in 3; the signature is 65, framed in 2.
        assert_eq!(transaction_bandwidth(&raw), 133 + 3 + 67 + 64);
    }

    /// The wire form of R21.10B.7: fees as decimal strings in TRX, `account_creation_fee` only when not zero.
    #[test]
    fn fee_details_carry_the_members_of_the_withdraw_result() {
        let estimate = TronFeeEstimate {
            bandwidth_used: 345,
            bandwidth_fee_sun: 345_000,
            energy_used: 14_000,
            energy_fee_sun: 5_880_000,
            account_creation_fee_sun: 0,
        };
        let total = estimate.total_fee_sun().expect("a total");
        let details = TronTxFeeDetails::new("TRX", &estimate, total).expect("details");
        let wire = serde_json::to_value(&details).expect("JSON");
        assert_eq!(
            wire,
            serde_json::json!({
                "coin": "TRX",
                "bandwidth_used": 345,
                "energy_used": 14000,
                "bandwidth_fee": "0.345",
                "energy_fee": "5.88",
                "total_fee": "6.225",
            })
        );

        let creating = TronFeeEstimate {
            bandwidth_used: 0,
            bandwidth_fee_sun: 100_000,
            energy_used: 0,
            energy_fee_sun: 0,
            account_creation_fee_sun: 1_000_000,
        };
        let details = TronTxFeeDetails::new("TRX", &creating, 1_100_000).expect("details");
        let wire = serde_json::to_value(&details).expect("JSON");
        assert_eq!(wire["account_creation_fee"], "1");
        assert_eq!(wire["total_fee"], "1.1");
        assert_eq!(wire["energy_fee"], "0");
    }
}
