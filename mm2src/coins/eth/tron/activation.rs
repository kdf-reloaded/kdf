//! TRON coin activation (P10.2 wiring).
//!
//! Builds an [`EthCoin`] backed by [`EthCoinType::Tron`] (or
//! [`EthCoinType::Trc20`]) with a populated [`TronApiClient`]. The TRON code
//! paths (withdraw, fees, balance) use that client and not the EVM `web3`
//! field. An alloy provider over the TRON URLs is still constructed as a
//! placeholder for the shared coin shape; a TRON node does not serve EVM
//! JSON-RPC, so no TRON path may use it, and the EVM-only RPC methods refuse a
//! TRON coin before reaching it (CRD ch.21 D7).
//!
//! TRON activation deliberately does not parse `swap_contract_address` or
//! V2 contracts because TRON-side atomic swaps are still pending
//! (P10.3.7 / nft_swap_v2). When the field is missing in the request we fall
//! back to the zero address so the coin can be enabled for withdraw / balance
//! flows.

use super::api::TronApiClient;
use super::balance::trc20_decimals;
use super::gasfree::binding::{CoinGasFree, GasFreeBinding, TokenGasless, TronTokenGaslessRequest};
use super::nodes::{parse_nodes, TronApiKey, TronNode};
use super::{Network, TronAddress, TRX_DECIMALS};

use crate::eth::{build_eth_hd_wallet, derive_eth_hd_activation_key, rpc_event_handlers_for_eth_transport,
                 Erc20BuildError, EthCoin, EthCoinBuildError, EthCoinImpl, EthCoinType, EthGasLimit, EthGasLimitV2,
                 EthHDWallet, EthSigner, SwapGasFeePolicy, ETH_GAS_STATION_DECIMALS};
use crate::hd_wallet_storage::HDWalletStorageKeyPolicy;
use crate::rpc_command::get_my_address::PathToAddress;
use crate::{CoinProtocol, DerivationMethod, HistorySyncState, MarketCoinOps};

use common::log::warn;
use crypto::GlobalHDAccountArc;
use ethereum_types::Address;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_eth::keys::KeyPair;
use serde_json::{self as json, Value as Json};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// The swap confirmations of a TRON platform coin: the request value, else the coin configuration's, else 1 (CRD
/// ch.21 R21.10B.2). The mmrpc 2.0 EVM default of 3 does not apply to TRON.
pub fn tron_required_confirmations(requested: Option<u64>, conf: &Json) -> u64 {
    requested.unwrap_or_else(|| conf["required_confirmations"].as_u64().unwrap_or(1))
}

