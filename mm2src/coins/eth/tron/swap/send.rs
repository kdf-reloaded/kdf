//! The sending side of a TRON V1 swap (CRD ch.21 §21.11B.2, D21.B5, D21.B6, D21.B9, §21.11C): the payment with
//! its token approval, the DEX-fee transfer, the question whether the own payment was sent, and the fee figures
//! of these steps.
//!
//! # Invariants
//!
//! - The secret hash is the 32-byte value of the swap and reaches the contract unchanged (R21.11C.1).
//! - Every contract call is dry-run first. A reverting dry run refuses the call before anything is signed.
//! - A token approval is never above the amount of the payment, and counts only after its receipt reports
//!   success at the coin's confirmation depth.
//! - The transaction a payment returns, in a result or in a broadcast error, is the payment call, never an
//!   approval.
//! - The own-payment question never answers "not sent" from a failed read.
//! - A token that takes a fee on a transfer is refused when a swap starts and before its payment; a failed read
//!   of that setting never counts as "no fee".
//! - Every fee figure is an amount of TRX and is never taken from the traded volume.

use super::contract_abi::{decode_allowance, decode_payments_output, encode_allowance_query, encode_approve,
                          encode_call, encode_payments_query, PaymentState, TronSwapCall};
use super::fee_limit::swap_call_fee_limit;
use super::ids::{require_32_byte_hash, tron_payment_hash, tron_swap_id};
use super::trigger::{dry_run_energy, read_swap_head, send_trigger};
use super::tx::{call_of, TronSwapTx};
use super::TronSwapError;
use crate::eth::tron::api::{AccountListing, TronApiClient, TronApiError};
use crate::eth::tron::balance::trc20_balance_units;
use crate::eth::tron::fee::{estimate_trc20_transfer_fee, estimate_trx_transfer_fee, transaction_bandwidth,
                            TronAccountResources, TronChainPrices, TronFeeError, TronFeeEstimate,
                            TRC20_FEE_LIMIT_MAX_SUN};
use crate::eth::tron::history::signed_transaction_bytes;
use crate::eth::tron::proto::{TaposBlockData, TransactionRaw, DEFAULT_EXPIRATION_SEC};
use crate::eth::tron::sign::sign_transaction_raw;
use crate::eth::tron::tx_builder::{abi_encode_trc20_transfer, build_trigger_smart_contract, build_trx_transfer};
use crate::eth::tron::{TronAddress, TRX_DECIMALS};
use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType};
use crate::{DexFee, MarketCoinOps, MmCoin, TradeFee, TradePreimageError, TransactionErr};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::now_ms;
use ethereum_types::U256;
use kdf_crypto::keccak256;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;
use std::fmt;

/// The energy a native `ethPayment` call is estimated to use when it cannot be dry-run. A Reloaded estimate:
/// the default gas limit of the same call of the EVM V1 swap contract (CRD ch.57 R57.5.2), read as energy
/// units. It prices a fee figure only and never a `fee_limit`.
pub(crate) const TRX_PAYMENT_ENERGY_ESTIMATE: u64 = 65_000;
/// As [`TRX_PAYMENT_ENERGY_ESTIMATE`] for an `erc20Payment` call. A token payment cannot be dry-run while its
/// allowance is short.
pub(crate) const TRC20_PAYMENT_ENERGY_ESTIMATE: u64 = 150_000;
/// The energy a `receiverSpend` call of a native payment is estimated to use. A Reloaded estimate, derived as
/// [`TRX_PAYMENT_ENERGY_ESTIMATE`]: no payment of the counterparty exists when the figure is asked for, so the
/// call cannot be dry-run.
pub(crate) const TRX_SPEND_ENERGY_ESTIMATE: u64 = 65_000;
/// As [`TRX_SPEND_ENERGY_ESTIMATE`] for the spend of a token payment.
pub(crate) const TRC20_SPEND_ENERGY_ESTIMATE: u64 = 150_000;

/// The energy a `senderRefund` call of a native payment is estimated to use. A Reloaded estimate: the default gas
/// limit of the refund call of the EVM V1 swap contract (CRD ch.57 R57.5.2), read as energy units. It is part of
/// the fee to send a payment, so that the sender keeps the TRX to take the payment back, and never prices a
/// `fee_limit`.
pub(crate) const TRX_REFUND_ENERGY_ESTIMATE: u64 = 100_000;
/// As [`TRX_REFUND_ENERGY_ESTIMATE`] for the refund of a token payment.
pub(crate) const TRC20_REFUND_ENERGY_ESTIMATE: u64 = 150_000;

/// The pause between two reads of an approval's receipt, in seconds.
const RECEIPT_POLL_SEC: f64 = if cfg!(test) { 0.02 } else { 3. };
/// The most reads of an approval's receipt: with the pause, ten minutes.
const RECEIPT_POLL_MAX: usize = 200;
/// How long after its expiry an approval that no node knows is still waited for, in milliseconds of chain time.
const EXPIRY_GRACE_MS: i64 = 30_000;
/// The most pages of the account's transaction listing the own-payment question reads.
const OWN_PAYMENT_LISTING_PAGES: usize = 5;

