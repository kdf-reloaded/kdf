//! Typed EVM coin activation parameters and the shared EVM coin builder
//! (CRD ch. 35 §35.1 / §35.2).
//!
//! The builder is identical for every signing policy except how the
//! [`EthSigner`] and the coin's [`DerivationMethod`] are constructed. The
//! failure classes of the builder are typed ([`EthCoinBuildError`]) so the
//! activation layer can map them to the wire discriminants of R35.1.5 /
//! R35.1.6 without parsing error text.

use super::*;
use crate::eth::eth_hd_wallet::{EthHDAccount, EthHDWallet};
use crate::eth::node_url::{host_only, scrub_node_url};
use crate::hd_wallet::{HDAccountsMap, HDAccountsMutex};
use crate::hd_wallet_storage::HDWalletCoinStorage;
use crate::is_wallet_only_conf;
use crate::rpc_command::get_my_address::PathToAddress;
use alloy::providers::Provider as _;
use common::log::{info, warn};
use crypto::{Bip44PathToCoin, ChildNumber, DerivationPath, GlobalHDAccountArc};

/// The failure classes of the EVM coin builder.
#[derive(Debug, Display)]
pub enum EthCoinBuildError {
    /// The request names no node at all.
    #[display(fmt = "{}", _0)]
    AtLeastOneNodeRequired(String),
    /// The caller-supplied payload cannot be used: a node URL, a swap contract, the coin's derivation path.
    #[display(fmt = "{}", _0)]
    InvalidPayload(String),
    /// Every supplied node failed the reachability probe (R35.1.6).
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// Any other failure, including HD wallet storage failures.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

/// The failure classes of building an ERC-20 token coin on top of an activated platform.
#[derive(Debug, Display)]
pub enum Erc20BuildError {
    /// The token's decimals could not be read from the contract: the nodes gave no usable answer (R35.1.6).
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// The token configuration declares decimals the framework cannot represent.
    #[display(fmt = "{}", _0)]
    UnsupportedDecimals(String),
    /// The platform coin's context is gone.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

/// Typed activation parameters of an EVM platform coin (the request fields of CRD R35.1.1 that reach
/// the coin builder).
#[derive(Clone, Debug, Default)]
pub struct EthCoinActivationParams {
    /// The EVM JSON-RPC endpoints (HTTP or HTTPS).
    pub urls: Vec<String>,
    /// The v1 swap contract. `None` is accepted only for a wallet-only coin.
    pub swap_contract_address: Option<Address>,
    /// The fallback v1 swap contract.
    pub fallback_swap_contract: Option<Address>,
    /// The v2 maker/taker swap contracts.
    pub swap_v2_contracts: Option<SwapV2Contracts>,
    /// The NFT maker v2 swap contract.
    pub nft_swap_v2_contract: Option<Address>,
    /// Swap confirmations of the platform coin; the coin configuration, then
    /// [`Self::default_required_confirmations`], when absent.
    pub required_confirmations: Option<u64>,
    /// The confirmations used when neither the request nor the coin configuration gives a value: `1` for the
    /// legacy `enable`/`electrum` methods ([`LEGACY_EVM_REQUIRED_CONFIRMATIONS`]), `3` for the mmrpc 2.0 EVM
    /// methods ([`V2_EVM_REQUIRED_CONFIRMATIONS`]).
    pub default_required_confirmations: u64,
    /// Whether the transaction history is preloaded in the background.
    pub tx_history: bool,
    /// The gas-station endpoint used for legacy gas-price estimation.
    pub gas_station_url: Option<String>,
    /// Decimals that denominate the gas-station response in gwei.
    pub gas_station_decimals: Option<u8>,
    /// How the gas price is derived from the gas-station response.
    pub gas_station_policy: GasStationPricePolicy,
    /// The initial swap gas-fee policy; the legacy policy when absent.
    pub swap_gas_fee_policy: Option<SwapGasFeePolicy>,
    /// Which route set [`Self::swap_gas_fee_policy`]; only used to log it at activation.
    pub swap_gas_fee_policy_source: SwapGasFeePolicySource,
}

/// Which route set the initial swap gas-fee policy of a platform coin (CRD R35.1.1: request
/// parameter, else coin configuration, else the built-in `Legacy` default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SwapGasFeePolicySource {
    /// Neither the request nor the coin configuration named a policy.
    #[default]
    BuiltInDefault,
    /// The `swap_gas_fee_policy` activation parameter.
    RequestParameter,
    /// The `swap_gas_fee_policy` key of the platform coin's configuration entry.
    CoinConfig,
}

impl SwapGasFeePolicySource {
    fn describe(self) -> &'static str {
        match self {
            SwapGasFeePolicySource::BuiltInDefault => "built-in default",
            SwapGasFeePolicySource::RequestParameter => "request parameter",
            SwapGasFeePolicySource::CoinConfig => "coin config",
        }
    }
}

/// The initial swap gas-fee policy of a platform coin and the route that set it: the request parameter, else
/// the `swap_gas_fee_policy` key of the coin configuration, else none (the coin then starts as `Legacy`).
pub fn resolve_swap_gas_fee_policy(
    requested: Option<SwapGasFeePolicy>,
    platform_conf: &Json,
) -> (Option<SwapGasFeePolicy>, SwapGasFeePolicySource) {
    if let Some(policy) = requested {
        return (Some(policy), SwapGasFeePolicySource::RequestParameter);
    }
    match json::from_value(platform_conf["swap_gas_fee_policy"].clone()) {
        Ok(policy) => (Some(policy), SwapGasFeePolicySource::CoinConfig),
        Err(_) => (None, SwapGasFeePolicySource::BuiltInDefault),
    }
}

