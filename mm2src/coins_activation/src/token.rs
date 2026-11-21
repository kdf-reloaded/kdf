/// Contains token activation traits and their implementations for various coins
///
use crate::context::lock_activation_under;
use crate::platform_coin_with_tokens::CustomTokenError;
use crate::prelude::*;
use async_trait::async_trait;
use coins::utxo::rpc_clients::UtxoRpcError;
use coins::{lp_coinfind, lp_coinfind_or_err, BalanceError, CoinProtocol, CoinsContext, MmCoinEnum,
            UnexpectedDerivationMethod};
use common::{HttpStatusCode, StatusCode};
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};

pub trait TokenProtocolParams {
    fn platform_coin_ticker(&self) -> &str;
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait TokenActivationOps: Into<MmCoinEnum> + Clone {
    type PlatformCoin: TryPlatformCoinFromMmCoinEnum + Clone;
    type ActivationParams;
    type ProtocolInfo: TokenProtocolParams + TryFromCoinProtocol;
    type ActivationResult;
    type ActivationError: NotMmError;

    async fn enable_token(
        ticker: String,
        platform_coin: Self::PlatformCoin,
        activation_params: Self::ActivationParams,
        protocol_conf: Self::ProtocolInfo,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>>;

    /// Records the token on its platform coin once the token is registered, so a failed activation leaves the
    /// platform's token set unchanged (CRD R36.2.2).
    fn on_token_registered(_platform_coin: &Self::PlatformCoin, _token: &Self) {}
}

/// What a single-token activation found wrong with the derivation method of the platform coin.
#[derive(Debug, Display, PartialEq, Serialize)]
pub enum TokenDerivationError {
    #[display(fmt = "Iguana private key is unavailable")]
    IguanaPrivKeyUnavailable,
    #[display(fmt = "HD wallet is unavailable")]
    HDWalletUnavailable,
    /// The token needs a single address but the platform coin is HD-derived (CRD R35.2.5).
    #[display(fmt = "A single-address wallet is expected")]
    ExpectedSingleAddress,
}

impl From<UnexpectedDerivationMethod> for TokenDerivationError {
    fn from(e: UnexpectedDerivationMethod) -> Self {
        match e {
            UnexpectedDerivationMethod::IguanaPrivKeyUnavailable => TokenDerivationError::IguanaPrivKeyUnavailable,
            UnexpectedDerivationMethod::HDWalletUnavailable => TokenDerivationError::HDWalletUnavailable,
        }
    }
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
// `UnexpectedTokenProtocol` carries a `CoinProtocol` by value for diagnostics on
// a cold error path. Boxing it (clippy's suggestion) would force `Box::new` at
// every construction site in this compatibility-sensitive activation code for a
// negligible size win on a rarely built RPC error enum. The per-item allow is
// the established precedent for this lint across coins_activation.
#[allow(clippy::large_enum_variant)]
pub enum EnableTokenError {
    #[display(fmt = "Token {} is already activated", _0)]
    TokenIsAlreadyActivated(String),
    #[display(fmt = "Token {} config is not found", _0)]
    TokenConfigIsNotFound(String),
    #[display(fmt = "Token {} protocol parsing failed: {}", ticker, error)]
    TokenProtocolParseError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Unexpected token protocol {:?} for {}", protocol, ticker)]
    UnexpectedTokenProtocol {
        ticker: String,
        protocol: CoinProtocol,
    },
    #[display(fmt = "Custom token rejected: {}", _0)]
    CustomTokenError(CustomTokenError),
    #[display(fmt = "Platform coin {} is not activated", _0)]
    PlatformCoinIsNotActivated(String),
    #[display(fmt = "{} is not a platform coin for token {}", platform_coin_ticker, token_ticker)]
    UnsupportedPlatformCoin {
        platform_coin_ticker: String,
        token_ticker: String,
    },
    #[display(fmt = "{}", _0)]
    UnexpectedDerivationMethod(TokenDerivationError),
    Transport(String),
    #[display(fmt = "Invalid payload: {}", _0)]
    InvalidPayload(String),
    /// Reserved by the chapter: no failure maps here today.
    #[allow(dead_code)]
    #[display(fmt = "Private key policy is not allowed: {}", _0)]
    PrivKeyPolicyNotAllowed(String),
    /// The token and its active platform are of different families: a TRC20 token on an EVM platform, or an ERC-20
    /// token on a TRON platform (CRD ch.21 R21.10B.2).
    #[display(fmt = "The token protocol names a platform other than the activated one")]
    PlatformCoinMismatch,
    #[display(fmt = "Could not fetch the balance: {}", _0)]
    CouldNotFetchBalance(String),
    /// The token's coin-configuration entry holds an unusable value, e.g. a `gas_limit` member that is not a
    /// non-negative integer (CRD R57.5.1). The message names the key.
    #[display(fmt = "Invalid config: {}", _0)]
    InvalidConfig(String),
    Internal(String),
}

impl From<CoinConfWithProtocolError> for EnableTokenError {
    fn from(err: CoinConfWithProtocolError) -> Self {
        match err {
            CoinConfWithProtocolError::ConfigIsNotFound(ticker) => EnableTokenError::TokenConfigIsNotFound(ticker),
            CoinConfWithProtocolError::CoinProtocolParseError { ticker, err } => {
                EnableTokenError::TokenProtocolParseError {
                    ticker,
                    error: err.to_string(),
                }
            },
            CoinConfWithProtocolError::UnexpectedProtocol { ticker, protocol } => {
                EnableTokenError::UnexpectedTokenProtocol { ticker, protocol }
            },
        }
    }
}

impl From<BalanceError> for EnableTokenError {
    fn from(e: BalanceError) -> Self {
        match e {
            BalanceError::Transport(e) | BalanceError::InvalidResponse(e) => EnableTokenError::Transport(e),
            // A balance read sends no fee cap; the arm keeps the node's text so the conversion is total.
            e @ BalanceError::FeeCapBelowBaseFee(_) => EnableTokenError::Transport(e.to_string()),
            BalanceError::UnexpectedDerivationMethod(e) => EnableTokenError::UnexpectedDerivationMethod(e.into()),
            BalanceError::Internal(e) | BalanceError::WalletStorageError(e) => EnableTokenError::Internal(e),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct EnableTokenRequest<T> {
    ticker: String,
    activation_params: T,
}

pub async fn enable_token<Token>(
    ctx: MmArc,
    req: EnableTokenRequest<Token::ActivationParams>,
) -> Result<Token::ActivationResult, MmError<EnableTokenError>>
where
    Token: TokenActivationOps,
    EnableTokenError: From<Token::ActivationError>,
{
    enable_token_resolving::<Token, _>(ctx, req.ticker, req.activation_params, |ctx, ticker, _| {
        let (_, token_protocol): (_, Token::ProtocolInfo) = coin_conf_with_protocol(ctx, ticker)
            .mm_err(|e: CoinConfWithProtocolError| -> EnableTokenError { e.into() })?;
        Ok(token_protocol)
    })
    .await
}

/// [`enable_token`] with the token's protocol resolved by `resolve`: the coins configuration for the generic
/// activation, a request-supplied protocol where the request may carry one (CRD ch.40 R40.3.2). `resolve` runs after
/// a first already-activated check and before the activation locks are taken, since the locks need the platform
/// ticker that the protocol names.
pub(crate) async fn enable_token_resolving<Token, Resolve>(
    ctx: MmArc,
    ticker: String,
    activation_params: Token::ActivationParams,
    resolve: Resolve,
) -> Result<Token::ActivationResult, MmError<EnableTokenError>>
where
    Token: TokenActivationOps,
    EnableTokenError: From<Token::ActivationError>,
    Resolve: FnOnce(&MmArc, &str, &Token::ActivationParams) -> Result<Token::ProtocolInfo, MmError<EnableTokenError>>,
{
    // The lock needs the platform ticker, which the token's protocol names, so the protocol is resolved first. A
    // registered ticker keeps the refusal it has always had, before any other check.
    if let Ok(Some(_)) = lp_coinfind(&ctx, &ticker).await {
        return MmError::err(EnableTokenError::TokenIsAlreadyActivated(ticker));
    }
    let token_protocol = resolve(&ctx, &ticker, &activation_params)?;
    let platform_ticker = token_protocol.platform_coin_ticker().to_owned();

    // R55.1.12 with D43.1F.2 (b): the lock of the token (exclusive) and the lock of its platform (shared) are held
    // together through registration, so a `disable_coin` of the platform cannot run in between. The checks below
    // follow the wait.
    let _activation_guard = lock_activation_under(&ctx, &platform_ticker, &[&ticker])
        .await
        .map_to_mm(EnableTokenError::Internal)?;
    if let Ok(Some(_)) = lp_coinfind(&ctx, &ticker).await {
        return MmError::err(EnableTokenError::TokenIsAlreadyActivated(ticker));
    }

    // The platform must be active: a passive platform counts as not activated (CRD ch.43 R43.1F.6).
    let platform_coin = lp_coinfind_or_err(&ctx, &platform_ticker)
        .await
        .mm_err(|_| EnableTokenError::PlatformCoinIsNotActivated(token_protocol.platform_coin_ticker().to_owned()))?;

    let platform_coin = Token::PlatformCoin::try_from_mm_coin(platform_coin).or_mm_err(|| {
        EnableTokenError::UnsupportedPlatformCoin {
            platform_coin_ticker: token_protocol.platform_coin_ticker().into(),
            token_ticker: ticker.clone(),
        }
    })?;

    // The work the build spawns belongs to the instance, and ends if the instance is not registered (CRD ch.43
    // R43.1E.5 item 4, §43.1E not-established item (1)).
    let work = coins::coin_work::PendingCoinWork::new();
    let (token, activation_result) = work
        .scope(Token::enable_token(
            ticker,
            platform_coin.clone(),
            activation_params,
            token_protocol,
        ))
        .await
        .mm_err(Into::into)?;

    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    coins_ctx
        .add_token_with_work(&platform_ticker, token.clone().into(), work)
        .await
        .mm_err(|e| EnableTokenError::TokenIsAlreadyActivated(e.ticker))?;
    Token::on_token_registered(&platform_coin, &token);

    Ok(activation_result)
}

impl From<UtxoRpcError> for EnableTokenError {
    fn from(err: UtxoRpcError) -> Self {
        match err {
            UtxoRpcError::Transport(e) | UtxoRpcError::ResponseParseError(e) => {
                EnableTokenError::Transport(e.to_string())
            },
            UtxoRpcError::InvalidResponse(e) => EnableTokenError::Transport(e),
            UtxoRpcError::Internal(e) => EnableTokenError::Internal(e),
        }
    }
}

impl HttpStatusCode for EnableTokenError {
    fn status_code(&self) -> StatusCode {
        match self {
            EnableTokenError::TokenIsAlreadyActivated(_)
            | EnableTokenError::PlatformCoinIsNotActivated(_)
            | EnableTokenError::TokenConfigIsNotFound { .. }
            | EnableTokenError::UnexpectedTokenProtocol { .. }
            | EnableTokenError::CustomTokenError(_)
            | EnableTokenError::InvalidPayload(_)
            | EnableTokenError::PlatformCoinMismatch => StatusCode::BAD_REQUEST,
            EnableTokenError::TokenProtocolParseError { .. }
            | EnableTokenError::UnsupportedPlatformCoin { .. }
            | EnableTokenError::UnexpectedDerivationMethod(_)
            | EnableTokenError::Transport(_)
            | EnableTokenError::PrivKeyPolicyNotAllowed(_)
            | EnableTokenError::CouldNotFetchBalance(_)
            | EnableTokenError::InvalidConfig(_)
            | EnableTokenError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
