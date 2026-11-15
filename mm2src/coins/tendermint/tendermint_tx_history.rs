//! Transaction history of a Tendermint platform coin and its tokens (CRD ch.36 §36.14.1, R36.14.1 --
//! R36.14.12; decisions D36.10 -- D36.14).
//!
//! The module has four parts:
//!
//! 1. the identifier and the stored form of a row ([`history_internal_id`], [`stored_form`] and friends), which
//!    follow D36.11 (a): byte-compatible with the store of the reference (R36.14.10);
//! 2. the pure classification of one transaction into rows ([`rows_for_tx`], R36.14.5 -- R36.14.9);
//! 3. the background fetcher ([`start_tendermint_history`], R36.14.3, R36.14.4, R36.14.11);
//! 4. the two entry points of the history method ([`history_scope`], [`served_row`], R36.14.1, R36.14.2).
//!
//! The history method and the fetcher exist on a native build only (the browser build has no `my_tx_history`
//! store in this tree, `NotSupportedInWasm`), so the module is compiled for native targets only.

use super::htlc::CustomTendermintMsgType;
use super::rpc::*;
use super::tendermint_helpers::TendermintCommons;
use super::tendermint_types::{TendermintCoin, TendermintFeeDetails, ABCI_GET_BLOCK_BY_HEIGHT_PATH,
                              ABCI_REQUEST_HEIGHT, ABCI_REQUEST_PROVE};
use super::TENDERMINT_ASSET_PROTOCOL_TYPE;
use crate::my_tx_history_v2::{HistoryCoinType, TxHistoryStorage};
use crate::node_url::scrub_urls_in_text;
use crate::tx_history_streaming::{publish_tx_history_error, publish_tx_history_records};
use crate::{HistorySyncState, MarketCoinOps, MmCoinEnum, TransactionDetails, TransactionType, TxFeeDetails};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::log::{debug, warn};
use cosmrs::proto::cosmos::base::tendermint::v1beta1::{GetBlockByHeightRequest, GetBlockByHeightResponse};
use cosmrs::proto::prost::Message;
use kdf_crypto::sha256;
use mm2_core::mm_ctx::MmArc;
use rpc::v1::types::Bytes as BytesJson;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;

/// Results per `tx_search` page (R36.14.3).
const HISTORY_PAGE_SIZE: u8 = 50;
/// The pause after a node failure and the interval of the balance reads between passes (R36.14.3).
const HISTORY_PAUSE_SECS: f64 = 30.;
/// The length of a row identifier in bytes (R36.14.10).
const INTERNAL_ID_LEN: usize = 33;
/// The length of the two earlier identifier forms in bytes (finding 2 of §36.14.1).
const OBSOLETE_INTERNAL_ID_LEN: usize = 32;

// ————————————————————————————————————————————————————————————————
// Identifier and stored form (D36.11 (a), R36.14.10)
// ————————————————————————————————————————————————————————————————

/// The token id of a denomination: the SHA-256 of the lower-cased denom, 32 bytes (R36.14.2).
pub(crate) fn token_id_of_denom(denom: &str) -> BytesJson { sha256(denom.to_lowercase().as_bytes()).to_vec().into() }

/// The byte string an identifier is cut from: the position as a little-endian integer of the build's native word
/// size, followed by the characters of the upper-case hex hash (R36.14.10).
fn id_byte_string(position: usize, tx_hash: &str) -> Vec<u8> {
    let mut bytes = position.to_le_bytes().to_vec();
    bytes.extend_from_slice(tx_hash.as_bytes());
    bytes
}

/// **D36.11 (a).** The `internal_id` of the row of the transfer at `position` of the transaction `tx_hash`
/// (upper-case hex): the byte string of [`id_byte_string`] cut to 33 bytes and written in reverse byte order
/// (R36.14.10). The identifier depends on the build's word size, as at the reference; only the first 25 characters
/// of the hash take part in it on a 64-bit build.
pub(super) fn history_internal_id(position: usize, tx_hash: &str) -> BytesJson {
    let mut bytes = id_byte_string(position, tx_hash);
    bytes.truncate(INTERNAL_ID_LEN);
    bytes.reverse();
    bytes.into()
}

/// **D36.11 (a).** The identifier of the fee row of a token transaction: the bytes of the identifier of the token
/// row in forward order, that is the reverse of the token row's (R36.14.10).
pub(super) fn fee_row_internal_id(token_row_id: &BytesJson) -> BytesJson {
    let mut bytes = token_row_id.0.clone();
    bytes.reverse();
    bytes.into()
}

/// **D36.11 (a).** The two earlier 32-byte identifier forms of the same row, in reverse and in forward order,
/// which are deleted before a row is written (finding 2 of §36.14.1).
pub(super) fn obsolete_internal_ids(position: usize, tx_hash: &str) -> [BytesJson; 2] {
    let mut forward = id_byte_string(position, tx_hash);
    forward.truncate(OBSOLETE_INTERNAL_ID_LEN);
    let mut reversed = forward.clone();
    reversed.reverse();
    [reversed.into(), forward.into()]
}

