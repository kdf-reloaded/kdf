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
use crate::my_tx_history_v2::{HistoryCoinType, TxHistoryStorage};
use crate::node_url::{host_only, scrub_urls_in_text};
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
use std::collections::{HashMap, HashSet};
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

/// The byte string an identifier is cut from: the position as a little-endian integer of `word_bytes` bytes (the
/// build's native word size: 8, or 4 in the 32-bit browser build), followed by the characters of the upper-case hex
/// hash (R36.14.10, R36.14.19 item 7).
fn id_byte_string(position: usize, tx_hash: &str, word_bytes: usize) -> Vec<u8> {
    let mut bytes = (position as u64).to_le_bytes()[..word_bytes].to_vec();
    bytes.extend_from_slice(tx_hash.as_bytes());
    bytes
}

/// **D36.11 (a).** The `internal_id` of the row of the transfer at `position` of the transaction `tx_hash`
/// (upper-case hex): the byte string of [`id_byte_string`] cut to 33 bytes and written in reverse byte order
/// (R36.14.10). The identifier depends on the build's word size, as at the reference; only the first 25 characters
/// of the hash take part in it on a 64-bit build.
pub(super) fn history_internal_id(position: usize, tx_hash: &str) -> BytesJson {
    history_internal_id_for_word(position, tx_hash, std::mem::size_of::<usize>())
}

/// [`history_internal_id`] for a build whose native word is `word_bytes` bytes (8 or 4); the 32-bit form is
/// reproduced in tests on a 64-bit build (T36.45 (viii)).
pub(super) fn history_internal_id_for_word(position: usize, tx_hash: &str, word_bytes: usize) -> BytesJson {
    let mut bytes = id_byte_string(position, tx_hash, word_bytes);
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
    let mut forward = id_byte_string(position, tx_hash, std::mem::size_of::<usize>());
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
    /// The attributes as read (decoded, R36.14.5).
    pub(super) attributes: Vec<(String, String)>,
    /// The attributes in the form the node delivered them: key, value and index flag, in order. The comparison of
    /// repeated events uses this form (R36.14.19 item 1).
    raw: Vec<(String, String, bool)>,
}

impl HistoryEvent {
    /// An event from the raw attribute pairs of a node, each with the index flag `true`. An attribute key may
    /// arrive base64-encoded (older nodes), in which case its value is base64-decoded; both forms are read
    /// (R36.14.5).
    pub(super) fn from_raw(kind: String, raw: impl IntoIterator<Item = (String, String)>) -> Self {
        HistoryEvent::from_raw_indexed(kind, raw.into_iter().map(|(key, value)| (key, value, true)))
    }

    /// An event from the attributes of a node as delivered: key, value and index flag.
    pub(super) fn from_raw_indexed(kind: String, raw: impl IntoIterator<Item = (String, String, bool)>) -> Self {
        let raw: Vec<(String, String, bool)> = raw.into_iter().collect();
        HistoryEvent {
            kind,
            attributes: raw.iter().map(|(key, value, _)| decode_attribute(key, value)).collect(),
            raw,
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
    /// The entries of the transaction's fee list, in the order the transaction encodes them (R36.14.22; D36.19 (b),
    /// D36.20 (b)).
    pub(super) fee: Vec<FeeEntry>,
    pub(super) gas_limit: u64,
    pub(super) events: Vec<HistoryEvent>,
    /// The raw signed transaction (D36.12 (b)).
    pub(super) raw_tx: Vec<u8>,
    /// What the decoded messages of the transaction pay, so that a genuine send of exactly the fee amount is not
    /// taken for the fee payment.
    pub(super) payments: Vec<MessagePayment>,
    /// The delegators of the transaction's staking messages by kind (D36.20 (b)).
    pub(super) staking: StakingMessages,
}

/// One coin of a fee list: base units and the denomination text.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FeeEntry {
    pub(super) amount: u128,
    pub(super) denom: String,
}

/// The `delegator_address` of each staking message of a transaction, by the kind of event the message causes
/// (D36.20 (b)): `MsgDelegate` -- `delegate`, `MsgUndelegate` -- `unbond`, `MsgWithdrawDelegatorReward` --
/// `withdraw_rewards`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct StakingMessages {
    pub(super) delegate: Vec<String>,
    pub(super) unbond: Vec<String>,
    pub(super) withdraw_rewards: Vec<String>,
}

