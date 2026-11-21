//! The gasless rail of a TRC20 withdraw (CRD ch.63 §63.7, §63.8 steps 2 to 5, §63.8A, §63.9, R29).
//!
//! The rail is sign-only: it reads the provider's account record, decides whether the transfer may proceed, signs
//! the `PermitTransfer` authorization and returns it as the withdraw result. It sends nothing to the provider and
//! nothing to the chain (D63.6).
//!
//! The rail selection (`native`, `gasless`, `auto`) and the fallback to the native rail live in
//! [`super::super::withdraw`]; this module answers whether the rail applies to a coin ([`resolve`]) and runs it
//! ([`GaslessRail::run`]), telling its caller how each failure counts for the fallback ([`FailureClass`]).
//!
//! # Invariants
//!
//! - The rail never estimates a TRX fee, never reads the sender's TRX balance or resources and never dry-runs the
//!   transfer: the fee is paid in the token, by the provider. Its node reads are the controller's
//!   `getGasFreeAddress` (R29, at most once per address in 10 minutes) and the token balance of the custody
//!   address.
//! - Every token amount (the value, the fees, the caps, the balances, the signed `value` and `maxFee`) is a
//!   256-bit integer in the token's smallest unit.
//! - Nothing is signed after a refusal, and nothing is signed before the controller check and the preflight
//!   passed under the sender's lock.
//! - No error text and no log line of this module carries a provider credential or a signature (R9). A failure of
//!   the provider exchange that the withdraw RPC reports through its shared transport arm has one fixed text
//!   there; its kind goes to the log only (§63.9.3).

use super::super::api::TronApiClient;
use super::super::fee::{GaslessFeeMethod, GaslessProviderName, TronGaslessFeeDetails};
use super::super::TronAddress;
use super::authorization::{sign_permit, GasFreeRelayPayload, PermitTransfer};
use super::binding::{CoinGasFree, GasFreeBinding};
use super::custody::{check_against_controller, custody_token_balance};
use super::failure::{GasFreeError, GaslessMember, RpcArm};
use super::preflight::{decide, CustodyBalance, PreflightDecision, PreflightIncomplete, Quote, Refusal};
use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType, EvmWithdrawSender};
use crate::rpc_command::init_withdraw::{WithdrawInProgressStatus, WithdrawTaskHandle};
use crate::{GaslessWithdrawError, TransactionDetails, UnsignedTransaction, WithdrawError, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::log::warn;
use common::now_ms;
use ethereum_types::U256;

/// How long a signed authorization stays valid when the request does not say, in seconds (§63.8 step 3, R20).
pub(crate) const DEFAULT_DEADLINE_SECONDS: u64 = 300;

/// The text the withdraw RPC shows for every provider failure of its shared transport arm (§63.9.3): a caller
/// cannot tell the kinds apart there.
const PROVIDER_TRANSPORT_TEXT: &str = "The GasFree provider could not be reached or did not serve the request";

/// The gasless terms of a withdraw request (§63.3.4), read and checked before anything else is done (§63.9.4).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GaslessTerms {
    /// The request's cap on the provider fee, in token units.
    max_fee: Option<BigDecimal>,
    /// The validity window of the authorization, in seconds: the request's, or the default.
    deadline_seconds: u64,
    /// Whether a deterministically unavailable rail may be replaced by the native rail.
    pub(crate) fallback_to_native: bool,
}

impl GaslessTerms {
    /// The terms of `request`: its `gasless` object, or the defaults when it has none.
    ///
    /// # Errors
    ///
    /// [`WithdrawError::InvalidFeePolicy`] for a `max_fee` that is not a decimal amount or is below zero, and for
    /// a `deadline_seconds` of zero (§63.9.4, R7).
    pub(crate) fn from_request(request: &WithdrawRequest) -> Result<Self, WithdrawError> {
        let Some(options) = &request.gasless else {
            return Ok(GaslessTerms {
                max_fee: None,
                deadline_seconds: DEFAULT_DEADLINE_SECONDS,
                fallback_to_native: false,
            });
        };
        let deadline_seconds = match options.deadline_seconds {
            Some(0) => {
                return Err(WithdrawError::InvalidFeePolicy(
                    "'gasless.deadline_seconds' must be greater than zero".to_owned(),
                ))
            },
            Some(seconds) => seconds,
            // The documented window of a request without the member (R20).
            None => DEFAULT_DEADLINE_SECONDS,
        };
        Ok(GaslessTerms {
            max_fee: options.max_fee_amount()?,
            deadline_seconds,
            fallback_to_native: options.fallback_to_native,
        })
    }
}