/// TRC20 contracts are 21 raw bytes (0x41 prefix + 20-byte EVM address) when
/// expressed in TRON's native hex form. Both Base58Check and hex inputs are
/// accepted to mirror the rest of the TRON pipeline.
///
/// # Errors
///
/// The text of why `raw` is empty, is neither form of CRD ch.21 R-AD1, or is the zero address.
pub fn parse_trc20_contract(raw: &str) -> Result<Address, String> {
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

/// The decimals of a TRC20 token: the coin config's `decimals` when it is 1 to 255, else (absent or 0) the token's
/// own `decimals()` read through `triggerconstantcontract` (CRD ch.21 R21.10B.2; this replaces the refusal of R-A1
/// step 4, see its status note). `caller` is the address the read is made from.
///
/// # Errors
///
/// [`EthCoinBuildError::InvalidConfig`] for a configured value above 255; [`EthCoinBuildError::Transport`] when
/// the read fails or its value is above 255.
pub(super) async fn resolve_trc20_decimals(
    conf: &Json,
    ticker: &str,
    api: &TronApiClient,
    contract: &TronAddress,
    caller: &TronAddress,
) -> Result<u8, MmError<EthCoinBuildError>> {
    match conf["decimals"].as_u64() {
        Some(d) if (1..=255).contains(&d) => Ok(d as u8),
        Some(d) if d > 255 => MmError::err(EthCoinBuildError::InvalidConfig(format!(
            "TRC20 coin '{ticker}' declares decimals above 255 in its coin config"
        ))),
        _ => trc20_decimals(api, contract, caller)
            .await
            .map_to_mm(|e| EthCoinBuildError::Transport(format!("TRC20 coin '{ticker}': {e}"))),
    }
}

/// What the TRON coin builder takes from an activation request, already parsed.
pub struct TronCoinParams {
    /// The validated node list, in the order given.
    pub nodes: Vec<TronNode>,
    /// The swap confirmations, resolved by [`tron_required_confirmations`].
    pub required_confirmations: u64,
    /// Whether the request asks for the transaction history (`tx_history`, on both activation routes). The history
    /// is enabled only when the node list also has a keyed TronGrid node (CRD ch.21 §21.10C).
    pub tx_history: bool,
    /// The GasFree provider binding of a TRX platform coin (CRD ch.63 §63.3.1), already validated. `None` when the
    /// request gave no `tron_gasless_provider`; the legacy route has no such member.
    pub gasfree: Option<GasFreeBinding>,
}

/// Assemble the [`EthCoinImpl`] for a TRX or TRC20 coin and wrap it in
/// [`EthCoin`]. The legacy `enable` / `electrum` entry; the request is the stringly typed legacy one.
pub async fn tron_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    priv_key: &[u8],
    protocol: CoinProtocol,
) -> Result<EthCoin, String> {
    // Each entry of `urls` is a URL string or an object with the node's own request headers; `tron_api_key`
    // reaches the TronGrid hosts only (CRD ch.21 R-N9). Misconfiguration surfaces here, and no error echoes a
    // configured string.
    let api_key = TronApiKey::from_conf(&ctx.conf).map_err(|e| e.to_string())?;
    let nodes = parse_nodes(&req["urls"], api_key.as_ref()).map_err(|e| e.to_string())?;

    if req["swap_contract_address"].is_string() {
        warn!("TRON coin '{ticker}': swap_contract_address ignored (TRON swaps not yet wired)");
    }
    let params = TronCoinParams {
        nodes,
        required_confirmations: tron_required_confirmations(req["required_confirmations"].as_u64(), conf),
        tx_history: req["tx_history"].as_bool().unwrap_or(false),
        gasfree: None,
    };
    tron_coin_from_params(ctx, ticker, conf, params, priv_key, protocol)
        .await
        .map_err(|e| e.into_inner().to_string())
}

/// Assemble the [`EthCoinImpl`] for a TRX or TRC20 coin that signs with the single key `priv_key`.
///
/// A TRC20 coin whose config has no `decimals` reads them from the contract through the node list (CRD ch.21
/// R21.10B.2).
///
/// # Errors
///
/// [`EthCoinBuildError::InvalidPayload`] for a TRC20 contract address that is not valid;
/// [`EthCoinBuildError::InvalidConfig`] or [`EthCoinBuildError::Transport`] as [`resolve_trc20_decimals`];
/// [`EthCoinBuildError::Internal`] for a key or a provider that cannot be built, or a protocol that is not TRON.
pub async fn tron_coin_from_params(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: TronCoinParams,
    priv_key: &[u8],
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    let key_pair = KeyPair::from_secret_slice(priv_key)
        .map_to_mm(|e| EthCoinBuildError::Internal(format!("Failed to derive TRON key pair: {e}")))?;
    let derivation_method = DerivationMethod::Iguana(key_pair.address());
    build_tron_coin(ctx, ticker, conf, params, key_pair, derivation_method, protocol).await
}

