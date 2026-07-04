//! Top-level (unnamespaced) EVM ERC-20 allowance RPC handlers.
//!
//! `get_token_allowance` reads the current ERC-20 allowance an EVM coin has
//! granted to a spender (e.g. the 1inch aggregation router) and returns it in
//! coin units. `approve_token` broadcasts an ERC-20 `approve` raising that
//! allowance to the requested coin-unit amount and returns the transaction
//! hash. See CRD chapter 23 §23.8A.4.

use bigdecimal::BigDecimal;
use common::HttpStatusCode;
use derive_more::Display;
use futures::compat::Future01CompatExt;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

use crate::eth::{u256_to_big_decimal, valid_addr_from_str, wei_from_big_decimal, BytesJson, EthCoin};
use crate::{lp_coinfind_or_err, CoinFindError, MmCoinEnum};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetTokenAllowanceRequest {
    /// EVM coin ticker.
    pub coin: String,
    /// 0x-prefixed spender address (e.g. the aggregation router).
    pub spender: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveTokenRequest {
    /// EVM coin ticker.
    pub coin: String,
    /// 0x-prefixed spender address.
    pub spender: String,
    /// Allowance to set, in `coin` units (with fraction).
    pub amount: BigDecimal,
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum Erc20AllowanceError {
    #[display(fmt = "No such coin {}", _0)]
    NoSuchCoin(String),
    #[display(fmt = "Coin {} is not an EVM coin", _0)]
    CoinNotSupported(String),
    #[display(fmt = "Invalid address: {}", _0)]
    InvalidAddress(String),
    #[display(fmt = "Numeric conversion error: {}", _0)]
    NumConversion(String),
    #[display(fmt = "Transaction error: {}", _0)]
    TransactionError(String),
    #[display(fmt = "EVM RPC error: {}", _0)]
    Transport(String),
}

impl HttpStatusCode for Erc20AllowanceError {
    fn status_code(&self) -> StatusCode {
        match self {
            // NOTE (status divergence, §23.8A.4): an unknown coin ticker is a
            // 400 on these handlers, not the 404 used by the classic-swap
            // handlers. Preserve this per-surface difference.
            Erc20AllowanceError::NoSuchCoin(_)
            | Erc20AllowanceError::CoinNotSupported(_)
            | Erc20AllowanceError::InvalidAddress(_)
            | Erc20AllowanceError::NumConversion(_) => StatusCode::BAD_REQUEST,
            Erc20AllowanceError::TransactionError(_) | Erc20AllowanceError::Transport(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

impl From<CoinFindError> for Erc20AllowanceError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => Erc20AllowanceError::NoSuchCoin(coin),
        }
    }
}

/// Resolve a ticker to an active EVM coin, rejecting unknown and non-EVM coins.
async fn eth_coin_from_ticker(ctx: &MmArc, ticker: &str) -> MmResult<EthCoin, Erc20AllowanceError> {
    match lp_coinfind_or_err(ctx, ticker).await.map_mm_err()? {
        MmCoinEnum::EthCoin(eth) => Ok(eth),
        _ => MmError::err(Erc20AllowanceError::CoinNotSupported(ticker.to_owned())),
    }
}

pub async fn get_token_allowance(
    ctx: MmArc,
    req: GetTokenAllowanceRequest,
) -> MmResult<BigDecimal, Erc20AllowanceError> {
    let coin = eth_coin_from_ticker(&ctx, &req.coin).await?;
    let spender = valid_addr_from_str(&req.spender).map_to_mm(Erc20AllowanceError::InvalidAddress)?;

    let wei = coin
        .allowance(spender)
        .compat()
        .await
        .mm_err(|e| Erc20AllowanceError::Transport(e.to_string()))?;

    u256_to_big_decimal(wei, coin.decimals).mm_err(|e| Erc20AllowanceError::NumConversion(e.to_string()))
}

pub async fn approve_token(ctx: MmArc, req: ApproveTokenRequest) -> MmResult<String, Erc20AllowanceError> {
    let coin = eth_coin_from_ticker(&ctx, &req.coin).await?;
    let spender = valid_addr_from_str(&req.spender).map_to_mm(Erc20AllowanceError::InvalidAddress)?;

    let amount = wei_from_big_decimal(&req.amount, coin.decimals)
        .mm_err(|e| Erc20AllowanceError::NumConversion(e.to_string()))?;

    let tx = coin
        .approve(spender, amount)
        .compat()
        .await
        .map_to_mm(|e| Erc20AllowanceError::TransactionError(e.get_plain_text_format()))?;

    Ok(format!("0x{:02x}", BytesJson(tx.hash.as_bytes().to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_token_allowance_request_deserializes() {
        let req: GetTokenAllowanceRequest =
            serde_json::from_str(r#"{"coin":"ETH","spender":"0x111111125421ca6dc452d289314280a0f8842a65"}"#).unwrap();
        assert_eq!(req.coin, "ETH");
        assert_eq!(req.spender, "0x111111125421ca6dc452d289314280a0f8842a65");
    }

    #[test]
    fn get_token_allowance_request_rejects_unknown_field() {
        let res: Result<GetTokenAllowanceRequest, _> =
            serde_json::from_str(r#"{"coin":"ETH","spender":"0x111111125421ca6dc452d289314280a0f8842a65","extra":1}"#);
        assert!(res.is_err());
    }

    #[test]
    fn approve_token_request_deserializes_decimal_amount() {
        let req: ApproveTokenRequest = serde_json::from_str(
            r#"{"coin":"ETH","spender":"0x111111125421ca6dc452d289314280a0f8842a65","amount":"1.5"}"#,
        )
        .unwrap();
        assert_eq!(req.coin, "ETH");
        assert_eq!(req.amount, "1.5".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn approve_token_request_rejects_unknown_field() {
        let res: Result<ApproveTokenRequest, _> =
            serde_json::from_str(r#"{"coin":"ETH","spender":"0x0","amount":"1","extra":true}"#);
        assert!(res.is_err());
    }

    #[test]
    fn approve_token_request_requires_amount() {
        let res: Result<ApproveTokenRequest, _> = serde_json::from_str(r#"{"coin":"ETH","spender":"0x0"}"#);
        assert!(res.is_err());
    }

    #[test]
    fn error_status_mapping() {
        use http::StatusCode;

        // 400 conditions.
        assert_eq!(
            Erc20AllowanceError::NoSuchCoin("ETH".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Erc20AllowanceError::CoinNotSupported("KMD".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Erc20AllowanceError::InvalidAddress("bad".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Erc20AllowanceError::NumConversion("overflow".into()).status_code(),
            StatusCode::BAD_REQUEST
        );

        // 500 conditions.
        assert_eq!(
            Erc20AllowanceError::TransactionError("reverted".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            Erc20AllowanceError::Transport("timeout".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn allowance_conversion_raw_to_coin_units() {
        use ethereum_types::U256;

        // get_token_allowance converts the raw U256 allowance to coin units.
        let raw = U256::from_dec_str("1500000000000000000").unwrap();
        let coin_units = u256_to_big_decimal(raw, 18).unwrap();
        assert_eq!(coin_units, "1.5".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn approve_conversion_coin_units_to_raw() {
        use ethereum_types::U256;

        // approve_token converts the coin-unit amount to a raw U256 via decimals.
        let amount = "1.5".parse::<BigDecimal>().unwrap();
        let raw = wei_from_big_decimal(&amount, 18).unwrap();
        assert_eq!(raw, U256::from_dec_str("1500000000000000000").unwrap());

        // A 6-decimal token (e.g. USDC-style) rounds to the token's precision.
        let amount = "2.000001".parse::<BigDecimal>().unwrap();
        let raw = wei_from_big_decimal(&amount, 6).unwrap();
        assert_eq!(raw, U256::from_dec_str("2000001").unwrap());
    }
}
