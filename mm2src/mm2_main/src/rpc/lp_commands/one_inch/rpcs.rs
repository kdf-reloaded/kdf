//! The five `experimental::1inch_v6_0::classic_swap_*` JSON-RPC v2 handlers
//! (CRD §23.8A). These wire the trading-API 1inch library to the public RPC
//! surface; the library stays handler-free and coin-free (RP5 / RP4).

use coins::eth::{checksum_address, u256_to_big_decimal, wei_from_big_decimal, wei_to_gwei_decimal, EthCoin};
use coins::{lp_coinfind_or_err, MarketCoinOps, MmCoin, MmCoinEnum};
use common::mm_number::{BigDecimal, MmNumber};
use ethereum_types::{Address, U256};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use trading_api::one_inch_api::classic_swap_types::{ClassicSwapCreateParams, ClassicSwapData, ClassicSwapQuoteParams,
                                                    ProtocolsResponse, TokensResponse, TxFields};
use trading_api::one_inch_api::client::{ApiClient, SwapApiMethods, SwapUrlBuilder};

use super::errors::OneInchClassicSwapError;
use super::types::{ClassicSwapContractRequest, ClassicSwapCreateRequest, ClassicSwapLiquiditySourcesRequest,
                   ClassicSwapLiquiditySourcesResponse, ClassicSwapQuoteRequest, ClassicSwapResponse,
                   ClassicSwapTokensRequest, ClassicSwapTokensResponse, TxResponse};

type SwapResult<T> = MmResult<T, OneInchClassicSwapError>;

/// EVM native-currency precision (wei → coin units). The 1inch `tx.value` is a
/// native-coin amount; §23.8A.4 does not pin its decimals, so the EVM-standard
/// 18 is used.
const NATIVE_COIN_DECIMALS: u8 = 18;

/// `classic_swap_contract` — returns the 1inch v6.0 aggregation-router address.
pub async fn classic_swap_contract(_ctx: MmArc, _req: ClassicSwapContractRequest) -> SwapResult<String> {
    Ok(ApiClient::classic_swap_contract().to_owned())
}

/// `classic_swap_quote` — indicative swap quote (`GET /swap/v6.0/{chainId}/quote`).
pub async fn classic_swap_quote(ctx: MmArc, req: ClassicSwapQuoteRequest) -> SwapResult<ClassicSwapResponse> {
    let base = eth_coin_from_ticker(&ctx, &req.base).await?;
    let rel = eth_coin_from_ticker(&ctx, &req.rel).await?;
    let chain_id = resolve_common_chain(&base, &rel)?;

    let src = one_inch_token_address(&base)?;
    let dst = one_inch_token_address(&rel)?;
    let amount = sell_amount_in_wei(&req.amount, &base)?;

    let mut params = ClassicSwapQuoteParams::new(src, dst, amount);
    params
        .with_fee(req.fee)
        .with_protocols(req.protocols)
        .with_gas_price(req.gas_price)
        .with_complexity_level(req.complexity_level)
        .with_parts(req.parts)
        .with_main_route_parts(req.main_route_parts)
        .with_gas_limit(req.gas_limit)
        .with_include_tokens_info(Some(req.include_tokens_info))
        .with_include_protocols(Some(req.include_protocols))
        .with_include_gas(Some(req.include_gas))
        .with_connector_tokens(req.connector_tokens);

    let query = params.build_query_params().map_mm_err()?;
    let url = SwapUrlBuilder::create_api_url_builder(&ctx, chain_id, SwapApiMethods::ClassicSwapQuote)
        .map_mm_err()?
        .with_query_params(query)
        .build()
        .map_mm_err()?;

    let data: ClassicSwapData = ApiClient::call_api(url).await.map_mm_err()?;
    build_classic_swap_response(data, base.ticker().to_owned(), rel.ticker().to_owned(), rel.decimals())
}

