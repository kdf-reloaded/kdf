//! The watch side of a TRON V1 swap (CRD ch.21 D21.B8, R21.B5, §21.11B.2; ch.51 R88, R91, R98, R100): validating
//! the counterparty's payment and fee, spending and refunding our own side, the refund gate, and finding the
//! transaction that consumed a payment.
//!
//! - `evidence` (private): the solidified reads and the agreement of two nodes, or of the evidence node alone.
//! - `discovery` (private): the transaction that spent or refunded a payment.
//!
//! # Invariants
//!
//! - Bytes received from the peer give only a transaction id. The transaction, its receipt and `payments(id)` are
//!   read from the node; the decoded call is used for field checks and error names, and never decides.
//! - The deciding evidence of a payment is: a successful receipt at the required depth, the selected contract of
//!   the swap (ch.51 R98), the agreed `payments(id)` being `PaymentSent` and its stored hash equal to the hash of
//!   the expected values.
//! - A read the node cannot prove itself (the payment state, the refund gate's chain time) rests on two distinct
//!   nodes in agreement, or on the evidence node alone (ch.51 R100). Disagreement and silence are errors that
//!   make the step wait: no refund and no claim follows.
//! - A spend or refund of ours counts only after its receipt says SUCCESS at the required depth.
//! - No error and no log line carries a secret.

pub(super) mod discovery;
pub(super) mod evidence;

use self::discovery::{find_consumer, Caps, Kind, DEFAULT_CAPS};
use self::evidence::{agreed_chain_time, agreed_claim_read, agreed_payment, agreed_state, record_of};
use super::send::TronSendError;
use super::trigger::tron_api;
use super::{call_of, encode_call, encode_payments_query, require_32_byte_hash, tron_payment_hash, tron_swap_id,
            PaymentState, TronSwapCall, TronSwapError, TronSwapEvent, TronSwapTx};
use crate::eth::tron::api::{TronApiClient, TronApiError};
use crate::eth::tron::balance::{trx_balance_sun, TronBalanceError};
use crate::eth::tron::fee::TRC20_FEE_LIMIT_MAX_SUN;
use crate::eth::tron::proto::{Transaction as ProtoTransaction, TransactionRaw, TransferContract,
                              TRANSFER_CONTRACT_TYPE_URL};
use crate::eth::tron::TronAddress;
use crate::eth::{addr_from_raw_pubkey, wei_from_big_decimal, EthCoin, EthCoinType, TryToAddress};
use crate::{BytesJson, CanRefundHtlc, FoundSwapTxSpend, MmCoin};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::now_ms;
use ethereum_types::U256;
use prost::Message;
use std::future::Future;
use std::sync::atomic::Ordering;

/// How long and how often a step polls.
#[derive(Clone, Copy, Debug)]
pub(super) struct Waits {
    pub poll_s: f64,
    pub window_ms: u64,
}

/// The block time of the chain, in milliseconds.
const BLOCK_TIME_MS: i64 = 3_000;

/// The wait of a payment or fee validation for the data to reach the node and to solidify.
pub(super) const VALIDATION_WAITS: Waits = Waits {
    poll_s: 5.0,
    window_ms: 180_000,
};
/// The wait of a spend or refund of ours for its receipt at the required depth.
pub(super) const OWN_STEP_WAITS: Waits = Waits {
    poll_s: 5.0,
    window_ms: 600_000,
};

fn my_address(coin: &EthCoin) -> TronAddress { TronAddress::from_evm_address(coin.my_address) }

/// The swap contract the swap negotiated, from the swap operation's input (ch.51 R98): never the coin's own
/// primary field.
fn selected_contract(selected: &Option<BytesJson>) -> Result<TronAddress, TronSwapError> {
    selected
        .try_to_address()
        .map(TronAddress::from_evm_address)
        .map_err(|_| TronSwapError::Mismatch { field: "swap contract" })
}

/// A failure of the transaction's own form or of its decoding: the transaction is not one this step reads.
pub(super) fn is_foreign(error: &TronSwapError) -> bool {
    matches!(
        error,
        TronSwapError::Transaction(_)
            | TronSwapError::NotAContractCall
            | TronSwapError::WrongSelector
            | TronSwapError::Abi(_)
    )
}

/// Failures that a later poll may cure.
fn is_retryable(error: &TronSwapError) -> bool {
    matches!(
        error,
        TronSwapError::Node(_) | TronSwapError::Undetermined(_) | TronSwapError::NotYetFinal
    )
}

