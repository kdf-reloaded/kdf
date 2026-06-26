//! TRON version-1 atomic-swap on-chain execution (Chapter 21 §21.13–§21.16).
//!
//! These async flows drive the *dictated* interop core in [`super::swap`] over
//! the Tron node HTTP API ([`super::api`]): they build, sign ([`super::sign`]),
//! and broadcast the swap-contract `TriggerSmartContract` calls, read back
//! `payments(id)` state and transaction receipts, and surface results through
//! [`SignedTronTx`] / [`crate::TransactionEnum::TronTx`] (R-T5).
//!
//! Routing is by chain family: the EVM-family `SwapOps`/`MarketCoinOps` surface
//! dispatches a TRON-family coin here (R-S1) while Ethereum coins stay on the
//! Ethereum path. The generic swap drivers carry no TRON control flow other
//! than secret-hash selection (R-S2).

use bigdecimal::BigDecimal;
use common::now_ms;
use ethereum_types::{Address, H256, U256};
use prost::Message as _;

use super::api::{ContractEvent, ContractEventsQuery, TronApiClient};
use super::fee::{self, TronAccountResources, TronChainPrices, TronTxFeeDetails};
use super::proto;
use super::sign::{hash_transaction_raw, sign_transaction_raw};
use super::swap;
use super::tx_builder::{build_trc20_transfer, build_trigger_smart_contract, build_trx_transfer};
use super::{SignedTronTx, TronAddress};
use crate::eth::{wei_from_big_decimal, EthCoin, EthCoinType};
use crate::{FoundSwapTxSpend, TransactionEnum};

/// Fee limit (in SUN) attached to swap-contract calls. Swap HTLC methods burn
/// more energy than a plain transfer; 150 TRX is a conventional safe cap.
const SWAP_FEE_LIMIT_SUN: i64 = 150_000_000;

/// Fee limit (in SUN) for a TRC20 dex-fee `transfer`.
const TRC20_FEE_LIMIT_SUN: i64 = 100_000_000;

// ---------------------------------------------------------------------------
// Small conversions / accessors
// ---------------------------------------------------------------------------

fn tron_api(coin: &EthCoin) -> Result<TronApiClient, String> {
    coin.tron_api
        .clone()
        .ok_or_else(|| "TRON API client missing on this coin".to_owned())
}

fn u256_to_i64_sun(v: U256) -> Result<i64, String> {
    if v > U256::from(i64::MAX as u64) {
        return Err(format!("amount {v} exceeds i64 SUN range"));
    }
    Ok(v.as_u64() as i64)
}

fn local_secret(coin: &EthCoin) -> Result<&mm2_eth::keys::Secret, String> {
    coin.signer
        .local_secret()
        .ok_or_else(|| "TRON swaps require a local private key".to_owned())
}

// ---------------------------------------------------------------------------
// build → sign → broadcast
// ---------------------------------------------------------------------------

/// Sign a built `TransactionRaw`, broadcast it, and surface the signed TRON
/// transaction (R-L7: txID is SHA-256 of the raw body, not an RLP hash).
async fn sign_and_broadcast(
    coin: &EthCoin,
    api: &TronApiClient,
    raw: proto::TransactionRaw,
) -> Result<SignedTronTx, String> {
    let secret = local_secret(coin)?;
    let (hash, sig) = sign_transaction_raw(secret, &raw).map_err(|e| e.to_string())?;
    let tx = proto::Transaction {
        raw_data: Some(raw),
        signature: vec![sig],
    };
    let tx_bytes = tx.encode_to_vec();
    api.broadcast_hex(&hex::encode(&tx_bytes))
        .await
        .map_err(|e| e.to_string())?;
    Ok(SignedTronTx {
        tx_bytes,
        tx_hash: hash,
    })
}

/// Build, sign, and broadcast a `TriggerSmartContract` swap-contract call.
async fn broadcast_trigger(
    coin: &EthCoin,
    api: &TronApiClient,
    contract: &TronAddress,
    call_value_sun: i64,
    data: Vec<u8>,
) -> Result<SignedTronTx, String> {
    let sender = TronAddress::from_evm_address(coin.my_address);
    let tapos = api
        .get_now_block()
        .await
        .map_err(|e| e.to_string())?
        .to_tapos()
        .map_err(|e| e.to_string())?;
    let raw = build_trigger_smart_contract(
        &sender,
        contract,
        call_value_sun,
        data,
        &tapos,
        now_ms() as i64,
        SWAP_FEE_LIMIT_SUN,
    );
    sign_and_broadcast(coin, api, raw).await
}

// ---------------------------------------------------------------------------
// On-chain reads (payments(id), allowance)
// ---------------------------------------------------------------------------