/// `classic_swap_create` — build an executable swap tx (`GET /swap/v6.0/{chainId}/swap`).
/// The returned `tx` fields are handed to EVM coin support to sign and
/// broadcast; this handler neither signs nor broadcasts (RP5).
pub async fn classic_swap_create(ctx: MmArc, req: ClassicSwapCreateRequest) -> SwapResult<ClassicSwapResponse> {
    let base = eth_coin_from_ticker(&ctx, &req.base).await?;
    let rel = eth_coin_from_ticker(&ctx, &req.rel).await?;
    let chain_id = resolve_common_chain(&base, &rel)?;

    let src = one_inch_token_address(&base)?;
    let dst = one_inch_token_address(&rel)?;
    let amount = sell_amount_in_wei(&req.amount, &base)?;
    let from = base.my_address().map_to_mm(OneInchClassicSwapError::InvalidAddress)?;

    let mut params = ClassicSwapCreateParams::new(src, dst, amount, from, req.slippage);
    params
        .with_fee(req.fee)
        .with_protocols(req.protocols)
        .with_gas_price(req.gas_price)
        .with_complexity_level(req.complexity_level)
        .with_parts(req.parts)
        .with_main_route_parts(req.main_route_parts)
        .with_gas_limit(req.gas_limit)
        .with_include_tokens_info(Some(req.include_tokens_info))
        .with_include_protocols(Some(req.include_protocols))
        .with_include_gas(Some(req.include_gas))
        .with_connector_tokens(req.connector_tokens)
        .with_excluded_protocols(req.excluded_protocols)
        .with_permit(req.permit)
        .with_compatibility(req.compatibility)
        .with_receiver(req.receiver)
        .with_referrer(req.referrer)
        .with_disable_estimate(req.disable_estimate)
        .with_allow_partial_fill(req.allow_partial_fill)
        .with_use_permit2(req.use_permit2);

    let query = params.build_query_params().map_mm_err()?;
    let url = SwapUrlBuilder::create_api_url_builder(&ctx, chain_id, SwapApiMethods::ClassicSwapCreate)
        .map_mm_err()?
        .with_query_params(query)
        .build()
        .map_mm_err()?;

    let data: ClassicSwapData = ApiClient::call_api(url).await.map_mm_err()?;
    build_classic_swap_response(data, base.ticker().to_owned(), rel.ticker().to_owned(), rel.decimals())
}

/// `classic_swap_liquidity_sources` — enumerate router protocols.
pub async fn classic_swap_liquidity_sources(
    ctx: MmArc,
    req: ClassicSwapLiquiditySourcesRequest,
) -> SwapResult<ClassicSwapLiquiditySourcesResponse> {
    require_supported_chain(req.chain_id)?;

    let url = SwapUrlBuilder::create_api_url_builder(&ctx, req.chain_id, SwapApiMethods::LiquiditySources)
        .map_mm_err()?
        .build()
        .map_mm_err()?;

    let response: ProtocolsResponse = ApiClient::call_api(url).await.map_mm_err()?;
    Ok(ClassicSwapLiquiditySourcesResponse {
        protocols: response.protocols,
    })
}

/// `classic_swap_tokens` — enumerate supported tokens.
pub async fn classic_swap_tokens(ctx: MmArc, req: ClassicSwapTokensRequest) -> SwapResult<ClassicSwapTokensResponse> {
    require_supported_chain(req.chain_id)?;

    let url = SwapUrlBuilder::create_api_url_builder(&ctx, req.chain_id, SwapApiMethods::Tokens)
        .map_mm_err()?
        .build()
        .map_mm_err()?;

    let response: TokensResponse = ApiClient::call_api(url).await.map_mm_err()?;
    Ok(ClassicSwapTokensResponse {
        tokens: response.tokens,
    })
}

/// Resolve a ticker to an active EVM coin, rejecting unknown (404) and non-EVM
/// (400) coins.
async fn eth_coin_from_ticker(ctx: &MmArc, ticker: &str) -> SwapResult<EthCoin> {
    match lp_coinfind_or_err(ctx, ticker).await.map_mm_err()? {
        MmCoinEnum::EthCoin(eth) => Ok(eth),
        _ => MmError::err(OneInchClassicSwapError::CoinTypeError(ticker.to_owned())),
    }
}

