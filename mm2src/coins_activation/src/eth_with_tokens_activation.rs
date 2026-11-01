//! EVM platform-coin-with-tokens activation (CRD §35.1 -- `enable_eth_with_tokens`, §35.3 -- `task::enable_eth`).
//!
//! Implements the generic platform-coin-with-tokens activation traits for the EVM platform coin (`EthCoin`
//! with native `Eth` coin-type), so the framework's `enable_platform_coin_with_tokens::<EthCoin>` entrypoint
//! can activate an EVM platform coin plus an inline batch of ERC-20 tokens (and optionally its NFT
//! pseudo-coin) in one call, and the task framework can run the same activation as a long-running task.

use crate::erc20_token_activation::{build_erc20_token, resolve_erc20_protocol, Erc20ActivationRequest};
use crate::platform_coin_with_tokens::*;
use crate::prelude::*;
use async_trait::async_trait;
use coins::coin_balance::{EnableCoinBalance, EnableCoinBalanceError, EnableCoinScanPolicy, HDWalletBalanceOps};
use coins::eth::{eth_coin_from_params_with_global_hd, eth_coin_from_params_with_priv_key, EthCoin,
                 EthCoinActivationParams, EthCoinBuildError, GasStationPricePolicy, SwapGasFeePolicy, SwapV2Contracts,
                 ETH_DEFAULT_GAP_LIMIT};
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::nft::{fetch_and_store_owned_nfts, Chain, ChainTicker, EnableNftError, NftCtx, NftInfo, NftProvider};
use coins::node_url::{host_only, ServiceUrl};
use coins::rpc_command::get_my_address::PathToAddress;
use coins::{is_wallet_only_conf, BalanceError, CoinBalance, CoinProtocol, MarketCoinOps, UnexpectedDerivationMethod};
use common::executor::spawn;
use common::log::info;
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use crypto::hw_rpc_task::TrezorRpcTaskConnectProcessor;
use crypto::{CryptoCtx, KeyPairPolicy};
use ethereum_types::Address;
use futures::future::{abortable, AbortHandle};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use rpc_task::RpcTaskHandle;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::fmt;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use std::time::Duration;

use crate::init_platform_coin_with_tokens::{InitPlatformCoinWithTokensActivationOps, InitPlatformCoinWithTokensTask};

type ActivationErr = EnablePlatformCoinWithTokensError;

// The EVM token is itself an `EthCoin` (with an `Erc20` coin-type), so the
// platform coin and the token share a Rust type.
impl TokenOf for EthCoin {
    type PlatformCoin = EthCoin;
}

/// A single EVM JSON-RPC endpoint (R35.1.1 `nodes` entry). A `ws_url` next to the `url` is not consumed.
///
/// The CoinNode table of the public docs names the proxy flag `komodo_auth`; every docs example and the reference
/// use `komodo_proxy`, so only that spelling is read and `komodo_auth` is ignored. The docs examples also show a
/// `ws_url` that the reference does not consume; it is accepted and ignored (CRD ch.35 R35.1.1, erratum).
#[derive(Clone, Deserialize)]
pub struct EthNode {
    pub url: String,
    /// The endpoint sits behind the signed proxy. Carried for wire parity: requests are not proxy-signed.
    #[serde(default)]
    #[allow(dead_code)]
    pub komodo_proxy: bool,
}

/// The debug form names the node by its host-only form (CRD ch.35 R35.1.17).
impl fmt::Debug for EthNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EthNode")
            .field("url", &host_only(&self.url))
            .field("komodo_proxy", &self.komodo_proxy)
            .finish()
    }
}

/// The JSON-RPC transport of the platform coin (CRD R35.1.4a).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub enum EthRpcMode {
    /// The coin talks to the `nodes` of the request.
    #[default]
    Default,
    /// The coin talks to the connected MetaMask provider. Exists only on the WASM target.
    #[cfg(target_arch = "wasm32")]
    Metamask,
}