/// `10^decimals` as a decimal number, for any number of decimals.
fn power_of_ten(decimals: u8) -> BigDecimal {
    let mut power = BigDecimal::from(1);
    for _ in 0..decimals {
        power = power * BigDecimal::from(10);
    }
    power
}

/// **D36.11 (a).** The form a row is stored in (R36.14.10): its three amounts as base-unit integers and a zero
/// balance change. The fee row of a token transaction is stored in display units, as it is served. `decimals` are
/// those of the row's own denomination.
pub(super) fn stored_form(mut row: TransactionDetails, decimals: u8) -> TransactionDetails {
    if row.transaction_type == TransactionType::FeeForTokenTx {
        return row;
    }
    let factor = power_of_ten(decimals);
    row.total_amount = (&row.total_amount * &factor).with_scale(0);
    row.spent_by_me = (&row.spent_by_me * &factor).with_scale(0);
    row.received_by_me = (&row.received_by_me * &factor).with_scale(0);
    row.my_balance_change = BigDecimal::from(0);
    row
}

/// **D36.11 (a).** The inverse of [`stored_form`], applied when a row is served (R36.14.7, R36.14.10): the three
/// amounts scaled to display units with the decimals of the coin or token the request names, and the balance
/// change recomputed. A stored amount that is not a non-negative integer is an error, never an abort (finding 1
/// of §36.14.1); the caller answers `StorageError`.
///
/// # Errors
///
/// Returns the description of the first stored amount that is not a non-negative integer.
pub(super) fn served_form(mut row: TransactionDetails, decimals: u8) -> Result<TransactionDetails, String> {
    if row.transaction_type == TransactionType::FeeForTokenTx {
        return Ok(row);
    }
    let zero = BigDecimal::from(0);
    for (name, amount) in [
        ("total_amount", &row.total_amount),
        ("spent_by_me", &row.spent_by_me),
        ("received_by_me", &row.received_by_me),
    ] {
        if !amount.is_integer() || amount < &zero {
            return Err(format!(
                "the stored {name} of transaction {} is not a non-negative integer",
                row.tx_hash
            ));
        }
    }
    let factor = power_of_ten(decimals);
    row.total_amount = &row.total_amount / &factor;
    row.spent_by_me = &row.spent_by_me / &factor;
    row.received_by_me = &row.received_by_me / &factor;
    row.my_balance_change = &row.received_by_me - &row.spent_by_me;
    Ok(row)
}

// ————————————————————————————————————————————————————————————————
// Classification of one transaction (R36.14.5 -- R36.14.9)
// ————————————————————————————————————————————————————————————————

/// What a denomination resolves to (D36.13): the ticker of the row, the decimals of the amounts and the token id
/// (`None` for the platform denom).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DenomInfo {
    pub(super) ticker: String,
    pub(super) decimals: u8,
    pub(super) token_id: Option<BytesJson>,
}

/// One event of a transaction's execution result, attribute keys and values as text.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HistoryEvent {
    pub(super) kind: String,
    pub(super) attributes: Vec<(String, String)>,
}

impl HistoryEvent {
    /// An event from the raw attribute pairs of a node. An attribute key may arrive base64-encoded (older
    /// nodes), in which case its value is base64-decoded; both forms are read (R36.14.5).
    pub(super) fn from_raw(kind: String, raw: impl IntoIterator<Item = (String, String)>) -> Self {
        HistoryEvent {
            kind,
            attributes: raw
                .into_iter()
                .map(|(key, value)| decode_attribute(&key, &value))
                .collect(),
        }
    }

    fn attr(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }
}

fn decode_attribute(key: &str, value: &str) -> (String, String) {
    if let Ok(key_bytes) = base64::decode(key) {
        if let Ok(decoded_key) = String::from_utf8(key_bytes) {
            // Every key this module reads is lower-case words joined by underscores.
            if !decoded_key.is_empty() && decoded_key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                let decoded_value = base64::decode(value)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_else(|| value.to_owned());
                return (decoded_key, decoded_value);
            }
        }
    }
    (key.to_owned(), value.to_owned())
}

/// The facts of a transaction the classification needs, read from the node's answer.
pub(super) struct TxFacts {
    /// The node's hash of the transaction, upper-case hex.
    pub(super) hash: String,
    pub(super) height: u64,
    /// The block time in seconds.
    pub(super) timestamp: u64,
    pub(super) memo: String,
    /// The first coin of the transaction's fee, in base units, and its denomination.
    pub(super) fee_amount: u128,
    pub(super) fee_denom: String,
    pub(super) gas_limit: u64,
    pub(super) events: Vec<HistoryEvent>,
    /// The raw signed transaction (D36.12 (b)).
    pub(super) raw_tx: Vec<u8>,
    /// What the decoded messages of the transaction pay, so that a genuine send of exactly the fee amount is not
    /// taken for the fee payment.
    pub(super) payments: Vec<MessagePayment>,
}

/// A decoded message that moves coins.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MessagePayment {
    /// A bank send: its recipient and the coin texts it sends.
    BankSend { recipient: String, amounts: Vec<String> },
    /// An IBC transfer or an HTLC creation: the bank transfer it causes goes to an escrow or module account, so
    /// only the transaction's having such a message counts.
    Escrowed,
}

