//! Gasless withdraw: rail selection, the sign-only orchestration, and the
//! public request/response surface (§49.3 / §49.8).
//!
//! **Sign-only today** (§49.8 step 5): the rail preflights, computes the fee
//! and the effective cap, signs the `PermitTransfer`, and returns the signed
//! authorization as a typed off-chain payload plus the gasless fee details. It
//! does **not** POST the authorization to the provider nor poll for settlement
//! (D-submit).

use super::client::{GasFreeRestClient, SubmitFields, SubmitRequest};
use super::config::{GaslessTokenConfig, TronGaslessProviderConfig, DEFAULT_DEADLINE_SECONDS};
use super::derive::derive_gasfree_address;
use super::error::{GasFreeProviderError, GasFreeWithdrawError};
use super::permit::{sign_permit, PermitDomain, PermitTransfer};
use super::preflight::{preflight, resolve_fee_cap, DisabledReason, PreflightInputs, PreflightOutcome};
use crate::eth::tron::address::TronAddress;
use crate::eth::tron::api::TronApiClient;
use crate::eth::tron::Network;
use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType};
use bigdecimal::BigDecimal;
use ethereum_types::U256;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public request surface (§49.3.4)
// ---------------------------------------------------------------------------

/// The withdraw fee-rail selector (§49.3.4).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FeeMethod {
    /// Default behaviour; unchanged native Tron resource-fee rail.
    #[default]
    Native,
    /// Require the gasless rail.
    Gasless,
    /// Try gasless, fall back to native when appropriate.
    Auto,
}

/// Per-request gasless constraints (§49.3.4). Meaningful only with
/// `fee_method = gasless | auto`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaslessWithdrawOptions {
    /// Per-request cap (token units) on the accepted provider fee.
    #[serde(default)]
    pub max_fee: Option<BigDecimal>,
    /// Authorization validity window (seconds) from signing time; must be > 0.
    #[serde(default)]
    pub deadline_seconds: Option<u64>,
    /// With `fee_method = gasless`, permit silent fallback to native when the
    /// rail is deterministically unavailable.
    #[serde(default)]
    pub fallback_to_native: bool,
}

// ---------------------------------------------------------------------------
// Rail selection (§49.8 step 1)
// ---------------------------------------------------------------------------

/// The resolved rail for a withdraw (§49.8 step 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailDecision {
    /// Use the native Tron resource-fee rail.
    Native,
    /// Use the gasless rail; `allow_fallback` permits a deterministic
    /// unavailability to fall back to native instead of erroring.
    Gasless { allow_fallback: bool },
}

/// Select the rail from the request (§49.8 step 1).
///
/// Gasless options supplied with the native rail are rejected; a max-amount
/// request is rejected on the gasless rail and never routed through `auto`
/// (R14).
pub fn select_rail(
    fee_method: Option<FeeMethod>,
    gasless_opts: Option<&GaslessWithdrawOptions>,
    is_max: bool,
) -> Result<RailDecision, GasFreeWithdrawError> {
    let fm = fee_method.unwrap_or_default();

    if gasless_opts.is_some() && fm == FeeMethod::Native {
        return Err(GasFreeWithdrawError::Config(
            "gasless options require fee_method 'gasless' or 'auto'".to_owned(),
        ));
    }

    match fm {
        FeeMethod::Native => Ok(RailDecision::Native),
        FeeMethod::Gasless => {
            if is_max {
                return Err(GasFreeWithdrawError::Config(
                    "max-amount withdraw is unsupported on the gasless rail".to_owned(),
                ));
            }
            let allow_fallback = gasless_opts.map(|o| o.fallback_to_native).unwrap_or(false);
            Ok(RailDecision::Gasless { allow_fallback })
        },
        // auto never routes a max-amount request through the rail (R14).
        FeeMethod::Auto if is_max => Ok(RailDecision::Native),
        FeeMethod::Auto => Ok(RailDecision::Gasless { allow_fallback: true }),
    }
}

