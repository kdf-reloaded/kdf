//! The gasless rail of a TRC20 withdraw (CRD ch.63 §63.7, §63.8 steps 2 to 5, §63.8A, §63.8B, §63.9, R29).
//!
//! The rail reads the provider's account record, decides whether the transfer may proceed and signs the
//! `PermitTransfer` authorization. What follows depends on the route (D63.9):
//!
//! - The direct `withdraw` method is sign-only: the signed authorization is the withdraw result (§63.8A). Nothing
//!   is sent to the provider and nothing to the chain.
//! - The task route (`task::withdraw::init`, the caller that passes a task handle) submits the authorization to
//!   the provider ([`submit`]) and follows the transfer until it settles ([`track`]). Its result is the same
//!   result with the provider's trace id in the fee details and the transaction hash as `tx_hash`.
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
//! - The sender's lock is held until the signature on the direct method, and until the provider answered the
//!   submission (or the submission failed) on the task route. It is never held while the rail waits for the
//!   settlement.
//! - The submission is one request and is never repeated. Each submission carries a fresh version-4 UUID as its
//!   `requestId`. No request reaches the provider for an authorization whose deadline has passed.
//! - A failure after the submission never counts as an unavailability of the rail: the native rail does not run
//!   after it. Its text names the trace id.
//! - Every wait is bounded: no trace request is made at or after the bound [`track`] is given.
//! - No error text and no log line of this module carries a provider credential or a signature (R9). A failure of
//!   the provider exchange that the withdraw RPC reports through its shared transport arm has one fixed text
//!   there; its kind goes to the log only (§63.9.3).

