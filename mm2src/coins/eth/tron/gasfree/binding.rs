//! The GasFree settings a TRON activation gives (CRD ch.63 §63.3.1, §63.3.2, R1, R9, R13, R20) and the custody
//! address a report shows (§63.3.3, R18, R29).
//!
//! - A TRX platform coin holds a [`GasFreeBinding`]: the parsed `tron_gasless_provider` of its activation request
//!   and the controller confirmations of its addresses. A TRC20 token resolves the binding from its platform coin.
//! - A TRC20 token coin holds a [`TokenGasless`]: its enabled `gasless` block, with the fee cap in the token's
//!   smallest unit.
//!
//! Nothing here sends a provider request: the withdraw rail reads both values.
//!
//! # Invariants
//!
//! - The two credentials are secrets (R9): they reach [`ProviderCredentials`] and no `Debug` rendering, error text
//!   or log line of this module. A configured URL is rendered in its host-only form (CRD ch.35 R35.1.17).
//! - No error of this module repeats a value of the request.
//! - A report never shows a custody address that the controller contract contradicts (R29).

use super::super::api::TronApiClient;
use super::super::{Network, TronAddress};
use super::custody::{check_against_controller, derive_custody, ControllerChecks};
use super::failure::GasFreeError;
use super::networks::gasfree_network;
use super::provider::{GasFreeProvider, ProviderCredentials, DEFAULT_STATUS_POLL_INTERVAL_MS};
use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin};
use crate::node_url::{host_only, SECRET_SET};
use crate::AsyncMutex;
use derive_more::Display;
use ethereum_types::{Address, U256};
use mm2_number::BigDecimal;
use serde::{Deserialize, Deserializer};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

/// The `tron_gasless_provider` object of a platform activation request (CRD ch.63 §63.3.1), as sent.
/// [`GasFreeBinding::from_request`] validates it.
///
/// The type decodes itself member by member, so that no decoding error repeats a part of the value (R9): a value
/// that is not an object, a missing member and a member of the wrong JSON type are each refused with a fixed text
/// that names the member only.
#[derive(Clone)]
pub struct TronGaslessProviderRequest {
    /// The provider's URL: scheme and host only (R13).
    base_url: String,
    /// A secret (R9).
    api_key: String,
    /// A secret (R9).
    api_secret: String,
    /// The base58check TRON address of the fee-paying provider.
    service_provider: String,
    /// The per-call HTTP timeout; 15 000 ms when absent (R20).
    request_timeout_ms: Option<u64>,
    /// The settlement poll interval; 3 000 ms when absent (R20).
    status_poll_interval_ms: Option<u64>,
}

/// Why a value does not decode as one of the two GasFree request objects. The text names a member and never
/// repeats a part of the value.
fn decoding_refusal<E: serde::de::Error>(text: &'static str) -> E { E::custom(text) }

impl<'de> Deserialize<'de> for TronGaslessProviderRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Json::Object(mut object) = Json::deserialize(deserializer)? else {
            return Err(decoding_refusal("tron_gasless_provider must be an object"));
        };
        let mut text = |missing: &'static str, wrong: &'static str, member: &str| match object.remove(member) {
            Some(Json::String(text)) => Ok(text),
            Some(Json::Null) | None => Err(decoding_refusal::<D::Error>(missing)),
            Some(_) => Err(decoding_refusal(wrong)),
        };
        let base_url = text(
            "tron_gasless_provider: missing member base_url",
            "tron_gasless_provider: base_url must be a string",
            "base_url",
        )?;
        let api_key = text(
            "tron_gasless_provider: missing member api_key",
            "tron_gasless_provider: api_key must be a string",
            "api_key",
        )?;
        let api_secret = text(
            "tron_gasless_provider: missing member api_secret",
            "tron_gasless_provider: api_secret must be a string",
            "api_secret",
        )?;
        let service_provider = text(
            "tron_gasless_provider: missing member service_provider",
            "tron_gasless_provider: service_provider must be a string",
            "service_provider",
        )?;
        let mut interval = |wrong: &'static str, member: &str| match object.remove(member) {
            Some(Json::Null) | None => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| decoding_refusal::<D::Error>(wrong)),
        };
        let request_timeout_ms = interval(
            "tron_gasless_provider: request_timeout_ms must be a non-negative integer",
            "request_timeout_ms",
        )?;
        let status_poll_interval_ms = interval(
            "tron_gasless_provider: status_poll_interval_ms must be a non-negative integer",
            "status_poll_interval_ms",
        )?;
        Ok(TronGaslessProviderRequest {
            base_url,
            api_key,
            api_secret,
            service_provider,
            request_timeout_ms,
            status_poll_interval_ms,
        })
    }
}

