//! TRON withdraw pipeline (CRD ch.21 §21.10, R21.10B.7, D21.10B.1).
//!
//! The `withdraw` RPC produces a signed serialized transaction that the caller
//! can later submit via `send_raw_transaction`. For TRON the flow is:
//!
//! 1. Refuse a `fee` descriptor and a `memo`, before any node is asked.
//! 2. Parse the destination (Base58Check or hex) to a [`TronAddress`].
//! 3. Read the sender's balance, then the inputs of the estimate: the current block (TAPOS data and the time the
//!    expiry counts from), the sender's resources and the chain's prices. For TRC20, dry-run the transfer for its
//!    energy and derive the transaction's `fee_limit` from it.
//! 4. Build the unsigned [`proto::TransactionRaw`] with [`tx_builder`](super::tx_builder), estimate its fee from
//!    its size and settle the amount (`max` for TRX is the balance less the fee of the transaction that carries
//!    that amount).
//! 5. SHA-256 hash the canonical protobuf serialization and sign with
//!    secp256k1 to produce a 65-byte R||S||V signature.
//! 6. Wrap raw + signature into a [`proto::Transaction`] and emit the
//!    hex-encoded protobuf as `tx_hex`. The txid is the hash.
//!
//! A request may select the fee rail (CRD ch.63 §63.3.4, §63.8): `native` (the flow above, also when the member is
//! absent), `gasless` (the rail of [`gasfree::withdraw`](super::gasfree::withdraw), which needs no TRX) or `auto`
//! (the flow above first, the gasless rail only when the flow above cannot be funded). The gasless rail signs an
//! off-chain authorization. For the direct method that is the whole withdraw; for a task
//! ([`withdraw_tron_with_progress`]) the rail also submits the authorization to the provider and follows the
//! transfer until it settles (CRD ch.63 D63.9).
//!
//! # Invariants
//!
//! - A sender without a local private key is refused before any node or provider is asked (CRD ch.63 R22).
//! - The rail that ran is visible in the result: the native rail answers the `Tron` fee details, the gasless rail
//!   the `TronGasless` ones (CRD ch.63 R24). Under `gasless` the native rail runs only with `fallback_to_native`.
//! - No fee figure is assumed. A resource, parameter or dry-run read that fails, fails the withdraw.
//! - Every balance and resource read is for the sender's address: the activated address, or the HD address a
//!   `from` member selected.
//! - A TRX transaction is built only when its amount plus its estimated fee fits the balance, and a TRC20
//!   transaction only when the sender's TRX balance covers its estimated fee.
//! - A TRC20 transfer whose dry run reverts is never signed.

use super::api::{TronApiClient, TronApiError};
use super::balance::{trc20_balance_units, trx_balance_sun};
use super::fee::{estimate_trc20_transfer_fee, estimate_trx_transfer_fee, transaction_bandwidth, trc20_fee_limit_sun,
                 TronAccountResources, TronChainPrices, TronFeeError, TronFeeEstimate, TronTxFeeDetails};
use super::gasfree::failure::GaslessMember;
use super::gasfree::withdraw::{self as gasless_rail, FailureClass, GaslessTerms, RailEligibility, RailFailure};
use super::proto::{self, TaposBlockData, DEFAULT_EXPIRATION_SEC};
use super::sign::sign_transaction_raw;
use super::tx_builder::{abi_encode_trc20_transfer, build_trc20_transfer, build_trx_transfer};
use super::{TronAddress, TRX_DECIMALS};

use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType, EvmWithdrawSender};
use crate::rpc_command::init_withdraw::WithdrawTaskHandle;
use crate::{BytesJson, TransactionDetails, WithdrawError, WithdrawFeeMethod, WithdrawRequest, WithdrawResult};

use bigdecimal::BigDecimal;
use common::log::warn;
use common::now_ms;
use ethereum_types::{H256, U256};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use prost::Message as _;
use serde_json::Value as Json;
use std::sync::atomic::{AtomicBool, Ordering};

/// How many times the amount of a TRX `max` withdraw is recomputed from the fee of the transaction that carries
/// it. The fee depends on the amount only through the length of its encoding, so it settles within two rounds or
/// alternates between two neighboring figures; see [`settle_max_amount`].
const MAX_AMOUNT_ROUNDS: usize = 3;

/// The top-level MM2.json boolean that lets a refusal of the controller check (CRD ch.63 R29) count as a
/// deterministic unavailability for `fallback_to_native`. Absent is `false`.
pub(crate) const GUARD_ALLOWS_FALLBACK_KEY: &str = "tron_gasfree_guard_allows_fallback";

/// Whether the warning about a non-boolean [`GUARD_ALLOWS_FALLBACK_KEY`] has been logged.
static GUARD_SWITCH_WARNED: AtomicBool = AtomicBool::new(false);

/// The value of [`GUARD_ALLOWS_FALLBACK_KEY`] in the configuration `conf`. Only the JSON value `true` switches it
/// on. A value that is not a boolean counts as `false`; the first time one is read, one warning line says so.
fn guard_allows_fallback_in(conf: &Json) -> bool {
    match &conf[GUARD_ALLOWS_FALLBACK_KEY] {
        Json::Bool(allowed) => *allowed,
        Json::Null => false,
        _ => {
            if !GUARD_SWITCH_WARNED.swap(true, Ordering::Relaxed) {
                warn!(
                    "MM2.json: '{}' is not a boolean and counts as false",
                    GUARD_ALLOWS_FALLBACK_KEY
                );
            }
            false
        },
    }
}

