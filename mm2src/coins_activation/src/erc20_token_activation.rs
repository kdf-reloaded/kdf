//! ERC-20 token activation (CRD §35.2 `enable_erc20` / `enable_nft`, §35.3A `task::enable_erc20`).
//!
//! The EVM token is an `EthCoin` whose coin-type is `Erc20`, so the platform coin and the token share a Rust
//! type. This module resolves a token's protocol (from the coins configuration or from the inline `protocol` of
//! a custom token), builds the token on top of an activated platform, and serves the single-token RPC and its
//! task variant.

use crate::context::{lock_activation_under, CoinsActivationContext};
use crate::init_token::{InitTokenActivationOps, InitTokenError, InitTokenInProgressStatus, InitTokenTaskHandle,
                        InitTokenTaskManagerShared};
use crate::platform_coin_with_tokens::{CustomTokenError, InitTokensAsMmCoinsError, RegisterTokenInfo};
use crate::prelude::*;
use crate::token::{EnableTokenError, TokenDerivationError, TokenProtocolParams};
use async_trait::async_trait;
use coins::coin_balance::{coin_balance_map_for_ticker, CoinBalanceMap, HDWalletBalance};
use coins::eth::tron::{parse_trc20_contract, TronAddress, TronTokenGaslessRequest};
use coins::eth::{addr_from_str, checksum_address, derive_eth_hd_activation_key, find_active_erc20_by_contract,
                 valid_addr_from_str, Erc20BuildError, EthCoin};
use coins::nft::{fetch_and_store_owned_nfts, Chain, ChainTicker, EnableNftError, EnableNftResponse, NftCtx,
                 NftProvider};
use coins::rpc_command::get_my_address::PathToAddress;
use coins::rpc_command::get_token_info::config_ticker_for_contract;
use coins::{coin_conf, lp_coinfind, lp_coinfind_or_err, BalanceError, CoinBalance, CoinProtocol, CoinsContext,
            DerivationMethod, MarketCoinOps, MmCoin, MmCoinEnum};
use common::Future01CompatExt;
use crypto::{CryptoCtx, KeyPairPolicy};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;

impl TryPlatformCoinFromMmCoinEnum for EthCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized,
    {
        match coin {
            // A token is an `EthCoin` too, but only the native coin of an EVM chain or of a TRON chain is a
            // platform coin. Whether the platform is of the token's family is checked by the token route.
            MmCoinEnum::EthCoin(coin) if coin.is_evm_platform_coin() || coin.is_tron_platform_coin() => Some(coin),
            _ => None,
        }
    }
}

/// The token entry of an `enable_eth_with_tokens` request (CRD R35.1.1 `erc20_tokens_requests`); the ticker sits
/// beside these fields.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Erc20ActivationRequest {
    /// Swap confirmations of the token; the token configuration, then the platform's, when absent.
    #[serde(default)]
    pub required_confirmations: Option<u64>,
    /// The protocol of a custom token that is not in the coins configuration (CRD R35.2.7).
    #[serde(default)]
    pub protocol: Option<CoinProtocol>,
    /// The GasFree block of a TRC20 token (CRD ch.63 §63.3.2). A token that is not a TRC20 token refuses an
    /// enabled block.
    #[serde(default)]
    pub gasless: Option<TronTokenGaslessRequest>,
}

/// The protocol of an ERC-20 token resolved from the coins configuration or from an inline `protocol`
/// (CRD R35.2.2): the platform the token lives on and its contract address.
#[derive(Clone, Debug)]
pub struct Erc20Protocol {
    pub platform_coin_ticker: String,
    pub contract_address: String,
    /// The token comes from an inline `protocol` rather than from the coins configuration.
    pub custom: bool,
    /// The token is a TRC20 token of a TRON platform (CRD ch.21 R21.10B.2), not an ERC-20 token.
    pub tron: bool,
}

impl TryFromCoinProtocol for Erc20Protocol {
    /// A configured contract address is accepted only in the exact EIP-55 checksummed form (CRD R35.5.1). A TRC20
    /// contract is accepted in either form of CRD ch.21 R-AD1.
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::ERC20 {
                platform,
                contract_address,
            } if valid_addr_from_str(&contract_address).is_ok() => Ok(Erc20Protocol {
                platform_coin_ticker: platform,
                contract_address,
                custom: false,
                tron: false,
            }),
            CoinProtocol::TRC20 {
                platform,
                contract_address,
            } if parse_trc20_contract(&contract_address).is_ok() => Ok(Erc20Protocol {
                platform_coin_ticker: platform,
                contract_address,
                custom: false,
                tron: true,
            }),
            proto => MmError::err(proto),
        }
    }
}

impl TokenProtocolParams for Erc20Protocol {
    fn platform_coin_ticker(&self) -> &str { &self.platform_coin_ticker }
}

/// Why the protocol of a token could not be resolved.
// `UnexpectedProtocol` carries a `CoinProtocol` by value for diagnostics on a
// cold error path. Boxing it (clippy's suggestion) would force `Box::new` at
// every construction site in this compatibility-sensitive activation code for a
// negligible size win on a rarely built error enum. The per-item allow is the
// established precedent for this lint across coins_activation.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ProtocolResolveError {
    ConfigIsNotFound(String),
    ProtocolParseError { ticker: String, error: String },
    UnexpectedProtocol { ticker: String, protocol: CoinProtocol },
    Custom(CustomTokenError),
}

