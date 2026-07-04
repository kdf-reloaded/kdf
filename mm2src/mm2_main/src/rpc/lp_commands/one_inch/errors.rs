//! RPC-layer error envelope for the `experimental::1inch_v6_0::classic_swap_*`
//! handlers. The trading-API library deliberately keeps the `HttpStatusCode`
//! mapping out (CRD §23.5 R7 / §23.8A.4 RP3); it lives here.

use common::HttpStatusCode;
use derive_more::Display;
use ethereum_types::U256;
use http::StatusCode;
use trading_api::one_inch_api::errors::ApiClientError;

use coins::CoinFindError;

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum OneInchClassicSwapError {
    #[display(fmt = "No such coin {}", _0)]
    NoSuchCoin(String),
    #[display(fmt = "Coin {} is not supported by the 1inch provider", _0)]
    CoinTypeError(String),
    #[display(fmt = "Chain {} is not supported by the 1inch provider", _0)]
    ChainNotSupported(u64),
    #[display(fmt = "Coins {} and {} are not on the same chain", _0, _1)]
    DifferentChains(String, String),
    #[display(fmt = "Invalid address: {}", _0)]
    InvalidAddress(String),
    #[display(fmt = "Invalid parameter: {}", _0)]
    InvalidParam(String),
    #[display(fmt = "Parameter {param} out of bounds, value: {value}, min: {min} max: {max}")]
    OutOfBounds {
        param: String,
        value: String,
        min: String,
        max: String,
    },
    #[display(fmt = "Numeric conversion error: {}", _0)]
    NumConversion(String),
    #[display(fmt = "Allowance not enough, needed: {amount} allowance: {allowance}")]
    AllowanceNotEnough {
        /// Allowance currently held by the router contract.
        allowance: U256,
        /// Allowance the router still needs granted before the swap can run.
        amount: U256,
    },
    #[display(fmt = "1inch provider error: {}", _0)]
    OneInchProviderError(String),
    #[display(fmt = "Provider data error: {}", _0)]
    ApiDataError(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}

impl HttpStatusCode for OneInchClassicSwapError {
    fn status_code(&self) -> StatusCode {
        match self {
            // NOTE (status divergence, §23.8A.4): an unknown / not-activated
            // coin ticker is a 404 on the classic-swap handlers, but a 400 on
            // the get_token_allowance / approve_token handlers. Preserve the
            // per-surface difference for GUI compatibility.
            OneInchClassicSwapError::NoSuchCoin(_) => StatusCode::NOT_FOUND,
            OneInchClassicSwapError::CoinTypeError(_)
            | OneInchClassicSwapError::ChainNotSupported(_)
            | OneInchClassicSwapError::DifferentChains(_, _)
            | OneInchClassicSwapError::InvalidAddress(_)
            | OneInchClassicSwapError::InvalidParam(_)
            | OneInchClassicSwapError::OutOfBounds { .. }
            | OneInchClassicSwapError::NumConversion(_)
            | OneInchClassicSwapError::AllowanceNotEnough { .. } => StatusCode::BAD_REQUEST,
            OneInchClassicSwapError::OneInchProviderError(_) | OneInchClassicSwapError::ApiDataError(_) => {
                StatusCode::BAD_GATEWAY
            },
            OneInchClassicSwapError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for OneInchClassicSwapError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => OneInchClassicSwapError::NoSuchCoin(coin),
        }
    }
}

impl From<ApiClientError> for OneInchClassicSwapError {
    fn from(e: ApiClientError) -> Self {
        match e {
            ApiClientError::InvalidParam(s) => OneInchClassicSwapError::InvalidParam(s),
            ApiClientError::OutOfBounds { param, value, min, max } => {
                OneInchClassicSwapError::OutOfBounds { param, value, min, max }
            },
            ApiClientError::TransportError(e) => OneInchClassicSwapError::OneInchProviderError(e.to_string()),
            ApiClientError::ParseBodyError { error_msg } => OneInchClassicSwapError::ApiDataError(error_msg),
            ApiClientError::GeneralApiError {
                error_msg,
                description,
                status_code,
            } => OneInchClassicSwapError::OneInchProviderError(format!("{error_msg} ({status_code}): {description}")),
            ApiClientError::AllowanceNotEnough { allowance, amount, .. } => {
                // The library carries these as its own (newer) ethereum-types
                // `U256`; promote them into the EVM-coin-support `U256` (R6) via
                // their decimal representation. Both are 256-bit, so the
                // round-trip is lossless.
                OneInchClassicSwapError::AllowanceNotEnough {
                    allowance: U256::from_dec_str(&allowance.to_string()).unwrap_or_default(),
                    amount: U256::from_dec_str(&amount.to_string()).unwrap_or_default(),
                }
            },
        }
    }
}
