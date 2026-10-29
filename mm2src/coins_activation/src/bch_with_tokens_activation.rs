use crate::erc20_token_activation::{configured_protocol, ProtocolResolveError};
use crate::platform_coin_with_tokens::*;
use crate::prelude::*;
use async_trait::async_trait;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::utxo::bch::{bch_coin_from_conf_and_params, BchActivationRequest, BchCoin, CashAddrPrefix};
use coins::utxo::bch_and_slp_tx_history::bch_and_slp_history_loop;
use coins::utxo::rpc_clients::UtxoRpcError;
use coins::utxo::slp::{SlpProtocolConf, SlpToken};
use coins::utxo::UtxoCommonOps;
use coins::{coin_conf, CoinBalance, CoinProtocol, MarketCoinOps, MmCoin, MmCoinEnum, PrivKeyActivationPolicy,
            PrivKeyNotAllowed, UnexpectedDerivationMethod};
use common::executor::spawn;
use common::log::info;
use common::mm_number::BigDecimal;
use common::Future01CompatExt;
use futures::future::{abortable, AbortHandle};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::str::FromStr;

pub struct SlpTokenInitializer {
    platform_coin: BchCoin,
}

impl TokenOf for SlpToken {
    type PlatformCoin = BchCoin;
}

/// One entry of `slp_tokens_requests` (CRD ch.55 R55.9.2).
#[derive(Clone, Debug, Deserialize)]
pub struct BchSlpTokenRequest {
    /// Takes precedence over the confirmations of the token's protocol configuration, which take precedence
    /// over the platform coin's.
    pub required_confirmations: Option<u64>,
    /// The protocol of a custom token that is absent from the coins configuration (CRD ch.55 D55.3).
    #[serde(default)]
    pub protocol: Option<CoinProtocol>,
}

/// Resolves the protocol of a requested SLP token: the inline `protocol` of a custom token, else the coins
/// configuration (CRD ch.55 R55.9.2).
///
/// A custom token must not be in the coins configuration (`CustomTokenError::DuplicateTickerInConfig`) and
/// must carry the SLP token protocol. Only that much of the custom-token rules applies to SLP so far;
/// ch.55 D55.3 defers the rest.
fn resolve_slp_protocol(
    ctx: &MmArc,
    ticker: &str,
    inline_protocol: Option<&CoinProtocol>,
) -> Result<SlpProtocolConf, ProtocolResolveError> {
    let protocol = match inline_protocol {
        Some(protocol) => {
            if !coin_conf(ctx, ticker).is_null() {
                return Err(ProtocolResolveError::Custom(
                    CustomTokenError::DuplicateTickerInConfig {
                        ticker_in_config: ticker.to_owned(),
                    },
                ));
            }
            protocol.clone()
        },
        None => configured_protocol(ctx, ticker)?,
    };
    match SlpProtocolConf::try_from_coin_protocol(protocol) {
        Ok(conf) => Ok(conf),
        Err(other) => Err(ProtocolResolveError::UnexpectedProtocol {
            ticker: ticker.to_owned(),
            protocol: other.into_inner(),
        }),
    }
}