use super::super::api::TronApiClient;
use super::super::fee::{GaslessFeeMethod, GaslessProviderName, TronGaslessFeeDetails};
use super::super::TronAddress;
use super::authorization::{sign_permit, GasFreeRelayPayload, PermitTransfer, SignedPermit, PERMIT_VERSION};
use super::binding::{CoinGasFree, GasFreeBinding};
use super::custody::{check_against_controller, custody_token_balance};
use super::failure::{sanitize_provider_text, GasFreeError, GaslessMember, RpcArm};
use super::preflight::{decide, CustodyBalance, PreflightDecision, PreflightIncomplete, Quote, Refusal};
use super::provider_payloads::{SubmitRequest, TransactionHash, TransactionState, TransferRecord, TransferState};
use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType, EvmWithdrawSender};
use crate::rpc_command::init_withdraw::{WithdrawInProgressStatus, WithdrawTaskHandle};
use crate::{GaslessWithdrawError, TransactionDetails, UnsignedTransaction, WithdrawError, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::log::{info, warn};
use common::{new_uuid, now_ms};
use ethereum_types::U256;
use futures::lock::MutexGuard as AsyncMutexGuard;
use uuid::Uuid;

/// How long a signed authorization stays valid when the request does not say, in seconds (§63.8 step 3, R20).
pub(crate) const DEFAULT_DEADLINE_SECONDS: u64 = 300;

/// How long after the signed deadline the task route still asks the provider for the transfer, in milliseconds
/// (D63.9 item 7).
pub(crate) const SETTLEMENT_GRACE_MS: u64 = 120_000;

/// The text the withdraw RPC shows for every provider failure of its shared transport arm (§63.9.3): a caller
/// cannot tell the kinds apart there.
const PROVIDER_TRANSPORT_TEXT: &str = "The GasFree provider could not be reached or did not serve the request";

/// The shortest time between two trace requests of one transfer, in milliseconds. A configured
/// `status_poll_interval_ms` below it, zero included, is raised to it where the interval is used; the activation
/// accepts the value as it is.
pub(crate) const MIN_POLL_INTERVAL_MS: u64 = 1_000;

/// The sentence the error of a submission with an unknown outcome ends with (D63.9): the request may have reached
/// the provider, and a second withdraw would sign a new authorization.
const MAYBE_ACCEPTED_TEXT: &str = ". The provider may have accepted the transfer; check the GasFree address balance \
                                   and its history before trying again";

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
    /// 5): under the sender's lock, the controller check (R29), the preflight (§63.7), the fee cap, the signature.
    ///
    /// `progress` is the handle of the task that runs the withdraw, if a task does, and it selects what follows
    /// the signature (D63.9):
    ///
    /// - `None`, the direct method: the sign-only result (§63.8A). No submit and no trace request is made.
    /// - `Some`, the task route: the authorization is submitted ([`submit`]) and the transfer is followed until
    ///   it settles or [`SETTLEMENT_GRACE_MS`] after the signed deadline ([`track`]). The result is the sign-only
    ///   result with the trace id in the fee details and the transaction hash as `tx_hash`. One INFO line is
    ///   logged when the provider took the submission and one when the wait ended.
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
    ///   and §63.9.3 map them;
    /// - on the task route also the errors of [`submit`] and [`track`]. None of them has a class other than
    ///   [`FailureClass::Other`].
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

        report_progress(progress, WithdrawInProgressStatus::FetchingGaslessQuote).map_err(RailFailure::other)?;

        // From here to the signature, and on the task route to the provider's answer to the submission, nothing
        // else quotes or signs for this address on this platform.
        let lock = binding.sender_lock(sender);
        let guard = lock.lock().await;

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

        report_progress(progress, WithdrawInProgressStatus::SigningGaslessAuthorization).map_err(RailFailure::other)?;

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

        if progress.is_none() {
            // The direct method signs only (§63.8A).
            return rail_result(coin, sender, receiver, request, &quote, cap, payload, now_s, None)
                .map_err(RailFailure::other);
        }

        let submitted = submit(
            binding,
            &payload.signed_authorization,
            guard,
            progress,
            &coin.ticker,
            &request.amount,
        )
        .await
        .map_err(RailFailure::other)?;
        let trace_id = submitted.trace_id.clone();
        info!(
            "GasFree transfer submitted: coin {}, trace id {}, from {}, to {}, amount {}",
            coin.ticker,
            trace_id,
            sender.to_base58(),
            receiver.to_base58(),
            request.amount
        );

        let bound_ms = deadline.saturating_mul(1000).saturating_add(SETTLEMENT_GRACE_MS);
        let outcome = track(binding, submitted, bound_ms, progress).await;
        match &outcome {
            Ok(hash) => info!("GasFree transfer {}: settled, transaction {}", trace_id, hash.to_hex()),
            Err(error) => info!("GasFree transfer {}: not settled: {}", trace_id, error),
        }
        let hash = outcome.map_err(RailFailure::other)?;
        rail_result(
            coin,
            sender,
            receiver,
            request,
            &quote,
            cap,
            payload,
            now_s,
            Some((&trace_id, hash)),
        )
        .map_err(RailFailure::other)
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
fn report_progress(
    progress: Option<&WithdrawTaskHandle>,
    status: WithdrawInProgressStatus,
) -> Result<(), WithdrawError> {
    match progress {
        Some(handle) => handle
            .update_in_progress_status(status)
            .map_err(|e| WithdrawError::from(e.into_inner())),
        None => Ok(()),
    }
}

/// A transfer the provider took: its trace id, a UUID, and the provider's record of it in the answer to the
/// submission.
pub(crate) struct Submitted {
    pub(crate) trace_id: String,
    record: TransferRecord,
}

/// Submits `signed` to the provider of `binding` (§63.4.6, D63.9): the phase `SubmittingGaslessAuthorization`,
/// then one request with a fresh version-4 UUID as its `requestId`, never repeated. Every member of the request
/// is the member of `signed`.
///
/// `guard` is the guard of the sender's lock (§63.8 step 3). It is released when the provider's answer is in or
/// the request failed, and when this function returns early (D63.9 item 1). `coin` and `amount` (in token units)
/// are for the log lines.
///
/// One INFO line is written just before the request is sent, with the coin, the sender, the receiver, the amount
/// and the `requestId`: a task that is cancelled, or a node that stops, while the request is under way leaves the
/// outcome unknown, and that line identifies the submission.
///
/// The outcome is unknown when the request may have reached the provider and no usable answer came back: a failed
/// connection (one that failed before anything was sent is not told apart), a timeout, a 5xx answer, an answer
/// that does not decode, and an answer that fails the checks below. The provider may then have accepted the
/// transfer. One WARN line says so, with the coin, the sender, the receiver, the amount and the `requestId`, and
/// the error text ends with the advice to check the GasFree address before trying again. A refusal before the
/// request, and a 4xx answer of the provider, are not such a case.
///
/// # Errors
///
/// - `Gasless`/`QuoteExpired` when the deadline of `signed` has passed; no request is made (D63.9 item 4).
/// - The collapse of §63.9.3 for a failed request: `Gasless`/`ProviderRejected` (a 404 included),
///   `Gasless`/`InvalidProviderResponse`, [`WithdrawError::Transport`], [`WithdrawError::InternalError`].
/// - `Gasless`/`InvalidProviderResponse` when the answer's `nonce`, `version`, `accountAddress`, `targetAddress`,
///   `tokenAddress` or `amount` is not what was signed (D63.9 item 3), and when its `id` is not a UUID. The text
///   names the `id`.
pub(crate) async fn submit(
    binding: &GasFreeBinding,
    signed: &SignedPermit,
    guard: AsyncMutexGuard<'_, ()>,
    progress: Option<&WithdrawTaskHandle>,
    coin: &str,
    amount: &BigDecimal,
) -> Result<Submitted, WithdrawError> {
    report_progress(progress, WithdrawInProgressStatus::SubmittingGaslessAuthorization)?;

    let permit = signed.permit();
    let now_s = now_ms() / 1000;
    if permit.deadline <= now_s {
        return Err(gasless_error(
            GaslessMember::QuoteExpired,
            format!(
                "the signed authorization was valid until {} and it is {} now; nothing was sent to the provider",
                permit.deadline, now_s
            ),
        ));
    }
    let request_id = new_uuid().to_string();
    let request = SubmitRequest::new(signed.clone(), Some(&request_id)).map_err(WithdrawError::InternalError)?;

    // Whatever ends the task from here on, the log says that this submission was sent.
    info!(
        "GasFree submission is being sent: coin {}, from {}, to {}, amount {}, request id {}",
        coin,
        permit.user.to_base58(),
        permit.receiver.to_base58(),
        amount,
        request_id
    );
    let answer = binding.provider().submit(&request).await;
    // The provider's nonce moves with a submission only, and this one has been answered or has failed.
    drop(guard);
    let warn_unknown_outcome = || {
        warn!(
            "GasFree submission with an unknown outcome, the provider may have accepted the transfer: coin {}, \
             from {}, to {}, amount {}, request id {}",
            coin,
            permit.user.to_base58(),
            permit.receiver.to_base58(),
            amount,
            request_id
        )
    };
    let record = match answer {
        Ok(record) => record,
        // The request may have reached the provider, and no usable answer came back.
        Err(
            error @ (GasFreeError::Transport(_)
            | GasFreeError::Timeout(_)
            | GasFreeError::ProviderFailed { .. }
            | GasFreeError::InvalidResponse(_)),
        ) => {
            warn_unknown_outcome();
            return Err(collapse_noted(error, MAYBE_ACCEPTED_TEXT));
        },
        // Nothing was sent, or the provider refused the request.
        Err(error) => return Err(collapse(error)),
    };

    // The provider's id is its own text: it is shown only when it is the UUID a trace request needs.
    let shown_id = sanitize_provider_text(&record.id);
    if let Some(member) = submit_answer_difference(&record, permit) {
        warn_unknown_outcome();
        return Err(gasless_error(
            GaslessMember::InvalidProviderResponse,
            format!(
                "the provider's answer to the submission carries another {member} than the signed authorization; \
                 its trace id is {shown_id}{MAYBE_ACCEPTED_TEXT}"
            ),
        ));
    }
    if Uuid::parse_str(&record.id).is_err() {
        warn_unknown_outcome();
        return Err(gasless_error(
            GaslessMember::InvalidProviderResponse,
            format!(
                "the provider's answer to the submission carries the trace id {shown_id}, which is not a \
                 UUID{MAYBE_ACCEPTED_TEXT}"
            ),
        ));
    }
    Ok(Submitted {
        trace_id: record.id.clone(),
        record,
    })
}

/// The first member of the provider's answer to a submission that is not what `permit` says (D63.9 item 3), by
/// its name in the answer. `version` is compared when the answer carries it.
fn submit_answer_difference(record: &TransferRecord, permit: &PermitTransfer) -> Option<&'static str> {
    if record.nonce != permit.nonce {
        return Some("nonce");
    }
    if record.version.is_some_and(|version| version != PERMIT_VERSION) {
        return Some("version");
    }
    if record.account_address != permit.user {
        return Some("accountAddress");
    }
    if record.target_address != permit.receiver {
        return Some("targetAddress");
    }
    if record.token_address != permit.token {
        return Some("tokenAddress");
    }
    if record.amount != permit.value {
        return Some("amount");
    }
    None
}