/// Derive the numeric chain id from `base` and require `rel` to resolve to the
/// same supported chain (CRD RP2). Rejection happens before any network call
/// (AC2).
fn resolve_common_chain(base: &EthCoin, rel: &EthCoin) -> SwapResult<u64> {
    let base_chain = base
        .chain_id()
        .or_mm_err(|| OneInchClassicSwapError::InvalidParam(format!("{} has no chain id", base.ticker())))?;
    require_supported_chain(base_chain)?;

    let rel_chain = rel
        .chain_id()
        .or_mm_err(|| OneInchClassicSwapError::InvalidParam(format!("{} has no chain id", rel.ticker())))?;
    if base_chain != rel_chain {
        return MmError::err(OneInchClassicSwapError::DifferentChains(
            base.ticker().to_owned(),
            rel.ticker().to_owned(),
        ));
    }
    Ok(base_chain)
}

/// Reject a chain id outside the 1inch v6.0 supported set before any network
/// call (AC2 / §23.2).
fn require_supported_chain(chain_id: u64) -> SwapResult<()> {
    if ApiClient::is_chain_supported(chain_id) {
        Ok(())
    } else {
        MmError::err(OneInchClassicSwapError::ChainNotSupported(chain_id))
    }
}

/// The token address the provider expects for a coin: the native-asset sentinel
/// for a native EVM coin, the ERC-20 contract address otherwise.
fn one_inch_token_address(coin: &EthCoin) -> SwapResult<String> {
    let addr = coin
        .get_token_address()
        .map_to_mm(OneInchClassicSwapError::CoinTypeError)?;
    if addr == Address::default() {
        Ok(ApiClient::eth_special_contract().to_owned())
    } else {
        Ok(format!("{:#x}", addr))
    }
}

/// Convert the coin-unit sell amount into the wei-denominated decimal string the
/// 1inch param builder expects (never via floating point — R5).
fn sell_amount_in_wei(amount: &MmNumber, base: &EthCoin) -> SwapResult<String> {
    let wei = wei_from_big_decimal(&amount.to_decimal(), base.decimals())
        .mm_err(|e| OneInchClassicSwapError::NumConversion(e.to_string()))?;
    Ok(wei.to_string())
}

/// Build the shared classic-swap response from the library's [`ClassicSwapData`].
fn build_classic_swap_response(
    data: ClassicSwapData,
    src_ticker: String,
    dst_ticker: String,
    rel_decimals: u8,
) -> SwapResult<ClassicSwapResponse> {
    let dst_wei = U256::from_dec_str(&data.dst_amount)
        .map_to_mm(|e| OneInchClassicSwapError::ApiDataError(format!("invalid dst_amount: {e:?}")))?;
    let dst_amount =
        u256_to_big_decimal(dst_wei, rel_decimals).mm_err(|e| OneInchClassicSwapError::NumConversion(e.to_string()))?;

    let tx = match data.tx {
        Some(tx) => Some(build_tx_response(tx)?),
        None => None,
    };

    Ok(ClassicSwapResponse {
        dst_amount: MmNumber::from(dst_amount).into(),
        src_token: data.src_token,
        src_token_kdf: Some(src_ticker),
        dst_token: data.dst_token,
        dst_token_kdf: Some(dst_ticker),
        protocols: data.protocols,
        tx,
        gas: data.gas,
    })
}

