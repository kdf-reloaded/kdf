//! # Infura gas-price client
//!
//! Adapter around Infura's `suggestedGasFees` REST endpoint. Unlike Blocknative
//! the response already comes pre-bucketed into low / medium / high tiers and
//! includes wait-time predictions, so the projection onto our
//! [`FeePerGasEstimated`] is a near-flat copy.
//!
//! # Public exports
//! - [`InfuraFeeFetcher`] — single-call entry point invoked by the EIP-1559
//!   gas-fee dispatcher in `eth_impl::EthCoin::get_eip1559_gas_fee`.
//!
//! # Invariants
//! - Endpoint path (`networks/1/suggestedGasFees`) is wire contract.
//! - Wait times are reported in **milliseconds** by Infura and forwarded
//!   verbatim.
//! - Fee values arrive in **gwei** and are converted to wei via
//!   [`wei_from_gwei_decimal`] before exposure.

use std::convert::{TryFrom, TryInto};

use bigdecimal::BigDecimal;
use http::StatusCode;
use serde::Deserialize;

use mm2_err_handle::mm_error::MmError;
use mm2_err_handle::prelude::*;
use mm2_net::transport::slurp_url_with_headers;

use super::{fee_service_status_error, fee_service_transport_error, EstimationSource, FeePerGasEstimated,
            FeePerGasLevel};
use crate::eth::{wei_from_gwei_decimal, Web3RpcError, Web3RpcResult};
use crate::NumConversError;

lazy_static! {
    /// Bearer token for the Infura gas API (env var
    /// `INFURA_GAS_API_AUTH_TEST`); empty when unset.
    static ref INFURA_GAS_API_AUTH_TEST: String =
        std::env::var("INFURA_GAS_API_AUTH_TEST").unwrap_or_default();
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One priority tier of the Infura `suggestedGasFees` response.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct InfuraFeePerGasLevel {
    #[serde(rename = "suggestedMaxPriorityFeePerGas")]
    pub suggested_max_priority_fee_per_gas: BigDecimal,
    #[serde(rename = "suggestedMaxFeePerGas")]
    pub suggested_max_fee_per_gas: BigDecimal,
    #[serde(rename = "minWaitTimeEstimate")]
    pub min_wait_time_estimate: u32,
    #[serde(rename = "maxWaitTimeEstimate")]
    pub max_wait_time_estimate: u32,
}

impl InfuraFeePerGasLevel {
    /// Convert this tier to the engine-internal wei-denominated shape.
    fn to_internal(&self) -> Result<FeePerGasLevel, MmError<NumConversError>> {
        Ok(FeePerGasLevel {
            max_priority_fee_per_gas: wei_from_gwei_decimal(&self.suggested_max_priority_fee_per_gas)?,
            max_fee_per_gas: wei_from_gwei_decimal(&self.suggested_max_fee_per_gas)?,
            min_wait_time: Some(self.min_wait_time_estimate),
            max_wait_time: Some(self.max_wait_time_estimate),
        })
    }
}

/// Top-level Infura `suggestedGasFees` envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct InfuraFeePerGas {
    pub low: InfuraFeePerGasLevel,
    pub medium: InfuraFeePerGasLevel,
    pub high: InfuraFeePerGasLevel,
    #[serde(rename = "estimatedBaseFee")]
    pub estimated_base_fee: BigDecimal,
    #[serde(rename = "priorityFeeTrend")]
    pub priority_fee_trend: String,
    #[serde(rename = "baseFeeTrend")]
    pub base_fee_trend: String,
}

impl TryFrom<InfuraFeePerGas> for FeePerGasEstimated {
    type Error = MmError<NumConversError>;

    fn try_from(envelope: InfuraFeePerGas) -> Result<Self, Self::Error> {
        Ok(Self {
            base_fee: wei_from_gwei_decimal(&envelope.estimated_base_fee)?,
            source: EstimationSource::Infura,
            base_fee_trend: envelope.base_fee_trend,
            priority_fee_trend: envelope.priority_fee_trend,
            low: envelope.low.to_internal()?,
            medium: envelope.medium.to_internal()?,
            high: envelope.high.to_internal()?,
        })
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Outbound request descriptor.
struct InfuraRequest {
    url: String,
    headers: Vec<(&'static str, &'static str)>,
}

/// Stateless adapter that fetches an Infura `suggestedGasFees` snapshot and
/// projects it onto [`FeePerGasEstimated`].
pub(crate) struct InfuraFeeFetcher;

impl InfuraFeeFetcher {
    const ENDPOINT: &'static str = "networks/1/suggestedGasFees";

    fn request(base_url: &str) -> InfuraRequest {
        let url = format!("{}/{}", base_url.trim_end_matches('/'), Self::ENDPOINT);
        let headers = vec![("Authorization", INFURA_GAS_API_AUTH_TEST.as_str())];
        InfuraRequest { url, headers }
    }

    async fn issue(req: InfuraRequest) -> Result<InfuraFeePerGas, MmError<String>> {
        let resp = slurp_url_with_headers(&req.url, req.headers)
            .await
            .mm_err(|e| fee_service_transport_error(&e, &req.url))?;
        if resp.0 != StatusCode::OK {
            return MmError::err(fee_service_status_error(&req.url, resp.0));
        }
        serde_json::from_slice(&resp.2).map_to_mm(|e| e.to_string())
    }

    /// Fetch the next-block fee estimate from Infura.
    ///
    /// # Errors
    /// - [`Web3RpcError::Transport`] for transport / non-200 status responses.
    /// - [`Web3RpcError::Internal`] for numeric conversion failures.
    pub async fn fetch_fee_estimation(base_url: &str) -> Web3RpcResult<FeePerGasEstimated> {
        let request = Self::request(base_url);
        let envelope = Self::issue(request).await.mm_err(Web3RpcError::Transport)?;
        envelope
            .try_into()
            .mm_err(|e: NumConversError| Web3RpcError::Internal(e.to_string()))
    }
}
