//! TRON coin activation (P10.2 wiring).
//!
//! Builds an [`EthCoin`] backed by [`EthCoinType::Tron`] (or
//! [`EthCoinType::Trc20`]) with a populated [`TronApiClient`]. The TRON code
//! paths (withdraw, fees, balance) bypass the EVM `web3` field entirely, so we
//! still construct an alloy provider over the TRON URLs purely as a
//! placeholder — it is never invoked.
//!
//! TRON activation binds `swap_contract_address` (R-A3) when present, making
//! the coin version-1 atomic-swap capable; when the field is missing the coin
//! falls back to the zero address and stays wallet-only (withdraw / balance).

use super::api::TronApiClient;
use super::{Network, TronAddress, TRX_DECIMALS};

use crate::eth::{rpc_event_handlers_for_eth_transport, EthCoin, EthCoinImpl, EthCoinType, EthGasLimitV2, EthSigner,
                 SwapGasFeePolicy, ETH_GAS_STATION_DECIMALS};
use crate::{CoinProtocol, DerivationMethod, HistorySyncState};

use ethereum_types::Address;
use mm2_core::mm_ctx::MmArc;
use mm2_eth::keys::KeyPair;
use serde_json::{self as json, Value as Json};
use std::str::FromStr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// TRC20 contracts are 21 raw bytes (0x41 prefix + 20-byte EVM address) when
/// expressed in TRON's native hex form. Both Base58Check and hex inputs are
/// accepted to mirror the rest of the TRON pipeline.
fn parse_trc20_contract(raw: &str) -> Result<Address, String> {
    let trim = raw.trim();
    if trim.is_empty() {
        return Err("TRC20 contract_address is empty".to_owned());
    }
    let tron = if trim.starts_with("0x") || trim.starts_with("41") {
        TronAddress::from_hex(trim).map_err(|e| format!("Invalid TRC20 hex address: {e}"))?
    } else {
        TronAddress::from_base58(trim).map_err(|e| format!("Invalid TRC20 base58 address: {e}"))?
    };
    let evm = tron.to_evm_address();
    if evm.is_zero() {
        return Err("TRC20 contract_address cannot be zero".to_owned());
    }
    Ok(evm)
}

/// R-A3 / R-AD1: parse the version-1 swap-contract address. Accepts base58check
/// (`T...`) and hex (`0x41...` / `41...`) forms, converting to the internal
/// 20-byte EVM payload, and rejects the zero address.
fn parse_tron_swap_contract(raw: &str) -> Result<Address, String> {
    let trim = raw.trim();
    let tron = if trim.starts_with("0x") || trim.starts_with("41") {
        TronAddress::from_hex(trim).map_err(|e| format!("Invalid swap_contract_address hex: {e}"))?
    } else {
        TronAddress::from_base58(trim).map_err(|e| format!("Invalid swap_contract_address base58: {e}"))?
    };
    let evm = tron.to_evm_address();
    if evm.is_zero() {
        return Err("swap_contract_address cannot be zero".to_owned());
    }
    Ok(evm)
}

/// Network-aware default decimals for TRX. TRC20 decimals must come from the
/// coin config; we error out if the operator forgot them.
fn resolve_trc20_decimals(conf: &Json, ticker: &str) -> Result<u8, String> {
    match conf["decimals"].as_u64() {
        Some(d) if (1..=255).contains(&d) => Ok(d as u8),
        _ => Err(format!(
            "TRC20 coin '{ticker}' must declare 'decimals' in its coin config"
        )),
    }
}