impl StakingMessages {
    /// The delegator of the one message of a kind, when the transaction has exactly one (D36.20 (b)).
    fn only(addresses: &[String]) -> Option<&str> {
        match addresses {
            [only] => Some(only.as_str()),
            _ => None,
        }
    }
}

/// The staking messages of a decoded transaction.
pub(super) fn staking_messages(messages: &[cosmrs::Any]) -> StakingMessages {
    use cosmrs::proto::cosmos::distribution::v1beta1::MsgWithdrawDelegatorReward;
    use cosmrs::proto::cosmos::staking::v1beta1::{MsgDelegate, MsgUndelegate};

    let mut found = StakingMessages::default();
    for message in messages {
        let value = message.value.as_slice();
        match message.type_url.as_str() {
            "/cosmos.staking.v1beta1.MsgDelegate" => {
                if let Ok(decoded) = MsgDelegate::decode(value) {
                    found.delegate.push(decoded.delegator_address);
                }
            },
            "/cosmos.staking.v1beta1.MsgUndelegate" => {
                if let Ok(decoded) = MsgUndelegate::decode(value) {
                    found.unbond.push(decoded.delegator_address);
                }
            },
            "/cosmos.distribution.v1beta1.MsgWithdrawDelegatorReward" => {
                if let Ok(decoded) = MsgWithdrawDelegatorReward::decode(value) {
                    found.withdraw_rewards.push(decoded.delegator_address);
                }
            },
            _ => {},
        }
    }
    found
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

/// One coin of an amount text: its integer must fit an unsigned 64-bit integer (R36.14.19 item 4), else the coin is
/// not counted.
fn parse_coin(text: &str) -> Option<(BigDecimal, String)> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (digits, denom) = text.split_at(split);
    Some((BigDecimal::from(digits.parse::<u64>().ok()?), denom.to_owned()))
}

fn parse_coins(text: &str) -> Vec<(BigDecimal, String)> {
    text.split(',').filter_map(|part| parse_coin(part.trim())).collect()
}

/// What a denomination resolves to (D36.13 (a'), the lookup of R36.14.19 item 4): the platform denom compared
/// exactly, else the first coins-configuration entry whose denom equals it without regard to letter case. The map
/// holds the platform denom under its exact text and the entries under their lower-cased denom
/// ([`resolution_map`]), so the lookup is the exact key, then the lower-cased one. The set of denominations that
/// resolve decides which coins take a position and which rows are written.
fn resolve_denom<'a>(denoms: &'a HashMap<String, DenomInfo>, denom: &str) -> Option<&'a DenomInfo> {
    denoms.get(denom).or_else(|| denoms.get(&denom.to_lowercase()))
}

/// What the denominations of a platform coin resolve to now (D36.13 (a')): the platform denom under its exact text;
/// every coins-configuration entry that names a denom, of whatever platform, under its lower-cased denom, the first
/// entry of the file winning; the ticker of an entry is that of the activated token matching it, when there is one.
/// A denom that is neither (a custom token absent from the coins file) is not in the map and resolves to nothing.
fn resolution_map(coin: &TendermintCoin) -> HashMap<String, DenomInfo> {
    let platform = DenomInfo {
        ticker: coin.ticker.clone(),
        decimals: coin.decimals(),
        token_id: None,
    };
    let mut map: HashMap<String, DenomInfo> = HashMap::new();
    if let Some(ctx) = MmArc::from_weak(&coin.ctx) {
        for (denom, ticker, decimals) in coins_file_denoms(&ctx) {
            let info = if ticker == coin.ticker {
                platform.clone()
            } else {
                DenomInfo {
                    token_id: Some(token_id_of_denom(&denom)),
                    ticker,
                    decimals,
                }
            };
            map.entry(denom.to_lowercase()).or_insert(info);
        }
    }
    for token in coin.tokens_info.lock().values() {
        if let Some(info) = map.get_mut(&token.denom.to_string().to_lowercase()) {
            if info.token_id.is_some() {
                info.ticker = token.ticker.clone();
                info.decimals = token.decimals;
            }
        }
    }
    map.insert(coin.platform_denom().to_string(), platform);
    map
}

