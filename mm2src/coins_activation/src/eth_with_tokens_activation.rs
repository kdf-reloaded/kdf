//! EVM platform-coin-with-tokens activation (CRD §35.1 -- `enable_eth_with_tokens`).
//!
//! Implements the generic platform-coin-with-tokens activation traits for the
//! EVM platform coin (`EthCoin` with native `Eth` coin-type), so the framework's
//! `enable_platform_coin_with_tokens::<EthCoin>` entrypoint can activate an EVM
//! platform coin plus an inline batch of ERC-20 tokens in one call.
//!
//! This module covers the single-address ("Iguana") activation path: the
//! generic activator passes a secp256k1 private key, so HD / hardware / external
//! signer policies (R35.1.4) -- which require the task variant of §35.3 -- are
//! out of scope here.

use crate::erc20_token_activation::{Erc20ActivationRequest, Erc20Protocol};
use crate::platform_coin_with_tokens::*;
use crate::prelude::*;
use async_trait::async_trait;
#[cfg(target_arch = "wasm32")]
use coins::eth::eth_coin_from_conf_and_request_with_metamask;
use coins::eth::{eth_coin_from_conf_and_request, EthCoin};
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::{CoinBalance, CoinProtocol, MarketCoinOps, MmCoin, UnexpectedDerivationMethod};
use common::executor::spawn;
use common::log::info;
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, TrezorRpcTaskConnectProcessor};
#[cfg(target_arch = "wasm32")] use crypto::CryptoCtx;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use crypto::CryptoCtx;
use futures::future::{abortable, AbortHandle};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use rpc_task::RpcTaskHandle;
use serde_derive::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use std::time::Duration;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
use crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensInProgressStatus;
use crate::init_platform_coin_with_tokens::{InitPlatformCoinWithTokensActivationOps, InitPlatformCoinWithTokensTask};

// The EVM token is itself an `EthCoin` (with an `Erc20` coin-type), so the
// platform coin and the token share a Rust type.
impl TokenOf for EthCoin {
    type PlatformCoin = EthCoin;
}

/// A single EVM JSON-RPC endpoint (R35.1.1 `nodes` entry).
#[derive(Clone, Debug, Deserialize)]
pub struct EthNode {
    pub url: String,
    /// Whether the endpoint is reached through the project's proxy. Accepted for
    /// wire-compatibility; the legacy EVM builder does not currently consume it.
    #[serde(default)]
    #[allow(dead_code)]
    pub komodo_proxy: bool,
}

/// `enable_eth_with_tokens` request parameters (R35.1.1). The single-address
/// activation subset; HD / device / NFT parameters are not consumed here.
#[derive(Clone, Debug, Deserialize)]
pub struct EthWithTokensActivationRequest {
    nodes: Vec<EthNode>,
    #[serde(default)]
    erc20_tokens_requests: Vec<TokenActivationRequest<Erc20ActivationRequest>>,
    #[serde(default)]
    swap_contract_address: Option<String>,
    #[serde(default)]
    fallback_swap_contract: Option<String>,
    #[serde(default)]
    required_confirmations: Option<u64>,
    #[serde(default)]
    tx_history: bool,
    /// EVM signing policy (CRD R47.5.1). Defaults to `Iguana` (local secret).
    #[serde(default)]
    priv_key_policy: EthActivationPolicy,
}

impl TxHistory for EthWithTokensActivationRequest {
    fn tx_history(&self) -> bool { self.tx_history }
}

/// EVM activation signing policy (CRD R47.5.1 / R35.1.4), selected via the
/// `priv_key_policy` request field as a tagged object (`{"type":"Iguana"}` /
/// `{"type":"Metamask"}`). `Iguana` (the default) signs with the
/// centrally-threaded local secret, exactly as before. `Metamask` delegates
/// signing and broadcast to a connected browser MetaMask session and is defined
/// **only on the WASM target** (CRD R47.7.1); on native builds the variant does
/// not exist, so a `{"type":"Metamask"}` request fails deserialization and is
/// rejected (CRD R47.7.2 / R47.6.7).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "type")]
pub enum EthActivationPolicy {
    #[serde(alias = "IguanaPrivKey", alias = "ContextPrivKey")]
    #[default]
    Iguana,
    #[cfg(target_arch = "wasm32")]
    Metamask,
    /// Trezor hardware-wallet policy (CRD R35.1.4 / §50). Native, non-iOS only;
    /// requires the interactive `task::enable_eth` path so connect / PIN /
    /// passphrase / address-confirmation states can be surfaced and answered.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
    Trezor,
}