/// Why a sending step of a TRON swap failed.
#[derive(Debug)]
pub(crate) enum TronSendError {
    /// A failure of the shared swap layer. Only this kind can carry a signed transaction, and then it is the
    /// payment or the DEX-fee transaction itself.
    Swap(TronSwapError),
    /// An amount that does not convert to the units of the coin or does not fit the protocol's field.
    Amount(String),
    /// The token takes a fee on a transfer: its rate is not zero and its maximum is not zero, or it has a rate and
    /// no readable maximum. The swap contract accepts a payment only when the receiver's balance grows by the
    /// whole amount, so such a token is not swapped.
    TokenTransferFeeNotZero {
        /// `basisPointsRate()` of the token.
        rate: U256,
        /// `maximumFee()` of the token; `None` when the token has no such getter.
        maximum: Option<U256>,
    },
    /// A DEX-fee form the TRON family does not send.
    DexFee(&'static str),
    /// A token approval that failed. Its transaction is not kept: it is not the payment.
    Approve(String),
    /// The contract holds a payment under the swap id whose stored payment hash is not the one of this swap's
    /// terms.
    OtherPayment,
    /// The contract holds a payment under the swap id, and its transaction could not be found.
    Undetermined(String),
}

impl fmt::Display for TronSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TronSendError::Swap(e) => write!(f, "{e}"),
            TronSendError::Amount(reason) => write!(f, "unusable amount: {reason}"),
            TronSendError::TokenTransferFeeNotZero { rate, maximum } => {
                write!(
                    f,
                    "the token takes a fee on a transfer (rate {rate} basis points, maximum "
                )?;
                match maximum {
                    Some(maximum) => write!(f, "{maximum}")?,
                    None => write!(f, "not readable")?,
                }
                write!(f, "): it cannot be swapped")
            },
            TronSendError::DexFee(reason) => write!(f, "no DEX-fee transaction is sent: {reason}"),
            TronSendError::Approve(reason) => write!(f, "the token approval failed: {reason}"),
            TronSendError::OtherPayment => write!(
                f,
                "the swap id holds another payment: its stored payment hash is not the one of this swap"
            ),
            TronSendError::Undetermined(reason) => {
                write!(f, "Undetermined whether the payment was sent: {reason}")
            },
        }
    }
}

impl std::error::Error for TronSendError {}

impl From<TronSwapError> for TronSendError {
    fn from(e: TronSwapError) -> Self { TronSendError::Swap(e) }
}

impl From<TronSendError> for TransactionErr {
    /// A broadcast that did not end with an acceptance keeps its signed transaction, because a node that did not
    /// answer may still have taken it. Every other failure is a plain error: nothing was broadcast.
    fn from(e: TronSendError) -> Self {
        match e {
            TronSendError::Swap(TronSwapError::Broadcast { tx, error }) => {
                let text = format!("the broadcast of the TRON transaction {:x} failed: {error}", tx.txid());
                TransactionErr::TxRecoverable((*tx).into(), text)
            },
            other => TransactionErr::Plain(other.to_string()),
        }
    }
}

fn tron_api(coin: &EthCoin) -> Result<&TronApiClient, TronSwapError> {
    match (&coin.tron_api, coin.is_tron_family()) {
        (Some(api), true) => Ok(api),
        _ => Err(TronSwapError::NotTronCoin),
    }
}

fn my_address(coin: &EthCoin) -> TronAddress { TronAddress::from_evm_address(coin.my_address) }

/// `amount` of the coin in its smallest units.
fn units_of(coin: &EthCoin, amount: &BigDecimal) -> Result<U256, TronSendError> {
    wei_from_big_decimal(amount, coin.decimals).map_err(|e| TronSendError::Amount(e.to_string()))
}

/// A TRX amount in SUN as the signed 64-bit value the protocol carries.
fn sun_of(units: U256) -> Result<i64, TronSendError> {
    if units > U256::from(i64::MAX as u64) {
        return Err(TronSendError::Amount(
            "the amount exceeds the 64-bit amount range of TRON".to_owned(),
        ));
    }
    Ok(units.low_u64() as i64)
}

// ---------------------------------------------------------------------------------------------------------------
// Token transfer fee
// ---------------------------------------------------------------------------------------------------------------

/// The value of the token's parameterless `uint256` getter with the canonical signature `signature`, read by a
/// constant call; `None` when the token has no such getter (the call reverts, or returns no data).
async fn read_token_setting(
    api: &TronApiClient,
    owner: &TronAddress,
    token: &TronAddress,
    signature: &str,
) -> Result<Option<U256>, TronSwapError> {
    let hash = keccak256(signature.as_bytes());
    let selector = &hash.as_slice()[..4];
    let answer = match api.trigger_constant_dry_run_data(owner, token, selector, 0).await {
        Ok(answer) => answer,
        Err(TronApiError::ContractRevert(_)) => return Ok(None),
        Err(other) => return Err(TronSwapError::Node(other)),
    };
    let unusable = |reason: &str| TronSwapError::Node(TronApiError::InvalidResponse(format!("{signature}: {reason}")));
    let results = answer
        .constant_result
        .ok_or_else(|| unusable("the answer carries no constant result"))?;
    let word = match results.first() {
        Some(word) if !word.is_empty() => hex::decode(word).map_err(|_| unusable("the result is not hex"))?,
        _ => return Ok(None),
    };
    if word.len() != 32 {
        return Err(unusable("the result is not one 32-byte word"));
    }
    Ok(Some(U256::from_big_endian(&word)))
}