/// The debug form shows neither credential and names the URL by its host-only form (R9; CRD ch.35 R35.1.17).
impl fmt::Debug for TronGaslessProviderRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TronGaslessProviderRequest")
            .field("base_url", &host_only(&self.base_url))
            .field("api_key", &format_args!("{SECRET_SET}"))
            .field("api_secret", &format_args!("{SECRET_SET}"))
            .field("service_provider", &self.service_provider)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("status_poll_interval_ms", &self.status_poll_interval_ms)
            .finish()
    }
}

/// The `gasless` object of a TRC20 token activation request (CRD ch.63 §63.3.2), as sent.
///
/// Like [`TronGaslessProviderRequest`] it decodes itself, so that no decoding error repeats a part of the value.
#[derive(Clone, Debug)]
pub struct TronTokenGaslessRequest {
    /// Opts the token into the gasless rail. A block with `false` behaves exactly as an absent block.
    enabled: bool,
    /// The cap on the provider fee the user accepts, in token units.
    transfer_max_fee: Option<BigDecimal>,
}

impl<'de> Deserialize<'de> for TronTokenGaslessRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Json::Object(mut object) = Json::deserialize(deserializer)? else {
            return Err(decoding_refusal("gasless must be an object"));
        };
        let enabled = match object.remove("enabled") {
            None => false,
            Some(Json::Bool(enabled)) => enabled,
            Some(_) => return Err(decoding_refusal("gasless: enabled must be a boolean")),
        };
        let transfer_max_fee = match object.remove("transfer_max_fee") {
            Some(Json::Null) | None => None,
            Some(value @ (Json::String(_) | Json::Number(_))) => Some(
                serde_json::from_value::<BigDecimal>(value)
                    .map_err(|_| decoding_refusal::<D::Error>("gasless: transfer_max_fee must be a decimal number"))?,
            ),
            Some(_) => return Err(decoding_refusal("gasless: transfer_max_fee must be a decimal number")),
        };
        Ok(TronTokenGaslessRequest {
            enabled,
            transfer_max_fee,
        })
    }
}

impl TronTokenGaslessRequest {
    /// Whether the block opts the token in. A block that does not is treated as absent everywhere.
    pub fn is_enabled(&self) -> bool { self.enabled }
}

/// Why the GasFree part of an activation request is refused. Every case is an invalid payload of the request; no
/// text repeats a value of the request.
#[derive(Debug, Display, PartialEq, Eq)]
pub enum GaslessConfigError {
    /// `base_url` is more or less than a scheme and a host (R13).
    #[display(fmt = "tron_gasless_provider: base_url is not usable: {}", _0)]
    BaseUrl(&'static str),
    /// `service_provider` is not a base58check TRON address.
    #[display(fmt = "tron_gasless_provider: service_provider is not a well-formed TRON address")]
    ServiceProvider,
    /// The provider client could not be set up for another reason.
    #[display(fmt = "tron_gasless_provider: {}", _0)]
    Provider(String),
    /// A token's `gasless` block is enabled but its platform has no `tron_gasless_provider` (R1).
    #[display(fmt = "gasless: the platform coin was activated without a tron_gasless_provider")]
    NoPlatformProvider,
    /// `transfer_max_fee` is below zero.
    #[display(fmt = "gasless: transfer_max_fee is negative")]
    NegativeTransferMaxFee,
    /// `transfer_max_fee` has more decimal places than the token.
    #[display(
        fmt = "gasless: transfer_max_fee is finer than the {} decimals of the token",
        decimals
    )]
    TransferMaxFeeTooFine { decimals: u8 },
    /// `transfer_max_fee` cannot be expressed in the token's smallest unit.
    #[display(fmt = "gasless: transfer_max_fee is outside the amounts the token can express")]
    TransferMaxFeeOutOfRange,
}

impl std::error::Error for GaslessConfigError {}

/// The GasFree provider binding of a TRX platform coin (CRD ch.63 §63.3.1): the validated `tron_gasless_provider`
/// of its activation request, for the platform's network. The platform coin and its TRC20 tokens share it, and
/// with it the controller confirmations of R29.
pub struct GasFreeBinding {
    network: Network,
    /// The provider client: the host-only base, the credentials and the per-call timeout.
    provider: GasFreeProvider,
    /// The fee-paying provider, the `serviceProvider` of every authorization (§63.6).
    service_provider: TronAddress,
    /// The settlement poll interval, in milliseconds (R20): how long the task route waits between two trace
    /// requests (D63.9).
    status_poll_interval_ms: u64,
    /// The confirmations of this platform's custody addresses by the controller contract (R29).
    checks: ControllerChecks,
    /// One lock per sending address, held while the gasless rail quotes and signs for that address (§63.8 step 3).
    /// The map is never held across an `.await`; the lock it hands out is.
    sender_locks: Mutex<BTreeMap<[u8; 20], Arc<AsyncMutex<()>>>>,
}

