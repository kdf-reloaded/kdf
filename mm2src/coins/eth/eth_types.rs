//! Ethereum coin types, constants, contract ABIs, and error conversions.

use super::*;

/// https://github.com/artemii235/etomic-swap/blob/master/contracts/EtomicSwap.sol
/// Dev chain (195.201.0.6:8565) contract address: 0xa09ad3cd7e96586ebd05a2607ee56b56fb2db8fd
/// Ropsten: https://ropsten.etherscan.io/address/0x7bc1bbdd6a0a722fc9bffc49c921b685ecb84b94
/// ETH mainnet: https://etherscan.io/address/0x8500AFc0bc5214728082163326C2FF0C73f4a871
pub(crate) const SWAP_CONTRACT_ABI: &str = r#"[{"constant":false,"inputs":[{"name":"_id","type":"bytes32"},{"name":"_amount","type":"uint256"},{"name":"_secret","type":"bytes32"},{"name":"_tokenAddress","type":"address"},{"name":"_sender","type":"address"}],"name":"receiverSpend","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[{"name":"","type":"bytes32"}],"name":"payments","outputs":[{"name":"paymentHash","type":"bytes20"},{"name":"lockTime","type":"uint64"},{"name":"state","type":"uint8"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"_id","type":"bytes32"},{"name":"_receiver","type":"address"},{"name":"_secretHash","type":"bytes20"},{"name":"_lockTime","type":"uint64"}],"name":"ethPayment","outputs":[],"payable":true,"stateMutability":"payable","type":"function"},{"constant":false,"inputs":[{"name":"_id","type":"bytes32"},{"name":"_amount","type":"uint256"},{"name":"_paymentHash","type":"bytes20"},{"name":"_tokenAddress","type":"address"},{"name":"_receiver","type":"address"}],"name":"senderRefund","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"name":"_id","type":"bytes32"},{"name":"_amount","type":"uint256"},{"name":"_tokenAddress","type":"address"},{"name":"_receiver","type":"address"},{"name":"_secretHash","type":"bytes20"},{"name":"_lockTime","type":"uint64"}],"name":"erc20Payment","outputs":[],"payable":true,"stateMutability":"payable","type":"function"},{"inputs":[],"payable":false,"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":false,"name":"id","type":"bytes32"}],"name":"PaymentSent","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"name":"id","type":"bytes32"},{"indexed":false,"name":"secret","type":"bytes32"}],"name":"ReceiverSpent","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"name":"id","type":"bytes32"}],"name":"SenderRefunded","type":"event"}]"#;
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-20.md
pub(crate) const ERC20_ABI: &str = r#"[{"constant":true,"inputs":[],"name":"name","outputs":[{"name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"_spender","type":"address"},{"name":"_value","type":"uint256"}],"name":"approve","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"totalSupply","outputs":[{"name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"},{"name":"_value","type":"uint256"}],"name":"transferFrom","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"decimals","outputs":[{"name":"","type":"uint8"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"_spender","type":"address"},{"name":"_subtractedValue","type":"uint256"}],"name":"decreaseApproval","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[{"name":"_owner","type":"address"}],"name":"balanceOf","outputs":[{"name":"balance","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"symbol","outputs":[{"name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"_to","type":"address"},{"name":"_value","type":"uint256"}],"name":"transfer","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"name":"_spender","type":"address"},{"name":"_addedValue","type":"uint256"}],"name":"increaseApproval","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[{"name":"_owner","type":"address"},{"name":"_spender","type":"address"}],"name":"allowance","outputs":[{"name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"inputs":[],"payable":false,"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"name":"owner","type":"address"},{"indexed":true,"name":"spender","type":"address"},{"indexed":false,"name":"value","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"name":"from","type":"address"},{"indexed":true,"name":"to","type":"address"},{"indexed":false,"name":"value","type":"uint256"}],"name":"Transfer","type":"event"}]"#;