/// [`guard_allows_fallback_in`] the configuration of the node that runs `coin`.
fn guard_allows_fallback(coin: &EthCoin) -> MmResult<bool, WithdrawError> {
    let ctx = MmArc::from_weak(&coin.ctx)
        .or_mm_err(|| WithdrawError::InternalError("MM context has been dropped".to_owned()))?;
    Ok(guard_allows_fallback_in(&ctx.conf))
}

/// Whether `error` of the native rail is a funding failure, the only kind after which `auto` turns to the gasless
/// rail (CRD ch.63 §63.8 step 1, R16): too little of the coin, too little TRX for the fee, or an amount below the
/// minimum.
fn is_funding_failure(error: &WithdrawError) -> bool {
    matches!(
        error,
        WithdrawError::NotSufficientBalance { .. }
            | WithdrawError::NotSufficientPlatformBalanceForFee { .. }
            | WithdrawError::AmountTooLow { .. }
    )
}

/// Entry point for TRX / TRC20 withdrawals. `sender` is the address the transaction is sent from and the key that
/// signs it: the activated address, or the HD address a `from` member selected (CRD ch.21 R21.10B.4, ch.47).
///
/// # Errors
///
/// Before a node is asked: [`WithdrawError::UnsupportedError`] for a sender without a local private key (CRD ch.63
/// R22); [`WithdrawError::InvalidFeePolicy`] for any `fee` descriptor (the TRON fee is always estimated from the
/// account's bandwidth and energy, CRD ch.21 R-W3; the `WithdrawFee::Tron` form exists only so that it can be refused
/// here, CRD ch.21 R-WP6), for `gasless` options without `fee_method` `gasless` or `auto`, for a `gasless.max_fee` that
/// is not a decimal amount and for a `gasless.deadline_seconds` of zero (CRD ch.63 §63.9.4); and
/// [`WithdrawError::UnsupportedError`] for a `memo` (CRD ch.21 R21.10B.7, ch.49 R49.27a) and for `max` with
/// `fee_method` `gasless` (CRD ch.63 R14).
///
/// From the amounts (CRD ch.21 R21.10B.7): [`WithdrawError::ZeroBalanceToWithdrawMax`],
/// [`WithdrawError::AmountTooLow`], [`WithdrawError::NotSufficientBalance`] and, for a TRC20 transfer whose
/// sender holds less TRX than the fee, [`WithdrawError::NotSufficientPlatformBalanceForFee`].
///
/// From the estimate: [`WithdrawError::Transport`] for every failed node read, a dry run without a positive
/// energy figure included; [`WithdrawError::InternalError`] when a TRX transfer creates the recipient's account
/// and the node reports no fee for that; [`WithdrawError::UnsupportedError`] when the energy of a TRC20 transfer
/// costs more than the highest `fee_limit` (CRD ch.21 D21.10B.1), and when the dry run of a TRC20 transfer
/// reverts: the token contract refuses the transfer, and nothing is signed (CRD ch.21 R21.10B.13 and its finding).
///
/// From the gasless rail: the errors of [`gasless_rail::GaslessRail::run`], and `Gasless`/`Unavailable` for a
/// coin the rail does not serve (CRD ch.63 §63.9). This entry point makes no submit and no trace request, so it
/// raises none of the errors of the submission.
pub(crate) async fn withdraw_tron(coin: EthCoin, req: WithdrawRequest, sender: EvmWithdrawSender) -> WithdrawResult {
    withdraw_tron_with_progress(coin, req, sender, None).await
}