impl MessagePayment {
    /// Whether this message is the origin of a bank transfer to `recipient` of the coin text `amount`.
    fn explains(&self, recipient: Option<&str>, amount: &str) -> bool {
        match self {
            MessagePayment::BankSend { recipient: to, amounts } => {
                Some(to.as_str()) == recipient && amounts.iter().any(|a| a == amount)
            },
            MessagePayment::Escrowed => true,
        }
    }
}

/// The payments of the messages of a decoded transaction.
pub(super) fn message_payments(messages: &[cosmrs::Any]) -> Vec<MessagePayment> {
    messages
        .iter()
        .filter_map(|message| {
            if message.type_url == "/cosmos.bank.v1beta1.MsgSend" {
                let send = cosmrs::proto::cosmos::bank::v1beta1::MsgSend::decode(message.value.as_slice()).ok()?;
                Some(MessagePayment::BankSend {
                    recipient: send.to_address,
                    amounts: send
                        .amount
                        .iter()
                        .map(|coin| format!("{}{}", coin.amount, coin.denom))
                        .collect(),
                })
            } else if message.type_url.ends_with(".MsgTransfer") || message.type_url.ends_with(".MsgCreateHTLC") {
                Some(MessagePayment::Escrowed)
            } else {
                None
            }
        })
        .collect()
}

/// The coin the rows belong to.
pub(super) struct ClassifyContext<'a> {
    /// The activated address.
    pub(super) me: &'a str,
    pub(super) platform_ticker: &'a str,
    pub(super) platform_denom: &'a str,
    pub(super) platform_decimals: u8,
    /// Denomination to what it resolves to; a denomination missing from it gets no row (D36.13).
    pub(super) denoms: &'a HashMap<String, DenomInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TransferKind {
    Plain,
    Ibc,
    HtlcCreate,
    HtlcClaim,
    Delegation,
    Undelegation,
    RewardClaim,
}

#[derive(Clone, Debug)]
struct Transfer {
    sender: String,
    recipient: Option<String>,
    denom: String,
    /// Base units.
    amount: BigDecimal,
    kind: TransferKind,
    /// The only `transfer` event of the result, and it moves exactly the transaction's fee: the transaction has no
    /// other transfer, and the fee payment is the row (D36.12 (b), counted once).
    is_fee_payment: bool,
}

/// A row with what the store needs beside it.
pub(super) struct RowDraft {
    /// The row in display units, as it is served and streamed.
    pub(super) details: TransactionDetails,
    /// The decimals of the row's own denomination, for [`stored_form`].
    pub(super) decimals: u8,
    /// The position of the transfer among the transfers of its transaction; `None` for the fee row of a token
    /// transaction.
    pub(super) position: Option<usize>,
}

fn parse_coin(text: &str) -> Option<(BigDecimal, String)> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (digits, denom) = text.split_at(split);
    Some((BigDecimal::from_str(digits).ok()?, denom.to_owned()))
}

fn parse_coins(text: &str) -> Vec<(BigDecimal, String)> {
    text.split(',').filter_map(|part| parse_coin(part.trim())).collect()
}

fn delegation_transfers(event: &HistoryEvent) -> Vec<Transfer> {
    let (delegator, validator) = match (event.attr("delegator"), event.attr("validator")) {
        (Some(delegator), Some(validator)) => (delegator, validator),
        _ => return Vec::new(),
    };
    parse_coins(event.attr("amount").unwrap_or_default())
        .into_iter()
        .map(|(amount, denom)| Transfer {
            sender: delegator.to_owned(),
            recipient: Some(validator.to_owned()),
            denom,
            amount,
            kind: TransferKind::Delegation,
            is_fee_payment: false,
        })
        .collect()
}

fn reward_claim_transfers(event: &HistoryEvent) -> Vec<Transfer> {
    let (delegator, validator) = match (event.attr("delegator"), event.attr("validator")) {
        (Some(delegator), Some(validator)) => (delegator, validator),
        _ => return Vec::new(),
    };
    parse_coins(event.attr("amount").unwrap_or_default())
        .into_iter()
        .map(|(amount, denom)| Transfer {
            sender: validator.to_owned(),
            recipient: Some(delegator.to_owned()),
            denom,
            amount,
            kind: TransferKind::RewardClaim,
            is_fee_payment: false,
        })
        .collect()
}

fn undelegation_transfer(event: &HistoryEvent, platform_denom: &str) -> Option<Transfer> {
    Some(Transfer {
        sender: event.attr("delegator")?.to_owned(),
        recipient: None,
        denom: platform_denom.to_owned(),
        amount: BigDecimal::from(0),
        kind: TransferKind::Undelegation,
        is_fee_payment: false,
    })
}