/// Runs `step` until it answers `Some`, it fails with an error a later poll cannot cure, or the window ends (then
/// the last curable error, or [`TronSwapError::NotYetFinal`]).
async fn poll<T, F, Fut>(waits: Waits, mut step: F) -> Result<T, TronSwapError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, TronSwapError>>,
{
    let deadline = now_ms().saturating_add(waits.window_ms);
    let mut last: Option<TronSwapError> = None;
    loop {
        match step().await {
            Ok(Some(done)) => return Ok(done),
            Ok(None) => {},
            Err(e) if is_retryable(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
        if now_ms() >= deadline {
            return Err(last.unwrap_or(TronSwapError::NotYetFinal));
        }
        Timer::sleep(waits.poll_s).await;
    }
}

/// The transaction `txid_hex` as the node keeps it.
///
/// # Errors
///
/// [`TronSwapError::Node`] when it cannot be read; the errors of [`TronSwapTx::from_bytes`] when what the node
/// holds is not a transaction of the form a swap exchanges.
pub(super) async fn fetch_swap_tx(api: &TronApiClient, txid_hex: &str) -> Result<TronSwapTx, TronSwapError> {
    let answer = api.get_transaction_by_id(txid_hex).await.map_err(TronSwapError::Node)?;
    let bad = |reason: &str| TronSwapError::Transaction(reason.to_owned());
    let raw_hex = answer.raw_data_hex.ok_or_else(|| bad("the node holds no raw body"))?;
    let raw_bytes = hex::decode(raw_hex).map_err(|_| bad("the raw body is not hex"))?;
    let raw = TransactionRaw::decode(raw_bytes.as_slice()).map_err(|_| bad("the raw body does not decode"))?;
    let signature = answer
        .signature
        .iter()
        .map(hex::decode)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| bad("a signature is not hex"))?;
    let bytes = ProtoTransaction {
        raw_data: Some(raw),
        signature,
    }
    .encode_to_vec();
    TronSwapTx::from_bytes(&bytes)
}

/// The id, in hex, that peer-given transaction bytes name. Nothing else of the bytes is used.
fn txid_of_bytes(bytes: &[u8]) -> Result<String, TronSwapError> {
    Ok(format!("{:x}", TronSwapTx::from_bytes(bytes)?.txid()))
}

/// The block of a transaction whose receipt is a success and at depth `confirmations`; `Ok(None)` while it is
/// not in a block or not deep enough. `contract_call` says whether an execution result is required.
///
/// # Errors
///
/// [`TronSwapError::Reverted`] for a receipt that is not a success.
async fn receipt_block(
    api: &TronApiClient,
    txid: &str,
    contract_call: bool,
    confirmations: u64,
) -> Result<Option<u64>, TronSwapError> {
    let info = api
        .get_transaction_info_by_id(txid)
        .await
        .map_err(TronSwapError::Node)?;
    let Some(block) = info.block_number else {
        return Ok(None);
    };
    match info.receipt.as_ref().and_then(|receipt| receipt.result.as_deref()) {
        Some("SUCCESS") => {},
        Some(other) => {
            return Err(TronSwapError::Reverted {
                result: other.to_owned(),
            })
        },
        None if contract_call => {
            return Err(TronSwapError::Reverted {
                result: "the receipt carries no execution result".to_owned(),
            })
        },
        None => {},
    }
    let head = api
        .get_now_block()
        .await
        .map_err(TronSwapError::Node)?
        .block_header
        .raw_data
        .number;
    Ok((head.saturating_sub(block) + 1 >= confirmations).then_some(block))
}

/// Waits for the receipt of `txid` to be a success at depth `confirmations` (R21.B5) and returns its block.
pub(super) async fn wait_for_receipt(
    api: &TronApiClient,
    txid: &str,
    contract_call: bool,
    confirmations: u64,
    waits: Waits,
) -> Result<u64, TronSwapError> {
    poll(waits, || async move {
        receipt_block(api, txid, contract_call, confirmations).await
    })
    .await
}

/// What a payment call locked: the facts of the transaction the node holds.
#[derive(Clone, Debug)]
struct PaymentFacts {
    id: [u8; 32],
    amount: U256,
    token: Option<TronAddress>,
    sender: TronAddress,
    receiver: TronAddress,
    secret_hash: [u8; 32],
    lock_time: u64,
    call_value: i64,
}

fn mismatch(field: &'static str) -> TronSwapError { TronSwapError::Mismatch { field } }

/// The facts of a payment transaction, which must call `selected` (ch.51 R98).
fn facts_of(tx: &TronSwapTx, selected: &TronAddress) -> Result<PaymentFacts, TronSwapError> {
    let sender = tx.trigger()?.owner;
    let (contract, call_value, call) = call_of(tx)?;
    if &contract != selected {
        return Err(mismatch("swap contract"));
    }
    let value = u64::try_from(call_value).map_err(|_| mismatch("call value"))?;
    match call {
        TronSwapCall::EthPayment {
            id,
            receiver,
            secret_hash,
            lock_time,
        } => Ok(PaymentFacts {
            id,
            amount: U256::from(value),
            token: None,
            sender,
            receiver,
            secret_hash,
            lock_time,
            call_value,
        }),
        TronSwapCall::Erc20Payment {
            id,
            amount,
            token,
            receiver,
            secret_hash,
            lock_time,
        } => Ok(PaymentFacts {
            id,
            amount,
            token: Some(token),
            sender,
            receiver,
            secret_hash,
            lock_time,
            call_value,
        }),
        _ => Err(mismatch("method")),
    }
}

/// The token and the amount in the coin's smallest unit that a payment of `amount` of `coin` carries.
fn expected_asset(coin: &EthCoin, amount: &BigDecimal) -> Result<(Option<TronAddress>, U256), TronSwapError> {
    let units = wei_from_big_decimal(amount, coin.decimals).map_err(|_| mismatch("amount"))?;
    let token = match &coin.coin_type {
        EthCoinType::Tron => None,
        EthCoinType::Trc20 { token_addr, .. } => Some(TronAddress::from_evm_address(*token_addr)),
        _ => return Err(TronSwapError::NotTronCoin),
    };
    Ok((token, units))
}

fn address_of_pubkey(pubkey: &[u8]) -> Result<TronAddress, TronSwapError> {
    addr_from_raw_pubkey(pubkey)
        .map(TronAddress::from_evm_address)
        .map_err(|_| mismatch("counterparty key"))
}

// ---------------------------------------------------------------------------------------------------------------
// Validation (A2, A3)
// ---------------------------------------------------------------------------------------------------------------

/// What a payment validation checks the counterparty's payment against.
pub struct PaymentCheck<'a> {
    /// The payment transaction as the peer sent it: only its id is read.
    pub payment_tx: &'a [u8],
    pub time_lock: u32,
    /// The public key of the counterparty, the payment's sender.
    pub sender_pub: &'a [u8],
    pub secret_hash: &'a [u8],
    pub amount: &'a BigDecimal,
    /// The swap contract the swap negotiated.
    pub swap_contract: &'a Option<BytesJson>,
    pub confirmations: u64,
}

/// Validates the counterparty's payment (A2). The payment is read from the node by the id the peer's bytes name.
///
/// # Errors
///
/// [`TronSwapError::Mismatch`] naming the value that differs; [`TronSwapError::Reverted`] for a receipt that is
/// not a success; [`TronSwapError::Undetermined`] or a node error when the
/// evidence stays undetermined for the whole window.
pub async fn validate_payment(coin: &EthCoin, check: &PaymentCheck<'_>) -> Result<(), TronSwapError> {
    validate_payment_with(coin, check, VALIDATION_WAITS).await
}

pub(super) async fn validate_payment_with(
    coin: &EthCoin,
    check: &PaymentCheck<'_>,
    waits: Waits,
) -> Result<(), TronSwapError> {
    let api = tron_api(coin)?;
    let selected = selected_contract(check.swap_contract)?;
    let secret_hash = require_32_byte_hash(check.secret_hash)?;
    let me = my_address(coin);
    let counterparty = address_of_pubkey(check.sender_pub)?;
    let (token, amount) = expected_asset(coin, check.amount)?;
    if let Some(token) = &token {
        // A token that takes a fee on a transfer is refused; the reader is the send side's.
        super::send::ensure_no_token_transfer_fee(coin, token)
            .await
            .map_err(|e| match e {
                TronSendError::Swap(error) => error,
                _ => mismatch("token transfer fee"),
            })?;
    }
    // The id is the client's own derivation from the agreed lock time and secret hash, never read from the peer's
    // call; `payments(id)` below is asked under it, and the call's id is only compared with it.
    let id = tron_swap_id(check.time_lock, &secret_hash);
    let txid = txid_of_bytes(check.payment_tx)?;

    // The transaction the node holds, read for the field checks and the error names. It is polled for: the node
    // may not have it yet.
    let tx = {
        let (api, txid) = (&api, txid.as_str());
        poll(waits, || async move { fetch_swap_tx(api, txid).await.map(Some) }).await?
    };
    if format!("{:x}", tx.txid()) != txid {
        return Err(mismatch("transaction id"));
    }
    let facts = facts_of(&tx, &selected)?;
    if facts.sender != counterparty {
        return Err(mismatch("sender"));
    }
    if facts.receiver != me {
        return Err(mismatch("receiver"));
    }
    if facts.id != id {
        return Err(mismatch("id"));
    }
    if facts.secret_hash != secret_hash {
        return Err(mismatch("secret hash"));
    }
    if facts.lock_time != u64::from(check.time_lock) {
        return Err(mismatch("lock time"));
    }
    if facts.token != token {
        return Err(mismatch("token"));
    }
    if facts.amount != amount {
        return Err(mismatch("amount"));
    }
    let expected_call_value = if token.is_none() {
        if amount > U256::from(i64::MAX as u64) {
            return Err(mismatch("amount"));
        }
        amount.low_u64() as i64
    } else {
        0
    };
    if facts.call_value != expected_call_value {
        return Err(mismatch("call value"));
    }

    wait_for_receipt(&api, &txid, true, check.confirmations.max(1), waits).await?;

    // The deciding read: the agreed payment record against the hash of the expected values.
    let expected_hash = tron_payment_hash(&me, &counterparty, &secret_hash, token.as_ref(), amount);
    let record = {
        let policy = coin.swap_evidence_policy();
        let (api, policy, me, selected, id) = (&api, &policy, &me, &selected, &id);
        poll(waits, || async move {
            let record = agreed_payment(api, policy, me, selected, id).await?;
            match record.state {
                PaymentState::PaymentSent => Ok(Some(record)),
                PaymentState::Uninitialized => Ok(None),
                _ => Err(mismatch("payment state")),
            }
        })
        .await?
    };
    if record.payment_hash != expected_hash {
        return Err(mismatch("payment hash"));
    }
    if record.lock_time != u64::from(check.time_lock) {
        return Err(mismatch("lock time"));
    }
    Ok(())
}

/// What a DEX fee validation checks the fee transaction against.
pub struct FeeCheck<'a> {
    /// The fee transaction as the peer sent it: only its id is read.
    pub fee_tx: &'a TronSwapTx,
    /// The public key of the expected sender.
    pub sender_pub: &'a [u8],
    /// The raw public key of the fee address.
    pub fee_addr_pub: &'a [u8],
    /// The fee amount in the coin's decimal form.
    pub amount: &'a BigDecimal,
    pub min_block_number: u64,
}