/// [`withdraw_tron`] for the task that `progress` is the handle of: the gasless rail reports its phases to it (CRD
/// ch.63 §63.3.6) and, with a handle, submits the signed authorization to the provider and follows the transfer
/// until it settles (CRD ch.63 §63.8B, D63.9).
///
/// # Errors
///
/// Those of [`withdraw_tron`]. With a handle, the gasless rail also raises (CRD ch.63 D63.9):
///
/// - `Gasless`/`QuoteExpired` when the signed deadline has passed before the submit request; nothing is sent;
/// - `Gasless`/`ProviderRejected` when the provider refuses the submission, and when it reports the transfer as
///   failed;
/// - `Gasless`/`InvalidProviderResponse` when the answer to the submission differs from what was signed or names
///   no usable trace id, when a trace answer cannot be used, and for a settled transfer without a transaction
///   hash;
/// - `Gasless`/`TraceNotFound` when the provider does not know the trace id;
/// - [`WithdrawError::Transport`] when the provider cannot be reached for the submission, and when the transfer
///   has not settled 120 seconds after the signed deadline: its outcome is then unknown.
///
/// Every error after the submission names the provider's trace id in its text. None of them leads to the native
/// rail.
pub(crate) async fn withdraw_tron_with_progress(
    coin: EthCoin,
    req: WithdrawRequest,
    sender: EvmWithdrawSender,
    progress: Option<&WithdrawTaskHandle>,
) -> WithdrawResult {
    // CRD ch.63 R22: no rail runs, and nothing is read, for a signing policy that holds no local key (a hardware
    // wallet, WalletConnect, a browser-extension wallet).
    if sender.local_secret(&coin).is_none() {
        return MmError::err(WithdrawError::UnsupportedError(
            "A TRON withdraw needs a local private key: it is not supported under a hardware-wallet, WalletConnect \
             or browser-extension signing policy"
                .to_owned(),
        ));
    }
    if let Some(fee) = &req.fee {
        return MmError::err(WithdrawError::InvalidFeePolicy(format!(
            "TRON withdraw takes no 'fee' descriptor, its fee is estimated from the account's bandwidth and \
             energy; found {:?}",
            fee
        )));
    }
    if req.memo.is_some() {
        return MmError::err(WithdrawError::UnsupportedError(
            "A TRON withdraw takes no 'memo'".to_owned(),
        ));
    }
    // The shared dispatch has made this check already (CRD ch.63 R21); a caller that comes another way gets the
    // same answer.
    req.check_fee_rail(true)?;
    // Absent is `native` (CRD ch.63 §63.3.4).
    let fee_method = req.fee_method.unwrap_or(WithdrawFeeMethod::Native);
    // The request-shape rejections of CRD ch.63 §63.9.4 come before any node or provider request.
    let terms = match fee_method {
        WithdrawFeeMethod::Native => None,
        WithdrawFeeMethod::Gasless | WithdrawFeeMethod::Auto => Some(GaslessTerms::from_request(&req)?),
    };
    if fee_method == WithdrawFeeMethod::Gasless && req.max {
        return MmError::err(WithdrawError::UnsupportedError(
            "A gasless withdraw takes no 'max': the provider's fee is paid from the same token balance".to_owned(),
        ));
    }

    let api = coin
        .tron_api
        .clone()
        .ok_or_else(|| WithdrawError::InternalError("TRON API client missing on this coin".to_owned()))?;

    let signer = sender;
    let sender = TronAddress::from_evm_address(signer.address());
    let receiver = parse_tron_address(&req.to)?;

    if receiver.to_evm_address().is_zero() {
        return MmError::err(WithdrawError::InvalidAddress(
            "Receiver cannot be zero address".to_owned(),
        ));
    }

    let terms = match (fee_method, terms) {
        (WithdrawFeeMethod::Gasless | WithdrawFeeMethod::Auto, Some(terms)) => terms,
        _ => return withdraw_native(&coin, &api, &signer, sender, receiver, &req).await,
    };

    if fee_method == WithdrawFeeMethod::Auto {
        // CRD ch.63 R16: the native rail first. Only a funding failure of a request without `max` turns to the
        // gasless rail; if that rail does not apply to the coin, the native error stands.
        let native_error = match withdraw_native(&coin, &api, &signer, sender, receiver, &req).await {
            Ok(details) => return Ok(details),
            Err(error) => error,
        };
        if req.max || !is_funding_failure(native_error.get_inner()) {
            return Err(native_error);
        }
        let RailEligibility::Applicable(rail) = gasless_rail::resolve(&coin).await else {
            return Err(native_error);
        };
        // The rail was attempted: its error is the answer.
        return rail
            .run(&coin, &api, &signer, &sender, &receiver, &req, &terms, progress)
            .await
            .map_err(|failure| MmError::new(failure.error));
    }

    // `gasless`: the rail is required.
    let failure = match gasless_rail::resolve(&coin).await {
        RailEligibility::Applicable(rail) => {
            match rail
                .run(&coin, &api, &signer, &sender, &receiver, &req, &terms, progress)
                .await
            {
                Ok(details) => return Ok(details),
                Err(failure) => failure,
            }
        },
        // The rail is not configured for the coin: a deterministic unavailability (CRD ch.63 §63.8 step 6).
        RailEligibility::NotApplicable(reason) => RailFailure {
            error: gasless_rail::gasless_error(GaslessMember::Unavailable, format!("{}: {}", coin.ticker, reason)),
            class: FailureClass::Deterministic,
        },
    };
    // CRD ch.63 §63.8 step 6, R24: the native rail replaces the gasless one only with the request's consent and
    // only after a deterministic unavailability. The refusal of the controller check (R29) is one only when the
    // operator's switch says so.
    let falls_back = terms.fallback_to_native
        && match failure.class {
            FailureClass::Deterministic => true,
            FailureClass::Guard => guard_allows_fallback(&coin)?,
            FailureClass::Other => false,
        };
    if !falls_back {
        return MmError::err(failure.error);
    }
    withdraw_native(&coin, &api, &signer, sender, receiver, &req).await
}

