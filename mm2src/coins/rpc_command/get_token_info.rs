//! Top-level (unnamespaced) `get_token_info` RPC handler (CRD §35.4).
//!
//! Given a token's coin-protocol descriptor (naming its platform and contract
//! address), this reads the ERC-20 contract's on-chain `symbol` and `decimals`
//! over the platform coin's EVM JSON-RPC client, and reports the configured
//! ticker for that contract when one is known.

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

use crate::eth::{addr_from_str, get_token_decimals, get_token_symbol, EthCoin};
use crate::{lp_coinfind_or_err, CoinFindError, CoinProtocol, MmCoinEnum};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetTokenInfoRequest {
    /// Coin-protocol descriptor selecting the token's platform and contract
    /// address (R35.5 shape).
    pub protocol: CoinProtocol,
}

/// On-chain token info, tagged by token kind (R35.4.3).
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "info")]
pub enum TokenInfo {
    #[serde(rename = "ERC20")]
    Erc20 { symbol: String, decimals: u8 },
}

#[derive(Debug, Serialize)]
pub struct GetTokenInfoResponse {
    /// The ticker under which this contract is known in coin configuration,
    /// when one is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_ticker: Option<String>,
    #[serde(flatten)]
    pub info: TokenInfo,
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetTokenInfoError {
    #[display(fmt = "No such coin {}", _0)]
    NoSuchCoin(String),
    #[display(fmt = "Unsupported token protocol for platform {}", _0)]
    UnsupportedTokenProtocol(String),
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Could not retrieve token info: {}", _0)]
    RetrieveInfoError(String),
}

impl HttpStatusCode for GetTokenInfoError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetTokenInfoError::NoSuchCoin(_) => StatusCode::NOT_FOUND,
            GetTokenInfoError::UnsupportedTokenProtocol(_) | GetTokenInfoError::InvalidRequest(_) => {
                StatusCode::BAD_REQUEST
            },
            GetTokenInfoError::RetrieveInfoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for GetTokenInfoError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => GetTokenInfoError::NoSuchCoin(coin),
        }
    }
}

/// Scans coin configuration for an `ERC20` entry on `platform` whose contract
/// address matches `contract_address` (case-insensitive), returning its ticker.
fn config_ticker_for_contract(ctx: &MmArc, platform: &str, contract_address: &str) -> Option<String> {
    let coins = ctx.conf["coins"].as_array()?;
    let wanted = contract_address.to_lowercase();
    for coin in coins {
        if coin["protocol"]["type"].as_str() != Some("ERC20") {
            continue;
        }
        let data = &coin["protocol"]["protocol_data"];
        if data["platform"].as_str() != Some(platform) {
            continue;
        }
        if data["contract_address"].as_str().map(|c| c.to_lowercase()).as_deref() == Some(wanted.as_str()) {
            return coin["coin"].as_str().map(|s| s.to_owned());
        }
    }
    None
}

pub async fn get_token_info(ctx: MmArc, req: GetTokenInfoRequest) -> MmResult<GetTokenInfoResponse, GetTokenInfoError> {
    let (platform, contract_address) = match req.protocol {
        CoinProtocol::ERC20 {
            platform,
            contract_address,
        } => (platform, contract_address),
        proto => {
            return MmError::err(GetTokenInfoError::UnsupportedTokenProtocol(format!("{:?}", proto)));
        },
    };

    let platform_coin = lp_coinfind_or_err(&ctx, &platform).await.map_mm_err()?;
    let platform_coin: EthCoin = match platform_coin {
        MmCoinEnum::EthCoin(eth) => eth,
        _ => return MmError::err(GetTokenInfoError::UnsupportedTokenProtocol(platform)),
    };

    let token_addr = addr_from_str(&contract_address).map_to_mm(GetTokenInfoError::InvalidRequest)?;

    let web3 = platform_coin.alloy_provider();
    let symbol = get_token_symbol(&web3, token_addr)
        .await
        .map_to_mm(GetTokenInfoError::RetrieveInfoError)?;
    let decimals = get_token_decimals(&web3, token_addr)
        .await
        .map_to_mm(GetTokenInfoError::RetrieveInfoError)?;

    let config_ticker = config_ticker_for_contract(&ctx, &platform, &contract_address);

    Ok(GetTokenInfoResponse {
        config_ticker,
        info: TokenInfo::Erc20 { symbol, decimals },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserializes_erc20_protocol() {
        let req: GetTokenInfoRequest = serde_json::from_str(
            r#"{"protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0xdAC17F958D2ee523a2206206994597C13D831ec7"}}}"#,
        )
        .unwrap();
        match req.protocol {
            CoinProtocol::ERC20 {
                platform,
                contract_address,
            } => {
                assert_eq!(platform, "ETH");
                assert_eq!(contract_address, "0xdAC17F958D2ee523a2206206994597C13D831ec7");
            },
            other => panic!("unexpected protocol {:?}", other),
        }
    }

    #[test]
    fn response_serializes_tagged_with_optional_config_ticker() {
        let resp = GetTokenInfoResponse {
            config_ticker: Some("USDT-ERC20".to_owned()),
            info: TokenInfo::Erc20 {
                symbol: "USDT".to_owned(),
                decimals: 6,
            },
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["config_ticker"], "USDT-ERC20");
        assert_eq!(v["type"], "ERC20");
        assert_eq!(v["info"]["symbol"], "USDT");
        assert_eq!(v["info"]["decimals"], 6);
    }

    #[test]
    fn response_omits_config_ticker_when_unknown() {
        let resp = GetTokenInfoResponse {
            config_ticker: None,
            info: TokenInfo::Erc20 {
                symbol: "JST".to_owned(),
                decimals: 8,
            },
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v.get("config_ticker").is_none());
        assert_eq!(v["type"], "ERC20");
    }
}