/// Corrects the parties of the plain transfers from the module events of the result (R36.14.5). One module event
/// corrects one transfer: the first one not corrected yet.
fn apply_module_events(transfers: &mut [Transfer], events: &[HistoryEvent]) {
    let has_htlc_event = events
        .iter()
        .any(|event| event.kind == "create_htlc" || event.kind == "claim_htlc");
    let mut corrected = vec![false; transfers.len()];
    for event in events {
        let kind = match (event.kind.as_str(), has_htlc_event) {
            ("create_htlc", _) => TransferKind::HtlcCreate,
            ("claim_htlc", _) => TransferKind::HtlcClaim,
            ("ibc_transfer", false) | ("fungible_token_packet", false) | ("non_fungible_token_packet", false) => {
                TransferKind::Ibc
            },
            _ => continue,
        };
        let sender = match event.attr("sender") {
            Some(sender) => sender,
            None => continue,
        };
        let receiver = event.attr("receiver");
        if kind != TransferKind::HtlcClaim && receiver.is_none() {
            continue;
        }
        let index = match corrected.iter().position(|done| !done) {
            Some(index) => index,
            None => break,
        };
        corrected[index] = true;
        let transfer = &mut transfers[index];
        transfer.sender = sender.to_owned();
        if kind != TransferKind::HtlcClaim {
            transfer.recipient = receiver.map(str::to_owned);
        }
        transfer.kind = kind;
    }
}

/// The transfers of a transaction, in the order R36.14.5 yields them. Under D36.12 (b) a transaction with several
/// delegations, undelegations or reward claims yields one transfer per event, one row per validator.
fn transfers_of(events: &[HistoryEvent], fee_text: &str, platform_denom: &str) -> Vec<Transfer> {
    let of_kind = |kind: &str| -> Vec<&HistoryEvent> { events.iter().filter(|event| event.kind == kind).collect() };

    let delegations = of_kind("delegate");
    if !delegations.is_empty() {
        return delegations.into_iter().flat_map(delegation_transfers).collect();
    }
    let undelegations = of_kind("unbond");
    if !undelegations.is_empty() {
        return undelegations
            .into_iter()
            .filter_map(|event| undelegation_transfer(event, platform_denom))
            .collect();
    }
    let claims = of_kind("withdraw_rewards");
    if !claims.is_empty() {
        return claims.into_iter().flat_map(reward_claim_transfers).collect();
    }

    let transfer_events = of_kind("transfer");
    let only_transfer = transfer_events.len() == 1;
    let mut fee_payment_left_out = false;
    let mut transfers: Vec<Transfer> = Vec::new();
    for event in &transfer_events {
        let (sender, recipient, amount_text) =
            match (event.attr("sender"), event.attr("recipient"), event.attr("amount")) {
                (Some(sender), Some(recipient), Some(amount)) => (sender, recipient, amount),
                _ => continue,
            };
        let is_fee_text = amount_text == fee_text;
        // With more than one transfer event, the one that moves the fee coin is the fee payment (R36.14.5).
        if transfer_events.len() > 1 && is_fee_text && !fee_payment_left_out {
            fee_payment_left_out = true;
            continue;
        }
        for (amount, denom) in parse_coins(amount_text) {
            match transfers.iter_mut().find(|transfer| {
                transfer.sender == sender && transfer.recipient.as_deref() == Some(recipient) && transfer.denom == denom
            }) {
                Some(merged) => {
                    merged.amount += &amount;
                    merged.is_fee_payment = false;
                },
                None => transfers.push(Transfer {
                    sender: sender.to_owned(),
                    recipient: Some(recipient.to_owned()),
                    denom,
                    amount,
                    kind: TransferKind::Plain,
                    is_fee_payment: only_transfer && is_fee_text,
                }),
            }
        }
    }
    apply_module_events(&mut transfers, events);
    transfers
}

