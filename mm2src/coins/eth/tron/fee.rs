//! TRON fee estimation types.
//!
//! TRON has a dual fee model:
//! - **Bandwidth**: consumed by all transactions (based on serialized size).
//!   Each account gets 600 free bandwidth per day; beyond that, 1 bandwidth = `getTransactionFee` SUN.
//! - **Energy**: consumed only by smart contract calls (TRC20, HTLC).
//!   Must be staked for or paid at `getEnergyFee` SUN per unit.

use serde::{Deserialize, Serialize};

use super::api::{ChainParameter, GetChainParametersResponse, TronApiError};

// ---------------------------------------------------------------------------
// Chain price parameters
// ---------------------------------------------------------------------------

/// Key chain parameters relevant to fee estimation.
#[derive(Clone, Debug, Default)]
pub struct TronChainPrices {
    /// Cost per bandwidth unit in SUN (key: `getTransactionFee`).
    pub bandwidth_price_sun: i64,
    /// Cost per energy unit in SUN (key: `getEnergyFee`).
    pub energy_price_sun: i64,
    /// Fee to create a new account via system contract in SUN.
    pub create_account_fee_sun: i64,
    /// Fee to create a new account via bandwidth in SUN.
    pub create_new_account_fee_sun: i64,
}

impl TronChainPrices {
    /// Parse chain prices from the `/wallet/getchainparameters` response.
    pub fn from_chain_parameters(resp: &GetChainParametersResponse) -> Self {
        let mut prices = TronChainPrices::default();
        for p in &resp.chain_parameter {
            match p.key.as_str() {
                "getTransactionFee" => prices.bandwidth_price_sun = p.value.unwrap_or(0),
                "getEnergyFee" => prices.energy_price_sun = p.value.unwrap_or(0),
                "getCreateNewAccountFeeInSystemContract" => prices.create_new_account_fee_sun = p.value.unwrap_or(0),
                "getCreateAccountFee" => prices.create_account_fee_sun = p.value.unwrap_or(0),
                _ => {},
            }
        }
        prices
    }
}

// ---------------------------------------------------------------------------
// Account resources
// ---------------------------------------------------------------------------

/// Account bandwidth and energy resource quotas (from `/wallet/getaccountresource`).
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
    pub fn free_bandwidth_remaining(&self) -> i64 { (self.free_net_limit - self.free_net_used).max(0) }

    /// Remaining staked bandwidth.
    pub fn staked_bandwidth_remaining(&self) -> i64 { (self.net_limit - self.net_used).max(0) }

    /// Remaining energy.
    pub fn energy_remaining(&self) -> i64 { (self.energy_limit - self.energy_used).max(0) }
}

// ---------------------------------------------------------------------------
// Fee details (returned to the user)
// ---------------------------------------------------------------------------

/// TRON transaction fee breakdown for display.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TronTxFeeDetails {
    /// Total fee in SUN.
    pub total_fee_sun: i64,
    /// Bandwidth consumed (bytes).
    pub bandwidth_used: i64,
    /// Bandwidth fee component in SUN (0 if covered by free/staked bandwidth).
    pub bandwidth_fee_sun: i64,
    /// Energy consumed (only for smart contract calls).
    pub energy_used: i64,
    /// Energy fee component in SUN (0 if covered by staked energy).
    pub energy_fee_sun: i64,
    /// Account activation fee if sending to a new address.
    pub activation_fee_sun: i64,
}

/// Estimate fee for a native TRX transfer.
///
/// Native transfers consume ~270 bytes of bandwidth and no energy.
/// If the recipient account doesn't exist, there's an additional activation fee.
pub fn estimate_trx_transfer_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    recipient_exists: bool,
) -> TronTxFeeDetails {
    // Typical TRX transfer size is ~270 bytes of bandwidth.
    const TRX_TRANSFER_BANDWIDTH: i64 = 270;

    let activation_fee = if recipient_exists {
        0
    } else {
        prices.create_new_account_fee_sun
    };

    let free_bw = resources.free_bandwidth_remaining();
    let staked_bw = resources.staked_bandwidth_remaining();
    let total_bw = free_bw + staked_bw;

    let bandwidth_fee = if total_bw >= TRX_TRANSFER_BANDWIDTH {
        0 // Covered by free + staked bandwidth.
    } else {
        TRX_TRANSFER_BANDWIDTH * prices.bandwidth_price_sun
    };

    TronTxFeeDetails {
        total_fee_sun: bandwidth_fee + activation_fee,
        bandwidth_used: TRX_TRANSFER_BANDWIDTH,
        bandwidth_fee_sun: bandwidth_fee,
        energy_used: 0,
        energy_fee_sun: 0,
        activation_fee_sun: activation_fee,
    }
}