// ---------------------------------------------------------------------------
// Public response surface (§49.3.5)
// ---------------------------------------------------------------------------

/// Gasless fee details (§49.3.5). All amounts are token units; the fee is paid
/// in the token, not in TRX.
#[derive(Clone, Debug, Serialize)]
pub struct GaslessFeeDetails {
    pub coin: String,
    pub fee_method: &'static str,
    pub provider_name: &'static str,
    pub gasfree_address: String,
    pub transfer_fee: BigDecimal,
    pub activation_fee: BigDecimal,
    pub total_token_fee: BigDecimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_max_fee: Option<BigDecimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// The signed off-chain authorization payload (§49.8 step 5). The `type`
/// discriminator and envelope shape are discretionary; the signed fields mirror
/// the provider submit payload (§49.4.6). The raw `signature` is the only
/// sensitive field and is surfaced (it is the authorization the caller holds),
/// but it is redacted in `Debug` via [`super::permit::PermitSignature`].
#[derive(Clone, Debug, Serialize)]
pub struct GaslessSignedAuthorization {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub gasfree_address: String,
    pub token: String,
    pub service_provider: String,
    pub user: String,
    pub receiver: String,
    pub value: String,
    pub max_fee: String,
    pub deadline: u64,
    pub version: u64,
    pub nonce: u64,
    pub signature: String,
    pub created_at: u64,
}

/// The sign-only gasless withdraw outcome.
pub enum GaslessWithdrawOutcome {
    /// A signed (not broadcast, not submitted) authorization plus fee details.
    Signed {
        authorization: GaslessSignedAuthorization,
        fee_details: GaslessFeeDetails,
        /// The validated submit payload, ready for the deferred submission flow
        /// (D-submit); not sent today.
        submit_payload: SubmitRequest,
    },
    /// Deterministic unavailability with fallback permitted (§49.8 step 6): the
    /// caller should retry on the native rail.
    FallbackToNative,
}

// ---------------------------------------------------------------------------
// Sign-only orchestration (§49.8)
// ---------------------------------------------------------------------------

/// Inputs to a gasless withdraw (resolved at the call site).
pub struct GaslessWithdrawRequest<'a> {
    pub coin: &'a EthCoin,
    pub provider: &'a TronGaslessProviderConfig,
    pub token_config: &'a GaslessTokenConfig,
    pub network: Network,
    pub token_contract: TronAddress,
    pub receiver: TronAddress,
    /// Transfer amount in token units.
    pub amount: BigDecimal,
    pub options: Option<GaslessWithdrawOptions>,
    pub allow_fallback: bool,
}

fn now_secs() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        (common::now_ms() / 1000) as u64
    }
}

/// Read the on-chain TRC20 `balanceOf(owner)` (base units) via the Tron node
/// client (§21.6), used for the preflight (§49.7).
async fn trc20_balance_of(
    api: &TronApiClient,
    contract: &TronAddress,
    owner: &TronAddress,
) -> Result<U256, GasFreeWithdrawError> {
    let mut parameter = [0u8; 32];
    parameter[12..].copy_from_slice(owner.to_evm_address().as_ref());
    let param_hex = hex::encode(parameter);
    let resp = api
        .trigger_constant_contract(owner, contract, "balanceOf(address)", &param_hex)
        .await
        .map_err(|e| GasFreeWithdrawError::Provider(GasFreeProviderError::Transport(e.to_string())))?;
    let raw = resp
        .constant_result
        .and_then(|mut v| v.pop())
        .ok_or_else(|| GasFreeWithdrawError::InvalidProviderResponse("balanceOf returned no result".to_owned()))?;
    let bytes = hex::decode(raw)
        .map_err(|e| GasFreeWithdrawError::InvalidProviderResponse(format!("bad balanceOf hex: {e}")))?;
    if bytes.len() < 32 {
        return Err(GasFreeWithdrawError::InvalidProviderResponse(
            "balanceOf result shorter than 32 bytes".to_owned(),
        ));
    }
    Ok(U256::from_big_endian(&bytes[..32]))
}

