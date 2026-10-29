//! # EIP-1559 fee estimation
//!
//! # Public exports
//! - [`FeePerGasEstimated`] — full base+tip estimate at three priority levels.
//! - [`FeePerGasLevel`] — per-priority cap and (optional) wait window.
//! - [`EstimationSource`] — which backend produced an estimate.
//! - [`GasApiConfig`] / [`GasApiProvider`] — runtime gas-API selection.
//!
//! # Invariants
//! - `EstimationSource::Display` strings (`empty`, `simple`, `infura`,
//!   `blocknative`) are part of the JSON response surface; do not rename.
//! - `FEE_PRIORITY_LEVEL_N == 3` is hard-coded throughout: low / medium / high.
//! - All `U256` fee values are denominated in **wei**, never gwei.

pub mod block_native;
pub mod infura;
pub mod simple;

use ethereum_types::U256;
use serde::Deserialize;

/// The text of a failed request to the fee-estimation service at `url` (CRD R35.1.14): the service is named
/// host-only, and a copy of the URL embedded in the lower layer's message is replaced the same way.
pub(crate) fn fee_service_transport_error(err: &mm2_net::transport::SlurpError, url: &str) -> String {
    crate::node_url::scrub_node_url(&err.to_string(), url)
}

/// The text of an HTTP status other than 200 from the fee-estimation service at `url`, naming it host-only.
pub(crate) fn fee_service_status_error(url: &str, status: http::StatusCode) -> String {
    format!("{} failed with status code {}", crate::node_url::host_only(url), status)
}

/// Number of priority tiers we always emit (low, medium, high).
pub(crate) const FEE_PRIORITY_LEVEL_N: usize = 3;

/// Backend that produced an [`FeePerGasEstimated`] sample.
///
/// The `Display` impl yields the lower-case provider name and is part of the
/// JSON response surface (`source` field of `FeePerGasEstimated`); see the
/// module-level invariants.
#[derive(Clone, Debug, Default)]
pub enum EstimationSource {
    /// No data has been gathered yet.
    #[default]
    Empty,
    /// Built-in `eth_feeHistory` percentile estimator (see [`simple`]).
    Simple,
    /// Infura `suggestedGasFees` REST endpoint (see [`infura`]).
    Infura,
    /// Blocknative `gasprices/blockprices` REST endpoint (see [`block_native`]).
    Blocknative,
}

impl std::fmt::Display for EstimationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            EstimationSource::Empty => "empty",
            EstimationSource::Simple => "simple",
            EstimationSource::Infura => "infura",
            EstimationSource::Blocknative => "blocknative",
        };
        f.write_str(label)
    }
}

/// Index of a priority tier in `[low, medium, high]` ordering.
pub(crate) enum PriorityLevelId {
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Estimated fee caps for a single priority tier (wei).
#[derive(Clone, Debug, Default)]
pub struct FeePerGasLevel {
    /// Tip paid to the validator above the base fee, in wei.
    pub max_priority_fee_per_gas: U256,
    /// Hard cap (`max_priority_fee_per_gas + base_fee_buffer`), in wei.
    pub max_fee_per_gas: U256,
    /// Predicted minimum mempool wait time, in milliseconds.
    pub min_wait_time: Option<u32>,
    /// Predicted maximum mempool wait time, in milliseconds.
    pub max_wait_time: Option<u32>,
}

/// Full fee snapshot produced by an [`EstimationSource`].
///
/// `base_fee` is the next-block base fee in wei. Per-tier caps live in
/// [`FeePerGasLevel`]. Trend strings are provider-supplied free-form labels
/// (e.g. `"up"`, `"stable"`).
#[derive(Clone, Debug, Default)]
pub struct FeePerGasEstimated {
    /// Predicted base fee for the next block, in wei.
    pub base_fee: U256,
    /// Backend that produced this estimate.
    pub source: EstimationSource,
    /// Free-form base-fee trend label, provider-defined.
    pub base_fee_trend: String,
    /// Free-form priority-fee trend label, provider-defined.
    pub priority_fee_trend: String,
    /// Conservative tier (cheapest, slowest).
    pub low: FeePerGasLevel,
    /// Default tier.
    pub medium: FeePerGasLevel,
    /// Aggressive tier (most expensive, fastest).
    pub high: FeePerGasLevel,
}

/// External REST gas-API backend the user may opt into.
#[derive(Clone, Deserialize)]
pub enum GasApiProvider {
    Infura,
    Blocknative,
}

/// Runtime configuration block selecting a [`GasApiProvider`] and its base URL.
#[derive(Clone, Deserialize)]
pub struct GasApiConfig {
    pub provider: GasApiProvider,
    /// Base URL of the gas-API or proxy.
    pub url: String,
}