/// Estimate fee for a TRC20 token transfer.
///
/// TRC20 transfers consume bandwidth (~350 bytes) plus energy (~29000 units typical).
pub fn estimate_trc20_transfer_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    estimated_energy: i64,
    recipient_exists: bool,
) -> TronTxFeeDetails {
    const TRC20_TRANSFER_BANDWIDTH: i64 = 350;

    let activation_fee = if recipient_exists {
        0
    } else {
        prices.create_new_account_fee_sun
    };

    let free_bw = resources.free_bandwidth_remaining();
    let staked_bw = resources.staked_bandwidth_remaining();
    let total_bw = free_bw + staked_bw;

    let bandwidth_fee = if total_bw >= TRC20_TRANSFER_BANDWIDTH {
        0
    } else {
        TRC20_TRANSFER_BANDWIDTH * prices.bandwidth_price_sun
    };

    let available_energy = resources.energy_remaining();
    let energy_to_pay = (estimated_energy - available_energy).max(0);
    let energy_fee = energy_to_pay * prices.energy_price_sun;

    TronTxFeeDetails {
        total_fee_sun: bandwidth_fee + energy_fee + activation_fee,
        bandwidth_used: TRC20_TRANSFER_BANDWIDTH,
        bandwidth_fee_sun: bandwidth_fee,
        energy_used: estimated_energy,
        energy_fee_sun: energy_fee,
        activation_fee_sun: activation_fee,
    }
}

/// Estimate the resource fee of a swap-contract call (R-TF1).
///
/// Swap HTLC calls (`ethPayment`/`erc20Payment`/`receiverSpend`/`senderRefund`)
/// are `TriggerSmartContract` invocations: they consume bandwidth for their
/// serialized size and energy for VM execution, priced against the Tron
/// resource model (§21.8). `estimated_energy` comes from the constant-contract
/// dry-run endpoint (§21.9); the bandwidth size is the typical call size.
pub fn estimate_swap_contract_call_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    bandwidth_bytes: i64,
    estimated_energy: i64,
) -> TronTxFeeDetails {
    let free_bw = resources.free_bandwidth_remaining();
    let staked_bw = resources.staked_bandwidth_remaining();
    let total_bw = free_bw + staked_bw;

    let bandwidth_fee = if total_bw >= bandwidth_bytes {
        0
    } else {
        bandwidth_bytes * prices.bandwidth_price_sun
    };

    let available_energy = resources.energy_remaining();
    let energy_to_pay = (estimated_energy - available_energy).max(0);
    let energy_fee = energy_to_pay * prices.energy_price_sun;

    TronTxFeeDetails {
        total_fee_sun: bandwidth_fee + energy_fee,
        bandwidth_used: bandwidth_bytes,
        bandwidth_fee_sun: bandwidth_fee,
        energy_used: estimated_energy,
        energy_fee_sun: energy_fee,
        activation_fee_sun: 0,
    }
}

/// Typical serialized size (bandwidth) of a swap-contract HTLC call.
pub const SWAP_CALL_BANDWIDTH: i64 = 350;

/// Conservative default energy for an HTLC payment call when the
/// constant-contract dry-run endpoint is unavailable.
pub const SWAP_PAYMENT_ENERGY_DEFAULT: i64 = 60_000;

/// Conservative default energy for an HTLC spend/refund call.
pub const SWAP_SPEND_ENERGY_DEFAULT: i64 = 40_000;

/// Trade-fee estimate for a maker/taker HTLC *payment* (R-TF1): the cost of the
/// `ethPayment`/`erc20Payment` lock the coin will broadcast.
pub fn estimate_maker_taker_payment_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    estimated_energy: Option<i64>,
) -> TronTxFeeDetails {
    let energy = estimated_energy.unwrap_or(SWAP_PAYMENT_ENERGY_DEFAULT);
    estimate_swap_contract_call_fee(prices, resources, SWAP_CALL_BANDWIDTH, energy)
}

