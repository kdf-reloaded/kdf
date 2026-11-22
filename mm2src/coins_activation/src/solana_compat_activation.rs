//! The compatibility activation methods `experimental::enable_solana_with_assets` and
//! `experimental::enable_solana_token` (CRD ch.40 §40.1-§40.3).
//!
//! Both are a second request and result form over the activation code of the primary surface (§40.3A): the platform
//! method turns its request into a [`SolanaWithTokensActivationRequest`] and runs the generic platform activation;
//! the token method runs the generic token activation with a protocol resolved from the request. The two surfaces
//! therefore share the registry, the mint-owner check, the Token-2022 refusal and the error rows.
//!
//! A coin activated through these methods sends no commitment level with its node requests, so the node's default
//! applies (the request has no commitment member).

use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq, TokenActivationRequest};
use crate::solana_with_tokens_activation::{SolanaWithTokensActivationRequest, SolanaWithTokensActivationResult};
use crate::spl_token_activation::{resolve_spl_protocol, SplActivationRequest, SplInitResult};
use crate::token::{enable_token_resolving, EnableTokenError};
use coins::solana::{SolanaHdLayout, SolanaNodeUrl};
use coins::{CoinBalance, PrivKeyActivationPolicy, SolanaActivationParams, SolanaCoin, SplToken};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;

/// One entry of `nodes` (CRD ch.40 R40.2.1).
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaCompatNode {
    url: SolanaNodeUrl,
}

/// One entry of `tokens_params` (CRD ch.40 R40.2.1): the token's ticker and, for a custom token, its `protocol`,
/// kept raw so that one that does not parse is a `TokenProtocolParseError` (R40.2.4). Other members are ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaCompatTokenParams {
    ticker: String,
    #[serde(default)]
    protocol: Option<Json>,
}

/// The request of `experimental::enable_solana_with_assets` (CRD ch.40 R40.2.1, R40.1A.5): members flat in `params`,
/// unknown members ignored. `tx_history` is an optional boolean (default `false`) whose value is never used: it has
/// no effect and never aborts the request (R40.2.5). There is no commitment member.
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaCompatPlatformRequest {
    ticker: String,
    nodes: Vec<SolanaCompatNode>,
    tokens_params: Vec<SolanaCompatTokenParams>,
    /// Accepted and never used (R40.2.5); a value that is not a boolean fails request parsing.
    #[serde(default)]
    #[allow(dead_code)]
    tx_history: bool,
    #[serde(default)]
    hd_path_layout: Option<SolanaHdLayout>,
    #[serde(default)]
    path_to_address: Option<Json>,
    #[serde(default)]
    priv_key_policy: Option<PrivKeyActivationPolicy>,
}

impl SolanaCompatPlatformRequest {
    /// The request of the shared activation code: the nodes in order and no commitment level.
    fn into_shared(self) -> SolanaWithTokensActivationRequest {
        let urls = self.nodes.iter().map(|node| node.url.as_str().to_owned()).collect();
        SolanaWithTokensActivationRequest {
            platform_request: SolanaActivationParams::without_commitment(urls),
            spl_tokens_requests: self
                .tokens_params
                .into_iter()
                .map(|token| TokenActivationRequest {
                    ticker: token.ticker,
                    request: SplActivationRequest {
                        protocol: token.protocol,
                        read_block_height: false,
                    },
                })
                .collect(),
            hd_path_layout: self.hd_path_layout,
            path_to_address: self.path_to_address,
            priv_key_policy: self.priv_key_policy,
        }
    }
}

/// The result of `experimental::enable_solana_with_assets` (CRD ch.40 R40.2.3, T40.6): exactly these members.
#[derive(Debug, Serialize)]
pub struct SolanaCompatPlatformResult {
    ticker: String,
    address: String,
    current_block: u64,
    balance: CoinBalance,
    tokens_balances: HashMap<String, CoinBalance>,
}

impl SolanaCompatPlatformResult {
    /// The compatibility form of the result of the shared activation, which holds one entry keyed by the platform
    /// address in each of its two maps.
    fn from_shared(
        ticker: String,
        mut shared: SolanaWithTokensActivationResult,
    ) -> Result<Self, MmError<EnablePlatformCoinWithTokensError>> {
        let missing = |what: &str| {
            MmError::new(EnablePlatformCoinWithTokensError::Internal(format!(
                "The activation result has no {what}"
            )))
        };
        let (address, platform_info) = shared
            .solana_addresses_infos
            .drain()
            .next()
            .ok_or_else(|| missing("platform address"))?;
        let tokens_balances = shared
            .spl_addresses_infos
            .remove(&address)
            .ok_or_else(|| missing("token balances"))?
            .balances;
        Ok(SolanaCompatPlatformResult {
            ticker,
            address,
            current_block: shared.current_block,
            balance: platform_info.balances,
            tokens_balances,
        })
    }
}

