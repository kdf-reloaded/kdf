//! # Blocknative gas-price client
//!
//! Adapter around the Blocknative `gasprices/blockprices` REST endpoint that
//! turns the provider's confidence-tier response into our internal
//! [`FeePerGasEstimated`] shape.
//!
//! # Public exports
//! - [`BlocknativeFeeFetcher`] — single-call entry point invoked by the EIP-1559
//!   gas-fee dispatcher in `eth_impl::EthCoin::get_eip1559_gas_fee`.
//!
//! # Invariants
//! - The endpoint path (`gasprices/blockprices`) and the four query
//!   parameters (`confidenceLevels=10|50|90`, `withBaseFees=true`) are wire
//!   contract; they form Blocknative's documented URL surface.
//! - Confidence ordering in the response is **high → medium → low** (indices
//!   `0`, `1`, `2`); we map those to our `[low, medium, high]` tiers below.
//! - Fee values arrive in **gwei** as `BigDecimal`; we always convert through
//!   [`wei_from_gwei_decimal`] before exposing them to the rest of the stack.

use std::convert::{TryFrom, TryInto};

use bigdecimal::BigDecimal;
use http::StatusCode;
use serde::Deserialize;

use mm2_err_handle::mm_error::MmError;
use mm2_err_handle::prelude::*;
use mm2_net::transport::slurp_url_with_headers;

use super::{fee_service_status_error, fee_service_transport_error, EstimationSource, FeePerGasEstimated,
            FeePerGasLevel, FEE_PRIORITY_LEVEL_N};
use crate::eth::{wei_from_gwei_decimal, Web3RpcError, Web3RpcResult};
use crate::NumConversError;

lazy_static! {
    /// Bearer token for the Blocknative gas API. Sourced from the
    /// `BLOCKNATIVE_GAS_API_AUTH_TEST` environment variable; empty in
    /// release builds where the user is expected to proxy through their
    /// own gateway.
    static ref BLOCKNATIVE_GAS_API_AUTH_TEST: String =
        std::env::var("BLOCKNATIVE_GAS_API_AUTH_TEST").unwrap_or_default();
}

/// Indices of Blocknative's confidence tiers within `estimated_prices`.
///
/// Blocknative returns *highest* confidence first; we name those positions
/// here so the [`TryFrom`] mapping below stays readable.
mod confidence {
    pub const HIGH: usize = 0;
    pub const MEDIUM: usize = 1;
    pub const LOW: usize = 2;
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One confidence tier inside [`BlocknativeBlockPrices`].
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BlocknativeEstimatedPrices {
    #[serde(rename = "maxPriorityFeePerGas")]
    pub max_priority_fee_per_gas: BigDecimal,
    #[serde(rename = "maxFeePerGas")]
    pub max_fee_per_gas: BigDecimal,
}

/// One block's worth of price predictions returned by Blocknative.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BlocknativeBlockPrices {
    #[serde(rename = "baseFeePerGas")]
    pub base_fee_per_gas: BigDecimal,
    #[serde(rename = "estimatedPrices")]
    pub estimated_prices: Vec<BlocknativeEstimatedPrices>,
}

/// Top-level Blocknative response envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct BlocknativeBlockPricesResponse {
    #[serde(rename = "blockPrices")]
    pub block_prices: Vec<BlocknativeBlockPrices>,
}

impl BlocknativeBlockPrices {
    /// Build a [`FeePerGasLevel`] from one confidence tier of this block.
    ///
    /// Blocknative does not publish wait-time predictions, so the
    /// `min_wait_time` / `max_wait_time` fields stay `None`.
    fn level_at(&self, idx: usize) -> Result<FeePerGasLevel, MmError<NumConversError>> {
        let tier = &self.estimated_prices[idx];
        Ok(FeePerGasLevel {
            max_fee_per_gas: wei_from_gwei_decimal(&tier.max_fee_per_gas)?,
            max_priority_fee_per_gas: wei_from_gwei_decimal(&tier.max_priority_fee_per_gas)?,
            min_wait_time: None,
            max_wait_time: None,
        })
    }
}

impl TryFrom<BlocknativeBlockPricesResponse> for FeePerGasEstimated {
    type Error = MmError<NumConversError>;

    fn try_from(response: BlocknativeBlockPricesResponse) -> Result<Self, Self::Error> {
        // No prediction → return the all-zero default. The caller will fall
        // back to the simple `eth_feeHistory` estimator.
        let block = match response.block_prices.first() {
            Some(b) if b.estimated_prices.len() >= FEE_PRIORITY_LEVEL_N => b,
            _ => return Ok(Self::default()),
        };

        Ok(Self {
            base_fee: wei_from_gwei_decimal(&block.base_fee_per_gas)?,
            source: EstimationSource::Blocknative,
            base_fee_trend: String::new(),
            priority_fee_trend: String::new(),
            low: block.level_at(confidence::LOW)?,
            medium: block.level_at(confidence::MEDIUM)?,
            high: block.level_at(confidence::HIGH)?,
        })
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Outbound request descriptor passed to [`slurp_url_with_headers`].
struct BlocknativeRequest {
    url: String,
    headers: Vec<(&'static str, &'static str)>,
}

/// Stateless adapter that fetches a Blocknative `blockprices` snapshot and
/// projects it onto [`FeePerGasEstimated`].
pub(crate) struct BlocknativeFeeFetcher;

impl BlocknativeFeeFetcher {
    const ENDPOINT: &'static str = "gasprices/blockprices";

    /// Compose the full request URL plus auth headers from `base_url`.
    fn request(base_url: &str) -> BlocknativeRequest {
        let url = format!(
            "{}/{}?confidenceLevels=10&confidenceLevels=50&confidenceLevels=90&withBaseFees=true",
            base_url.trim_end_matches('/'),
            Self::ENDPOINT,
        );
        let headers = vec![("Authorization", BLOCKNATIVE_GAS_API_AUTH_TEST.as_str())];
        BlocknativeRequest { url, headers }
    }

    /// Issue the GET request and JSON-decode the body.
    async fn issue(req: BlocknativeRequest) -> Result<BlocknativeBlockPricesResponse, MmError<String>> {
        let resp = slurp_url_with_headers(&req.url, req.headers)
            .await
            .mm_err(|e| fee_service_transport_error(&e, &req.url))?;
        if resp.0 != StatusCode::OK {
            return MmError::err(fee_service_status_error(&req.url, resp.0));
        }
        serde_json::from_slice(&resp.2).map_to_mm(|e| e.to_string())
    }

    /// Fetch the next-block fee estimate from Blocknative.
    ///
    /// # Errors
    /// - [`Web3RpcError::Transport`] when the HTTP call fails or the gateway
    ///   returns a non-200 status.
    /// - [`Web3RpcError::Internal`] when the JSON envelope decodes but the
    ///   numeric conversion to wei underflows / overflows.
    pub async fn fetch_fee_estimation(base_url: &str) -> Web3RpcResult<FeePerGasEstimated> {
        let request = Self::request(base_url);
        let envelope = Self::issue(request).await.mm_err(Web3RpcError::Transport)?;
        envelope
            .try_into()
            .mm_err(|e: NumConversError| Web3RpcError::Internal(e.to_string()))
    }
}