/// Refuses a token that takes a fee on a transfer (maintainer decision of 2026-11-22). The swap contract checks
/// that the receiver's balance grew by exactly the amount.
///
/// The token's setting is read with two constant calls, `basisPointsRate()` and `maximumFee()`; nothing is
/// broadcast. The fee of a transfer is `min(amount * rate / 10000, maximum)`, so it is zero when either value
/// is zero. A token without the rate getter has no such setting and passes.
///
/// # Errors
///
/// [`TronSendError::TokenTransferFeeNotZero`] when the rate is not zero and the maximum is not zero or cannot
/// be read from the token; [`TronSendError::Swap`] with [`TronSwapError::Node`] when a read fails or its answer
/// is not usable. A failed read never counts as a fee of zero.
pub(crate) async fn ensure_no_token_transfer_fee(coin: &EthCoin, token: &TronAddress) -> Result<(), TronSendError> {
    let api = tron_api(coin)?;
    let owner = my_address(coin);
    let rate = match read_token_setting(api, &owner, token, "basisPointsRate()").await? {
        Some(rate) if !rate.is_zero() => rate,
        _ => return Ok(()),
    };
    match read_token_setting(api, &owner, token, "maximumFee()").await? {
        Some(maximum) if maximum.is_zero() => Ok(()),
        maximum => Err(TronSendError::TokenTransferFeeNotZero { rate, maximum }),
    }
}