/// The rows of one transaction (R36.14.5 -- R36.14.9, D36.12 (b), D36.13 (b)); none when no transfer yields a row.
///
/// The position that goes into the identifier counts the transfers as R36.14.5 yields them, including a transfer
/// that gets no row because its denomination does not resolve or because it is an HTLC creation by another account.
pub(super) fn rows_for_tx(facts: &TxFacts, ctx: &ClassifyContext<'_>) -> Vec<RowDraft> {
    let mut events = facts.events.clone();
    // An event repeated identically in direct succession counts once (R36.14.5).
    events.dedup();

    let fee_text = format!("{}{}", facts.fee_amount, facts.fee_denom);
    let transfers = transfers_of(&events, &fee_text, ctx.platform_denom);

    // The decimal text of an integer always parses; a transaction whose fee could not be read would be left out.
    let fee_base = match BigDecimal::from_str(&facts.fee_amount.to_string()) {
        Ok(fee_base) => fee_base,
        Err(_) => return Vec::new(),
    };
    let fee_display = &fee_base / &power_of_ten(ctx.platform_decimals);
    let fee_details = TxFeeDetails::Tendermint(TendermintFeeDetails {
        coin: ctx.platform_ticker.to_owned(),
        amount: fee_display.clone(),
        // Not serialized and never read from a history row; the saturated value is only the in-memory form.
        uamount: u64::try_from(facts.fee_amount).unwrap_or(u64::MAX),
        gas_limit: facts.gas_limit,
    });

    let zero = BigDecimal::from(0);
    let mut rows: Vec<RowDraft> = Vec::new();
    let mut fee_taken = false;
    let mut first_token_row_sent_by_me: Option<usize> = None;

    for (position, transfer) in transfers.iter().enumerate() {
        let info = match ctx.denoms.get(&transfer.denom) {
            Some(info) => info,
            None => continue,
        };
        let sent_by_me = transfer.sender == ctx.me;
        let recipient_list: Vec<String> = transfer.recipient.iter().cloned().collect();
        let parties = |from: &str| vec![from.to_owned()];

        let (transaction_type, from, to) = match transfer.kind {
            TransferKind::Plain => (
                match &info.token_id {
                    None => TransactionType::StandardTransfer,
                    Some(token_id) => TransactionType::TokenTransfer(token_id.clone()),
                },
                parties(&transfer.sender),
                recipient_list,
            ),
            TransferKind::Ibc => (
                TransactionType::TendermintIBCTransfer {
                    token_id: info.token_id.clone(),
                },
                parties(&transfer.sender),
                recipient_list,
            ),
            TransferKind::HtlcCreate => {
                if !sent_by_me {
                    continue;
                }
                (
                    TransactionType::CustomTendermintMsg {
                        msg_type: CustomTendermintMsgType::SendHtlcAmount,
                        token_id: info.token_id.clone(),
                    },
                    parties(ctx.me),
                    Vec::new(),
                )
            },
            TransferKind::HtlcClaim => (
                TransactionType::CustomTendermintMsg {
                    msg_type: if sent_by_me {
                        CustomTendermintMsgType::SignClaimHtlc
                    } else {
                        CustomTendermintMsgType::ClaimHtlcAmount
                    },
                    token_id: info.token_id.clone(),
                },
                parties(ctx.me),
                Vec::new(),
            ),
            TransferKind::Delegation => (
                TransactionType::StakingDelegation,
                parties(&transfer.sender),
                recipient_list,
            ),
            TransferKind::Undelegation => (TransactionType::RemoveDelegation, parties(ctx.me), Vec::new()),
            TransferKind::RewardClaim => (
                TransactionType::ClaimDelegationRewards,
                parties(&transfer.sender),
                recipient_list,
            ),
        };

        // R36.14.7.
        let amount = &transfer.amount / &power_of_ten(info.decimals);
        let pays_self = sent_by_me && transfer.recipient.as_deref() == Some(ctx.me);
        let is_claim = transfer.kind == TransferKind::HtlcClaim;
        let mut spent_by_me = if sent_by_me && !is_claim {
            amount.clone()
        } else {
            zero.clone()
        };
        let received_by_me = if !sent_by_me || pays_self {
            amount.clone()
        } else {
            zero.clone()
        };
        let mut total_amount = if is_claim && sent_by_me && !pays_self {
            zero.clone()
        } else {
            amount.clone()
        };

        // R36.14.8 item 2 and D36.12 (b): the first platform-denom transfer this account sent, or a reward claim,
        // bears the fee; the fee payment itself already is the fee.
        let is_platform = info.token_id.is_none();
        if !fee_taken && is_platform && (sent_by_me || transfer.kind == TransferKind::RewardClaim) {
            fee_taken = true;
            // Fee-only: the transfer is the fee payment and no message of the transaction pays it.
            let is_fee_payment = transfer.is_fee_payment
                && !facts
                    .payments
                    .iter()
                    .any(|payment| payment.explains(transfer.recipient.as_deref(), &fee_text));
            if !is_fee_payment {
                spent_by_me += &fee_display;
                total_amount += &fee_display;
            }
        }

        let details = TransactionDetails {
            tx_hex: facts.raw_tx.clone().into(),
            tx_json: None,
            tx_hash: facts.hash.clone(),
            from,
            to,
            total_amount,
            my_balance_change: &received_by_me - &spent_by_me,
            spent_by_me,
            received_by_me,
            block_height: facts.height,
            timestamp: facts.timestamp,
            fee_details: Some(fee_details.clone()),
            coin: info.ticker.clone(),
            internal_id: history_internal_id(position, &facts.hash),
            kmd_rewards: None,
            transaction_type,
            memo: Some(facts.memo.clone()),
        };
        if !is_platform && sent_by_me && first_token_row_sent_by_me.is_none() {
            first_token_row_sent_by_me = Some(rows.len());
        }
        rows.push(RowDraft {
            details,
            decimals: info.decimals,
            position: Some(position),
        });
    }

    // R36.14.8 item 3.
    if let (false, Some(index)) = (fee_taken, first_token_row_sent_by_me) {
        let token_row = &rows[index].details;
        let fee_row = TransactionDetails {
            tx_hex: token_row.tx_hex.clone(),
            tx_json: None,
            tx_hash: token_row.tx_hash.clone(),
            from: token_row.from.clone(),
            to: Vec::new(),
            total_amount: fee_display.clone(),
            spent_by_me: fee_display.clone(),
            received_by_me: zero.clone(),
            my_balance_change: &zero - &fee_display,
            block_height: token_row.block_height,
            timestamp: token_row.timestamp,
            fee_details: token_row.fee_details.clone(),
            coin: ctx.platform_ticker.to_owned(),
            internal_id: fee_row_internal_id(&token_row.internal_id),
            kmd_rewards: None,
            transaction_type: TransactionType::FeeForTokenTx,
            memo: token_row.memo.clone(),
        };
        rows.push(RowDraft {
            details: fee_row,
            decimals: ctx.platform_decimals,
            position: None,
        });
    }
    rows
}