/// Convert the library's wei-denominated [`TxFields`] into the coin-unit
/// transaction-fields response.
fn build_tx_response(tx: TxFields) -> SwapResult<TxResponse> {
    let value_wei = U256::from_dec_str(&tx.value)
        .map_to_mm(|e| OneInchClassicSwapError::ApiDataError(format!("invalid tx value: {e:?}")))?;
    let value = u256_to_big_decimal(value_wei, NATIVE_COIN_DECIMALS)
        .mm_err(|e| OneInchClassicSwapError::NumConversion(e.to_string()))?;

    let gas_price_wei = U256::from_dec_str(&tx.gas_price)
        .map_to_mm(|e| OneInchClassicSwapError::ApiDataError(format!("invalid tx gas price: {e:?}")))?;
    let gas_price: BigDecimal =
        wei_to_gwei_decimal(gas_price_wei).mm_err(|e| OneInchClassicSwapError::NumConversion(e.to_string()))?;

    Ok(TxResponse {
        from: checksum_address(&format!("{:#x}", tx.from)),
        to: checksum_address(&format!("{:#x}", tx.to)),
        data: tx.data,
        value,
        gas_price,
        gas: tx.gas,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::HttpStatusCode;
    use http::StatusCode;

    #[test]
    fn quote_request_deserializes_with_defaults() {
        let req: ClassicSwapQuoteRequest =
            serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":"1.5"}"#).unwrap();
        assert_eq!(req.base, "ETH");
        assert_eq!(req.rel, "USDC");
        assert_eq!(req.amount.to_decimal(), "1.5".parse::<BigDecimal>().unwrap());
        // include_* default to true (§23.8A.4).
        assert!(req.include_tokens_info);
        assert!(req.include_protocols);
        assert!(req.include_gas);
    }

    #[test]
    fn quote_request_accepts_numeric_amount() {
        let req: ClassicSwapQuoteRequest = serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":1.5}"#).unwrap();
        assert_eq!(req.amount.to_decimal(), "1.5".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn quote_request_rejects_unknown_field() {
        let res: Result<ClassicSwapQuoteRequest, _> =
            serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":"1","oops":1}"#);
        assert!(res.is_err());
    }

    #[test]
    fn create_request_deserializes_with_create_only_fields() {
        let req: ClassicSwapCreateRequest =
            serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":"1","slippage":1.0,"receiver":"0xabc"}"#)
                .unwrap();
        assert_eq!(req.slippage, 1.0);
        assert_eq!(req.receiver.as_deref(), Some("0xabc"));
    }

    #[test]
    fn create_request_requires_slippage() {
        let res: Result<ClassicSwapCreateRequest, _> =
            serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":"1"}"#);
        assert!(res.is_err());
    }

    #[test]
    fn create_request_rejects_unknown_field() {
        let res: Result<ClassicSwapCreateRequest, _> =
            serde_json::from_str(r#"{"base":"ETH","rel":"USDC","amount":"1","slippage":1.0,"oops":true}"#);
        assert!(res.is_err());
    }

    #[test]
    fn liquidity_sources_request_requires_chain_id() {
        let ok: ClassicSwapLiquiditySourcesRequest = serde_json::from_str(r#"{"chain_id":1}"#).unwrap();
        assert_eq!(ok.chain_id, 1);
        assert!(serde_json::from_str::<ClassicSwapLiquiditySourcesRequest>(r#"{}"#).is_err());
        assert!(serde_json::from_str::<ClassicSwapLiquiditySourcesRequest>(r#"{"chain_id":1,"x":2}"#).is_err());
    }

    #[test]
    fn contract_request_rejects_unknown_field() {
        assert!(serde_json::from_str::<ClassicSwapContractRequest>(r#"{}"#).is_ok());
        assert!(serde_json::from_str::<ClassicSwapContractRequest>(r#"{"chain_id":1}"#).is_err());
    }

    #[test]
    fn unsupported_chain_rejected_before_network() {
        // AC2: an out-of-set chain id is rejected with the invalid-parameter
        // (400) condition before any network call is made.
        let err = require_supported_chain(999_999).unwrap_err().into_inner();
        assert!(matches!(err, OneInchClassicSwapError::ChainNotSupported(999_999)));
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        // A supported chain passes the gate.
        assert!(require_supported_chain(1).is_ok());
    }

    #[test]
    fn unknown_coin_is_404_on_classic_swap_surface() {
        // NOTE (status divergence, §23.8A.4): unknown coin is 404 here but 400
        // on get_token_allowance / approve_token.
        assert_eq!(
            OneInchClassicSwapError::NoSuchCoin("FOO".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        // Non-EVM coin is a 400.
        assert_eq!(
            OneInchClassicSwapError::CoinTypeError("KMD".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn allowance_not_enough_is_400_with_payload_shape() {
        let err = OneInchClassicSwapError::AllowanceNotEnough {
            allowance: U256::from(7u64),
            amount: U256::from(42u64),
        };
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);

        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error_type"], "AllowanceNotEnough");
        // error_data carries `allowance` (current) and `amount` (required).
        assert!(json["error_data"].get("allowance").is_some());
        assert!(json["error_data"].get("amount").is_some());
    }

    #[test]
    fn out_of_bounds_payload_shape() {
        let err = OneInchClassicSwapError::OutOfBounds {
            param: "slippage".into(),
            value: "60".into(),
            min: "0".into(),
            max: "50".into(),
        };
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error_type"], "OutOfBounds");
        assert_eq!(json["error_data"]["param"], "slippage");
        assert_eq!(json["error_data"]["value"], "60");
        assert_eq!(json["error_data"]["min"], "0");
        assert_eq!(json["error_data"]["max"], "50");
    }

    #[test]
    fn provider_errors_map_to_expected_status() {
        use trading_api::one_inch_api::errors::ApiClientError;

        // Out-of-bounds from the param builder → 400.
        let e: OneInchClassicSwapError = ApiClientError::OutOfBounds {
            param: "fee".into(),
            value: "5".into(),
            min: "0".into(),
            max: "3".into(),
        }
        .into();
        assert_eq!(e.status_code(), StatusCode::BAD_REQUEST);

        // Allowance shortfall → 400 carrying both figures.
        let e: OneInchClassicSwapError = ApiClientError::AllowanceNotEnough {
            error_msg: "allowance is not enough".into(),
            description: String::new(),
            status_code: 400,
            // `.into()` infers the library's ethereum-types `U256` flavour.
            amount: 100u64.into(),
            allowance: 10u64.into(),
        }
        .into();
        match &e {
            OneInchClassicSwapError::AllowanceNotEnough { allowance, amount } => {
                assert_eq!(*allowance, U256::from(10u64));
                assert_eq!(*amount, U256::from(100u64));
            },
            other => panic!("unexpected variant: {other:?}"),
        }
        assert_eq!(e.status_code(), StatusCode::BAD_REQUEST);

        // General provider API error → 502.
        let e: OneInchClassicSwapError = ApiClientError::GeneralApiError {
            error_msg: "boom".into(),
            description: "bad".into(),
            status_code: 500,
        }
        .into();
        assert_eq!(e.status_code(), StatusCode::BAD_GATEWAY);

        // Body-parse / provider-data conversion → 502.
        let e: OneInchClassicSwapError = ApiClientError::ParseBodyError {
            error_msg: "garbage".into(),
        }
        .into();
        assert_eq!(e.status_code(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn tx_response_converts_wei_to_coin_units_and_gwei() {
        // Build TxFields via its own decode path (avoids depending on the
        // library's ethereum-types `Address` flavour at construction).
        let tx: TxFields = serde_json::from_str(
            r#"{
                "from":"0x0000000000000000000000000000000000000000",
                "to":"0x0000000000000000000000000000000000000000",
                "data":"0xdead",
                "value":"1000000000000000000",
                "gasPrice":"5000000000",
                "gas":21000
            }"#,
        )
        .unwrap();
        let resp = build_tx_response(tx).unwrap();
        assert_eq!(resp.value, "1".parse::<BigDecimal>().unwrap()); // 1 ETH
        assert_eq!(resp.gas_price, "5".parse::<BigDecimal>().unwrap()); // 5 Gwei
        assert_eq!(resp.gas, 21000);
        assert_eq!(resp.data, "0xdead");
    }
}