impl From<ProtocolResolveError> for EnableTokenError {
    fn from(err: ProtocolResolveError) -> Self {
        match err {
            ProtocolResolveError::ConfigIsNotFound(ticker) => EnableTokenError::TokenConfigIsNotFound(ticker),
            ProtocolResolveError::ProtocolParseError { ticker, error } => {
                EnableTokenError::TokenProtocolParseError { ticker, error }
            },
            ProtocolResolveError::UnexpectedProtocol { ticker, protocol } => {
                EnableTokenError::UnexpectedTokenProtocol { ticker, protocol }
            },
            ProtocolResolveError::Custom(e) => EnableTokenError::CustomTokenError(e),
        }
    }
}

impl From<ProtocolResolveError> for InitTokensAsMmCoinsError {
    fn from(err: ProtocolResolveError) -> Self {
        match err {
            ProtocolResolveError::ConfigIsNotFound(ticker) => InitTokensAsMmCoinsError::TokenConfigIsNotFound(ticker),
            ProtocolResolveError::ProtocolParseError { ticker, error } => {
                InitTokensAsMmCoinsError::TokenProtocolParseError { ticker, error }
            },
            ProtocolResolveError::UnexpectedProtocol { ticker, protocol } => {
                InitTokensAsMmCoinsError::UnexpectedTokenProtocol { ticker, protocol }
            },
            ProtocolResolveError::Custom(e) => InitTokensAsMmCoinsError::CustomTokenError(e),
        }
    }
}

impl From<ProtocolResolveError> for InitTokenError {
    fn from(err: ProtocolResolveError) -> Self {
        match err {
            ProtocolResolveError::ConfigIsNotFound(ticker) => InitTokenError::TokenConfigIsNotFound(ticker),
            ProtocolResolveError::ProtocolParseError { ticker, error } => {
                InitTokenError::TokenProtocolParseError { ticker, error }
            },
            ProtocolResolveError::UnexpectedProtocol { ticker, protocol } => {
                InitTokenError::UnexpectedTokenProtocol { ticker, protocol }
            },
            ProtocolResolveError::Custom(e) => InitTokenError::CustomTokenError(e),
        }
    }
}

/// The protocol of an EVM token: an ERC-20 contract or the NFT pseudo-coin of a platform.
pub(crate) enum EvmTokenProtocol {
    Erc20(Erc20Protocol),
    Nft { platform: String },
}

/// Reads and parses the `protocol` of a token's coins configuration entry.
pub(crate) fn configured_protocol(ctx: &MmArc, ticker: &str) -> Result<CoinProtocol, ProtocolResolveError> {
    let conf = coin_conf(ctx, ticker);
    if conf.is_null() {
        return Err(ProtocolResolveError::ConfigIsNotFound(ticker.to_owned()));
    }
    CoinProtocol::from_conf_json(conf["protocol"].clone()).map_err(|e| ProtocolResolveError::ProtocolParseError {
        ticker: ticker.to_owned(),
        error: e.to_string(),
    })
}

/// Validates the inline protocol of a custom ERC-20 token (CRD R35.2.7): the ticker must not be in the coins
/// configuration, the contract address must parse and must not be configured under another ticker of the
/// platform. Whether another token with the same contract is *active* is checked when the token is built.
fn custom_erc20_protocol(
    ctx: &MmArc,
    ticker: &str,
    platform: String,
    contract_address: String,
) -> Result<Erc20Protocol, ProtocolResolveError> {
    if !coin_conf(ctx, ticker).is_null() {
        return Err(ProtocolResolveError::Custom(
            CustomTokenError::DuplicateTickerInConfig {
                ticker_in_config: ticker.to_owned(),
            },
        ));
    }
    if addr_from_str(&contract_address).is_err() {
        return Err(ProtocolResolveError::Custom(CustomTokenError::InvalidTokenAddress));
    }
    if let Some(ticker_in_config) = config_ticker_for_contract(ctx, &platform, &contract_address) {
        return Err(ProtocolResolveError::Custom(
            CustomTokenError::DuplicateContractInConfig { ticker_in_config },
        ));
    }
    Ok(Erc20Protocol {
        platform_coin_ticker: platform,
        contract_address,
        custom: true,
        tron: false,
    })
}

/// The protocol of a configured TRC20 token: its contract address must be one of the two forms of CRD ch.21 R-AD1
/// and not zero, else the configuration is a protocol parse failure (R21.10B.2).
fn trc20_protocol(
    ticker: &str,
    platform: String,
    contract_address: String,
) -> Result<Erc20Protocol, ProtocolResolveError> {
    match parse_trc20_contract(&contract_address) {
        Ok(_) => Ok(Erc20Protocol {
            platform_coin_ticker: platform,
            contract_address,
            custom: false,
            tron: true,
        }),
        Err(error) => Err(ProtocolResolveError::ProtocolParseError {
            ticker: ticker.to_owned(),
            error,
        }),
    }
}