/// Whether every token's protocol names the platform being activated.
fn all_on_platform(protocols: &[SlpProtocolConf], platform_ticker: &str) -> bool {
    protocols
        .iter()
        .all(|protocol| protocol.platform_coin_ticker == platform_ticker)
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TokenAsMmCoinInitializer for SlpTokenInitializer {
    type PlatformCoin = BchCoin;
    type ActivationRequest = BchWithTokensActivationRequest;

    /// Enables the requested tokens in passes, each covering every token in request order before the next
    /// starts, so the error reported is that of the earliest failing pass: the resolution of every token's
    /// protocol, then the platform consistency of every token (CRD ch.55 R55.9.4 `PlatformCoinMismatch`).
    async fn enable_tokens_as_mm_coins(
        &self,
        ctx: MmArc,
        request: &Self::ActivationRequest,
    ) -> Result<Vec<MmCoinEnum>, MmError<InitTokensAsMmCoinsError>> {
        let requests = &request.slp_tokens_requests;

        let mut protocols = Vec::with_capacity(requests.len());
        for token_request in requests {
            let protocol = resolve_slp_protocol(&ctx, &token_request.ticker, token_request.request.protocol.as_ref())
                .map_to_mm(InitTokensAsMmCoinsError::from)?;
            protocols.push(protocol);
        }

        if !all_on_platform(&protocols, self.platform_coin.ticker()) {
            return MmError::err(InitTokensAsMmCoinsError::PlatformCoinMismatch);
        }

        let tokens: Vec<SlpToken> = requests
            .iter()
            .zip(protocols)
            .map(|(token_request, protocol)| {
                // confirmation settings from the RPC request have the highest priority
                let required_confirmations = token_request.request.required_confirmations.unwrap_or_else(|| {
                    protocol
                        .required_confirmations
                        .unwrap_or_else(|| self.platform_coin.required_confirmations())
                });
                SlpToken::new(
                    protocol.decimals,
                    token_request.ticker.clone(),
                    protocol.token_id,
                    self.platform_coin.clone(),
                    required_confirmations,
                )
            })
            .collect();

        for token in tokens.iter() {
            self.platform_coin.register_token_info(token);
        }
        Ok(tokens.into_iter().map(Into::into).collect())
    }
}

impl RegisterTokenInfo<SlpToken> for BchCoin {
    fn register_token_info(&self, token: &SlpToken) { self.add_slp_token_info(token.ticker().into(), token.get_info()) }
}

impl From<BchWithTokensActivationError> for EnablePlatformCoinWithTokensError {
    fn from(err: BchWithTokensActivationError) -> Self {
        match err {
            BchWithTokensActivationError::PlatformCoinCreationError { ticker, error } => {
                EnablePlatformCoinWithTokensError::PlatformCoinCreationError { ticker, error }
            },
            BchWithTokensActivationError::InvalidSlpPrefix { ticker, prefix, error } => {
                EnablePlatformCoinWithTokensError::Internal(format!(
                    "Invalid slp prefix {} configured for {}. Error: {}",
                    prefix, ticker, error
                ))
            },
            BchWithTokensActivationError::PrivKeyNotAllowed(e) => {
                EnablePlatformCoinWithTokensError::PrivKeyPolicyNotAllowed(e)
            },
            BchWithTokensActivationError::UnexpectedDerivationMethod(e) => {
                EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod(e)
            },
            BchWithTokensActivationError::UnexpectedDeviceActivationPolicy => {
                EnablePlatformCoinWithTokensError::UnexpectedDeviceActivationPolicy
            },
            BchWithTokensActivationError::Transport(e) => EnablePlatformCoinWithTokensError::Transport(e),
            BchWithTokensActivationError::Internal(e) => EnablePlatformCoinWithTokensError::Internal(e),
        }
    }
}

// Docs vs reference (CRD ch.55 R55.9.2, public-docs erratum): the public page marks `utxo_merge_params` as
// required and gives `requires_notarization` the default `true`, and it lists `tx_history` twice. The
// reference reads `utxo_merge_params` as optional, defaults `requires_notarization` from the coin's
// configuration, and defaults `tx_history` to `false`. Reloaded follows the reference on all three (the
// maintainer confirmed it on 2026-10-11), so a client written from the public page keeps working.
#[derive(Clone, Debug, Deserialize)]
pub struct BchWithTokensActivationRequest {
    #[serde(flatten)]
    platform_request: BchActivationRequest,
    slp_tokens_requests: Vec<TokenActivationRequest<BchSlpTokenRequest>>,
    /// Whether the balances are returned. Defaults to `true`; `false` reports the activated SLP tickers
    /// instead of the balances (CRD ch.55 R55.9.2, R55.9.3).
    #[serde(default)]
    get_balances: Option<bool>,
}

impl BchWithTokensActivationRequest {
    fn get_balances(&self) -> bool { self.get_balances.unwrap_or(true) }
}

impl TxHistory for BchWithTokensActivationRequest {
    fn tx_history(&self) -> bool { self.platform_request.utxo_params.tx_history }
}

pub struct BchProtocolInfo {
    slp_prefix: String,
}

impl BchProtocolInfo {
    pub(crate) fn slp_prefix(&self) -> &str { &self.slp_prefix }
}

impl TryFromCoinProtocol for BchProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::BCH { slp_prefix } => Ok(BchProtocolInfo { slp_prefix }),
            protocol => MmError::err(protocol),
        }
    }
}