/// Follows a submitted transfer until it settles (D63.9), starting with the provider's answer to the submission
/// and asking for its record every `status_poll_interval_ms` of `binding`, at least [`MIN_POLL_INTERVAL_MS`]
/// apart. Each record is read as follows:
///
/// - `state` `FAILED`, or `txnState` `ON_CHAIN_FAILED`: the transfer failed;
/// - else `state` `SUCCEED`: the transfer is settled, and its `txnHash` is returned;
/// - else `state` `CONFIRMING`, or a `txnHash`: the phase `WaitingForGaslessSettlement`;
/// - else (`WAITING`, `INPROGRESS`): the phase `WaitingForGaslessProvider`, with the `state` as the provider
///   writes it.
///
/// A trace request that fails with a failed connection, a timeout, a 5xx or a 429 is logged and the next one is
/// made after the interval. No trace request is made at or after `bound_ms` (Unix milliseconds).
///
/// # Errors
///
/// Each text names the trace id.
///
/// - `Gasless`/`ProviderRejected` for a failed transfer.
/// - `Gasless`/`InvalidProviderResponse` for a settled transfer without a `txnHash`, and for a trace answer that
///   cannot be used.
/// - `Gasless`/`TraceNotFound` when the provider does not know the trace id (D63.9 item 5).
/// - [`WithdrawError::Transport`] when `bound_ms` is reached: the outcome of the transfer is unknown. Also for a
///   trace request the provider refuses with 401 or 403.
/// - [`WithdrawError::InternalError`] for a trace request that could not be formed.
/// - The error of the task's status update, when the task is gone.
pub(crate) async fn track(
    binding: &GasFreeBinding,
    submitted: Submitted,
    bound_ms: u64,
    progress: Option<&WithdrawTaskHandle>,
) -> Result<TransactionHash, WithdrawError> {
    let Submitted { trace_id, mut record } = submitted;
    loop {
        let tx_hash = record.txn_hash.map(TransactionHash::to_hex);
        if record.state == TransferState::Failed || record.txn_state == Some(TransactionState::OnChainFailed) {
            let transaction = match &tx_hash {
                Some(hash) => format!(", transaction {hash}"),
                None => String::new(),
            };
            return Err(gasless_error(
                GaslessMember::ProviderRejected,
                format!(
                    "the provider reports the transfer with the trace id {trace_id} as failed (state {}{transaction})",
                    record.state.wire_name()
                ),
            ));
        }
        if record.state == TransferState::Succeed {
            return record.txn_hash.ok_or_else(|| {
                gasless_error(
                    GaslessMember::InvalidProviderResponse,
                    format!(
                        "the provider reports the transfer with the trace id {trace_id} as settled and names no \
                         transaction"
                    ),
                )
            });
        }
        let phase = if record.state == TransferState::Confirming || tx_hash.is_some() {
            WithdrawInProgressStatus::WaitingForGaslessSettlement {
                trace_id: trace_id.clone(),
                tx_hash,
            }
        } else {
            WithdrawInProgressStatus::WaitingForGaslessProvider {
                trace_id: trace_id.clone(),
                provider_state: record.state.wire_name().to_owned(),
            }
        };
        report_progress(progress, phase)?;
        record = next_record(binding, &trace_id, bound_ms).await?;
    }
}