/// How a failure of the rail counts for the fallback to the native rail (§63.8 step 6, R29).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureClass {
    /// A deterministic unavailability: the token is not enrolled with the provider, or the custody account is
    /// short.
    Deterministic,
    /// The controller contract contradicts the derived custody address (R29). Not deterministic, unless the
    /// operator's switch says that it may fall back.
    Guard,
    /// Everything else: it always surfaces.
    Other,
}

/// A failure of the rail: the error of the withdraw RPC and its class for the fallback.
#[derive(Debug)]
pub(crate) struct RailFailure {
    pub(crate) error: WithdrawError,
    pub(crate) class: FailureClass,
}

impl RailFailure {
    fn other(error: WithdrawError) -> Self {
        RailFailure {
            error,
            class: FailureClass::Other,
        }
    }
}

/// Whether the gasless rail applies to a coin (§63.8 step 2).
pub(crate) enum RailEligibility {
    Applicable(GaslessRail),
    /// The rail does not exist for the coin; the text says why.
    NotApplicable(&'static str),
}

/// The gasless rail of one TRC20 token coin.
pub(crate) struct GaslessRail {
    /// The platform coin: it holds the provider binding the token resolves (§63.8 step 2).
    platform: EthCoin,
    /// The token's contract.
    token: TronAddress,
    /// The activation-time cap on the provider fee, in the token's smallest unit.
    transfer_max_fee: Option<U256>,
}

/// Whether the gasless rail applies to `coin` (§63.8 step 2, R1): a TRC20 token that was activated with an
/// enabled `gasless` block and whose platform coin holds a provider binding.
pub(crate) async fn resolve(coin: &EthCoin) -> RailEligibility {
    let EthCoinType::Trc20 { token_addr, .. } = &coin.coin_type else {
        return RailEligibility::NotApplicable("the gasless rail serves TRC20 tokens only");
    };
    let Some(CoinGasFree::Token(gasless)) = &coin.tron_gasfree else {
        return RailEligibility::NotApplicable("the token was activated without the gasless rail");
    };
    let Ok(platform) = coin.hd_serving_coin().await else {
        return RailEligibility::NotApplicable("the platform coin of the token is not active");
    };
    if platform.gasfree_platform_binding().is_none() {
        return RailEligibility::NotApplicable("the platform coin was activated without a GasFree provider");
    }
    RailEligibility::Applicable(GaslessRail {
        platform,
        token: TronAddress::from_evm_address(*token_addr),
        transfer_max_fee: gasless.transfer_max_fee,
    })
}

/// The gasless error `member` of the withdraw RPC with `text`.
pub(crate) fn gasless_error(member: GaslessMember, text: String) -> WithdrawError {
    WithdrawError::Gasless(match member {
        GaslessMember::Unavailable => GaslessWithdrawError::Unavailable(text),
        GaslessMember::PendingTransfer => GaslessWithdrawError::PendingTransfer(text),
        GaslessMember::MaxFeeExceeded => GaslessWithdrawError::MaxFeeExceeded(text),
        GaslessMember::QuoteExpired => GaslessWithdrawError::QuoteExpired(text),
        GaslessMember::ProviderRejected => GaslessWithdrawError::ProviderRejected(text),
        GaslessMember::InvalidProviderResponse => GaslessWithdrawError::InvalidProviderResponse(text),
        GaslessMember::TraceNotFound => GaslessWithdrawError::TraceNotFound(text),
    })
}

/// The withdraw error of a failed step of the GasFree group (§63.9.3, R17): the collapse of
/// [`GasFreeError::rpc_arm`]. A failed node read is the node error, as on the native rail. A provider failure of
/// the transport arm has one fixed text; its kind is logged.
fn collapse(error: GasFreeError) -> WithdrawError {
    match error.rpc_arm() {
        RpcArm::Gasless(member) => gasless_error(member, error.to_string()),
        RpcArm::Transport => match error {
            GasFreeError::Node(node) => WithdrawError::Transport(node.to_string()),
            GasFreeError::CustodyBalance(balance) => WithdrawError::from(balance),
            provider => {
                warn!("GasFree provider failure: {}", provider);
                WithdrawError::Transport(PROVIDER_TRANSPORT_TEXT.to_owned())
            },
        },
        RpcArm::InternalError | RpcArm::NotSufficientBalance => WithdrawError::InternalError(error.to_string()),
    }
}

/// `amount`, a decimal amount in token units, in the smallest unit of a token with `decimals`.
///
/// # Errors
///
/// [`WithdrawError::InvalidFeePolicy`] when the amount has more decimal places than the token or does not fit
/// 256 bits. `member` names the request member in the text.
fn fee_units(amount: &BigDecimal, decimals: u8, member: &str) -> Result<U256, WithdrawError> {
    let refusal = || WithdrawError::InvalidFeePolicy(format!("'{member}' is not an amount the token can express"));
    // The conversion cuts the digits beyond `decimals`; a value that does not read back as it was given had some.
    let units = wei_from_big_decimal(amount, decimals).map_err(|_| refusal())?;
    let read_back = u256_to_big_decimal(units, decimals).map_err(|_| refusal())?;
    if read_back != *amount {
        return Err(refusal());
    }
    Ok(units)
}

/// The effective cap on the provider fee (§63.8 step 3, R8): the smaller of the caps that are present, else the
/// quoted total fee itself.
pub(crate) fn effective_cap(request_cap: Option<U256>, activation_cap: Option<U256>, quoted_total: U256) -> U256 {
    match (request_cap, activation_cap) {
        (Some(request), Some(activation)) => request.min(activation),
        (Some(cap), None) | (None, Some(cap)) => cap,
        (None, None) => quoted_total,
    }
}

impl GaslessRail {
    fn binding(&self) -> Result<&GasFreeBinding, WithdrawError> {
        self.platform.gasfree_platform_binding().ok_or_else(|| {
            WithdrawError::InternalError("the platform coin of the gasless rail holds no provider binding".to_owned())
        })
    }