/// The EVM signing policy, selected through the `priv_key_policy` request field as an object tagged by `type`
/// with any payload in a sibling `params` object (CRD R35.1.4).
///
/// The docs show the WalletConnect policy as `{"type": "WalletConnect", "data": <topic>}`; the reference requires
/// `{"type": "WalletConnect", "params": {"session_topic": <topic>}}`, which is what is parsed here. The docs form
/// is to be accepted alongside it when EVM WalletConnect signing is implemented (CRD ch.35 R35.1.4, erratum (e)).
///
/// `ContextPrivKey` (the default, also accepted as `Iguana` and `IguanaPrivKey`) is the daemon's own key:
/// single-address in Iguana mode, HD derivation in HD mode. `Metamask` is defined **only on the WASM target**
/// (CRD R47.7.1); on native builds the variant does not exist, so a `{"type":"Metamask"}` request fails to
/// deserialize and is rejected (CRD R47.7.2 / R47.6.7).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "type", content = "params")]
pub enum EthPrivKeyPolicy {
    #[serde(alias = "IguanaPrivKey", alias = "Iguana")]
    #[default]
    ContextPrivKey,
    /// The hardware wallet policy. Native, non-iOS only; usable through the task variant only because the
    /// connect / PIN / passphrase exchange needs a task to surface its states.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
    Trezor,
    /// The external wallet of an established WalletConnect session. Parsed, but this build cannot sign with one.
    WalletConnect {
        #[allow(dead_code)]
        session_topic: String,
    },
    #[cfg(target_arch = "wasm32")]
    Metamask,
}

/// The inline NFT activation of the request: the documented bare provider or the `{"provider": ...}` wrapper
/// of the reference version (CRD R35.1.4b). The docs show the bare form, the reference requires the wrapper;
/// both are accepted.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum NftReq {
    Wrapped { provider: NftProvider },
    Bare(NftProvider),
}

impl NftReq {
    fn provider(&self) -> &NftProvider {
        match self {
            NftReq::Wrapped { provider } | NftReq::Bare(provider) => provider,
        }
    }
}

/// The v2 swap contracts of the request (CRD R35.1.1 `swap_v2_contracts`). The docs link the member table to one
/// copied from TokensRequest; the members here are those of the docs example and the reference (erratum (c)).
#[derive(Clone, Debug, Deserialize)]
pub struct SwapV2ContractsRequest {
    maker_swap_v2_contract: Address,
    taker_swap_v2_contract: Address,
    nft_maker_swap_v2_contract: Address,
}

/// Deserializes an optional field, treating a malformed value as absent (CRD R35.1.1a).
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Json::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// `enable_eth_with_tokens` request parameters (CRD R35.1.1).
#[derive(Clone, Debug, Deserialize)]
pub struct EthWithTokensActivationRequest {
    /// Absent is treated as empty and rejected by the activation (CRD R35.1.5 `AtLeastOneNodeRequired`). The docs
    /// mark `nodes` required, which would make an absent array a request-parse failure; the reference treats it as
    /// empty and Reloaded does the same (CRD ch.35 R35.1.1).
    #[serde(default)]
    nodes: Option<Vec<EthNode>>,
    erc20_tokens_requests: Vec<TokenActivationRequest<Erc20ActivationRequest>>,
    // The docs mark `swap_contract_address` and `fallback_swap_contract` required. The reference accepts their
    // absence, so Reloaded does too, a superset of the documented requests. A trading-capable coin without a swap
    // contract is still `InvalidPayload` (CRD ch.35 R35.1.1, erratum (h)).
    #[serde(default)]
    swap_contract_address: Option<Address>,
    #[serde(default)]
    fallback_swap_contract: Option<Address>,
    #[serde(default)]
    swap_v2_contracts: Option<SwapV2ContractsRequest>,
    /// Whether the v1 swap contract supports the watcher protocol. The EVM coin has no watcher gate that reads it.
    #[serde(default)]
    #[allow(dead_code)]
    contract_supports_watchers: Option<bool>,
    // The docs state a default of 3. The reference falls back to the coin configuration, else 1, and Reloaded does the
    // same (CRD ch.35 R35.1.1, erratum (g); the default is an open maintainer decision).
    #[serde(default)]
    required_confirmations: Option<u64>,
    /// No effect on EVM coins.
    #[serde(default, deserialize_with = "lenient")]
    #[allow(dead_code)]
    requires_notarization: Option<bool>,
    /// Accepted as an unsigned 8-bit integer and not otherwise enforced.
    #[serde(default)]
    #[allow(dead_code)]
    mm2: Option<u8>,
    // The reference does not consume `tx_history`, `requires_notarization` and the gas-station fields of this method.
    // The docs describe their effect, so Reloaded keeps the documented effect; it changes no wire shape (CRD ch.35
    // R35.1.1a).
    #[serde(default, deserialize_with = "lenient")]
    tx_history: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    gas_station_url: Option<ServiceUrl>,
    #[serde(default, deserialize_with = "lenient")]
    gas_station_decimals: Option<u8>,
    #[serde(default, deserialize_with = "lenient")]
    gas_station_policy: Option<GasStationPricePolicy>,
    #[serde(default)]
    swap_gas_fee_policy: Option<SwapGasFeePolicy>,
    /// Only consulted on WASM, where it must pair with the signing policy.
    #[serde(default)]
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    rpc_mode: Option<EthRpcMode>,
    #[serde(default)]
    priv_key_policy: Option<EthPrivKeyPolicy>,
    #[serde(default)]
    get_balances: Option<bool>,
    #[serde(default)]
    nft_req: Option<NftReq>,
    #[serde(default)]
    path_to_address: Option<PathToAddress>,
    #[serde(default)]
    gap_limit: Option<u32>,
    #[serde(default)]
    scan_policy: Option<EnableCoinScanPolicy>,
    #[serde(default)]
    min_addresses_number: Option<u32>,
}