/// The lookup over the loaded coins configuration (D36.13 (a')): `(denom, ticker, decimals)` of every entry whose
/// `protocol_data` names a `denom` and `decimals`, in the order of the file.
fn coins_file_denoms(ctx: &MmArc) -> Vec<(String, String, u8)> {
    ctx.conf["coins"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let data = &entry["protocol"]["protocol_data"];
                    let decimals = u8::try_from(data["decimals"].as_u64()?).ok()?;
                    Some((
                        data["denom"].as_str()?.to_owned(),
                        entry["coin"].as_str()?.to_owned(),
                        decimals,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The record kept beside the store (D36.21 (b)): what every denomination of the map resolves to, sorted, as a JSON
/// array of texts. A change of it changes which transfers count or which ticker a row carries.
fn resolution_record(denoms: &HashMap<String, DenomInfo>) -> String {
    let mut lines: Vec<String> = denoms
        .iter()
        .map(|(denom, info)| {
            let kind = if info.token_id.is_some() { "token" } else { "platform" };
            format!("{denom}|{}|{}|{kind}", info.ticker, info.decimals)
        })
        .collect();
    lines.sort();
    json!(lines).to_string()
}

/// The coins of an amount text that meet the conditions of R36.14.19 item 4 on the amount: a u64 integer and a
/// denomination that resolves. The party attributes are checked by the caller.
fn counted_coins(text: &str, denoms: &HashMap<String, DenomInfo>) -> Vec<(BigDecimal, String)> {
    parse_coins(text)
        .into_iter()
        .filter(|(_, denom)| resolve_denom(denoms, denom).is_some())
        .collect()
}

/// R36.14.19 item 1: an event is dropped when it is identical to the event of the same type emitted just before it,
/// compared in the form the node delivered the events: the attributes' key, value and index flag, in order.
fn drop_repeated_events(events: &[HistoryEvent]) -> Vec<HistoryEvent> {
    let mut last: HashMap<&str, &HistoryEvent> = HashMap::new();
    let mut kept = Vec::new();
    for event in events {
        if last
            .get(event.kind.as_str())
            .is_some_and(|previous| previous.raw == event.raw)
        {
            continue;
        }
        last.insert(event.kind.as_str(), event);
        kept.push(event.clone());
    }
    kept
}

/// R36.14.19 item 5: a candidate with the sender, recipient and denomination of a transfer already in the list is
/// added to it (the earlier one keeps its kind); otherwise it is appended.
fn push_merged(list: &mut Vec<Transfer>, candidate: Transfer) {
    match list.iter_mut().find(|transfer| {
        transfer.sender == candidate.sender
            && transfer.recipient == candidate.recipient
            && transfer.denom == candidate.denom
    }) {
        Some(merged) => {
            merged.amount += &candidate.amount;
            merged.is_fee_payment = false;
        },
        None => list.push(candidate),
    }
}

/// The text of the fee payment's `transfer` event (R36.14.5; D36.19 (b)): the text of the whole fee list, the coins
/// separated by commas. `None` for an empty fee list, which has no fee payment.
fn fee_payment_text(facts: &TxFacts) -> Option<String> {
    if facts.fee.is_empty() {
        return None;
    }
    Some(
        facts
            .fee
            .iter()
            .map(|entry| format!("{}{}", entry.amount, entry.denom))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// The staking events without a `delegator` attribute take the `delegator_address` of the transaction's one staking
/// message of the kind when there is exactly one (D36.20 (b)); `fallback` is that address.
fn delegation_transfers(
    event: &HistoryEvent,
    denoms: &HashMap<String, DenomInfo>,
    fallback: Option<&str>,
) -> Vec<Transfer> {
    let (delegator, validator) = match (event.attr("delegator").or(fallback), event.attr("validator")) {
        (Some(delegator), Some(validator)) => (delegator, validator),
        _ => return Vec::new(),
    };
    let mut list = Vec::new();
    for (amount, denom) in counted_coins(event.attr("amount").unwrap_or_default(), denoms) {
        push_merged(&mut list, Transfer {
            sender: delegator.to_owned(),
            recipient: Some(validator.to_owned()),
            denom,
            amount,
            kind: TransferKind::Delegation,
            is_fee_payment: false,
        });
    }
    list
}

fn reward_claim_transfers(
    event: &HistoryEvent,
    denoms: &HashMap<String, DenomInfo>,
    fallback: Option<&str>,
) -> Vec<Transfer> {
    let (delegator, validator) = match (event.attr("delegator").or(fallback), event.attr("validator")) {
        (Some(delegator), Some(validator)) => (delegator, validator),
        _ => return Vec::new(),
    };
    let mut list = Vec::new();
    for (amount, denom) in counted_coins(event.attr("amount").unwrap_or_default(), denoms) {
        push_merged(&mut list, Transfer {
            sender: validator.to_owned(),
            recipient: Some(delegator.to_owned()),
            denom,
            amount,
            kind: TransferKind::RewardClaim,
            is_fee_payment: false,
        });
    }
    list
}

/// R36.14.21: an `unbond` event needs `delegator` (or the fallback of D36.20 (b)) and an amount with at least one
/// coin that counts, although the row's amount is zero whatever the text says; the row is in the platform
/// denomination.
fn undelegation_transfers(
    event: &HistoryEvent,
    platform_denom: &str,
    denoms: &HashMap<String, DenomInfo>,
    fallback: Option<&str>,
) -> Vec<Transfer> {
    let delegator = match event.attr("delegator").or(fallback) {
        Some(delegator) => delegator,
        None => return Vec::new(),
    };
    let mut list = Vec::new();
    for _ in counted_coins(event.attr("amount").unwrap_or_default(), denoms) {
        push_merged(&mut list, Transfer {
            sender: delegator.to_owned(),
            recipient: None,
            denom: platform_denom.to_owned(),
            amount: BigDecimal::from(0),
            kind: TransferKind::Undelegation,
            is_fee_payment: false,
        });
    }
    list
}

/// The module events a candidate of case 4 can take, in the fixed order of R36.14.20: the `create_htlc` events, the
/// `claim_htlc` events, then the three IBC types, each type from the last emitted to the first. **Isolated for
/// D36.22 (a)**, the pairing by order.
fn module_event_queue(events: &[HistoryEvent]) -> Vec<&HistoryEvent> {
    let mut queue = Vec::new();
    for kind in [
        "create_htlc",
        "claim_htlc",
        "non_fungible_token_packet",
        "ibc_transfer",
        "fungible_token_packet",
    ] {
        queue.extend(events.iter().rev().filter(|event| event.kind == kind));
    }
    queue
}

/// Corrects the parties of a candidate from the module event it took (R36.14.20). An event without the attributes
/// the correction needs corrects nothing; it is used up all the same.
fn apply_module_event(transfer: &mut Transfer, event: &HistoryEvent) {
    let sender = event.attr("sender");
    let receiver = event.attr("receiver");
    match event.kind.as_str() {
        "claim_htlc" => {
            if let Some(sender) = sender {
                transfer.sender = sender.to_owned();
                transfer.kind = TransferKind::HtlcClaim;
            }
        },
        kind => {
            if let (Some(sender), Some(receiver)) = (sender, receiver) {
                transfer.sender = sender.to_owned();
                transfer.recipient = Some(receiver.to_owned());
                transfer.kind = if kind == "create_htlc" {
                    TransferKind::HtlcCreate
                } else {
                    TransferKind::Ibc
                };
            }
        },
    }
}

/// The transfer list of a transaction (R36.14.19). `events` are already freed of repeated events.
///
/// Staking events decide the whole transaction (R36.14.5 cases 1 to 3). The reference reads the first event of the
/// type only; under D36.12 (b) Reloaded writes one row per validator, so the later events follow the first one's
/// transfers in the list. They come after every position the reference computes, so no reference identifier moves.
fn transfers_of(
    events: &[HistoryEvent],
    fee_text: Option<&str>,
    platform_denom: &str,
    denoms: &HashMap<String, DenomInfo>,
    staking: &StakingMessages,
) -> Vec<Transfer> {
    let of_kind = |kind: &str| -> Vec<&HistoryEvent> { events.iter().filter(|event| event.kind == kind).collect() };

    let delegations = of_kind("delegate");
    if !delegations.is_empty() {
        return delegations
            .into_iter()
            .flat_map(|event| delegation_transfers(event, denoms, StakingMessages::only(&staking.delegate)))
            .collect();
    }
    let undelegations = of_kind("unbond");
    if !undelegations.is_empty() {
        return undelegations
            .into_iter()
            .flat_map(|event| {
                undelegation_transfers(event, platform_denom, denoms, StakingMessages::only(&staking.unbond))
            })
            .collect();
    }
    let claims = of_kind("withdraw_rewards");
    if !claims.is_empty() {
        return claims
            .into_iter()
            .flat_map(|event| reward_claim_transfers(event, denoms, StakingMessages::only(&staking.withdraw_rewards)))
            .collect();
    }

    let transfer_events = of_kind("transfer");
    let only_transfer = transfer_events.len() == 1;
    let has_parties = |event: &HistoryEvent| {
        event.attr("sender").is_some() && event.attr("recipient").is_some() && event.attr("amount").is_some()
    };
    // With more than one transfer event, the first that moves the fee coin is the fee payment (R36.14.5).
    let fee_payment_index = if transfer_events.len() > 1 {
        transfer_events
            .iter()
            .position(|event| has_parties(event) && fee_text.is_some() && event.attr("amount") == fee_text)
    } else {
        None
    };

    let mut module_events = module_event_queue(events).into_iter();
    let mut transfers: Vec<Transfer> = Vec::new();
    // R36.14.19 item 3: from the last emitted to the first.
    for (index, event) in transfer_events.iter().enumerate().rev() {
        if Some(index) == fee_payment_index || !has_parties(event) {
            continue;
        }
        let (sender, recipient, amount_text) =
            match (event.attr("sender"), event.attr("recipient"), event.attr("amount")) {
                (Some(sender), Some(recipient), Some(amount)) => (sender, recipient, amount),
                _ => continue,
            };
        let is_fee_text = Some(amount_text) == fee_text;
        for (amount, denom) in counted_coins(amount_text, denoms) {
            let mut candidate = Transfer {
                sender: sender.to_owned(),
                recipient: Some(recipient.to_owned()),
                denom,
                amount,
                kind: TransferKind::Plain,
                is_fee_payment: only_transfer && is_fee_text,
            };
            if let Some(module_event) = module_events.next() {
                apply_module_event(&mut candidate, module_event);
            }
            push_merged(&mut transfers, candidate);
        }
    }
    transfers
}

/// An integer of base units as a decimal in display units, `None` only when the decimal text does not parse (it
/// always does).
fn display_amount(amount: u128, decimals: u8) -> Option<BigDecimal> {
    Some(BigDecimal::from_str(&amount.to_string()).ok()? / power_of_ten(decimals))
}

fn fee_details_of(coin: &str, amount: u128, display: BigDecimal, gas_limit: u64) -> TxFeeDetails {
    TxFeeDetails::Tendermint(TendermintFeeDetails {
        coin: coin.to_owned(),
        amount: display,
        // Not serialized and never read from a history row; the saturated value is only the in-memory form.
        uamount: u64::try_from(amount).unwrap_or(u64::MAX),
        gas_limit,
    })
}

/// The rows of one transaction (R36.14.5 -- R36.14.9, D36.12 (b), D36.13 (a'), D36.19 (b), D36.20 (b)); none when
/// no transfer yields a row.
///
/// The position that goes into the identifier is the place in the transfer list of R36.14.19, which counts a transfer
/// whether or not a row is written for it (an HTLC creation by another account, a transfer already stored); a coin
/// that does not resolve is not in the list.
pub(super) fn rows_for_tx(facts: &TxFacts, ctx: &ClassifyContext<'_>) -> Vec<RowDraft> {
    // R36.14.22 item 3: a fee entry that is read and does not fit an unsigned 64-bit integer leaves the transaction
    // out. The entries read are the first one and, under D36.19 (b), the one in the platform denom.
    let platform_entry = facts.fee.iter().find(|entry| entry.denom == ctx.platform_denom);
    if facts
        .fee
        .first()
        .into_iter()
        .chain(platform_entry)
        .any(|entry| entry.amount > u128::from(u64::MAX))
    {
        return Vec::new();
    }
    // An event repeated identically in direct succession counts once, per event type (R36.14.19 item 1).
    let events = drop_repeated_events(&facts.events);

    let fee_text = fee_payment_text(facts);
    let transfers = transfers_of(
        &events,
        fee_text.as_deref(),
        ctx.platform_denom,
        ctx.denoms,
        &facts.staking,
    );

    // The fee as it enters amounts (D36.19 (b), D36.20 (b)): the platform entry wherever it stands in the list; an
    // empty list is a fee of zero and has no fee row; a list without a platform entry enters no amount and has no
    // fee row, and `fee_details` names the coin of the first entry when it resolves.
    let zero = BigDecimal::from(0);
    let (fee_display, fee_row_allowed, fee_details) = match (platform_entry, facts.fee.first()) {
        (Some(entry), _) => {
            let display = match display_amount(entry.amount, ctx.platform_decimals) {
                Some(display) => display,
                None => return Vec::new(),
            };
            let details = fee_details_of(ctx.platform_ticker, entry.amount, display.clone(), facts.gas_limit);
            (Some(display), true, Some(details))
        },
        (None, None) => (
            Some(zero.clone()),
            false,
            Some(fee_details_of(ctx.platform_ticker, 0, zero.clone(), facts.gas_limit)),
        ),
        (None, Some(first)) => {
            let details = resolve_denom(ctx.denoms, &first.denom).and_then(|info| {
                let display = display_amount(first.amount, info.decimals)?;
                Some(fee_details_of(&info.ticker, first.amount, display, facts.gas_limit))
            });
            (None, false, details)
        },
    };

    let mut rows: Vec<RowDraft> = Vec::new();
    let mut fee_taken = false;
    let mut fee_row_for: Option<usize> = None;

    for (position, transfer) in transfers.iter().enumerate() {
        let info = match resolve_denom(ctx.denoms, &transfer.denom) {
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

        // R36.14.8 item 2, R36.14.22 item 4 and D36.12 (b): the first written entry this account sent (or a reward
        // claim) bears the fee once, in its own amounts when it is in the platform denom and as a fee row when it is
        // in a token denom; the fee payment itself already is the fee.
        let is_platform = info.token_id.is_none();
        if let Some(fee_display) = &fee_display {
            if !fee_taken && (sent_by_me || transfer.kind == TransferKind::RewardClaim) {
                fee_taken = true;
                if is_platform {
                    // Fee-only: the transfer is the fee payment and no message of the transaction pays it.
                    let is_fee_payment = transfer.is_fee_payment
                        && !facts.payments.iter().any(|payment| {
                            fee_text
                                .as_deref()
                                .is_some_and(|text| payment.explains(transfer.recipient.as_deref(), text))
                        });
                    if !is_fee_payment {
                        spent_by_me += fee_display;
                        total_amount += fee_display;
                    }
                } else if fee_row_allowed {
                    fee_row_for = Some(rows.len());
                }
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
            fee_details: fee_details.clone(),
            coin: info.ticker.clone(),
            internal_id: history_internal_id(position, &facts.hash),
            kmd_rewards: None,
            transaction_type,
            memo: Some(facts.memo.clone()),
        };
        rows.push(RowDraft {
            details,
            decimals: info.decimals,
            position: Some(position),
        });
    }

    // R36.14.8 item 3.
    if let (Some(index), Some(fee_display)) = (fee_row_for, &fee_display) {
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
            my_balance_change: &zero - fee_display,
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
    /// What the denominations resolve to in the current pass (D36.13 (a'), D36.21 (b)); read once at the start of a
    /// pass, so that one pass counts with one lookup.
    denoms: HashMap<String, DenomInfo>,
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
            denoms: HashMap::new(),
        }
    }

    /// D36.21 (b): the record of the denominations the lookup resolves is kept beside the store. When this pass
    /// resolves a different set than the one the rows were written under, the platform coin's store (the rows of the
    /// platform and of its tokens) is cleared, the fetcher starts again at the first page and the history state is
    /// `InProgress`, so the store becomes what the reference writes into an empty store under the same configuration.
    async fn rebuild_when_resolution_changed(&mut self) -> Result<(), String> {
        let ticker = self.coin.ticker.clone();
        let store_error = |e: mm2_err_handle::prelude::MmError<S::Error>| format!("{:?}", e.into_inner());
        let record = resolution_record(&self.denoms);
        let stored = self.storage.get_resolved_denoms(&ticker).await.map_err(store_error)?;
        if stored.as_deref() == Some(record.as_str()) {
            return Ok(());
        }
        if stored.is_some() {
            self.storage.clear_history(&ticker).await.map_err(store_error)?;
            self.cursors = [1, 1];
            self.first_pass_done = false;
            self.coin
                .set_history_sync_state(HistorySyncState::InProgress(json!({ "transactions_left": null })));
        }
        self.storage
            .set_resolved_denoms(&ticker, &record)
            .await
            .map_err(store_error)
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
        if let Err(e) = self.storage.backfill_address_index(&self.coin.ticker).await {
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
        self.denoms = resolution_map(&self.coin);
        self.rebuild_when_resolution_changed().await?;
        let queries = self.queries();
        for (index, query) in queries.iter().enumerate() {
            // D36.24 (b): the earliest page of this pass that held a transaction left out for want of a block time.
            let mut earliest_left_out: Option<u32> = None;
            loop {
                let page = self.cursors[index];
                match self.read_page(query, page).await {
                    Ok((total, left_out_for_time)) => {
                        if left_out_for_time && earliest_left_out.is_none() {
                            earliest_left_out = Some(page);
                        }
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
            if let Some(page) = earliest_left_out {
                // Every later pass reads from there again.
                self.cursors[index] = page;
            }
        }
        self.first_pass_done = true;
        self.coin.set_history_sync_state(HistorySyncState::Finished);
        Ok(())
    }

    /// Reads, classifies and stores one page; returns the total of the query and whether a transaction of the page
    /// was left out because no node had the time of its block (D36.24 (b)).
    async fn read_page(&self, query: &str, page: u32) -> Result<(u64, bool), PageError> {
        let response = search_page(&self.coin, query, page).await.map_err(PageError::Node)?;
        let role = if query.starts_with("coin_spent") {
            "spender"
        } else {
            "receiver"
        };
        debug!(
            "history {} {} page {}: total {}, {} transactions",
            self.coin.ticker,
            role,
            page,
            response.total_count,
            response.txs.len()
        );
        let (drafts, left_out_for_time) = self.drafts_of(&response).await?;
        self.store(drafts).await.map_err(PageError::Store)?;
        Ok((u64::from(response.total_count), left_out_for_time))
    }

    async fn drafts_of(&self, response: &TxSearchResponse) -> Result<(Vec<RowDraft>, bool), PageError> {
        let me = self.coin.account_id.to_string();
        let platform_denom = self.coin.platform_denom().to_string();
        let ctx = ClassifyContext {
            me: &me,
            platform_ticker: &self.coin.ticker,
            platform_denom: &platform_denom,
            platform_decimals: self.coin.decimals(),
            denoms: &self.denoms,
        };
        let mut block_times: HashMap<u64, Option<u64>> = HashMap::new();
        let mut drafts = Vec::new();
        let mut left_out_for_time = false;
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
            let height = entry.height.value();
            let timestamp = match block_times.get(&height) {
                Some(time) => *time,
                None => {
                    let time = block_time_from_any_node(&self.coin, height)
                        .await
                        .map_err(PageError::Node)?;
                    block_times.insert(height, time);
                    time
                },
            };
            let timestamp = match timestamp {
                Some(timestamp) => timestamp,
                None => {
                    left_out_for_time = true;
                    warn!(
                        "{}: history: the block query for height {} returned no time, the transaction {} is left out",
                        self.coin.ticker, height, entry.hash
                    );
                    continue;
                },
            };
            let events = entry
                .tx_result
                .events
                .iter()
                .map(|event| {
                    HistoryEvent::from_raw_indexed(
                        event.kind.clone(),
                        event
                            .attributes
                            .iter()
                            .map(|attribute| (attribute.key.clone(), attribute.value.clone(), attribute.index)),
                    )
                })
                .collect();
            let facts = TxFacts {
                hash: entry.hash.to_string().to_uppercase(),
                height,
                timestamp,
                memo: tx.body.memo.clone(),
                fee: tx
                    .auth_info
                    .fee
                    .amount
                    .iter()
                    .map(|coin| FeeEntry {
                        amount: coin.amount,
                        denom: coin.denom.to_string(),
                    })
                    .collect(),
                gas_limit: tx.auth_info.fee.gas_limit,
                events,
                raw_tx: entry.tx.clone(),
                payments: message_payments(&tx.body.messages),
                staking: staking_messages(&tx.body.messages),
            };
            drafts.extend(rows_for_tx(&facts, &ctx));
        }
        Ok((drafts, left_out_for_time))
    }

    /// Writes the rows that are not stored yet; a row whose identifier is stored is never written again
    /// (R36.14.10). Under D36.11 (a) the rows under the two earlier identifier forms are deleted first.
    async fn store(&self, drafts: Vec<RowDraft>) -> Result<(), String> {
        let ticker = self.coin.ticker.as_str();
        let store_error = |e: mm2_err_handle::prelude::MmError<S::Error>| format!("{:?}", e.into_inner());
        let mut written = Vec::new();
        let mut remove = Vec::new();
        let mut add = Vec::new();
        let mut page_ids = HashSet::new();
        for draft in drafts {
            if let Some(position) = draft.position {
                remove.extend(obsolete_internal_ids(position, &draft.details.tx_hash));
            }
            let stored = stored_form(draft.details.clone(), draft.decimals);
            if !page_ids.insert(format!("{:02x}", stored.internal_id)) {
                continue;
            }
            if self
                .storage
                .get_tx_from_history(ticker, &stored.internal_id)
                .await
                .map_err(store_error)?
                .is_some()
            {
                continue;
            }
            add.push(stored);
            written.push(draft.details);
        }
        // R36.14.24 item 3: the page is stored whole or not at all.
        self.storage
            .store_page(ticker, remove, add)
            .await
            .map_err(store_error)?;
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
    debug!(
        "history {}: tx_search page {} answered by node {}",
        coin.ticker,
        page,
        host_only(&client.uri().to_string())
    );
    client
        .perform(request)
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))
}

/// The time in seconds of the block at `height`; `None` when the node's answer carries no time for it
/// (R36.14.4).
async fn block_time(coin: &TendermintCoin, height: u64) -> Result<Option<u64>, String> {
    let client = coin
        .rpc_client()
        .await
        .map_err(|e| scrub_urls_in_text(&e.to_string()))?;
    block_time_on(&client, height).await
}

/// The time of the block at `height` from the node that answers first; when its answer carries no block or no time,
/// every other configured node is asked once (D36.24 (b)). `None` when no node has it. A failure of the first node
/// is returned as before; a failure of another node counts as that node not having the block.
async fn block_time_from_any_node(coin: &TendermintCoin, height: u64) -> Result<Option<u64>, String> {
    if let Some(time) = block_time(coin, height).await? {
        return Ok(Some(time));
    }
    let clients = coin.client.0.lock().await.rpc_clients.clone();
    for client in clients {
        match block_time_on(&client, height).await {
            Ok(Some(time)) => return Ok(Some(time)),
            Ok(None) => {},
            Err(e) => debug!(
                "history {}: a node could not answer the block query for height {}: {}",
                coin.ticker, height, e
            ),
        }
    }
    Ok(None)
}

async fn block_time_on(client: &HttpClient, height: u64) -> Result<Option<u64>, String> {
    let request = GetBlockByHeightRequest {
        height: i64::try_from(height).map_err(|e| e.to_string())?,
    };
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