/// Assemble the [`EthCoinImpl`] for a TRX or TRC20 coin and wrap it in
/// [`EthCoin`].
pub async fn tron_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    priv_key: &[u8],
    protocol: CoinProtocol,
) -> Result<EthCoin, String> {
    let urls: Vec<String> = match json::from_value(req["urls"].clone()) {
        Ok(v) => v,
        Err(e) => return Err(format!("Failed to parse 'urls' for TRON activation: {e}")),
    };
    if urls.is_empty() {
        return Err("Enable request for TRON coin must have at least 1 node URL".to_owned());
    }

    // Validate URLs eagerly so misconfiguration surfaces at activation time.
    for url in &urls {
        if http::Uri::from_str(url).is_err() {
            return Err(format!("TRON node URL '{url}' is not a valid URI"));
        }
    }

    let (coin_type, decimals, network) = match protocol {
        CoinProtocol::TRX { network } => (EthCoinType::Tron, TRX_DECIMALS, network),
        CoinProtocol::TRC20 {
            platform,
            contract_address,
        } => {
            let token_addr = parse_trc20_contract(&contract_address)?;
            let decimals = resolve_trc20_decimals(conf, ticker)?;
            (
                EthCoinType::Trc20 { platform, token_addr },
                decimals,
                Network::default(),
            )
        },
        _ => {
            return Err(format!(
                "tron_coin_from_conf_and_request called with non-TRON protocol for {ticker}"
            ))
        },
    };
    // network is captured for future use (per-network defaults / endpoints);
    // current TRON pipeline picks endpoints from `urls` directly.
    let _ = network;

    let key_pair = KeyPair::from_secret_slice(priv_key).map_err(|e| format!("Failed to derive TRON key pair: {e}"))?;
    let my_address = key_pair.address();

    // R-L6: optional indexed contract-event service base URL (the TronGrid-style
    // event indexer, distinct from the bare full node), used for swap
    // spend/refund discovery. When absent the coin stays discovery-incapable and
    // discovery fails with a typed error rather than silently reporting "not
    // found".
    let event_indexer = req["event_indexer_url"]
        .as_str()
        .or_else(|| conf["event_indexer_url"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(url) = &event_indexer {
        if http::Uri::from_str(url).is_err() {
            return Err(format!("TRON event_indexer_url '{url}' is not a valid URI"));
        }
    }

    let tron_api = TronApiClient::new(urls.clone(), event_indexer);

    // Placeholder alloy provider over the TRON URLs. The TRON coin paths
    // never dispatch RPCs through this provider; it exists only to satisfy
    // the shared `EthCoinImpl` shape.
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    let web3 = crate::eth::alloy_compat::build_provider(urls, event_handlers)
        .map_err(|e| format!("Failed to build placeholder alloy provider for TRON: {e}"))?;

    // R-A3: a swap-contract address makes the coin atomic-swap capable. It is
    // supplied in base58check (`T...`) or hex (`0x41...` / `41...`) form and is
    // converted to the internal 20-byte EVM payload. When absent the coin
    // stays wallet-only (the zero address is refused by the swap pipeline).
    let swap_contract_address = match req["swap_contract_address"].as_str() {
        Some(raw) if !raw.trim().is_empty() => parse_tron_swap_contract(raw)?,
        _ => Address::default(),
    };

    let required_confirmations = req["required_confirmations"]
        .as_u64()
        .unwrap_or_else(|| conf["required_confirmations"].as_u64().unwrap_or(1))
        .into();

    let initial_history_state = if req["tx_history"].as_bool().unwrap_or(false) {
        HistorySyncState::NotStarted
    } else {
        HistorySyncState::NotEnabled
    };

    let coin = EthCoinImpl {
        ticker: ticker.into(),
        coin_type,
        signer: EthSigner::Local(key_pair),
        my_address,
        sign_message_prefix: json::from_value(conf["sign_message_prefix"].clone()).unwrap_or(None),
        // R-A3: Tron-deployed version-1 swap contract (20-byte EVM payload), or
        // the zero address when the coin is activated wallet-only. Swap paths
        // reject the zero address defensively (R-D1).
        swap_contract_address,
        fallback_swap_contract: None,
        web3,
        web3_instances: Vec::new(),
        decimals,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        gas_station_policy: Default::default(),
        history_sync_state: Mutex::new(initial_history_state),
        required_confirmations: AtomicU64::new(required_confirmations),
        ctx: ctx.weak(),
        chain_id: conf["chain_id"].as_u64(),
        logs_block_range: 0,
        derivation_method: DerivationMethod::Iguana(my_address),
        swap_v2_contracts: None,
        gas_limit_v2: EthGasLimitV2::default(),
        tron_api: Some(tron_api),
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
    };
    Ok(EthCoin(Arc::new(coin)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn priv_key() -> [u8; 32] {
        // Deterministic non-zero key (do not use in production).
        let mut k = [0u8; 32];
        k[31] = 1;
        k
    }

    #[test]
    fn parses_base58_trc20_contract() {
        // USDT-TRC20 mainnet contract.
        let evm = parse_trc20_contract("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").expect("parse");
        assert!(!evm.is_zero());
    }

    #[test]
    fn parses_hex_trc20_contract() {
        let evm = parse_trc20_contract("41a614f803b6fd780986a42c78ec9c7f77e6ded13c").expect("parse");
        assert!(!evm.is_zero());
    }

    #[test]
    fn rejects_zero_trc20_contract() {
        let err = parse_trc20_contract("410000000000000000000000000000000000000000").unwrap_err();
        assert!(err.contains("zero"));
    }

    #[test]
    fn rejects_empty_trc20_contract() {
        assert!(parse_trc20_contract("   ").is_err());
    }

    #[test]
    fn rejects_garbage_trc20_contract() {
        assert!(parse_trc20_contract("not-an-address").is_err());
    }

    #[test]
    fn requires_decimals_for_trc20() {
        let conf = json::json!({});
        assert!(resolve_trc20_decimals(&conf, "USDT").is_err());
        let conf = json::json!({"decimals": 0});
        assert!(resolve_trc20_decimals(&conf, "USDT").is_err());
        let conf = json::json!({"decimals": 6});
        assert_eq!(resolve_trc20_decimals(&conf, "USDT").unwrap(), 6);
    }

    #[tokio::test]
    async fn rejects_empty_urls() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": []});
        let err = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .unwrap_err();
        assert!(err.contains("at least 1 node URL"));
    }

    #[tokio::test]
    async fn rejects_invalid_url() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": ["not a url"]});
        let err = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .unwrap_err();
        assert!(err.contains("not a valid URI"));
    }

    #[tokio::test]
    async fn builds_native_trx_coin_with_tron_api() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": ["https://api.trongrid.io"]});
        let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .expect("build");
        assert!(matches!(coin.coin_type, EthCoinType::Tron));
        assert_eq!(coin.decimals, TRX_DECIMALS);
        assert!(
            coin.tron_api.is_some(),
            "tron_api must be populated for TRON activation"
        );
    }

    #[tokio::test]
    async fn builds_trc20_coin_with_token_addr() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({"decimals": 6});
        let req = json::json!({"urls": ["https://api.trongrid.io"]});
        let coin = tron_coin_from_conf_and_request(&ctx, "USDT-TRC20", &conf, &req, &priv_key(), CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".to_owned(),
        })
        .await
        .expect("build");
        match &coin.coin_type {
            EthCoinType::Trc20 { platform, token_addr } => {
                assert_eq!(platform, "TRX");
                assert!(!token_addr.is_zero());
            },
            other => panic!("expected Trc20, got {other:?}"),
        }
        assert_eq!(coin.decimals, 6);
        assert!(coin.tron_api.is_some());
    }

    #[tokio::test]
    async fn rejects_trc20_without_decimals() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": ["https://api.trongrid.io"]});
        let err = tron_coin_from_conf_and_request(&ctx, "USDT-TRC20", &conf, &req, &priv_key(), CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".to_owned(),
        })
        .await
        .unwrap_err();
        assert!(err.contains("decimals"));
    }

    #[test]
    fn parses_swap_contract_base58_and_hex() {
        // R-A3 / R-AD1: both display and wire forms resolve to the same payload.
        let from_b58 = parse_tron_swap_contract("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").expect("base58");
        let from_hex = parse_tron_swap_contract("41a614f803b6fd780986a42c78ec9c7f77e6ded13c").expect("hex");
        assert!(!from_b58.is_zero());
        assert_eq!(from_b58, from_hex);
    }

    #[test]
    fn rejects_zero_swap_contract() {
        let err = parse_tron_swap_contract("410000000000000000000000000000000000000000").unwrap_err();
        assert!(err.contains("zero"));
    }

    #[tokio::test]
    async fn native_trx_without_swap_contract_is_wallet_only() {
        // R-A3 / R-D1: no swap_contract_address => zero address (wallet-only).
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": ["https://api.trongrid.io"]});
        let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .expect("build");
        assert!(coin.swap_contract_address.is_zero());
    }

    #[tokio::test]
    async fn native_trx_binds_swap_contract_when_present() {
        // R-A3: a supplied swap_contract_address makes the coin swap-capable.
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({
            "urls": ["https://api.trongrid.io"],
            "swap_contract_address": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
        });
        let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .expect("build");
        let expected = parse_tron_swap_contract("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").unwrap();
        assert_eq!(coin.swap_contract_address, expected);
        assert!(!coin.swap_contract_address.is_zero());
    }
}