impl TxHistory for EthWithTokensActivationRequest {
    fn tx_history(&self) -> bool { self.tx_history.unwrap_or(false) }
}

impl EthWithTokensActivationRequest {
    fn priv_key_policy(&self) -> EthPrivKeyPolicy { self.priv_key_policy.clone().unwrap_or_default() }

    fn get_balances(&self) -> bool { self.get_balances.unwrap_or(true) }

    fn path_to_address(&self) -> PathToAddress { self.path_to_address.clone().unwrap_or_default() }

    fn gap_limit(&self) -> u32 { self.gap_limit.unwrap_or(ETH_DEFAULT_GAP_LIMIT) }

    /// The number of external addresses every account of an HD report covers at least: the requested minimum,
    /// and the enabled address, which must always be part of the report.
    fn min_addresses_to_report(&self) -> u32 {
        let enabled_address_id = self.path_to_address().address_id;
        self.min_addresses_number
            .unwrap_or_default()
            .max(enabled_address_id.saturating_add(1))
    }

    /// Whether the request selects a hardware wallet.
    fn is_trezor_policy(&self) -> bool {
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        {
            matches!(self.priv_key_policy, Some(EthPrivKeyPolicy::Trezor))
        }
        #[cfg(not(all(not(target_arch = "wasm32"), not(target_os = "ios"))))]
        {
            false
        }
    }
}

/// EVM platform protocol info resolved from coin configuration.
pub struct EthProtocolInfo {
    chain_id: Option<u64>,
}

impl EthProtocolInfo {
    /// The coin protocol handed to the EVM coin builder. An EVM platform protocol without an integer
    /// `chain_id` is a protocol parse failure (CRD R35.1.2 phase 3).
    fn coin_protocol(&self, ticker: &str) -> Result<CoinProtocol, MmError<ActivationErr>> {
        match self.chain_id {
            Some(chain_id) => Ok(CoinProtocol::ETH {
                chain_id: Some(chain_id),
            }),
            None => MmError::err(ActivationErr::CoinProtocolParseError {
                ticker: ticker.to_owned(),
                error: "the EVM platform protocol needs an integer chain_id".to_owned(),
            }),
        }
    }
}

impl TryFromCoinProtocol for EthProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::ETH { chain_id } => Ok(EthProtocolInfo { chain_id }),
            proto => MmError::err(proto),
        }
    }
}

impl From<EthCoinBuildError> for ActivationErr {
    fn from(err: EthCoinBuildError) -> Self {
        match err {
            EthCoinBuildError::AtLeastOneNodeRequired(e) => ActivationErr::AtLeastOneNodeRequired(e),
            EthCoinBuildError::InvalidPayload(e) => ActivationErr::InvalidPayload(e),
            // An unusable coin-configuration member (CRD R57.5.1) is reported on the closed wire set of
            // ch.35 as an invalid payload; the message names the key.
            EthCoinBuildError::InvalidConfig(e) => ActivationErr::InvalidPayload(e),
            EthCoinBuildError::Transport(e) => ActivationErr::Transport(e),
            EthCoinBuildError::Internal(e) => ActivationErr::Internal(e),
        }
    }
}

impl From<UnexpectedDerivationMethod> for ActivationErr {
    fn from(e: UnexpectedDerivationMethod) -> Self { ActivationErr::UnexpectedDerivationMethod(e.to_string()) }
}

/// Maps a backend balance failure to the platform error set: no usable answer from the nodes is a transport
/// failure (CRD R35.1.6).
fn balance_error(err: BalanceError) -> ActivationErr {
    match err {
        BalanceError::Transport(e) | BalanceError::InvalidResponse(e) => ActivationErr::Transport(e),
        // A balance read sends no fee cap; the arm keeps the node's text so the conversion is total.
        e @ BalanceError::FeeCapBelowBaseFee(_) => ActivationErr::Transport(e.to_string()),
        BalanceError::UnexpectedDerivationMethod(e) => ActivationErr::UnexpectedDerivationMethod(e.to_string()),
        BalanceError::Internal(e) | BalanceError::WalletStorageError(e) => ActivationErr::Internal(e),
    }
}