/// Assemble the [`EthCoinImpl`] of the TRX platform coin of a software HD wallet (CRD ch.21 R-A2, R21.10B.4): the
/// coin's configured `derivation_path` (coin type 195 in practice) extended by `path_to_address` selects the enabled
/// address, and the wallet is the same HD wallet an EVM platform has, with `gap_limit`. A TRC20 token is built on
/// the platform and uses its wallet, so this builds a TRX platform only.
///
/// # Errors
///
/// [`EthCoinBuildError::InvalidPayload`] for a coin config without a usable `derivation_path` or a path that cannot
/// be derived; [`EthCoinBuildError::Internal`] for a protocol that is not TRX, a wallet storage that cannot be
/// opened, or a provider that cannot be built.
pub async fn tron_coin_from_params_with_global_hd(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: TronCoinParams,
    global_hd: &GlobalHDAccountArc,
    path_to_address: &PathToAddress,
    gap_limit: u32,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    if !matches!(protocol, CoinProtocol::TRX { .. }) {
        return MmError::err(EthCoinBuildError::Internal(format!(
            "the TRON HD wallet is built for a TRX platform, not for {ticker}"
        )));
    }
    let key_pair = derive_eth_hd_activation_key(global_hd, conf, path_to_address)?;
    let hd_wallet = build_eth_hd_wallet(ctx, ticker, conf, gap_limit, HDWalletStorageKeyPolicy::Software).await?;
    build_tron_coin(
        ctx,
        ticker,
        conf,
        params,
        key_pair,
        DerivationMethod::HDWallet(hd_wallet),
        protocol,
    )
    .await
}

/// The coin builder behind [`tron_coin_from_params`] and [`tron_coin_from_params_with_global_hd`]: `key_pair` is the
/// key of the enabled address, and `derivation_method` the wallet it belongs to.
async fn build_tron_coin(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: TronCoinParams,
    key_pair: KeyPair,
    derivation_method: DerivationMethod<Address, EthHDWallet>,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    let TronCoinParams {
        nodes,
        required_confirmations,
        tx_history,
        gasfree,
    } = params;
    let urls: Vec<String> = nodes.iter().map(|node| node.url().to_owned()).collect();

    let my_address = key_pair.address();

    let tron_api = TronApiClient::new(ticker, nodes);

    let (coin_type, decimals, network) = match protocol {
        CoinProtocol::TRX { network } => (EthCoinType::Tron, TRX_DECIMALS, network),
        CoinProtocol::TRC20 {
            platform,
            contract_address,
        } => {
            // The binding lives on the platform coin (CRD ch.63 §63.3.1); a token built on its own has none.
            if gasfree.is_some() {
                return MmError::err(EthCoinBuildError::Internal(format!(
                    "a GasFree provider binding belongs to a TRX platform coin, not to the token {ticker}"
                )));
            }
            let token_addr = parse_trc20_contract(&contract_address).map_to_mm(EthCoinBuildError::InvalidPayload)?;
            let decimals = resolve_trc20_decimals(
                conf,
                ticker,
                &tron_api,
                &TronAddress::from_evm_address(token_addr),
                &TronAddress::from_evm_address(my_address),
            )
            .await?;
            (
                EthCoinType::Trc20 { platform, token_addr },
                decimals,
                Network::default(),
            )
        },
        _ => {
            return MmError::err(EthCoinBuildError::Internal(format!(
                "tron_coin_from_conf_and_request called with non-TRON protocol for {ticker}"
            )))
        },
    };
    // network is captured for future use (per-network defaults / endpoints);
    // current TRON pipeline picks endpoints from the node list directly.
    let _ = network;

    // Placeholder alloy provider over the TRON URLs, to satisfy the shared `EthCoinImpl` shape. The TRON wallet
    // paths (balance, withdraw, broadcast, current block) use the TRON API client instead, and no TRON path may
    // use this provider: a TRON node does not serve EVM JSON-RPC. The EVM-only RPC methods refuse a TRX or TRC20
    // coin before reaching it (CRD ch.21 D7). Known boundary: the provider is given the plain URL strings only,
    // so a request sent through it carries no headers and no key.
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    let web3 = crate::eth::alloy_compat::build_provider(crate::node_url::listed_nodes(ticker, urls), event_handlers)
        .map_to_mm(|e| {
            EthCoinBuildError::Internal(format!("Failed to build placeholder alloy provider for TRON: {e}"))
        })?;

    // The history needs both the request and a keyed TronGrid node (CRD ch.21 §21.10C); without either the coin
    // keeps the reference behavior of R21.10B.6.
    let initial_history_state = if tx_history && tron_api.has_history_nodes() {
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
        // TRON has no EtomicSwap contract today — leave as zero address. Swap
        // paths reject this defensively until P10.3.7 lands.
        swap_contract_address: Address::default(),
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
        logs_window: Default::default(),
        derivation_method,
        swap_v2_contracts: None,
        // TRON has no V1 or V2 EVM swap and no EVM gas model (energy and bandwidth instead): the gas-limit
        // blocks of CRD ch. 57 do not apply, so the defaults stand.
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        // No EVM estimation on TRON, so no multiplier (CRD R57.5.3b).
        estimate_gas_mult: None,
        taker_fee_quote_at_limit: true,
        tron_api: Some(tron_api),
        tron_gasfree: gasfree.map(CoinGasFree::Platform),
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
    };
    Ok(EthCoin(Arc::new(coin)))
}