/// Resolves the protocol of an ERC-20 token for the platform activation and the token task: the inline
/// `protocol` of a custom token, else the coins configuration (CRD R35.2.2).
pub(crate) fn resolve_erc20_protocol(
    ctx: &MmArc,
    ticker: &str,
    inline_protocol: Option<&CoinProtocol>,
) -> Result<Erc20Protocol, ProtocolResolveError> {
    match inline_protocol {
        Some(CoinProtocol::ERC20 {
            platform,
            contract_address,
        }) => custom_erc20_protocol(ctx, ticker, platform.clone(), contract_address.clone()),
        Some(other) => Err(ProtocolResolveError::UnexpectedProtocol {
            ticker: ticker.to_owned(),
            protocol: other.clone(),
        }),
        None => match configured_protocol(ctx, ticker)? {
            CoinProtocol::ERC20 {
                platform,
                contract_address,
            } if valid_addr_from_str(&contract_address).is_ok() => Ok(Erc20Protocol {
                platform_coin_ticker: platform,
                contract_address,
                custom: false,
                tron: false,
            }),
            CoinProtocol::TRC20 {
                platform,
                contract_address,
            } => trc20_protocol(ticker, platform, contract_address),
            protocol => Err(ProtocolResolveError::UnexpectedProtocol {
                ticker: ticker.to_owned(),
                protocol,
            }),
        },
    }
}

/// Resolves the protocol of the token of a single-token request: an ERC-20 contract or an NFT pseudo-coin.
fn resolve_evm_token_protocol(
    ctx: &MmArc,
    ticker: &str,
    inline_protocol: Option<&CoinProtocol>,
) -> Result<EvmTokenProtocol, ProtocolResolveError> {
    let protocol = match inline_protocol {
        Some(CoinProtocol::NFT { platform }) => {
            return Ok(EvmTokenProtocol::Nft {
                platform: platform.clone(),
            })
        },
        Some(_) => return resolve_erc20_protocol(ctx, ticker, inline_protocol).map(EvmTokenProtocol::Erc20),
        None => configured_protocol(ctx, ticker)?,
    };
    match protocol {
        CoinProtocol::NFT { platform } => Ok(EvmTokenProtocol::Nft { platform }),
        CoinProtocol::ERC20 {
            platform,
            contract_address,
        } if valid_addr_from_str(&contract_address).is_ok() => Ok(EvmTokenProtocol::Erc20(Erc20Protocol {
            platform_coin_ticker: platform,
            contract_address,
            custom: false,
            tron: false,
        })),
        CoinProtocol::TRC20 {
            platform,
            contract_address,
        } => trc20_protocol(ticker, platform, contract_address).map(EvmTokenProtocol::Erc20),
        protocol => Err(ProtocolResolveError::UnexpectedProtocol {
            ticker: ticker.to_owned(),
            protocol,
        }),
    }
}

/// Why an ERC-20 token could not be built on top of its platform.
pub(crate) enum Erc20TokenError {
    Custom(CustomTokenError),
    /// The nodes gave no usable answer while the decimals were read from the contract (CRD R35.1.6).
    Transport(String),
    UnsupportedDecimals(String),
    /// The token's coin-configuration entry holds an unusable `gas_limit` / `gas_limit_v2` member (CRD R57.5.1).
    InvalidConfig(String),
    /// The `gasless` block of the request is refused (CRD ch.63 §63.3.2, R1).
    InvalidPayload(String),
    Internal(String),
}

impl From<Erc20TokenError> for EnableTokenError {
    fn from(err: Erc20TokenError) -> Self {
        match err {
            Erc20TokenError::Custom(e) => EnableTokenError::CustomTokenError(e),
            Erc20TokenError::Transport(e) => EnableTokenError::Transport(e),
            Erc20TokenError::InvalidConfig(e) => EnableTokenError::InvalidConfig(e),
            Erc20TokenError::InvalidPayload(e) => EnableTokenError::InvalidPayload(e),
            Erc20TokenError::UnsupportedDecimals(e) | Erc20TokenError::Internal(e) => EnableTokenError::Internal(e),
        }
    }
}

impl From<Erc20TokenError> for InitTokensAsMmCoinsError {
    fn from(err: Erc20TokenError) -> Self {
        match err {
            Erc20TokenError::Custom(e) => InitTokensAsMmCoinsError::CustomTokenError(e),
            Erc20TokenError::Transport(e) => InitTokensAsMmCoinsError::Transport(e),
            Erc20TokenError::InvalidPayload(e) => InitTokensAsMmCoinsError::InvalidPayload(e),
            Erc20TokenError::UnsupportedDecimals(e)
            | Erc20TokenError::InvalidConfig(e)
            | Erc20TokenError::Internal(e) => InitTokensAsMmCoinsError::InvalidPubkey(e),
        }
    }
}

impl From<Erc20TokenError> for InitTokenError {
    fn from(err: Erc20TokenError) -> Self {
        match err {
            Erc20TokenError::Custom(e) => InitTokenError::CustomTokenError(e),
            // An unusable configuration is an invalid-payload class failure of the construction, which the
            // token task reports as `Transport` (CRD R35.3A.6). A refused `gasless` block (CRD ch.63 §63.3.2) is of
            // that class.
            Erc20TokenError::Transport(e)
            | Erc20TokenError::UnsupportedDecimals(e)
            | Erc20TokenError::InvalidPayload(e) => InitTokenError::Transport(e),
            Erc20TokenError::InvalidConfig(e) | Erc20TokenError::Internal(e) => InitTokenError::Internal(e),
        }
    }
}

/// The 20-byte contract address of a token: the EVM form of an ERC-20 token, either TRON form of a TRC20 token.
fn token_address(protocol: &Erc20Protocol) -> Result<ethereum_types::Address, String> {
    if protocol.tron {
        parse_trc20_contract(&protocol.contract_address)
    } else {
        addr_from_str(&protocol.contract_address).map_err(|e| format!("Invalid ERC20 contract address: {}", e))
    }
}