fn enable_balance_error(err: EnableCoinBalanceError) -> ActivationErr {
    match err {
        EnableCoinBalanceError::BalanceError(e) => balance_error(e),
        EnableCoinBalanceError::NewAccountCreatingError(e) => ActivationErr::Internal(e.to_string()),
    }
}

/// Token initializer enabling the inline ERC-20 tokens of an `enable_eth_with_tokens` request.
pub struct Erc20TokenInitializer {
    platform_coin: EthCoin,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenAsMmCoinInitializer for Erc20TokenInitializer {
    type PlatformCoin = EthCoin;
    type ActivationRequest = EthWithTokensActivationRequest;

    /// Enables the requested tokens in three passes, each covering every token in request order before the
    /// next starts, so the error reported is that of the earliest failing pass (CRD R35.1.2 phase 5): the
    /// resolution of every token's protocol, the platform consistency of every token, then the construction.
    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<coins::MmCoinEnum>, MmError<InitTokensAsMmCoinsError>> {
        let requests = &request.erc20_tokens_requests;

        let mut protocols = Vec::with_capacity(requests.len());
        for token_request in requests {
            let protocol = resolve_erc20_protocol(&ctx, &token_request.ticker, token_request.request.protocol.as_ref())
                .map_to_mm(InitTokensAsMmCoinsError::from)?;
            protocols.push(protocol);
        }

        if protocols
            .iter()
            .any(|protocol| protocol.platform_coin_ticker != self.platform_coin.ticker())
        {
            return MmError::err(InitTokensAsMmCoinsError::PlatformCoinMismatch);
        }

        let mut tokens = Vec::with_capacity(requests.len());
        for (token_request, protocol) in requests.iter().zip(protocols.iter()) {
            let token = build_erc20_token(
                &ctx,
                &self.platform_coin,
                &token_request.ticker,
                protocol,
                token_request.request.required_confirmations,
            )
            .await
            .mm_err(InitTokensAsMmCoinsError::from)?;
            tokens.push(token);
        }

        for token in tokens.iter() {
            self.platform_coin.register_token_info(token);
        }
        Ok(tokens.into_iter().map(Into::into).collect())
    }
}

/// The platform-coin address entry of the single-address result (CRD R35.1.3(a)).
#[derive(Clone, Debug, Serialize)]
pub struct EvmAddressInfo<Balance> {
    derivation_method: DerivationMethod,
    pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    balances: Option<Balance>,
    /// The activated token tickers, reported instead of the balances when `get_balances` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tickers: Option<Vec<String>>,
}

/// The single-address result of `enable_eth_with_tokens` (CRD R35.1.3(a)).
#[derive(Clone, Debug, Serialize)]
pub struct IguanaEthWithTokensActivationResult {
    current_block: u64,
    eth_addresses_infos: HashMap<String, EvmAddressInfo<CoinBalance>>,
    erc20_addresses_infos: HashMap<String, EvmAddressInfo<TokenBalances>>,
    // The docs call `nfts_infos` a list; every docs example and the reference return an object map keyed
    // `<token_address>,<token_id>`, which is what Reloaded returns (CRD ch.35 R35.1.3, erratum (d)).
    nfts_infos: HashMap<String, NftInfo>,
}

/// The HD result of `enable_eth_with_tokens` (CRD R35.1.3(b)).
#[derive(Clone, Debug, Serialize)]
pub struct HDEthWithTokensActivationResult {
    current_block: u64,
    ticker: String,
    wallet_balance: EnableCoinBalance,
    nfts_infos: HashMap<String, NftInfo>,
}

/// The result of `enable_eth_with_tokens`: one of two shapes, distinguished structurally (CRD R35.1.3).
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum EthWithTokensActivationResult {
    Iguana(IguanaEthWithTokensActivationResult),
    HD(HDEthWithTokensActivationResult),
}

impl GetPlatformBalance for EthWithTokensActivationResult {
    /// The balance the transaction-history loop starts from. A result that omits the balances
    /// (`get_balances: false`) reports none; the EVM history loop reads the chain itself and ignores this value.
    fn get_platform_balance(&self) -> BigDecimal {
        match self {
            EthWithTokensActivationResult::Iguana(result) => {
                result
                    .eth_addresses_infos
                    .values()
                    .fold(BigDecimal::from(0), |total, addr_info| match &addr_info.balances {
                        Some(balance) => &total + &balance.get_total(),
                        None => total,
                    })
            },
            EthWithTokensActivationResult::HD(result) => {
                match &result.wallet_balance {
                    EnableCoinBalance::HD(hd) => hd.accounts.iter().fold(BigDecimal::from(0), |total, account| {
                        match account.total_balance.get(&result.ticker) {
                            Some(balance) => &total + &balance.get_total(),
                            None => total,
                        }
                    }),
                    EnableCoinBalance::Iguana(iguana) => iguana.balance.get_total(),
                }
            },
        }
    }
}

impl CurrentBlock for EthWithTokensActivationResult {
    fn current_block(&self) -> u64 {
        match self {
            EthWithTokensActivationResult::Iguana(result) => result.current_block,
            EthWithTokensActivationResult::HD(result) => result.current_block,
        }
    }
}

/// The `nfts_infos` key of an owned NFT (CRD R35.1.3): `<token_address>,<token_id>`.
fn nfts_infos_from_inventory(owned: Vec<NftInfo>) -> HashMap<String, NftInfo> {
    owned
        .into_iter()
        .map(|nft| (format!("{:#x},{}", nft.token_address, nft.token_id), nft))
        .collect()
}

/// The request fields of an EVM coin activation that reach the coin builder, validated (CRD R35.1.2 phase 4:
/// the swap-contract validation).
fn activation_params(
    ctx: &MmArc,
    platform_conf: &Json,
    request: &EthWithTokensActivationRequest,
) -> Result<EthCoinActivationParams, MmError<ActivationErr>> {
    let wallet_only = is_wallet_only_conf(platform_conf);
    if !wallet_only && request.swap_v2_contracts.is_none() && ctx.conf["use_trading_proto_v2"].as_bool() == Some(true) {
        return MmError::err(ActivationErr::InvalidPayload(
            "swap_v2_contracts is required while trading protocol v2 is enabled".to_owned(),
        ));
    }

    let (swap_gas_fee_policy, swap_gas_fee_policy_source) =
        coins::eth::resolve_swap_gas_fee_policy(request.swap_gas_fee_policy, platform_conf);
    let params = EthCoinActivationParams {
        urls: request.nodes.iter().flatten().map(|node| node.url.clone()).collect(),
        swap_contract_address: request.swap_contract_address,
        fallback_swap_contract: request.fallback_swap_contract,
        swap_v2_contracts: request.swap_v2_contracts.as_ref().map(|contracts| SwapV2Contracts {
            maker_swap_v2_contract: contracts.maker_swap_v2_contract,
            taker_swap_v2_contract: contracts.taker_swap_v2_contract,
        }),
        nft_swap_v2_contract: request
            .swap_v2_contracts
            .as_ref()
            .map(|contracts| contracts.nft_maker_swap_v2_contract),
        required_confirmations: request.required_confirmations,
        default_required_confirmations: coins::eth::V2_EVM_REQUIRED_CONFIRMATIONS,
        tx_history: request.tx_history(),
        gas_station_url: request.gas_station_url.as_ref().map(|url| url.as_str().to_owned()),
        gas_station_decimals: request.gas_station_decimals,
        gas_station_policy: request.gas_station_policy.unwrap_or_default(),
        swap_gas_fee_policy,
        swap_gas_fee_policy_source,
    };
    params
        .validate_swap_contracts(platform_conf)
        .mm_err(ActivationErr::from)?;
    Ok(params)
}

/// The unusable-on-this-build policies: the request names a signing policy this build cannot serve.
fn unsupported_policy_error(policy: &EthPrivKeyPolicy) -> Option<ActivationErr> {
    match policy {
        EthPrivKeyPolicy::WalletConnect { .. } => Some(ActivationErr::WalletConnectError(
            "The WalletConnect signing policy is not supported by this build".to_owned(),
        )),
        _ => None,
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PlatformWithTokensActivationOps for EthCoin {
    type ActivationRequest = EthWithTokensActivationRequest;
    type PlatformProtocolInfo = EthProtocolInfo;
    type ActivationResult = EthWithTokensActivationResult;
    type ActivationError = EnablePlatformCoinWithTokensError;

    /// The hardware wallet policy is usable through `task::enable_eth` only (CRD R35.1.2 phase 1).
    fn check_one_shot_request(request: &Self::ActivationRequest) -> Result<(), MmError<Self::ActivationError>> {
        if request.is_trezor_policy() {
            return MmError::err(ActivationErr::UnexpectedDeviceActivationPolicy);
        }
        Ok(())
    }

    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        platform_conf: Json,
        activation_request: Self::ActivationRequest,
        protocol_conf: Self::PlatformProtocolInfo,
        _priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        let protocol = protocol_conf.coin_protocol(&ticker)?;
        let policy = activation_request.priv_key_policy();
        if let Some(error) = unsupported_policy_error(&policy) {
            return MmError::err(error);
        }
        // The hardware wallet policy never builds a coin here: it needs a task to drive the device, and binding
        // the daemon's own key instead would activate the wrong address.
        if activation_request.is_trezor_policy() {
            return MmError::err(ActivationErr::UnexpectedDeviceActivationPolicy);
        }

        // Signing-policy preparation comes before the swap-contract validation (CRD R35.1.2 phase 4).
        #[cfg(target_arch = "wasm32")]
        let metamask = if matches!(policy, EthPrivKeyPolicy::Metamask) {
            Some(prepare_metamask(&ctx, &ticker, &platform_conf).await?)
        } else {
            None
        };

        let params = activation_params(&ctx, &platform_conf, &activation_request)?;

        // On WASM the rpc mode and the signing policy are both MetaMask or neither (CRD R35.1.4a).
        #[cfg(target_arch = "wasm32")]
        {
            let rpc_mode_metamask = activation_request.rpc_mode == Some(EthRpcMode::Metamask);
            if rpc_mode_metamask != matches!(policy, EthPrivKeyPolicy::Metamask) {
                return MmError::err(ActivationErr::PlatformCoinCreationError {
                    ticker,
                    error: "rpc_mode and priv_key_policy must be both Metamask or neither".to_owned(),
                });
            }
        }

        #[cfg(target_arch = "wasm32")]
        if let Some(metamask) = metamask {
            return coins::eth::eth_coin_from_params_with_metamask(
                &ctx,
                &ticker,
                &platform_conf,
                &params,
                metamask,
                protocol,
            )
            .await
            .mm_err(ActivationErr::from);
        }

        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|e| ActivationErr::Internal(e.to_string()))?;
        match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::Iguana => eth_coin_from_params_with_priv_key(
                &ctx,
                &ticker,
                &platform_conf,
                &params,
                crypto_ctx.mm2_internal_privkey_slice(),
                protocol,
            )
            .await
            .mm_err(ActivationErr::from),
            KeyPairPolicy::GlobalHDAccount(global_hd) => eth_coin_from_params_with_global_hd(
                &ctx,
                &ticker,
                &platform_conf,
                &params,
                global_hd,
                &activation_request.path_to_address(),
                activation_request.gap_limit(),
                protocol,
            )
            .await
            .mm_err(ActivationErr::from),
        }
    }

    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String> {
        activation_request
            .erc20_tokens_requests
            .iter()
            .map(|token| token.ticker.clone())
            .collect()
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(Erc20TokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result<XPubExtractor>(
        &self,
        ctx: &MmArc,
        activation_request: &Self::ActivationRequest,
        xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<EthWithTokensActivationResult, MmError<Self::ActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        // The inline NFT activation comes before the chain reads (CRD R35.1.2 phases 6 and 7).
        let nfts_infos = match &activation_request.nft_req {
            Some(nft_req) => activate_inline_nft(self, ctx, nft_req).await?,
            None => HashMap::new(),
        };

        // The docs document `Transport` for a chain-height read that gets no usable answer, which is explicit.
        // The reference release answers `Internal` there. Reloaded follows the docs (CRD ch.35 R35.1.6).
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(ActivationErr::Transport)?;

        match &self.derivation_method {
            coins::DerivationMethod::Iguana(_) => {
                iguana_activation_result(self, activation_request, current_block, nfts_infos).await
            },
            coins::DerivationMethod::HDWallet(hd_wallet) => {
                let mut wallet_balance = self
                    .enable_hd_wallet(
                        hd_wallet,
                        xpub_extractor,
                        activation_request.scan_policy.unwrap_or_default(),
                        activation_request.min_addresses_to_report(),
                    )
                    .await
                    .mm_err(enable_balance_error)?;
                self.add_erc20_balances_to_hd_report(&mut wallet_balance)
                    .await
                    .mm_err(balance_error)?;
                Ok(EthWithTokensActivationResult::HD(HDEthWithTokensActivationResult {
                    current_block,
                    ticker: self.ticker().to_owned(),
                    wallet_balance: EnableCoinBalance::HD(wallet_balance),
                    nfts_infos,
                }))
            },
        }
    }

    fn on_platform_registered(ctx: &MmArc, ticker: &str, activation_request: &Self::ActivationRequest) {
        if activation_request.nft_req.is_none() {
            return;
        }
        if let (Ok(chain), Ok(nft_ctx)) = (Chain::from_coin_ticker(ticker), NftCtx::from_mm_ctx(ctx)) {
            nft_ctx.mark_activated(chain);
        }
    }

    fn start_history_background_fetching(
        &self,
        _ctx: mm2_core::mm_ctx::MmArc,
        _metrics: MetricsArc,
        _storage: impl TxHistoryStorage + 'static,
        _initial_balance: BigDecimal,
    ) -> AbortHandle {
        let ticker = self.ticker().to_owned();
        let coin = self.clone();
        let (fut, abort_handle) = abortable(async move { coin.process_eth_history_loop().await });
        spawn(async move {
            if fut.await.is_err() {
                info!("eth history loop stopped for {}", ticker);
            }
        });
        abort_handle
    }
}

/// The inline NFT activation of the platform (CRD R35.1.4b): the same initial owned-inventory fetch as
/// `enable_nft`, for the single address of the platform. The NFT pseudo-coin is marked active once the
/// platform is registered.
async fn activate_inline_nft(
    coin: &EthCoin,
    ctx: &MmArc,
    nft_req: &NftReq,
) -> Result<HashMap<String, NftInfo>, MmError<ActivationErr>> {
    if !matches!(coin.derivation_method, coins::DerivationMethod::Iguana(_)) {
        return MmError::err(ActivationErr::UnexpectedDerivationMethod(
            "The inline NFT activation needs a single-address platform; use enable_nft after the activation".to_owned(),
        ));
    }
    let chain = Chain::from_coin_ticker(coin.ticker())
        .map_to_mm(|_| ActivationErr::Internal(format!("Platform coin {} has no NFT support", coin.ticker())))?;
    let nft_ctx = NftCtx::from_mm_ctx(ctx).map_to_mm(ActivationErr::Internal)?;
    let owned = fetch_and_store_owned_nfts(&nft_ctx, chain, coin.enabled_address(), nft_req.provider())
        .await
        .mm_err(|e| match e {
            EnableNftError::CrawlFailed(e) => ActivationErr::Transport(e),
            EnableNftError::ProviderRejectedRequest(e) => ActivationErr::InvalidPayload(e),
            other => ActivationErr::Internal(other.to_string()),
        })?;
    Ok(nfts_infos_from_inventory(owned))
}

/// The single-address result (CRD R35.1.3(a)).
async fn iguana_activation_result(
    coin: &EthCoin,
    activation_request: &EthWithTokensActivationRequest,
    current_block: u64,
    nfts_infos: HashMap<String, NftInfo>,
) -> Result<EthWithTokensActivationResult, MmError<ActivationErr>> {
    let my_address = coin.my_address().map_to_mm(ActivationErr::Internal)?;
    let pubkey = coin.activation_public_key();

    let (eth_balances, erc20_balances, tickers) = if activation_request.get_balances() {
        let eth_balance = coin.my_balance().compat().await.mm_err(balance_error)?;
        let token_balances = coin.get_tokens_balance_list().await.mm_err(balance_error)?;
        (Some(eth_balance), Some(token_balances), None)
    } else {
        let mut tickers: Vec<String> = coin.get_erc20_tokens_infos().into_keys().collect();
        tickers.sort();
        (None, None, Some(tickers))
    };

    let mut eth_addresses_infos = HashMap::new();
    eth_addresses_infos.insert(my_address.clone(), EvmAddressInfo {
        derivation_method: DerivationMethod::Iguana,
        pubkey: pubkey.clone(),
        balances: eth_balances,
        tickers: None,
    });
    let mut erc20_addresses_infos = HashMap::new();
    erc20_addresses_infos.insert(my_address, EvmAddressInfo {
        derivation_method: DerivationMethod::Iguana,
        pubkey,
        balances: erc20_balances,
        tickers,
    });
    Ok(EthWithTokensActivationResult::Iguana(
        IguanaEthWithTokensActivationResult {
            current_block,
            eth_addresses_infos,
            erc20_addresses_infos,
            nfts_infos,
        },
    ))
}

/// Resolves the connected MetaMask session of the MetaMask policy and prepares the wallet for the coin's
/// chain (CRD R47.5.2 -- R47.5.4, R47.6.7).
#[cfg(target_arch = "wasm32")]
async fn prepare_metamask(
    ctx: &MmArc,
    ticker: &str,
    platform_conf: &Json,
) -> Result<crypto::MetamaskArc, MmError<ActivationErr>> {
    // A MetaMask session must already be connected (via task::connect_metamask::init); activation does not
    // perform the handshake itself.
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| ActivationErr::Internal(e.to_string()))?;
    let metamask = crypto_ctx.metamask_ctx().or_mm_err(|| {
        ActivationErr::Transport(
            "MetaMask session is not initialized; call task::connect_metamask::init first".to_owned(),
        )
    })?;
    // Bind only the account the connected session authenticated: reject if the wallet's active account
    // drifted before broadcasting any activation-bound address.
    metamask
        .check_active_eth_account()
        .await
        .mm_err(|e| ActivationErr::Transport(e.to_string()))?;
    let chain_id = platform_conf["chain_id"]
        .as_u64()
        .or_mm_err(|| ActivationErr::PlatformCoinCreationError {
            ticker: ticker.to_owned(),
            error: "the coin configuration has no chain_id".to_owned(),
        })?;
    metamask.ensure_active_chain(chain_id).await.mm_err(|e| match e {
        crypto::MetamaskError::Internal(e) => ActivationErr::Internal(e),
        other => ActivationErr::PlatformCoinCreationError {
            ticker: ticker.to_owned(),
            error: other.to_string(),
        },
    })?;
    Ok(metamask)
}

/// Per-coin task registry for the EVM `task::enable_eth::*` family (CRD ch. 48).
pub type EthTaskManagerShared =
    crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensTaskManagerShared<EthCoin>;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl InitPlatformCoinWithTokensActivationOps for EthCoin {
    fn rpc_task_manager(activation_ctx: &crate::context::CoinsActivationContext) -> &EthTaskManagerShared {
        &activation_ctx.init_eth_task_manager
    }

    async fn enable_platform_coin_with_task(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: EthWithTokensActivationRequest,
        protocol_conf: EthProtocolInfo,
        priv_key: &[u8],
        task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<Self>>,
    ) -> Result<Self, MmError<ActivationErr>> {
        // The Trezor policy sources the coin's address / account public key from the connected device,
        // driving the interaction through `task_handle` (R50.1 / R50.4). Every other policy is
        // non-interactive and builds the coin exactly as the one-shot activator (R48.6.1).
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        if matches!(activation_request.priv_key_policy, Some(EthPrivKeyPolicy::Trezor)) {
            return enable_eth_platform_coin_trezor(
                ctx,
                ticker,
                coin_conf,
                activation_request,
                protocol_conf,
                task_handle,
            )
            .await;
        }

        let _ = task_handle;
        <Self as PlatformWithTokensActivationOps>::enable_platform_coin(
            ctx,
            ticker,
            coin_conf,
            activation_request,
            protocol_conf,
            priv_key,
        )
        .await
    }
}

/// Build an EVM platform coin under the Trezor policy (CRD R35.3.2): require an initialized hardware-wallet
/// context and a coin configuration with hardware support, then source the enabled address / account public key
/// from the device and build the coin as an HD wallet under `EthSigner::Trezor` (R50.1 / R50.4). Connect /
/// PIN / passphrase / confirmation are surfaced through the activation task's status / user-action vocabulary.
/// Every hardware failure is reported as `Internal`.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
async fn enable_eth_platform_coin_trezor(
    ctx: MmArc,
    ticker: String,
    platform_conf: Json,
    activation_request: EthWithTokensActivationRequest,
    protocol_conf: EthProtocolInfo,
    task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<EthCoin>>,
) -> Result<EthCoin, MmError<ActivationErr>> {
    let protocol = protocol_conf.coin_protocol(&ticker)?;

    // Signing-policy preparation: a device context and hardware support of the coin. Neither needs the device.
    let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|e| ActivationErr::Internal(e.to_string()))?;
    if crypto_ctx.hw_ctx().is_none() {
        return MmError::err(ActivationErr::Internal(
            "Trezor device is not initialized; connect a Trezor (task::init_trezor) first".to_owned(),
        ));
    }
    if platform_conf["trezor_coin"].as_str().is_none() {
        return MmError::err(ActivationErr::Internal(format!(
            "Coin {} does not support the Trezor hardware wallet",
            ticker
        )));
    }