/// Payment states from etomic swap smart contract: https://github.com/artemii235/etomic-swap/blob/master/contracts/EtomicSwap.sol#L5
pub const PAYMENT_STATE_UNINITIALIZED: u8 = 0;
pub const PAYMENT_STATE_SENT: u8 = 1;
pub(crate) const _PAYMENT_STATE_SPENT: u8 = 2;
pub(crate) const _PAYMENT_STATE_REFUNDED: u8 = 3;
// Ethgasstation API returns response in 10^8 wei units. So 10 from their API mean 1 gwei
pub(crate) const ETH_GAS_STATION_DECIMALS: u8 = 8;
pub(crate) const GAS_PRICE_PERCENT: u64 = 10;
/// It can change 12.5% max each block according to https://www.blocknative.com/blog/eip-1559-fees
pub(crate) const BASE_BLOCK_FEE_DIFF_PCT: u64 = 13;
pub(crate) const DEFAULT_LOGS_BLOCK_RANGE: u64 = 1000;

/// Take into account that the dynamic fee may increase by 3% during the swap.
pub(crate) const GAS_PRICE_APPROXIMATION_PERCENT_ON_START_SWAP: u64 = 3;
/// Take into account that the dynamic fee may increase at each of the following stages:
/// - it may increase by 2% until a swap is started;
/// - it may increase by 3% during the swap.
pub(crate) const GAS_PRICE_APPROXIMATION_PERCENT_ON_ORDER_ISSUE: u64 = 5;
/// Take into account that the dynamic fee may increase at each of the following stages:
/// - it may increase by 2% until an order is issued;
/// - it may increase by 2% until a swap is started;
/// - it may increase by 3% during the swap.
pub(crate) const GAS_PRICE_APPROXIMATION_PERCENT_ON_TRADE_PREIMAGE: u64 = 7;

// V2 swap contract ABIs (from https://github.com/KomodoPlatform/etomic-swap)
pub(crate) const MAKER_SWAP_V2_ABI: &str = include_str!("maker_swap_v2_abi.json");
pub(crate) const TAKER_SWAP_V2_ABI: &str = include_str!("taker_swap_v2_abi.json");

lazy_static! {
    pub static ref SWAP_CONTRACT: Contract = Contract::load(SWAP_CONTRACT_ABI.as_bytes()).unwrap();
    pub static ref ERC20_CONTRACT: Contract = Contract::load(ERC20_ABI.as_bytes()).unwrap();
    pub(crate) static ref MAKER_SWAP_V2: Contract = Contract::load(MAKER_SWAP_V2_ABI.as_bytes()).unwrap();
    pub(crate) static ref TAKER_SWAP_V2: Contract = Contract::load(TAKER_SWAP_V2_ABI.as_bytes()).unwrap();
}

pub type Web3RpcFut<T> = Box<dyn Future<Item = T, Error = MmError<Web3RpcError>> + Send>;
pub type Web3RpcResult<T> = Result<T, MmError<Web3RpcError>>;
pub type GasStationResult = Result<GasStationData, MmError<GasStationReqErr>>;

#[derive(Debug, Display)]
pub enum GasStationReqErr {
    #[display(fmt = "Transport '{}' error: {}", uri, error)]
    Transport {
        uri: String,
        error: String,
    },
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    Internal(String),
}

impl From<serde_json::Error> for GasStationReqErr {
    fn from(e: serde_json::Error) -> Self { GasStationReqErr::InvalidResponse(e.to_string()) }
}

impl From<SlurpError> for GasStationReqErr {
    fn from(e: SlurpError) -> Self {
        let error = e.to_string();
        match e {
            SlurpError::ErrorDeserializing { .. } => GasStationReqErr::InvalidResponse(error),
            SlurpError::Transport { uri, .. } | SlurpError::Timeout { uri, .. } => {
                GasStationReqErr::Transport { uri, error }
            },
            SlurpError::Internal(_) | SlurpError::InvalidRequest(_) => GasStationReqErr::Internal(error),
        }
    }
}

#[derive(Debug, Display)]
pub enum Web3RpcError {
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

impl From<GasStationReqErr> for Web3RpcError {
    fn from(err: GasStationReqErr) -> Self {
        match err {
            GasStationReqErr::Transport { .. } => Web3RpcError::Transport(err.to_string()),
            GasStationReqErr::InvalidResponse(err) => Web3RpcError::InvalidResponse(err),
            GasStationReqErr::Internal(err) => Web3RpcError::Internal(err),
        }
    }
}

impl From<serde_json::Error> for Web3RpcError {
    fn from(e: serde_json::Error) -> Self { Web3RpcError::InvalidResponse(e.to_string()) }
}

impl From<crate::eth::abi::AbiError> for Web3RpcError {
    fn from(e: crate::eth::abi::AbiError) -> Web3RpcError {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        Web3RpcError::Internal(e.to_string())
    }
}

impl From<crate::eth::abi::AbiError> for WithdrawError {
    fn from(e: crate::eth::abi::AbiError) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        WithdrawError::InternalError(e.to_string())
    }
}

