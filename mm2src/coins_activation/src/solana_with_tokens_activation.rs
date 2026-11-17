use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq, GetPlatformBalance,
                                       PlatformWithTokensActivationOps, RegisterTokenInfo, TokenActivationParams,
                                       TokenActivationRequest, TokenAsMmCoinInitializer, TokenInitializer, TokenOf};
use crate::prelude::*;
use crate::prelude::{CoinAddressInfo, TokenBalances, TryFromCoinProtocol, TxHistory};
use crate::spl_token_activation::SplActivationRequest;
use async_trait::async_trait;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::node_url::scrub_urls_in_text;
use coins::rpc_command::get_my_address::PathToAddress;
use coins::solana::spl::SplProtocolConf;
use coins::solana::{solana_hd_derivation_path, solana_hd_secret, SolanaHdLayout};
use coins::{coin_conf, solana_coin_from_conf_and_params, CoinBalance, CoinProtocol, MarketCoinOps,
            PrivKeyActivationPolicy, SolanaActivationParams, SolanaCoin, SplToken, SOLANA_ACTIVATION_AVAILABLE,
            SOLANA_NOT_AVAILABLE};
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
use crypto::{Bip44Chain, CryptoCtx, KeyPairPolicy};
use futures::future::AbortHandle;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;

/// The `enable_solana_with_tokens` entry point.
///
/// While [`SOLANA_ACTIVATION_AVAILABLE`] is `false` (Solana is postponed for the current beta) the request is
/// refused with a `PlatformCoinCreationError` saying so, and nothing is registered. Otherwise it is the generic
/// platform activation.
pub async fn enable_solana_with_tokens(
    ctx: MmArc,
    req: EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest>,
) -> Result<SolanaWithTokensActivationResult, MmError<EnablePlatformCoinWithTokensError>> {
    if !SOLANA_ACTIVATION_AVAILABLE {
        return MmError::err(EnablePlatformCoinWithTokensError::PlatformCoinCreationError {
            ticker: req.ticker,
            error: SOLANA_NOT_AVAILABLE.to_owned(),
        });
    }
    enable_platform_coin_with_tokens::<SolanaCoin>(ctx, req).await
}

pub struct SplTokenInitializer {
    platform_coin: SolanaCoin,
}