/// The native rail: the TRX or TRC20 transfer whose fee is paid in TRX (CRD ch.21 R21.10B.7).
async fn withdraw_native(
    coin: &EthCoin,
    api: &TronApiClient,
    signer: &EvmWithdrawSender,
    sender: TronAddress,
    receiver: TronAddress,
    req: &WithdrawRequest,
) -> WithdrawResult {
    match &coin.coin_type {
        EthCoinType::Tron => withdraw_native_trx(coin.clone(), api.clone(), signer, sender, receiver, req).await,
        EthCoinType::Trc20 { platform, token_addr } => {
            let token = Trc20Token {
                contract: TronAddress::from_evm_address(*token_addr),
                platform: platform.clone(),
            };
            withdraw_trc20_token(coin.clone(), api.clone(), signer, sender, receiver, token, req).await
        },
        _ => MmError::err(WithdrawError::InternalError(
            "withdraw_tron invoked on non-TRON coin".to_owned(),
        )),
    }
}

/// The TRC20 token a withdraw sends: its contract, and the ticker of the platform coin its fee is paid in.
struct Trc20Token {
    contract: TronAddress,
    platform: String,
}

/// What every TRON withdraw reads from the node before it builds a transaction (CRD ch.21 R21.10B.7).
struct WithdrawInputs {
    /// The reference block of the transaction.
    tapos: TaposBlockData,
    /// The transaction's own timestamp: the local time of building.
    timestamp_ms: i64,
    /// The transaction's expiry, counted from the reference block's time.
    expiration_ms: i64,
    prices: TronChainPrices,
    /// The resources of the sender.
    resources: TronAccountResources,
}

/// Reads the current block, the resources of `sender` and the chain's prices. Nothing is assumed: each failed
/// read fails the withdraw.
async fn read_withdraw_inputs(
    api: &TronApiClient,
    sender: &TronAddress,
    expiration_seconds: Option<u64>,
) -> MmResult<WithdrawInputs, WithdrawError> {
    let block = api.get_now_block().await.map_to_mm(node_error)?;
    let tapos = block.to_tapos().map_to_mm(node_error)?;
    let expiration_ms = expiration_ms(block.block_header.raw_data.timestamp, expiration_seconds)?;

    let resources = api.get_account_resource(sender).await.map_to_mm(node_error)?;
    let resources: TronAccountResources = serde_json::from_value(resources)
        .map_to_mm(|e| WithdrawError::Transport(format!("getaccountresource answer does not decode: {e}")))?;

    let parameters = api.get_chain_parameters().await.map_to_mm(node_error)?;
    let prices = TronChainPrices::from_chain_parameters(&parameters).map_to_mm(fee_error)?;

    Ok(WithdrawInputs {
        tapos,
        timestamp_ms: now_ms() as i64,
        expiration_ms,
        prices,
        resources,
    })
}

/// The expiry of a transaction whose reference block has the time `block_ms`: `expiration_seconds` later, 60
/// seconds when the request does not say (CRD ch.21 R21.10B.7). There is no upper bound but the range of the
/// protocol's 64-bit millisecond field.
fn expiration_ms(block_ms: i64, expiration_seconds: Option<u64>) -> MmResult<i64, WithdrawError> {
    // The default is the rule's own figure for a request without the member, not a stand-in for a failed read.
    let seconds = expiration_seconds.unwrap_or(DEFAULT_EXPIRATION_SEC);
    seconds
        .checked_mul(1000)
        .and_then(|lifetime_ms| i64::try_from(lifetime_ms).ok())
        .and_then(|lifetime_ms| block_ms.checked_add(lifetime_ms))
        .or_mm_err(|| {
            WithdrawError::UnsupportedError(format!(
                "'expiration_seconds' of {seconds} is beyond the range of a TRON transaction's expiry"
            ))
        })
}