impl From<Web3RpcError> for WithdrawError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(err) | Web3RpcError::InvalidResponse(err) => WithdrawError::Transport(err),
            Web3RpcError::Internal(internal) => WithdrawError::InternalError(internal),
        }
    }
}

impl From<Web3RpcError> for TradePreimageError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(err) | Web3RpcError::InvalidResponse(err) => TradePreimageError::Transport(err),
            Web3RpcError::Internal(internal) => TradePreimageError::InternalError(internal),
        }
    }
}

impl From<Web3RpcError> for BalanceError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(err) | Web3RpcError::InvalidResponse(err) => BalanceError::Transport(err),
            Web3RpcError::Internal(internal) => BalanceError::Internal(internal),
        }
    }
}

impl From<crate::eth::abi::AbiError> for TradePreimageError {
    fn from(e: crate::eth::abi::AbiError) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        TradePreimageError::InternalError(e.to_string())
    }
}

impl From<crate::eth::abi::AbiError> for BalanceError {
    fn from(e: crate::eth::abi::AbiError) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        BalanceError::Internal(e.to_string())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SavedTraces {
    /// ETH traces for my_address
    pub(crate) traces: Vec<Trace>,
    /// Earliest processed block
    pub(crate) earliest_block: U256,
    /// Latest processed block
    pub(crate) latest_block: U256,
}

/// LP-17: replaces the legacy `web3_transport::FeeHistoryResult`.
///
/// Wire-compatible deserialiser for the `eth_feeHistory` JSON-RPC
/// response. Field names (`oldestBlock`, `baseFeePerGas`,
/// `gasUsedRatio`, `reward`) and value encodings match the Ethereum
/// RPC spec; numeric fields decode as `alloy::primitives::U256`,
/// which accepts the same `0x`-prefixed hex strings that the legacy
/// `web3::types::U256` did.
#[derive(Debug, Deserialize)]
pub struct FeeHistoryResult {
    #[serde(rename = "oldestBlock")]
    pub oldest_block: U256,
    #[serde(rename = "baseFeePerGas")]
    pub base_fee_per_gas: Vec<U256>,
    #[serde(rename = "gasUsedRatio")]
    pub gas_used_ratio: Option<Vec<f64>>,
    #[serde(rename = "reward")]
    pub priority_rewards: Option<Vec<Vec<U256>>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SavedErc20Events {
    /// ERC20 events for my_address
    pub(crate) events: Vec<Log>,
    /// Earliest processed block
    pub(crate) earliest_block: U256,
    /// Latest processed block
    pub(crate) latest_block: U256,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EthCoinType {
    /// Ethereum itself or it's forks: ETC/others
    Eth,
    /// ERC20 token with smart contract address
    /// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-20.md
    Erc20 { platform: String, token_addr: Address },
    /// Native TRON (TRX). Like `Eth` but the host blockchain uses
    /// SHA-256 hashing, protobuf transactions, Base58Check addresses
    /// and a bandwidth/energy fee model. Decimals are fixed at 6 (SUN).
    Tron,
    /// TRC20 token deployed as a smart contract on TRON. Holds the
    /// platform name (the TRX coin ticker the token rides on) and the
    /// 20-byte EVM-shaped contract address.
    Trc20 { platform: String, token_addr: Address },
}

/// Per-coin swap gas-fee policy (CRD R35.6.3). Governs how the EVM coin prices
/// gas for subsequent swap transactions: `Legacy` selects pre-EIP-1559
/// single gas-price pricing (the default); `Low`/`Medium`/`High` select an
/// EIP-1559 max-fee / max-priority-fee tier.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum SwapGasFeePolicy {
    #[default]
    Legacy,
    Low,
    Medium,
    High,
}

/// Lightweight registration record for an ERC-20 token activated on top of an
/// EVM platform coin. Stored on the platform [`EthCoinImpl`] so the V2
/// platform-with-tokens activation result can report per-token balances
/// (CRD §35.1.3). Mirrors `solana::SplTokenInfo`.
#[derive(Clone, Copy, Debug)]
pub struct Erc20TokenInfo {
    pub token_addr: Address,
    pub decimals: u8,
}

/// EVM signing policy held by an [`EthCoinImpl`].
///
/// `Local` wraps the secp256k1 [`KeyPair`] derived from a locally-held secret
/// (Iguana / HD-activated key / TRON) and signs transactions offline, exactly
/// as the coin did before this seam was introduced. `Metamask` (WASM-only)
/// delegates signing — and, for transactions, broadcast — to a connected
/// browser MetaMask session over EIP-1193; the framework holds no secret
/// (CRD §47.5). `Trezor` (native, non-iOS) delegates signing to a local Trezor
/// hardware device driven through the interactive withdrawal task; the
/// framework holds no local secret either (CRD §50, sibling of `Metamask`).
#[derive(Clone)]
pub(crate) enum EthSigner {
    Local(KeyPair),
    #[cfg(target_arch = "wasm32")]
    Metamask(crypto::MetamaskArc),
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
    Trezor(EthTrezorSigner),
}

/// Trezor hardware-wallet EVM signing state held **without** a live device
/// handle (CRD §50.1). The device connection is established at sign time through
/// the withdrawal task's ctx + task handle, mirroring the UTXO Trezor withdraw
/// path. This struct only carries the enabled/selected address, its account
/// public key, and the BIP-44 derivation path, so the coin can report its
/// address / public key and select the signing path without holding any local
/// secret.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[derive(Clone)]
pub(crate) struct EthTrezorSigner {
    /// BIP-44 derivation path of the enabled address (e.g. `m/44'/60'/0'/0/0`).
    pub(crate) derivation_path: crypto::DerivationPath,
    /// Address controlled by the device at `derivation_path`.
    pub(crate) address: Address,
    /// Uncompressed secp256k1 public key (64-byte `X || Y`) for the address.
    pub(crate) public: Public,
}

impl std::fmt::Debug for EthSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EthSigner::Local(_) => f.write_str("EthSigner::Local"),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(_) => f.write_str("EthSigner::Metamask"),
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(_) => f.write_str("EthSigner::Trezor"),
        }
    }
}