// ————————————————————————————————————————————————————————————————
// The entry points of the history method (R36.14.1, R36.14.2)
// ————————————————————————————————————————————————————————————————

/// The store scope of a Tendermint coin or token and the decimals its served rows are scaled with, or `None` for a
/// coin of another family. The scope names the activated address, so only the rows that list it in `from` or `to`
/// are served (R36.14.2).
pub fn history_scope(coin: &MmCoinEnum) -> Option<(HistoryCoinType, u8)> {
    match coin {
        MmCoinEnum::TendermintCoin(platform) => Some((
            HistoryCoinType::AccountCoin {
                ticker: platform.ticker().to_owned(),
                address: platform.account_id.to_string(),
            },
            platform.decimals(),
        )),
        MmCoinEnum::TendermintToken(token) => Some((
            HistoryCoinType::AccountToken {
                platform: token.platform_coin.ticker().to_owned(),
                token_id: token_id_of_denom(&token.denom.to_string()),
                address: token.platform_coin.account_id.to_string(),
            },
            token.decimals,
        )),
        _ => None,
    }
}

/// A stored row in the form it is served in (R36.14.7, R36.14.10).
///
/// # Errors
///
/// Returns the description of a stored amount that is not a non-negative integer; the history method answers
/// `StorageError`.
pub fn served_row(row: TransactionDetails, decimals: u8) -> Result<TransactionDetails, String> {
    served_form(row, decimals)
}

// ————————————————————————————————————————————————————————————————
// The background fetcher (R36.14.3, R36.14.4, R36.14.11; D36.10 (b))
// ————————————————————————————————————————————————————————————————

enum PageError {
    /// A node failed; the page is retried after a pause.
    Node(String),
    /// The history store could not be written; the fetcher stops.
    Store(String),
}

type TxSearchResponse = tendermint_rpc::endpoint::tx_search::Response;

/// Starts the history fetcher of a platform coin: sets the history state to `InProgress` at once and returns the
/// future that runs the fetcher. The future never completes on its own; it ends with an error text only when the
/// fetcher stops for good, after the state became `Error`.
pub fn start_tendermint_history<S: TxHistoryStorage>(
    coin: TendermintCoin,
    storage: S,
) -> impl Future<Output = Result<(), String>> + Send {
    HistoryFetcher::new(coin, storage, HISTORY_PAUSE_SECS).run()
}

pub(super) struct HistoryFetcher<S: TxHistoryStorage> {
    coin: TendermintCoin,
    storage: S,
    /// The pause after a node failure and between the balance reads.
    pause: f64,
    /// For each of the two queries, the page the next pass starts at (R36.14.3).
    cursors: [u32; 2],
    first_pass_done: bool,
}

impl<S: TxHistoryStorage> HistoryFetcher<S> {
    fn new(coin: TendermintCoin, storage: S, pause: f64) -> Self {
        coin.set_history_sync_state(HistorySyncState::InProgress(json!({ "transactions_left": null })));
        HistoryFetcher {
            coin,
            storage,
            pause,
            cursors: [1, 1],
            first_pass_done: false,
        }
    }

    /// The two event queries, walked in turn (R36.14.3).
    fn queries(&self) -> [String; 2] {
        let address = self.coin.account_id.to_string();
        [
            format!("coin_spent.spender = '{}'", address),
            format!("coin_received.receiver = '{}'", address),
        ]
    }

    fn fail(&self, message: String) -> String {
        self.coin
            .set_history_sync_state(HistorySyncState::Error(json!({ "message": message })));
        if let Some(ctx) = MmArc::from_weak(&self.coin.ctx) {
            publish_tx_history_error(&ctx, &self.coin.ticker, message.clone());
        }
        message
    }

    pub(super) async fn run(mut self) -> Result<(), String> {
        if let Err(e) = self.storage.init(&self.coin.ticker).await {
            let message = format!("the history store could not be prepared: {:?}", e.into_inner());
            return Err(self.fail(message));
        }
        // A failed first balance read is retried, not silent (D36.10 (b)).
        let mut snapshot = loop {
            match self.read_balances().await {
                Ok(balances) => break balances,
                Err(e) => {
                    warn!(
                        "{}: the first balance read of the history fetcher failed: {}",
                        self.coin.ticker, e
                    );
                    Timer::sleep(self.pause).await;
                },
            }
        };
        loop {
            if let Err(e) = self.pass().await {
                return Err(self.fail(e));
            }
            // Wait for a balance change, which starts another pass (R36.14.3).
            loop {
                Timer::sleep(self.pause).await;
                match self.read_balances().await {
                    Ok(balances) if balances != snapshot => {
                        snapshot = balances;
                        break;
                    },
                    Ok(_) => {},
                    Err(e) => debug!(
                        "{}: balance read of the history fetcher failed: {}",
                        self.coin.ticker, e
                    ),
                }
            }
        }
    }