/// The `transfer(address,uint256)` selector of a TRC20 token.
const TRC20_TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

/// Validates the DEX fee (A3): a TRX transfer, or a TRC20 `transfer` of the coin's token, from the expected
/// sender to the fee address for at least the amount, with a successful receipt in a block at or after
/// `min_block_number`.
pub async fn validate_fee(coin: &EthCoin, check: &FeeCheck<'_>) -> Result<(), TronSwapError> {
    validate_fee_with(coin, check, VALIDATION_WAITS).await
}

pub(super) async fn validate_fee_with(coin: &EthCoin, check: &FeeCheck<'_>, waits: Waits) -> Result<(), TronSwapError> {
    let api = tron_api(coin)?;
    let sender = address_of_pubkey(check.sender_pub)?;
    let fee_to = address_of_pubkey(check.fee_addr_pub)?;
    let units = wei_from_big_decimal(check.amount, coin.decimals).map_err(|_| mismatch("amount"))?;
    let txid = format!("{:x}", check.fee_tx.txid());
    let tx = {
        let (api, txid) = (&api, txid.as_str());
        poll(waits, || async move { fetch_swap_tx(api, txid).await.map(Some) }).await?
    };
    if format!("{:x}", tx.txid()) != txid {
        return Err(mismatch("transaction id"));
    }
    let address = |bytes: &[u8]| {
        TronAddress::from_bytes(bytes).map_err(|_| TronSwapError::Transaction("not a TRON address".to_owned()))
    };
    let (owner, to, paid, contract_call) = match &coin.coin_type {
        EthCoinType::Tron => {
            let transfer = transfer_of(&tx)?;
            (
                address(&transfer.owner_address)?,
                address(&transfer.to_address)?,
                U256::from(u64::try_from(transfer.amount).map_err(|_| mismatch("amount"))?),
                false,
            )
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let trigger = tx.trigger()?;
            if trigger.contract != TronAddress::from_evm_address(*token_addr) {
                return Err(mismatch("token"));
            }
            let data = trigger.data.as_slice();
            if data.len() != 68 || data[..4] != TRC20_TRANSFER_SELECTOR || trigger.call_value != 0 {
                return Err(mismatch("method"));
            }
            let to = TronAddress::from_evm_address(ethereum_types::Address::from_slice(&data[16..36]));
            (trigger.owner, to, U256::from_big_endian(&data[36..68]), true)
        },
        _ => return Err(TronSwapError::NotTronCoin),
    };
    if owner != sender {
        return Err(mismatch("sender"));
    }
    if to != fee_to {
        return Err(mismatch("recipient"));
    }
    if paid < units {
        return Err(mismatch("amount"));
    }
    let block = wait_for_receipt(&api, &txid, contract_call, 1, waits).await?;
    if block < check.min_block_number {
        return Err(mismatch("block number"));
    }
    Ok(())
}