impl EthSigner {
    /// The address controlled by this signing policy. For `Local` this is the
    /// key pair's address (unchanged behaviour); for `Metamask` it is the
    /// connected account proven at connect time (CRD R47.5.3/R47.5.14); for
    /// `Trezor` it is the device-sourced enabled/selected address (CRD R50.1).
    pub(crate) fn address(&self) -> Address {
        match self {
            EthSigner::Local(key_pair) => key_pair.address(),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(ctx) => ctx.eth_account(),
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(signer) => signer.address,
        }
    }

    /// The uncompressed secp256k1 public key (64-byte `X || Y`, no `0x04`
    /// prefix). For `Metamask` it is the connected account's public key as
    /// recovered at connect time (CRD R47.5.14); for `Trezor` it is the account
    /// public key sourced from the device at activation (CRD R50.1).
    pub(crate) fn public(&self) -> Public {
        match self {
            EthSigner::Local(key_pair) => *key_pair.public(),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(ctx) => {
                // `eth_account_pubkey_uncompressed` is the 65-byte
                // `0x04 || X || Y` form; `Public` is the 64-byte `X || Y` body.
                let uncompressed = ctx.eth_account_pubkey_uncompressed();
                #[allow(deprecated)]
                Public::from_slice(&AsRef::<[u8]>::as_ref(&uncompressed)[1..65])
            },
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(signer) => signer.public,
        }
    }

    /// The local signing secret, or `None` under a non-local-key policy. The
    /// framework never holds a MetaMask account key (CRD R47.5.7/R47.5.14) nor
    /// a Trezor account key (CRD R50.1) — those secrets never leave the wallet
    /// / device.
    pub(crate) fn local_secret(&self) -> Option<&mm2_eth::keys::Secret> {
        match self {
            EthSigner::Local(key_pair) => Some(key_pair.secret()),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(_) => None,
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(_) => None,
        }
    }
}