/// The address entry of the result (CRD ch.55 R55.9.3). A key that is not present is omitted, not `null`.
#[derive(Debug, Serialize)]
pub struct BchAddressInfo<Balance> {
    derivation_method: DerivationMethod,
    pubkey: String,
    /// Present when `get_balances` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    balances: Option<Balance>,
    /// The activated SLP tickers, in the SLP entry when `get_balances` is `false` (order not bound).
    #[serde(skip_serializing_if = "Option::is_none")]
    tickers: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct BchWithTokensActivationResult {
    current_block: u64,
    bch_addresses_infos: HashMap<String, BchAddressInfo<CoinBalance>>,
    slp_addresses_infos: HashMap<String, BchAddressInfo<TokenBalances>>,
}

impl GetPlatformBalance for BchWithTokensActivationResult {
    /// The balance the transaction-history loop starts from. A result that omits the balances
    /// (`get_balances: false`) reports none; the loop only compares it with the balance it reads itself.
    fn get_platform_balance(&self) -> BigDecimal {
        self.bch_addresses_infos
            .iter()
            .fold(BigDecimal::from(0), |total, (_, addr_info)| match &addr_info.balances {
                Some(balances) => &total + &balances.get_total(),
                None => total,
            })
    }
}

impl CurrentBlock for BchWithTokensActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

#[derive(Debug)]
pub enum BchWithTokensActivationError {
    PlatformCoinCreationError {
        ticker: String,
        error: String,
    },
    InvalidSlpPrefix {
        ticker: String,
        prefix: String,
        error: String,
    },
    PrivKeyNotAllowed(String),
    UnexpectedDerivationMethod(String),
    /// The request selects a hardware wallet, which only the task methods accept.
    UnexpectedDeviceActivationPolicy,
    Transport(String),
    Internal(String),
}

impl From<UtxoRpcError> for BchWithTokensActivationError {
    fn from(err: UtxoRpcError) -> Self { BchWithTokensActivationError::Transport(err.to_string()) }
}

impl From<UnexpectedDerivationMethod> for BchWithTokensActivationError {
    fn from(e: UnexpectedDerivationMethod) -> Self {
        BchWithTokensActivationError::UnexpectedDerivationMethod(e.to_string())
    }
}

impl From<PrivKeyNotAllowed> for BchWithTokensActivationError {
    fn from(e: PrivKeyNotAllowed) -> Self { BchWithTokensActivationError::PrivKeyNotAllowed(e.to_string()) }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PlatformWithTokensActivationOps for BchCoin {
    type ActivationRequest = BchWithTokensActivationRequest;
    type PlatformProtocolInfo = BchProtocolInfo;
    type ActivationResult = BchWithTokensActivationResult;
    type ActivationError = BchWithTokensActivationError;

    /// `priv_key_policy` is read only to reject a hardware wallet: the platform is always built with the
    /// wallet context's key policy, and only the task methods accept a device (CRD ch.55 R55.9.2, R55.9.4).
    fn check_one_shot_request(request: &Self::ActivationRequest) -> Result<(), MmError<Self::ActivationError>> {
        if matches!(
            request.platform_request.utxo_params.priv_key_policy,
            PrivKeyActivationPolicy::Trezor
        ) {
            return MmError::err(BchWithTokensActivationError::UnexpectedDeviceActivationPolicy);
        }
        Ok(())
    }

    // Docs vs reference (CRD ch.55 R55.3.2, R55.9.4): the public docs show an Electrum set that does not
    // respond as `PlatformCoinCreationError` `{ticker, error}`. The reference release no longer connects
    // while building a UTXO-family coin, so it reports that case later as `Transport` (502) at the first
    // height query. Per the precedence rule the public docs govern this row, so Reloaded reports a build
    // failure, including the Electrum connection, as `PlatformCoinCreationError`.
    async fn enable_platform_coin(
        ctx: MmArc,
        ticker: String,
        platform_conf: Json,
        activation_request: Self::ActivationRequest,
        protocol_conf: Self::PlatformProtocolInfo,
        priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        let slp_prefix = CashAddrPrefix::from_str(&protocol_conf.slp_prefix).map_to_mm(|error| {
            BchWithTokensActivationError::InvalidSlpPrefix {
                ticker: ticker.clone(),
                prefix: protocol_conf.slp_prefix,
                error,
            }
        })?;

        let platform_coin = bch_coin_from_conf_and_params(
            &ctx,
            &ticker,
            &platform_conf,
            activation_request.platform_request,
            slp_prefix,
            priv_key,
        )
        .await
        .map_to_mm(|error| BchWithTokensActivationError::PlatformCoinCreationError { ticker, error })?;
        Ok(platform_coin)
    }

    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String> {
        activation_request
            .slp_tokens_requests
            .iter()
            .map(|token| token.ticker.clone())
            .collect()
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        vec![Box::new(SlpTokenInitializer {
            platform_coin: self.clone(),
        })]
    }

    async fn get_activation_result<XPubExtractor>(
        &self,
        _ctx: &MmArc,
        activation_request: &Self::ActivationRequest,
        _xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<BchWithTokensActivationResult, MmError<BchWithTokensActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        let my_address = self.as_ref().derivation_method.iguana_or_err().mm_err(Into::into)?;
        let my_slp_address = self
            .get_my_slp_address()
            .map_to_mm(BchWithTokensActivationError::Internal)?
            .encode()
            .map_to_mm(BchWithTokensActivationError::Internal)?;

        let current_block = self
            .as_ref()
            .rpc_client
            .get_block_count()
            .compat()
            .await
            .mm_err(Into::into)?;

        let pubkey = self.my_public_key().mm_err(Into::into)?.to_string();
        let (bch_balance, token_balances, tickers) = if activation_request.get_balances() {
            let bch_unspents = self.bch_unspents_for_display(my_address).await.mm_err(Into::into)?;
            let bch_balance = bch_unspents.platform_balance(self.decimals());

            let mut token_balances = HashMap::new();
            for (token_ticker, info) in self.get_slp_tokens_infos().iter() {
                let token_balance = bch_unspents.slp_token_balance(&info.token_id, info.decimals);
                token_balances.insert(token_ticker.clone(), token_balance);
            }
            (Some(bch_balance), Some(token_balances), None)
        } else {
            // Without balances the SLP entry lists the activated tickers instead (R55.9.3).
            let mut tickers: Vec<String> = self.get_slp_tokens_infos().keys().cloned().collect();
            tickers.sort();
            (None, None, Some(tickers))
        };

        let mut result = BchWithTokensActivationResult {
            current_block,
            bch_addresses_infos: HashMap::new(),
            slp_addresses_infos: HashMap::new(),
        };

        result
            .bch_addresses_infos
            .insert(my_address.to_string(), BchAddressInfo {
                derivation_method: DerivationMethod::Iguana,
                pubkey: pubkey.clone(),
                balances: bch_balance,
                tickers: None,
            });

        result.slp_addresses_infos.insert(my_slp_address, BchAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey,
            balances: token_balances,
            tickers,
        });
        Ok(result)
    }

