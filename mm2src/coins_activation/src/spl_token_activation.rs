use crate::erc20_token_activation::{configured_protocol, ProtocolResolveError};
use crate::platform_coin_with_tokens::CustomTokenError;
use crate::prelude::{TryFromCoinProtocol, TryPlatformCoinFromMmCoinEnum};
use crate::token::{enable_token, EnableTokenError, EnableTokenRequest, TokenActivationOps, TokenProtocolParams};
use async_trait::async_trait;
use coins::node_url::scrub_urls_in_text;
use coins::solana::spl::{SplProtocolConf, SPL_TOKEN_PROGRAM_ID, TOKEN_2022_NOT_SUPPORTED, TOKEN_2022_PROGRAM_ID};
use coins::{coin_conf, BalanceError, CoinBalance, CoinProtocol, MarketCoinOps, MmCoinEnum, SolanaCoin, SplToken,
            SOLANA_ACTIVATION_AVAILABLE, SOLANA_NOT_AVAILABLE};
use common::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;

impl TryPlatformCoinFromMmCoinEnum for SolanaCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized,
    {
        match coin {
            MmCoinEnum::SolanaCoin(coin) => Some(coin),
            _ => None,
        }
    }
}

/// The per-token request of the Solana activation methods. Neither member is read from a request body: the
/// primary surface takes a token from the coins configuration only and ignores whatever else the member holds (CRD
/// ch.40 R40.3A.1, R40.3A.4). The compatibility surface (§40.2, §40.3) builds the request itself.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SplActivationRequest {
    /// The request-supplied `protocol` of a custom token (CRD ch.40 R40.2.1, R40.3.1), kept raw so that one that
    /// does not parse is a `TokenProtocolParseError` and not a request-parse failure. `Some` makes the token custom.
    #[serde(skip)]
    pub(crate) protocol: Option<Json>,
    /// Whether the activation also reads the chain's block height (the single-token method of the compatibility
    /// surface, CRD ch.40 R40.3.2, R40.3.3). The primary surface reads none.
    #[serde(skip)]
    pub(crate) read_block_height: bool,
}

/// Resolves the protocol of an SPL token: the request-supplied `protocol` of a custom token, else the coins
/// configuration (CRD ch.40 R40.2.2, R40.3.2).
///
/// A custom token must not be in the coins configuration (`CustomTokenError::DuplicateTickerInConfig`); that check
/// comes before the protocol is parsed. The protocol must be a Solana token protocol, which is read as it is in the
/// configuration.
pub(crate) fn resolve_spl_protocol(
    ctx: &MmArc,
    ticker: &str,
    inline_protocol: Option<&Json>,
) -> Result<SplProtocolConf, ProtocolResolveError> {
    let protocol = match inline_protocol {
        Some(inline) => {
            if !coin_conf(ctx, ticker).is_null() {
                return Err(ProtocolResolveError::Custom(
                    CustomTokenError::DuplicateTickerInConfig {
                        ticker_in_config: ticker.to_owned(),
                    },
                ));
            }
            CoinProtocol::from_conf_json(inline.clone()).map_err(|e| ProtocolResolveError::ProtocolParseError {
                ticker: ticker.to_owned(),
                error: e.to_string(),
            })?
        },
        None => configured_protocol(ctx, ticker)?,
    };
    SplProtocolConf::try_from_coin_protocol(protocol).map_err(|other| ProtocolResolveError::UnexpectedProtocol {
        ticker: ticker.to_owned(),
        protocol: other.into_inner(),
    })
}

/// The `enable_spl` entry point.
///
/// While [`SOLANA_ACTIVATION_AVAILABLE`] is `false` (Solana is postponed for the current beta) the request is
/// refused with an `Internal` error saying so, and nothing is registered. Otherwise it is the generic token
/// activation.
pub async fn enable_spl(
    ctx: MmArc,
    req: EnableTokenRequest<SplActivationRequest>,
) -> Result<SplInitResult, MmError<EnableTokenError>> {
    if !SOLANA_ACTIVATION_AVAILABLE {
        return MmError::err(EnableTokenError::Internal(SOLANA_NOT_AVAILABLE.to_owned()));
    }
    enable_token::<SplToken>(ctx, req).await
}

impl TryFromCoinProtocol for SplProtocolConf {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::SPLTOKEN {
                platform,
                token_contract_address,
                decimals,
            } => Ok(SplProtocolConf {
                platform_coin_ticker: platform,
                decimals,
                token_contract_address,
                program_id: SPL_TOKEN_PROGRAM_ID,
            }),
            CoinProtocol::SOLANATOKEN {
                platform,
                decimals,
                mint_address,
                program_id,
            } => Ok(SplProtocolConf {
                platform_coin_ticker: platform,
                decimals,
                token_contract_address: mint_address,
                program_id,
            }),
            proto => MmError::err(proto),
        }
    }
}