/// Error returned by [`EthCoinImpl`] signing entrypoints when the active EVM
/// signing policy cannot satisfy the request (CRD §47.5). Constructed only on
/// the WASM target, where the MetaMask policy exists.
///
/// `Debug` delegates to `Display` so the bound condition text surfaces verbatim
/// through the `try_tx_s!`/`{:?}` swap error path (CRD R47.6.7).
pub enum EthSignerError {
    /// Atomic-swap / HTLC sign-and-broadcast is unsupported under the MetaMask
    /// signing policy. A MetaMask-policy EVM coin is a non-swap account: the
    /// wallet only signs transactions it immediately broadcasts itself, so the
    /// framework cannot produce the framework-scheduled HTLC payment / spend /
    /// refund broadcasts a swap requires (CRD R47.5.12 / R47.5.13). Reached only
    /// via the swap/HTLC send path (`sign_and_send_transaction_impl`); the
    /// user-facing `withdraw` delegated-broadcast path is handled separately in
    /// `withdraw_impl` via `eth_sendTransaction` (CRD R47.5.6).
    SwapSendUnsupported,
    /// Offline raw-transaction signing is unavailable under MetaMask: the
    /// wallet never yields a detached, re-broadcastable signed raw transaction
    /// (CRD R47.5.7 / R47.5.12).
    OfflineSigningUnsupported,
}

impl std::fmt::Debug for EthSignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { std::fmt::Display::fmt(self, f) }
}

impl std::fmt::Display for EthSignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EthSignerError::SwapSendUnsupported => f.write_str(
                "Atomic swaps are unsupported under the MetaMask signing policy (CRD R47.5.12): a \
                 MetaMask-policy EVM coin is a non-swap account",
            ),
            EthSignerError::OfflineSigningUnsupported => f.write_str(
                "Offline raw transaction signing is unsupported under the MetaMask signing policy (CRD R47.5.7)",
            ),
        }
    }
}

/// pImpl idiom.
#[derive(Debug)]
pub struct EthCoinImpl {
    pub(crate) ticker: String,
    pub(crate) coin_type: EthCoinType,
    pub(crate) signer: EthSigner,
    pub(crate) my_address: Address,
    pub(crate) sign_message_prefix: Option<String>,
    pub(crate) swap_contract_address: Address,
    pub(crate) fallback_swap_contract: Option<Address>,
    pub(crate) web3: super::alloy_compat::KdfProvider,
    /// The separate provider instances kept to get nonce, will replace the web3 completely soon
    pub(crate) web3_instances: Vec<Web3Instance>,
    pub(crate) decimals: u8,
    pub(crate) gas_station_url: Option<String>,
    pub(crate) gas_station_decimals: u8,
    pub(crate) gas_station_policy: GasStationPricePolicy,
    pub(crate) history_sync_state: Mutex<HistorySyncState>,
    pub(crate) required_confirmations: AtomicU64,
    /// Coin needs access to the context in order to reuse the logging and shutdown facilities.
    /// Using a weak reference by default in order to avoid circular references and leaks.
    pub(crate) ctx: MmWeak,
    pub(crate) chain_id: Option<u64>,
    /// the block range used for eth_getLogs
    pub(crate) logs_block_range: u64,
    /// HD wallet derivation method. Iguana when using a single key pair, HDWallet for BIP44 HD.
    pub derivation_method: DerivationMethod<Address, EthHDWallet>,
    /// V2 swap contract addresses (maker, taker). None if V2 not configured.
    pub(crate) swap_v2_contracts: Option<SwapV2Contracts>,
    /// Gas limits for V2 swap operations.
    pub(crate) gas_limit_v2: EthGasLimitV2,
    /// HTTP API client for TRON full nodes. Populated only when
    /// `coin_type` is [`EthCoinType::Tron`] or [`EthCoinType::Trc20`].
    /// `None` for ETH/ERC20 coins.
    pub(crate) tron_api: Option<crate::eth::tron::api::TronApiClient>,
    /// Optional address of the maker-side NFT swap V2 contract
    /// (`EtomicSwapMakerV2-NFT`). Populated from coin activation when
    /// the chain has an NFT HTLC contract deployed; `None` disables
    /// NFT swap paths for this coin (P10.3.7.b).
    pub(crate) nft_swap_v2_contract: Option<Address>,
    /// Per-coin swap gas-fee policy (CRD R35.6). Mutable at runtime via
    /// `set_swap_gas_fee_policy`; defaults to [`SwapGasFeePolicy::Legacy`].
    pub(crate) swap_gas_fee_policy: Mutex<SwapGasFeePolicy>,
    /// ERC-20 tokens activated on top of this platform coin, keyed by ticker.
    /// Populated during V2 platform-with-tokens activation so the activation
    /// result can report per-token balances (CRD §35.1.3).
    pub(crate) erc20_tokens_infos: Arc<Mutex<std::collections::HashMap<String, Erc20TokenInfo>>>,
}