/// The `TransferContract` a transaction is.
fn transfer_of(tx: &TronSwapTx) -> Result<TransferContract, TronSwapError> {
    let [contract] = tx.raw().contract.as_slice() else {
        return Err(mismatch("contract type"));
    };
    let parameter = contract.parameter.as_ref().ok_or_else(|| mismatch("contract type"))?;
    if parameter.type_url != TRANSFER_CONTRACT_TYPE_URL {
        return Err(mismatch("contract type"));
    }
    TransferContract::decode(parameter.value.as_slice())
        .map_err(|_| TronSwapError::Transaction("the transfer does not decode".to_owned()))
}

// ---------------------------------------------------------------------------------------------------------------
// Own spend and refund (A4)
// ---------------------------------------------------------------------------------------------------------------

/// A spend or a refund that did not end with a confirmed success. `tx` is the signed transaction when it was
/// broadcast, so that the caller keeps it (a node that did not answer may still have taken it).
#[derive(Debug)]
pub struct OwnStepFailure {
    pub error: TronSwapError,
    pub tx: Option<TronSwapTx>,
}

impl From<OwnStepFailure> for crate::TransactionErr {
    fn from(failure: OwnStepFailure) -> Self {
        let text = failure.error.to_string();
        match failure.tx {
            Some(tx) => crate::TransactionErr::TxRecoverable(tx.into(), text),
            None => crate::TransactionErr::Plain(text),
        }
    }
}

/// Which claim a spend is: the one that reveals the secret, or the one whose secret is public already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimKind {
    /// The maker claims the taker's payment, the claim that reveals the secret: it is sent only after its dry run
    /// succeeded, under the swap fee-limit policy; a reverting dry run sends nothing.
    RevealsSecret,
    /// The taker claims the maker's payment with a secret that is public already: always worth sending while the
    /// payment is unspent, with no time gate. A dry run that reverts or fails, or an energy cost above the cap,
    /// falls back to [`PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN`].
    SecretPublic,
}

/// The fixed `fee_limit` of a public-secret claim whose dry run did not give a usable figure: the same cap as every
/// other swap call, [`TRC20_FEE_LIMIT_MAX_SUN`] (100 TRX).
pub const PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN: u64 = TRC20_FEE_LIMIT_MAX_SUN;

/// The claim that reveals the secret is signed only when the agreed chain time plus this margin, in seconds, is below
/// the stored lock time.
pub const REVEALING_CLAIM_LOCK_MARGIN_SECS: u64 = 300;