    /// Runs the rail for a transfer of `request.amount` of `coin` from `sender` to `receiver` (§63.8 steps 3 to
    /// 5): under the sender's lock, the controller check (R29), the preflight (§63.7), the fee cap, the signature;
    /// then the sign-only result (§63.8A). `progress` is the handle of the task that runs the withdraw, if a task
    /// does.
    ///
    /// # Errors
    ///
    /// A [`RailFailure`] whose error is:
    ///
    /// - [`WithdrawError::InvalidFeePolicy`] for an amount of zero and for a `max_fee` the token cannot express,
    ///   before any node or provider request;
    /// - `Gasless`/`Unavailable` when the controller contradicts the derived custody address (class
    ///   [`FailureClass::Guard`]) and when the provider does not list the token (class
    ///   [`FailureClass::Deterministic`]);
    /// - [`WithdrawError::NotSufficientCustodyBalance`] for a custody shortfall (class
    ///   [`FailureClass::Deterministic`]);
    /// - `Gasless`/`PendingTransfer`, `MaxFeeExceeded`, `QuoteExpired`, `ProviderRejected` and
    ///   `InvalidProviderResponse`, [`WithdrawError::Transport`] and [`WithdrawError::InternalError`] as §63.9.2
    ///   and §63.9.3 map them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(
        &self,
        coin: &EthCoin,
        api: &TronApiClient,
        signer: &EvmWithdrawSender,
        sender: &TronAddress,
        receiver: &TronAddress,
        request: &WithdrawRequest,
        terms: &GaslessTerms,
        progress: Option<&WithdrawTaskHandle>,
    ) -> Result<TransactionDetails, RailFailure> {
        let binding = self.binding().map_err(RailFailure::other)?;
        let decimals = coin.decimals;

        let value = wei_from_big_decimal(&request.amount, decimals)
            .map_err(|e| RailFailure::other(WithdrawError::from(e.into_inner())))?;
        if value.is_zero() {
            return Err(RailFailure::other(WithdrawError::InvalidFeePolicy(
                "a gasless withdraw needs an amount above zero".to_owned(),
            )));
        }
        let request_cap = match &terms.max_fee {
            Some(max_fee) => Some(fee_units(max_fee, decimals, "gasless.max_fee").map_err(RailFailure::other)?),
            None => None,
        };

        report_progress(progress, WithdrawInProgressStatus::FetchingGaslessQuote)?;

        // From here to the signature nothing else quotes or signs for this address on this platform.
        let lock = binding.sender_lock(sender);
        let _guard = lock.lock().await;

        // R29: the derived custody address, confirmed by the controller contract now or within the last 10 minutes.
        let custody = check_against_controller(api, binding.controller_checks(), binding.network(), sender)
            .await
            .map_err(|error| {
                let class = match error {
                    GasFreeError::DerivationMismatch { .. } => FailureClass::Guard,
                    _ => FailureClass::Other,
                };
                RailFailure {
                    error: collapse(error),
                    class,
                }
            })?;

        let quote = self
            .preflight(coin, api, binding, sender, &custody, value, decimals)
            .await?;

        let cap = effective_cap(request_cap, self.transfer_max_fee, quote.total_fee);
        if quote.total_fee > cap {
            let text = format!(
                "the provider quotes a fee of {} {}, above the accepted maximum of {}",
                token_amount(quote.total_fee, decimals).map_err(RailFailure::other)?,
                coin.ticker,
                token_amount(cap, decimals).map_err(RailFailure::other)?,
            );
            return Err(RailFailure::other(gasless_error(GaslessMember::MaxFeeExceeded, text)));
        }

        report_progress(progress, WithdrawInProgressStatus::SigningGaslessAuthorization)?;

        let now_s = now_ms() / 1000;
        let deadline = now_s.checked_add(terms.deadline_seconds).ok_or_else(|| {
            RailFailure::other(WithdrawError::InvalidFeePolicy(
                "'gasless.deadline_seconds' is beyond the range of a deadline".to_owned(),
            ))
        })?;
        let permit = PermitTransfer {
            token: self.token,
            service_provider: *binding.service_provider(),
            user: *sender,
            receiver: *receiver,
            value,
            max_fee: cap,
            deadline,
            nonce: quote.nonce,
        };
        let secret = signer.local_secret(coin).ok_or_else(|| {
            RailFailure::other(WithdrawError::UnsupportedError(
                "A gasless TRON withdraw needs a local private key".to_owned(),
            ))
        })?;
        let signed =
            sign_permit(binding.network(), permit, secret, now_s).map_err(|e| RailFailure::other(collapse(e)))?;
        let payload = GasFreeRelayPayload::new(binding.network(), signed, now_s);

        sign_only_result(coin, sender, receiver, request, &quote, cap, payload, now_s).map_err(RailFailure::other)
    }