/// Trade-fee estimate for the *send-taker-fee* step (R-TF1, R-DF1): a native
/// TRX transfer or a TRC20 transfer to the dex-fee recipient.
pub fn estimate_fee_to_send_taker_fee(
    prices: &TronChainPrices,
    resources: &TronAccountResources,
    is_trc20: bool,
    estimated_energy: Option<i64>,
    recipient_exists: bool,
) -> TronTxFeeDetails {
    if is_trc20 {
        let energy = estimated_energy.unwrap_or(SWAP_SPEND_ENERGY_DEFAULT);
        estimate_trc20_transfer_fee(prices, resources, energy, recipient_exists)
    } else {
        estimate_trx_transfer_fee(prices, resources, recipient_exists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_prices() -> TronChainPrices {
        TronChainPrices {
            bandwidth_price_sun: 1000,
            energy_price_sun: 420,
            create_account_fee_sun: 100_000,
            create_new_account_fee_sun: 1_000_000,
        }
    }

    fn fresh_resources() -> TronAccountResources {
        TronAccountResources {
            free_net_used: 0,
            free_net_limit: 600,
            net_used: 0,
            net_limit: 0,
            energy_used: 0,
            energy_limit: 0,
        }
    }

    #[test]
    fn test_trx_transfer_free_bandwidth() {
        let prices = default_prices();
        let resources = fresh_resources();
        let fee = estimate_trx_transfer_fee(&prices, &resources, true);
        // 270 < 600 free bandwidth, so bandwidth_fee = 0.
        assert_eq!(fee.bandwidth_fee_sun, 0);
        assert_eq!(fee.activation_fee_sun, 0);
        assert_eq!(fee.total_fee_sun, 0);
    }

    #[test]
    fn test_trx_transfer_no_bandwidth() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.free_net_used = 600; // All free bandwidth used.
        let fee = estimate_trx_transfer_fee(&prices, &resources, true);
        assert_eq!(fee.bandwidth_fee_sun, 270 * 1000);
        assert_eq!(fee.total_fee_sun, 270_000);
    }

    #[test]
    fn test_trx_transfer_new_account() {
        let prices = default_prices();
        let resources = fresh_resources();
        let fee = estimate_trx_transfer_fee(&prices, &resources, false);
        assert_eq!(fee.activation_fee_sun, 1_000_000);
        assert_eq!(fee.total_fee_sun, 1_000_000);
    }

    #[test]
    fn test_trc20_transfer_with_energy() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.free_net_used = 600; // No free bandwidth.
        let fee = estimate_trc20_transfer_fee(&prices, &resources, 29000, true);
        // bandwidth_fee = 350 * 1000 = 350_000
        assert_eq!(fee.bandwidth_fee_sun, 350_000);
        // energy_fee = 29000 * 420 = 12_180_000
        assert_eq!(fee.energy_fee_sun, 12_180_000);
        assert_eq!(fee.total_fee_sun, 350_000 + 12_180_000);
    }

    #[test]
    fn test_trc20_transfer_with_staked_energy() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.energy_limit = 50_000;
        resources.energy_used = 10_000;
        // Available energy: 40_000, needed: 29_000 → fully covered.
        let fee = estimate_trc20_transfer_fee(&prices, &resources, 29000, true);
        assert_eq!(fee.energy_fee_sun, 0);
    }

    #[test]
    fn test_chain_prices_from_parameters() {
        let resp = GetChainParametersResponse {
            chain_parameter: vec![
                ChainParameter {
                    key: "getTransactionFee".to_string(),
                    value: Some(1000),
                },
                ChainParameter {
                    key: "getEnergyFee".to_string(),
                    value: Some(420),
                },
                ChainParameter {
                    key: "getCreateNewAccountFeeInSystemContract".to_string(),
                    value: Some(1_000_000),
                },
                ChainParameter {
                    key: "getCreateAccountFee".to_string(),
                    value: Some(100_000),
                },
                ChainParameter {
                    key: "unrelated_key".to_string(),
                    value: Some(999),
                },
            ],
        };
        let prices = TronChainPrices::from_chain_parameters(&resp);
        assert_eq!(prices.bandwidth_price_sun, 1000);
        assert_eq!(prices.energy_price_sun, 420);
        assert_eq!(prices.create_new_account_fee_sun, 1_000_000);
        assert_eq!(prices.create_account_fee_sun, 100_000);
    }

    #[test]
    fn test_account_resources_remaining() {
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
    }

    #[test]
    fn test_resources_default_zero() {
        let json = "{}";
        let res: TronAccountResources = serde_json::from_str(json).unwrap();
        assert_eq!(res.free_net_used, 0);
        assert_eq!(res.free_net_limit, 0);
        assert_eq!(res.energy_remaining(), 0);
    }

    // ---- R-TF1 trade-fee estimators ----

    #[test]
    fn test_swap_payment_fee_uses_default_energy() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.free_net_used = 600; // no free bandwidth
        let fee = estimate_maker_taker_payment_fee(&prices, &resources, None);
        assert_eq!(fee.energy_used, SWAP_PAYMENT_ENERGY_DEFAULT);
        assert_eq!(fee.bandwidth_fee_sun, SWAP_CALL_BANDWIDTH * 1000);
        assert_eq!(fee.energy_fee_sun, SWAP_PAYMENT_ENERGY_DEFAULT * 420);
        // No activation fee component for a contract call.
        assert_eq!(fee.activation_fee_sun, 0);
    }

    #[test]
    fn test_swap_payment_fee_dry_run_energy_overrides_default() {
        let prices = default_prices();
        let resources = fresh_resources();
        let fee = estimate_maker_taker_payment_fee(&prices, &resources, Some(12_345));
        assert_eq!(fee.energy_used, 12_345);
    }

    #[test]
    fn test_swap_payment_fee_staked_energy_covers_cost() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.energy_limit = 100_000; // covers the default payment energy
        let fee = estimate_maker_taker_payment_fee(&prices, &resources, None);
        assert_eq!(fee.energy_fee_sun, 0);
    }

    #[test]
    fn test_fee_to_send_taker_fee_native_vs_trc20() {
        let prices = default_prices();
        let mut resources = fresh_resources();
        resources.free_net_used = 600;
        // Native TRX taker-fee: no energy component.
        let native = estimate_fee_to_send_taker_fee(&prices, &resources, false, None, true);
        assert_eq!(native.energy_used, 0);
        // TRC20 taker-fee: energy component present.
        let trc20 = estimate_fee_to_send_taker_fee(&prices, &resources, true, Some(29_000), true);
        assert_eq!(trc20.energy_used, 29_000);
        assert_eq!(trc20.energy_fee_sun, 29_000 * 420);
    }
}