/// How the `fee_limit` of a sent call is settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendMode {
    /// The `fee_limit` of the swap policy; any failure of it is the error.
    Strict,
    /// As [`SendMode::Strict`], and the coin's TRX balance must cover the `fee_limit` before the broadcast.
    StrictWithBalance,
    /// The `fee_limit` of the swap policy, or [`PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN`] when that fails.
    PublicSecret,
}

/// Sends the call through S2's `send_trigger` under the shared `fee_limit` policy.
async fn send_swap_call(
    coin: &EthCoin,
    contract: &TronAddress,
    data: Vec<u8>,
    call_value: i64,
    mode: SendMode,
) -> Result<TronSwapTx, TronSwapError> {
    // The one call site of the shared fee policy of the swap track (dry-run energy with a margin, capped).
    let fee_limit_sun = match super::swap_call_fee_limit(coin, contract, &data, call_value).await {
        Ok((_energy, fee_limit_sun)) => fee_limit_sun,
        Err(_) if mode == SendMode::PublicSecret => PUBLIC_SECRET_CLAIM_FALLBACK_FEE_LIMIT_SUN,
        Err(e) => return Err(e),
    };
    if mode == SendMode::StrictWithBalance {
        let api = tron_api(coin)?;
        let balance = trx_balance_sun(&api, &my_address(coin)).await.map_err(|e| match e {
            TronBalanceError::Node(error) => TronSwapError::Node(error),
            other => TronSwapError::Node(TronApiError::InvalidResponse(format!(
                "the TRX balance cannot be read: {other}"
            ))),
        })?;
        if balance < U256::from(fee_limit_sun) {
            return Err(mismatch("TRX balance"));
        }
    }
    super::send_trigger(coin, contract, data, call_value, fee_limit_sun).await
}

async fn send_and_confirm(
    coin: &EthCoin,
    contract: &TronAddress,
    call: &TronSwapCall,
    mode: SendMode,
    waits: Waits,
) -> Result<TronSwapTx, OwnStepFailure> {
    let plain = |error| OwnStepFailure { error, tx: None };
    let api = tron_api(coin).map_err(plain)?;
    let data = encode_call(call).map_err(plain)?;
    let tx = match send_swap_call(coin, contract, data, 0, mode).await {
        Ok(tx) => tx,
        Err(TronSwapError::Broadcast { tx, error }) => {
            return Err(OwnStepFailure {
                error: TronSwapError::Node(error),
                tx: Some(*tx),
            })
        },
        Err(error) => return Err(plain(error)),
    };
    let txid = format!("{:x}", tx.txid());
    let confirmations = coin.required_confirmations.load(Ordering::Relaxed).max(1);
    match wait_for_receipt(&api, &txid, true, confirmations, waits).await {
        Ok(_) => Ok(tx),
        Err(error) => Err(OwnStepFailure { error, tx: Some(tx) }),
    }
}

/// The payment transaction the node holds for the id the bytes name, and its facts.
async fn payment_from_node(
    api: &TronApiClient,
    payment_tx: &[u8],
    selected: &TronAddress,
) -> Result<(PaymentFacts, String), TronSwapError> {
    let txid = txid_of_bytes(payment_tx)?;
    let tx = fetch_swap_tx(api, &txid).await?;
    if format!("{:x}", tx.txid()) != txid {
        return Err(mismatch("transaction id"));
    }
    Ok((facts_of(&tx, selected)?, txid))
}

/// Spends the payment `payment_tx` (the counterparty's) with `secret` (A4), as the claim `kind`. The call goes to
/// the selected contract; it counts after a successful receipt at the required depth.
pub async fn spend_payment(
    coin: &EthCoin,
    payment_tx: &[u8],
    secret: &[u8],
    swap_contract: &Option<BytesJson>,
    kind: ClaimKind,
) -> Result<TronSwapTx, OwnStepFailure> {
    spend_payment_with(coin, payment_tx, secret, swap_contract, kind, OWN_STEP_WAITS).await
}

pub(super) async fn spend_payment_with(
    coin: &EthCoin,
    payment_tx: &[u8],
    secret: &[u8],
    swap_contract: &Option<BytesJson>,
    kind: ClaimKind,
    waits: Waits,
) -> Result<TronSwapTx, OwnStepFailure> {
    let plain = |error| OwnStepFailure { error, tx: None };
    let api = tron_api(coin).map_err(plain)?;
    let selected = selected_contract(swap_contract).map_err(plain)?;
    let (facts, _) = payment_from_node(&api, payment_tx, &selected).await.map_err(plain)?;
    let secret = <[u8; 32]>::try_from(secret).map_err(|_| plain(mismatch("secret")))?;
    if kdf_crypto::sha256(&secret).as_slice() != facts.secret_hash.as_slice() {
        return Err(plain(mismatch("secret")));
    }
    if facts.receiver != my_address(coin) {
        return Err(plain(mismatch("receiver")));
    }
    let call = TronSwapCall::ReceiverSpend {
        id: facts.id,
        amount: facts.amount,
        secret,
        token: facts.token,
        sender: facts.sender,
    };
    if kind == ClaimKind::SecretPublic {
        // No time gate: the claim is sent while the payment is unspent. Spent or refunded is no send; a read that
        // fails does not block it.
        if let Ok(state) = ordinary_state(&api, &my_address(coin), &selected, &facts.id).await {
            if matches!(state, PaymentState::ReceiverSpent | PaymentState::SenderRefunded) {
                return Err(plain(mismatch("payment state")));
            }
        }
    }
    let mode = match kind {
        ClaimKind::RevealsSecret => {
            // The agreed solidified record must say `PaymentSent` and the chain time plus the margin must be below
            // the stored lock time; the dry run and the balance check follow in `send_swap_call`.
            let read = agreed_claim_read(
                &api,
                &coin.swap_evidence_policy(),
                &my_address(coin),
                &selected,
                &facts.id,
            )
            .await
            .map_err(plain)?;
            require_payment_sent(read.record.state).map_err(plain)?;
            revealing_claim_margin(read.chain_time_s, read.record.lock_time).map_err(plain)?;
            SendMode::StrictWithBalance
        },
        ClaimKind::SecretPublic => SendMode::PublicSecret,
    };
    send_and_confirm(coin, &selected, &call, mode, waits).await
}