/// The contract address as the result of a token activation reports it: lower-case hex for an ERC-20 token (CRD
/// R35.2.3), base58check for a TRC20 token (CRD ch.21 R21.10B.5).
fn reported_contract_address(protocol: &Erc20Protocol, token_addr: ethereum_types::Address) -> String {
    if protocol.tron {
        TronAddress::from_evm_address(token_addr).to_base58()
    } else {
        format!("{:#x}", token_addr)
    }
}

/// Builds an ERC-20 token on top of an activated platform coin (CRD R35.2.2). A TRC20 token (`protocol.tron`) is
/// built on a TRX platform by the same steps, its decimals read through the TRON node API when its configuration
/// has none (CRD ch.21 R21.10B.2).
///
/// The decimals come from the token configuration and are read from the contract when the configuration lacks
/// them. The swap confirmations are the requested ones, else the token configuration's, else the platform's.
/// A custom token is rejected while another token with the same contract is active (CRD R35.2.7).
///
/// `gasless` is the GasFree block of the token's request (CRD ch.63 §63.3.2, R1). A block with `enabled: false`
/// behaves as an absent one. An enabled block is an invalid payload for a token that is not a TRC20 token, for a
/// TRON platform without a `tron_gasless_provider`, and for a `transfer_max_fee` that is negative or finer than
/// the token's decimals.
pub(crate) async fn build_erc20_token(
    ctx: &MmArc,
    platform_coin: &EthCoin,
    ticker: &str,
    protocol: &Erc20Protocol,
    requested_confirmations: Option<u64>,
    gasless: Option<&TronTokenGaslessRequest>,
) -> Result<EthCoin, MmError<Erc20TokenError>> {
    let gasless = gasless.filter(|gasless| gasless.is_enabled());
    if gasless.is_some() && !protocol.tron {
        return MmError::err(Erc20TokenError::InvalidPayload(
            "gasless is valid for a TRC20 token of a TRON platform only".to_owned(),
        ));
    }
    let token_addr = token_address(protocol).map_to_mm(Erc20TokenError::Internal)?;

    if protocol.custom {
        if let Some(active_ticker) =
            find_active_erc20_by_contract(ctx, &protocol.platform_coin_ticker, token_addr).await
        {
            return MmError::err(Erc20TokenError::Custom(
                CustomTokenError::TokenWithSameContractAlreadyActivated {
                    ticker: active_ticker,
                    contract_address: protocol.contract_address.clone(),
                },
            ));
        }
    }

    let required_confirmations = requested_confirmations
        .or_else(|| coin_conf(ctx, ticker)["required_confirmations"].as_u64())
        .unwrap_or_else(|| platform_coin.required_confirmations());

    let built = if protocol.tron {
        platform_coin
            .trc20_token_from_conf_or_contract(ticker.to_owned(), token_addr, required_confirmations, gasless)
            .await
    } else {
        platform_coin
            .erc20_token_from_conf_or_contract(ticker.to_owned(), token_addr, required_confirmations, protocol.custom)
            .await
    };
    built.mm_err(|e| match e {
        Erc20BuildError::Transport(e) => Erc20TokenError::Transport(e),
        Erc20BuildError::UnsupportedDecimals(e) => Erc20TokenError::UnsupportedDecimals(e),
        Erc20BuildError::InvalidConfig(e) => Erc20TokenError::InvalidConfig(e),
        Erc20BuildError::InvalidPayload(e) => Erc20TokenError::InvalidPayload(e),
        Erc20BuildError::Internal(e) => Erc20TokenError::Internal(e),
    })
}

