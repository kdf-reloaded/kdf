use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       EnablePlatformCoinWithTokensReq, GetPlatformBalance, InitTokensAsMmCoinsError,
                                       PlatformWithTokensActivationOps, RegisterTokenInfo, TokenActivationRequest,
                                       TokenAsMmCoinInitializer, TokenOf};
use crate::prelude::*;
use crate::prelude::{CoinAddressInfo, TokenBalances, TryFromCoinProtocol, TxHistory};
use crate::spl_token_activation::{resolve_spl_protocol, SplActivationRequest};
use async_trait::async_trait;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::node_url::scrub_urls_in_text;
use coins::rpc_command::get_my_address::PathToAddress;
use coins::solana::rpc_pool::solana_node_order;
use coins::solana::spl::{TOKEN_2022_NOT_SUPPORTED, TOKEN_2022_PROGRAM_ID};
use coins::solana::{solana_hd_derivation_path, solana_hd_secret, SolanaHdLayout};
use coins::{coin_conf, solana_coin_with_node_order, CoinBalance, CoinProtocol, MarketCoinOps, MmCoinEnum,
            PrivKeyActivationPolicy, SolanaActivationParams, SolanaCoin, SplToken};
use common::mm_number::BigDecimal;
use crypto::{Bip44Chain, CryptoCtx, KeyPairPolicy};
use futures::future::AbortHandle;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;

