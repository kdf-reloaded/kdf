//! Public JSON-RPC request and response shapes for the
//! `experimental::1inch_v6_0::classic_swap_*` handlers (CRD §23.8A.4).
//!
//! Response records reuse the trading-API library's provider-sourced records
//! (`TokenInfo`, `ProtocolInfo`, `ProtocolImage`) so that the library's
//! anti-phishing image-URL validation (CRD §23.10) is applied on decode.

use common::mm_number::{BigDecimal, MmNumber, MmNumberMultiRepr};
use std::collections::HashMap;
use trading_api::one_inch_api::classic_swap_types::{ProtocolImage, ProtocolInfo, TokenInfo};

const fn default_true() -> bool { true }

/// `classic_swap_contract` request: an empty JSON object (no chain field).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicSwapContractRequest {}

/// `classic_swap_quote` request (binds `GET /swap/v6.0/{chainId}/quote`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicSwapQuoteRequest {
    /// Source coin ticker; resolved to an EVM coin that supplies the chain id.
    pub base: String,
    /// Destination coin ticker; must resolve to the same chain.
    pub rel: String,
    /// Sell amount in `base` coin units (with fraction).
    pub amount: MmNumber,
    pub fee: Option<f32>,
    pub protocols: Option<String>,
    pub gas_price: Option<String>,
    pub complexity_level: Option<u32>,
    pub parts: Option<u32>,
    pub main_route_parts: Option<u32>,
    pub gas_limit: Option<u128>,
    #[serde(default = "default_true")]
    pub include_tokens_info: bool,
    #[serde(default = "default_true")]
    pub include_protocols: bool,
    #[serde(default = "default_true")]
    pub include_gas: bool,
    pub connector_tokens: Option<String>,
}

/// `classic_swap_create` request (binds `GET /swap/v6.0/{chainId}/swap`).
///
/// Carries all of [`ClassicSwapQuoteRequest`]'s fields (identical spellings,
/// types, optionality, and bounds) plus the create-only fields. The fields are
/// listed explicitly rather than flattened because `deny_unknown_fields` is
/// incompatible with `#[serde(flatten)]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicSwapCreateRequest {
    pub base: String,
    pub rel: String,
    pub amount: MmNumber,
    pub fee: Option<f32>,
    pub protocols: Option<String>,
    pub gas_price: Option<String>,
    pub complexity_level: Option<u32>,
    pub parts: Option<u32>,
    pub main_route_parts: Option<u32>,
    pub gas_limit: Option<u128>,
    #[serde(default = "default_true")]
    pub include_tokens_info: bool,
    #[serde(default = "default_true")]
    pub include_protocols: bool,
    #[serde(default = "default_true")]
    pub include_gas: bool,
    pub connector_tokens: Option<String>,
    /// Allowed slippage, min 0, max 50.
    pub slippage: f32,
    pub excluded_protocols: Option<String>,
    pub permit: Option<String>,
    pub compatibility: Option<bool>,
    pub receiver: Option<String>,
    pub referrer: Option<String>,
    pub disable_estimate: Option<bool>,
    pub allow_partial_fill: Option<bool>,
    pub use_permit2: Option<bool>,
}

/// `classic_swap_liquidity_sources` request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicSwapLiquiditySourcesRequest {
    pub chain_id: u64,
}

/// `classic_swap_tokens` request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicSwapTokensRequest {
    pub chain_id: u64,
}

/// Shared classic-swap response for `classic_swap_quote` and
/// `classic_swap_create`.
#[derive(Serialize)]
pub struct ClassicSwapResponse {
    /// Destination amount in `rel` coin units (detailed-decimal).
    pub dst_amount: MmNumberMultiRepr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_token: Option<TokenInfo>,
    pub src_token_kdf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_token: Option<TokenInfo>,
    pub dst_token_kdf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<Vec<Vec<ProtocolInfo>>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx: Option<TxResponse>,
    /// Estimated gas; populated chiefly for `classic_swap_quote`.
    pub gas: Option<u128>,
}

/// Transaction fields returned by `classic_swap_create`, to be signed and
/// broadcast by EVM coin support (the handler itself does not sign — RP5).
#[derive(Debug, Serialize)]
pub struct TxResponse {
    pub from: String,
    pub to: String,
    /// 0x-prefixed hex call data.
    pub data: String,
    /// Native value in coin units.
    pub value: BigDecimal,
    /// Gas price in Gwei.
    pub gas_price: BigDecimal,
    /// Gas limit.
    pub gas: u128,
}

/// `classic_swap_liquidity_sources` response.
#[derive(Serialize)]
pub struct ClassicSwapLiquiditySourcesResponse {
    pub protocols: Vec<ProtocolImage>,
}

/// `classic_swap_tokens` response.
#[derive(Serialize)]
pub struct ClassicSwapTokensResponse {
    pub tokens: HashMap<String, TokenInfo>,
}