async fn withdraw_native_trx(
    coin: EthCoin,
    api: TronApiClient,
    signer: &EvmWithdrawSender,
    sender: TronAddress,
    receiver: TronAddress,
    req: &WithdrawRequest,
) -> WithdrawResult {
    let (balance, balance_dec) = fetch_trx_balance(&api, &sender).await?;
    let balance_sun = u256_to_u64(balance)?;

    // `None` is `max`: the amount follows from the fee, below.
    let requested_sun = if req.max {
        if balance_sun == 0 {
            return MmError::err(WithdrawError::ZeroBalanceToWithdrawMax);
        }
        None
    } else {
        let amount = wei_from_big_decimal(&req.amount, TRX_DECIMALS).mm_err(Into::into)?;
        if amount > balance {
            return MmError::err(WithdrawError::NotSufficientBalance {
                coin: coin.ticker.clone(),
                available: balance_dec,
                required: req.amount.clone(),
            });
        }
        Some(u256_to_u64(amount)?)
    };

    let inputs = read_withdraw_inputs(&api, &sender, req.expiration_seconds).await?;
    // Only the documented empty answer is an account the chain has not created; a failed read is an error.
    let recipient_exists = api.get_account(&receiver).await.map_to_mm(node_error)?.is_some();

    // The transaction that carries `amount_sun`, its estimate and its whole fee.
    let transfer_of = |amount_sun: u64| -> MmResult<(proto::TransactionRaw, TronFeeEstimate, u64), WithdrawError> {
        let amount = i64::try_from(amount_sun)
            .map_to_mm(|_| WithdrawError::InternalError("Amount exceeds TRON i64 protocol limit".to_owned()))?;
        let raw = build_trx_transfer(
            &sender,
            &receiver,
            amount,
            &inputs.tapos,
            inputs.timestamp_ms,
            inputs.expiration_ms,
        );
        let estimate = estimate_trx_transfer_fee(
            &inputs.prices,
            &inputs.resources,
            transaction_bandwidth(&raw),
            recipient_exists,
        )
        .map_to_mm(fee_error)?;
        let fee_sun = estimate.total_fee_sun().map_to_mm(fee_error)?;
        Ok((raw, estimate, fee_sun))
    };

    let amount_sun = match requested_sun {
        Some(amount_sun) => amount_sun,
        None => {
            let settled = settle_max_amount(balance_sun, |amount_sun| {
                transfer_of(amount_sun).map(|(_, _, fee_sun)| fee_sun)
            })?;
            match settled {
                MaxAmount::Settled { amount_sun } => amount_sun,
                MaxAmount::BelowFee { fee_sun } => {
                    return MmError::err(WithdrawError::AmountTooLow {
                        amount: 0.into(),
                        threshold: sun_to_trx(fee_sun)?,
                    })
                },
            }
        },
    };

    let (raw, estimate, fee_sun) = transfer_of(amount_sun)?;
    let required_sun = amount_sun
        .checked_add(fee_sun)
        .or_mm_err(|| fee_error(TronFeeError::Overflow))?;
    if required_sun > balance_sun {
        return MmError::err(WithdrawError::NotSufficientBalance {
            coin: coin.ticker.clone(),
            available: balance_dec,
            required: sun_to_trx(required_sun)?,
        });
    }

    let amount_dec = if req.max {
        sun_to_trx(amount_sun)?
    } else {
        req.amount.clone()
    };
    let fee_details = TronTxFeeDetails::new(&coin.ticker, &estimate, fee_sun).mm_err(Into::into)?;
    // The fee is paid in the coin that is sent.
    let spent_by_me = &amount_dec + &fee_details.total_fee;

    let (hash, tx_bytes) = sign_transaction(&coin, signer, raw)?;
    Ok(transaction_details(
        &coin,
        &sender,
        &receiver,
        hash,
        tx_bytes,
        amount_dec,
        spent_by_me,
        fee_details,
    ))
}

async fn withdraw_trc20_token(
    coin: EthCoin,
    api: TronApiClient,
    signer: &EvmWithdrawSender,
    sender: TronAddress,
    receiver: TronAddress,
    token: Trc20Token,
    req: &WithdrawRequest,
) -> WithdrawResult {
    let decimals = coin.decimals;

    let (balance_units, balance_dec) = fetch_trc20_balance(&api, &token.contract, &sender, decimals).await?;

    // `max` sends the whole token balance: the fee is paid in TRX. A token amount is a 256-bit value from the
    // request to the call data; only TRX amounts, in SUN, are held in 64 bits.
    let (amount_units, amount_dec) = if req.max {
        (balance_units, balance_dec.clone())
    } else {
        let amount = wei_from_big_decimal(&req.amount, decimals).mm_err(Into::into)?;
        (amount, req.amount.clone())
    };

    if amount_units > balance_units {
        return MmError::err(WithdrawError::NotSufficientBalance {
            coin: coin.ticker.clone(),
            available: balance_dec,
            required: amount_dec,
        });
    }

    let inputs = read_withdraw_inputs(&api, &sender, req.expiration_seconds).await?;

    let energy = dry_run_trc20_energy(&api, &sender, &token.contract, &receiver, amount_units).await?;
    let fee_limit_sun = trc20_fee_limit_sun(&inputs.prices, energy).map_to_mm(fee_error)?;
    let fee_limit = i64::try_from(fee_limit_sun).map_to_mm(|_| fee_error(TronFeeError::Overflow))?;
    let raw = build_trc20_transfer(
        &sender,
        &token.contract,
        &receiver,
        amount_units,
        &inputs.tapos,
        inputs.timestamp_ms,
        inputs.expiration_ms,
        fee_limit,
    );

    // A TRC20 transfer never pays for the creation of the recipient's account, so the recipient is not read.
    let estimate = estimate_trc20_transfer_fee(&inputs.prices, &inputs.resources, transaction_bandwidth(&raw), energy)
        .map_to_mm(fee_error)?;
    let fee_sun = estimate.total_fee_sun().map_to_mm(fee_error)?;

    let (trx_balance, trx_balance_dec) = fetch_trx_balance(&api, &sender).await?;
    if trx_balance < U256::from(fee_sun) {
        return MmError::err(WithdrawError::NotSufficientPlatformBalanceForFee {
            coin: token.platform,
            available: trx_balance_dec,
            required: sun_to_trx(fee_sun)?,
        });
    }

    let fee_details = TronTxFeeDetails::new(&token.platform, &estimate, fee_sun).mm_err(Into::into)?;
    // The fee is paid in the platform coin, so it is no part of what is spent of the token.
    let spent_by_me = amount_dec.clone();

    let (hash, tx_bytes) = sign_transaction(&coin, signer, raw)?;
    Ok(transaction_details(
        &coin,
        &sender,
        &receiver,
        hash,
        tx_bytes,
        amount_dec,
        spent_by_me,
        fee_details,
    ))
}