    let params = activation_params(&ctx, &platform_conf, &activation_request)?;

    let processor = eth_activation_trezor_connect_processor(task_handle);
    coins::eth::eth_coin_activate_with_trezor_hd(
        &ctx,
        &ticker,
        &platform_conf,
        &params,
        protocol,
        &activation_request.path_to_address(),
        activation_request.gap_limit(),
        &processor,
    )
    .await
    .mm_err(ActivationErr::from)
}

/// Device connect / interaction time budget shared with the withdraw path (R50.16).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
const TREZOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
const TREZOR_PIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Map the device connect / PIN / passphrase / confirmation requests onto the platform activation task's
/// in-progress / awaiting-status vocabulary (R48.3.3 / R50.13 / R50.14).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
fn eth_activation_trezor_connect_processor(
    task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<EthCoin>>,
) -> TrezorRpcTaskConnectProcessor<'_, InitPlatformCoinWithTokensTask<EthCoin>> {
    TrezorRpcTaskConnectProcessor::new(
        task_handle,
        crate::init_platform_coin_with_tokens::xpub_extractor_rpc_statuses(),
    )
    .with_connect_timeout(TREZOR_CONNECT_TIMEOUT)
    .with_pin_timeout(TREZOR_PIN_TIMEOUT)
}

#[cfg(all(test, not(target_arch = "wasm32")))] mod tests;