// ─── V2 swap types ──────────────────────────────────────────────────────────

/// Addresses of the EtomicSwap V2 smart contracts.
#[derive(Debug, Copy, Clone, Deserialize)]
pub struct SwapV2Contracts {
    pub maker_swap_v2_contract: Address,
    pub taker_swap_v2_contract: Address,
}

/// On-chain payment states for the EtomicSwapMakerV2 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum MakerPaymentStateV2 {
    Uninitialized = 0,
    PaymentSent = 1,
    TakerSpent = 2,
    MakerRefunded = 3,
}

/// On-chain payment states for the EtomicSwapTakerV2 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum TakerPaymentStateV2 {
    Uninitialized = 0,
    PaymentSent = 1,
    TakerApproved = 2,
    MakerSpent = 3,
    TakerRefunded = 4,
}

/// Gas limits for V2 swap contract calls.
#[derive(Debug, Clone)]
pub struct EthGasLimitV2 {
    pub maker: MakerGasLimitV2,
    pub taker: TakerGasLimitV2,
}

#[derive(Debug, Clone)]
pub struct MakerGasLimitV2 {
    pub eth_payment: u64,
    pub erc20_payment: u64,
    pub eth_taker_spend: u64,
    pub erc20_taker_spend: u64,
    pub eth_maker_refund_timelock: u64,
    pub erc20_maker_refund_timelock: u64,
    pub eth_maker_refund_secret: u64,
    pub erc20_maker_refund_secret: u64,
    // P10.3.7.b — NFT HTLC entrypoints. ERC-721 and ERC-1155 are budgeted
    // separately because ERC-1155 carries an extra `amount` argument plus
    // balance bookkeeping inside the token contract.
    pub nft_erc721_payment: u64,
    pub nft_erc1155_payment: u64,
    pub nft_erc721_taker_spend: u64,
    pub nft_erc1155_taker_spend: u64,
    pub nft_erc721_maker_refund_timelock: u64,
    pub nft_erc1155_maker_refund_timelock: u64,
    pub nft_erc721_maker_refund_secret: u64,
    pub nft_erc1155_maker_refund_secret: u64,
}

#[derive(Debug, Clone)]
pub struct TakerGasLimitV2 {
    pub eth_payment: u64,
    pub erc20_payment: u64,
    pub eth_maker_spend: u64,
    pub erc20_maker_spend: u64,
    pub eth_taker_refund_timelock: u64,
    pub erc20_taker_refund_timelock: u64,
    pub eth_taker_refund_secret: u64,
    pub erc20_taker_refund_secret: u64,
    pub approve_payment: u64,
}

impl Default for EthGasLimitV2 {
    fn default() -> Self {
        EthGasLimitV2 {
            maker: MakerGasLimitV2 {
                eth_payment: 150_000,
                erc20_payment: 250_000,
                eth_taker_spend: 150_000,
                erc20_taker_spend: 150_000,
                eth_maker_refund_timelock: 150_000,
                erc20_maker_refund_timelock: 150_000,
                eth_maker_refund_secret: 150_000,
                erc20_maker_refund_secret: 150_000,
                // NFT defaults sit above ERC-20 because the HTLC contract
                // additionally invokes `safeTransferFrom` on the token contract.
                nft_erc721_payment: 200_000,
                nft_erc1155_payment: 220_000,
                nft_erc721_taker_spend: 200_000,
                nft_erc1155_taker_spend: 220_000,
                nft_erc721_maker_refund_timelock: 200_000,
                nft_erc1155_maker_refund_timelock: 220_000,
                nft_erc721_maker_refund_secret: 200_000,
                nft_erc1155_maker_refund_secret: 220_000,
            },
            taker: TakerGasLimitV2 {
                eth_payment: 150_000,
                erc20_payment: 250_000,
                eth_maker_spend: 150_000,
                erc20_maker_spend: 150_000,
                eth_taker_refund_timelock: 150_000,
                erc20_taker_refund_timelock: 150_000,
                eth_taker_refund_secret: 150_000,
                erc20_taker_refund_secret: 150_000,
                approve_payment: 150_000,
            },
        }
    }
}