/// What a TRX `max` withdraw can send.
#[derive(Debug, PartialEq)]
enum MaxAmount {
    /// The amount to send. It and the fee of the transaction that carries it fit the balance.
    Settled { amount_sun: u64 },
    /// The balance does not exceed the fee.
    BelowFee { fee_sun: u64 },
}

/// The amount of a TRX `max` withdraw: the balance less the fee of the transaction that carries that amount
/// (CRD ch.21 R21.10B.7). `fee_of` is the whole fee of a transfer of the given amount.
///
/// The fee can depend on the amount, because a larger amount can take one more byte. The first fee is the one of
/// the whole balance, the largest there is. Each round takes the balance less the current fee as the amount and
/// reads the fee of that amount. The amount is settled when that fee is the current fee. Next to a length
/// boundary the two figures can alternate: the smaller fee belongs to an amount whose remainder would need the
/// larger one. The amount then stays at the last one that fits with its own fee, which leaves the price of at
/// most the alternating bytes unsent, and never asks for more than the balance.
fn settle_max_amount<E>(balance_sun: u64, mut fee_of: impl FnMut(u64) -> Result<u64, E>) -> Result<MaxAmount, E> {
    let mut fee_sun = fee_of(balance_sun)?;
    let mut fits = None;
    for _ in 0..MAX_AMOUNT_ROUNDS {
        if balance_sun <= fee_sun {
            break;
        }
        let amount_sun = balance_sun - fee_sun;
        let fee_of_amount = fee_of(amount_sun)?;
        if fee_of_amount == fee_sun {
            return Ok(MaxAmount::Settled { amount_sun });
        }
        if fee_of_amount > fee_sun {
            // This amount and its own fee are above the balance.
            break;
        }
        // The fee went down: this amount and its own fee are below the balance.
        fits = Some(amount_sun);
        fee_sun = fee_of_amount;
    }
    Ok(match fits {
        Some(amount_sun) => MaxAmount::Settled { amount_sun },
        None => MaxAmount::BelowFee { fee_sun },
    })
}

/// Signs `raw` with the sender's key. Returns the transaction hash and the serialized signed transaction.
fn sign_transaction(
    coin: &EthCoin,
    signer: &EvmWithdrawSender,
    raw: proto::TransactionRaw,
) -> MmResult<(H256, Vec<u8>), WithdrawError> {
    let secret = signer
        .local_secret(coin)
        .ok_or_else(|| WithdrawError::InternalError("TRON requires a local private key".to_string()))?;
    let (hash, sig) = sign_transaction_raw(secret, &raw)
        .map_err(|e| WithdrawError::InternalError(format!("TRON signing failed: {e}")))?;
    let tx = proto::Transaction {
        raw_data: Some(raw),
        signature: vec![sig],
    };
    Ok((hash, tx.encode_to_vec()))
}

/// The withdraw result of a signed transfer of `amount_dec` (CRD ch.21 R21.10B.6, R21.10B.7).
#[allow(clippy::too_many_arguments)]
fn transaction_details(
    coin: &EthCoin,
    sender: &TronAddress,
    receiver: &TronAddress,
    hash: H256,
    tx_bytes: Vec<u8>,
    amount_dec: BigDecimal,
    spent_by_me: BigDecimal,
    fee_details: TronTxFeeDetails,
) -> TransactionDetails {
    let my_address_str = sender.to_base58();
    let receiver_str = receiver.to_base58();
    let received_by_me: BigDecimal = if receiver_str == my_address_str {
        amount_dec.clone()
    } else {
        0.into()
    };

    TransactionDetails {
        unsigned: None,
        tx_json: None,
        tx_hex: BytesJson::from(tx_bytes),
        tx_hash: format!("{:x}", hash),
        from: vec![my_address_str],
        to: vec![receiver_str],
        total_amount: amount_dec,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: Some(fee_details.into()),
        coin: coin.ticker.clone(),
        internal_id: vec![].into(),
        kmd_rewards: None,
        transaction_type: Default::default(),
        memo: None,
    }
}

fn parse_tron_address(s: &str) -> MmResult<TronAddress, WithdrawError> {
    TronAddress::parse(s).map_to_mm(|e| WithdrawError::InvalidAddress(e.to_string()))
}

async fn fetch_trx_balance(api: &TronApiClient, address: &TronAddress) -> MmResult<(U256, BigDecimal), WithdrawError> {
    let sun = trx_balance_sun(api, address).await.map_to_mm(WithdrawError::from)?;
    let dec = u256_to_big_decimal(sun, TRX_DECIMALS).mm_err(Into::into)?;
    Ok((sun, dec))
}

async fn fetch_trc20_balance(
    api: &TronApiClient,
    contract: &TronAddress,
    owner: &TronAddress,
    decimals: u8,
) -> MmResult<(U256, BigDecimal), WithdrawError> {
    let units = trc20_balance_units(api, contract, owner)
        .await
        .map_to_mm(WithdrawError::from)?;
    let dec = u256_to_big_decimal(units, decimals).mm_err(Into::into)?;
    Ok((units, dec))
}

