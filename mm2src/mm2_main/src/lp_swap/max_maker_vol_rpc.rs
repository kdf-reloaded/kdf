use coins::request_fee_scope::with_request_fee_scope;
use coins::{lp_coinfind_or_err, CoinFindError, FeeCapBelowBaseFee};
use common::mm_number::MmNumberMultiRepr;
use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

use super::check_balance::CheckBalanceError;
use super::maker_swap::{get_max_maker_vol, CoinVolumeInfo};

#[derive(Deserialize)]
pub struct MaxMakerVolRequest {
    coin: String,
}

#[derive(Debug, Serialize)]
pub struct MaxMakerVolResponse {
    coin: String,
    volume: MmNumberMultiRepr,
    balance: MmNumberMultiRepr,
    locked_by_swaps: MmNumberMultiRepr,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MaxMakerVolRpcError {
    #[display(
        fmt = "Not enough {} for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBalance {
        coin: String,
        available: String,
        required: String,
        locked_by_swaps: Option<String>,
    },
    #[display(
        fmt = "Not enough base coin {} balance for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBaseCoinBalance {
        coin: String,
        available: String,
        required: String,
        locked_by_swaps: Option<String>,
    },
    #[display(
        fmt = "The volume {} of the {} coin less than minimum transaction amount {}",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: String,
        threshold: String,
    },
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    /// An EVM node rejected an estimation or a transaction for a fee cap below the block base fee (CRD ch.56 R5).
    #[display(fmt = "{}", _0)]
    FeeCapBelowBaseFee(FeeCapBelowBaseFee),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}

impl HttpStatusCode for MaxMakerVolRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            MaxMakerVolRpcError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            MaxMakerVolRpcError::NotSufficientBalance { .. }
            | MaxMakerVolRpcError::NotSufficientBaseCoinBalance { .. }
            | MaxMakerVolRpcError::VolumeTooLow { .. } => StatusCode::BAD_REQUEST,
            // Like `Transport`: a node's verdict on a fee the daemon chose, not a fault of the request.
            MaxMakerVolRpcError::Transport(_)
            | MaxMakerVolRpcError::FeeCapBelowBaseFee(_)
            | MaxMakerVolRpcError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for MaxMakerVolRpcError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => MaxMakerVolRpcError::NoSuchCoin { coin },
        }
    }
}

impl From<CheckBalanceError> for MaxMakerVolRpcError {
    fn from(e: CheckBalanceError) -> Self {
        match e {
            CheckBalanceError::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            } => MaxMakerVolRpcError::NotSufficientBalance {
                coin,
                available: available.to_string(),
                required: required.to_string(),
                locked_by_swaps: locked_by_swaps.map(|v| v.to_string()),
            },
            CheckBalanceError::NotSufficientBaseCoinBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            } => MaxMakerVolRpcError::NotSufficientBaseCoinBalance {
                coin,
                available: available.to_string(),
                required: required.to_string(),
                locked_by_swaps: locked_by_swaps.map(|v| v.to_string()),
            },
            CheckBalanceError::VolumeTooLow {
                coin,
                volume,
                threshold,
            } => MaxMakerVolRpcError::VolumeTooLow {
                coin,
                volume: volume.to_string(),
                threshold: threshold.to_string(),
            },
            CheckBalanceError::Transport(e) => MaxMakerVolRpcError::Transport(e),
            CheckBalanceError::FeeCapBelowBaseFee(rejection) => MaxMakerVolRpcError::FeeCapBelowBaseFee(rejection),
            CheckBalanceError::InternalError(e) => MaxMakerVolRpcError::InternalError(e),
        }
    }
}

pub async fn max_maker_vol(
    ctx: MmArc,
    req: MaxMakerVolRequest,
) -> Result<MaxMakerVolResponse, MmError<MaxMakerVolRpcError>> {
    let scope_ctx = ctx.clone();
    with_request_fee_scope(&scope_ctx, max_maker_vol_impl(ctx, req)).await
}

async fn max_maker_vol_impl(
    ctx: MmArc,
    req: MaxMakerVolRequest,
) -> Result<MaxMakerVolResponse, MmError<MaxMakerVolRpcError>> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let CoinVolumeInfo {
        volume,
        balance,
        locked_by_swaps,
    } = get_max_maker_vol(&ctx, &coin).await.mm_err(Into::into)?;

    Ok(MaxMakerVolResponse {
        coin: req.coin,
        volume: MmNumberMultiRepr::from(volume),
        balance: MmNumberMultiRepr::from(balance),
        locked_by_swaps: MmNumberMultiRepr::from(locked_by_swaps),
    })
}