/// The next record of the transfer `trace_id` the provider answers with: one trace request per poll interval,
/// until one is answered or `bound_ms` is reached. See [`track`].
async fn next_record(binding: &GasFreeBinding, trace_id: &str, bound_ms: u64) -> Result<TransferRecord, WithdrawError> {
    loop {
        let wait_ms = binding
            .status_poll_interval_ms()
            .max(MIN_POLL_INTERVAL_MS)
            .min(bound_ms.saturating_sub(now_ms()))
            .min(u64::from(u32::MAX));
        Timer::sleep_ms(wait_ms as u32).await;
        if now_ms() >= bound_ms {
            return Err(WithdrawError::Transport(format!(
                "the GasFree transfer with the trace id {trace_id} was submitted and the provider has not reported \
                 it as settled by the end of the wait; its outcome is unknown"
            )));
        }
        match binding.provider().trace(trace_id).await {
            Ok(record) => return Ok(record),
            Err(
                error @ (GasFreeError::Transport(_)
                | GasFreeError::Timeout(_)
                | GasFreeError::ProviderFailed { .. }
                | GasFreeError::RateLimited),
            ) => {
                warn!(
                    "GasFree transfer {}: a trace request failed, asking again: {}",
                    trace_id, error
                );
            },
            Err(error) => {
                let note = format!(" (the transfer was submitted; its trace id is {trace_id})");
                return Err(collapse_noted(error, &note));
            },
        }
    }
}