    /// The preflight of §63.7: the provider's account record and, only when the decision needs it, the token
    /// balance of the custody address. Returns the quote of a transfer that may proceed.
    #[allow(clippy::too_many_arguments)]
    async fn preflight(
        &self,
        coin: &EthCoin,
        api: &TronApiClient,
        binding: &GasFreeBinding,
        sender: &TronAddress,
        custody: &TronAddress,
        value: U256,
        decimals: u8,
    ) -> Result<Quote, RailFailure> {
        let account = binding
            .provider()
            .account_info(sender)
            .await
            .map_err(|e| RailFailure::other(collapse(e)))?;

        // The wait decision and three of the refusals need no chain read: the balance is read only when asked for.
        let decision = match decide(&account, CustodyBalance::NotRead, custody, &self.token, decimals, value) {
            Err(PreflightIncomplete::BalanceNotRead) => {
                let balance = custody_token_balance(api, &self.token, custody)
                    .await
                    .map_err(|e| RailFailure::other(collapse(e)))?;
                decide(
                    &account,
                    CustodyBalance::Read(balance),
                    custody,
                    &self.token,
                    decimals,
                    value,
                )
            },
            decided => decided,
        };
        let decision = decision.map_err(|incomplete| {
            RailFailure::other(WithdrawError::InternalError(match incomplete {
                PreflightIncomplete::BalanceNotRead => {
                    "the GasFree preflight asked for a balance it was given".to_owned()
                },
                PreflightIncomplete::AmountOverflow => {
                    "the amount and the provider's fee do not fit the token's amount range".to_owned()
                },
            }))
        })?;

        match decision {
            PreflightDecision::Proceed(quote) => Ok(quote),
            PreflightDecision::Wait => Err(RailFailure::other(gasless_error(
                GaslessMember::PendingTransfer,
                "the provider accepts no new transfer of this account until the earlier one is settled".to_owned(),
            ))),
            PreflightDecision::Refuse(Refusal::TokenNotEnrolled) => Err(RailFailure {
                error: gasless_error(
                    GaslessMember::Unavailable,
                    format!("the GasFree provider does not list {} for this account", coin.ticker),
                ),
                class: FailureClass::Deterministic,
            }),
            PreflightDecision::Refuse(Refusal::CustodyMismatch) => Err(RailFailure::other(gasless_error(
                GaslessMember::InvalidProviderResponse,
                "the provider reports another custody address than the one derived for the sender".to_owned(),
            ))),
            PreflightDecision::Refuse(Refusal::PrecisionDisagreement) => Err(RailFailure::other(gasless_error(
                GaslessMember::InvalidProviderResponse,
                format!(
                    "the provider reports another number of decimals for {} than the activated token has",
                    coin.ticker
                ),
            ))),
            PreflightDecision::Refuse(Refusal::Shortfall { available, required }) => Err(RailFailure {
                error: WithdrawError::NotSufficientCustodyBalance {
                    coin: coin.ticker.clone(),
                    available: token_amount(available, decimals).map_err(RailFailure::other)?,
                    required: token_amount(required, decimals).map_err(RailFailure::other)?,
                    custody_address: custody.to_base58(),
                },
                class: FailureClass::Deterministic,
            }),
        }
    }
}