/// The `enable_solana_with_tokens` entry point: the generic platform activation.
pub async fn enable_solana_with_tokens(
    ctx: MmArc,
    req: EnablePlatformCoinWithTokensReq<SolanaWithTokensActivationRequest>,
) -> Result<SolanaWithTokensActivationResult, MmError<EnablePlatformCoinWithTokensError>> {
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
impl TokenAsMmCoinInitializer for SplTokenInitializer {
    type PlatformCoin = SolanaCoin;
    type ActivationRequest = SolanaWithTokensActivationRequest;

    /// Enables the requested tokens in passes, each covering every token in request order before the next starts,
    /// so the error reported is that of the earliest failing pass (CRD ch.40 R40.2.2): the resolution of every
    /// token's protocol, from its request-supplied `protocol` or the coins configuration; then the platform
    /// consistency of every token (`PlatformCoinMismatch`); then the initialisation, which reads each mint account
    /// from a node and checks its owner (`Transport` on a node failure or a foreign owner). Nothing is registered
    /// here, so a failure leaves nothing behind.
    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<MmCoinEnum>, MmError<InitTokensAsMmCoinsError>> {
        let requests = &request.spl_tokens_requests;

        let mut protocols = Vec::with_capacity(requests.len());
        for token_request in requests {
            let protocol = resolve_spl_protocol(&ctx, &token_request.ticker, token_request.request.protocol.as_ref())
                .map_to_mm(InitTokensAsMmCoinsError::from)?;
            protocols.push(protocol);
        }

        if protocols
            .iter()
            .any(|protocol| protocol.platform_coin_ticker != self.platform_coin.ticker())
        {
            return MmError::err(InitTokensAsMmCoinsError::PlatformCoinMismatch);
        }

        // The mint accounts of all tokens are read from one node (CRD ch.40 R40.1.5).
        let mints: Vec<_> = protocols
            .iter()
            .map(|protocol| (protocol.token_contract_address, protocol.program_id))
            .collect();
        self.platform_coin
            .check_mint_owners(&mints)
            .await
            .mm_err(|e| InitTokensAsMmCoinsError::Transport(e.to_string()))?;
        let mut tokens = Vec::with_capacity(requests.len());
        for (token_request, protocol) in requests.iter().zip(protocols) {
            tokens.push(SplToken::with_custom(
                protocol.decimals,
                token_request.ticker.clone(),
                protocol.token_contract_address,
                protocol.program_id,
                self.platform_coin.clone(),
                token_request.request.protocol.is_some(),
            ));
        }

        // The platform's token record is written at registration, not here (CRD ch.43 D43.1F.2 (b)).
        Ok(tokens.into_iter().map(Into::into).collect())
    }
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
    pub(crate) platform_request: SolanaActivationParams,
    pub(crate) spl_tokens_requests: Vec<TokenActivationRequest<SplActivationRequest>>,
    #[serde(default)]
    pub(crate) hd_path_layout: Option<SolanaHdLayout>,
    // Kept raw: it matters under an HD login only, where a value that does not parse is a platform-coin creation
    // error (CRD ch.40 R40.1A.6); under a single-key login it is ignored whatever it holds.
    #[serde(default)]
    pub(crate) path_to_address: Option<Json>,
    #[serde(default)]
    pub(crate) priv_key_policy: Option<PrivKeyActivationPolicy>,
    /// Whether the platform's transaction history runs (CRD ch.40 R40.6C.1). Default `false`.
    #[serde(default)]
    pub(crate) tx_history: bool,
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
    fn tx_history(&self) -> bool { self.tx_history }
}

/// Whether the activation starts the Solana history process: when `tx_history` is asked and the platform is newly
/// activated. The re-activation of a passive platform starts none; the platform keeps the one it had (CRD ch.40
/// R40.6C.1, ch.43 D43.1F.5 (a)).
pub(crate) fn starts_solana_history(tx_history: bool, reactivation: bool) -> bool { tx_history && !reactivation }

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
    pub(crate) current_block: u64,
    pub(crate) solana_addresses_infos: HashMap<String, CoinAddressInfo<CoinBalance>>,
    pub(crate) spl_addresses_infos: HashMap<String, CoinAddressInfo<TokenBalances>>,
    /// Present and `true` only when the swap evidence was relaxed to a single host (`swap_evidence_allow_single_host`).
    #[serde(skip_serializing_if = "coins::swap_evidence::is_false")]
    pub(crate) swap_evidence_single_host: bool,
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
    PlatformCoinCreationError {
        ticker: String,
        error: String,
    },
    /// A `swap_evidence_node` outside the node list (CRD ch.51 R100 item 2).
    SwapEvidenceNodeNotInList(coins::swap_evidence::SwapEvidenceNodeNotInList),
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
            SolanaWithTokensActivationError::SwapEvidenceNodeNotInList(error) => {
                EnablePlatformCoinWithTokensError::SwapEvidenceNodeNotInList(error)
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

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
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
        // A `swap_evidence_node` outside the node list is refused before any node is contacted (CRD ch.51 R100).
        let platform_request = activation_request
            .platform_request
            .clone()
            .with_allow_single_host(coins::swap_evidence::allow_single_host(&ctx));
        let (_, single_host) = platform_request
            .swap_evidence_policy_relaxed(&ticker)
            .map_err(|e| MmError::new(SolanaWithTokensActivationError::SwapEvidenceNodeNotInList(e)))?;
        coins::swap_evidence::record_single_host_outcome(&ctx, &ticker, single_host);
        // D40.6B.4: a Token-2022 inline token is refused before any node is contacted and nothing is
        // registered. A token whose protocol does not resolve, or that names another platform, is left to the
        // later token initialisation, which answers the error of its own row.
        for token in &activation_request.spl_tokens_requests {
            if let Ok(protocol) = resolve_spl_protocol(&ctx, &token.ticker, token.request.protocol.as_ref()) {
                if protocol.platform_coin_ticker == ticker && protocol.program_id == TOKEN_2022_PROGRAM_ID {
                    return Err(creation_error(
                        &ticker,
                        format!("{}: {}", token.ticker, TOKEN_2022_NOT_SUPPORTED),
                    ));
                }
            }
        }
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
        let node_order = solana_node_order(&ctx);
        let tx_history = activation_request.tx_history;
        // R40.6D.2: the program of the entry, read once; the activation succeeds whatever the check finds.
        let swap_program = coins::solana::swap_program_id_from_conf(&platform_conf).map_to_mm(|error| {
            SolanaWithTokensActivationError::PlatformCoinCreationError {
                ticker: ticker.clone(),
                error,
            }
        })?;
        let platform_coin = solana_coin_with_node_order(&ticker, &platform_conf, platform_request, key, node_order)
            .await
            .map_to_mm(|error| SolanaWithTokensActivationError::PlatformCoinCreationError { ticker, error })?;
        platform_coin.check_swap_program(swap_program).await;
        // R40.6C.1: the history is enabled before the instance is registered, so no answer sees it half started; the
        // writer itself starts in `on_platform_registered`.
        if tx_history {
            platform_coin.history.enable();
        }
        Ok(platform_coin)
    }

    /// The Solana history needs no SQL store and also runs on the browser build, so it is started in
    /// [`Self::on_platform_registered`] (CRD ch.40 R40.6C.1).
    fn history_supported(&self) -> bool { false }

    fn on_platform_registered(
        ctx: &MmArc,
        ticker: &str,
        activation_request: &Self::ActivationRequest,
        reactivation: bool,
    ) {
        if starts_solana_history(activation_request.tx_history, reactivation) {
            coins::solana::spawn_platform_history(ctx, ticker);
        }
    }

    fn record_token(&self, token: &MmCoinEnum) {
        if let MmCoinEnum::SplToken(token) = token {
            self.register_token_info(token);
        }
    }

    fn forget_token_record(&self, ticker: &str) { self.remove_spl_token_info(ticker); }

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
        // The block height, the SOL balance and the token balances are read from one node (CRD ch.40 R40.1.5).
        let balances = self.activation_balances().await.map_err(read_failure)?;
        let current_block = balances.current_block;
        let solana_balance = balances.balance;
        let token_balances = balances.token_balances;
        let mut result = SolanaWithTokensActivationResult {
            current_block,
            solana_addresses_infos: HashMap::new(),
            spl_addresses_infos: HashMap::new(),
            swap_evidence_single_host: coins::swap_evidence::single_host_relaxed(ctx, self.ticker()),
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
        // The Solana history starts in `on_platform_registered` and `history_supported()` is `false`, so this is not
        // reached. Should that change, no task starts: the handle belongs to nothing and aborting it does nothing.
        let (abort_handle, _abort_registration) = AbortHandle::new_pair();
        abort_handle
    }
}