/// Refunds our own payment `payment_tx` (A4), after the agreed solidified state says it is still `PaymentSent`. The
/// time gate of [`tron_can_refund_htlc`] is the caller's.
pub async fn refund_payment(
    coin: &EthCoin,
    payment_tx: &[u8],
    secret_hash: &[u8],
    swap_contract: &Option<BytesJson>,
) -> Result<TronSwapTx, OwnStepFailure> {
    refund_payment_with(coin, payment_tx, secret_hash, swap_contract, OWN_STEP_WAITS).await
}

pub(super) async fn refund_payment_with(
    coin: &EthCoin,
    payment_tx: &[u8],
    secret_hash: &[u8],
    swap_contract: &Option<BytesJson>,
    waits: Waits,
) -> Result<TronSwapTx, OwnStepFailure> {
    let plain = |error| OwnStepFailure { error, tx: None };
    let api = tron_api(coin).map_err(plain)?;
    let selected = selected_contract(swap_contract).map_err(plain)?;
    let (facts, _) = payment_from_node(&api, payment_tx, &selected).await.map_err(plain)?;
    let secret_hash = require_32_byte_hash(secret_hash).map_err(plain)?;
    if facts.secret_hash != secret_hash {
        return Err(plain(mismatch("secret hash")));
    }
    if facts.sender != my_address(coin) {
        return Err(plain(mismatch("sender")));
    }
    // Before signing: the agreed solidified state must be `PaymentSent`; any other state or an undetermined read
    // is an error and never a refund (CRD ch.51 R88, R100).
    let state = agreed_state(
        &api,
        &coin.swap_evidence_policy(),
        &my_address(coin),
        &selected,
        &facts.id,
    )
    .await
    .map_err(plain)?;
    require_payment_sent(state).map_err(plain)?;
    let call = TronSwapCall::SenderRefund {
        id: facts.id,
        amount: facts.amount,
        secret_hash,
        token: facts.token,
        receiver: facts.receiver,
    };
    send_and_confirm(coin, &selected, &call, SendMode::Strict, waits).await
}

// ---------------------------------------------------------------------------------------------------------------
// Refund gate (A5)
// ---------------------------------------------------------------------------------------------------------------

/// The time part of the refund gate (ch.51 R88): a refund is allowed once the agreed chain time has reached the
/// lock time.
///
/// # Errors
///
/// [`TronSwapError::TooEarly`] before the lock time.
pub(super) fn refund_time_decision(chain_time_s: u64, lock_time: u64) -> Result<(), TronSwapError> {
    if chain_time_s < lock_time {
        return Err(TronSwapError::TooEarly {
            chain_time: chain_time_s,
            lock_time,
        });
    }
    Ok(())
}

/// The state part of a refund: only a payment that is still `PaymentSent` is refunded.
///
/// # Errors
///
/// [`TronSwapError::Mismatch`] for any other state.
pub(super) fn require_payment_sent(state: PaymentState) -> Result<(), TronSwapError> {
    if state == PaymentState::PaymentSent {
        Ok(())
    } else {
        Err(mismatch("payment state"))
    }
}

/// Whether the agreed chain time plus [`REVEALING_CLAIM_LOCK_MARGIN_SECS`] is below the stored lock time.
///
/// # Errors
///
/// [`TronSwapError::Mismatch`] naming the lock margin when it is not.
pub(super) fn revealing_claim_margin(chain_time_s: u64, lock_time: u64) -> Result<(), TronSwapError> {
    if chain_time_s.saturating_add(REVEALING_CLAIM_LOCK_MARGIN_SECS) < lock_time {
        Ok(())
    } else {
        Err(mismatch("lock margin"))
    }
}