impl EthGasLimitV2 {
    /// Returns the appropriate gas limit for a (coin_type, payment_type, method) triple.
    pub fn gas_limit(
        &self,
        coin_type: &EthCoinType,
        payment_type: eth_swap_v2::EthPaymentType,
        method: eth_swap_v2::PaymentMethod,
    ) -> Result<u64, String> {
        use eth_swap_v2::{EthPaymentType, PaymentMethod};
        match (coin_type, payment_type, method) {
            (EthCoinType::Eth, EthPaymentType::MakerPayments, PaymentMethod::Send) => Ok(self.maker.eth_payment),
            (EthCoinType::Erc20 { .. }, EthPaymentType::MakerPayments, PaymentMethod::Send) => {
                Ok(self.maker.erc20_payment)
            },
            (EthCoinType::Eth, EthPaymentType::MakerPayments, PaymentMethod::Spend) => Ok(self.maker.eth_taker_spend),
            (EthCoinType::Erc20 { .. }, EthPaymentType::MakerPayments, PaymentMethod::Spend) => {
                Ok(self.maker.erc20_taker_spend)
            },
            (EthCoinType::Eth, EthPaymentType::MakerPayments, PaymentMethod::RefundTimelock) => {
                Ok(self.maker.eth_maker_refund_timelock)
            },
            (EthCoinType::Erc20 { .. }, EthPaymentType::MakerPayments, PaymentMethod::RefundTimelock) => {
                Ok(self.maker.erc20_maker_refund_timelock)
            },
            (EthCoinType::Eth, EthPaymentType::MakerPayments, PaymentMethod::RefundSecret) => {
                Ok(self.maker.eth_maker_refund_secret)
            },
            (EthCoinType::Erc20 { .. }, EthPaymentType::MakerPayments, PaymentMethod::RefundSecret) => {
                Ok(self.maker.erc20_maker_refund_secret)
            },
            (EthCoinType::Eth, EthPaymentType::TakerPayments, PaymentMethod::Send) => Ok(self.taker.eth_payment),
            (EthCoinType::Erc20 { .. }, EthPaymentType::TakerPayments, PaymentMethod::Send) => {
                Ok(self.taker.erc20_payment)
            },
            (EthCoinType::Eth, EthPaymentType::TakerPayments, PaymentMethod::Spend) => Ok(self.taker.eth_maker_spend),
            (EthCoinType::Erc20 { .. }, EthPaymentType::TakerPayments, PaymentMethod::Spend) => {
                Ok(self.taker.erc20_maker_spend)
            },
            (EthCoinType::Eth, EthPaymentType::TakerPayments, PaymentMethod::RefundTimelock) => {
                Ok(self.taker.eth_taker_refund_timelock)
            },
            (EthCoinType::Erc20 { .. }, EthPaymentType::TakerPayments, PaymentMethod::RefundTimelock) => {
                Ok(self.taker.erc20_taker_refund_timelock)
            },
            (EthCoinType::Eth, EthPaymentType::TakerPayments, PaymentMethod::RefundSecret) => {
                Ok(self.taker.eth_taker_refund_secret)
            },
            (EthCoinType::Erc20 { .. }, EthPaymentType::TakerPayments, PaymentMethod::RefundSecret) => {
                Ok(self.taker.erc20_taker_refund_secret)
            },
            // P10.2.5: TRON gas estimates not modelled here. Activation rejects
            // TRON coins until swap V2 wiring lands, so this branch is unreachable.
            (EthCoinType::Tron, _, _) | (EthCoinType::Trc20 { .. }, _, _) => {
                Err("TRON swap V2 gas limits not yet defined (P10.2.5)".to_string())
            },
        }
    }