    /// One pass over the two queries, each resumed at its cursor. A node failure pauses and retries the failed
    /// page; only a store failure ends the pass with an error. The history state becomes `Finished` when the
    /// first pass completes.
    pub(super) async fn pass(&mut self) -> Result<(), String> {
        let queries = self.queries();
        for (index, query) in queries.iter().enumerate() {
            loop {
                let page = self.cursors[index];
                match self.read_page(query, page).await {
                    Ok(total) => {
                        if !self.first_pass_done {
                            let left = total.saturating_sub(u64::from(page) * u64::from(HISTORY_PAGE_SIZE));
                            self.coin.set_history_sync_state(HistorySyncState::InProgress(
                                json!({ "transactions_left": left }),
                            ));
                        }
                        if u64::from(page) * u64::from(HISTORY_PAGE_SIZE) >= total {
                            // The last page stays the cursor: the next pass reads it again (R36.14.3).
                            break;
                        }
                        self.cursors[index] = page + 1;
                    },
                    Err(PageError::Node(e)) => {
                        warn!("{}: history page {} failed, retrying: {}", self.coin.ticker, page, e);
                        Timer::sleep(self.pause).await;
                    },
                    Err(PageError::Store(e)) => return Err(e),
                }
            }
        }
        self.first_pass_done = true;
        self.coin.set_history_sync_state(HistorySyncState::Finished);
        Ok(())
    }

    /// Reads, classifies and stores one page; returns the total of the query.
    async fn read_page(&self, query: &str, page: u32) -> Result<u64, PageError> {
        let response = search_page(&self.coin, query, page).await.map_err(PageError::Node)?;
        let drafts = self.drafts_of(&response).await?;
        self.store(drafts).await.map_err(PageError::Store)?;
        Ok(u64::from(response.total_count))
    }

    /// The denominations that get rows, by D36.13 (b): the platform coin, then a token activated on this platform
    /// (custom tokens included), then a coins-configuration entry whose platform is this coin.
    fn denom_map(&self) -> HashMap<String, DenomInfo> {
        let mut map = HashMap::new();
        if let Some(ctx) = MmArc::from_weak(&self.coin.ctx) {
            if let Some(entries) = ctx.conf["coins"].as_array() {
                for entry in entries {
                    let protocol = &entry["protocol"];
                    let data = &protocol["protocol_data"];
                    if protocol["type"].as_str() != Some(TENDERMINT_ASSET_PROTOCOL_TYPE)
                        || data["platform"].as_str() != Some(self.coin.ticker.as_str())
                    {
                        continue;
                    }
                    let decimals = data["decimals"].as_u64().and_then(|d| u8::try_from(d).ok());
                    if let (Some(denom), Some(decimals), Some(ticker)) =
                        (data["denom"].as_str(), decimals, entry["coin"].as_str())
                    {
                        map.insert(denom.to_owned(), DenomInfo {
                            ticker: ticker.to_owned(),
                            decimals,
                            token_id: Some(token_id_of_denom(denom)),
                        });
                    }
                }
            }
        }
        for info in self.coin.tokens_info.lock().values() {
            let denom = info.denom.to_string();
            map.insert(denom.clone(), DenomInfo {
                ticker: info.ticker.clone(),
                decimals: info.decimals,
                token_id: Some(token_id_of_denom(&denom)),
            });
        }
        let platform_denom = self.coin.platform_denom().to_string();
        map.insert(platform_denom, DenomInfo {
            ticker: self.coin.ticker.clone(),
            decimals: self.coin.decimals(),
            token_id: None,
        });
        map
    }

    async fn drafts_of(&self, response: &TxSearchResponse) -> Result<Vec<RowDraft>, PageError> {
        let denoms = self.denom_map();
        let me = self.coin.account_id.to_string();
        let platform_denom = self.coin.platform_denom().to_string();
        let ctx = ClassifyContext {
            me: &me,
            platform_ticker: &self.coin.ticker,
            platform_denom: &platform_denom,
            platform_decimals: self.coin.decimals(),
            denoms: &denoms,
        };
        let mut block_times: HashMap<u64, Option<u64>> = HashMap::new();
        let mut drafts = Vec::new();
        for entry in &response.txs {
            // R36.14.4: left out entirely, without stopping the pass.
            if !entry.tx_result.code.is_ok() {
                continue;
            }
            let tx = match cosmrs::Tx::from_bytes(&entry.tx) {
                Ok(tx) => tx,
                Err(_) => continue,
            };
            if tx.body.messages.is_empty() {
                continue;
            }
            let fee_coin = match tx.auth_info.fee.amount.first() {
                Some(coin) => coin,
                None => continue,
            };
            let height = entry.height.value();
            let timestamp = match block_times.get(&height) {
                Some(time) => *time,
                None => {
                    let time = block_time(&self.coin, height).await.map_err(PageError::Node)?;
                    block_times.insert(height, time);
                    time
                },
            };
            let timestamp = match timestamp {
                Some(timestamp) => timestamp,
                None => continue,
            };
            let events = entry
                .tx_result
                .events
                .iter()
                .map(|event| {
                    HistoryEvent::from_raw(
                        event.kind.clone(),
                        event
                            .attributes
                            .iter()
                            .map(|attribute| (attribute.key.clone(), attribute.value.clone())),
                    )
                })
                .collect();
            let facts = TxFacts {
                hash: entry.hash.to_string().to_uppercase(),
                height,
                timestamp,
                memo: tx.body.memo.clone(),
                fee_amount: fee_coin.amount,
                fee_denom: fee_coin.denom.to_string(),
                gas_limit: tx.auth_info.fee.gas_limit,
                events,
                raw_tx: entry.tx.clone(),
                payments: message_payments(&tx.body.messages),
            };
            drafts.extend(rows_for_tx(&facts, &ctx));
        }
        Ok(drafts)
    }