/// [`collapse`] for a failure of a submit or trace request, with `note` at the end of the text.
fn collapse_noted(error: GasFreeError, note: &str) -> WithdrawError {
    let note = |text: &str| format!("{text}{note}");
    match error.rpc_arm() {
        RpcArm::Gasless(member) => gasless_error(member, note(&error.to_string())),
        RpcArm::Transport => {
            warn!("GasFree provider failure: {}", error);
            WithdrawError::Transport(note(PROVIDER_TRANSPORT_TEXT))
        },
        RpcArm::InternalError | RpcArm::NotSufficientBalance => WithdrawError::InternalError(note(&error.to_string())),
    }
}

/// `units` of a token with `decimals` as a decimal amount in token units.
fn token_amount(units: U256, decimals: u8) -> Result<BigDecimal, WithdrawError> {
    u256_to_big_decimal(units, decimals).map_err(|e| WithdrawError::from(e.into_inner()))
}

/// The result of the rail (§63.8 step 5, §63.8A R25 to R28, D63.8 (a)): the shared members of a withdraw result
/// with the payload's members beside them and the gasless fee details, without `tx_hex`.
///
/// `settled` is `None` for the sign-only result: the fee details' `trace_id` is `null` and there is no `tx_hash`.
/// For a transfer the task route followed to its settlement it is the provider's trace id and the hash of the
/// transaction: the fee details carry the first, and `tx_hash` is the second in the form a native TRON withdraw
/// writes it (D63.9 item 6).
#[allow(clippy::too_many_arguments)]
fn rail_result(
    coin: &EthCoin,
    sender: &TronAddress,
    receiver: &TronAddress,
    request: &WithdrawRequest,
    quote: &Quote,
    signed_max_fee: U256,
    payload: GasFreeRelayPayload,
    created_at: u64,
    settled: Option<(&str, TransactionHash)>,
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
        // The provider gives a trace id with its answer to a submission; the sign-only result has none.
        trace_id: settled.map(|(trace_id, _)| trace_id.to_owned()),
    };

    let amount = request.amount.clone();
    // The provider takes its fee from the custody account in the same token.
    let spent_by_me = &amount + &total_token_fee;
    let received_by_me: BigDecimal = if receiver == sender { amount.clone() } else { 0.into() };

    Ok(TransactionDetails {
        // The result carries no transaction of this node: `tx_hex` is omitted from its serialized form, and so
        // is `tx_hash` unless the provider's transaction is known (R25, D63.9).
        tx_hex: Vec::new().into(),
        tx_json: None,
        tx_hash: settled.map_or_else(String::new, |(_, hash)| hash.to_hex()),
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