/// Execute the sign-only gasless withdraw (§49.8). Eligibility (R1): the coin
/// must be a Tron TRC20 token.
pub async fn gasless_withdraw(req: GaslessWithdrawRequest<'_>) -> Result<GaslessWithdrawOutcome, GasFreeWithdrawError> {
    // R1: TRC20-only, Tron-only.
    if !matches!(req.coin.coin_type, EthCoinType::Trc20 { .. }) {
        return Err(GasFreeWithdrawError::Config(
            "gasless rail applies only to Tron TRC20 tokens".to_owned(),
        ));
    }
    if !req.token_config.enabled {
        return Err(GasFreeWithdrawError::Config(
            "token has not enabled the gasless rail".to_owned(),
        ));
    }

    let decimals = req.coin.decimals;
    let user = TronAddress::from_evm_address(req.coin.my_address);

    // §49.5: derive and (in preflight) verify the custody address.
    let artifacts = super::config::network_artifacts(&req.network);
    let custody = derive_gasfree_address(&req.coin.my_address, &artifacts);

    let value = wei_from_big_decimal(&req.amount, decimals)
        .map_err(|e| GasFreeWithdrawError::Config(format!("invalid amount: {e}")))?;

    let api = req
        .coin
        .tron_api
        .as_ref()
        .ok_or_else(|| GasFreeWithdrawError::Config("Tron node client missing on this coin".to_owned()))?;

    let client = GasFreeRestClient::new(
        req.provider.base_url.clone(),
        req.network.clone(),
        req.provider.api_key.clone(),
        req.provider.api_secret.clone(),
    );

    // §49.7 inputs.
    let account = client.account_info(&custody.to_base58()).await?;
    let onchain_balance = trc20_balance_of(api, &req.token_contract, &custody).await?;

    let outcome = preflight(&PreflightInputs {
        derived_custody: &custody,
        account: &account,
        onchain_balance,
        token: &req.token_contract,
        activated_decimals: decimals,
        value,
    });

    let quote = match outcome {
        PreflightOutcome::Available(q) => q,
        PreflightOutcome::Pending => return Err(GasFreeWithdrawError::PendingTransfer),
        PreflightOutcome::Disabled(reason) => return disabled_to_result(reason, req.allow_fallback),
    };

    // §49.8 step 3: effective fee cap (token units → base units).
    let per_request_cap = opt_cap_to_base(req.options.as_ref().and_then(|o| o.max_fee.as_ref()), decimals)?;
    let activation_cap = opt_cap_to_base(req.token_config.transfer_max_fee.as_ref(), decimals)?;
    let signed_max_fee = resolve_fee_cap(quote.total_token_fee, per_request_cap, activation_cap)?;

    // §49.8 step 3: deadline window.
    let window = match req.options.as_ref().and_then(|o| o.deadline_seconds) {
        Some(0) => {
            return Err(GasFreeWithdrawError::Config(
                "deadline_seconds must be greater than zero".to_owned(),
            ))
        },
        Some(w) => w,
        None => DEFAULT_DEADLINE_SECONDS,
    };
    let created_at = now_secs();
    let deadline = created_at.saturating_add(window);

    // §49.8 step 4: sign the PermitTransfer.
    let secret = req
        .coin
        .signer
        .local_secret()
        .ok_or_else(|| GasFreeWithdrawError::Config("gasless withdraw requires a local signing key".to_owned()))?;

    let permit = PermitTransfer::new(
        req.token_contract.to_evm_address().into(),
        req.provider.service_provider.to_evm_address().into(),
        user.to_evm_address().into(),
        req.receiver.to_evm_address().into(),
        value,
        signed_max_fee,
        deadline,
        quote.nonce,
    );
    let domain = PermitDomain {
        chain_id: super::config::tip712_chain_id(&req.network),
        verifying_contract: artifacts.controller,
    };
    let signature = sign_permit(secret, &domain, &permit, created_at)?;

    // §49.8 step 5: build the typed off-chain payload + fee details.
    let submit_payload = SubmitRequest::new(SubmitFields {
        request_id: Some(uuid::Uuid::new_v4()),
        token: req.token_contract,
        service_provider: req.provider.service_provider,
        user,
        receiver: req.receiver,
        value,
        max_fee: signed_max_fee,
        deadline,
        version: permit.version,
        nonce: quote.nonce,
        sig_hex: signature.to_hex(),
    })
    .map_err(GasFreeWithdrawError::Provider)?;

    let authorization = GaslessSignedAuthorization {
        kind: "gasfree_permit_transfer",
        gasfree_address: custody.to_base58(),
        token: req.token_contract.to_base58(),
        service_provider: req.provider.service_provider.to_base58(),
        user: user.to_base58(),
        receiver: req.receiver.to_base58(),
        value: value.to_string(),
        max_fee: signed_max_fee.to_string(),
        deadline,
        version: permit.version,
        nonce: quote.nonce,
        signature: signature.to_hex(),
        created_at,
    };

    let fee_details = GaslessFeeDetails {
        coin: req.coin.ticker.clone(),
        fee_method: "gasless",
        provider_name: "gasfree",
        gasfree_address: custody.to_base58(),
        transfer_fee: base_to_token(quote.transfer_fee, decimals)?,
        activation_fee: base_to_token(quote.activation_fee, decimals)?,
        total_token_fee: base_to_token(quote.total_token_fee, decimals)?,
        signed_max_fee: Some(base_to_token(signed_max_fee, decimals)?),
        trace_id: None,
    };

    Ok(GaslessWithdrawOutcome::Signed {
        authorization,
        fee_details,
        submit_payload,
    })
}