impl TokenProtocolParams for SplProtocolConf {
    fn platform_coin_ticker(&self) -> &str { &self.platform_coin_ticker }
}

#[derive(Debug, Serialize)]
pub struct SplInitResult {
    balances: HashMap<String, CoinBalance>,
    token_contract_address: String,
    platform_coin: String,
    /// What the single-token method of the compatibility surface reports beside the primary members; `None` on the
    /// primary surface. Never serialized.
    #[serde(skip)]
    pub(crate) compat: Option<SplCompatDetails>,
}

/// The token's own account, its balance and the chain's block height, as the compatibility surface reports them
/// (CRD ch.40 R40.3.3).
#[derive(Clone, Debug)]
pub(crate) struct SplCompatDetails {
    /// The wallet's associated token account for the mint and the token's `program_id` (R40.1.4).
    pub(crate) address: String,
    pub(crate) current_block: u64,
    pub(crate) balance: CoinBalance,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SplInitError {
    GetBalanceError(BalanceError),
    MyAddressError(String),
    /// The mint account could not be read, or another program than the token's `program_id` owns it (CRD ch.40
    /// R40.2.2, R40.3.4): the surface's `Transport` row.
    MintCheckError(String),
    /// The node failed while the block height was read (CRD ch.40 R40.3.4): the surface's `Transport` row.
    BlockHeightError(String),
    /// A Token-2022 token (CRD ch.40 D40.6B.4): refused with `Internal` and the text, before any node contact.
    Token2022(String),
}

impl From<SplInitError> for EnableTokenError {
    fn from(err: SplInitError) -> Self {
        match err {
            SplInitError::GetBalanceError(rpc_err) => rpc_err.into(),
            SplInitError::MyAddressError(e) => EnableTokenError::Internal(e),
            SplInitError::MintCheckError(e) | SplInitError::BlockHeightError(e) => EnableTokenError::Transport(e),
            SplInitError::Token2022(e) => EnableTokenError::Internal(e),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenActivationOps for SplToken {
    type PlatformCoin = SolanaCoin;
    type ActivationParams = SplActivationRequest;
    type ProtocolInfo = SplProtocolConf;
    type ActivationResult = SplInitResult;
    type ActivationError = SplInitError;

    async fn enable_token(
        ticker: String,
        platform_coin: Self::PlatformCoin,
        activation_params: Self::ActivationParams,
        protocol_conf: Self::ProtocolInfo,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>> {
        // D40.6B.4: a Token-2022 token is refused before any node is contacted and nothing is registered.
        if protocol_conf.program_id == TOKEN_2022_PROGRAM_ID {
            return MmError::err(SplInitError::Token2022(format!(
                "{}: {}",
                ticker, TOKEN_2022_NOT_SUPPORTED
            )));
        }
        // R40.2.2: before anything is read or registered, the mint account, if it exists, must be owned by the
        // token's program; a mint account that does not exist is accepted.
        platform_coin
            .check_mint_owner(&protocol_conf.token_contract_address, &protocol_conf.program_id)
            .await
            .map_err(|e| SplInitError::MintCheckError(e.into_inner().to_string()))?;
        // A request-supplied protocol makes the token custom (CRD ch.40 R40.6B.6).
        let token = Self::with_custom(
            protocol_conf.decimals,
            ticker,
            protocol_conf.token_contract_address,
            protocol_conf.program_id,
            platform_coin,
            activation_params.protocol.is_some(),
        );
        let balance = token
            .my_balance()
            .compat()
            .await
            .map_err(|e| SplInitError::GetBalanceError(e.into_inner()))?;
        let my_address = token.my_address().map_to_mm(SplInitError::MyAddressError)?;
        // R40.3.2: the block height is read before the token is registered, so a failure leaves nothing behind.
        let compat = if activation_params.read_block_height {
            let current_block = token
                .platform_coin
                .current_block()
                .compat()
                .await
                .map_to_mm(|e| SplInitError::BlockHeightError(scrub_urls_in_text(&e)))?;
            Some(SplCompatDetails {
                address: token.platform_coin.token_account_address(&token.get_info()).to_string(),
                current_block,
                balance: balance.clone(),
            })
        } else {
            None
        };
        let mut balances = HashMap::new();
        balances.insert(my_address, balance);
        let init_result = SplInitResult {
            balances,
            token_contract_address: token.conf.token_contract_address.to_string(),
            platform_coin: token.platform_coin.ticker().to_owned(),
            compat,
        };
        Ok((token, init_result))
    }

    // The platform records the token once it is registered, so that its token set and the disable hook agree
    // (CRD ch.40 T40.6B.20; the with-tokens path records its tokens the same way).
    fn on_token_registered(platform_coin: &Self::PlatformCoin, token: &Self) {
        platform_coin.add_spl_token_info(token.ticker().into(), token.get_info());
    }
}