impl EthCoin {
    /// Whether this coin is the native TRX coin of a TRON chain (not a TRC20 token).
    pub fn is_tron_platform_coin(&self) -> bool { matches!(self.coin_type, EthCoinType::Tron) }

    /// Builds a TRC20 token coin on top of this activated TRX platform coin: it shares the platform's key, address
    /// and node client (and so its node order), and carries the token's own ticker, contract, decimals and
    /// confirmation requirement. The decimals come from the token's coin config when it declares 1 to 255, and are
    /// read from the contract otherwise (CRD ch.21 R21.10B.2).
    ///
    /// `gasless` is the `gasless` block of the token's activation request (CRD ch.63 §63.3.2). A block with
    /// `enabled: false` behaves as an absent one. An enabled block needs a provider binding on this platform coin;
    /// its `transfer_max_fee` is kept on the token in the token's smallest unit.
    ///
    /// # Errors
    ///
    /// [`Erc20BuildError::InvalidPayload`] for an enabled `gasless` block on a platform without a provider binding
    /// and for a `transfer_max_fee` that is negative or finer than the token's decimals; the first two are refused
    /// before any node is asked;
    /// [`Erc20BuildError::Transport`] when the decimals read fails or gives a value above 255;
    /// [`Erc20BuildError::UnsupportedDecimals`] for a configured value above 255;
    /// [`Erc20BuildError::Internal`] when this is not a TRX platform coin or its context is gone.
    pub async fn trc20_token_from_conf_or_contract(
        &self,
        ticker: String,
        token_addr: Address,
        required_confirmations: u64,
        gasless: Option<&TronTokenGaslessRequest>,
    ) -> Result<EthCoin, MmError<Erc20BuildError>> {
        let (tron_api, EthCoinType::Tron) = (&self.tron_api, &self.coin_type) else {
            return MmError::err(Erc20BuildError::Internal(
                "a TRC20 token needs an activated TRX platform coin".to_owned(),
            ));
        };
        let Some(tron_api) = tron_api else {
            return MmError::err(Erc20BuildError::Internal(
                "the TRX platform coin has no node client".to_owned(),
            ));
        };
        let gasless = gasless.filter(|gasless| gasless.is_enabled());
        if let Some(gasless) = gasless {
            TokenGasless::precheck(gasless, self.gasfree_platform_binding().is_some())
                .map_to_mm(|e| Erc20BuildError::InvalidPayload(e.to_string()))?;
        }
        let ctx = MmArc::from_weak(&self.ctx)
            .or_mm_err(|| Erc20BuildError::Internal("MM context has been dropped".to_owned()))?;
        let conf = crate::coin_conf(&ctx, &ticker);
        let decimals = resolve_trc20_decimals(
            &conf,
            &ticker,
            tron_api,
            &TronAddress::from_evm_address(token_addr),
            &TronAddress::from_evm_address(self.my_address),
        )
        .await
        .mm_err(|e| match e {
            EthCoinBuildError::InvalidConfig(text) => Erc20BuildError::UnsupportedDecimals(text),
            EthCoinBuildError::Transport(text) => Erc20BuildError::Transport(text),
            other => Erc20BuildError::Internal(other.to_string()),
        })?;
        let tron_gasfree = match gasless {
            Some(gasless) => Some(CoinGasFree::Token(
                TokenGasless::from_request(gasless, decimals)
                    .map_to_mm(|e| Erc20BuildError::InvalidPayload(e.to_string()))?,
            )),
            None => None,
        };
        let token_impl = EthCoinImpl {
            signer: self.signer.clone(),
            my_address: self.my_address,
            coin_type: EthCoinType::Trc20 {
                platform: self.ticker().to_string(),
                token_addr,
            },
            sign_message_prefix: self.sign_message_prefix.clone(),
            swap_contract_address: self.swap_contract_address,
            fallback_swap_contract: self.fallback_swap_contract,
            decimals,
            ticker,
            gas_station_url: self.gas_station_url.clone(),
            gas_station_decimals: self.gas_station_decimals,
            gas_station_policy: self.gas_station_policy.clone(),
            web3: self.web3.clone(),
            web3_instances: self.web3_instances.clone(),
            // The platform's history process also writes the history of its tokens (CRD ch.21 §21.10C).
            history_sync_state: Mutex::new(if self.tron_history_enabled() {
                HistorySyncState::NotStarted
            } else {
                HistorySyncState::NotEnabled
            }),
            ctx: self.ctx.clone(),
            required_confirmations: required_confirmations.into(),
            chain_id: self.chain_id,
            logs_block_range: self.logs_block_range,
            logs_window: self.logs_window.clone(),
            derivation_method: DerivationMethod::Iguana(self.my_address),
            swap_v2_contracts: self.swap_v2_contracts,
            gas_limit: self.gas_limit.clone(),
            gas_limit_v2: self.gas_limit_v2.clone(),
            estimate_gas_mult: self.estimate_gas_mult,
            taker_fee_quote_at_limit: self.taker_fee_quote_at_limit,
            tron_api: Some(tron_api.clone()),
            tron_gasfree,
            nft_swap_v2_contract: self.nft_swap_v2_contract,
            swap_gas_fee_policy: Mutex::new(self.swap_gas_fee_policy()),
            erc20_tokens_infos: Default::default(),
            custom_token: false,
        };
        Ok(EthCoin(Arc::new(token_impl)))
    }
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