    fn start_history_background_fetching(
        &self,
        ctx: MmArc,
        metrics: MetricsArc,
        storage: impl TxHistoryStorage + 'static,
        initial_balance: BigDecimal,
    ) -> AbortHandle {
        let ticker = self.ticker().to_owned();
        let (fut, abort_handle) = abortable(bch_and_slp_history_loop(
            self.clone(),
            ctx,
            storage,
            metrics,
            initial_balance,
        ));
        spawn(async move {
            if let Err(e) = fut.await {
                info!("bch_and_slp_history_loop stopped for {}, reason {}", ticker, e);
            }
        });
        abort_handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{HttpStatusCode, StatusCode};
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    const TOKEN_ID: &str = "4de69e374a8ed21cbddd47f2338cc0f479dc58daa2bbe11cd604ca488eca0ddf";

    fn slp_protocol_json(platform: &str) -> Json {
        json!({
            "type": "SLPTOKEN",
            "protocol_data": { "platform": platform, "token_id": TOKEN_ID, "decimals": 4 }
        })
    }

    fn resolved(result: Result<SlpProtocolConf, ProtocolResolveError>, what: &str) -> SlpProtocolConf {
        match result {
            Ok(conf) => conf,
            Err(_) => panic!("{what} did not resolve"),
        }
    }

    fn ctx() -> MmArc {
        MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [
                    { "coin": "ASLP", "protocol": slp_protocol_json("BCH") },
                    { "coin": "OTHER", "protocol": slp_protocol_json("XYZ") },
                    { "coin": "NOTSLP", "protocol": { "type": "UTXO" } },
                ]
            }))
            .into_mm_arc()
    }

    fn request_json(extra: Json) -> Json {
        let mut request = json!({
            "ticker": "BCH",
            "mode": { "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "bch.example:50002" }] } },
            "bchd_urls": ["https://bchd.example:8335"],
            "slp_tokens_requests": [{ "ticker": "ASLP" }],
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        request
    }

    /// CRD ch.55 R55.1.12: the activation locks the tickers of the tokens the request registers beside the platform.
    #[test]
    fn the_requested_token_tickers_are_the_slp_tickers_of_the_request() {
        let request: BchWithTokensActivationRequest = serde_json::from_value(request_json(json!({
            "slp_tokens_requests": [{ "ticker": "ASLP" }, { "ticker": "BSLP" }],
        })))
        .unwrap();
        assert_eq!(
            <BchCoin as PlatformWithTokensActivationOps>::requested_token_tickers(&request),
            ["ASLP", "BSLP"]
        );
    }

    #[test]
    fn get_balances_defaults_to_true() {
        // R55.9.2
        let request: BchWithTokensActivationRequest = serde_json::from_value(request_json(json!({}))).unwrap();
        assert!(request.get_balances());
        let request: BchWithTokensActivationRequest =
            serde_json::from_value(request_json(json!({ "get_balances": false }))).unwrap();
        assert!(!request.get_balances());
    }

    #[test]
    fn token_request_accepts_confirmations_and_a_custom_protocol() {
        // R55.9.2, D55.3: `protocol` is accepted on a token request; unknown keys are ignored (R55.1.8).
        let request: BchWithTokensActivationRequest = serde_json::from_value(request_json(json!({
            "slp_tokens_requests": [
                { "ticker": "ASLP", "required_confirmations": 3, "unknown": 1 },
                { "ticker": "CUSTOM", "protocol": slp_protocol_json("BCH") },
            ],
            "something_else": true,
        })))
        .unwrap();
        assert_eq!(request.slp_tokens_requests.len(), 2);
        assert_eq!(request.slp_tokens_requests[0].request.required_confirmations, Some(3));
        assert!(request.slp_tokens_requests[0].request.protocol.is_none());
        assert!(matches!(
            request.slp_tokens_requests[1].request.protocol,
            Some(CoinProtocol::SLPTOKEN { .. })
        ));
    }

    fn address_info<B>(balances: Option<B>, tickers: Option<Vec<String>>) -> BchAddressInfo<B> {
        BchAddressInfo {
            derivation_method: DerivationMethod::Iguana,
            pubkey: "02ab".to_owned(),
            balances,
            tickers,
        }
    }

    #[test]
    fn result_with_balances_has_no_tickers() {
        // R55.9.3, `get_balances: true`
        let mut bch = HashMap::new();
        bch.insert(
            "bitcoincash:q1".to_owned(),
            address_info(Some(CoinBalance::default()), None),
        );
        let mut slp = HashMap::new();
        let mut token_balances = TokenBalances::new();
        token_balances.insert("ASLP".to_owned(), CoinBalance::default());
        slp.insert("simpleledger:q1".to_owned(), address_info(Some(token_balances), None));
        let result = BchWithTokensActivationResult {
            current_block: 7,
            bch_addresses_infos: bch,
            slp_addresses_infos: slp,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "current_block": 7,
                "bch_addresses_infos": { "bitcoincash:q1": {
                    "derivation_method": { "type": "Iguana" },
                    "pubkey": "02ab",
                    "balances": { "spendable": "0", "unspendable": "0" }
                } },
                "slp_addresses_infos": { "simpleledger:q1": {
                    "derivation_method": { "type": "Iguana" },
                    "pubkey": "02ab",
                    "balances": { "ASLP": { "spendable": "0", "unspendable": "0" } }
                } },
            })
        );
    }

    #[test]
    fn result_without_balances_lists_the_slp_tickers_and_omits_the_missing_keys() {
        // R55.9.3, `get_balances: false`: the BCH entry carries neither `balances` nor `tickers`, the SLP
        // entry carries `tickers` and no `balances`; absent keys are omitted, never `null`.
        let mut bch = HashMap::new();
        bch.insert("bitcoincash:q1".to_owned(), address_info::<CoinBalance>(None, None));
        let mut slp = HashMap::new();
        slp.insert(
            "simpleledger:q1".to_owned(),
            address_info::<TokenBalances>(None, Some(vec!["ASLP".to_owned(), "BSLP".to_owned()])),
        );
        let result = BchWithTokensActivationResult {
            current_block: 7,
            bch_addresses_infos: bch,
            slp_addresses_infos: slp,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "current_block": 7,
                "bch_addresses_infos": { "bitcoincash:q1": {
                    "derivation_method": { "type": "Iguana" },
                    "pubkey": "02ab"
                } },
                "slp_addresses_infos": { "simpleledger:q1": {
                    "derivation_method": { "type": "Iguana" },
                    "pubkey": "02ab",
                    "tickers": ["ASLP", "BSLP"]
                } },
            })
        );
        assert_eq!(result.get_platform_balance(), BigDecimal::from(0));
    }

    #[test]
    fn a_hardware_priv_key_policy_is_rejected_with_unexpected_device_activation_policy() {
        // R55.9.4
        let trezor: BchWithTokensActivationRequest =
            serde_json::from_value(request_json(json!({ "priv_key_policy": "Trezor" }))).unwrap();
        let err = <BchCoin as PlatformWithTokensActivationOps>::check_one_shot_request(&trezor)
            .expect_err("a hardware policy must be rejected")
            .into_inner();
        let err = EnablePlatformCoinWithTokensError::from(err);
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({ "error_type": "UnexpectedDeviceActivationPolicy" })
        );
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);

        for policy in [json!({}), json!({ "priv_key_policy": "ContextPrivKey" })] {
            let request: BchWithTokensActivationRequest = serde_json::from_value(request_json(policy)).unwrap();
            assert!(<BchCoin as PlatformWithTokensActivationOps>::check_one_shot_request(&request).is_ok());
        }
    }

    #[test]
    fn a_configured_token_resolves_from_the_coins_config() {
        let ctx = ctx();
        let conf = resolved(resolve_slp_protocol(&ctx, "ASLP", None), "configured SLP token");
        assert_eq!(conf.platform_coin_ticker, "BCH");
        assert_eq!(conf.decimals, 4);

        assert!(matches!(
            resolve_slp_protocol(&ctx, "MISSING", None),
            Err(ProtocolResolveError::ConfigIsNotFound(ticker)) if ticker == "MISSING"
        ));
        assert!(matches!(
            resolve_slp_protocol(&ctx, "NOTSLP", None),
            Err(ProtocolResolveError::UnexpectedProtocol { ticker, .. }) if ticker == "NOTSLP"
        ));
    }

    #[test]
    fn a_custom_token_must_be_absent_from_the_coins_config_and_carry_an_slp_protocol() {
        let ctx = ctx();
        let custom = serde_json::from_value::<CoinProtocol>(slp_protocol_json("BCH")).unwrap();
        // Absent from the config: the inline protocol is used.
        let conf = resolved(resolve_slp_protocol(&ctx, "CUSTOM", Some(&custom)), "custom token");
        assert_eq!(conf.platform_coin_ticker, "BCH");
        // Present in the config: rejected.
        assert!(matches!(
            resolve_slp_protocol(&ctx, "ASLP", Some(&custom)),
            Err(ProtocolResolveError::Custom(CustomTokenError::DuplicateTickerInConfig { ticker_in_config }))
                if ticker_in_config == "ASLP"
        ));
        // Not an SLP protocol.
        let utxo = serde_json::from_value::<CoinProtocol>(json!({ "type": "UTXO" })).unwrap();
        assert!(matches!(
            resolve_slp_protocol(&ctx, "CUSTOM", Some(&utxo)),
            Err(ProtocolResolveError::UnexpectedProtocol { .. })
        ));
    }

    #[test]
    fn a_token_of_another_platform_is_a_platform_coin_mismatch() {
        // R55.9.4: the token/platform consistency check.
        let ctx = ctx();
        let on_bch = resolved(resolve_slp_protocol(&ctx, "ASLP", None), "a resolvable token");
        let on_xyz = resolved(resolve_slp_protocol(&ctx, "OTHER", None), "a resolvable token");
        assert!(all_on_platform(&[], "BCH"));
        assert!(!all_on_platform(&[on_xyz], "BCH"));
        let on_bch_again = resolved(resolve_slp_protocol(&ctx, "ASLP", None), "a resolvable token");
        assert!(all_on_platform(&[on_bch, on_bch_again], "BCH"));

        let wire = EnablePlatformCoinWithTokensError::from(InitTokensAsMmCoinsError::PlatformCoinMismatch);
        assert_eq!(
            serde_json::to_value(&wire).unwrap(),
            json!({ "error_type": "PlatformCoinMismatch" })
        );
        assert_eq!(wire.status_code(), StatusCode::BAD_REQUEST);
    }
}