/// Read the swap contract's stored `payments(id)` lifecycle state (R-L2/§21.9).
async fn read_payment_state(
    api: &TronApiClient,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<swap::SwapPaymentState, String> {
    let param_hex = hex::encode(swap::encode_payments_query_param(id));
    let resp = api
        .trigger_constant_contract(owner, contract, "payments(bytes32)", &param_hex)
        .await
        .map_err(|e| e.to_string())?;
    let raw = resp
        .constant_result
        .and_then(|mut v| v.pop())
        .ok_or_else(|| "payments() returned no result".to_owned())?;
    let bytes = hex::decode(raw).map_err(|e| format!("bad payments() hex: {e}"))?;
    let (_hash, _lock_time, state) = swap::decode_payments_output(&bytes).map_err(|e| e.to_string())?;
    Ok(state)
}

/// Read the TRC20 allowance the funder has granted the swap contract (R-AP1).
async fn read_allowance(
    api: &TronApiClient,
    owner: &TronAddress,
    token: &TronAddress,
    spender: &TronAddress,
) -> Result<U256, String> {
    let mut param = [0u8; 64];
    param[12..32].copy_from_slice(owner.to_evm_address().as_ref());
    param[44..64].copy_from_slice(spender.to_evm_address().as_ref());
    let resp = api
        .trigger_constant_contract(owner, token, "allowance(address,address)", &hex::encode(param))
        .await
        .map_err(|e| e.to_string())?;
    let raw = resp
        .constant_result
        .and_then(|mut v| v.pop())
        .ok_or_else(|| "allowance() returned no result".to_owned())?;
    let bytes = hex::decode(raw).map_err(|e| format!("bad allowance() hex: {e}"))?;
    if bytes.len() < 32 {
        return Err("allowance() result shorter than 32 bytes".to_owned());
    }
    Ok(U256::from_big_endian(&bytes[..32]))
}

/// Ensure the swap contract is approved to move the funded TRC20 token,
/// using the USDT-style approve-to-zero-first reset when required (R-AP1).
async fn ensure_allowance(
    coin: &EthCoin,
    api: &TronApiClient,
    token: &TronAddress,
    spender: &TronAddress,
    required: U256,
) -> Result<(), String> {
    let owner = TronAddress::from_evm_address(coin.my_address);
    let current = read_allowance(api, &owner, token, spender).await?;
    if swap::allowance_sufficient(current, required) {
        return Ok(());
    }
    if swap::needs_approve_to_zero(current, required) {
        let zero = swap::encode_trc20_approve(spender.to_evm_address(), U256::zero()).map_err(|e| e.to_string())?;
        broadcast_trigger(coin, api, token, 0, zero).await?;
    }
    let approve = swap::encode_trc20_approve(spender.to_evm_address(), U256::max_value()).map_err(|e| e.to_string())?;
    broadcast_trigger(coin, api, token, 0, approve).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// R-L1: payment send (maker / taker)
// ---------------------------------------------------------------------------

/// Lock a maker/taker HTLC payment in the Tron swap contract under `id` (R-L1).
pub async fn send_payment(
    coin: EthCoin,
    swap_contract: Address,
    receiver: Address,
    secret_hash: [u8; 32],
    time_lock: u32,
    amount: BigDecimal,
) -> Result<SignedTronTx, String> {
    let api = tron_api(&coin)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let id = swap::swap_id(time_lock, &secret_hash);
    let amount_units = wei_from_big_decimal(&amount, coin.decimals).map_err(|e| e.to_string())?;

    match &coin.coin_type {
        EthCoinType::Tron => {
            let data = swap::encode_eth_payment(&swap::EthPaymentArgs {
                id,
                receiver,
                secret_hash,
                lock_time: time_lock as u64,
            })
            .map_err(|e| e.to_string())?;
            let call_value = u256_to_i64_sun(amount_units)?;
            broadcast_trigger(&coin, &api, &contract, call_value, data).await
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            ensure_allowance(&coin, &api, &token, &contract, amount_units).await?;
            let data = swap::encode_erc20_payment(&swap::Erc20PaymentArgs {
                id,
                amount: amount_units,
                token_addr: *token_addr,
                receiver,
                secret_hash,
                lock_time: time_lock as u64,
            })
            .map_err(|e| e.to_string())?;
            broadcast_trigger(&coin, &api, &contract, 0, data).await
        },
        _ => Err("tron::swap_ops::send_payment invoked on a non-TRON coin".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// R-L2: payment validation
// ---------------------------------------------------------------------------

/// Validate a counterparty's broadcast payment against the negotiated terms
/// (R-L2): decode + ABI cross-check via [`swap::validate_payment_tx`], then
/// confirm the on-chain `payments(id)` state records the lock.
#[allow(clippy::too_many_arguments)]
pub async fn validate_payment(
    coin: EthCoin,
    payment_tx: Vec<u8>,
    swap_contract: Address,
    funder: Address,
    receiver: Address,
    secret_hash: [u8; 32],
    time_lock: u64,
    amount: BigDecimal,
) -> Result<(), String> {
    let api = tron_api(&coin)?;
    let amount_units = wei_from_big_decimal(&amount, coin.decimals).map_err(|e| e.to_string())?;
    let token_addr = match &coin.coin_type {
        EthCoinType::Tron => None,
        EthCoinType::Trc20 { token_addr, .. } => Some(*token_addr),
        _ => return Err("tron::swap_ops::validate_payment invoked on a non-TRON coin".to_owned()),
    };

    let expected = swap::ExpectedPayment {
        swap_contract,
        funder,
        receiver,
        secret_hash,
        lock_time: time_lock,
        amount: amount_units,
        token_addr,
    };
    swap::validate_payment_tx(&payment_tx, &expected).map_err(|e| e.to_string())?;

    let owner = TronAddress::from_evm_address(coin.my_address);
    let contract = TronAddress::from_evm_address(swap_contract);
    let id = swap::swap_id(time_lock as u32, &secret_hash);
    match read_payment_state(&api, &owner, &contract, &id).await? {
        swap::SwapPaymentState::PaymentSent
        | swap::SwapPaymentState::ReceiverSpent
        | swap::SwapPaymentState::SenderRefunded => Ok(()),
        swap::SwapPaymentState::Uninitialised => {
            Err("payment not recorded on-chain (payments(id) is uninitialised)".to_owned())
        },
    }
}

// ---------------------------------------------------------------------------
// Reconstruct payment arguments from a decoded payment transaction
// ---------------------------------------------------------------------------

struct PaymentArgs {
    id: [u8; 32],
    amount: U256,
    secret_hash: [u8; 32],
    token_addr: Address,
    funder: Address,
    receiver: Address,
}

/// Decode an original payment transaction (protobuf) into the call arguments a
/// spend (R-L3) or refund (R-L4) must reconstruct.
fn decode_payment_args(payment_tx: &[u8]) -> Result<PaymentArgs, String> {
    let trigger = match swap::decode_single_contract(payment_tx).map_err(|e| e.to_string())? {
        swap::DecodedContract::Trigger(t) => t,
        swap::DecodedContract::Transfer(_) => {
            return Err("payment tx is a TransferContract, not a swap-contract call".to_owned())
        },
    };
    let funder = TronAddress::from_bytes(&trigger.owner_address)
        .map_err(|e| e.to_string())?
        .to_evm_address();

    // Native ethPayment carries the locked value as call_value; TRC20
    // erc20Payment carries the amount and token in the ABI arguments.
    if let Ok(args) = swap::decode_eth_payment_call(&trigger.data) {
        return Ok(PaymentArgs {
            id: args.id,
            amount: U256::from(trigger.call_value.max(0) as u64),
            secret_hash: args.secret_hash,
            token_addr: Address::zero(),
            funder,
            receiver: args.receiver,
        });
    }
    let args = swap::decode_erc20_payment_call(&trigger.data).map_err(|e| e.to_string())?;
    Ok(PaymentArgs {
        id: args.id,
        amount: args.amount,
        secret_hash: args.secret_hash,
        token_addr: args.token_addr,
        funder,
        receiver: args.receiver,
    })
}

// ---------------------------------------------------------------------------
// R-L3: spend
// ---------------------------------------------------------------------------

/// Spend a payment by revealing the secret (R-L3): reconstruct the original
/// `ethPayment`/`erc20Payment` arguments, confirm the on-chain state, and call
/// `receiverSpend`.
pub async fn spend_payment(
    coin: EthCoin,
    swap_contract: Address,
    payment_tx: Vec<u8>,
    secret: [u8; 32],
) -> Result<SignedTronTx, String> {
    let api = tron_api(&coin)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let args = decode_payment_args(&payment_tx)?;

    let owner = TronAddress::from_evm_address(coin.my_address);
    if read_payment_state(&api, &owner, &contract, &args.id).await? != swap::SwapPaymentState::PaymentSent {
        return Err("payment is not in PaymentSent state; cannot spend".to_owned());
    }

    let data = swap::encode_receiver_spend(&swap::ReceiverSpendArgs {
        id: args.id,
        amount: args.amount,
        secret,
        token_addr: args.token_addr,
        sender: args.funder,
    })
    .map_err(|e| e.to_string())?;
    broadcast_trigger(&coin, &api, &contract, 0, data).await
}

// ---------------------------------------------------------------------------
// R-L4: refund
// ---------------------------------------------------------------------------

/// Refund a payment after its lock-time (R-L4): reconstruct the original
/// arguments, confirm the lock-time has elapsed and the on-chain state, and
/// call `senderRefund`.
pub async fn refund_payment(
    coin: EthCoin,
    swap_contract: Address,
    payment_tx: Vec<u8>,
    time_lock: u32,
) -> Result<SignedTronTx, String> {
    let api = tron_api(&coin)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let args = decode_payment_args(&payment_tx)?;

    if (now_ms() / 1000) < time_lock as u64 {
        return Err(format!(
            "lock-time {time_lock} has not elapsed; refund not yet permitted"
        ));
    }

    let owner = TronAddress::from_evm_address(coin.my_address);
    if read_payment_state(&api, &owner, &contract, &args.id).await? != swap::SwapPaymentState::PaymentSent {
        return Err("payment is not in PaymentSent state; cannot refund".to_owned());
    }

    let data = swap::encode_sender_refund(&args.id, args.amount, &args.secret_hash, args.token_addr, args.receiver)
        .map_err(|e| e.to_string())?;
    broadcast_trigger(&coin, &api, &contract, 0, data).await
}

// ---------------------------------------------------------------------------
// R-L5: secret extraction
// ---------------------------------------------------------------------------

/// Extract the revealed secret from a counterparty's spend transaction (R-L5):
/// decode the protobuf, confirm a `receiverSpend` call, and ABI-decode the
/// secret. The secret is also carried by the `ReceiverSpent` event (R-SA1).
pub fn extract_secret(spend_tx: &[u8]) -> Result<Vec<u8>, String> {
    let trigger = match swap::decode_single_contract(spend_tx).map_err(|e| e.to_string())? {
        swap::DecodedContract::Trigger(t) => t,
        swap::DecodedContract::Transfer(_) => {
            return Err("spend tx is a TransferContract, not a receiverSpend call".to_owned())
        },
    };
    let secret = swap::extract_secret_from_spend(&trigger.data).map_err(|e| e.to_string())?;
    Ok(secret.to_vec())
}

// ---------------------------------------------------------------------------
// R-DF1 / R-DF2: dex taker-fee send and validation
// ---------------------------------------------------------------------------

/// Send the DEX taker-fee to the protocol fee recipient (R-DF1): a native TRX
/// transfer for TRX, or a TRC20 token transfer for a TRC20 coin.
pub async fn send_dex_fee(coin: EthCoin, recipient: Address, amount: BigDecimal) -> Result<SignedTronTx, String> {
    let api = tron_api(&coin)?;
    let sender = TronAddress::from_evm_address(coin.my_address);
    let receiver = TronAddress::from_evm_address(recipient);
    let amount_units = wei_from_big_decimal(&amount, coin.decimals).map_err(|e| e.to_string())?;
    let tapos = api
        .get_now_block()
        .await
        .map_err(|e| e.to_string())?
        .to_tapos()
        .map_err(|e| e.to_string())?;
    let now = now_ms() as i64;

    let raw = match &coin.coin_type {
        EthCoinType::Tron => {
            let amount_sun = u256_to_i64_sun(amount_units)?;
            build_trx_transfer(&sender, &receiver, amount_sun, &tapos, now)
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            let amount_u64 = amount_units
                .try_into()
                .map_err(|_| "TRC20 fee amount exceeds u64".to_owned())?;
            build_trc20_transfer(&sender, &token, &receiver, amount_u64, &tapos, now, TRC20_FEE_LIMIT_SUN)
        },
        _ => return Err("tron::swap_ops::send_dex_fee invoked on a non-TRON coin".to_owned()),
    };
    sign_and_broadcast(&coin, &api, raw).await
}

/// Validate a counterparty's broadcast taker-fee payment (R-DF2).
pub fn validate_dex_fee(coin: &EthCoin, fee_tx: &[u8], recipient: Address, amount: BigDecimal) -> Result<(), String> {
    let amount_units = wei_from_big_decimal(&amount, coin.decimals).map_err(|e| e.to_string())?;
    let token_addr = match &coin.coin_type {
        EthCoinType::Tron => None,
        EthCoinType::Trc20 { token_addr, .. } => Some(*token_addr),
        _ => return Err("tron::swap_ops::validate_dex_fee invoked on a non-TRON coin".to_owned()),
    };
    swap::validate_dex_fee_tx(fee_tx, recipient, amount_units, token_addr).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// R-L6: spend/refund discovery (event-indexer based)
// ---------------------------------------------------------------------------

/// Page size requested from the event indexer.
const EVENT_PAGE_LIMIT: u32 = 50;
/// Upper bound on indexer pages walked per discovery query (cursor pagination).
const EVENT_MAX_PAGES: usize = 20;

/// Refuse discovery with the CRD-sanctioned typed error when no event-indexer
/// endpoint is configured (R-L6): discovery fails loudly rather than silently
/// reporting "not found".
fn require_event_indexer(api: &TronApiClient) -> Result<(), String> {
    if api.has_event_indexer() {
        Ok(())
    } else {
        Err(swap::TronSwapError::NoEventEndpoint.to_string())
    }
}

/// Whether a normalised swap event carries the swap `id` under discovery.
fn event_matches_id(ev: &swap::SwapEvent, id: &[u8; 32]) -> bool {
    match ev {
        swap::SwapEvent::PaymentSent { id: e } | swap::SwapEvent::SenderRefunded { id: e } => e == id,
        swap::SwapEvent::ReceiverSpent { id: e, .. } => e == id,
    }
}

/// Find the first event in a single indexer page whose decoded swap id matches
/// `id`, returning its transaction id and the normalised event. Pure (no I/O)
/// so the match-by-swap-id logic is unit-testable offline.
fn first_matching_event(events: &[ContractEvent], id: &[u8; 32]) -> Option<(String, swap::SwapEvent)> {
    for ev in events {
        if let Ok(decoded) = swap::decode_event_from_indexer(&ev.event_name, &ev.result) {
            if event_matches_id(&decoded, id) {
                return Some((ev.transaction_id.clone(), decoded));
            }
        }
    }
    None
}

/// Walk the indexer's cursor pagination for `event_name` on `contract_b58`,
/// returning the first event matching `id` together with its transaction id
/// (R-L6).
async fn find_contract_event(
    api: &TronApiClient,
    contract_b58: &str,
    event_name: &str,
    id: &[u8; 32],
) -> Result<Option<(String, swap::SwapEvent)>, String> {
    let mut fingerprint: Option<String> = None;
    for _ in 0..EVENT_MAX_PAGES {
        let query = ContractEventsQuery {
            event_name: Some(event_name),
            only_confirmed: true,
            min_block: None,
            limit: EVENT_PAGE_LIMIT,
            fingerprint: fingerprint.as_deref(),
        };
        let resp = api
            .get_contract_events(contract_b58, &query)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(found) = first_matching_event(&resp.data, id) {
            return Ok(Some(found));
        }
        match resp.meta.fingerprint {
            Some(fp) if !fp.is_empty() && !resp.data.is_empty() => fingerprint = Some(fp),
            _ => break,
        }
    }
    Ok(None)
}

/// Confirm a discovered transaction via the per-tx receipt endpoint
/// (`gettransactioninfobyid`, §21.9): require the tx to be in a block and to
/// have not failed execution.
async fn confirm_via_receipt(api: &TronApiClient, txid: &str) -> Result<bool, String> {
    let info = api.get_transaction_info_by_id(txid).await.map_err(|e| e.to_string())?;
    if info.block_number.is_none() {
        return Ok(false);
    }
    if let Some(receipt) = &info.receipt {
        if let Some(result) = &receipt.result {
            if result != "SUCCESS" && result != "DEFAULT" {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Parse a Tron txID (SHA-256 hex) into an `H256`.
fn txid_to_h256(txid: &str) -> Result<H256, String> {
    let bytes = hex::decode(txid.strip_prefix("0x").unwrap_or(txid)).map_err(|e| format!("bad txid hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("tron txid must be 32 bytes, got {}", bytes.len()));
    }
    Ok(H256::from_slice(&bytes))
}

/// Surface a discovered swap transaction as a [`SignedTronTx`] (R-L6,
/// "surfaced as the spend or refund outcome").
///
/// The Tron node HTTP API (§21.9) exposes no raw-protobuf retrieval, so the
/// transaction body is reconstructed from the discovered event's call arguments
/// (e.g. the `receiverSpend` secret for a spend) and paired with the *real*
/// SHA-256 txID located through discovery. The reconstructed body keeps the
/// downstream secret-extraction (`extract_secret`) and id-decoding
/// (`decode_payment_args`) paths working over `tx_hex()`, while `tx_hash` is the
/// authentic on-chain identifier confirmed via the receipt endpoint.
fn surface_discovery_tx(contract: &TronAddress, data: Vec<u8>, txid: H256) -> SignedTronTx {
    let owner = TronAddress::from_evm_address(Address::zero());
    let tapos = proto::TaposBlockData {
        ref_block_bytes: vec![0u8; 2],
        ref_block_hash: vec![0u8; 8],
    };
    let raw = build_trigger_smart_contract(&owner, contract, 0, data, &tapos, 0, 0);
    let tx = proto::Transaction {
        raw_data: Some(raw),
        signature: vec![],
    };
    SignedTronTx {
        tx_bytes: tx.encode_to_vec(),
        tx_hash: txid,
    }
}

/// Reconstruct the spend (`receiverSpend`) call body from a discovered
/// `ReceiverSpent` event's id and revealed secret.
fn spend_call_data(id: [u8; 32], secret: [u8; 32]) -> Result<Vec<u8>, String> {
    swap::encode_receiver_spend(&swap::ReceiverSpendArgs {
        id,
        amount: U256::zero(),
        secret,
        token_addr: Address::zero(),
        sender: Address::zero(),
    })
    .map_err(|e| e.to_string())
}

/// Reconstruct the refund (`senderRefund`) call body from a discovered
/// `SenderRefunded` event's id.
fn refund_call_data(id: [u8; 32]) -> Result<Vec<u8>, String> {
    swap::encode_sender_refund(&id, U256::zero(), &[0u8; 32], Address::zero(), Address::zero())
        .map_err(|e| e.to_string())
}

/// Reconstruct the payment (`ethPayment` / `erc20Payment`) call body carrying a
/// discovered `PaymentSent` event's id, so the surfaced payment decodes to that
/// id downstream.
fn payment_call_data(coin: &EthCoin, id: [u8; 32]) -> Result<Vec<u8>, String> {
    match &coin.coin_type {
        EthCoinType::Tron => swap::encode_eth_payment(&swap::EthPaymentArgs {
            id,
            receiver: Address::zero(),
            secret_hash: [0u8; 32],
            lock_time: 0,
        })
        .map_err(|e| e.to_string()),
        EthCoinType::Trc20 { token_addr, .. } => swap::encode_erc20_payment(&swap::Erc20PaymentArgs {
            id,
            amount: U256::zero(),
            token_addr: *token_addr,
            receiver: Address::zero(),
            secret_hash: [0u8; 32],
            lock_time: 0,
        })
        .map_err(|e| e.to_string()),
        _ => Err("tron::swap_ops::payment_call_data invoked on a non-TRON coin".to_owned()),
    }
}

/// The swap `id` of a payment, decoded from its protobuf bytes (for the
/// market-ops `wait_for_tx_spend` path, which only knows the payment tx).
pub fn payment_swap_id(payment_tx: &[u8]) -> Result<[u8; 32], String> { Ok(decode_payment_args(payment_tx)?.id) }

/// Discover whether the payment under `id` was spent or refunded (R-L6): query
/// the indexed contract-event endpoint for `ReceiverSpent` (spend) and
/// `SenderRefunded` (refund), confirm the candidate transaction via the receipt
/// endpoint, and surface the outcome. Fails with the typed
/// [`swap::TronSwapError::NoEventEndpoint`] when no event endpoint is configured.
pub async fn search_for_swap_tx_spend(
    coin: &EthCoin,
    swap_contract: Address,
    id: [u8; 32],
) -> Result<Option<FoundSwapTxSpend>, String> {
    let api = tron_api(coin)?;
    require_event_indexer(&api)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let contract_b58 = contract.to_base58();

    if let Some((txid, swap::SwapEvent::ReceiverSpent { secret, .. })) =
        find_contract_event(&api, &contract_b58, "ReceiverSpent", &id).await?
    {
        if confirm_via_receipt(&api, &txid).await? {
            let tx = surface_discovery_tx(&contract, spend_call_data(id, secret)?, txid_to_h256(&txid)?);
            return Ok(Some(FoundSwapTxSpend::Spent(TransactionEnum::from(tx))));
        }
    }
    if let Some((txid, _)) = find_contract_event(&api, &contract_b58, "SenderRefunded", &id).await? {
        if confirm_via_receipt(&api, &txid).await? {
            let tx = surface_discovery_tx(&contract, refund_call_data(id)?, txid_to_h256(&txid)?);
            return Ok(Some(FoundSwapTxSpend::Refunded(TransactionEnum::from(tx))));
        }
    }
    Ok(None)
}

/// Discover a previously-sent payment under `id` through the `PaymentSent` event
/// (R-L6), surfacing the payment transaction. Fails with the typed
/// [`swap::TronSwapError::NoEventEndpoint`] when no event endpoint is configured.
pub async fn check_if_payment_sent(
    coin: &EthCoin,
    swap_contract: Address,
    id: [u8; 32],
) -> Result<Option<TransactionEnum>, String> {
    let api = tron_api(coin)?;
    require_event_indexer(&api)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let contract_b58 = contract.to_base58();
    match find_contract_event(&api, &contract_b58, "PaymentSent", &id).await? {
        Some((txid, _)) => {
            let tx = surface_discovery_tx(&contract, payment_call_data(coin, id)?, txid_to_h256(&txid)?);
            Ok(Some(TransactionEnum::from(tx)))
        },
        None => Ok(None),
    }
}

/// Single-shot lookup of a `ReceiverSpent` spend for the payment under `id`
/// (R-L6), used by the market-ops `wait_for_tx_spend` poll loop. Returns the
/// surfaced spend transaction when present and confirmed.
pub async fn find_htlc_spend(
    coin: &EthCoin,
    swap_contract: Address,
    id: [u8; 32],
) -> Result<Option<SignedTronTx>, String> {
    let api = tron_api(coin)?;
    require_event_indexer(&api)?;
    let contract = TronAddress::from_evm_address(swap_contract);
    let contract_b58 = contract.to_base58();
    if let Some((txid, swap::SwapEvent::ReceiverSpent { secret, .. })) =
        find_contract_event(&api, &contract_b58, "ReceiverSpent", &id).await?
    {
        if confirm_via_receipt(&api, &txid).await? {
            return Ok(Some(surface_discovery_tx(
                &contract,
                spend_call_data(id, secret)?,
                txid_to_h256(&txid)?,
            )));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// R-TF1: trade-fee estimation against the bandwidth/energy resource model
// ---------------------------------------------------------------------------

/// Fetch the per-unit chain prices (§21.8/§21.9) for fee estimation.
async fn fetch_prices(api: &TronApiClient) -> Result<TronChainPrices, String> {
    let params = api.get_chain_parameters().await.map_err(|e| e.to_string())?;
    Ok(TronChainPrices::from_chain_parameters(&params))
}

/// Fetch the account's bandwidth/energy quotas (§21.8/§21.9). A node/account
/// without usable resource data falls back to an empty quota, which makes the
/// estimate conservative (it assumes the fee is paid rather than covered).
async fn fetch_resources(api: &TronApiClient, address: &TronAddress) -> TronAccountResources {
    match api.get_account_resource(address).await {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => TronAccountResources::default(),
    }
}

/// Dry-run a contract call against the constant-contract endpoint (§21.9) for
/// its energy cost. `data` is the ABI-encoded selector + arguments; the energy
/// is `None` when the node is unavailable or returns no estimate, in which case
/// the caller falls back to a conservative default.
async fn dry_run_energy(
    api: &TronApiClient,
    sender: &TronAddress,
    contract: &TronAddress,
    selector: &str,
    data: &[u8],
) -> Option<i64> {
    if data.len() < 4 {
        return None;
    }
    let param_hex = hex::encode(&data[4..]);
    let resp = api
        .trigger_constant_contract(sender, contract, selector, &param_hex)
        .await
        .ok()?;
    resp.energy_used.and_then(|e| i64::try_from(e).ok())
}

/// Energy cost of the maker/taker HTLC *payment* call, estimated via a
/// constant-contract dry-run of `ethPayment` / `erc20Payment` (R-TF1).
async fn payment_dry_run_energy(
    coin: &EthCoin,
    api: &TronApiClient,
    sender: &TronAddress,
    contract: &TronAddress,
    amount_units: U256,
) -> Option<i64> {
    match &coin.coin_type {
        EthCoinType::Tron => {
            let data = swap::encode_eth_payment(&swap::EthPaymentArgs {
                id: [0u8; 32],
                receiver: coin.my_address,
                secret_hash: [0u8; 32],
                lock_time: 0,
            })
            .ok()?;
            dry_run_energy(
                api,
                sender,
                contract,
                "ethPayment(bytes32,address,bytes32,uint64)",
                &data,
            )
            .await
        },
        EthCoinType::Trc20 { token_addr, .. } => {
            let data = swap::encode_erc20_payment(&swap::Erc20PaymentArgs {
                id: [0u8; 32],
                amount: amount_units,
                token_addr: *token_addr,
                receiver: coin.my_address,
                secret_hash: [0u8; 32],
                lock_time: 0,
            })
            .ok()?;
            dry_run_energy(
                api,
                sender,
                contract,
                "erc20Payment(bytes32,uint256,address,address,bytes32,uint64)",
                &data,
            )
            .await
        },
        _ => None,
    }
}

/// Sender-side trade fee (R-TF1): the bandwidth/energy cost of the maker/taker
/// HTLC payment the coin will broadcast, computed against the Tron resource
/// model (§21.8) — *not* the EVM gas model.
pub async fn sender_trade_fee_details(coin: &EthCoin, amount: BigDecimal) -> Result<TronTxFeeDetails, String> {
    let api = tron_api(coin)?;
    let prices = fetch_prices(&api).await?;
    let sender = TronAddress::from_evm_address(coin.my_address);
    let resources = fetch_resources(&api, &sender).await;
    let contract = TronAddress::from_evm_address(coin.swap_contract_address);
    let amount_units = wei_from_big_decimal(&amount, coin.decimals).map_err(|e| e.to_string())?;
    let energy = payment_dry_run_energy(coin, &api, &sender, &contract, amount_units).await;
    Ok(fee::estimate_maker_taker_payment_fee(&prices, &resources, energy))
}

/// Fee-to-send-taker-fee (R-TF1, R-DF1): the bandwidth/energy cost of the
/// dex-fee transfer — a native TRX transfer for TRX, or a TRC20 transfer for a
/// TRC20 coin — computed against the Tron resource model (§21.8).
pub async fn fee_to_send_taker_fee_details(
    coin: &EthCoin,
    dex_fee_amount: BigDecimal,
) -> Result<TronTxFeeDetails, String> {
    let api = tron_api(coin)?;
    let prices = fetch_prices(&api).await?;
    let sender = TronAddress::from_evm_address(coin.my_address);
    let resources = fetch_resources(&api, &sender).await;
    match &coin.coin_type {
        EthCoinType::Tron => Ok(fee::estimate_fee_to_send_taker_fee(
            &prices, &resources, false, None, true,
        )),
        EthCoinType::Trc20 { token_addr, .. } => {
            let token = TronAddress::from_evm_address(*token_addr);
            let amount_units = wei_from_big_decimal(&dex_fee_amount, coin.decimals).map_err(|e| e.to_string())?;
            let data = swap::encode_trc20_transfer(coin.my_address, amount_units).map_err(|e| e.to_string())?;
            let energy = dry_run_energy(&api, &sender, &token, "transfer(address,uint256)", &data).await;
            Ok(fee::estimate_fee_to_send_taker_fee(
                &prices, &resources, true, energy, true,
            ))
        },
        _ => Err("tron::swap_ops::fee_to_send_taker_fee_details invoked on a non-TRON coin".to_owned()),
    }
}

/// Receiver-side trade fee (R-TF1): the bandwidth/energy cost of the
/// `receiverSpend` claim the receiver will broadcast, computed against the Tron
/// resource model (§21.8) — *not* the EVM gas model.
pub async fn receiver_trade_fee_details(coin: &EthCoin) -> Result<TronTxFeeDetails, String> {
    let api = tron_api(coin)?;
    let prices = fetch_prices(&api).await?;
    let sender = TronAddress::from_evm_address(coin.my_address);
    let resources = fetch_resources(&api, &sender).await;
    let contract = TronAddress::from_evm_address(coin.swap_contract_address);
    let dummy = swap::encode_receiver_spend(&swap::ReceiverSpendArgs {
        id: [0u8; 32],
        amount: U256::zero(),
        secret: [0u8; 32],
        token_addr: Address::zero(),
        sender: coin.my_address,
    })
    .map_err(|e| e.to_string())?;
    let energy = dry_run_energy(
        &api,
        &sender,
        &contract,
        "receiverSpend(bytes32,uint256,bytes32,address,address)",
        &dummy,
    )
    .await
    .unwrap_or(fee::SWAP_SPEND_ENERGY_DEFAULT);
    Ok(fee::estimate_swap_contract_call_fee(
        &prices,
        &resources,
        fee::SWAP_CALL_BANDWIDTH,
        energy,
    ))
}

// ---------------------------------------------------------------------------
// R-L7: confirmation tracking
// ---------------------------------------------------------------------------

/// The TRON txID (SHA-256 of the raw body) for a broadcast transaction's
/// protobuf bytes (R-L7) — *not* an Ethereum-style RLP hash.
pub fn tron_txid(tx_bytes: &[u8]) -> Result<H256, String> {
    let tx = proto::Transaction::decode(tx_bytes).map_err(|e| e.to_string())?;
    let raw = tx.raw_data.ok_or_else(|| "transaction has no raw_data".to_owned())?;
    Ok(hash_transaction_raw(&raw))
}

/// Whether a broadcast transaction has reached `confirmations` depth (R-L7):
/// identify it by its protobuf-derived txID and poll the Tron
/// transaction-receipt endpoint (§21.9). Returns `Ok(true)` once confirmed,
/// `Ok(false)` if not yet confirmed, and `Err` on a failed execution.
pub async fn check_confirmations(coin: &EthCoin, tx_bytes: &[u8], confirmations: u64) -> Result<bool, String> {
    let api = tron_api(coin)?;
    let txid = tron_txid(tx_bytes)?;
    let txid_hex = format!("{:x}", txid);

    let info = api
        .get_transaction_info_by_id(&txid_hex)
        .await
        .map_err(|e| e.to_string())?;

    let confirmed_at = match info.block_number {
        Some(b) => b,
        None => return Ok(false),
    };
    if let Some(receipt) = &info.receipt {
        if let Some(result) = &receipt.result {
            if result != "SUCCESS" && result != "DEFAULT" {
                return Err(format!("TRON tx {txid_hex} execution result is {result}"));
            }
        }
    }

    let head = api
        .get_now_block()
        .await
        .map_err(|e| e.to_string())?
        .block_header
        .raw_data
        .number;
    Ok(head >= confirmed_at && head - confirmed_at + 1 >= confirmations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-S2: a TRON HTLC uses a 32-byte SHA-256 secret-hash, distinct from the
    /// EVM 20-byte RIPEMD-160(SHA-256) form.
    #[test]
    fn tron_secret_hash_is_sha256_32_bytes() {
        let secret = [1u8; 32];
        let tron_hash = swap::sha256_secret_hash(&secret);
        assert_eq!(tron_hash.len(), 32);

        let evm_hash = kdf_crypto::dhash160(&secret).to_vec();
        assert_eq!(evm_hash.len(), 20);
        assert_ne!(tron_hash.to_vec(), evm_hash);
    }

    fn indexer_event(txid: &str, name: &str, fields: &[(&str, &str)]) -> ContractEvent {
        ContractEvent {
            transaction_id: txid.to_owned(),
            event_name: name.to_owned(),
            block_number: 1,
            result: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    /// R-L6: discovery selects the event whose decoded swap id matches the
    /// queried id, ignoring same-name events for other swaps.
    #[test]
    fn first_matching_event_picks_correct_swap_id() {
        let want = [0xabu8; 32];
        let want_hex = hex::encode(want);
        let other_hex = hex::encode([0xcdu8; 32]);
        let secret_hex = hex::encode([0x11u8; 32]);

        let events = vec![
            indexer_event("tx_other", "ReceiverSpent", &[
                ("id", &other_hex),
                ("secret", &secret_hex),
            ]),
            indexer_event("tx_match", "ReceiverSpent", &[
                ("id", &want_hex),
                ("secret", &secret_hex),
            ]),
        ];

        let (txid, ev) = first_matching_event(&events, &want).expect("matching event");
        assert_eq!(txid, "tx_match");
        match ev {
            swap::SwapEvent::ReceiverSpent { id, secret } => {
                assert_eq!(id, want);
                assert_eq!(secret, [0x11u8; 32]);
            },
            other => panic!("unexpected event {other:?}"),
        }
    }

    /// R-L6: no event matches => discovery reports nothing found (later surfaced
    /// as `None`, not a spurious match).
    #[test]
    fn first_matching_event_returns_none_without_match() {
        let want = [0x01u8; 32];
        let other_hex = hex::encode([0x02u8; 32]);
        let events = vec![indexer_event("tx", "SenderRefunded", &[("id", &other_hex)])];
        assert!(first_matching_event(&events, &want).is_none());
    }

    /// R-L6: a surfaced spend transaction round-trips through the downstream
    /// secret-extraction path, recovering the secret revealed by the event.
    #[test]
    fn surfaced_spend_tx_yields_secret() {
        let id = [0x07u8; 32];
        let secret = [0x42u8; 32];
        let contract = TronAddress::from_evm_address(Address::from([0x12u8; 20]));
        let txid = H256::from([0xfeu8; 32]);

        let data = spend_call_data(id, secret).unwrap();
        let tx = surface_discovery_tx(&contract, data, txid);
        assert_eq!(tx.tx_hash, txid);

        let extracted = extract_secret(&tx.tx_bytes).unwrap();
        assert_eq!(extracted, secret.to_vec());
    }

    /// R-L6: a surfaced payment transaction decodes back to the discovered swap
    /// id so downstream id-keyed logic works.
    #[test]
    fn surfaced_payment_tx_yields_swap_id() {
        let id = [0x09u8; 32];
        let contract = TronAddress::from_evm_address(Address::from([0x99u8; 20]));
        let txid = H256::from([0xc0u8; 32]);

        // Native TRX payment (ethPayment).
        let coin_type = EthCoinType::Tron;
        let data = match &coin_type {
            EthCoinType::Tron => swap::encode_eth_payment(&swap::EthPaymentArgs {
                id,
                receiver: Address::zero(),
                secret_hash: [0u8; 32],
                lock_time: 0,
            })
            .unwrap(),
            _ => unreachable!(),
        };
        let tx = surface_discovery_tx(&contract, data, txid);
        assert_eq!(payment_swap_id(&tx.tx_bytes).unwrap(), id);
    }

    /// R-L6 txID parsing: 32-byte SHA-256 hex (with or without 0x) parses; other
    /// lengths are rejected.
    #[test]
    fn txid_to_h256_parses_32_bytes() {
        let hex = "ee".repeat(32);
        let parsed = txid_to_h256(&hex).unwrap();
        assert_eq!(parsed, H256::from([0xeeu8; 32]));
        assert_eq!(txid_to_h256(&format!("0x{hex}")).unwrap(), parsed);
        assert!(txid_to_h256("dead").is_err());
    }
}