impl RegisterTokenInfo<EthCoin> for EthCoin {
    fn register_token_info(&self, token: &EthCoin) {
        if let Some(info) = token.erc20_token_info() {
            self.add_erc20_token_info(token.ticker().into(), info);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// `enable_erc20` / `enable_nft` (CRD §35.2)
// ---------------------------------------------------------------------------------------------------------------

/// The `activation_params` of `enable_erc20` and `enable_nft` (CRD R35.2.1, R35.3A.2). A field the token
/// family does not use is accepted and ignored.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EvmTokenActivationParams {
    #[serde(default)]
    pub required_confirmations: Option<u64>,
    /// The NFT indexer provider. Kept raw: an unusable provider is not a request-parse failure (CRD R35.2.8).
    #[serde(default)]
    provider: Option<Json>,
    /// The enabled address of the token on an HD platform (task variant).
    #[serde(default)]
    pub path_to_address: Option<PathToAddress>,
    /// Accepted for wire parity; the addresses of a token report are the known addresses of its platform.
    #[serde(default)]
    #[allow(dead_code)]
    scan_policy: Option<Json>,
    /// Accepted for wire parity; the addresses of a token report are the known addresses of its platform.
    #[serde(default)]
    #[allow(dead_code)]
    min_addresses_number: Option<u32>,
    /// The GasFree block of a TRC20 token (CRD ch.63 §63.3.2). A token that is not a TRC20 token refuses an
    /// enabled block.
    #[serde(default)]
    pub gasless: Option<TronTokenGaslessRequest>,
}

#[derive(Debug, Deserialize)]
pub struct EnableEvmTokenRequest {
    ticker: String,
    #[serde(default)]
    protocol: Option<CoinProtocol>,
    activation_params: EvmTokenActivationParams,
}

/// The entry of one address in the `balances` map of `enable_erc20` (CRD R35.2.3): the BalanceInfo members, and
/// for a TRC20 token whose platform has a GasFree provider the custody address of that address (CRD ch.63 §63.3.3).
/// The member is omitted otherwise, which leaves the entry as it always was.
#[derive(Debug, Serialize)]
pub struct Erc20AddressBalance {
    #[serde(flatten)]
    balance: CoinBalance,
    #[serde(skip_serializing_if = "Option::is_none")]
    gasfree_address: Option<String>,
}

/// The success result of `enable_erc20` (CRD R35.2.3).
#[derive(Debug, Serialize)]
pub struct Erc20InitResult {
    balances: HashMap<String, Erc20AddressBalance>,
    platform_coin: String,
    token_contract_address: String,
    required_confirmations: u64,
}

/// The success result of the single-token handler: the ERC-20 object, or the NFT snapshot of `enable_nft`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum EvmTokenActivationResult {
    Erc20(Erc20InitResult),
    Nft(EnableNftResponse),
}

impl From<EnableNftError> for EnableTokenError {
    fn from(err: EnableNftError) -> Self {
        match err {
            EnableNftError::PlatformCoinIsNotActivated { coin } => EnableTokenError::PlatformCoinIsNotActivated(coin),
            EnableNftError::AlreadyActivated { ticker } => EnableTokenError::TokenIsAlreadyActivated(ticker),
            EnableNftError::UnsupportedPlatform { coin } => EnableTokenError::UnsupportedPlatformCoin {
                platform_coin_ticker: coin.clone(),
                token_ticker: coin,
            },
            EnableNftError::CrawlFailed(e) => EnableTokenError::Transport(e),
            EnableNftError::ProviderRejectedRequest(e) => EnableTokenError::InvalidPayload(e),
            other => EnableTokenError::Internal(other.to_string()),
        }
    }
}

/// The platform coin of a single-token request, active and of the token's family (CRD R35.2.6): an EVM platform for
/// an ERC-20 token, a TRON platform for a TRC20 token (`tron`). An active EVM-family platform of the other family is
/// `PlatformCoinMismatch` (CRD ch.21 R21.10B.2), except for an NFT (`nft`), which a TRON platform does not support.
async fn evm_platform_coin(
    ctx: &MmArc,
    platform_ticker: &str,
    token_ticker: &str,
    tron: bool,
    nft: bool,
) -> MmResult<EthCoin, EnableTokenError> {
    let platform_coin = lp_coinfind_or_err(ctx, platform_ticker)
        .await
        .mm_err(|_| EnableTokenError::PlatformCoinIsNotActivated(platform_ticker.to_owned()))?;
    let unsupported = || EnableTokenError::UnsupportedPlatformCoin {
        platform_coin_ticker: platform_ticker.to_owned(),
        token_ticker: token_ticker.to_owned(),
    };
    let platform_coin =
        <EthCoin as TryPlatformCoinFromMmCoinEnum>::try_from_mm_coin(platform_coin).or_mm_err(&unsupported)?;
    if platform_coin.is_tron_platform_coin() != tron {
        return if nft {
            MmError::err(unsupported())
        } else {
            MmError::err(EnableTokenError::PlatformCoinMismatch)
        };
    }
    Ok(platform_coin)
}

/// Refuses a token ticker that is registered, or an NFT pseudo-ticker whose chain is active. An NFT pseudo-coin is not
/// in the coin registry; it is active once its chain is.
async fn ensure_evm_token_not_active(ctx: &MmArc, ticker: &str) -> MmResult<(), EnableTokenError> {
    if let Ok(Some(_)) = lp_coinfind(ctx, ticker).await {
        return MmError::err(EnableTokenError::TokenIsAlreadyActivated(ticker.to_owned()));
    }
    if let Ok(chain) = Chain::from_nft_ticker(ticker) {
        let nft_ctx = NftCtx::from_mm_ctx(ctx).map_to_mm(EnableTokenError::Internal)?;
        if nft_ctx.is_activated(chain) {
            return MmError::err(EnableTokenError::TokenIsAlreadyActivated(ticker.to_owned()));
        }
    }
    Ok(())
}

/// `enable_erc20` and its alias `enable_nft` (CRD R35.2.4): one handler that selects the token kind from the
/// resolved protocol.
pub async fn enable_evm_token(
    ctx: MmArc,
    req: EnableEvmTokenRequest,
) -> MmResult<EvmTokenActivationResult, EnableTokenError> {
    // The lock needs the platform ticker, which the token's protocol names, so the protocol is resolved first. A
    // registered ticker keeps the refusal it has always had, before any other check.
    ensure_evm_token_not_active(&ctx, &req.ticker).await?;
    let protocol =
        resolve_evm_token_protocol(&ctx, &req.ticker, req.protocol.as_ref()).map_to_mm(EnableTokenError::from)?;
    let platform_ticker = match &protocol {
        EvmTokenProtocol::Erc20(protocol) => protocol.platform_coin_ticker.clone(),
        EvmTokenProtocol::Nft { platform } => platform.clone(),
    };

    // R55.1.12 with D43.1F.2 (b): the lock of the token (exclusive) and the lock of its platform (shared) are held
    // together through registration, so a `disable_coin` of the platform cannot run in between. The checks below
    // follow the wait.
    let _activation_guard = lock_activation_under(&ctx, &platform_ticker, &[&req.ticker])
        .await
        .map_to_mm(EnableTokenError::Internal)?;
    ensure_evm_token_not_active(&ctx, &req.ticker).await?;
    match protocol {
        EvmTokenProtocol::Erc20(protocol) => enable_erc20_token(&ctx, req, protocol)
            .await
            .map(EvmTokenActivationResult::Erc20),
        // An NFT pseudo-coin lives on an EVM chain: no GasFree block applies to it (CRD ch.63 §63.3.2, R1).
        EvmTokenProtocol::Nft { .. }
            if req
                .activation_params
                .gasless
                .as_ref()
                .is_some_and(TronTokenGaslessRequest::is_enabled) =>
        {
            MmError::err(EnableTokenError::InvalidPayload(
                "gasless is valid for a TRC20 token of a TRON platform only".to_owned(),
            ))
        },
        EvmTokenProtocol::Nft { platform } => enable_nft_token(&ctx, req, &platform)
            .await
            .map(EvmTokenActivationResult::Nft),
    }
}

async fn enable_erc20_token(
    ctx: &MmArc,
    req: EnableEvmTokenRequest,
    protocol: Erc20Protocol,
) -> MmResult<Erc20InitResult, EnableTokenError> {
    // An ERC-20 ticker with a usable NFT provider: the token kind and the parameters disagree.
    if let Some(provider) = req.activation_params.provider.as_ref() {
        if serde_json::from_value::<NftProvider>(provider.clone()).is_ok() {
            return MmError::err(EnableTokenError::Internal(
                "The activation_params carry an NFT provider but the token is an ERC-20".to_owned(),
            ));
        }
    }

    let platform_coin =
        evm_platform_coin(ctx, &protocol.platform_coin_ticker, &req.ticker, protocol.tron, false).await?;
    // The work the build spawns belongs to the instance, and ends if the instance is not registered (CRD ch.43
    // R43.1E.5 item 4, §43.1E not-established item (1)).
    let work = coins::coin_work::PendingCoinWork::new();
    let token = work
        .scope(build_erc20_token(
            ctx,
            &platform_coin,
            &req.ticker,
            &protocol,
            req.activation_params.required_confirmations,
            req.activation_params.gasless.as_ref(),
        ))
        .await
        .mm_err(EnableTokenError::from)?;

    let balances = match &platform_coin.derivation_method {
        DerivationMethod::Iguana(_) => {
            let balance = token.my_balance().compat().await.mm_err(EnableTokenError::from)?;
            let my_address = token.my_address().map_to_mm(EnableTokenError::Internal)?;
            let mut balances = HashMap::new();
            balances.insert(my_address, Erc20AddressBalance {
                balance,
                // The token is not registered yet; its platform holds the provider binding.
                gasfree_address: platform_coin.enabled_address_gasfree().await,
            });
            balances
        },
        DerivationMethod::HDWallet(_) => {
            // Only a software HD wallet is admitted. A hardware-backed platform keeps the legacy refusal: its
            // device confirmations travel through the task variant's `user_action` channel, which this call lacks.
            // The `erc20_hd_activation` opt-out (CRD R35.2.5) restores the refusal for software HD too.
            if !platform_coin.uses_software_hd_wallet() || !erc20_hd_activation(ctx) {
                return MmError::err(EnableTokenError::UnexpectedDerivationMethod(
                    TokenDerivationError::ExpectedSingleAddress,
                ));
            }
            if let Some(path_to_address) = &req.activation_params.path_to_address {
                let selects_enabled = path_selects_enabled_address(ctx, &platform_coin, path_to_address)
                    .await
                    .map_to_mm(enable_error_from_init)?;
                if !selects_enabled {
                    return MmError::err(EnableTokenError::Internal(
                        "A token can be enabled only on the enabled address of its platform coin".to_owned(),
                    ));
                }
            }
            let token_info = token
                .erc20_token_info()
                .or_mm_err(|| EnableTokenError::Internal("The built token has no token info".to_owned()))?;
            // Unlike `task::enable_erc20` there is no task channel here, so no progress is reported: the balance
            // of every known address is read over the network and the caller simply waits for all of them. This
            // asymmetry is what the task variant exists for on a wallet with many addresses.
            let report = platform_coin
                .erc20_hd_wallet_balance(&req.ticker, &token_info)
                .await
                .mm_err(EnableTokenError::from)?;
            flatten_hd_report(&req.ticker, report)?
        },
    };
    let token_addr = token_address(&protocol).map_to_mm(EnableTokenError::Internal)?;

    let coins_ctx = CoinsContext::from_ctx(ctx).map_to_mm(EnableTokenError::Internal)?;
    coins_ctx
        .add_token_with_work(platform_coin.ticker(), token.clone().into(), work)
        .await
        .mm_err(|e| EnableTokenError::TokenIsAlreadyActivated(e.ticker))?;
    // The token is recorded on its platform only once it is registered (CRD R35.2.6).
    platform_coin.register_token_info(&token);

    Ok(Erc20InitResult {
        balances,
        platform_coin: platform_coin.ticker().to_owned(),
        // Lower-case hex, as in the public docs example and `v2.6.0-beta`. `v3.0.0-beta` renders the EIP-55
        // checksummed form; the uncontradicted docs example governs (CRD ch.35 R35.2.3). A TRC20 token reports
        // base58check (CRD ch.21 R21.10B.5).
        token_contract_address: reported_contract_address(&protocol, token_addr),
        required_confirmations: token.required_confirmations(),
    })
}

/// Whether `enable_erc20` may activate a token on an HD platform. Only an explicit `false` in `MM2.json` restores
/// the legacy refusal (CRD R35.2.5), so an absent or malformed value keeps the HD support on.
pub fn erc20_hd_activation(ctx: &MmArc) -> bool { ctx.conf["erc20_hd_activation"].as_bool() != Some(false) }

/// Maps the failure of the shared enabled-address check onto the one-shot call's error set.
fn enable_error_from_init(err: InitTokenError) -> EnableTokenError {
    match err {
        InitTokenError::Internal(e) => EnableTokenError::Internal(e),
        other => EnableTokenError::Internal(other.to_string()),
    }
}

/// Flattens the HD report of a token into the address-to-balance map of the one-shot result.
fn flatten_hd_report(
    ticker: &str,
    report: HDWalletBalance,
) -> MmResult<HashMap<String, Erc20AddressBalance>, EnableTokenError> {
    let mut balances = HashMap::new();
    for account in report.accounts {
        for address in account.addresses {
            let balance = address.balance.get(ticker).cloned().or_mm_err(|| {
                EnableTokenError::Internal(format!("The report of {} has no {} balance", address.address, ticker))
            })?;
            balances.insert(address.address, Erc20AddressBalance {
                balance,
                gasfree_address: address.gasfree_address,
            });
        }
    }
    Ok(balances)
}

async fn enable_nft_token(
    ctx: &MmArc,
    req: EnableEvmTokenRequest,
    platform: &str,
) -> MmResult<EnableNftResponse, EnableTokenError> {
    let chain = Chain::from_coin_ticker(platform).ok();
    let nft_ctx = NftCtx::from_mm_ctx(ctx).map_to_mm(EnableTokenError::Internal)?;
    if let Some(chain) = chain {
        if nft_ctx.is_activated(chain) {
            return MmError::err(EnableTokenError::TokenIsAlreadyActivated(req.ticker));
        }
    }

    let platform_coin = evm_platform_coin(ctx, platform, &req.ticker, false, true).await?;
    if !matches!(platform_coin.derivation_method, DerivationMethod::Iguana(_)) {
        return MmError::err(EnableTokenError::UnexpectedDerivationMethod(
            TokenDerivationError::ExpectedSingleAddress,
        ));
    }
    let chain =
        chain.or_mm_err(|| EnableTokenError::Internal(format!("Platform coin {} has no NFT support", platform)))?;

    let provider = req
        .activation_params
        .provider
        .clone()
        .and_then(|provider| serde_json::from_value::<NftProvider>(provider).ok())
        .or_mm_err(|| EnableTokenError::Internal("The activation_params carry no usable NFT provider".to_owned()))?;

    let owned = fetch_and_store_owned_nfts(&nft_ctx, chain, platform_coin.enabled_address(), &provider)
        .await
        .mm_err(EnableTokenError::from)?;
    nft_ctx.mark_activated(chain);
    // The NFT chain is a dependent of its platform, with no coin entry of its own (CRD ch.43 R43.1F.8 item 1).
    let coins_ctx = CoinsContext::from_ctx(ctx).map_to_mm(EnableTokenError::Internal)?;
    coins_ctx
        .add_nft_dependent(platform_coin.ticker(), chain.nft_ticker())
        .await;

    let mut nfts = HashMap::with_capacity(owned.len());
    for nft in owned {
        nfts.insert(nft.token_id.to_string(), nft);
    }
    Ok(EnableNftResponse {
        nfts,
        platform_coin: chain.coin_ticker().to_owned(),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// `task::enable_erc20` (CRD §35.3A)
// ---------------------------------------------------------------------------------------------------------------

/// The wallet balance of a token task (CRD R35.3A.5): both forms key their balances by the token ticker.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "wallet_type")]
pub enum Erc20WalletBalance {
    Iguana {
        address: String,
        /// The TRON GasFree custody address of `address` (CRD ch.63 §63.3.3): present for a TRC20 token whose
        /// platform has a GasFree provider, omitted otherwise.
        #[serde(skip_serializing_if = "Option::is_none")]
        gasfree_address: Option<String>,
        balance: CoinBalanceMap,
    },
    HD(HDWalletBalance),
}

/// The success object of `task::enable_erc20` (CRD R35.3A.5).
#[derive(Clone, Debug, Serialize)]
pub struct Erc20TaskResult {
    ticker: String,
    platform_coin: String,
    token_contract_address: String,
    current_block: u64,
    required_confirmations: u64,
    wallet_balance: Erc20WalletBalance,
}

impl InitTokenError {
    fn from_balance_error(err: BalanceError) -> Self {
        match err {
            BalanceError::Transport(e) | BalanceError::InvalidResponse(e) => InitTokenError::CouldNotFetchBalance(e),
            other => InitTokenError::Internal(other.to_string()),
        }
    }
}

/// Whether `path_to_address` selects the enabled address of the platform. A token is enabled on the enabled
/// address of its platform; enabling it on another address is not supported.
async fn path_selects_enabled_address(
    ctx: &MmArc,
    platform_coin: &EthCoin,
    path_to_address: &PathToAddress,
) -> Result<bool, InitTokenError> {
    if let Some(enabled_path) = platform_coin.hardware_derivation_path() {
        let conf = coin_conf(ctx, platform_coin.ticker());
        let path = coins::eth::eth_full_derivation_path(&conf, path_to_address)
            .map_err(|e| InitTokenError::Internal(e.to_string()))?;
        return Ok(path == enabled_path);
    }
    // A single-address platform (`IguanaPrivKey` under an HD login, or any coin under an iguana login) has no
    // HD-derived address to compare with: the token runs on the platform's own address.
    if matches!(platform_coin.derivation_method, DerivationMethod::Iguana(_)) {
        return Ok(true);
    }
    let crypto_ctx = CryptoCtx::from_ctx(ctx).map_err(|e| InitTokenError::Internal(e.to_string()))?;
    match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => {
            let conf = coin_conf(ctx, platform_coin.ticker());
            let key_pair = derive_eth_hd_activation_key(global_hd, &conf, path_to_address)
                .map_err(|e| InitTokenError::Internal(e.to_string()))?;
            Ok(key_pair.address() == platform_coin.enabled_address())
        },
        KeyPairPolicy::Iguana => Ok(true),
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl InitTokenActivationOps for EthCoin {
    type PlatformCoin = EthCoin;
    type ActivationRequest = EvmTokenActivationParams;
    type ProtocolInfo = Erc20Protocol;
    type ActivationResult = Erc20TaskResult;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitTokenTaskManagerShared<Self> {
        &activation_ctx.init_erc20_task_manager
    }

    fn resolve_protocol(
        ctx: &MmArc,
        ticker: &str,
        inline_protocol: Option<&CoinProtocol>,
    ) -> Result<Self::ProtocolInfo, InitTokenError> {
        resolve_erc20_protocol(ctx, ticker, inline_protocol).map_err(InitTokenError::from)
    }

    async fn init_token(
        ctx: &MmArc,
        ticker: &str,
        platform_coin: &EthCoin,
        activation_request: &EvmTokenActivationParams,
        protocol_info: &Erc20Protocol,
        task_handle: &InitTokenTaskHandle<Self>,
    ) -> Result<(Self, Erc20TaskResult), MmError<InitTokenError>> {
        // A token of the other family than its active platform names another platform than the activated one
        // (CRD ch.21 R21.10B.2).
        if protocol_info.tron != platform_coin.is_tron_platform_coin() {
            return MmError::err(InitTokenError::PlatformCoinMismatch);
        }
        // A hardware-wallet platform needs the device context it was activated with.
        if platform_coin.uses_hardware_signer()
            && CryptoCtx::from_ctx(ctx)
                .mm_err(|e| InitTokenError::Internal(e.to_string()))?
                .hw_ctx()
                .is_none()
        {
            return MmError::err(InitTokenError::HwError("NotInitialized".to_owned()));
        }
        if let Some(path_to_address) = &activation_request.path_to_address {
            if !path_selects_enabled_address(ctx, platform_coin, path_to_address).await? {
                return MmError::err(InitTokenError::Internal(
                    "A token can be enabled only on the enabled address of its platform coin".to_owned(),
                ));
            }
        }

        let token = build_erc20_token(
            ctx,
            platform_coin,
            ticker,
            protocol_info,
            activation_request.required_confirmations,
            activation_request.gasless.as_ref(),
        )
        .await
        .mm_err(InitTokenError::from)?;

        task_handle
            .update_in_progress_status(InitTokenInProgressStatus::RequestingWalletBalance)
            .mm_err(InitTokenError::from)?;

        let current_block = platform_coin
            .current_block()
            .compat()
            .await
            .map_to_mm(InitTokenError::Transport)?;
        let token_info = token
            .erc20_token_info()
            .or_mm_err(|| InitTokenError::Internal("The built token has no token info".to_owned()))?;
        let wallet_balance = match &platform_coin.derivation_method {
            DerivationMethod::Iguana(address) => {
                let balance = token
                    .my_balance()
                    .compat()
                    .await
                    .mm_err(InitTokenError::from_balance_error)?;
                Erc20WalletBalance::Iguana {
                    address: if protocol_info.tron {
                        TronAddress::from_evm_address(*address).to_base58()
                    } else {
                        checksum_address(&format!("{:#x}", address))
                    },
                    // The token is not registered yet; its platform holds the provider binding.
                    gasfree_address: platform_coin.gasfree_report_address(address).await,
                    balance: coin_balance_map_for_ticker(ticker, balance),
                }
            },
            DerivationMethod::HDWallet(_) => {
                let report: HDWalletBalance = platform_coin
                    .erc20_hd_wallet_balance(ticker, &token_info)
                    .await
                    .mm_err(InitTokenError::from_balance_error)?;
                Erc20WalletBalance::HD(report)
            },
        };

        let result = Erc20TaskResult {
            ticker: ticker.to_owned(),
            platform_coin: platform_coin.ticker().to_owned(),
            token_contract_address: if protocol_info.tron {
                reported_contract_address(protocol_info, token_info.token_addr)
            } else {
                checksum_address(&format!("{:#x}", token_info.token_addr))
            },
            current_block,
            required_confirmations: token.required_confirmations(),
            wallet_balance,
        };
        Ok((token, result))
    }

    fn register_token_info(platform_coin: &EthCoin, token: &EthCoin) { platform_coin.register_token_info(token); }

    fn unregister_token_info(platform_coin: &EthCoin, ticker: &str) { platform_coin.remove_erc20_token_info(ticker); }
}

#[cfg(all(test, not(target_arch = "wasm32")))] mod tests;