/// Map a `Disabled` preflight outcome to a result, honouring the fallback
/// policy (§49.8 step 6). Address/decimal mismatches are safety stops and never
/// fall back silently (§49.7).
fn disabled_to_result(
    reason: DisabledReason,
    allow_fallback: bool,
) -> Result<GaslessWithdrawOutcome, GasFreeWithdrawError> {
    match reason {
        DisabledReason::AddressMismatch { local, provider } => Err(GasFreeWithdrawError::Unavailable(format!(
            "custody address mismatch (local {local} != provider {provider})"
        ))),
        DisabledReason::TokenDecimalMismatch { provider, activated } => Err(GasFreeWithdrawError::Unavailable(
            format!("token decimal mismatch (provider {provider} != activated {activated})"),
        )),
        // Deterministic unavailability eligible for fallback (§49.8 step 6).
        DisabledReason::TokenUnsupported
        | DisabledReason::InsufficientSpendableBalance { .. }
        | DisabledReason::InactiveInsufficientBalance { .. } => {
            if allow_fallback {
                Ok(GaslessWithdrawOutcome::FallbackToNative)
            } else {
                Err(GasFreeWithdrawError::Unavailable(
                    "gasless rail unavailable for this transfer".to_owned(),
                ))
            }
        },
    }
}

fn opt_cap_to_base(cap: Option<&BigDecimal>, decimals: u8) -> Result<Option<U256>, GasFreeWithdrawError> {
    match cap {
        Some(c) => wei_from_big_decimal(c, decimals)
            .map(Some)
            .map_err(|e| GasFreeWithdrawError::Config(format!("invalid fee cap: {e}"))),
        None => Ok(None),
    }
}