/// EVM platform protocol info resolved from coin configuration.
pub struct EthProtocolInfo {
    chain_id: Option<u64>,
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

/// Token initializer enabling the inline ERC-20 tokens of an
/// `enable_eth_with_tokens` request.
pub struct Erc20TokenInitializer {
    platform_coin: EthCoin,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenInitializer for Erc20TokenInitializer {
    type Token = EthCoin;
    type TokenActivationRequest = Erc20ActivationRequest;
    type TokenProtocol = Erc20Protocol;
    type InitTokensError = EthTokenInitError;

    fn tokens_requests_from_platform_request(
        platform_params: &EthWithTokensActivationRequest,
    ) -> Vec<TokenActivationRequest<Self::TokenActivationRequest>> {
        platform_params.erc20_tokens_requests.clone()
    }

    async fn enable_tokens(
        &self,
        activation_params: Vec<TokenActivationParams<Erc20ActivationRequest, Erc20Protocol>>,
    ) -> Result<Vec<EthCoin>, MmError<EthTokenInitError>> {
        let mut tokens = Vec::with_capacity(activation_params.len());
        for params in activation_params {
            let token_addr =
                coins::eth::addr_from_str(&params.protocol.contract_address).map_to_mm(EthTokenInitError::Internal)?;
            // confirmation settings from the RPC request have the highest priority
            let required_confirmations = params
                .activation_request
                .required_confirmations
                .unwrap_or_else(|| self.platform_coin.required_confirmations());
            let token = self
                .platform_coin
                .erc20_token_from_conf_or_contract(params.ticker, token_addr, required_confirmations)
                .await
                .map_to_mm(EthTokenInitError::Internal)?;
            tokens.push(token);
        }
        Ok(tokens)
    }

    fn platform_coin(&self) -> &EthCoin { &self.platform_coin }
}

#[derive(Debug)]
pub enum EthTokenInitError {
    Internal(String),
}

impl From<EthTokenInitError> for InitTokensAsMmCoinsError {
    fn from(err: EthTokenInitError) -> Self {
        match err {
            EthTokenInitError::Internal(e) => InitTokensAsMmCoinsError::InvalidPubkey(e),
        }
    }
}

impl RegisterTokenInfo<EthCoin> for EthCoin {
    fn register_token_info(&self, token: &EthCoin) {
        if let Some(info) = token.erc20_token_info() {
            self.add_erc20_token_info(token.ticker().into(), info);
        }
    }
}

/// Single-address ("Iguana") activation result (R35.1.3).
#[derive(Clone, Debug, Serialize)]
pub struct EthWithTokensActivationResult {
    current_block: u64,
    eth_addresses_infos: HashMap<String, CoinAddressInfo<CoinBalance>>,
    erc20_addresses_infos: HashMap<String, CoinAddressInfo<TokenBalances>>,
}

impl GetPlatformBalance for EthWithTokensActivationResult {
    fn get_platform_balance(&self) -> BigDecimal {
        self.eth_addresses_infos
            .iter()
            .fold(BigDecimal::from(0), |total, (_, addr_info)| {
                &total + &addr_info.balances.get_total()
            })
    }
}

impl CurrentBlock for EthWithTokensActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

#[derive(Debug)]
pub enum EthWithTokensActivationError {
    PlatformCoinCreationError { ticker: String, error: String },
    AtLeastOneNodeRequired,
    UnexpectedDerivationMethod(String),
    Transport(String),
    Internal(String),
}

impl From<EthWithTokensActivationError> for EnablePlatformCoinWithTokensError {
    fn from(err: EthWithTokensActivationError) -> Self {
        match err {
            EthWithTokensActivationError::PlatformCoinCreationError { ticker, error } => {
                EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error }
            },
            EthWithTokensActivationError::AtLeastOneNodeRequired => {
                EnablePlatformCoinWithTokensError::AtLeastOneNodeRequired
            },
            EthWithTokensActivationError::UnexpectedDerivationMethod(e) => {
                EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(e)
            },
            EthWithTokensActivationError::Transport(e) => EnablePlatformCoinWithTokensError::Transport(e),
            EthWithTokensActivationError::Internal(e) => EnablePlatformCoinWithTokensError::Internal(e),
        }
    }
}

impl From<UnexpectedDerivationMethod> for EthWithTokensActivationError {
    fn from(e: UnexpectedDerivationMethod) -> Self {
        EthWithTokensActivationError::UnexpectedDerivationMethod(e.to_string())
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PlatformWithTokensActivationOps for EthCoin {
    type ActivationRequest = EthWithTokensActivationRequest;
    type PlatformProtocolInfo = EthProtocolInfo;
    type ActivationResult = EthWithTokensActivationResult;
    type ActivationError = EthWithTokensActivationError;

    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        platform_conf: Json,
        activation_request: Self::ActivationRequest,
        protocol_conf: Self::PlatformProtocolInfo,
        priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        if activation_request.nodes.is_empty() {
            return MmError::err(EthWithTokensActivationError::AtLeastOneNodeRequired);
        }

        let urls: Vec<String> = activation_request.nodes.iter().map(|node| node.url.clone()).collect();
        let mut req = json!({
            "urls": urls,
            "tx_history": activation_request.tx_history,
        });
        if let Some(addr) = &activation_request.swap_contract_address {
            req["swap_contract_address"] = json!(addr);
        }
        if let Some(addr) = &activation_request.fallback_swap_contract {
            req["fallback_swap_contract"] = json!(addr);
        }
        if let Some(required_confirmations) = activation_request.required_confirmations {
            req["required_confirmations"] = json!(required_confirmations);
        }

        let protocol = CoinProtocol::ETH {
            chain_id: protocol_conf.chain_id,
        };

        // CRD R47.5.A -- resolve the signing policy. `Iguana` signs with the
        // centrally-threaded local secret (unchanged); `Metamask` (WASM only)
        // ignores `priv_key` and binds the coin to the connected MetaMask
        // session.
        let platform_coin = match activation_request.priv_key_policy {
            EthActivationPolicy::Iguana => {
                eth_coin_from_conf_and_request(&ctx, &ticker, &platform_conf, &req, priv_key, protocol)
                    .await
                    .map_to_mm(|error| EthWithTokensActivationError::PlatformCoinCreationError {
                        ticker: ticker.clone(),
                        error,
                    })?
            },
            #[cfg(target_arch = "wasm32")]
            EthActivationPolicy::Metamask => {
                // CRD R47.5.2 -- a MetaMask session must already be connected
                // (via task::connect_metamask::init); activation does not perform
                // the handshake itself.
                let crypto_ctx =
                    CryptoCtx::from_ctx(&ctx).mm_err(|e| EthWithTokensActivationError::Internal(e.to_string()))?;
                let metamask_arc = crypto_ctx.metamask_ctx().or_mm_err(|| {
                    EthWithTokensActivationError::Transport(
                        "MetaMask session is not initialized; call task::connect_metamask::init first".to_string(),
                    )
                })?;
                // CRD R47.5.4 -- bind only the account the connected session
                // authenticated; reject if the wallet's active account drifted
                // before broadcasting any activation-bound address.
                metamask_arc
                    .check_active_eth_account()
                    .await
                    .mm_err(|e| EthWithTokensActivationError::Transport(e.to_string()))?;
                eth_coin_from_conf_and_request_with_metamask(
                    &ctx,
                    &ticker,
                    &platform_conf,
                    &req,
                    metamask_arc,
                    protocol,
                )
                .await
                .map_to_mm(|error| EthWithTokensActivationError::PlatformCoinCreationError {
                    ticker: ticker.clone(),
                    error,
                })?
            },
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthActivationPolicy::Trezor => {
                // The Trezor policy needs the interactive connect / PIN /
                // passphrase / address-confirmation exchange, which only the
                // task path (`task::enable_eth`) can drive; the non-task
                // one-shot activator has no task handle to surface those states.
                return MmError::err(EthWithTokensActivationError::Transport(
                    "Trezor activation requires the interactive task::enable_eth path".to_owned(),
                ));
            },
        };
        Ok(platform_coin)
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(Erc20TokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result(
        &self,
    ) -> Result<EthWithTokensActivationResult, MmError<EthWithTokensActivationError>> {
        let my_address = self.my_address().map_to_mm(EthWithTokensActivationError::Internal)?;
        let pubkey = self.display_public_key();

        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(EthWithTokensActivationError::Transport)?;

        let eth_balance = self
            .my_balance()
            .compat()
            .await
            .mm_err(|e| EthWithTokensActivationError::Transport(e.to_string()))?;

        let token_balances = self
            .get_tokens_balance_list()
            .await
            .mm_err(|e| EthWithTokensActivationError::Transport(e.to_string()))?;

        let mut result = EthWithTokensActivationResult {
            current_block,
            eth_addresses_infos: HashMap::new(),
            erc20_addresses_infos: HashMap::new(),
        };

        result.eth_addresses_infos.insert(my_address.clone(), CoinAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey: pubkey.clone(),
            balances: eth_balance,
        });

        result.erc20_addresses_infos.insert(my_address, CoinAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey,
            balances: token_balances,
        });

        Ok(result)
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
    ) -> Result<Self, MmError<EthWithTokensActivationError>> {
        // The Trezor policy sources the coin's address / account public key from
        // the connected device, driving the interaction through `task_handle`
        // (R50.1 / R50.4). Every other policy is non-interactive and builds the
        // coin exactly as the one-shot activator (R48.6.1).
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        if matches!(activation_request.priv_key_policy, EthActivationPolicy::Trezor) {
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

/// Build an EVM platform coin under the Trezor policy: require an initialized
/// hardware-wallet context (R35.1.5, mirroring the MetaMask "must be connected"
/// precedent), then source the enabled address / account public key from the
/// device and build the coin under `EthSigner::Trezor` (R50.1 / R50.4). Connect
/// / PIN / passphrase / confirmation are surfaced through the activation task's
/// status / user-action vocabulary.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
async fn enable_eth_platform_coin_trezor(
    ctx: MmArc,
    ticker: String,
    platform_conf: Json,
    activation_request: EthWithTokensActivationRequest,
    protocol_conf: EthProtocolInfo,
    task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<EthCoin>>,
) -> Result<EthCoin, MmError<EthWithTokensActivationError>> {
    if activation_request.nodes.is_empty() {
        return MmError::err(EthWithTokensActivationError::AtLeastOneNodeRequired);
    }

    // R35.1.5: a Trezor device must already be initialized (mirror MetaMask).
    let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|e| EthWithTokensActivationError::Internal(e.to_string()))?;
    crypto_ctx.hw_ctx().or_mm_err(|| {
        EthWithTokensActivationError::Transport(
            "Trezor device is not initialized; connect a Trezor (task::init_trezor) first".to_string(),
        )
    })?;

    let urls: Vec<String> = activation_request.nodes.iter().map(|node| node.url.clone()).collect();
    let mut req = json!({
        "urls": urls,
        "tx_history": activation_request.tx_history,
    });
    if let Some(addr) = &activation_request.swap_contract_address {
        req["swap_contract_address"] = json!(addr);
    }
    if let Some(addr) = &activation_request.fallback_swap_contract {
        req["fallback_swap_contract"] = json!(addr);
    }
    if let Some(required_confirmations) = activation_request.required_confirmations {
        req["required_confirmations"] = json!(required_confirmations);
    }

    let protocol = CoinProtocol::ETH {
        chain_id: protocol_conf.chain_id,
    };

    let processor = eth_activation_trezor_connect_processor(task_handle);
    coins::eth::eth_coin_activate_with_trezor(&ctx, &ticker, &platform_conf, &req, protocol, &processor)
        .await
        .map_to_mm(|error| EthWithTokensActivationError::PlatformCoinCreationError { ticker, error })
}

/// Device connect / interaction time budget shared with the withdraw path (R50.16).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
const TREZOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
const TREZOR_PIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Map the device connect / PIN / passphrase / confirmation requests onto the
/// platform activation task's in-progress / awaiting-status vocabulary (R48.6.2
/// / R50.13 / R50.14).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
fn eth_activation_trezor_connect_processor(
    task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<EthCoin>>,
) -> TrezorRpcTaskConnectProcessor<'_, InitPlatformCoinWithTokensTask<EthCoin>> {
    TrezorRpcTaskConnectProcessor::new(task_handle, HwConnectStatuses {
        on_connect: InitPlatformCoinWithTokensInProgressStatus::WaitingForTrezorToConnect,
        on_connected: InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin,
        on_connection_failed: InitPlatformCoinWithTokensInProgressStatus::Finishing,
        on_button_request: InitPlatformCoinWithTokensInProgressStatus::WaitingForUserToConfirmPubkey,
        on_pin_request: HwRpcTaskAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: HwRpcTaskAwaitingStatus::EnterTrezorPassphrase,
        on_ready: InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin,
    })
    .with_connect_timeout(TREZOR_CONNECT_TIMEOUT)
    .with_pin_timeout(TREZOR_PIN_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_request_deserializes_with_defaults() {
        let req: EthWithTokensActivationRequest = serde_json::from_str(
            r#"{
                "nodes": [
                    {"url": "https://node1.example"},
                    {"url": "https://node2.example", "komodo_proxy": true}
                ],
                "swap_contract_address": "0x24ABE4c71FC658C91313b6552cd40cD808b3Ea80"
            }"#,
        )
        .unwrap();
        assert_eq!(req.nodes.len(), 2);
        assert_eq!(req.nodes[0].url, "https://node1.example");
        assert!(!req.nodes[0].komodo_proxy);
        assert!(req.nodes[1].komodo_proxy);
        assert!(req.erc20_tokens_requests.is_empty());
        assert!(!req.tx_history);
        assert_eq!(
            req.swap_contract_address.as_deref(),
            Some("0x24ABE4c71FC658C91313b6552cd40cD808b3Ea80")
        );
        assert!(req.fallback_swap_contract.is_none());
        assert!(req.required_confirmations.is_none());
        assert!(!req.tx_history());
    }

    #[test]
    fn protocol_info_from_eth_coin_protocol() {
        let parsed = EthProtocolInfo::try_from_coin_protocol(CoinProtocol::ETH { chain_id: Some(1) }).unwrap();
        assert_eq!(parsed.chain_id, Some(1));
    }

    #[test]
    fn protocol_info_rejects_non_eth() {
        let proto = CoinProtocol::ERC20 {
            platform: "ETH".to_owned(),
            contract_address: "0x0".to_owned(),
        };
        assert!(EthProtocolInfo::try_from_coin_protocol(proto).is_err());
    }

    #[test]
    fn activation_result_serializes_expected_shape() {
        let mut eth_addresses_infos = HashMap::new();
        eth_addresses_infos.insert("0xabc".to_owned(), CoinAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey: "0xpub".to_owned(),
            balances: CoinBalance {
                spendable: BigDecimal::from(10),
                unspendable: BigDecimal::from(0),
            },
        });
        let mut erc20_addresses_infos = HashMap::new();
        erc20_addresses_infos.insert("0xabc".to_owned(), CoinAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey: "0xpub".to_owned(),
            balances: TokenBalances::new(),
        });
        let result = EthWithTokensActivationResult {
            current_block: 42,
            eth_addresses_infos,
            erc20_addresses_infos,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["current_block"], 42);
        assert_eq!(v["eth_addresses_infos"]["0xabc"]["balances"]["spendable"], "10");
        assert_eq!(v["eth_addresses_infos"]["0xabc"]["derivation_method"]["type"], "Iguana");
        assert!(v["erc20_addresses_infos"]["0xabc"]["balances"].is_object());
    }

    #[test]
    fn get_platform_balance_sums_eth_addresses() {
        let mut eth_addresses_infos = HashMap::new();
        eth_addresses_infos.insert("0xabc".to_owned(), CoinAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey: "0xpub".to_owned(),
            balances: CoinBalance {
                spendable: BigDecimal::from(3),
                unspendable: BigDecimal::from(2),
            },
        });
        let result = EthWithTokensActivationResult {
            current_block: 1,
            eth_addresses_infos,
            erc20_addresses_infos: HashMap::new(),
        };
        assert_eq!(result.get_platform_balance(), BigDecimal::from(5));
        assert_eq!(result.current_block(), 1);
    }

    #[test]
    fn activation_policy_accepts_context_priv_key_alias() {
        let req: EthWithTokensActivationRequest = serde_json::from_str(
            r#"{
                "nodes": [{"url": "https://node1.example"}],
                "priv_key_policy": {"type": "ContextPrivKey"}
            }"#,
        )
        .unwrap();

        assert!(matches!(req.priv_key_policy, EthActivationPolicy::Iguana));
    }
}