/// The energy the dry run of `transfer(receiver, amount_units)` from `sender` uses (CRD ch.21 R21.10B.7).
///
/// # Errors
///
/// [`WithdrawError::UnsupportedError`] when the dry run reverts (CRD ch.21 R21.10B.13 and its finding): the token
/// contract refuses this transfer, whichever node is asked, so no other node is asked and the caller signs
/// nothing. The text carries the node's revert message, or the name of the simulated result.
///
/// [`WithdrawError::Transport`] when the read fails or the answer has no positive `energy_used`
/// (CRD ch.21 R21.10B.3). No figure stands in for a missing one.
async fn dry_run_trc20_energy(
    api: &TronApiClient,
    sender: &TronAddress,
    contract: &TronAddress,
    receiver: &TronAddress,
    amount_units: U256,
) -> MmResult<u64, WithdrawError> {
    let data = abi_encode_trc20_transfer(receiver, amount_units);
    // Strip the 4-byte selector: triggerconstantcontract wants parameters only.
    let param_hex = hex::encode(&data[4..]);
    let resp = api
        .trigger_constant_dry_run(sender, contract, "transfer(address,uint256)", &param_hex)
        .await
        .map_to_mm(dry_run_error)?;
    match resp.energy_used {
        Some(energy) if energy > 0 => Ok(energy),
        _ => MmError::err(WithdrawError::Transport(
            "the dry run of the TRC20 transfer reports no positive energy_used".to_owned(),
        )),
    }
}

/// The most characters of a node's revert reason a withdraw error shows.
const REVERT_REASON_MAX_CHARS: usize = 200;

/// The withdraw error of a failed energy dry run. A revert is the token contract's refusal of the transfer: a
/// request the caller has to change (CRD ch.21 R21.10B.13 and its finding, ch.49 R49.27a). The reason is node
/// text: the node client has removed the node's URL from it, and it is cut to [`REVERT_REASON_MAX_CHARS`] here.
/// Every other failure is a failed node read.
fn dry_run_error(e: TronApiError) -> WithdrawError {
    match e {
        TronApiError::ContractRevert(reason) => {
            let mut shown: String = reason.chars().take(REVERT_REASON_MAX_CHARS).collect();
            if shown.len() < reason.len() {
                shown.push_str("...");
            }
            WithdrawError::UnsupportedError(format!(
                "the token contract refused this transfer (its dry run reverted): {shown}"
            ))
        },
        other => node_error(other),
    }
}

/// A failed node read of the withdraw: every node failure, final or not, is a transport error
/// (CRD ch.21 R21.10B.3).
fn node_error(e: TronApiError) -> WithdrawError { WithdrawError::Transport(e.to_string()) }

/// The withdraw error of a failed estimate (CRD ch.21 R21.10B.7, D21.10B.1).
fn fee_error(e: TronFeeError) -> WithdrawError {
    match e {
        // A node answer the estimate cannot use.
        TronFeeError::UnusableChainParameter(_) => WithdrawError::Transport(e.to_string()),
        // Refused before anything is signed: nothing can be broadcast.
        TronFeeError::EnergyCostAboveMaximum { .. } => WithdrawError::UnsupportedError(e.to_string()),
        TronFeeError::NoAccountCreationFee | TronFeeError::NoCreateAccountFee | TronFeeError::Overflow => {
            WithdrawError::InternalError(e.to_string())
        },
    }
}

/// `sun` as a decimal amount of TRX.
fn sun_to_trx(sun: u64) -> MmResult<BigDecimal, WithdrawError> {
    u256_to_big_decimal(U256::from(sun), TRX_DECIMALS).mm_err(Into::into)
}