/// The provider client's own debug form shows no credential (R9).
impl fmt::Debug for GasFreeBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GasFreeBinding")
            .field("network", &gasfree_network(&self.network).name)
            .field("provider", &self.provider)
            .field("service_provider", &self.service_provider)
            .field("status_poll_interval_ms", &self.status_poll_interval_ms)
            .finish()
    }
}

impl GasFreeBinding {
    /// Validates the `tron_gasless_provider` object of an activation request for a platform on `network` (CRD ch.63
    /// §63.3.1). No node and no provider is contacted.
    ///
    /// # Errors
    ///
    /// [`GaslessConfigError::ServiceProvider`] when `service_provider` is not a base58check TRON address;
    /// [`GaslessConfigError::BaseUrl`] when `base_url` is not an `http` or `https` URL of a scheme and a host alone
    /// (R13).
    pub fn from_request(network: &Network, request: &TronGaslessProviderRequest) -> Result<Self, GaslessConfigError> {
        let service_provider =
            TronAddress::from_base58(&request.service_provider).map_err(|_| GaslessConfigError::ServiceProvider)?;
        let credentials = ProviderCredentials::new(request.api_key.clone(), request.api_secret.clone());
        let provider = GasFreeProvider::new(network, &request.base_url, credentials, request.request_timeout_ms)
            .map_err(|error| match error {
                GasFreeError::InvalidBaseUrl(reason) => GaslessConfigError::BaseUrl(reason),
                other => GaslessConfigError::Provider(other.to_string()),
            })?;
        Ok(GasFreeBinding {
            network: network.clone(),
            provider,
            service_provider,
            // An absent setting means the documented default (R20).
            status_poll_interval_ms: request
                .status_poll_interval_ms
                .unwrap_or(DEFAULT_STATUS_POLL_INTERVAL_MS),
            checks: ControllerChecks::new(),
            sender_locks: Mutex::new(BTreeMap::new()),
        })
    }

    /// The platform's network.
    pub(crate) fn network(&self) -> &Network { &self.network }

    /// The provider client.
    pub(crate) fn provider(&self) -> &GasFreeProvider { &self.provider }

    /// The fee-paying provider, the `serviceProvider` of every authorization (§63.6).
    pub(crate) fn service_provider(&self) -> &TronAddress { &self.service_provider }

    /// The settlement poll interval, in milliseconds (R20).
    pub(crate) fn status_poll_interval_ms(&self) -> u64 { self.status_poll_interval_ms }

    /// The controller confirmations of this platform (R29): the ones the reports use.
    pub(crate) fn controller_checks(&self) -> &ControllerChecks { &self.checks }

    /// The lock of the sending address `sender` (§63.8 step 3). While its guard is held, no other gasless withdraw
    /// of this platform quotes or signs for the same address, so two authorizations are never signed over one
    /// reading of the provider's nonce by this node at the same time. The task route holds it until the provider
    /// answered the submission (D63.9 item 1).
    pub(crate) fn sender_lock(&self, sender: &TronAddress) -> Arc<AsyncMutex<()>> {
        self.sender_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(sender.to_evm_address().0)
            .or_default()
            .clone()
    }

    /// The custody address a report shows for `user` (CRD ch.63 §63.3.3, R18), under the report rule of R29:
    ///
    /// - the controller contract confirms the derived address, now or within the last 10 minutes: that address;
    /// - the controller reports another address: `None`, so that the report omits the field; the check logs one
    ///   warning line;
    /// - the controller cannot be read: the locally derived address.
    pub(crate) async fn report_address(&self, api: &TronApiClient, user: &TronAddress) -> Option<TronAddress> {
        match check_against_controller(api, &self.checks, &self.network, user).await {
            Ok(custody) => Some(custody),
            Err(GasFreeError::DerivationMismatch { .. }) => None,
            // A failed read confirms nothing and contradicts nothing.
            Err(_) => Some(derive_custody(&self.network, user).address),
        }
    }
}

/// The GasFree custody address of `user` on `network`, derived locally (CRD ch.63 §63.5). It is a pure function of
/// its two arguments.
pub fn gasfree_custody_address(network: &Network, user: &TronAddress) -> TronAddress {
    derive_custody(network, user).address
}