    /// Writes the rows that are not stored yet; a row whose identifier is stored is never written again
    /// (R36.14.10). Under D36.11 (a) the rows under the two earlier identifier forms are deleted first.
    async fn store(&self, drafts: Vec<RowDraft>) -> Result<(), String> {
        let ticker = self.coin.ticker.as_str();
        let store_error = |e: mm2_err_handle::prelude::MmError<S::Error>| format!("{:?}", e.into_inner());
        let mut written = Vec::new();
        for draft in drafts {
            if let Some(position) = draft.position {
                for old_id in obsolete_internal_ids(position, &draft.details.tx_hash) {
                    self.storage
                        .remove_tx_from_history(ticker, &old_id)
                        .await
                        .map_err(store_error)?;
                }
            }
            let stored = stored_form(draft.details.clone(), draft.decimals);
            if self
                .storage
                .get_tx_from_history(ticker, &stored.internal_id)
                .await
                .map_err(store_error)?
                .is_some()
            {
                continue;
            }
            self.storage
                .add_transactions_to_history(ticker, vec![stored])
                .await
                .map_err(store_error)?;
            written.push(draft.details);
        }
        // CRD ch.10 R43: the stream carries what the history method serves.
        if let Some(ctx) = MmArc::from_weak(&self.coin.ctx) {
            for row in written {
                let row_ticker = row.coin.clone();
                publish_tx_history_records(&ctx, &row_ticker, [row]);
            }
        }
        Ok(())
    }

    /// The balances of the platform coin and of the activated tokens, in a fixed order, as the change detector.
    async fn read_balances(&self) -> Result<Vec<(String, u64)>, String> {
        let mut denoms: Vec<(String, String)> = self
            .coin
            .tokens_info
            .lock()
            .values()
            .map(|info| (info.ticker.clone(), info.denom.to_string()))
            .collect();
        denoms.sort();
        denoms.insert(0, (self.coin.ticker.clone(), self.coin.platform_denom().to_string()));
        let mut balances = Vec::with_capacity(denoms.len());
        for (ticker, denom) in denoms {
            let balance = self
                .coin
                .account_balance_for_denom(&self.coin.account_id, denom)
                .await
                .map_err(|e| scrub_urls_in_text(&e.to_string()))?;
            balances.push((ticker, balance));
        }
        Ok(balances)
    }
}

/// One page of `tx_search`, ascending and without proofs (R36.14.3).
async fn search_page(coin: &TendermintCoin, query: &str, page: u32) -> Result<TxSearchResponse, String> {
    let request = TxSearchRequest {
        query: query.to_owned(),
        order_by: TendermintResultOrder::Ascending.into(),
        page,
        per_page: HISTORY_PAGE_SIZE,
        prove: false,
    };
    let client = coin
        .rpc_client()
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))?;
    client
        .perform(request)
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))
}

/// The time in seconds of the block at `height`; `None` when the node's answer carries no time for it
/// (R36.14.4).
async fn block_time(coin: &TendermintCoin, height: u64) -> Result<Option<u64>, String> {
    let request = GetBlockByHeightRequest {
        height: i64::try_from(height).map_err(|e| e.to_string())?,
    };
    let client = coin
        .rpc_client()
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))?;
    let answer = client
        .abci_query(
            Some(ABCI_GET_BLOCK_BY_HEIGHT_PATH.to_string()),
            request.encode_to_vec(),
            ABCI_REQUEST_HEIGHT,
            ABCI_REQUEST_PROVE,
        )
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))?;
    let decoded = GetBlockByHeightResponse::decode(answer.value.as_slice()).map_err(|e| e.to_string())?;
    let from_block = decoded
        .block
        .as_ref()
        .and_then(|block| block.header.as_ref())
        .and_then(|header| header.time.as_ref())
        .map(|time| time.seconds);
    let from_sdk_block = decoded
        .sdk_block
        .as_ref()
        .and_then(|block| block.header.as_ref())
        .and_then(|header| header.time.as_ref())
        .map(|time| time.seconds);
    Ok(from_block
        .or(from_sdk_block)
        .and_then(|seconds| u64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0))
}

#[cfg(test)]
impl<S: TxHistoryStorage> HistoryFetcher<S> {
    /// The fetcher of the tests: a short pause, so a retry does not wait 30 seconds.
    pub(super) fn for_test(coin: TendermintCoin, storage: S) -> Self { HistoryFetcher::new(coin, storage, 0.01) }
}