    /// A 32-byte ABI word holding `value`.
    fn word(value: u64) -> String { format!("{value:064x}") }

    /// A stub node whose `decimals()` read answers `body` with `status`.
    #[cfg(not(target_arch = "wasm32"))]
    fn decimals_node(status: u16, body: String) -> super::super::test_node::StubNode {
        use super::super::test_node::{Reply, StubNode};
        StubNode::start(move |path, _| match path {
            "/wallet/triggerconstantcontract" => Reply::status(status, &body),
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn decimals_answer(value: u64) -> String {
        json::json!({"result": {"result": true}, "constant_result": [word(value)]}).to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_decimals(conf: &Json, node_url: &str) -> Result<u8, EthCoinBuildError> {
        let api = TronApiClient::new("USDT", vec![TronNode::plain(node_url.to_owned())]);
        let contract = TronAddress::from_base58("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").expect("a valid address");
        let caller = TronAddress::from_evm_address(Address::repeat_byte(7));
        common::block_on(resolve_trc20_decimals(conf, "USDT", &api, &contract, &caller)).map_err(MmError::into_inner)
    }

    /// CRD ch.21 R21.10B.2 (T21.10B.3, parity arm): a token config without `decimals`, or with 0, reads the
    /// contract's `decimals()`; a configured value of 1 to 255 is used as it is, with no node request.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn trc20_decimals_come_from_the_config_or_from_the_contract() {
        let node = decimals_node(200, decimals_answer(6));
        for conf in [
            json::json!({}),
            json::json!({"decimals": 0}),
            json::json!({"decimals": null}),
        ] {
            assert_eq!(read_decimals(&conf, &node.url()).expect("read"), 6, "{conf}");
        }
        let requests = node.requests().len();
        assert_eq!(requests, 3);
        // A configured value is used with no request, even if the node would answer another value.
        let other = decimals_node(200, decimals_answer(18));
        assert_eq!(
            read_decimals(&json::json!({"decimals": 6}), &other.url()).expect("config"),
            6
        );
        assert_eq!(
            read_decimals(&json::json!({"decimals": 255}), &other.url()).expect("config"),
            255
        );
        assert!(other.requests().is_empty());
        // The read asks `decimals()` with no parameter, from the caller, at the contract.
        let body = &node.bodies_for("/wallet/triggerconstantcontract")[0];
        assert_eq!(body["function_selector"], "decimals()");
        assert_eq!(body["parameter"], "");
        assert_eq!(body["contract_address"], "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t");
    }

    /// A value above 255 or a failed read is `Transport`; a configured value above 255 is a config error.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_failed_or_out_of_range_decimals_read_is_transport() {
        let conf = json::json!({});
        for node in [
            decimals_node(200, decimals_answer(256)),
            decimals_node(500, "{}".to_owned()),
            decimals_node(200, json::json!({"Error": "contract not found"}).to_string()),
            decimals_node(200, json::json!({"constant_result": [""]}).to_string()),
            decimals_node(200, json::json!({"constant_result": ["06"]}).to_string()),
        ] {
            let err = read_decimals(&conf, &node.url()).expect_err("refused");
            assert!(matches!(err, EthCoinBuildError::Transport(_)), "{err}");
        }
        let node = decimals_node(200, decimals_answer(6));
        let err = read_decimals(&json::json!({"decimals": 256}), &node.url()).expect_err("refused");
        assert!(matches!(err, EthCoinBuildError::InvalidConfig(_)), "{err}");
        assert!(node.requests().is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn invalid_url_error_does_not_echo_the_configured_string() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": ["https://user:secret@bad host/APIKEY?token=X"]});
        let err = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
            network: Network::Mainnet,
        })
        .await
        .unwrap_err();
        assert!(err.contains("not a valid URI"), "the invalid-URI reason is missing");
        for secret in ["user", "secret", "APIKEY", "token=X", "bad host"] {
            assert!(!err.contains(secret), "a credential part is rendered");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn trc20_without_decimals_reads_them_at_activation() {
        let node = decimals_node(200, decimals_answer(6));
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": [node.url()]});
        let coin = tron_coin_from_conf_and_request(&ctx, "USDT-TRC20", &conf, &req, &priv_key(), CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".to_owned(),
        })
        .await
        .expect("build");
        assert_eq!(coin.decimals, 6);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn trc20_without_decimals_fails_when_the_read_fails() {
        let node = decimals_node(500, "{}".to_owned());
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let conf = json::json!({});
        let req = json::json!({"urls": [node.url()]});
        let err = tron_coin_from_conf_and_request(&ctx, "USDT-TRC20", &conf, &req, &priv_key(), CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".to_owned(),
        })
        .await
        .unwrap_err();
        assert!(err.contains("TRC20 coin") && err.contains("All nodes failed"), "{err}");
    }
}