/// [`ensure_no_token_transfer_fee`] for a fee figure of the coin; a TRX coin is not read.
async fn ensure_swappable_token(coin: &EthCoin) -> MmResult<(), TradePreimageError> {
    let EthCoinType::Trc20 { token_addr, .. } = &coin.coin_type else {
        return Ok(());
    };
    let token = TronAddress::from_evm_address(*token_addr);
    ensure_no_token_transfer_fee(coin, &token).await.map_to_mm(|e| match e {
        TronSendError::Swap(e) => preimage_error(e),
        other => TradePreimageError::InternalError(other.to_string()),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Payment
// ---------------------------------------------------------------------------------------------------------------

/// Dry-runs the call, derives its `fee_limit` and sends it.
async fn send_call(
    coin: &EthCoin,
    contract: &TronAddress,
    data: Vec<u8>,
    call_value: i64,
) -> Result<TronSwapTx, TronSwapError> {
    let (_energy, fee_limit_sun) = swap_call_fee_limit(coin, contract, &data, call_value).await?;
    send_trigger(coin, contract, data, call_value, fee_limit_sun).await
}

/// What the token `token` lets `spender` take from the coin's address.
async fn read_allowance(coin: &EthCoin, token: &TronAddress, spender: &TronAddress) -> Result<U256, TronSwapError> {
    let owner = my_address(coin);
    let query = encode_allowance_query(&owner, spender)?;
    let output = constant_read(tron_api(coin)?, &owner, token, &query).await?;
    decode_allowance(&output)
}

/// The return data of the read `data` of `contract`.
async fn constant_read(
    api: &TronApiClient,
    owner: &TronAddress,
    contract: &TronAddress,
    data: &[u8],
) -> Result<Vec<u8>, TronSwapError> {
    let answer = api
        .trigger_constant_read_data(owner, contract, data)
        .await
        .map_err(TronSwapError::Node)?;
    let unusable = || TronSwapError::Node(TronApiError::InvalidResponse("the read returned no data".to_owned()));
    let first = answer
        .constant_result
        .and_then(|results| results.into_iter().next())
        .ok_or_else(unusable)?;
    hex::decode(first).map_err(|_| unusable())
}

/// Sends `approve(spender, amount)` to the token and waits until its receipt reports success at the coin's
/// confirmation depth.
async fn approve_and_wait(
    coin: &EthCoin,
    token: &TronAddress,
    spender: &TronAddress,
    amount: U256,
) -> Result<(), TronSendError> {
    let step = async {
        let data = encode_approve(spender, amount)?;
        let tx = send_call(coin, token, data, 0).await?;
        wait_for_success(coin, &tx).await
    };
    // The transaction of a failed approval is dropped on purpose: a caller must never take it for the payment.
    step.await
        .map_err(|e: TronSwapError| TronSendError::Approve(e.to_string()))
}

/// Waits until the receipt of `tx` reports `SUCCESS` and the transaction is at the coin's confirmation depth.
///
/// # Errors
///
/// [`TronSwapError::Reverted`] with the receipt's result when it is not `SUCCESS`;
/// [`TronSwapError::NotYetFinal`] when the transaction expired without entering a block, or did not reach the
/// depth within the bound of the wait; [`TronSwapError::Node`] for a receipt without an execution result.
async fn wait_for_success(coin: &EthCoin, tx: &TronSwapTx) -> Result<(), TronSwapError> {
    let api = tron_api(coin)?;
    let tx_hash = format!("{:x}", tx.txid());
    let required = coin.required_confirmations();
    for _ in 0..RECEIPT_POLL_MAX {
        // A failed read is not an outcome of the approval: the next round asks again, within the bound.
        let info = api.get_transaction_info_by_id(&tx_hash).await.ok();
        let head = api.get_now_block().await.ok();
        let included = info.and_then(|info| Some((info.block_number?, info.receipt?.result)));
        match (included, head) {
            (Some((block_number, result)), Some(head)) => {
                match result.as_deref() {
                    Some("SUCCESS") => {},
                    Some(other) => {
                        return Err(TronSwapError::Reverted {
                            result: other.to_owned(),
                        })
                    },
                    None => {
                        return Err(TronSwapError::Node(TronApiError::InvalidResponse(
                            "the receipt of a contract call carries no execution result".to_owned(),
                        )))
                    },
                }
                let confirmations = head.block_header.raw_data.number.saturating_sub(block_number) + 1;
                if confirmations >= required {
                    return Ok(());
                }
            },
            (None, Some(head)) => {
                let expired_at = tx.raw().expiration.saturating_add(EXPIRY_GRACE_MS);
                if head.block_header.raw_data.timestamp > expired_at {
                    return Err(TronSwapError::NotYetFinal);
                }
            },
            (_, None) => {},
        }
        Timer::sleep(RECEIPT_POLL_SEC).await;
    }
    Err(TronSwapError::NotYetFinal)
}

/// Sends the V1 payment of `amount` to the swap contract `contract`, locked for `receiver` under `secret_hash`
/// until `time_lock` (CRD ch.21 §21.11B.2, D21.B5, D21.B6).
///
/// TRX is locked by `ethPayment` with the amount as the call value. A token is locked by `erc20Payment`. Before
/// it the allowance of the contract is read: when it is below the amount, the amount is approved, after an
/// approval of zero when the allowance is not zero. The returned transaction is the payment call.
///
/// # Errors
///
/// [`TronSendError::Swap`] with [`TronSwapError::HashWidth`] for a secret hash that is not 32 bytes, with
/// [`TronSwapError::Reverted`] for a reverting dry run, with [`TronSwapError::FeeLimit`] for an energy cost above
/// the highest `fee_limit` (nothing is signed in these cases), with [`TronSwapError::Broadcast`] and the signed
/// payment when its broadcast did not end with an acceptance, and with the other errors of the shared layer;
/// [`TronSendError::Amount`]; [`TronSendError::TokenTransferFeeNotZero`] for a token that takes a fee on a
/// transfer, before any approval; [`TronSendError::Approve`] when an approval could not be sent or did not succeed,
/// in which case no payment is sent.
pub(crate) async fn send_payment(
    coin: &EthCoin,
    contract: &TronAddress,
    receiver: &TronAddress,
    secret_hash: &[u8],
    time_lock: u32,
    amount: &BigDecimal,
) -> Result<TronSwapTx, TronSendError> {
    let secret_hash = require_32_byte_hash(secret_hash)?;
    let id = tron_swap_id(time_lock, &secret_hash);
    let units = units_of(coin, amount)?;
    match &coin.coin_type {
        EthCoinType::Tron => {
            let call_value = sun_of(units)?;
            let data = encode_call(&TronSwapCall::EthPayment {
                id,
                receiver: *receiver,
                secret_hash,
                lock_time: u64::from(time_lock),
            })?;
            Ok(send_call(coin, contract, data, call_value).await?)
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            ensure_no_token_transfer_fee(coin, &token).await?;
            let allowance = read_allowance(coin, &token, contract).await?;
            if allowance < units {
                // D21.B6: some tokens refuse a change from one non-zero allowance to another.
                if !allowance.is_zero() {
                    approve_and_wait(coin, &token, contract, U256::zero()).await?;
                }
                approve_and_wait(coin, &token, contract, units).await?;
            }
            let data = encode_call(&TronSwapCall::Erc20Payment {
                id,
                amount: units,
                token,
                receiver: *receiver,
                secret_hash,
                lock_time: u64::from(time_lock),
            })?;
            Ok(send_call(coin, contract, data, 0).await?)
        },
        EthCoinType::Eth | EthCoinType::Erc20 { .. } => Err(TronSwapError::NotTronCoin.into()),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// DEX fee
// ---------------------------------------------------------------------------------------------------------------

/// Builds a TRX transfer on the reference block of the current head, signs it with the key of the coin's
/// address and broadcasts it.
async fn send_trx_transfer(coin: &EthCoin, to: &TronAddress, amount_sun: i64) -> Result<TronSwapTx, TronSwapError> {
    if coin.signer.local_secret().is_none() {
        return Err(TronSwapError::NoLocalKey);
    }
    let api = tron_api(coin)?;
    // The expiry counts from the head block's time, so the local clock bounds it, node by node, as for a
    // contract call (R21.11C.7).
    let block = read_swap_head(api).await?;
    let tapos = block.to_tapos().map_err(TronSwapError::Node)?;
    let head_time_ms = block.block_header.raw_data.timestamp;
    let local_time_ms = now_ms() as i64;
    let expiration_ms = head_time_ms
        .checked_add(DEFAULT_EXPIRATION_SEC as i64 * 1000)
        .ok_or_else(|| {
            TronSwapError::Node(TronApiError::InvalidResponse(
                "the head block's time leaves no room for an expiry".to_owned(),
            ))
        })?;
    let raw = build_trx_transfer(&my_address(coin), to, amount_sun, &tapos, local_time_ms, expiration_ms);
    let tx = {
        let secret = coin.signer.local_secret().ok_or(TronSwapError::NoLocalKey)?;
        let (_, signature) = sign_transaction_raw(secret, &raw).map_err(TronSwapError::Signing)?;
        TronSwapTx::new(raw, signature)
    };
    let answer = match api.broadcast_hex(&hex::encode(tx.to_bytes())).await {
        Ok(answer) => answer,
        Err(error) => {
            return Err(TronSwapError::Broadcast {
                tx: Box::new(tx),
                error,
            })
        },
    };
    // An accepting node that names another transaction did not accept this one.
    if let Some(named) = answer.txid {
        let named = named.strip_prefix("0x").unwrap_or(&named);
        if hex::decode(named).ok().as_deref() != Some(tx.txid().as_bytes()) {
            return Err(TronSwapError::Broadcast {
                tx: Box::new(tx),
                error: TronApiError::InvalidResponse("the node named another transaction id".to_owned()),
            });
        }
    }
    Ok(tx)
}

/// Sends the DEX fee `dex_fee` of a swap to `fee_address` (CRD ch.21 D21.B9): a TRX `TransferContract`, or a
/// TRC20 `transfer` call with the `fee_limit` of a swap call (R21.11C.7). Nothing is burned.
///
/// # Errors
///
/// [`TronSendError::DexFee`] without any node request for [`DexFee::NoFee`] (no fee is due, so there is no
/// transaction) and for [`DexFee::WithBurn`] (the TRON family has no burn share); [`TronSendError::Amount`];
/// [`TronSendError::Swap`] as for [`send_payment`].
pub(crate) async fn send_dex_fee(
    coin: &EthCoin,
    fee_address: &TronAddress,
    dex_fee: &DexFee,
) -> Result<TronSwapTx, TronSendError> {
    let amount = match dex_fee {
        DexFee::Standard(amount) => amount.to_decimal(),
        DexFee::NoFee => return Err(TronSendError::DexFee("the swap owes no DEX fee")),
        DexFee::WithBurn { .. } => return Err(TronSendError::DexFee("a TRON coin pays no burn share")),
    };
    let units = units_of(coin, &amount)?;
    match &coin.coin_type {
        EthCoinType::Tron => Ok(send_trx_transfer(coin, fee_address, sun_of(units)?).await?),
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            let data = abi_encode_trc20_transfer(fee_address, units);
            Ok(send_call(coin, &token, data, 0).await?)
        },
        EthCoinType::Eth | EthCoinType::Erc20 { .. } => Err(TronSwapError::NotTronCoin.into()),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Own payment
// ---------------------------------------------------------------------------------------------------------------

/// Whether the payment of the swap with `time_lock` and `secret_hash` was sent to `contract` from the coin's
/// address: one read of `payments(id)` at the head.
///
/// `None` when the contract holds no payment under the id. Otherwise the payment transaction, looked up in the
/// confirmed transaction listing of the coin's address: a successful payment call of `contract` for the id,
/// made by the coin's address.
///
/// `terms`, when given, are the receiver and the amount of the payment this swap would send. The stored payment
/// hash is compared with the one of these terms before anything is looked up; another hash is a typed error.
///
/// # Errors
///
/// [`TronSendError::Swap`] for a secret hash that is not 32 bytes and for a failed or unusable read: a failed
/// read never answers `None`. [`TronSendError::Amount`]. [`TronSendError::OtherPayment`] when the contract
/// holds a payment under the id and its stored payment hash is not the one of `terms`; nothing is looked up
/// then. [`TronSendError::Undetermined`] when the contract holds a payment and its
/// transaction is not found: the node list has no node that serves the listing, the listing fails, or the
/// transaction is not among the rows read.
pub(crate) async fn my_payment_sent(
    coin: &EthCoin,
    contract: &TronAddress,
    time_lock: u32,
    secret_hash: &[u8],
    terms: Option<(&TronAddress, &BigDecimal)>,
) -> Result<Option<TronSwapTx>, TronSendError> {
    let secret_hash = require_32_byte_hash(secret_hash)?;
    let id = tron_swap_id(time_lock, &secret_hash);
    let owner = my_address(coin);
    let payment_hash = match terms {
        Some((receiver, amount)) => {
            let token = match &coin.coin_type {
                EthCoinType::Trc20 { token_addr, .. } => Some(TronAddress::from_evm_address(*token_addr)),
                _ => None,
            };
            let units = units_of(coin, amount)?;
            Some(tron_payment_hash(receiver, &owner, &secret_hash, token.as_ref(), units))
        },
        None => None,
    };
    payment_sent_from(tron_api(coin)?, &owner, contract, &id, payment_hash.as_ref()).await
}

/// [`my_payment_sent`] for the payment of the swap `id` from `owner`, asked of `api`. `payment_hash` is the
/// payment hash of the swap's terms, when they are known.
pub(super) async fn payment_sent_from(
    api: &TronApiClient,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
    payment_hash: Option<&[u8; 32]>,
) -> Result<Option<TronSwapTx>, TronSendError> {
    let output = constant_read(api, owner, contract, &encode_payments_query(id)?).await?;
    let record = decode_payments_output(&output)?;
    if record.state == PaymentState::Uninitialized {
        return Ok(None);
    }
    if payment_hash.is_some_and(|expected| *expected != record.payment_hash) {
        return Err(TronSendError::OtherPayment);
    }

    let mut cursor: Option<String> = None;
    for _ in 0..OWN_PAYMENT_LISTING_PAGES {
        let page = api
            .get_account_listing(owner, AccountListing::Transactions, cursor.as_deref())
            .await
            .map_err(|e| {
                TronSendError::Undetermined(format!(
                    "the swap contract reports a payment for this swap, but the transactions of the address \
                     cannot be listed: {e}"
                ))
            })?;
        if let Some(tx) = page
            .data
            .iter()
            .find_map(|row| payment_of_row(row, owner, contract, id))
        {
            return Ok(Some(tx));
        }
        cursor = page.meta.and_then(|meta| meta.fingerprint);
        if cursor.is_none() {
            break;
        }
    }
    Err(TronSendError::Undetermined(
        "the swap contract reports a payment for this swap, but its transaction was not found among the \
         confirmed transactions of the address"
            .to_owned(),
    ))
}

/// The transaction of the listing row `row` when it is a successful payment call of `contract` for the swap
/// `id`, made by `owner`. A row of any other kind, or one that cannot be read, is no candidate.
fn payment_of_row(row: &Json, owner: &TronAddress, contract: &TronAddress, id: &[u8; 32]) -> Option<TronSwapTx> {
    let succeeded = row
        .get("ret")?
        .as_array()?
        .iter()
        .any(|entry| entry.get("contractRet").and_then(Json::as_str) == Some("SUCCESS"));
    if !succeeded {
        return None;
    }
    let raw_data_hex = row.get("raw_data_hex")?.as_str()?;
    let signatures: Vec<String> = row
        .get("signature")?
        .as_array()?
        .iter()
        .filter_map(|signature| signature.as_str().map(str::to_owned))
        .collect();
    let bytes = signed_transaction_bytes(raw_data_hex, &signatures).ok()?;
    let tx = TronSwapTx::from_bytes(&bytes).ok()?;
    if tx.trigger().ok()?.owner != *owner {
        return None;
    }
    let (called, _, call) = call_of(&tx).ok()?;
    let is_payment = matches!(
        call,
        TronSwapCall::EthPayment { .. } | TronSwapCall::Erc20Payment { .. }
    );
    (called == *contract && is_payment && call.id() == id).then_some(tx)
}

// ---------------------------------------------------------------------------------------------------------------
// Fee figures
// ---------------------------------------------------------------------------------------------------------------

async fn read_prices(api: &TronApiClient) -> Result<TronChainPrices, TronSwapError> {
    let parameters = api.get_chain_parameters().await.map_err(TronSwapError::Node)?;
    TronChainPrices::from_chain_parameters(&parameters)
        .map_err(|e| TronSwapError::Node(TronApiError::InvalidResponse(e.to_string())))
}

async fn read_resources(api: &TronApiClient, owner: &TronAddress) -> Result<TronAccountResources, TronSwapError> {
    let resources = api.get_account_resource(owner).await.map_err(TronSwapError::Node)?;
    serde_json::from_value(resources).map_err(|e| {
        TronSwapError::Node(TronApiError::InvalidResponse(format!(
            "getaccountresource answer does not decode: {e}"
        )))
    })
}

/// The reference-block members of a transaction that is built only to be measured. The size of a transaction
/// does not depend on their values: they are two and eight bytes for every block.
fn sizing_tapos() -> TaposBlockData {
    TaposBlockData {
        ref_block_bytes: vec![0; 2],
        ref_block_hash: vec![0; 8],
    }
}

/// The bandwidth of a call of `contract` with `data` and `call_value` from the coin's address, with the largest
/// `fee_limit` a call can carry.
fn call_bandwidth(coin: &EthCoin, contract: &TronAddress, data: Vec<u8>, call_value: i64) -> u64 {
    let now = now_ms() as i64;
    let raw: TransactionRaw = build_trigger_smart_contract(
        &my_address(coin),
        contract,
        data,
        call_value,
        &sizing_tapos(),
        now,
        now,
        TRC20_FEE_LIMIT_MAX_SUN as i64,
    );
    transaction_bandwidth(&raw)
}

/// The trade-preimage error of a failed fee figure: a node that does not answer, or refuses an estimate, is a
/// transport error (CRD ch.51 R95 item 5).
fn preimage_error(e: TronSwapError) -> TradePreimageError {
    match e {
        TronSwapError::Node(_) | TronSwapError::Undetermined(_) | TronSwapError::Reverted { .. } => {
            TradePreimageError::Transport(e.to_string())
        },
        other => TradePreimageError::InternalError(other.to_string()),
    }
}

fn fee_error(e: TronFeeError) -> TradePreimageError {
    match e {
        TronFeeError::UnusableChainParameter(_) => TradePreimageError::Transport(e.to_string()),
        other => TradePreimageError::InternalError(other.to_string()),
    }
}

/// The fee descriptor of `estimate`: an amount of TRX, named by the ticker of the platform coin, never paid
/// from the traded volume (CRD ch.51 R93).
fn trade_fee(coin: &EthCoin, estimate: &TronFeeEstimate) -> MmResult<TradeFee, TradePreimageError> {
    let fee_sun = estimate.total_fee_sun().map_to_mm(fee_error)?;
    trade_fee_of_sun(coin, fee_sun)
}

fn trade_fee_of_sun(coin: &EthCoin, fee_sun: u64) -> MmResult<TradeFee, TradePreimageError> {
    let amount = u256_to_big_decimal(U256::from(fee_sun), TRX_DECIMALS).mm_err(TradePreimageError::from)?;
    Ok(TradeFee {
        coin: coin.platform_ticker().to_owned(),
        amount: amount.into(),
        paid_from_trading_vol: false,
    })
}

/// The fee of contract calls that together use `energy` and `bandwidth`, after the resources of the coin's
/// address: the energy its staked energy does not cover at the energy price, and the bandwidth at the bandwidth
/// price unless one of its bandwidth quotas covers all of it.
async fn calls_fee(coin: &EthCoin, energy: u64, bandwidth: u64) -> MmResult<TradeFee, TradePreimageError> {
    let api = tron_api(coin).map_to_mm(preimage_error)?;
    let prices = read_prices(api).await.map_to_mm(preimage_error)?;
    let resources = read_resources(api, &my_address(coin)).await.map_to_mm(preimage_error)?;
    let estimate = estimate_trc20_transfer_fee(&prices, &resources, bandwidth, energy).map_to_mm(fee_error)?;
    trade_fee(coin, &estimate)
}

/// The swap contract the coin was activated with, for a fee figure.
fn own_swap_contract(coin: &EthCoin) -> MmResult<TronAddress, TradePreimageError> {
    if !coin.is_tron_family() || coin.tron_api.is_none() {
        return MmError::err(TradePreimageError::InternalError(
            TronSwapError::NotTronCoin.to_string(),
        ));
    }
    if coin.swap_contract_address.is_zero() {
        return MmError::err(TradePreimageError::InternalError(
            "the coin has no swap contract: it cannot trade".to_owned(),
        ));
    }
    Ok(TronAddress::from_evm_address(coin.swap_contract_address))
}

/// A payment call of the coin with stand-in swap values, for a dry run and for its size: `amount` locked for
/// the coin's own address under a swap id no payment uses. The energy and the size of a payment do not depend
/// on these values.
fn sample_payment(coin: &EthCoin, amount: U256) -> MmResult<(Vec<u8>, i64), TradePreimageError> {
    let lock_time = (now_ms() / 1000) as u32;
    let secret_hash = [0x5a; 32];
    let id = tron_swap_id(lock_time, &secret_hash);
    let receiver = my_address(coin);
    match &coin.coin_type {
        EthCoinType::Tron => {
            let call_value = sun_of(amount).map_to_mm(|e| TradePreimageError::InternalError(e.to_string()))?;
            let data = encode_call(&TronSwapCall::EthPayment {
                id,
                receiver,
                secret_hash,
                lock_time: u64::from(lock_time),
            })
            .map_to_mm(preimage_error)?;
            Ok((data, call_value))
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let data = encode_call(&TronSwapCall::Erc20Payment {
                id,
                amount,
                token: TronAddress::from_evm_address(*token_addr),
                receiver,
                secret_hash,
                lock_time: u64::from(lock_time),
            })
            .map_to_mm(preimage_error)?;
            Ok((data, 0))
        },
        EthCoinType::Eth | EthCoinType::Erc20 { .. } => MmError::err(preimage_error(TronSwapError::NotTronCoin)),
    }
}

/// The fixed energy estimate of a payment of the coin.
fn payment_energy_estimate(coin: &EthCoin) -> u64 {
    match coin.coin_type {
        EthCoinType::Trc20 { .. } => TRC20_PAYMENT_ENERGY_ESTIMATE,
        _ => TRX_PAYMENT_ENERGY_ESTIMATE,
    }
}

/// The fee to send the payment of `amount` (CRD ch.51 R93, ch.21 D21.B5): the energy of the payment's dry run
/// and the bandwidth of its transaction, priced after the resources of the coin's address. The fee of the
/// refund is part of it, at the fixed energy estimate of its kind and the bandwidth of its transaction, as the
/// EVM family reserves the refund call with the payment: a sender must be able to take the payment back.
///
/// For a token whose allowance is below `amount`, the energy and the bandwidth of the approval are added, and
/// those of the approval of zero when the allowance is not zero (the mirror of CRD ch.57 R57.5.3b). A payment
/// that cannot be simulated (its allowance is short, or its dry run reverts) is priced with the fixed estimate
/// of its kind.
///
/// # Errors
///
/// [`TradePreimageError::Transport`] when a node read fails or the dry run of an approval is refused;
/// [`TradePreimageError::InternalError`] for a coin without a swap contract, for a token that takes a fee on a
/// transfer ([`ensure_no_token_transfer_fee`]) and for a figure that does not convert.
pub(crate) async fn sender_trade_fee(coin: &EthCoin, amount: &BigDecimal) -> MmResult<TradeFee, TradePreimageError> {
    let contract = own_swap_contract(coin)?;
    ensure_swappable_token(coin).await?;
    let units = wei_from_big_decimal(amount, coin.decimals).mm_err(TradePreimageError::from)?;
    let (payment, call_value) = sample_payment(coin, units)?;
    let mut energy = 0u64;
    let mut bandwidth = call_bandwidth(coin, &contract, payment.clone(), call_value);
    let mut can_simulate = true;

    if let EthCoinType::Trc20 { token_addr, .. } = &coin.coin_type {
        let token = TronAddress::from_evm_address(*token_addr);
        let allowance = read_allowance(coin, &token, &contract)
            .await
            .map_to_mm(preimage_error)?;
        if allowance < units {
            can_simulate = false;
            let mut approvals = vec![units];
            if !allowance.is_zero() {
                approvals.push(U256::zero());
            }
            for approved in approvals {
                let data = encode_approve(&contract, approved).map_to_mm(preimage_error)?;
                let used = dry_run_energy(coin, &token, &data, 0).await.map_to_mm(preimage_error)?;
                energy = energy.saturating_add(used);
                bandwidth = bandwidth.saturating_add(call_bandwidth(coin, &token, data, 0));
            }
        }
    }

    let payment_energy = if can_simulate {
        match dry_run_energy(coin, &contract, &payment, call_value).await {
            Ok(used) => used,
            // The contract refuses the stand-in payment (an amount above the balance, for example). The balance
            // checks of the caller report that; the figure is the fixed estimate.
            Err(TronSwapError::Reverted { .. }) => payment_energy_estimate(coin),
            Err(other) => return MmError::err(preimage_error(other)),
        }
    } else {
        payment_energy_estimate(coin)
    };
    let (refund, refund_energy) = sample_refund(coin).map_to_mm(preimage_error)?;
    let bandwidth = bandwidth.saturating_add(call_bandwidth(coin, &contract, refund, 0));
    let energy = energy.saturating_add(payment_energy).saturating_add(refund_energy);
    calls_fee(coin, energy, bandwidth).await
}

/// A `senderRefund` call of the coin with stand-in values of the size of the real ones, for the size of its
/// transaction, and the fixed energy estimate of its kind.
fn sample_refund(coin: &EthCoin) -> Result<(Vec<u8>, u64), TronSwapError> {
    let (token, energy) = match &coin.coin_type {
        EthCoinType::Trc20 { token_addr, .. } => (
            Some(TronAddress::from_evm_address(*token_addr)),
            TRC20_REFUND_ENERGY_ESTIMATE,
        ),
        _ => (None, TRX_REFUND_ENERGY_ESTIMATE),
    };
    let data = encode_call(&TronSwapCall::SenderRefund {
        id: [0x5a; 32],
        amount: U256::MAX,
        secret_hash: [0x5a; 32],
        token,
        receiver: my_address(coin),
    })?;
    Ok((data, energy))
}

/// The legacy trade-fee figure of the coin: the fee of a payment at the fixed energy estimate of its kind. No
/// amount is known here, so nothing is dry-run.
///
/// # Errors
///
/// As [`sender_trade_fee`].
pub(crate) async fn flat_trade_fee(coin: &EthCoin) -> MmResult<TradeFee, TradePreimageError> {
    let contract = own_swap_contract(coin)?;
    let (payment, call_value) = sample_payment(coin, U256::one())?;
    let bandwidth = call_bandwidth(coin, &contract, payment, call_value);
    calls_fee(coin, payment_energy_estimate(coin), bandwidth).await
}

/// The fee to spend the counterparty's payment (CRD ch.51 R93): a `receiverSpend` call at the fixed energy
/// estimate of its kind and the bandwidth of its transaction.
///
/// # Errors
///
/// As [`sender_trade_fee`].
pub(crate) async fn receiver_trade_fee(coin: &EthCoin) -> MmResult<TradeFee, TradePreimageError> {
    let contract = own_swap_contract(coin)?;
    ensure_swappable_token(coin).await?;
    let (token, energy) = match &coin.coin_type {
        EthCoinType::Trc20 { token_addr, .. } => (
            Some(TronAddress::from_evm_address(*token_addr)),
            TRC20_SPEND_ENERGY_ESTIMATE,
        ),
        _ => (None, TRX_SPEND_ENERGY_ESTIMATE),
    };
    // Stand-in values of the size of the real ones: an amount and a secret that fill their words.
    let data = encode_call(&TronSwapCall::ReceiverSpend {
        id: [0x5a; 32],
        amount: U256::MAX,
        secret: [0x5a; 32],
        token,
        sender: my_address(coin),
    })
    .map_to_mm(preimage_error)?;
    let bandwidth = call_bandwidth(coin, &contract, data, 0);
    calls_fee(coin, energy, bandwidth).await
}

/// The fee to send the DEX fee of `units` (the coin's smallest units) to `fee_address` (CRD ch.51 R93, ch.21
/// D21.B9): for TRX the fee of the transfer, for a token the energy of the transfer's dry run and the bandwidth
/// of its transaction. A fee of zero is sent by nothing, so its figure is zero.
///
/// # Errors
///
/// [`TradePreimageError::NotSufficientBalance`] naming the token when the transfer's dry run reverts and the
/// token balance is below the fee; [`TradePreimageError::Transport`] when a node read fails or the dry run is
/// refused for another reason; [`TradePreimageError::InternalError`] otherwise.
pub(crate) async fn dex_fee_send_fee(
    coin: &EthCoin,
    fee_address: &TronAddress,
    units: U256,
) -> MmResult<TradeFee, TradePreimageError> {
    if units.is_zero() {
        return trade_fee_of_sun(coin, 0);
    }
    let api = tron_api(coin).map_to_mm(preimage_error)?;
    let owner = my_address(coin);
    match &coin.coin_type {
        EthCoinType::Tron => {
            let amount_sun = sun_of(units).map_to_mm(|e| TradePreimageError::InternalError(e.to_string()))?;
            let now = now_ms() as i64;
            let raw = build_trx_transfer(&owner, fee_address, amount_sun, &sizing_tapos(), now, now);
            let recipient_exists = api
                .get_account(fee_address)
                .await
                .map_to_mm(|e| preimage_error(TronSwapError::Node(e)))?
                .is_some();
            let prices = read_prices(api).await.map_to_mm(preimage_error)?;
            let resources = read_resources(api, &owner).await.map_to_mm(preimage_error)?;
            let estimate =
                estimate_trx_transfer_fee(&prices, &resources, transaction_bandwidth(&raw), recipient_exists)
                    .map_to_mm(fee_error)?;
            trade_fee(coin, &estimate)
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            let data = abi_encode_trc20_transfer(fee_address, units);
            let energy = match dry_run_energy(coin, &token, &data, 0).await {
                Ok(energy) => energy,
                Err(TronSwapError::Reverted { result }) => {
                    let balance = trc20_balance_units(api, &token, &owner)
                        .await
                        .map_to_mm(|e| TradePreimageError::Transport(e.to_string()))?;
                    if balance < units {
                        return MmError::err(TradePreimageError::NotSufficientBalance {
                            coin: coin.ticker().to_owned(),
                            available: u256_to_big_decimal(balance, coin.decimals).mm_err(TradePreimageError::from)?,
                            required: u256_to_big_decimal(units, coin.decimals).mm_err(TradePreimageError::from)?,
                        });
                    }
                    return MmError::err(preimage_error(TronSwapError::Reverted { result }));
                },
                Err(other) => return MmError::err(preimage_error(other)),
            };
            let bandwidth = call_bandwidth(coin, &token, data, 0);
            calls_fee(coin, energy, bandwidth).await
        },
        EthCoinType::Eth | EthCoinType::Erc20 { .. } => MmError::err(preimage_error(TronSwapError::NotTronCoin)),
    }
}