/// Tells the task that runs the withdraw, if one does, that the rail entered `status` (§63.3.6).
fn report_progress(progress: Option<&WithdrawTaskHandle>, status: WithdrawInProgressStatus) -> Result<(), RailFailure> {
    match progress {
        Some(handle) => handle
            .update_in_progress_status(status)
            .map_err(|e| RailFailure::other(WithdrawError::from(e.into_inner()))),
        None => Ok(()),
    }
}

/// `units` of a token with `decimals` as a decimal amount in token units.
fn token_amount(units: U256, decimals: u8) -> Result<BigDecimal, WithdrawError> {
    u256_to_big_decimal(units, decimals).map_err(|e| WithdrawError::from(e.into_inner()))
}

/// The sign-only result of the rail (§63.8 step 5, §63.8A R25 to R28, D63.8 (a)): the shared members of a
/// withdraw result with the payload's members beside them, the gasless fee details, and neither `tx_hex` nor
/// `tx_hash`.
#[allow(clippy::too_many_arguments)]
fn sign_only_result(
    coin: &EthCoin,
    sender: &TronAddress,
    receiver: &TronAddress,
    request: &WithdrawRequest,
    quote: &Quote,
    signed_max_fee: U256,
    payload: GasFreeRelayPayload,
    created_at: u64,
) -> Result<TransactionDetails, WithdrawError> {
    let decimals = coin.decimals;
    let total_token_fee = token_amount(quote.total_fee, decimals)?;
    let fee_details = TronGaslessFeeDetails {
        coin: coin.ticker.clone(),
        fee_method: GaslessFeeMethod::Gasless,
        provider_name: GaslessProviderName::GasFree,
        gasfree_address: payload.gasfree_address.to_base58(),
        transfer_fee: token_amount(quote.transfer_fee, decimals)?,
        activation_fee: token_amount(quote.activation_fee, decimals)?,
        total_token_fee: total_token_fee.clone(),
        signed_max_fee: Some(token_amount(signed_max_fee, decimals)?),
        // Nothing is submitted, so the provider has given no trace id (D63.6).
        trace_id: None,
    };

    let amount = request.amount.clone();
    // The provider takes its fee from the custody account in the same token.
    let spent_by_me = &amount + &total_token_fee;
    let received_by_me: BigDecimal = if receiver == sender { amount.clone() } else { 0.into() };

    Ok(TransactionDetails {
        // The result carries no transaction: the two members are omitted from its serialized form (R25).
        tx_hex: Vec::new().into(),
        tx_json: None,
        tx_hash: String::new(),
        from: vec![sender.to_base58()],
        to: vec![receiver.to_base58()],
        total_amount: amount,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: created_at,
        fee_details: Some(fee_details.into()),
        coin: coin.ticker.clone(),
        internal_id: Vec::new().into(),
        kmd_rewards: None,
        transaction_type: Default::default(),
        memo: None,
        unsigned: Some(UnsignedTransaction::TronGasFree(payload)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(gasless: serde_json::Value) -> WithdrawRequest {
        serde_json::from_value(json!({
            "coin": "USDT-TRC20",
            "to": "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL",
            "amount": "1",
            "fee_method": "gasless",
            "gasless": gasless,
        }))
        .expect("a withdraw request")
    }

    /// ch.63 T8: the effective cap is the smaller of the present caps, else the quoted total.
    #[test]
    fn the_effective_cap_is_the_smaller_present_cap_or_the_quote() {
        let quoted = U256::from(2_000_000u64);
        let (small, large) = (U256::from(1_500_000u64), U256::from(3_000_000u64));
        assert_eq!(effective_cap(None, None, quoted), quoted);
        assert_eq!(effective_cap(Some(large), None, quoted), large);
        assert_eq!(effective_cap(None, Some(small), quoted), small);
        assert_eq!(effective_cap(Some(large), Some(small), quoted), small);
        assert_eq!(effective_cap(Some(small), Some(large), quoted), small);
        // A cap of zero is a cap.
        assert_eq!(effective_cap(Some(U256::zero()), Some(large), quoted), U256::zero());
    }

    /// ch.63 T8, §63.9.4: the request terms, their defaults and their refusals.
    #[test]
    fn the_terms_have_the_documented_defaults_and_refuse_a_zero_deadline_and_an_unusable_max_fee() {
        let terms = GaslessTerms::from_request(&request(json!({}))).expect("the defaults");
        assert_eq!(terms.deadline_seconds, 300);
        assert_eq!(terms.max_fee, None);
        assert!(!terms.fallback_to_native);

        let terms = GaslessTerms::from_request(&request(
            json!({ "max_fee": "1.5", "deadline_seconds": 60, "fallback_to_native": true }),
        ))
        .expect("usable terms");
        assert_eq!(terms.deadline_seconds, 60);
        assert_eq!(terms.max_fee, Some("1.5".parse().expect("a decimal")));
        assert!(terms.fallback_to_native);
        // A JSON number is a decimal amount too.
        let terms = GaslessTerms::from_request(&request(json!({ "max_fee": 2 }))).expect("usable terms");
        assert_eq!(terms.max_fee, Some(BigDecimal::from(2)));

        for refused in [
            json!({ "deadline_seconds": 0 }),
            json!({ "max_fee": "a lot" }),
            json!({ "max_fee": "" }),
            json!({ "max_fee": true }),
            json!({ "max_fee": ["1"] }),
            json!({ "max_fee": { "amount": "1" } }),
            json!({ "max_fee": "-0.1" }),
        ] {
            let error = GaslessTerms::from_request(&request(refused)).expect_err("unusable terms");
            assert!(
                matches!(error, WithdrawError::InvalidFeePolicy(_)),
                "a fee-validation error"
            );
        }
    }

    #[test]
    fn a_cap_finer_than_the_token_is_a_fee_validation_error() {
        let units = |text: &str| fee_units(&text.parse().expect("a decimal"), 6, "gasless.max_fee");
        assert_eq!(units("1.5").ok(), Some(U256::from(1_500_000u64)));
        assert_eq!(units("0").ok(), Some(U256::zero()));
        assert!(matches!(units("0.0000001"), Err(WithdrawError::InvalidFeePolicy(_))));
    }

    /// §63.9.1: the members of the withdraw error carry the statuses the group's own table binds.
    #[test]
    fn each_member_has_the_wire_name_and_status_of_the_family() {
        use common::HttpStatusCode;
        for member in [
            GaslessMember::Unavailable,
            GaslessMember::PendingTransfer,
            GaslessMember::MaxFeeExceeded,
            GaslessMember::QuoteExpired,
            GaslessMember::ProviderRejected,
            GaslessMember::InvalidProviderResponse,
            GaslessMember::TraceNotFound,
        ] {
            let error = gasless_error(member, "text".to_owned());
            assert_eq!(error.status_code().as_u16(), member.http_status());
            let document = serde_json::to_value(&error).expect("a document");
            assert_eq!(document["error_type"], "Gasless");
            assert_eq!(document["error_data"]["error_type"], member.wire_name());
            assert!(document["error_data"]["error_data"].is_string());
        }
    }
}