/// The enabled `gasless` block of a TRC20 token coin (CRD ch.63 §63.3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenGasless {
    /// The activation-time cap on the provider fee, in the token's smallest unit; `None` when the request gave none.
    pub(crate) transfer_max_fee: Option<U256>,
}

impl TokenGasless {
    /// The checks of an enabled `gasless` block that need no token decimals (CRD ch.63 §63.3.2, R1): the platform
    /// has a provider binding, and the cap is not negative.
    ///
    /// # Errors
    ///
    /// [`GaslessConfigError::NoPlatformProvider`] or [`GaslessConfigError::NegativeTransferMaxFee`].
    pub(crate) fn precheck(
        request: &TronTokenGaslessRequest,
        platform_has_provider: bool,
    ) -> Result<(), GaslessConfigError> {
        if !platform_has_provider {
            return Err(GaslessConfigError::NoPlatformProvider);
        }
        match &request.transfer_max_fee {
            Some(fee) if *fee < BigDecimal::from(0) => Err(GaslessConfigError::NegativeTransferMaxFee),
            _ => Ok(()),
        }
    }

    /// The enabled block `request` of a token with `decimals`, its cap converted to the token's smallest unit.
    ///
    /// # Errors
    ///
    /// [`GaslessConfigError::NegativeTransferMaxFee`], [`GaslessConfigError::TransferMaxFeeTooFine`] when the cap
    /// has more decimal places than the token, [`GaslessConfigError::TransferMaxFeeOutOfRange`] when it does not
    /// fit the token's amount type.
    pub(crate) fn from_request(request: &TronTokenGaslessRequest, decimals: u8) -> Result<Self, GaslessConfigError> {
        let transfer_max_fee = match &request.transfer_max_fee {
            Some(fee) => Some(transfer_max_fee_units(fee, decimals)?),
            None => None,
        };
        Ok(TokenGasless { transfer_max_fee })
    }
}

/// `fee`, an amount in token units, in the smallest unit of a token with `decimals`.
fn transfer_max_fee_units(fee: &BigDecimal, decimals: u8) -> Result<U256, GaslessConfigError> {
    if *fee < BigDecimal::from(0) {
        return Err(GaslessConfigError::NegativeTransferMaxFee);
    }
    // The conversion cuts the digits beyond `decimals`; a value that does not read back as it was given had some.
    let units = wei_from_big_decimal(fee, decimals).map_err(|_| GaslessConfigError::TransferMaxFeeOutOfRange)?;
    let read_back = u256_to_big_decimal(units, decimals).map_err(|_| GaslessConfigError::TransferMaxFeeOutOfRange)?;
    if read_back != *fee {
        return Err(GaslessConfigError::TransferMaxFeeTooFine { decimals });
    }
    Ok(units)
}

/// What a TRON coin holds of GasFree: the provider binding on a TRX platform coin, the enabled `gasless` block on a
/// TRC20 token coin. A coin holds neither when its activation gave none.
#[derive(Debug)]
pub(crate) enum CoinGasFree {
    Platform(GasFreeBinding),
    Token(TokenGasless),
}

impl EthCoin {
    /// The GasFree provider binding this coin itself holds: `Some` for a TRX platform coin activated with a
    /// `tron_gasless_provider`.
    pub(crate) fn gasfree_platform_binding(&self) -> Option<&GasFreeBinding> {
        match &self.tron_gasfree {
            Some(CoinGasFree::Platform(binding)) => Some(binding),
            Some(CoinGasFree::Token(_)) | None => None,
        }
    }

    /// The GasFree custody address a report shows next to `address` of this coin, in base58check (CRD ch.63
    /// §63.3.3, R18). This is the one place where the field of every report of the EVM family gets its value.
    ///
    /// `None`, so that the report omits the field, for a coin that is not a TRON coin, for a TRON coin whose
    /// platform has no provider binding, for a TRC20 token whose platform coin is not active, and when the
    /// controller contract contradicts the derived address ([`GasFreeBinding::report_address`], R29).
    pub async fn gasfree_report_address(&self, address: &Address) -> Option<String> {
        if !self.is_tron_family() {
            return None;
        }
        // A token resolves the binding from its platform coin (§63.8 step 2).
        let holder = self.hd_serving_coin().await.ok()?;
        let binding = holder.gasfree_platform_binding()?;
        let api = holder.tron_api.as_ref()?;
        let user = TronAddress::from_evm_address(*address);
        let custody = binding.report_address(api, &user).await?;
        Some(custody.to_base58())
    }

    /// [`Self::gasfree_report_address`] of the address the coin is enabled on, for the single-address reports.
    pub async fn enabled_address_gasfree(&self) -> Option<String> {
        self.gasfree_report_address(&self.my_address).await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))] mod tests;