/// The `MM2.json` key of the compatibility switch that lifts the cap for a coin whose configuration has no
/// `max_eth_tx_type`.
pub const MAX_ETH_TX_TYPE_ABSENT_UNRESTRICTED_KEY: &str = "max_eth_tx_type_absent_unrestricted";

/// Whether an absent `max_eth_tx_type` coin-configuration key means *unrestricted*, CRD D35.7.
///
/// The default matches the reference lineage: an absent key reads as `0` (untyped transactions only). Setting
/// this switch to the boolean `true` opts into Reloaded's divergence, under which a coin whose configuration is
/// silent about the cap can still send an EIP-1559 swap transaction. Any other value, including the string
/// `"true"`, leaves the default.
pub fn max_eth_tx_type_absent_unrestricted(ctx: &MmArc) -> bool {
    ctx.conf[MAX_ETH_TX_TYPE_ABSENT_UNRESTRICTED_KEY].as_bool() == Some(true)
}

/// Where the effective cap on the emitted transaction type of a coin came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxTypeCapSource {
    /// The `max_eth_tx_type` key of the coin's own configuration entry, or of its platform coin's entry.
    CoinConfig,
    /// No key anywhere, and the compatibility switch makes the absent key mean no cap.
    Unrestricted,
    /// No key anywhere: the default reading, as in the reference, is `0`.
    AbsentKeyIsZero,
}

impl TxTypeCapSource {
    fn describe(self) -> &'static str {
        match self {
            TxTypeCapSource::CoinConfig => "coin config",
            TxTypeCapSource::Unrestricted => {
                "no max_eth_tx_type key, unrestricted by max_eth_tx_type_absent_unrestricted"
            },
            TxTypeCapSource::AbsentKeyIsZero => "no max_eth_tx_type key, treated as 0",
        }
    }
}

/// The highest EIP-2718 transaction type `confs` allow a coin to send (CRD D35.7), and where that came from.
///
/// `confs` lists the coin's own configuration entry first and, for a token, its platform coin's entry second;
/// the first entry holding a non-negative integer `max_eth_tx_type` decides. A value that is not a
/// non-negative integer counts as absent; an absent key is `0` unless `absent_unrestricted`. `None` means
/// unrestricted.
pub fn resolve_max_eth_tx_type(confs: &[&Json], absent_unrestricted: bool) -> (Option<u64>, TxTypeCapSource) {
    if let Some(max) = confs.iter().find_map(|conf| conf["max_eth_tx_type"].as_u64()) {
        return (Some(max), TxTypeCapSource::CoinConfig);
    }
    if absent_unrestricted {
        (None, TxTypeCapSource::Unrestricted)
    } else {
        (Some(0), TxTypeCapSource::AbsentKeyIsZero)
    }
}

impl EthCoinImpl {
    /// The effective cap on the transaction type this coin may send, and its source. A token falls back to
    /// its platform coin's configuration entry.
    pub(crate) fn max_eth_tx_type(&self, ctx: &MmArc) -> (Option<u64>, TxTypeCapSource) {
        let own = crate::coin_conf(ctx, &self.ticker);
        let platform = match &self.coin_type {
            EthCoinType::Erc20 { platform, .. } | EthCoinType::Trc20 { platform, .. } => {
                crate::coin_conf(ctx, platform)
            },
            _ => Json::Null,
        };
        resolve_max_eth_tx_type(&[&own, &platform], max_eth_tx_type_absent_unrestricted(ctx))
    }
}

/// The one-line INFO message logged when an EVM platform coin is built.
fn swap_gas_fee_policy_log_line(
    ticker: &str,
    policy: SwapGasFeePolicy,
    source: SwapGasFeePolicySource,
    max_tx_type: (Option<u64>, TxTypeCapSource),
) -> String {
    let cap = match max_tx_type.0 {
        Some(max) => max.to_string(),
        None => "none".to_owned(),
    };
    format!(
        "{}: swap gas-fee policy {:?} (source: {}); max tx type {} (source: {})",
        ticker,
        policy,
        source.describe(),
        cap,
        max_tx_type.1.describe()
    )
}

impl EthCoinActivationParams {
    /// Parses the stringly typed request of the legacy `enable` method.
    ///
    /// `conf` is the coin's own configuration entry. The initial swap gas-fee policy is resolved exactly as on
    /// the v2 and task routes (CRD R35.6.7, D35.6): the `swap_gas_fee_policy` request field, else the
    /// coin-configuration key, else the built-in `Legacy`. Unlike those routes, an unrecognised value in the
    /// request field is **ignored** in favour of the configured or default policy rather than rejected
    /// (CRD R55.4.4); the two parses are deliberately not shared.
    pub fn from_legacy_request(req: &Json, conf: &Json) -> Result<Self, String> {
        let urls: Vec<String> = try_s!(json::from_value(req["urls"].clone()));
        let swap_contract_address: Option<Address> = try_s!(json::from_value(req["swap_contract_address"].clone()));
        let fallback_swap_contract: Option<Address> = try_s!(json::from_value(req["fallback_swap_contract"].clone()));
        let gas_station_decimals: Option<u8> = try_s!(json::from_value(req["gas_station_decimals"].clone()));
        let gas_station_policy: GasStationPricePolicy =
            json::from_value(req["gas_station_policy"].clone()).unwrap_or_default();
        let gas_station_url: Option<String> = try_s!(json::from_value(req["gas_station_url"].clone()));
        if req["requires_notarization"].as_bool().is_some() {
            log!("Warning: requires_notarization doesn't take any effect on ETH/ERC20 coins");
        }
        let requested_policy: Option<SwapGasFeePolicy> = json::from_value(req["swap_gas_fee_policy"].clone()).ok();
        let (swap_gas_fee_policy, swap_gas_fee_policy_source) = resolve_swap_gas_fee_policy(requested_policy, conf);
        Ok(EthCoinActivationParams {
            urls,
            swap_contract_address,
            fallback_swap_contract,
            swap_v2_contracts: None,
            nft_swap_v2_contract: None,
            required_confirmations: req["required_confirmations"].as_u64(),
            default_required_confirmations: LEGACY_EVM_REQUIRED_CONFIRMATIONS,
            tx_history: req["tx_history"].as_bool().unwrap_or(false),
            gas_station_url,
            gas_station_decimals,
            gas_station_policy,
            swap_gas_fee_policy,
            swap_gas_fee_policy_source,
        })
    }