/// A TRX amount in SUN as the 64-bit value the protocol carries. Token amounts never pass through here.
fn u256_to_u64(v: U256) -> MmResult<u64, WithdrawError> {
    if v > U256::from(u64::MAX) {
        return MmError::err(WithdrawError::InternalError(
            "Amount exceeds the 64-bit amount range of TRON".to_owned(),
        ));
    }
    Ok(v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_base58() {
        let s = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";
        assert!(parse_tron_address(s).is_ok());
    }

    #[test]
    fn parse_accepts_0x41_hex() {
        let raw = format!("0x41{}", "0".repeat(40));
        assert!(parse_tron_address(&raw).is_ok());
    }

    #[test]
    fn parse_rejects_short_hex() {
        let bad = "0x4128ba"; // way too short
        assert!(parse_tron_address(bad).is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_tron_address("not-an-address").is_err());
    }

    #[test]
    fn u256_to_u64_rejects_overflow() {
        let huge = U256::from(u64::MAX) + 1;
        assert!(u256_to_u64(huge).is_err());
        assert_eq!(
            u256_to_u64(U256::from(u64::MAX)).map_err(|e| e.to_string()),
            Ok(u64::MAX)
        );
    }

    /// A fee of 1000 SUN per byte of a transfer whose size grows by one byte at an amount of `boundary`.
    fn stepped_fee(boundary: u64) -> impl FnMut(u64) -> Result<u64, String> {
        move |amount| Ok(if amount >= boundary { 269_000 } else { 268_000 })
    }

    #[test]
    fn a_max_amount_with_a_constant_fee_is_the_balance_less_the_fee() {
        let settled = settle_max_amount(1_000_000, |_| Ok::<_, String>(267_000));
        assert_eq!(settled, Ok(MaxAmount::Settled { amount_sun: 733_000 }));
        // No fee at all: the whole balance.
        let free = settle_max_amount(1_000_000, |_| Ok::<_, String>(0));
        assert_eq!(free, Ok(MaxAmount::Settled { amount_sun: 1_000_000 }));
    }

    /// The balance is one byte longer than the amount that is left of it: the fee settles at the smaller figure
    /// and the amount plus that fee is the whole balance.
    #[test]
    fn a_max_amount_settles_when_the_fee_depends_on_the_amount() {
        let boundary = 1u64 << 28;
        let balance = 268_500_000;
        // The fee of the whole balance is 269 000; 268 231 000 is below the boundary, fee 268 000; the balance
        // less 268 000 is 268 232 000, still below the boundary, same fee.
        let settled = settle_max_amount(balance, stepped_fee(boundary));
        assert_eq!(
            settled,
            Ok(MaxAmount::Settled {
                amount_sun: 268_232_000
            })
        );
    }

    /// Next to the boundary the two fees alternate: the amount stays at the one that fits with its own fee.
    #[test]
    fn a_max_amount_next_to_a_length_boundary_never_exceeds_the_balance() {
        let boundary = 1u64 << 28;
        let balance = boundary + 268_500;
        // The balance less 269 000 is below the boundary (fee 268 000); the balance less 268 000 is above it
        // (fee 269 000) and would overdraw by 1000.
        let settled = settle_max_amount(balance, stepped_fee(boundary));
        let amount = balance - 269_000;
        assert_eq!(settled, Ok(MaxAmount::Settled { amount_sun: amount }));
        assert_eq!(amount + 268_000, balance - 1000);
    }

    #[test]
    fn a_max_amount_of_a_balance_not_above_the_fee_is_below_fee() {
        for balance in [1, 266_999, 267_000] {
            let settled = settle_max_amount(balance, |_| Ok::<_, String>(267_000));
            assert_eq!(settled, Ok(MaxAmount::BelowFee { fee_sun: 267_000 }));
        }
        let above = settle_max_amount(267_001, |_| Ok::<_, String>(267_000));
        assert_eq!(above, Ok(MaxAmount::Settled { amount_sun: 1 }));
    }

    #[test]
    fn a_failed_fee_read_fails_the_max_amount() {
        let failed = settle_max_amount(1_000_000, |_| Err::<u64, _>("no fee".to_owned()));
        assert_eq!(failed, Err("no fee".to_owned()));
    }

    #[test]
    fn the_expiry_counts_from_the_block_time_with_a_default_of_60_seconds() {
        let at = |seconds| expiration_ms(1_700_000_000_000, seconds).map_err(|e| e.into_inner().to_string());
        assert_eq!(at(None), Ok(1_700_000_060_000));
        assert_eq!(at(Some(0)), Ok(1_700_000_000_000));
        assert_eq!(at(Some(86_400)), Ok(1_700_086_400_000));
        assert!(at(Some(u64::MAX)).is_err());
        assert!(at(Some(i64::MAX as u64 / 1000)).is_err());
    }

    #[test]
    fn a_reverting_dry_run_maps_to_the_contracts_refusal_with_a_bounded_reason() {
        match dry_run_error(TronApiError::ContractRevert("REVERT opcode executed".to_owned())) {
            WithdrawError::UnsupportedError(text) => assert_eq!(
                text,
                "the token contract refused this transfer (its dry run reverted): REVERT opcode executed"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
        // A long reason is cut at 200 characters, on a character boundary.
        match dry_run_error(TronApiError::ContractRevert("\u{e9}".repeat(500))) {
            WithdrawError::UnsupportedError(text) => {
                let reason = text.rsplit(": ").next().unwrap_or_default();
                assert_eq!(reason, format!("{}...", "\u{e9}".repeat(200)));
            },
            other => panic!("unexpected error: {other:?}"),
        }
        // Exactly 200 characters are shown whole.
        match dry_run_error(TronApiError::ContractRevert("x".repeat(200))) {
            WithdrawError::UnsupportedError(text) => assert!(text.ends_with(&"x".repeat(200)), "{text}"),
            other => panic!("unexpected error: {other:?}"),
        }
        // Any other failure of the dry run is a failed node read.
        assert!(matches!(
            dry_run_error(TronApiError::Timeout("cutoff".to_owned())),
            WithdrawError::Transport(_)
        ));
    }

    #[test]
    fn a_failed_estimate_maps_to_the_withdraw_error_of_its_class() {
        assert!(matches!(
            fee_error(TronFeeError::UnusableChainParameter("getEnergyFee")),
            WithdrawError::Transport(_)
        ));
        assert!(matches!(
            fee_error(TronFeeError::NoAccountCreationFee),
            WithdrawError::InternalError(_)
        ));
        let above = fee_error(TronFeeError::EnergyCostAboveMaximum {
            estimate_sun: 100_001_000,
            maximum_sun: 100_000_000,
        });
        match above {
            WithdrawError::UnsupportedError(text) => {
                assert!(text.contains("100001000") && text.contains("100000000"), "{text}")
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