impl TokenOf for SplToken {
    type PlatformCoin = SolanaCoin;
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenInitializer for SplTokenInitializer {
    type Token = SplToken;
    type TokenActivationRequest = SplActivationRequest;
    type TokenProtocol = SplProtocolConf;
    /// Initialising a token reads no node and cannot fail: its key is parsed with its protocol.
    type InitTokensError = std::convert::Infallible;

    fn tokens_requests_from_platform_request(
        platform_params: &SolanaWithTokensActivationRequest,
    ) -> Vec<TokenActivationRequest<Self::TokenActivationRequest>> {
        platform_params.spl_tokens_requests.clone()
    }

    async fn enable_tokens(
        &self,
        activation_params: Vec<TokenActivationParams<SplActivationRequest, SplProtocolConf>>,
    ) -> Result<Vec<SplToken>, MmError<Self::InitTokensError>> {
        let tokens = activation_params
            .into_iter()
            .map(|param| {
                SplToken::new(
                    param.protocol.decimals,
                    param.ticker,
                    param.protocol.token_contract_address,
                    param.protocol.program_id,
                    self.platform_coin.clone(),
                )
            })
            .collect();
        Ok(tokens)
    }

    fn platform_coin(&self) -> &SolanaCoin { &self.platform_coin }
}

impl RegisterTokenInfo<SplToken> for SolanaCoin {
    fn register_token_info(&self, token: &SplToken) { self.add_spl_token_info(token.ticker().into(), token.get_info()) }
}

/// The `enable_solana_with_tokens` request. The key members (CRD ch.40 R40.1A.5) are all optional:
/// `hd_path_layout` (an unknown value fails request parsing), `path_to_address` and `priv_key_policy` (any form
/// other than a bare or tagged `ContextPrivKey`, `IguanaPrivKey` or `Trezor` fails request parsing).
#[derive(Clone, Debug, Deserialize)]
pub struct SolanaWithTokensActivationRequest {
    #[serde(flatten)]
    platform_request: SolanaActivationParams,
    spl_tokens_requests: Vec<TokenActivationRequest<SplActivationRequest>>,
    #[serde(default)]
    hd_path_layout: Option<SolanaHdLayout>,
    // Kept raw: it matters under an HD login only, where a value that does not parse is a platform-coin creation
    // error (CRD ch.40 R40.1A.6); under a single-key login it is ignored whatever it holds.
    #[serde(default)]
    path_to_address: Option<Json>,
    #[serde(default)]
    priv_key_policy: Option<PrivKeyActivationPolicy>,
}

fn creation_error(ticker: &str, error: impl Into<String>) -> MmError<SolanaWithTokensActivationError> {
    MmError::new(SolanaWithTokensActivationError::PlatformCoinCreationError {
        ticker: ticker.to_owned(),
        error: error.into(),
    })
}

/// Whether the request asks for `IguanaPrivKey`: the single key, under either login. Under a global-HD login it is
/// the context's internal key (CRD ch.40 R40.1A.1, maintainer decision of 2026-11-17).
fn iguana_key_requested(priv_key_policy: &Option<PrivKeyActivationPolicy>) -> bool {
    matches!(priv_key_policy, Some(PrivKeyActivationPolicy::IguanaPrivKey))
}

/// Refuses a hardware `priv_key_policy` before any node is contacted (CRD ch.40 R40.1A.6). An absent value,
/// `ContextPrivKey` and `IguanaPrivKey` pass; `IguanaPrivKey` is honoured under either login.
fn reject_unsupported_policy(
    ticker: &str,
    priv_key_policy: &Option<PrivKeyActivationPolicy>,
) -> Result<(), MmError<SolanaWithTokensActivationError>> {
    match priv_key_policy {
        Some(PrivKeyActivationPolicy::Trezor) => Err(creation_error(
            ticker,
            "the hardware key policy is not supported for Solana coins; activate with the local key",
        )),
        _ => Ok(()),
    }
}

/// The account of a request under an HD login (CRD ch.40 R40.1A.2, R40.1A.6): `path_to_address` defaults to
/// account 0, `External`, address 0; a value that does not parse, a `chain` other than `External` and an
/// `address_id` other than 0 are platform-coin creation errors, because neither layout has an address-index level.
fn hd_account_of(
    ticker: &str,
    path_to_address: &Option<Json>,
) -> Result<u32, MmError<SolanaWithTokensActivationError>> {
    let path_to_address = match path_to_address {
        Some(path) if !path.is_null() => serde_json::from_value::<PathToAddress>(path.clone()).map_to_mm(|e| {
            SolanaWithTokensActivationError::PlatformCoinCreationError {
                ticker: ticker.to_owned(),
                error: format!("Invalid `path_to_address`: {e}"),
            }
        })?,
        _ => PathToAddress::default(),
    };
    if path_to_address.chain != Bip44Chain::External {
        return Err(creation_error(
            ticker,
            "Invalid `path_to_address`: `chain` must be `External`, Solana paths have no address-index level",
        ));
    }
    if path_to_address.address_id != 0 {
        return Err(creation_error(
            ticker,
            "Invalid `path_to_address`: `address_id` must be 0, Solana paths have no address-index level",
        ));
    }
    Ok(path_to_address.account_id)
}

impl TxHistory for SolanaWithTokensActivationRequest {
    fn tx_history(&self) -> bool { false }
}

/// The `derivation_method` of the result (CRD ch.40 R40.1A.7): the full path under a global-HD login (the request
/// was validated when the coin was built), `Iguana` under a single-key login and whenever the request asks for
/// `IguanaPrivKey`.
pub(crate) fn derivation_method_of(
    ctx: &MmArc,
    ticker: &str,
    activation_request: &SolanaWithTokensActivationRequest,
) -> Result<DerivationMethod, MmError<SolanaWithTokensActivationError>> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| SolanaWithTokensActivationError::Internal(e.to_string()))?;
    match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(_) if !iguana_key_requested(&activation_request.priv_key_policy) => {
            let account_id = hd_account_of(ticker, &activation_request.path_to_address)?;
            let layout = activation_request.hd_path_layout.unwrap_or_default();
            solana_hd_derivation_path(&coin_conf(ctx, ticker), layout, account_id)
                .map(DerivationMethod::HDWallet)
                .map_to_mm(SolanaWithTokensActivationError::Internal)
        },
        _ => Ok(DerivationMethod::Iguana),
    }
}

#[derive(Debug, Serialize)]
pub struct SolanaWithTokensActivationResult {
    current_block: u64,
    solana_addresses_infos: HashMap<String, CoinAddressInfo<CoinBalance>>,
    spl_addresses_infos: HashMap<String, CoinAddressInfo<TokenBalances>>,
}

impl GetPlatformBalance for SolanaWithTokensActivationResult {
    fn get_platform_balance(&self) -> BigDecimal {
        self.solana_addresses_infos
            .iter()
            .fold(BigDecimal::from(0), |total, (_, addr_info)| {
                &total + &addr_info.balances.get_total()
            })
    }
}

impl CurrentBlock for SolanaWithTokensActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

#[derive(Debug)]
pub enum SolanaWithTokensActivationError {
    PlatformCoinCreationError { ticker: String, error: String },
    UnableToRetrieveMyAddress(String),
    Transport(String),
    Internal(String),
}

impl From<SolanaWithTokensActivationError> for EnablePlatformCoinWithTokensError {
    fn from(e: SolanaWithTokensActivationError) -> Self {
        match e {
            SolanaWithTokensActivationError::PlatformCoinCreationError { ticker, error } => {
                EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error }
            },
            SolanaWithTokensActivationError::UnableToRetrieveMyAddress(e) => {
                EnablePlatformCoinWithTokensError::Internal(e)
            },
            SolanaWithTokensActivationError::Transport(e) => EnablePlatformCoinWithTokensError::Transport(e),
            SolanaWithTokensActivationError::Internal(e) => EnablePlatformCoinWithTokensError::Internal(e),
        }
    }
}