    /// Validates the swap contracts against the coin configuration (CRD R35.1.1): a trading-capable coin
    /// needs a non-zero v1 swap contract; a zero fallback or v2 contract is rejected.
    ///
    /// A wallet-only coin needs no swap contract at all.
    ///
    /// The reference `v2.6.0-beta` requires `swap_contract_address` for every coin at request-parse level;
    /// `v3.0.0-beta` accepts its absence for a wallet-only coin and answers `InvalidPayload` for a trading-capable
    /// one. Reloaded binds `v3.0.0-beta` (CRD ch.35 R35.1.1).
    pub fn validate_swap_contracts(&self, conf: &Json) -> Result<(), MmError<EthCoinBuildError>> {
        if is_wallet_only_conf(conf) {
            return Ok(());
        }
        match self.swap_contract_address {
            None => {
                return MmError::err(EthCoinBuildError::InvalidPayload(
                    "swap_contract_address is required for a coin that can trade".to_owned(),
                ))
            },
            Some(addr) if addr == Address::default() => {
                return MmError::err(EthCoinBuildError::InvalidPayload(
                    "swap_contract_address can't be zero address".to_owned(),
                ))
            },
            Some(_) => {},
        }
        if self.fallback_swap_contract == Some(Address::default()) {
            return MmError::err(EthCoinBuildError::InvalidPayload(
                "fallback_swap_contract can't be zero address".to_owned(),
            ));
        }
        if let Some(contracts) = &self.swap_v2_contracts {
            let nft_contract = self.nft_swap_v2_contract;
            if contracts.maker_swap_v2_contract == Address::default()
                || contracts.taker_swap_v2_contract == Address::default()
                || nft_contract == Some(Address::default())
            {
                return MmError::err(EthCoinBuildError::InvalidPayload(
                    "swap_v2_contracts can't contain the zero address".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// The scheme check of a node URL (CRD R35.1.1): `http` and `https` are served by the JSON-RPC transport.
///
/// A WebSocket endpoint (`ws`, `wss`) is accepted by the chapter but the transport of this build speaks HTTP
/// only, so it is reported as an unusable payload rather than failing a later probe.
pub fn validate_eth_node_url(url: &str) -> Result<(), MmError<EthCoinBuildError>> {
    let uri = url
        .parse::<http::Uri>()
        .map_to_mm(|e| EthCoinBuildError::InvalidPayload(format!("Invalid node URL: {}", e)))?;
    match uri.scheme_str() {
        Some("http") | Some("https") if uri.host().is_some() => Ok(()),
        Some(scheme) => MmError::err(EthCoinBuildError::InvalidPayload(format!(
            "Unsupported node URL scheme '{}': only http and https are supported",
            scheme
        ))),
        None => MmError::err(EthCoinBuildError::InvalidPayload(
            "Invalid node URL: the scheme is missing".to_owned(),
        )),
    }
}

/// Derives the key pair of the enabled address of a software HD wallet: the coin's configured
/// `derivation_path` extended by `path_to_address` (CRD R35.1.4 `ContextPrivKey` in HD mode).
pub fn derive_eth_hd_activation_key(
    global_hd: &GlobalHDAccountArc,
    conf: &Json,
    path_to_address: &PathToAddress,
) -> Result<KeyPair, MmError<EthCoinBuildError>> {
    let full_path = eth_full_derivation_path(conf, path_to_address)?;
    let secret = global_hd
        .derive_secp256k1_secret(&full_path)
        .mm_err(|e| EthCoinBuildError::InvalidPayload(format!("Cannot derive the enabled address: {}", e)))?;
    KeyPair::from_secret_slice(secret.as_slice()).map_to_mm(|e| EthCoinBuildError::Internal(e.to_string()))
}

/// The configured `derivation_path` of the coin (down to the coin type level).
pub fn eth_path_to_coin(conf: &Json) -> Result<Bip44PathToCoin, MmError<EthCoinBuildError>> {
    if conf["derivation_path"].is_null() {
        return MmError::err(EthCoinBuildError::InvalidPayload(
            "The coin configuration has no derivation_path".to_owned(),
        ));
    }
    json::from_value(conf["derivation_path"].clone())
        .map_to_mm(|e| EthCoinBuildError::InvalidPayload(format!("Unusable derivation_path: {}", e)))
}

/// The full derivation path of the address selected by `path_to_address`.
pub fn eth_full_derivation_path(
    conf: &Json,
    path_to_address: &PathToAddress,
) -> Result<DerivationPath, MmError<EthCoinBuildError>> {
    use crypto::Bip32DerPathOps;

    let mut path = eth_path_to_coin(conf)?.to_derivation_path();
    let child = |value: u32, hardened: bool| {
        ChildNumber::new(value, hardened).map_to_mm(|e| EthCoinBuildError::InvalidPayload(e.to_string()))
    };
    path.push(child(path_to_address.account_id, true)?);
    path.push(path_to_address.chain.to_child_number());
    path.push(child(path_to_address.address_id, false)?);
    Ok(path)
}

/// Builds the HD wallet of an EVM coin: the stored accounts of the wallet namespace the daemon runs in.
pub async fn build_eth_hd_wallet(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    gap_limit: u32,
) -> Result<EthHDWallet, MmError<EthCoinBuildError>> {
    let derivation_path = eth_path_to_coin(conf)?;
    let hd_wallet_storage = HDWalletCoinStorage::init(ctx, ticker.to_owned())
        .await
        .mm_err(|e| EthCoinBuildError::Internal(e.to_string()))?;
    let stored = hd_wallet_storage
        .load_all_accounts()
        .await
        .mm_err(|e| EthCoinBuildError::Internal(e.to_string()))?;
    let loaded: Result<HDAccountsMap<EthHDAccount>, _> = stored
        .iter()
        .map(|item| {
            EthHDAccount::try_from_storage_item(&derivation_path, item).map(|account| (account.account_id, account))
        })
        .collect();
    let accounts = match loaded {
        Ok(accounts) => accounts,
        Err(e) if e.get_inner().is_deserializing_err() => {
            warn!("Error loading HD accounts from the storage: '{}'. Clear accounts", e);
            hd_wallet_storage
                .clear_accounts()
                .await
                .mm_err(|e| EthCoinBuildError::Internal(e.to_string()))?;
            HDAccountsMap::new()
        },
        Err(e) => return MmError::err(EthCoinBuildError::Internal(e.to_string())),
    };
    Ok(EthHDWallet {
        hd_wallet_storage,
        derivation_path,
        accounts: HDAccountsMutex::new(accounts),
        gap_limit,
    })
}

/// The default `gap_limit` of an EVM HD wallet.
pub const ETH_DEFAULT_GAP_LIMIT: u32 = 20;

/// Builds an [`EthCoin`] bound to the single key of the daemon (Iguana mode, or the legacy `enable` path).
pub async fn eth_coin_from_params_with_priv_key(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    priv_key: &[u8],
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    let key_pair = KeyPair::from_secret_slice(priv_key).map_to_mm(|e| EthCoinBuildError::Internal(e.to_string()))?;
    let derivation_method = DerivationMethod::Iguana(key_pair.address());
    build_eth_coin(
        ctx,
        ticker,
        conf,
        params,
        EthSigner::Local(key_pair),
        derivation_method,
        protocol,
    )
    .await
}

/// Builds an [`EthCoin`] of a software HD wallet whose enabled address is the one selected by
/// `path_to_address` below the coin's configured `derivation_path` (CRD R35.1.4, R35.1.3(b)).
pub async fn eth_coin_from_params_with_global_hd(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    global_hd: &GlobalHDAccountArc,
    path_to_address: &PathToAddress,
    gap_limit: u32,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    let key_pair = derive_eth_hd_activation_key(global_hd, conf, path_to_address)?;
    let hd_wallet = build_eth_hd_wallet(ctx, ticker, conf, gap_limit).await?;
    build_eth_coin(
        ctx,
        ticker,
        conf,
        params,
        EthSigner::Local(key_pair),
        DerivationMethod::HDWallet(hd_wallet),
        protocol,
    )
    .await
}

/// MetaMask-policy EVM activation (CRD §47.5.A). Builds an [`EthCoin`] whose signer delegates signing and
/// broadcast to the connected browser MetaMask session; the framework holds no local key.
#[cfg(target_arch = "wasm32")]
pub async fn eth_coin_from_params_with_metamask(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    metamask_arc: crypto::MetamaskArc,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    let signer = EthSigner::Metamask(metamask_arc);
    let derivation_method = DerivationMethod::Iguana(signer.address());
    build_eth_coin(ctx, ticker, conf, params, signer, derivation_method, protocol).await
}

/// Builds an [`EthCoin`] bound to a device-sourced address (CRD §50.1): the signer carries the address, public
/// key and derivation path read from the device, and the coin holds no local secret.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
pub(crate) async fn eth_coin_from_params_with_trezor(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    signer: EthTrezorSigner,
    derivation_method: DerivationMethod<Address, EthHDWallet>,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    build_eth_coin(
        ctx,
        ticker,
        conf,
        params,
        EthSigner::Trezor(signer),
        derivation_method,
        protocol,
    )
    .await
}

/// Probes every node with `web3_clientVersion` and returns the providers that answered.
///
/// When none answers the failure is [`EthCoinBuildError::Transport`] (CRD R35.1.6): every node unresponsive is a
/// transport failure whichever way each node failed. The per-node causes are logged, never returned, and a node
/// is named by scheme, host and port only, because a node URL can embed an API key (CRD R35.1.14).
///
/// The probe itself runs under the per-node cutoff of the failover path (R35.1.10). The provider kept for a node
/// is a direct one: nonce gathering and the signed-transaction broadcast (R35.1.12) address each node on its own.
async fn probe_eth_nodes(
    ctx: &MmArc,
    ticker: &str,
    urls: &[String],
) -> Result<Vec<Web3Instance>, MmError<EthCoinBuildError>> {
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    let mut web3_instances = vec![];
    for url in urls.iter() {
        let node = host_only(url);
        // LP-17: alloy provider per URL + alloy raw web3_clientVersion.
        let provider = match super::alloy_compat::build_provider(vec![url.clone()], event_handlers.clone()) {
            Ok(p) => p,
            Err(e) => {
                log!("Failed to build alloy provider for node " (node) ", " (scrub_node_url(&e, url)));
                continue;
            },
        };
        let version: String = match provider.client().request_noparams("web3_clientVersion").await {
            Ok(v) => v,
            Err(e) => {
                log!("Couldn't get client version for node " (node) ", " (scrub_node_url(&e.to_string(), url)));
                continue;
            },
        };
        let direct = match super::alloy_compat::build_direct_provider(url.clone(), event_handlers.clone()) {
            Ok(p) => p,
            Err(e) => {
                log!("Failed to build alloy provider for node " (node) ", " (scrub_node_url(&e, url)));
                continue;
            },
        };
        web3_instances.push(Web3Instance {
            web3: direct,
            is_parity: version.contains("Parity") || version.contains("parity"),
        })
    }

    // The docs document `Transport` for every node unresponsive (their example reads "Failed to get client version
    // for all nodes"). The reference release no longer probes and answers `Internal` for the first failing read.
    // Reloaded follows the docs, which are explicit (CRD ch.35 R35.1.6).
    if web3_instances.is_empty() {
        return MmError::err(EthCoinBuildError::Transport(
            "Failed to get client version for all nodes".to_owned(),
        ));
    }
    Ok(web3_instances)
}

/// Default confirmations of the legacy `enable`/`electrum` EVM methods when neither the request nor the coin
/// configuration gives a value (unchanged from before the mmrpc 2.0 default was introduced).
pub const LEGACY_EVM_REQUIRED_CONFIRMATIONS: u64 = 1;
/// Default confirmations of the mmrpc 2.0 EVM methods (`enable_eth_with_tokens`, `task::enable_eth`).
pub const V2_EVM_REQUIRED_CONFIRMATIONS: u64 = 3;

/// The swap confirmations of an EVM platform coin: the request, else the coin configuration, else `default`.
///
/// Docs vs reference (CRD ch.35 R35.1.1, erratum (g)): the public docs state a default of 3. The reference
/// version uses the coin configuration, else 1. Reloaded implements the docs' default of 3 by maintainer
/// decision (2026-10-11) for the mmrpc 2.0 methods only; the legacy methods keep 1. The 3 is also the rule
/// the docs' own BCH page states: a coin without a configured value waits for the safer of the two documented numbers. A token falls back to "its configuration, else
/// the platform's effective value" instead (R35.1.1), so this default reaches a token only through its
/// platform.
pub(crate) fn platform_required_confirmations(requested: Option<u64>, conf: &Json, default: u64) -> u64 {
    requested.unwrap_or_else(|| conf["required_confirmations"].as_u64().unwrap_or(default))
}

/// Shared EVM-coin builder body: identical for every signing policy except how the [`EthSigner`] and the
/// [`DerivationMethod`] are constructed.
pub(crate) async fn build_eth_coin(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    signer: EthSigner,
    derivation_method: DerivationMethod<Address, EthHDWallet>,
    protocol: CoinProtocol,
) -> Result<EthCoin, MmError<EthCoinBuildError>> {
    // Defensive: TRON activates through a dedicated builder
    // (`tron::tron_coin_from_conf_and_request`); reject any attempt to route
    // TRX/TRC20 through the EVM legacy activator.
    if matches!(protocol, CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. }) {
        return MmError::err(EthCoinBuildError::Internal(
            "TRON protocol must activate through tron_coin_from_conf_and_request, not the EVM activator".to_owned(),
        ));
    }

    params.validate_swap_contracts(conf)?;
    // A wallet-only coin has no swap, so its swap contracts are not required: none given means the zero address,
    // which `validate_swap_contracts` never lets through for a trading-capable coin.
    let swap_contract_address = params.swap_contract_address.unwrap_or_default();
    let fallback_swap_contract = params.fallback_swap_contract;
    let swap_v2_contracts = params.swap_v2_contracts;
    let nft_swap_v2_contract = params.nft_swap_v2_contract;

    let mut urls = params.urls.clone();
    if urls.is_empty() {
        return MmError::err(EthCoinBuildError::AtLeastOneNodeRequired(
            "Enable request for ETH coin must have at least 1 node URL".to_owned(),
        ));
    }
    for url in urls.iter() {
        validate_eth_node_url(url)?;
    }
    let mut rng = small_rng();
    urls.as_mut_slice().shuffle(&mut rng);

    let my_address = signer.address();

    let web3_instances = probe_eth_nodes(ctx, ticker, &urls).await?;
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    let web3 = super::alloy_compat::build_provider(urls, event_handlers).map_to_mm(EthCoinBuildError::Internal)?;

    let (coin_type, decimals) = match protocol {
        CoinProtocol::ETH { .. } => (EthCoinType::Eth, 18),
        CoinProtocol::ERC20 {
            platform,
            contract_address,
        } => {
            let token_addr = valid_addr_from_str(&contract_address).map_to_mm(EthCoinBuildError::InvalidPayload)?;
            // An on-chain decimals read that gets no usable answer is `Transport`: the reference `v3.0.0-beta`
            // does the same, while `v2.6.0-beta` answers `Internal` (CRD ch.35 R35.1.6).
            let decimals = match conf["decimals"].as_u64() {
                None | Some(0) => get_token_decimals(&web3, token_addr)
                    .await
                    .map_to_mm(EthCoinBuildError::Transport)?,
                Some(d) => d as u8,
            };
            (EthCoinType::Erc20 { platform, token_addr }, decimals)
        },
        _ => return MmError::err(EthCoinBuildError::Internal("Expect ETH or ERC20 protocol".to_owned())),
    };

    let required_confirmations = platform_required_confirmations(
        params.required_confirmations,
        conf,
        params.default_required_confirmations,
    )
    .into();

    let sign_message_prefix: Option<String> = json::from_value(conf["sign_message_prefix"].clone()).unwrap_or(None);

    let initial_history_state = if params.tx_history {
        HistorySyncState::NotStarted
    } else {
        HistorySyncState::NotEnabled
    };

    let coin = EthCoinImpl {
        signer,
        my_address,
        coin_type,
        sign_message_prefix,
        swap_contract_address,
        fallback_swap_contract,
        decimals,
        ticker: ticker.into(),
        gas_station_url: params.gas_station_url.clone(),
        gas_station_decimals: params.gas_station_decimals.unwrap_or(ETH_GAS_STATION_DECIMALS),
        gas_station_policy: params.gas_station_policy,
        web3,
        web3_instances,
        history_sync_state: Mutex::new(initial_history_state),
        ctx: ctx.weak(),
        required_confirmations,
        chain_id: conf["chain_id"].as_u64(),
        logs_block_range: conf["logs_block_range"].as_u64().unwrap_or(DEFAULT_LOGS_BLOCK_RANGE),
        derivation_method,
        swap_v2_contracts,
        gas_limit_v2: EthGasLimitV2::default(),
        // ETH/ERC20 coins never use the TRON HTTP API; populated only by the
        // dedicated TRON activation path.
        tron_api: None,
        nft_swap_v2_contract,
        swap_gas_fee_policy: Mutex::new(params.swap_gas_fee_policy.unwrap_or_default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
    };
    // Every EVM platform activation route (legacy `enable`, v2, task, hardware wallet) ends here.
    if matches!(coin.coin_type, EthCoinType::Eth) {
        info!(
            "{}",
            swap_gas_fee_policy_log_line(
                ticker,
                params.swap_gas_fee_policy.unwrap_or_default(),
                params.swap_gas_fee_policy_source,
                coin.max_eth_tx_type(ctx)
            )
        );
    }
    Ok(EthCoin(Arc::new(coin)))
}

/// The ticker of an active ERC-20 token coin with the given platform and contract address (CRD R35.2.7:
/// a custom token is rejected while another token with the same contract is active).
pub async fn find_active_erc20_by_contract(ctx: &MmArc, platform: &str, token_addr: Address) -> Option<String> {
    let coins_ctx = CoinsContext::from_ctx(ctx).ok()?;
    let coins = coins_ctx.coins.lock().await;
    coins.values().find_map(|coin| match coin {
        crate::MmCoinEnum::EthCoin(eth) => match &eth.coin_type {
            EthCoinType::Erc20 {
                platform: token_platform,
                token_addr: active_addr,
            } if token_platform == platform && *active_addr == token_addr => Some(eth.ticker().to_owned()),
            _ => None,
        },
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SWAP_CONTRACT: &str = "0x24ABE4c71FC658C91313b6552cd40cD808b3Ea80";

    fn params(swap: Option<&str>) -> EthCoinActivationParams {
        EthCoinActivationParams {
            urls: vec!["http://127.0.0.1:1".to_owned()],
            swap_contract_address: swap.map(|addr| addr.parse().unwrap()),
            ..Default::default()
        }
    }

    fn invalid_payload(result: Result<(), MmError<EthCoinBuildError>>) -> bool {
        matches!(result, Err(e) if matches!(e.get_inner(), EthCoinBuildError::InvalidPayload(_)))
    }

    #[test]
    fn a_trading_capable_coin_needs_a_non_zero_swap_contract() {
        let conf = json!({ "coin": "ETH" });
        assert!(params(Some(SWAP_CONTRACT)).validate_swap_contracts(&conf).is_ok());
        assert!(invalid_payload(params(None).validate_swap_contracts(&conf)));
        let zero = "0x0000000000000000000000000000000000000000";
        assert!(invalid_payload(params(Some(zero)).validate_swap_contracts(&conf)));

        let mut zero_fallback = params(Some(SWAP_CONTRACT));
        zero_fallback.fallback_swap_contract = Some(Address::default());
        assert!(invalid_payload(zero_fallback.validate_swap_contracts(&conf)));

        let mut zero_v2 = params(Some(SWAP_CONTRACT));
        zero_v2.swap_v2_contracts = Some(SwapV2Contracts {
            maker_swap_v2_contract: SWAP_CONTRACT.parse().unwrap(),
            taker_swap_v2_contract: Address::default(),
        });
        assert!(invalid_payload(zero_v2.validate_swap_contracts(&conf)));
    }

    #[test]
    fn swap_gas_fee_policy_source_is_request_then_coin_config_then_default() {
        let conf = json!({ "swap_gas_fee_policy": "Medium" });
        assert_eq!(
            resolve_swap_gas_fee_policy(Some(SwapGasFeePolicy::High), &conf),
            (Some(SwapGasFeePolicy::High), SwapGasFeePolicySource::RequestParameter)
        );
        assert_eq!(
            resolve_swap_gas_fee_policy(None, &conf),
            (Some(SwapGasFeePolicy::Medium), SwapGasFeePolicySource::CoinConfig)
        );
        assert_eq!(
            resolve_swap_gas_fee_policy(None, &json!({})),
            (None, SwapGasFeePolicySource::BuiltInDefault)
        );
    }

    #[test]
    fn swap_gas_fee_policy_log_line_names_value_and_source() {
        assert_eq!(
            swap_gas_fee_policy_log_line(
                "MATIC",
                SwapGasFeePolicy::Medium,
                SwapGasFeePolicySource::CoinConfig,
                (Some(2), TxTypeCapSource::CoinConfig)
            ),
            "MATIC: swap gas-fee policy Medium (source: coin config); max tx type 2 (source: coin config)"
        );
        assert_eq!(
            swap_gas_fee_policy_log_line(
                "BNB",
                SwapGasFeePolicy::Legacy,
                SwapGasFeePolicySource::BuiltInDefault,
                (Some(0), TxTypeCapSource::AbsentKeyIsZero)
            ),
            "BNB: swap gas-fee policy Legacy (source: built-in default); max tx type 0 \
             (source: no max_eth_tx_type key, treated as 0)"
        );
        let line = swap_gas_fee_policy_log_line(
            "ETH",
            SwapGasFeePolicy::Legacy,
            SwapGasFeePolicySource::BuiltInDefault,
            (None, TxTypeCapSource::Unrestricted),
        );
        assert!(line.contains("max tx type none"), "{line}");
        assert!(line.contains("max_eth_tx_type_absent_unrestricted"), "{line}");
    }

    #[test]
    fn max_eth_tx_type_is_own_entry_then_platform_entry_then_zero_unless_the_switch_is_on() {
        let own = json!({ "max_eth_tx_type": 1 });
        let platform = json!({ "max_eth_tx_type": 2 });
        let silent = json!({ "coin": "X" });
        assert_eq!(
            resolve_max_eth_tx_type(&[&own, &platform], false),
            (Some(1), TxTypeCapSource::CoinConfig)
        );
        // A token whose own entry is silent inherits the platform's cap.
        assert_eq!(
            resolve_max_eth_tx_type(&[&silent, &platform], false),
            (Some(2), TxTypeCapSource::CoinConfig)
        );
        // An explicit 0 is a cap, not an absence, with or without the switch.
        let zero = json!({ "max_eth_tx_type": 0 });
        assert_eq!(
            resolve_max_eth_tx_type(&[&zero, &platform], false),
            (Some(0), TxTypeCapSource::CoinConfig)
        );
        // Absent everywhere: the default reads 0, as in the reference.
        assert_eq!(
            resolve_max_eth_tx_type(&[&silent, &silent], false),
            (Some(0), TxTypeCapSource::AbsentKeyIsZero)
        );
        // The switch opts into the unrestricted reading.
        assert_eq!(
            resolve_max_eth_tx_type(&[&silent, &silent], true),
            (None, TxTypeCapSource::Unrestricted)
        );
        // The switch never overrides a key that is present.
        assert_eq!(
            resolve_max_eth_tx_type(&[&platform], true),
            (Some(2), TxTypeCapSource::CoinConfig)
        );
        // A value that is not a non-negative integer counts as absent.
        for bad in [json!("2"), json!(-1), json!(1.5), json!(null), json!(true)] {
            let conf = json!({ "max_eth_tx_type": bad });
            assert_eq!(
                resolve_max_eth_tx_type(&[&conf], false),
                (Some(0), TxTypeCapSource::AbsentKeyIsZero),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_cap_switch_is_on_only_for_the_boolean_true() {
        use mm2_core::mm_ctx::MmCtxBuilder;
        let switch = |value: Json| {
            let ctx = MmCtxBuilder::new()
                .with_conf(json!({ MAX_ETH_TX_TYPE_ABSENT_UNRESTRICTED_KEY: value }))
                .into_mm_arc();
            max_eth_tx_type_absent_unrestricted(&ctx)
        };
        assert!(switch(json!(true)));
        for off in [json!(false), json!("true"), json!(1), json!("yes"), json!(null)] {
            assert!(!switch(off.clone()), "{off}");
        }
        assert!(!max_eth_tx_type_absent_unrestricted(&MmCtxBuilder::new().into_mm_arc()));
    }

    fn legacy_params(req_extra: Json, conf: Json) -> EthCoinActivationParams {
        let mut req = json!({ "urls": ["http://localhost:8545"] });
        for (k, v) in req_extra.as_object().unwrap() {
            req[k] = v.clone();
        }
        EthCoinActivationParams::from_legacy_request(&req, &conf).unwrap()
    }

    #[test]
    fn the_legacy_route_resolves_the_initial_policy_from_the_coin_configuration() {
        let params = legacy_params(json!({}), json!({ "swap_gas_fee_policy": "Medium" }));
        assert_eq!(params.swap_gas_fee_policy, Some(SwapGasFeePolicy::Medium));
        assert_eq!(params.swap_gas_fee_policy_source, SwapGasFeePolicySource::CoinConfig);
        let params = legacy_params(json!({}), json!({}));
        assert_eq!(params.swap_gas_fee_policy, None);
        assert_eq!(
            params.swap_gas_fee_policy_source,
            SwapGasFeePolicySource::BuiltInDefault
        );
    }

    #[test]
    fn the_legacy_route_honours_the_request_field_over_the_configuration() {
        let params = legacy_params(
            json!({ "swap_gas_fee_policy": "High" }),
            json!({ "swap_gas_fee_policy": "Medium" }),
        );
        assert_eq!(params.swap_gas_fee_policy, Some(SwapGasFeePolicy::High));
        assert_eq!(
            params.swap_gas_fee_policy_source,
            SwapGasFeePolicySource::RequestParameter
        );
        let params = legacy_params(
            json!({ "swap_gas_fee_policy": "Legacy" }),
            json!({ "swap_gas_fee_policy": "Medium" }),
        );
        assert_eq!(params.swap_gas_fee_policy, Some(SwapGasFeePolicy::Legacy));
    }

    /// CRD R55.4.4: on the legacy route an unrecognised value is ignored, not rejected -- the v2 and
    /// task routes reject it (their request types fail to parse; see the `coins_activation` tests).
    #[test]
    fn the_legacy_route_ignores_an_unrecognised_policy_value() {
        for bad in [json!("Ludicrous"), json!("medium"), json!(5), json!(null), json!({})] {
            let configured = legacy_params(
                json!({ "swap_gas_fee_policy": bad.clone() }),
                json!({ "swap_gas_fee_policy": "Medium" }),
            );
            assert_eq!(configured.swap_gas_fee_policy, Some(SwapGasFeePolicy::Medium), "{bad}");
            assert_eq!(
                configured.swap_gas_fee_policy_source,
                SwapGasFeePolicySource::CoinConfig
            );
            let defaulted = legacy_params(json!({ "swap_gas_fee_policy": bad.clone() }), json!({}));
            assert_eq!(defaulted.swap_gas_fee_policy, None, "{bad}");
            assert_eq!(
                defaulted.swap_gas_fee_policy_source,
                SwapGasFeePolicySource::BuiltInDefault
            );
        }
    }

    #[test]
    fn platform_confirmations_default_to_the_config_value_then_the_method_default() {
        // CRD ch.35 R35.1.1: request, else coin config, else the method family's default (v2: 3).
        let v2 = V2_EVM_REQUIRED_CONFIRMATIONS;
        assert_eq!(
            platform_required_confirmations(Some(5), &json!({ "required_confirmations": 2 }), v2),
            5
        );
        assert_eq!(
            platform_required_confirmations(None, &json!({ "required_confirmations": 2 }), v2),
            2
        );
        assert_eq!(platform_required_confirmations(None, &json!({}), v2), 3);
        assert_eq!(
            platform_required_confirmations(None, &json!({ "required_confirmations": "x" }), v2),
            3
        );
        assert_eq!(platform_required_confirmations(Some(1), &json!({}), v2), 1);
    }

    #[test]
    fn legacy_request_keeps_the_default_of_one_confirmation() {
        let req = json!({ "urls": ["http://localhost:8545"] });
        let params = EthCoinActivationParams::from_legacy_request(&req, &json!({})).unwrap();
        assert_eq!(params.default_required_confirmations, 1);
        assert_eq!(
            platform_required_confirmations(
                params.required_confirmations,
                &json!({}),
                params.default_required_confirmations
            ),
            1
        );
    }

    #[test]
    fn a_wallet_only_coin_needs_no_swap_contract() {
        let conf = json!({ "coin": "ETH", "wallet_only": true });
        assert!(params(None).validate_swap_contracts(&conf).is_ok());
    }

    #[test]
    fn node_urls_are_http_or_https() {
        for url in [
            "http://node.example",
            "https://node.example/v3/key",
            "http://127.0.0.1:8545",
        ] {
            assert!(validate_eth_node_url(url).is_ok(), "{url}");
        }
        for url in [
            "ws://node.example",
            "wss://node.example",
            "ftp://node.example",
            "node.example",
            "not a url",
            "",
        ] {
            assert!(invalid_payload(validate_eth_node_url(url)), "{url}");
        }
    }

    #[test]
    fn the_legacy_request_keeps_its_parsing() {
        let req = json!({
            "urls": ["http://node.example"],
            "swap_contract_address": SWAP_CONTRACT,
            "required_confirmations": 3,
            "tx_history": true,
            "gas_station_decimals": 9,
        });
        let params = EthCoinActivationParams::from_legacy_request(&req, &json!({})).unwrap();
        assert_eq!(params.urls, vec!["http://node.example".to_owned()]);
        assert_eq!(params.swap_contract_address, Some(SWAP_CONTRACT.parse().unwrap()));
        assert_eq!(params.required_confirmations, Some(3));
        assert!(params.tx_history);
        assert_eq!(params.gas_station_decimals, Some(9));
        // A swap contract that is not an address is still refused.
        assert!(EthCoinActivationParams::from_legacy_request(
            &json!({ "urls": [], "swap_contract_address": "nope" }),
            &json!({})
        )
        .is_err());
    }

    #[test]
    fn the_enabled_address_path_extends_the_coin_derivation_path() {
        let conf = json!({ "derivation_path": "m/44'/60'" });
        let path = PathToAddress {
            account_id: 2,
            chain: crypto::Bip44Chain::External,
            address_id: 5,
        };
        assert_eq!(
            eth_full_derivation_path(&conf, &path).unwrap().to_string(),
            "m/44'/60'/2'/0/5"
        );
        // A configuration without a usable path is an invalid payload.
        for conf in [json!({}), json!({ "derivation_path": "garbage" })] {
            assert!(matches!(
                eth_full_derivation_path(&conf, &path).map_err(|e| e.into_inner()),
                Err(EthCoinBuildError::InvalidPayload(_))
            ));
        }
    }

    /// T35.28: the activation probe names a failing node by scheme, host and port only, in its log lines and
    /// in the error it returns.
    #[test]
    fn the_activation_probe_names_no_credential() {
        use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};

        let capturing = install_log_capture();
        let url = "http://user:secret@127.0.0.1:1/v3/APIKEY?token=X".to_owned();
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        let err = common::block_on(probe_eth_nodes(&ctx, "ETH", &[url])).unwrap_err();
        assert!(matches!(err.get_inner(), EthCoinBuildError::Transport(_)), "{err}");
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|line| line.contains("client version for node"))
            .collect();
        if capturing {
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(lines[0].contains("http://127.0.0.1:1"), "{lines:?}");
        }
        for text in lines.iter().map(String::as_str).chain([err.to_string().as_str()]) {
            for secret in ["user", "secret", "/v3/APIKEY", "APIKEY", "token=X"] {
                assert!(!text.contains(secret), "{secret} in {text}");
            }
        }
    }

    /// T35.28 (`wss://` variant): until a WebSocket transport exists the URL is rejected at activation, and the
    /// rejection names none of its userinfo, path or query.
    #[test]
    fn a_rejected_node_url_is_not_echoed() {
        for url in [
            "wss://user:secret@rpc.example.org:8546/v3/APIKEY?token=X",
            "ftp://user:secret@rpc.example.org/v3/APIKEY?token=X",
            "user:secret@rpc.example.org/v3/APIKEY?token=X",
            "https://user:secret@rpc example.org/v3/APIKEY?token=X",
        ] {
            let err = validate_eth_node_url(url).unwrap_err().to_string();
            for secret in ["user", "secret", "/v3/APIKEY", "APIKEY", "token=X"] {
                assert!(!err.contains(secret), "{secret} in {err} for {url}");
            }
        }
    }
}