/// The `experimental::enable_solana_with_assets` entry point.
///
/// See [`activate_solana_with_assets`].
pub async fn enable_solana_with_assets(
    ctx: MmArc,
    req: SolanaCompatPlatformRequest,
) -> Result<SolanaCompatPlatformResult, MmError<EnablePlatformCoinWithTokensError>> {
    activate_solana_with_assets(ctx, req).await
}

/// The platform activation of the compatibility surface (CRD ch.40 R40.2.2): the generic platform activation over
/// the request of [`SolanaCompatPlatformRequest::into_shared`], its result in the form of R40.2.3. The generic
/// activation registers the platform and its tokens only after every step has succeeded.
#[doc(hidden)]
pub async fn activate_solana_with_assets(
    ctx: MmArc,
    req: SolanaCompatPlatformRequest,
) -> Result<SolanaCompatPlatformResult, MmError<EnablePlatformCoinWithTokensError>> {
    let ticker = req.ticker.clone();
    let shared_req = EnablePlatformCoinWithTokensReq {
        ticker: ticker.clone(),
        client_id: 0,
        request: req.into_shared(),
    };
    let shared = enable_platform_coin_with_tokens::<SolanaCoin>(ctx, shared_req).await?;
    SolanaCompatPlatformResult::from_shared(ticker, shared)
}

/// The `activation_params` of the single-token method: required, an object, its members ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaCompatActivationParams {}

/// The request of `experimental::enable_solana_token` (CRD ch.40 R40.3.1). `protocol` is raw for the reason given at
/// [`SolanaCompatTokenParams`].
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaCompatTokenRequest {
    ticker: String,
    #[allow(dead_code)]
    activation_params: SolanaCompatActivationParams,
    #[serde(default)]
    protocol: Option<Json>,
}

/// The result of `experimental::enable_solana_token` (CRD ch.40 R40.3.3).
#[derive(Debug, Serialize)]
pub struct SolanaCompatTokenResult {
    ticker: String,
    /// The wallet's associated token account for the mint and the token's `program_id` (R40.1.4).
    address: String,
    current_block: u64,
    balance: CoinBalance,
}

impl SolanaCompatTokenResult {
    fn from_init(ticker: String, init: SplInitResult) -> Result<Self, MmError<EnableTokenError>> {
        let details = init.compat.ok_or_else(|| {
            MmError::new(EnableTokenError::Internal(
                "The token activation result has no compatibility details".to_owned(),
            ))
        })?;
        Ok(SolanaCompatTokenResult {
            ticker,
            address: details.address,
            current_block: details.current_block,
            balance: details.balance,
        })
    }
}

/// The `experimental::enable_solana_token` entry point.
///
/// See [`activate_solana_token`].
pub async fn enable_solana_token(
    ctx: MmArc,
    req: SolanaCompatTokenRequest,
) -> Result<SolanaCompatTokenResult, MmError<EnableTokenError>> {
    activate_solana_token(ctx, req).await
}

/// The token activation of the compatibility surface (CRD ch.40 R40.3.2): the generic token activation with the
/// protocol resolved from the request's `protocol` or the coins configuration. The platform is found among the active
/// coins whichever surface activated it. The activation reads the block height before the token is registered.
#[doc(hidden)]
pub async fn activate_solana_token(
    ctx: MmArc,
    req: SolanaCompatTokenRequest,
) -> Result<SolanaCompatTokenResult, MmError<EnableTokenError>> {
    let ticker = req.ticker.clone();
    let params = SplActivationRequest {
        protocol: req.protocol,
        read_block_height: true,
    };
    let init = enable_token_resolving::<SplToken, _>(ctx, req.ticker, params, |ctx, ticker, params| {
        resolve_spl_protocol(ctx, ticker, params.protocol.as_ref()).map_to_mm(EnableTokenError::from)
    })
    .await?;
    SolanaCompatTokenResult::from_init(ticker, init)
}