pub struct SolanaProtocolInfo {}

impl TryFromCoinProtocol for SolanaProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::SOLANA => Ok(SolanaProtocolInfo {}),
            protocol => MmError::err(protocol),
        }
    }
}

#[async_trait]
impl PlatformWithTokensActivationOps for SolanaCoin {
    type ActivationRequest = SolanaWithTokensActivationRequest;
    type PlatformProtocolInfo = SolanaProtocolInfo;
    type ActivationResult = SolanaWithTokensActivationResult;
    type ActivationError = SolanaWithTokensActivationError;

    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        platform_conf: Json,
        activation_request: Self::ActivationRequest,
        _protocol_conf: Self::PlatformProtocolInfo,
        priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        let crypto_ctx =
            CryptoCtx::from_ctx(&ctx).mm_err(|e| SolanaWithTokensActivationError::Internal(e.to_string()))?;
        reject_unsupported_policy(&ticker, &activation_request.priv_key_policy)?;
        // R40.1A.1: a single-key login uses the key passed in; a global-HD login derives its own key, or uses the
        // context's internal key when the request asks for `IguanaPrivKey` (`hd_path_layout` and `path_to_address`
        // are then ignored).
        let hd_secret;
        let internal_secret;
        let key: &[u8] = match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::GlobalHDAccount(global_hd) if !iguana_key_requested(&activation_request.priv_key_policy) => {
                let account_id = hd_account_of(&ticker, &activation_request.path_to_address)?;
                let layout = activation_request.hd_path_layout.unwrap_or_default();
                let path = solana_hd_derivation_path(&platform_conf, layout, account_id).map_to_mm(|error| {
                    SolanaWithTokensActivationError::PlatformCoinCreationError {
                        ticker: ticker.clone(),
                        error,
                    }
                })?;
                hd_secret = solana_hd_secret(global_hd, &path).map_to_mm(|error| {
                    SolanaWithTokensActivationError::PlatformCoinCreationError {
                        ticker: ticker.clone(),
                        error,
                    }
                })?;
                &hd_secret
            },
            // `IguanaPrivKey` under a global-HD login: the context's internal key, derived from the seed, the key the
            // UTXO family uses for that policy.
            KeyPairPolicy::GlobalHDAccount(_) => {
                internal_secret = crypto_ctx.mm2_internal_privkey_secret();
                internal_secret.as_slice()
            },
            KeyPairPolicy::Iguana => priv_key,
        };
        let platform_coin =
            solana_coin_from_conf_and_params(&ticker, &platform_conf, activation_request.platform_request, key)
                .await
                .map_to_mm(|error| SolanaWithTokensActivationError::PlatformCoinCreationError { ticker, error })?;
        Ok(platform_coin)
    }

    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String> {
        activation_request
            .spl_tokens_requests
            .iter()
            .map(|token| token.ticker.clone())
            .collect()
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(SplTokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result<XPubExtractor>(
        &self,
        ctx: &MmArc,
        activation_request: &Self::ActivationRequest,
        _xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<Self::ActivationResult, MmError<Self::ActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        let my_address = self
            .my_address()
            .map_to_mm(Self::ActivationError::UnableToRetrieveMyAddress)?;
        let derivation_method = derivation_method_of(ctx, self.ticker(), activation_request)?;
        // A failure to read the block height, the SOL balance or a token balance is reported as a platform coin
        // creation error naming the platform ticker (CRD ch.40 R40.2.4); a node is named host-only in its text.
        let read_failure = |error: String| SolanaWithTokensActivationError::PlatformCoinCreationError {
            ticker: self.ticker().to_owned(),
            error: scrub_urls_in_text(&error),
        };
        let current_block = self.current_block().compat().await.map_to_mm(read_failure)?;
        let solana_balance = self
            .my_balance()
            .compat()
            .await
            .map_err(|e| read_failure(e.into_inner().to_string()))?;
        let mut token_balances = HashMap::new();
        let token_infos = self.get_spl_tokens_infos();
        for (token_ticker, info) in token_infos.into_iter() {
            let balance = self
                .my_balance_spl(&info.clone())
                .await
                .map_err(|e| read_failure(e.into_inner().to_string()))?;
            token_balances.insert(token_ticker.to_owned(), balance);
        }
        let mut result = SolanaWithTokensActivationResult {
            current_block,
            solana_addresses_infos: HashMap::new(),
            spl_addresses_infos: HashMap::new(),
        };
        result
            .solana_addresses_infos
            .insert(my_address.clone(), CoinAddressInfo {
                derivation_method: derivation_method.clone(),
                pubkey: my_address.clone(),
                balances: solana_balance,
            });

        result.spl_addresses_infos.insert(my_address.clone(), CoinAddressInfo {
            derivation_method,
            pubkey: my_address,
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
        // Solana has no transaction history yet (CRD ch.40 R40.6B.15) and `tx_history()` is `false`, so this is not
        // reached. Should that change, no task starts: the handle belongs to nothing and aborting it does nothing.
        let (abort_handle, _abort_registration) = AbortHandle::new_pair();
        abort_handle
    }
}