    /// Returns the appropriate gas limit for a maker-side NFT HTLC operation
    /// (P10.3.7.b). Only [`eth_swap_v2::PaymentMethod::Send`],
    /// [`eth_swap_v2::PaymentMethod::Spend`],
    /// [`eth_swap_v2::PaymentMethod::RefundTimelock`] and
    /// [`eth_swap_v2::PaymentMethod::RefundSecret`] are valid; all of them
    /// are always defined for both NFT kinds (no `Result`).
    pub fn nft_gas_limit(&self, kind: eth_swap_v2::nft_swap_v2::NftKind, method: eth_swap_v2::PaymentMethod) -> u64 {
        use eth_swap_v2::nft_swap_v2::NftKind;
        use eth_swap_v2::PaymentMethod;
        match (kind, method) {
            (NftKind::Erc721, PaymentMethod::Send) => self.maker.nft_erc721_payment,
            (NftKind::Erc1155, PaymentMethod::Send) => self.maker.nft_erc1155_payment,
            (NftKind::Erc721, PaymentMethod::Spend) => self.maker.nft_erc721_taker_spend,
            (NftKind::Erc1155, PaymentMethod::Spend) => self.maker.nft_erc1155_taker_spend,
            (NftKind::Erc721, PaymentMethod::RefundTimelock) => self.maker.nft_erc721_maker_refund_timelock,
            (NftKind::Erc1155, PaymentMethod::RefundTimelock) => self.maker.nft_erc1155_maker_refund_timelock,
            (NftKind::Erc721, PaymentMethod::RefundSecret) => self.maker.nft_erc721_maker_refund_secret,
            (NftKind::Erc1155, PaymentMethod::RefundSecret) => self.maker.nft_erc1155_maker_refund_secret,
        }
    }
}

/// Error type for EthCoin associated type parsing.
#[derive(Debug, Display)]
pub enum EthAssocTypesError {
    #[display(fmt = "Invalid hex string: {}", _0)]
    InvalidHexString(String),
    #[display(fmt = "Tx parse error: {}", _0)]
    TxParseError(String),
    #[display(fmt = "Parse signature error: {}", _0)]
    ParseSignatureError(String),
}

/// Type alias for validation results using ValidatePaymentError (V1 style).
pub type ValidatePaymentError = ValidateSwapV2TxError;
pub type ValidatePaymentResult<T> = MmResult<T, ValidatePaymentError>;

impl From<crate::eth::abi::AbiError> for FindPaymentSpendError {
    fn from(e: crate::eth::abi::AbiError) -> Self { FindPaymentSpendError::ABIError(e.to_string()) }
}

impl From<crate::eth::abi::AbiError> for ValidateSwapV2TxError {
    fn from(e: crate::eth::abi::AbiError) -> Self { ValidateSwapV2TxError::ABIError(e.to_string()) }
}

impl From<std::array::TryFromSliceError> for ValidateSwapV2TxError {
    fn from(e: std::array::TryFromSliceError) -> Self { ValidateSwapV2TxError::InternalError(e.to_string()) }
}

impl From<std::array::TryFromSliceError> for FindPaymentSpendError {
    fn from(e: std::array::TryFromSliceError) -> Self { FindPaymentSpendError::Internal(e.to_string()) }
}

impl From<NumConversError> for ValidateSwapV2TxError {
    fn from(e: NumConversError) -> Self { ValidateSwapV2TxError::InternalError(e.to_string()) }
}

impl From<eth_swap_v2::ValidatePaymentV2Err> for ValidateSwapV2TxError {
    fn from(err: eth_swap_v2::ValidatePaymentV2Err) -> Self {
        match err {
            eth_swap_v2::ValidatePaymentV2Err::WrongPaymentTx(e) => ValidateSwapV2TxError::WrongPaymentTx(e),
        }
    }
}

impl From<eth_swap_v2::PrepareTxDataError> for ValidateSwapV2TxError {
    fn from(err: eth_swap_v2::PrepareTxDataError) -> Self {
        match err {
            eth_swap_v2::PrepareTxDataError::ABIError(e) | eth_swap_v2::PrepareTxDataError::Internal(e) => {
                ValidateSwapV2TxError::InternalError(e)
            },
            eth_swap_v2::PrepareTxDataError::InvalidData(e) => ValidateSwapV2TxError::WrongPaymentTx(e),
        }
    }
}

impl fmt::Display for EthCoinType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EthCoinType::Eth => write!(f, "ETH"),
            EthCoinType::Erc20 { platform, .. } => write!(f, "ERC20({})", platform),
            EthCoinType::Tron => write!(f, "TRX"),
            EthCoinType::Trc20 { platform, .. } => write!(f, "TRC20({})", platform),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Web3Instance {
    pub(crate) web3: super::alloy_compat::KdfProvider,
    pub(crate) is_parity: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "format")]
pub enum EthAddressFormat {
    /// Single-case address (lowercase)
    #[serde(rename = "singlecase")]
    SingleCase,
    /// Mixed-case address.
    /// https://eips.ethereum.org/EIPS/eip-55
    #[serde(rename = "mixedcase")]
    MixedCase,
}