fn base_to_token(v: U256, decimals: u8) -> Result<BigDecimal, GasFreeWithdrawError> {
    u256_to_big_decimal(v, decimals).map_err(|e| GasFreeWithdrawError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(fallback: bool) -> GaslessWithdrawOptions {
        GaslessWithdrawOptions {
            max_fee: None,
            deadline_seconds: None,
            fallback_to_native: fallback,
        }
    }

    #[test]
    fn native_is_default() {
        assert_eq!(select_rail(None, None, false).unwrap(), RailDecision::Native);
        assert_eq!(
            select_rail(Some(FeeMethod::Native), None, false).unwrap(),
            RailDecision::Native
        );
    }

    #[test]
    fn gasless_options_with_native_rejected() {
        let err = select_rail(None, Some(&opts(false)), false).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Config(_)));
        let err = select_rail(Some(FeeMethod::Native), Some(&opts(false)), false).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Config(_)));
    }

    #[test]
    fn gasless_selects_gasless() {
        assert_eq!(
            select_rail(Some(FeeMethod::Gasless), None, false).unwrap(),
            RailDecision::Gasless { allow_fallback: false }
        );
        assert_eq!(
            select_rail(Some(FeeMethod::Gasless), Some(&opts(true)), false).unwrap(),
            RailDecision::Gasless { allow_fallback: true }
        );
    }

    #[test]
    fn gasless_max_amount_rejected() {
        let err = select_rail(Some(FeeMethod::Gasless), None, true).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Config(_)));
    }

    #[test]
    fn auto_routes_gasless_with_fallback() {
        assert_eq!(
            select_rail(Some(FeeMethod::Auto), None, false).unwrap(),
            RailDecision::Gasless { allow_fallback: true }
        );
    }

    #[test]
    fn auto_max_amount_routes_native() {
        assert_eq!(
            select_rail(Some(FeeMethod::Auto), None, true).unwrap(),
            RailDecision::Native
        );
    }

    #[test]
    fn fee_method_deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<FeeMethod>(r#""native""#).unwrap(),
            FeeMethod::Native
        );
        assert_eq!(
            serde_json::from_str::<FeeMethod>(r#""gasless""#).unwrap(),
            FeeMethod::Gasless
        );
        assert_eq!(serde_json::from_str::<FeeMethod>(r#""auto""#).unwrap(), FeeMethod::Auto);
        assert!(serde_json::from_str::<FeeMethod>(r#""bogus""#).is_err());
    }

    #[test]
    fn disabled_safety_stop_never_falls_back() {
        let r = disabled_to_result(
            DisabledReason::AddressMismatch {
                local: "a".into(),
                provider: "b".into(),
            },
            true,
        );
        assert!(matches!(r, Err(GasFreeWithdrawError::Unavailable(_))));
        let r = disabled_to_result(
            DisabledReason::TokenDecimalMismatch {
                provider: 18,
                activated: 6,
            },
            true,
        );
        assert!(matches!(r, Err(GasFreeWithdrawError::Unavailable(_))));
    }

    #[test]
    fn disabled_balance_falls_back_when_allowed() {
        let r = disabled_to_result(DisabledReason::TokenUnsupported, true);
        assert!(matches!(r, Ok(GaslessWithdrawOutcome::FallbackToNative)));
        let r = disabled_to_result(DisabledReason::TokenUnsupported, false);
        assert!(matches!(r, Err(GasFreeWithdrawError::Unavailable(_))));
    }

    #[test]
    fn fee_details_serializes_public_field_names() {
        let fd = GaslessFeeDetails {
            coin: "USDT-TRC20".into(),
            fee_method: "gasless",
            provider_name: "gasfree",
            gasfree_address: "TCustody".into(),
            transfer_fee: BigDecimal::from(2),
            activation_fee: BigDecimal::from(0),
            total_token_fee: BigDecimal::from(2),
            signed_max_fee: Some(BigDecimal::from(5)),
            trace_id: None,
        };
        let v = serde_json::to_value(&fd).unwrap();
        assert_eq!(v["fee_method"], "gasless");
        assert_eq!(v["provider_name"], "gasfree");
        assert_eq!(v["gasfree_address"], "TCustody");
        assert!(v.get("transfer_fee").is_some());
        assert!(v.get("activation_fee").is_some());
        assert!(v.get("total_token_fee").is_some());
        assert!(v.get("signed_max_fee").is_some());
        // trace_id omitted when None.
        assert!(v.get("trace_id").is_none());
    }
}