/// The refund gate the coin answers through `can_refund_htlc(locktime)`: reads the solidified chain time of two
/// nodes in agreement (or of the evidence node) and compares it with `lock_time`. Before the lock time the answer
/// is [`CanRefundHtlc::HaveToWait`] with the seconds left. The payment state is checked by the refund itself
/// ([`refund_payment`]), which has the payment id.
///
/// # Errors
///
/// Undetermined evidence ([`TronSwapError::Undetermined`], a node error):
/// the caller does not refund.
pub async fn tron_can_refund_htlc(coin: &EthCoin, lock_time: u64) -> Result<CanRefundHtlc, TronSwapError> {
    let api = tron_api(coin)?;
    let chain_time = agreed_chain_time(&api, &coin.swap_evidence_policy(), lock_time).await?;
    match refund_time_decision(chain_time, lock_time) {
        Ok(()) => Ok(CanRefundHtlc::CanRefundNow),
        Err(TronSwapError::TooEarly { chain_time, lock_time }) => {
            Ok(CanRefundHtlc::HaveToWait(lock_time.saturating_sub(chain_time) + 1))
        },
        Err(other) => Err(other),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Discovery and secret (A6)
// ---------------------------------------------------------------------------------------------------------------

/// A payment the node holds: its facts, block and block time.
struct PaymentContext {
    facts: PaymentFacts,
    block: u64,
    block_time_ms: Option<i64>,
}

async fn payment_context(
    api: &TronApiClient,
    payment_tx: &[u8],
    selected: &TronAddress,
) -> Result<PaymentContext, TronSwapError> {
    let (facts, txid) = payment_from_node(api, payment_tx, selected).await?;
    let info = api
        .get_confirmed_transaction_info(&txid)
        .await
        .map_err(TronSwapError::Node)?;
    if info.receipt.as_ref().and_then(|receipt| receipt.result.as_deref()) != Some("SUCCESS") {
        return Err(TronSwapError::Reverted {
            result: "the payment's receipt is not a success".to_owned(),
        });
    }
    let block = info.block_number.ok_or(TronSwapError::NotYetFinal)?;
    Ok(PaymentContext {
        facts,
        block,
        block_time_ms: info.block_time_stamp,
    })
}

/// The ordinary (not solidified) payment state, for a progress poll.
async fn ordinary_state(
    api: &TronApiClient,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<PaymentState, TronSwapError> {
    let data = encode_payments_query(id)?;
    let answer = api
        .trigger_constant_read_data(owner, contract, &data)
        .await
        .map_err(TronSwapError::Node)?;
    Ok(record_of(&answer)?.state)
}

/// Looks for the spend or refund of the payment `payment_tx` (A6). `Ok(None)` only when the agreed solidified
/// state says the payment is still locked.
///
/// # Errors
///
/// Undetermined evidence, a source that is down or capped, and a state that says spent with the consuming
/// transaction not (yet) found are errors, never "not spent".
pub async fn search_for_spend(
    coin: &EthCoin,
    payment_tx: &[u8],
    swap_contract: &Option<BytesJson>,
) -> Result<Option<FoundSwapTxSpend>, TronSwapError> {
    search_for_spend_with(coin, payment_tx, swap_contract, DEFAULT_CAPS).await
}

pub(super) async fn search_for_spend_with(
    coin: &EthCoin,
    payment_tx: &[u8],
    swap_contract: &Option<BytesJson>,
    caps: Caps,
) -> Result<Option<FoundSwapTxSpend>, TronSwapError> {
    let api = tron_api(coin)?;
    let selected = selected_contract(swap_contract)?;
    let context = payment_context(&api, payment_tx, &selected).await?;
    let id = context.facts.id;
    let kind = match agreed_state(&api, &coin.swap_evidence_policy(), &my_address(coin), &selected, &id).await? {
        PaymentState::PaymentSent => return Ok(None),
        PaymentState::Uninitialized => return Err(mismatch("payment state")),
        PaymentState::ReceiverSpent => Kind::Spend,
        PaymentState::SenderRefunded => Kind::Refund,
    };
    let tx = find_consumer(&api, &selected, &id, kind, context.block, context.block_time_ms, caps).await?;
    Ok(Some(match kind {
        Kind::Spend => FoundSwapTxSpend::Spent(tx.into()),
        Kind::Refund => FoundSwapTxSpend::Refunded(tx.into()),
    }))
}

/// Waits until `wait_until_s` (seconds) for the payment `payment_tx` to be spent and returns the spending
/// transaction (A6). The state is read from the ordinary API (a progress poll). A source that fails is polled
/// again until the deadline.
pub async fn wait_for_spend(
    coin: &EthCoin,
    payment_tx: &[u8],
    wait_until_s: u64,
    swap_contract: &Option<BytesJson>,
) -> Result<TronSwapTx, TronSwapError> {
    wait_for_spend_with(coin, payment_tx, wait_until_s, swap_contract, 15.0, DEFAULT_CAPS).await
}

pub(super) async fn wait_for_spend_with(
    coin: &EthCoin,
    payment_tx: &[u8],
    wait_until_s: u64,
    swap_contract: &Option<BytesJson>,
    poll_s: f64,
    caps: Caps,
) -> Result<TronSwapTx, TronSwapError> {
    let api = tron_api(coin)?;
    let selected = selected_contract(swap_contract)?;
    let me = my_address(coin);
    let mut context: Option<PaymentContext> = None;
    // The head block and its time (ms) at the last read that showed the payment unspent.
    let mut last_unspent: Option<(u64, i64)> = None;
    let mut last: Option<TronSwapError> = None;
    loop {
        let attempt: Result<Option<TronSwapTx>, TronSwapError> = async {
            if context.is_none() {
                context = Some(payment_context(&api, payment_tx, &selected).await?);
            }
            let known = context.as_ref().ok_or(TronSwapError::NotYetFinal)?;
            // The head is read before the state, so a state of `PaymentSent` was true at or after this head.
            let head = api.get_now_block().await.map_err(TronSwapError::Node)?;
            let head_point = (head.block_header.raw_data.number, head.block_header.raw_data.timestamp);
            match ordinary_state(&api, &me, &selected, &known.facts.id).await? {
                PaymentState::PaymentSent => {
                    last_unspent = Some(head_point);
                    Ok(None)
                },
                PaymentState::Uninitialized => Ok(None),
                PaymentState::SenderRefunded => Err(mismatch("payment state")),
                PaymentState::ReceiverSpent => {
                    // The search starts at the last unspent read, with an overlap of one block, and at the
                    // payment when no such read was made.
                    let (from_block, since_ms) = match last_unspent {
                        Some((block, time_ms)) => {
                            (block.saturating_sub(1), Some(time_ms.saturating_sub(BLOCK_TIME_MS)))
                        },
                        None => (known.block, known.block_time_ms),
                    };
                    let id = known.facts.id;
                    match find_consumer(&api, &selected, &id, Kind::Spend, from_block, since_ms, caps).await {
                        // Nothing from the last unspent start while the record says spent: the node that gave the
                        // state may have lagged the node that gave the head, so the search runs once more from
                        // the payment, the start of the one-time search.
                        Err(TronSwapError::NotYetFinal) if last_unspent.is_some() => {
                            find_consumer(
                                &api,
                                &selected,
                                &id,
                                Kind::Spend,
                                known.block,
                                known.block_time_ms,
                                caps,
                            )
                            .await
                        },
                        other => other,
                    }
                    .map(Some)
                },
            }
        }
        .await;
        match attempt {
            Ok(Some(tx)) => return Ok(tx),
            Ok(None) => {},
            Err(e) if is_retryable(&e) || matches!(e, TronSwapError::SearchCapped { .. }) => last = Some(e),
            Err(e) => return Err(e),
        }
        if now_ms() / 1000 >= wait_until_s {
            return Err(last.unwrap_or(TronSwapError::NotYetFinal));
        }
        Timer::sleep(poll_s).await;
    }
}

fn secret_of_call(tx: &TronSwapTx, secret_hash: &[u8]) -> Result<Option<[u8; 32]>, TronSwapError> {
    let Ok((_, _, TronSwapCall::ReceiverSpend { secret, .. })) = call_of(tx) else {
        return Ok(None);
    };
    let matches = crypto::secret_hash_algo::secret_matches_hash(&secret, secret_hash)
        .map_err(|_| TronSwapError::HashWidth { got: secret_hash.len() })?;
    Ok(matches.then_some(secret))
}

/// The secret of `secret_hash` in the call of the spend `spend_tx`, without a node. Only a value whose hash is
/// `secret_hash` is returned.
///
/// # Errors
///
/// [`TronSwapError::Mismatch`] when the transaction is no `receiverSpend` call with the secret of the swap.
pub fn extract_secret(secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, TronSwapError> {
    let tx = TronSwapTx::from_bytes(spend_tx)?;
    secret_of_call(&tx, secret_hash)?
        .map(|secret| secret.to_vec())
        .ok_or_else(|| mismatch("secret"))
}

/// The secret of `secret_hash` from the spend `spend_tx`: the call data first, then the `ReceiverSpent` event of
/// the successful receipt. Only a value whose hash is `secret_hash` is returned.
pub async fn extract_secret_from_spend(
    coin: &EthCoin,
    secret_hash: &[u8],
    spend_tx: &[u8],
) -> Result<Vec<u8>, TronSwapError> {
    let tx = TronSwapTx::from_bytes(spend_tx)?;
    if let Some(secret) = secret_of_call(&tx, secret_hash)? {
        return Ok(secret.to_vec());
    }
    let api = tron_api(coin)?;
    let contract = tx.trigger()?.contract;
    let info = api
        .get_confirmed_transaction_info(&format!("{:x}", tx.txid()))
        .await
        .map_err(TronSwapError::Node)?;
    if info.receipt.as_ref().and_then(|receipt| receipt.result.as_deref()) != Some("SUCCESS") {
        return Err(TronSwapError::Reverted {
            result: "the spend's receipt is not a success".to_owned(),
        });
    }
    for log in &info.log {
        if let Some(TronSwapEvent::ReceiverSpent { secret, .. }) = discovery::event_of_log(log, &contract) {
            let matches = crypto::secret_hash_algo::secret_matches_hash(&secret, secret_hash)
                .map_err(|_| TronSwapError::HashWidth { got: secret_hash.len() })?;
            if matches {
                return Ok(secret.to_vec());
            }
        }
    }
    Err(mismatch("secret"))
}

/// Waits until `wait_until_s` for the transaction `tx` to have a successful receipt at depth `confirmations`
/// (R21.B5), polling every `check_every_s` seconds.
pub async fn wait_for_confirmations(
    coin: &EthCoin,
    tx: &[u8],
    confirmations: u64,
    wait_until_s: u64,
    check_every_s: u64,
) -> Result<(), TronSwapError> {
    let api = tron_api(coin)?;
    let tx = TronSwapTx::from_bytes(tx)?;
    let contract_call = tx.trigger().is_ok();
    let waits = Waits {
        poll_s: check_every_s as f64,
        window_ms: (wait_until_s * 1000).saturating_sub(now_ms()),
    };
    wait_for_receipt(
        &api,
        &format!("{:x}", tx.txid()),
        contract_call,
        confirmations.max(1),
        waits,
    )
    .await
    .map(|_| ())
}
