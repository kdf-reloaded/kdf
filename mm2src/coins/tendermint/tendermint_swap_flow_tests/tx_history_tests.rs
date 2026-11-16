//! Tests of the Tendermint transaction history (CRD ch.36 §36.14.1: T36.30 -- T36.39).
//!
//! The classification tests feed the events of a transaction to the pure function and need no node. The fetcher
//! tests run it against the scripted node of the swap-flow tests, which serves `tx_search`, the block query and the
//! balance query from a list kept in the test. The platform coin has 6 decimals, the token `TOK` (denom `utoken`)
//! 6 decimals, and every fee is 5,000 base units of the platform denom (0.005). The figures are computed from the
//! rules. No network is used.

use super::super::htlc::CustomTendermintMsgType;
use super::super::tendermint_tx_history::{fee_row_internal_id, history_internal_id, history_internal_id_for_word,
                                          obsolete_internal_ids, rows_for_tx, served_form, stored_form,
                                          token_id_of_denom, ClassifyContext, DenomInfo, FeeEntry, HistoryEvent,
                                          HistoryFetcher, MessagePayment, RowDraft, StakingMessages, TxFacts};
use super::*;
use crate::my_tx_history_v2::{my_tx_history_v2_rpc, HistoryCoinType, MyTxHistoryErrorV2, TxHistoryStorage};
use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
use crate::{HistorySyncState, TransactionType, TxFeeDetails};
use common::{HttpStatusCode, PagingOptionsEnum, StatusCode};
use cosmrs::proto::cosmos::bank::v1beta1::QueryBalanceResponse;
use rpc::v1::types::Bytes as BytesJson;
use std::collections::HashMap;

const ME: &str = "iaa1me";
const PEER: &str = "iaa1peer";
const COLLECTOR: &str = "iaa1feecollector";
const VALIDATOR: &str = "iaavaloper1validator";
const OTHER_CHAIN: &str = "cosmos1otherchain";
const MODULE: &str = "iaa1htlcmodule";
const HASH: &str = "9924962C7A343AB394FCF5FE3D1F0A2B7C6E58491B0D3F7A5E2C8B6D4F1A0E93";
const BLOCK_TIME: u64 = 1_700_000_000;

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).expect("a decimal amount") }

// ---------------------------------------------------------------------------
// Classification (R36.14.5 -- R36.14.9)
// ---------------------------------------------------------------------------

fn event(kind: &str, attributes: &[(&str, &str)]) -> HistoryEvent {
    HistoryEvent::from_raw(
        kind.to_owned(),
        attributes.iter().map(|(k, v)| (k.to_string(), v.to_string())),
    )
}

fn transfer(sender: &str, recipient: &str, amount: &str) -> HistoryEvent {
    event("transfer", &[
        ("sender", sender),
        ("recipient", recipient),
        ("amount", amount),
    ])
}

/// The fee payment of an account: the bank's `transfer` event of the fee coin to the fee collector.
fn fee_from(payer: &str) -> HistoryEvent { transfer(payer, COLLECTOR, "5000uiris") }

fn denoms() -> HashMap<String, DenomInfo> {
    let mut map = HashMap::new();
    map.insert("uiris".to_owned(), DenomInfo {
        ticker: "IRIS".to_owned(),
        decimals: 6,
        token_id: None,
    });
    map.insert("utoken".to_owned(), DenomInfo {
        ticker: "TOK".to_owned(),
        decimals: 6,
        token_id: Some(token_id_of_denom("utoken")),
    });
    map
}

fn rows(events: Vec<HistoryEvent>) -> Vec<RowDraft> { rows_with(events, vec![]) }

fn rows_with(events: Vec<HistoryEvent>, payments: Vec<MessagePayment>) -> Vec<RowDraft> {
    rows_with_fee(events, payments, 5_000, "uiris")
}

fn rows_with_fee(
    events: Vec<HistoryEvent>,
    payments: Vec<MessagePayment>,
    fee_amount: u128,
    fee_denom: &str,
) -> Vec<RowDraft> {
    rows_with_fee_list(events, payments, &[(fee_amount, fee_denom)])
}

/// A transaction with the given fee list (empty for a transaction without a fee).
fn rows_with_fee_list(events: Vec<HistoryEvent>, payments: Vec<MessagePayment>, fee: &[(u128, &str)]) -> Vec<RowDraft> {
    rows_of(events, payments, fee, StakingMessages::default())
}

fn rows_of(
    events: Vec<HistoryEvent>,
    payments: Vec<MessagePayment>,
    fee: &[(u128, &str)],
    staking: StakingMessages,
) -> Vec<RowDraft> {
    let denoms = denoms();
    let ctx = ClassifyContext {
        me: ME,
        platform_ticker: "IRIS",
        platform_denom: "uiris",
        platform_decimals: 6,
        denoms: &denoms,
    };
    let facts = TxFacts {
        hash: HASH.to_owned(),
        height: 10,
        timestamp: BLOCK_TIME,
        memo: "a memo".to_owned(),
        fee: fee
            .iter()
            .map(|(amount, denom)| FeeEntry {
                amount: *amount,
                denom: (*denom).to_owned(),
            })
            .collect(),
        gas_limit: 125_000,
        events,
        raw_tx: vec![1, 2, 3],
        payments,
        staking,
    };
    rows_for_tx(&facts, &ctx)
}

/// `total_amount`, `spent_by_me`, `received_by_me` and `my_balance_change` of a row.
fn amounts(row: &RowDraft) -> [BigDecimal; 4] {
    let d = &row.details;
    [
        d.total_amount.clone(),
        d.spent_by_me.clone(),
        d.received_by_me.clone(),
        d.my_balance_change.clone(),
    ]
}

fn assert_amounts(row: &RowDraft, expected: [&str; 4]) {
    let expected = expected.map(dec);
    assert_eq!(amounts(row), expected, "amounts of {:?}", row.details.transaction_type);
}

fn one(events: Vec<HistoryEvent>) -> RowDraft {
    let mut rows = rows(events);
    assert_eq!(rows.len(), 1, "one row");
    rows.remove(0)
}

#[test]
fn plain_transfers_give_one_standard_row_each() {
    // T36.30
    let out = one(vec![fee_from(ME), transfer(ME, PEER, "1000000uiris")]);
    assert_eq!(out.details.transaction_type, TransactionType::StandardTransfer);
    assert_amounts(&out, ["1.005", "1.005", "0", "-1.005"]);
    assert_eq!(out.details.from, vec![ME.to_owned()]);
    assert_eq!(out.details.to, vec![PEER.to_owned()]);
    assert_eq!(out.details.coin, "IRIS");
    assert_eq!(out.details.tx_hash, HASH);
    assert_eq!(out.details.block_height, 10);
    assert_eq!(out.details.timestamp, BLOCK_TIME);
    assert_eq!(out.details.memo.as_deref(), Some("a memo"));
    assert_eq!(out.details.tx_hex, vec![1u8, 2, 3].into());
    match out.details.fee_details.clone() {
        Some(TxFeeDetails::Tendermint(fee)) => {
            assert_eq!(fee.coin, "IRIS");
            assert_eq!(fee.amount, dec("0.005"));
            assert_eq!(fee.gas_limit, 125_000);
        },
        other => panic!("fee details: {other:?}"),
    }

    // An incoming send in a transaction the sender paid for: the fee enters no amount.
    let incoming = one(vec![fee_from(PEER), transfer(PEER, ME, "2000000uiris")]);
    assert_amounts(&incoming, ["2", "0", "2", "2"]);

    // A send to the account itself.
    let own = one(vec![fee_from(ME), transfer(ME, ME, "300000uiris")]);
    assert_amounts(&own, ["0.305", "0.305", "0.3", "-0.005"]);
}

#[test]
fn a_token_transfer_has_a_fee_row_on_the_platform_coin() {
    // T36.31
    let rows = rows(vec![fee_from(ME), transfer(ME, PEER, "2500000utoken")]);
    assert_eq!(rows.len(), 2);
    let token_id = token_id_of_denom("utoken");
    assert_eq!(
        rows[0].details.transaction_type,
        TransactionType::TokenTransfer(token_id)
    );
    assert_eq!(rows[0].details.coin, "TOK");
    assert_amounts(&rows[0], ["2.5", "2.5", "0", "-2.5"]);
    assert_eq!(rows[1].details.transaction_type, TransactionType::FeeForTokenTx);
    assert_eq!(rows[1].details.coin, "IRIS");
    assert!(rows[1].details.to.is_empty());
    assert_eq!(rows[1].details.tx_hash, rows[0].details.tx_hash);
    assert_amounts(&rows[1], ["0.005", "0.005", "0", "-0.005"]);
    assert_eq!(
        rows[1].details.internal_id,
        fee_row_internal_id(&rows[0].details.internal_id)
    );
    assert_eq!(rows[1].position, None);

    // The token id is the SHA-256 of the lower-cased denom.
    let expected = sha2::Sha256::digest(b"utoken").to_vec();
    assert_eq!(token_id_of_denom("UToken").0, expected);

    // An incoming token send gives the token row only.
    let incoming = rows_of_incoming_token();
    assert_eq!(incoming.len(), 1);
    assert_amounts(&incoming[0], ["1", "0", "1", "1"]);
}

fn rows_of_incoming_token() -> Vec<RowDraft> { rows(vec![fee_from(PEER), transfer(PEER, ME, "1000000utoken")]) }

#[test]
fn htlc_rows_follow_who_created_and_who_claimed() {
    // T36.32
    let create = one(vec![
        fee_from(ME),
        transfer(ME, MODULE, "1000000uiris"),
        event("create_htlc", &[("sender", ME), ("receiver", PEER)]),
    ]);
    assert_eq!(create.details.transaction_type, TransactionType::CustomTendermintMsg {
        msg_type: CustomTendermintMsgType::SendHtlcAmount,
        token_id: None
    });
    assert_eq!(create.details.from, vec![ME.to_owned()]);
    assert!(create.details.to.is_empty());
    assert_amounts(&create, ["1.005", "1.005", "0", "-1.005"]);

    let own_claim = one(vec![
        fee_from(ME),
        transfer(MODULE, ME, "1000000uiris"),
        event("claim_htlc", &[("sender", ME)]),
    ]);
    assert_eq!(
        own_claim.details.transaction_type,
        TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SignClaimHtlc,
            token_id: None
        }
    );
    assert_amounts(&own_claim, ["1.005", "0.005", "1", "0.995"]);

    let other_claim = one(vec![
        fee_from(PEER),
        transfer(MODULE, ME, "1000000uiris"),
        event("claim_htlc", &[("sender", PEER)]),
    ]);
    assert_eq!(
        other_claim.details.transaction_type,
        TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::ClaimHtlcAmount,
            token_id: None
        }
    );
    assert_amounts(&other_claim, ["1", "0", "1", "1"]);

    // A creation in the token denom: the token row carries the token id, and the fee has a row of its own.
    let token_create = rows(vec![
        fee_from(ME),
        transfer(ME, MODULE, "1000000utoken"),
        event("create_htlc", &[("sender", ME), ("receiver", PEER)]),
    ]);
    assert_eq!(token_create.len(), 2);
    assert_eq!(
        token_create[0].details.transaction_type,
        TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SendHtlcAmount,
            token_id: Some(token_id_of_denom("utoken"))
        }
    );
    assert_eq!(token_create[1].details.transaction_type, TransactionType::FeeForTokenTx);

    // A creation by another account gives no row.
    assert!(rows(vec![
        fee_from(PEER),
        transfer(PEER, MODULE, "1000000uiris"),
        event("create_htlc", &[("sender", PEER), ("receiver", ME)]),
    ])
    .is_empty());
}

#[test]
fn ibc_rows_name_the_other_chain() {
    // T36.33
    let out = one(vec![
        fee_from(ME),
        transfer(ME, "iaa1escrow", "1000000uiris"),
        event("ibc_transfer", &[("sender", ME), ("receiver", OTHER_CHAIN)]),
    ]);
    assert_eq!(out.details.transaction_type, TransactionType::TendermintIBCTransfer {
        token_id: None
    });
    assert_eq!(out.details.from, vec![ME.to_owned()]);
    assert_eq!(out.details.to, vec![OTHER_CHAIN.to_owned()]);

    // Incoming, in a transaction a relayer paid for.
    let incoming = one(vec![
        fee_from("iaa1relayer"),
        transfer("iaa1escrow", ME, "1000000uiris"),
        event("fungible_token_packet", &[("sender", OTHER_CHAIN), ("receiver", ME)]),
    ]);
    assert_eq!(incoming.details.from, vec![OTHER_CHAIN.to_owned()]);
    assert_eq!(incoming.details.to, vec![ME.to_owned()]);
    assert_amounts(&incoming, ["1", "0", "1", "1"]);
    match incoming.details.fee_details {
        Some(TxFeeDetails::Tendermint(fee)) => assert_eq!(fee.amount, dec("0.005")),
        other => panic!("fee details: {other:?}"),
    }

    let token = rows(vec![
        fee_from(ME),
        transfer(ME, "iaa1escrow", "1000000utoken"),
        event("ibc_transfer", &[("sender", ME), ("receiver", OTHER_CHAIN)]),
    ]);
    assert_eq!(
        token[0].details.transaction_type,
        TransactionType::TendermintIBCTransfer {
            token_id: Some(token_id_of_denom("utoken"))
        }
    );
}

#[test]
fn staking_rows_follow_d36_12() {
    // T36.34 under D36.12 (b).
    let delegation = one(vec![event("delegate", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ])]);
    assert_eq!(delegation.details.transaction_type, TransactionType::StakingDelegation);
    assert_eq!(delegation.details.from, vec![ME.to_owned()]);
    assert_eq!(delegation.details.to, vec![VALIDATOR.to_owned()]);
    assert_amounts(&delegation, ["1.005", "1.005", "0", "-1.005"]);

    let undelegation = one(vec![event("unbond", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ])]);
    assert_eq!(undelegation.details.transaction_type, TransactionType::RemoveDelegation);
    assert_eq!(undelegation.details.from, vec![ME.to_owned()]);
    assert!(undelegation.details.to.is_empty());
    assert_amounts(&undelegation, ["0.005", "0.005", "0", "-0.005"]);

    let claim = one(vec![event("withdraw_rewards", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "12345uiris"),
    ])]);
    assert_eq!(claim.details.transaction_type, TransactionType::ClaimDelegationRewards);
    assert_eq!(claim.details.from, vec![VALIDATOR.to_owned()]);
    assert_eq!(claim.details.to, vec![ME.to_owned()]);
    assert_amounts(&claim, ["0.017345", "0.005", "0.012345", "0.007345"]);

    // One row per validator; the first bears the fee.
    let several = rows(vec![
        event("delegate", &[
            ("delegator", ME),
            ("validator", "iaavaloper1a"),
            ("amount", "1000000uiris"),
        ]),
        event("delegate", &[
            ("delegator", ME),
            ("validator", "iaavaloper1b"),
            ("amount", "2000000uiris"),
        ]),
    ]);
    assert_eq!(several.len(), 2);
    assert_amounts(&several[0], ["1.005", "1.005", "0", "-1.005"]);
    assert_amounts(&several[1], ["2", "2", "0", "-2"]);
    assert_ne!(several[0].details.internal_id, several[1].details.internal_id);

    // A transaction whose only transfer is its fee payment counts the fee once.
    let fee_only = one(vec![transfer(ME, COLLECTOR, "5000uiris")]);
    assert_eq!(fee_only.details.transaction_type, TransactionType::StandardTransfer);
    assert_eq!(fee_only.details.to, vec![COLLECTOR.to_owned()]);
    assert_amounts(&fee_only, ["0.005", "0.005", "0", "-0.005"]);
}

#[test]
fn a_send_of_exactly_the_fee_amount_is_not_taken_for_the_fee_payment() {
    // A bank send of 5,000 base units, the same as the fee, is the only `transfer` event of its result (a chain
    // that emits no event for the fee): the decoded message pays it, so the fee is added to the row.
    let events = || vec![transfer(ME, PEER, "5000uiris")];
    let send = MessagePayment::BankSend {
        recipient: PEER.to_owned(),
        amounts: vec!["5000uiris".to_owned()],
    };
    let mut genuine = rows_with(events(), vec![send]);
    assert_eq!(genuine.len(), 1);
    assert_amounts(&genuine.remove(0), ["0.01", "0.01", "0", "-0.01"]);

    // Without a message that pays it, the same event is the fee payment and the fee counts once.
    let mut fee_only = rows_with(events(), vec![]);
    assert_amounts(&fee_only.remove(0), ["0.005", "0.005", "0", "-0.005"]);

    // A bank send to another recipient does not explain the transfer.
    let other = MessagePayment::BankSend {
        recipient: "iaa1else".to_owned(),
        amounts: vec!["5000uiris".to_owned()],
    };
    assert_amounts(&rows_with(events(), vec![other]).remove(0), [
        "0.005", "0.005", "0", "-0.005",
    ]);
}

#[test]
fn what_yields_no_row() {
    // T36.35: a denomination that does not resolve; a transaction without a transfer.
    assert!(rows(vec![fee_from(ME), transfer(ME, PEER, "1000000ufoo")]).is_empty());
    assert!(rows(vec![event("message", &[("action", "/cosmos.gov.v1beta1.MsgVote")])]).is_empty());
}

#[test]
fn repeated_events_and_split_amounts() {
    // An event repeated identically in direct succession counts once.
    let once = one(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, PEER, "1000000uiris"),
    ]);
    assert_amounts(&once, ["1.005", "1.005", "0", "-1.005"]);

    // Several coins of one event give one row each; transfers with the same parties and denom are merged.
    let split = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris,500000utoken"),
        transfer(ME, "iaa1other", "1uiris"),
        transfer(ME, PEER, "2000000uiris"),
    ]);
    // The transfers are read from the last emitted to the first (R36.14.19 item 3): the later send to the peer is
    // position 0, the send to the other account 1, the earlier send to the peer merges into position 0, and the
    // token coin is position 2. The first row bears the fee, so the token row needs no fee row.
    assert_eq!(split.len(), 3);
    assert_amounts(&split[0], ["3.005", "3.005", "0", "-3.005"]);
    assert_eq!(split[1].details.to, vec!["iaa1other".to_owned()]);
    assert_eq!(
        split[2].details.transaction_type,
        TransactionType::TokenTransfer(token_id_of_denom("utoken"))
    );
    assert_eq!(split[0].position, Some(0));
    assert_eq!(split[1].position, Some(1));
    assert_eq!(split[2].position, Some(2));
}

#[test]
fn attribute_keys_may_be_base64() {
    // R36.14.5: an older node sends base64 keys and values.
    let b64 = |text: &str| base64::encode(text.as_bytes());
    let encoded = HistoryEvent::from_raw("transfer".to_owned(), vec![
        (b64("sender"), b64(ME)),
        (b64("recipient"), b64(PEER)),
        (b64("amount"), b64("1000000uiris")),
    ]);
    let row = one(vec![fee_from(ME), encoded]);
    assert_amounts(&row, ["1.005", "1.005", "0", "-1.005"]);
    assert_eq!(row.details.to, vec![PEER.to_owned()]);
}

#[test]
fn events_are_compared_in_the_form_the_node_delivered_them() {
    // R36.14.19 item 1: two same-type events that differ only in the index flag, or only in the encoding of a key
    // and value, are not repeats: both are counted (and then merged, so the amount shows it).
    let attributes = |index: bool| {
        vec![
            ("sender".to_owned(), ME.to_owned(), index),
            ("recipient".to_owned(), PEER.to_owned(), true),
            ("amount".to_owned(), "1000000uiris".to_owned(), true),
        ]
    };
    let make = |index: bool| HistoryEvent::from_raw_indexed("transfer".to_owned(), attributes(index));
    let identical = one(vec![fee_from(ME), make(true), make(true)]);
    assert_amounts(&identical, ["1.005", "1.005", "0", "-1.005"]);
    let index_differs = one(vec![fee_from(ME), make(true), make(false)]);
    assert_amounts(&index_differs, ["2.005", "2.005", "0", "-2.005"]);

    let b64 = |text: &str| base64::encode(text.as_bytes());
    let encoded = HistoryEvent::from_raw_indexed("transfer".to_owned(), vec![
        (b64("sender"), b64(ME), true),
        (b64("recipient"), b64(PEER), true),
        (b64("amount"), b64("1000000uiris"), true),
    ]);
    let encoding_differs = one(vec![fee_from(ME), make(true), encoded]);
    assert_amounts(&encoding_differs, ["2.005", "2.005", "0", "-2.005"]);
}

#[test]
fn a_denomination_resolves_without_regard_to_letter_case() {
    // D36.13 (a'): the platform denom exactly, any other denom case-insensitively.
    let rows = rows(vec![fee_from(ME), transfer(ME, PEER, "1000000UToken")]);
    assert_eq!(
        rows[0].details.transaction_type,
        TransactionType::TokenTransfer(token_id_of_denom("utoken"))
    );
    assert_eq!(rows[0].details.coin, "TOK");
}

// ---------------------------------------------------------------------------
// Positions, pairing, staking and fee forms (R36.14.19 -- R36.14.22; T36.45 -- T36.48, the reference's values)
// ---------------------------------------------------------------------------

const STRANGER: &str = "iaa1stranger";

/// Whether the identifier of a row ends in the 8-byte big-endian position `n` (R36.14.19 item 7).
fn at_position(row: &RowDraft, n: u64) -> bool {
    row.position == Some(n as usize)
        && row.details.internal_id.0[25..] == n.to_be_bytes()
        && row.details.internal_id == history_internal_id(n as usize, HASH)
}

fn find_to<'a>(rows: &'a [RowDraft], to: &str) -> &'a RowDraft {
    rows.iter()
        .find(|row| row.details.to == vec![to.to_owned()])
        .unwrap_or_else(|| panic!("a row to {to}"))
}

#[test]
fn positions_follow_the_transfer_list() {
    // T36.45 (i): read from the last emitted to the first, so the later send is position 0 and bears the fee.
    let sends = |between: Option<HistoryEvent>| {
        let mut events = vec![fee_from(ME), transfer(ME, PEER, "1000000uiris")];
        events.extend(between);
        events.push(transfer(ME, STRANGER, "2000000uiris"));
        rows(events)
    };
    let plain = sends(None);
    assert_eq!(plain.len(), 2);
    assert!(at_position(find_to(&plain, STRANGER), 0));
    assert_amounts(find_to(&plain, STRANGER), ["2.005", "2.005", "0", "-2.005"]);
    assert!(at_position(find_to(&plain, PEER), 1));
    assert_amounts(find_to(&plain, PEER), ["1", "1", "0", "-1"]);

    // (ii) A coin that does not resolve takes no position: the same two identifiers.
    let with_foreign = sends(Some(transfer(ME, "iaa1else", "7ufoo")));
    assert_eq!(with_foreign.len(), 2);
    assert!(at_position(find_to(&with_foreign, STRANGER), 0));
    assert!(at_position(find_to(&with_foreign, PEER), 1));
}

#[test]
fn transfers_of_other_accounts_count_and_are_stored() {
    // T36.45 (iii): X to Y, then X to A.
    let rows = rows(vec![
        fee_from(STRANGER),
        transfer(STRANGER, PEER, "1000000uiris"),
        transfer(STRANGER, ME, "1000000uiris"),
    ]);
    assert_eq!(rows.len(), 2);
    assert!(at_position(find_to(&rows, ME), 0));
    assert!(at_position(find_to(&rows, PEER), 1));
}

#[test]
fn equal_parties_merge_and_repeated_events_count_once_per_type() {
    // T36.45 (iv): A to X, A to X, A to Y.
    let merged = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, PEER, "2000000uiris"),
        transfer(ME, STRANGER, "3000000uiris"),
    ]);
    assert_eq!(merged.len(), 2);
    assert!(at_position(find_to(&merged, STRANGER), 0));
    assert!(at_position(find_to(&merged, PEER), 1));
    assert_amounts(find_to(&merged, PEER), ["3", "3", "0", "-3"]);

    // (v) Two identical events with only an event of another type between them are one transfer.
    let once = one(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        event("message", &[("action", "/cosmos.bank.v1beta1.MsgSend")]),
        transfer(ME, PEER, "1000000uiris"),
    ]);
    assert_amounts(&once, ["1.005", "1.005", "0", "-1.005"]);
    // With a `transfer` event to another account between them they are not repeated and the sends merge.
    let apart = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, STRANGER, "1000000uiris"),
        transfer(ME, PEER, "1000000uiris"),
    ]);
    assert_eq!(apart.len(), 2);
    assert!(at_position(find_to(&apart, PEER), 0));
    assert_amounts(find_to(&apart, PEER), ["2.005", "2.005", "0", "-2.005"]);
    assert!(at_position(find_to(&apart, STRANGER), 1));
}

#[test]
fn coins_of_one_event_take_positions_in_the_order_of_the_text() {
    // T36.45 (vi).
    let rows = rows(vec![fee_from(ME), transfer(ME, PEER, "2500000utoken,1000000uiris")]);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].details.transaction_type,
        TransactionType::TokenTransfer(token_id_of_denom("utoken"))
    );
    assert!(at_position(&rows[0], 0));
    assert_eq!(rows[1].details.transaction_type, TransactionType::StandardTransfer);
    assert!(at_position(&rows[1], 1));
    assert_amounts(&rows[1], ["1", "1", "0", "-1"]);
    assert_eq!(rows[2].details.transaction_type, TransactionType::FeeForTokenTx);
    assert_eq!(
        rows[2].details.internal_id,
        fee_row_internal_id(&rows[0].details.internal_id)
    );
}

#[test]
fn an_htlc_creation_of_another_account_counts_but_has_no_row() {
    // T36.45 (vii).
    let rows = rows(vec![
        fee_from(STRANGER),
        transfer(STRANGER, ME, "1000000uiris"),
        transfer(STRANGER, MODULE, "1000000uiris"),
        event("create_htlc", &[("sender", STRANGER), ("receiver", PEER)]),
    ]);
    assert_eq!(rows.len(), 1);
    assert!(at_position(&rows[0], 1));
    assert_eq!(rows[0].details.to, vec![ME.to_owned()]);
}

#[test]
fn module_events_correct_transfers_by_order() {
    let htlc_row = |to_module_first: bool| {
        let mut events = vec![fee_from(ME)];
        let (a, b) = (transfer(ME, MODULE, "1000000uiris"), transfer(ME, PEER, "2000000uiris"));
        events.extend(if to_module_first { vec![a, b] } else { vec![b, a] });
        events.push(event("create_htlc", &[("sender", ME), ("receiver", STRANGER)]));
        rows(events)
    };
    // T36.46 (i): the transfer read first takes the creation event.
    let rows_i = htlc_row(true);
    assert_eq!(rows_i.len(), 2);
    assert_eq!(
        rows_i[0].details.transaction_type,
        TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SendHtlcAmount,
            token_id: None
        }
    );
    assert!(at_position(&rows_i[0], 0));
    assert!(rows_i[0].details.to.is_empty());
    assert_amounts(&rows_i[0], ["2.005", "2.005", "0", "-2.005"]);
    assert_eq!(rows_i[1].details.transaction_type, TransactionType::StandardTransfer);
    assert_eq!(rows_i[1].details.to, vec![MODULE.to_owned()]);
    assert!(at_position(&rows_i[1], 1));

    // (ii) One transfer, a creation and a claim event, in either emission order: one creation row, nothing else.
    for claim_first in [false, true] {
        let create = event("create_htlc", &[("sender", ME), ("receiver", STRANGER)]);
        let claim = event("claim_htlc", &[("sender", ME)]);
        let mut events = vec![fee_from(ME), transfer(ME, MODULE, "1000000uiris")];
        events.extend(if claim_first {
            vec![claim, create]
        } else {
            vec![create, claim]
        });
        let rows = rows(events);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].details.transaction_type, TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SendHtlcAmount,
            token_id: None
        });
    }

    // (iii) Two transfers, one creation and one IBC event: the transfer read first is the creation.
    let mixed = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, STRANGER, "2000000uiris"),
        event("create_htlc", &[("sender", ME), ("receiver", "iaa1creation")]),
        event("ibc_transfer", &[("sender", ME), ("receiver", OTHER_CHAIN)]),
    ]);
    assert_eq!(mixed.len(), 2);
    assert_eq!(
        mixed[0].details.transaction_type,
        TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SendHtlcAmount,
            token_id: None
        }
    );
    assert_amounts(&mixed[0], ["2.005", "2.005", "0", "-2.005"]);
    assert_eq!(
        mixed[1].details.transaction_type,
        TransactionType::TendermintIBCTransfer { token_id: None }
    );
    assert_eq!(mixed[1].details.to, vec![OTHER_CHAIN.to_owned()]);

    // (iv) A creation event without `receiver` is used up and corrects nothing.
    let bare = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, STRANGER, "2000000uiris"),
        event("create_htlc", &[("sender", ME)]),
    ]);
    assert_eq!(bare.len(), 2);
    for row in &bare {
        assert_eq!(row.details.transaction_type, TransactionType::StandardTransfer);
    }
    assert_eq!(find_to(&bare, PEER).position, Some(1));
    assert_eq!(find_to(&bare, STRANGER).position, Some(0));
}

#[test]
fn a_packet_event_pairs_by_order_not_by_content() {
    // T36.46 (v): the transfer that does not resolve is not a candidate, so the one event goes to the other.
    let one_event = rows(vec![
        fee_from(PEER),
        transfer("iaa1escrow", ME, "1000000uiris"),
        transfer("iaa1escrow", ME, "9ufoo"),
        event("fungible_token_packet", &[("sender", OTHER_CHAIN), ("receiver", ME)]),
    ]);
    assert_eq!(one_event.len(), 1);
    assert_eq!(one_event[0].details.from, vec![OTHER_CHAIN.to_owned()]);
    assert_eq!(one_event[0].details.to, vec![ME.to_owned()]);
    assert!(at_position(&one_event[0], 0));
    assert_eq!(
        one_event[0].details.transaction_type,
        TransactionType::TendermintIBCTransfer { token_id: None }
    );

    // (vi) D36.22 (a): the event read first is the one emitted last, so the one row carries the parties of the
    // packet event to the other account.
    let unequal = rows(vec![
        fee_from(PEER),
        transfer("iaa1escrow", ME, "1000000uiris"),
        event("fungible_token_packet", &[("sender", OTHER_CHAIN), ("receiver", ME)]),
        transfer("iaa1escrow", STRANGER, "9ufoo"),
        event("fungible_token_packet", &[
            ("sender", "cosmos1third"),
            ("receiver", STRANGER),
        ]),
    ]);
    assert_eq!(unequal.len(), 1);
    assert!(at_position(&unequal[0], 0));
    assert_eq!(unequal[0].details.from, vec!["cosmos1third".to_owned()]);
    assert_eq!(unequal[0].details.to, vec![STRANGER.to_owned()]);
}

#[test]
fn a_staking_event_decides_the_whole_transaction() {
    // T36.46 (vii).
    let rows = rows(vec![
        event("delegate", &[
            ("delegator", ME),
            ("validator", VALIDATOR),
            ("amount", "1000000uiris"),
        ]),
        transfer(ME, PEER, "1000000uiris"),
    ]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].details.transaction_type, TransactionType::StakingDelegation);
    assert!(at_position(&rows[0], 0));
}

fn staking(kind: &str, attributes: &[(&str, &str)]) -> Vec<RowDraft> { rows(vec![event(kind, attributes)]) }

#[test]
fn staking_events_without_what_they_need_give_no_row() {
    // T36.47 (i): no `delegator`, although a `message` event and the bank events name the account.
    let no_delegator = rows(vec![
        fee_from(ME),
        event("message", &[("sender", ME)]),
        event("delegate", &[("validator", VALIDATOR), ("amount", "1000000uiris")]),
        transfer(ME, VALIDATOR, "1000000uiris"),
    ]);
    assert!(no_delegator.is_empty());

    // (ii) The same event with a base64 `delegator` key and value: the delegation row.
    let b64 = |text: &str| base64::encode(text.as_bytes());
    let encoded = HistoryEvent::from_raw("delegate".to_owned(), vec![
        (b64("delegator"), b64(ME)),
        ("validator".to_owned(), VALIDATOR.to_owned()),
        ("amount".to_owned(), "1000000uiris".to_owned()),
    ]);
    let row = one(vec![encoded]);
    assert_eq!(row.details.transaction_type, TransactionType::StakingDelegation);

    // (iii) `unbond`: without `delegator`, with an amount without a denomination, and in full.
    assert!(staking("unbond", &[("validator", VALIDATOR), ("amount", "1000000uiris")]).is_empty());
    assert!(staking("unbond", &[("delegator", ME), ("amount", "1000000")]).is_empty());
    assert!(staking("unbond", &[("delegator", ME), ("amount", "1000000ufoo")]).is_empty());
    let full = staking("unbond", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ]);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].details.transaction_type, TransactionType::RemoveDelegation);

    // (iv) `withdraw_rewards` without `delegator`, without `validator`, and with an empty amount.
    assert!(staking("withdraw_rewards", &[("validator", VALIDATOR), ("amount", "5uiris")]).is_empty());
    assert!(staking("withdraw_rewards", &[("delegator", ME), ("amount", "5uiris")]).is_empty());
    assert!(staking("withdraw_rewards", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "")
    ])
    .is_empty());
    assert!(staking("delegate", &[("delegator", ME), ("amount", "5uiris")]).is_empty());
}

#[test]
fn a_staking_event_without_a_delegator_takes_the_one_message_delegator() {
    // T36.47 (i) under D36.20 (b): the delegator of the transaction's one staking message of the kind.
    let delegate = || event("delegate", &[("validator", VALIDATOR), ("amount", "1000000uiris")]);
    let with =
        |messages: StakingMessages, event: HistoryEvent| rows_of(vec![event], vec![], &[(5_000, "uiris")], messages);
    let me = || vec![ME.to_owned()];

    let rows = with(
        StakingMessages {
            delegate: me(),
            ..StakingMessages::default()
        },
        delegate(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].details.transaction_type, TransactionType::StakingDelegation);
    assert_eq!(rows[0].details.from, me());
    assert_amounts(&rows[0], ["1.005", "1.005", "0", "-1.005"]);

    // Two messages of the kind, or only messages of another kind: no row.
    let two = StakingMessages {
        delegate: vec![ME.to_owned(), PEER.to_owned()],
        ..StakingMessages::default()
    };
    assert!(with(two, delegate()).is_empty());
    let other_kind = StakingMessages {
        unbond: me(),
        ..StakingMessages::default()
    };
    assert!(with(other_kind, delegate()).is_empty());

    // `unbond` and `withdraw_rewards` follow the same rule.
    let unbond = with(
        StakingMessages {
            unbond: me(),
            ..StakingMessages::default()
        },
        event("unbond", &[("validator", VALIDATOR), ("amount", "1000000uiris")]),
    );
    assert_eq!(unbond.len(), 1);
    assert_eq!(unbond[0].details.transaction_type, TransactionType::RemoveDelegation);
    let claim = with(
        StakingMessages {
            withdraw_rewards: me(),
            ..StakingMessages::default()
        },
        event("withdraw_rewards", &[
            ("validator", VALIDATOR),
            ("amount", "12345uiris"),
        ]),
    );
    assert_eq!(claim.len(), 1);
    assert_eq!(
        claim[0].details.transaction_type,
        TransactionType::ClaimDelegationRewards
    );
    assert_eq!(claim[0].details.to, me());

    // A `delegator` attribute on the event wins over the message; a missing `validator` still gives no row.
    let own = event("delegate", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ]);
    let elsewhere = StakingMessages {
        delegate: vec![PEER.to_owned()],
        ..StakingMessages::default()
    };
    assert_eq!(with(elsewhere, own)[0].details.from, me());
    let no_validator = StakingMessages {
        delegate: me(),
        ..StakingMessages::default()
    };
    assert!(with(no_validator, event("delegate", &[("amount", "1000000uiris")])).is_empty());
}

#[test]
fn a_reward_claim_of_two_denominations_gives_two_rows() {
    // T36.47 (v).
    let claim = staking("withdraw_rewards", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris,2000000utoken"),
    ]);
    assert_eq!(claim.len(), 2);
    for (row, position) in claim.iter().zip(0u64..) {
        assert_eq!(row.details.transaction_type, TransactionType::ClaimDelegationRewards);
        assert!(at_position(row, position));
    }
}

fn fee_details_of_row(row: &RowDraft) -> (String, BigDecimal) {
    match row.details.fee_details.clone() {
        Some(TxFeeDetails::Tendermint(fee)) => (fee.coin, fee.amount),
        other => panic!("fee details: {other:?}"),
    }
}

#[test]
fn the_fee_is_read_in_the_platform_denom_wherever_it_stands() {
    // T36.48 (i) under D36.19 (b): a fee of one entry in a token denom enters no amount and causes no fee row;
    // `fee_details` names the token, scaled with its decimals. The fee payment is recognized by the whole text.
    let send = || {
        vec![
            transfer(ME, COLLECTOR, "7000utoken"),
            transfer(ME, PEER, "1000000uiris"),
        ]
    };
    let rows = rows_with_fee(send(), vec![], 7_000, "utoken");
    assert_eq!(rows.len(), 1);
    assert_amounts(&rows[0], ["1", "1", "0", "-1"]);
    assert_eq!(fee_details_of_row(&rows[0]), ("TOK".to_owned(), dec("0.007")));

    // A fee in a denomination that resolves to nothing: `fee_details` is null.
    let unknown = rows_with_fee(
        vec![transfer(ME, COLLECTOR, "7000ufoo"), transfer(ME, PEER, "1000000uiris")],
        vec![],
        7_000,
        "ufoo",
    );
    assert_eq!(unknown.len(), 1);
    assert_amounts(&unknown[0], ["1", "1", "0", "-1"]);
    assert!(unknown[0].details.fee_details.is_none());

    // (ii) A fee of two entries, the token first: the platform entry is read wherever it stands, and the one event
    // that pays both is the fee payment (its text equals the whole list), so it is left out.
    let two = rows_with_fee_list(
        vec![
            transfer(ME, COLLECTOR, "300utoken,5000uiris"),
            transfer(ME, PEER, "1000000uiris"),
        ],
        vec![],
        &[(300, "utoken"), (5_000, "uiris")],
    );
    assert_eq!(two.len(), 1);
    assert!(at_position(&two[0], 0));
    assert_amounts(&two[0], ["1.005", "1.005", "0", "-1.005"]);
    assert_eq!(fee_details_of_row(&two[0]), ("IRIS".to_owned(), dec("0.005")));

    // A token transfer with the fee in the platform denom still has its fee row; a fee with no platform entry has
    // none.
    let token_send = |fee: &[(u128, &str)], fee_text: &str| {
        rows_with_fee_list(
            vec![transfer(ME, COLLECTOR, fee_text), transfer(ME, PEER, "2000000utoken")],
            vec![],
            fee,
        )
    };
    assert_eq!(token_send(&[(5_000, "uiris")], "5000uiris").len(), 2);
    assert_eq!(token_send(&[(7_000, "utoken")], "7000utoken").len(), 1);

    // (iii) A first fee entry of 2^64, or a platform entry of 2^64, leaves the transaction out.
    assert!(rows_with_fee(send(), vec![], 1u128 << 64, "uiris").is_empty());
    assert!(rows_with_fee_list(send(), vec![], &[(1, "utoken"), (1u128 << 64, "uiris")]).is_empty());
}

#[test]
fn an_empty_fee_list_gives_rows_with_a_fee_of_zero_and_no_fee_row() {
    // T36.48 (iv) under D36.20 (b): the rows of R36.14.5 -- R36.14.7 with a fee of zero.
    let sent = rows_with_fee_list(vec![transfer(ME, PEER, "1000000uiris")], vec![], &[]);
    assert_eq!(sent.len(), 1);
    assert_amounts(&sent[0], ["1", "1", "0", "-1"]);
    assert_eq!(fee_details_of_row(&sent[0]), ("IRIS".to_owned(), dec("0")));

    // A token transfer has no fee row, and an empty `amount` is not taken for a fee payment.
    let token = rows_with_fee_list(
        vec![
            event("transfer", &[("sender", ME), ("recipient", PEER), ("amount", "")]),
            transfer(ME, PEER, "2000000utoken"),
        ],
        vec![],
        &[],
    );
    assert_eq!(token.len(), 1);
    assert_eq!(
        token[0].details.transaction_type,
        TransactionType::TokenTransfer(token_id_of_denom("utoken"))
    );
}

#[test]
fn the_fee_is_attributed_once_to_the_first_entry_this_account_sent() {
    // R36.14.22 item 4: a token entry read first takes the fee as a fee row; the platform entry read later does not.
    let rows = rows(vec![
        fee_from(ME),
        transfer(ME, PEER, "1000000uiris"),
        transfer(ME, STRANGER, "2000000utoken"),
    ]);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].details.transaction_type, TransactionType::FeeForTokenTx);
    assert_amounts(find_to(&rows, PEER), ["1", "1", "0", "-1"]);
}

// ---------------------------------------------------------------------------
// Identifier, stored form and order (R36.14.10; D36.11 (a))
// ---------------------------------------------------------------------------

fn reversed_hash_first() -> Vec<u8> { HASH.as_bytes()[..25].iter().rev().copied().collect() }

#[test]
#[cfg(target_pointer_width = "64")]
fn identifiers_follow_the_reference_byte_string() {
    // T36.37
    let reversed_hash: Vec<u8> = HASH.as_bytes()[..25].iter().rev().copied().collect();
    let mut first = reversed_hash.clone();
    first.extend_from_slice(&[0u8; 8]);
    assert_eq!(history_internal_id(0, HASH).0, first);
    assert_eq!(first.len(), 33);

    let mut second = reversed_hash;
    second.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(history_internal_id(1, HASH).0, second);

    // The 32-bit browser build (T36.45 (viii)): the first 29 characters in reverse order, then a 4-byte position.
    let reversed_29: Vec<u8> = HASH.as_bytes()[..29].iter().rev().copied().collect();
    let mut browser = reversed_29;
    browser.extend_from_slice(&[0, 0, 0, 1]);
    assert_eq!(browser.len(), 33);
    assert_eq!(history_internal_id_for_word(1, HASH, 4).0, browser);
    // The first 25 characters of this hash are not a palindrome, so the order is visible.
    assert_ne!(reversed_hash_first(), HASH.as_bytes()[..25].to_vec());

    // The fee row has the bytes of its token row in the opposite order.
    let token_row_id = history_internal_id(0, HASH);
    let mut forward = token_row_id.0.clone();
    forward.reverse();
    assert_eq!(fee_row_internal_id(&token_row_id).0, forward);

    // The two earlier 32-byte forms: the first 32 bytes of the byte string, in forward and in reverse order.
    let [reverse_form, forward_form] = obsolete_internal_ids(0, HASH);
    assert_eq!(forward_form.0.len(), 32);
    assert_eq!(&forward_form.0[..8], &[0u8; 8]);
    assert_eq!(&forward_form.0[8..], &HASH.as_bytes()[..24]);
    let mut back = reverse_form.0.clone();
    back.reverse();
    assert_eq!(back, forward_form.0);
}

#[test]
fn stored_rows_hold_base_units_and_are_scaled_when_served() {
    let row = one(vec![fee_from(ME), transfer(ME, PEER, "1000000uiris")]);
    let stored = stored_form(row.details.clone(), row.decimals);
    assert_eq!(stored.total_amount.to_string(), "1005000");
    assert_eq!(stored.spent_by_me.to_string(), "1005000");
    assert_eq!(stored.received_by_me.to_string(), "0");
    assert_eq!(stored.my_balance_change, BigDecimal::from(0));
    // It survives the store's JSON.
    let stored: crate::TransactionDetails = serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
    let mut served = served_form(stored, 6).expect("a valid stored row");
    // `uamount` of the fee details is not serialized, so it is not compared.
    served.fee_details = row.details.fee_details.clone();
    assert_eq!(served, row.details);

    // The fee row is stored in display units and served as stored.
    let rows = rows(vec![fee_from(ME), transfer(ME, PEER, "2500000utoken")]);
    let fee_row = rows[1].details.clone();
    assert_eq!(stored_form(fee_row.clone(), 6), fee_row);
    assert_eq!(served_form(fee_row.clone(), 6).unwrap(), fee_row);

    // A stored amount that is not a non-negative integer is an error, never an abort.
    let mut bad = stored_form(row.details.clone(), 6);
    bad.total_amount = dec("1.5");
    assert!(served_form(bad, 6).is_err());
    let mut negative = stored_form(row.details, 6);
    negative.spent_by_me = dec("-1");
    assert!(served_form(negative, 6).is_err());
}

fn details(id: u8, height: u64, from: &str, token: Option<&str>) -> crate::TransactionDetails {
    crate::TransactionDetails {
        tx_hex: vec![id].into(),
        tx_json: None,
        tx_hash: format!("{id:02X}"),
        from: vec![from.to_owned()],
        to: vec![PEER.to_owned()],
        total_amount: BigDecimal::from(1),
        spent_by_me: BigDecimal::from(1),
        received_by_me: BigDecimal::from(0),
        my_balance_change: BigDecimal::from(0),
        block_height: height,
        timestamp: BLOCK_TIME,
        fee_details: None,
        coin: "IRIS".to_owned(),
        internal_id: vec![id].into(),
        kmd_rewards: None,
        transaction_type: match token {
            None => TransactionType::StandardTransfer,
            Some(denom) => TransactionType::TokenTransfer(token_id_of_denom(denom)),
        },
        memo: Some(String::new()),
    }
}

#[test]
fn the_store_serves_by_height_then_identifier_for_the_account_only() {
    // T36.37 (order and FromId) and R36.14.2 (the account scope).
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let rows = vec![
        details(5, 10, ME, None),
        details(7, 12, ME, None),
        details(6, 12, ME, None),
        details(8, 13, "iaa1stranger", None),
        details(9, 11, ME, Some("utoken")),
    ];
    common::block_on(storage.add_transactions_to_history("IRIS", rows)).unwrap();

    let coin_scope = || HistoryCoinType::AccountCoin {
        ticker: "IRIS".to_owned(),
        address: ME.to_owned(),
    };
    let page = common::block_on(storage.get_history(coin_scope(), PagingOptionsEnum::default(), 10)).unwrap();
    // The stranger's row is not served; the token's row is not the platform coin's.
    let ids: Vec<u8> = page.transactions.iter().map(|d| d.internal_id.0[0]).collect();
    assert_eq!(ids, vec![6, 7, 5]);
    assert_eq!(page.total, 3);

    let after =
        common::block_on(storage.get_history(coin_scope(), PagingOptionsEnum::FromId(BytesJson::from(vec![6u8])), 10))
            .unwrap();
    assert_eq!(after.skipped, 1);
    assert_eq!(after.transactions.len(), 2);

    // An identifier that matches no row gives an empty list and the true total.
    let none =
        common::block_on(storage.get_history(coin_scope(), PagingOptionsEnum::FromId(BytesJson::from(vec![99u8])), 10))
            .unwrap();
    assert!(none.transactions.is_empty());
    assert_eq!(none.skipped, 0);
    assert_eq!(none.total, 3);

    let token_page = common::block_on(storage.get_history(
        HistoryCoinType::AccountToken {
            platform: "IRIS".to_owned(),
            token_id: token_id_of_denom("utoken"),
            address: ME.to_owned(),
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!(token_page.total, 1);
    assert_eq!(token_page.transactions[0].internal_id.0[0], 9);
}

#[test]
fn a_memo_equal_to_the_address_does_not_put_a_row_in_the_account_scope() {
    // R36.14.2: the account scope binds the `from` and `to` lists, not any text of the row.
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let mut memo_row = details(1, 10, "iaa1stranger", None);
    memo_row.memo = Some(ME.to_owned());
    let mut to_row = details(2, 9, "iaa1stranger", None);
    to_row.to = vec![ME.to_owned()];
    let own_row = details(3, 8, ME, None);
    common::block_on(storage.add_transactions_to_history("IRIS", vec![memo_row, to_row, own_row])).unwrap();

    let scope = || HistoryCoinType::AccountCoin {
        ticker: "IRIS".to_owned(),
        address: ME.to_owned(),
    };
    let page = common::block_on(storage.get_history(scope(), PagingOptionsEnum::default(), 10)).unwrap();
    let ids: Vec<u8> = page.transactions.iter().map(|d| d.internal_id.0[0]).collect();
    assert_eq!(ids, vec![2, 3]);
    assert_eq!(page.total, 2);

    // Paging counts the filtered set: the second page of one row holds the second row.
    let second = common::block_on(storage.get_history(
        scope(),
        PagingOptionsEnum::PageNumber(std::num::NonZeroUsize::new(2).unwrap()),
        1,
    ))
    .unwrap();
    assert_eq!(second.transactions.len(), 1);
    assert_eq!(second.transactions[0].internal_id.0[0], 3);
    assert_eq!(second.skipped, 1);
    assert_eq!(second.total, 2);
}

// ---------------------------------------------------------------------------
// The fetcher against the scripted node (R36.14.3, R36.14.4, R36.14.11)
// ---------------------------------------------------------------------------

#[derive(prost::Message)]
struct TimestampWire {
    #[prost(int64, tag = "1")]
    seconds: i64,
    #[prost(int32, tag = "2")]
    nanos: i32,
}

#[derive(prost::Message)]
struct HeaderWire {
    #[prost(message, optional, tag = "4")]
    time: Option<TimestampWire>,
}

#[derive(prost::Message)]
struct BlockWire {
    #[prost(message, optional, tag = "1")]
    header: Option<HeaderWire>,
}

/// `GetBlockByHeightResponse`, with the block in field 2.
#[derive(prost::Message)]
struct BlockAnswerWire {
    #[prost(message, optional, tag = "2")]
    block: Option<BlockWire>,
}

struct NodeState {
    /// The answer of the `coin_spent` query and of the `coin_received` query.
    spender: Vec<Json>,
    receiver: Vec<Json>,
    /// The block time by height; a height not listed has `BLOCK_TIME`, `None` is a block without time.
    block_times: HashMap<u64, Option<i64>>,
    /// How many of the next block queries and balance queries the node refuses.
    block_failures: u32,
    balance_failures: u32,
    balance: u64,
    /// The history state of the coin at every `tx_search`.
    states: Vec<Json>,
}

type SharedState = Arc<Mutex<NodeState>>;

fn script_history(fx: &Fixture) -> SharedState { script_history_on(&fx.node, &fx.coin) }

/// Scripts one node (the history queries, the block query and the balance query) for the coin.
fn script_history_on(node: &FakeNode, coin: &TendermintCoin) -> SharedState {
    let state = Arc::new(Mutex::new(NodeState {
        spender: Vec::new(),
        receiver: Vec::new(),
        block_times: HashMap::new(),
        block_failures: 0,
        balance_failures: 0,
        balance: 1_000_000,
        states: Vec::new(),
    }));
    let (node_state, coin) = (state.clone(), coin.clone());
    node.answer(move |method, params| {
        let mut st = node_state.lock().unwrap();
        match method {
            "abci_info" => Some(json!({ "response": {
                "data": "test", "version": "0", "app_version": "1",
                "last_block_height": "100", "last_block_app_hash": ""
            } })),
            "tx_search" => {
                let state_now = serde_json::to_value(coin.history_sync_state.lock().unwrap().clone()).unwrap();
                st.states.push(state_now);
                let list = if params["query"].as_str()?.contains("coin_spent") {
                    st.spender.clone()
                } else {
                    st.receiver.clone()
                };
                let number = |value: &Json| {
                    value
                        .as_str()
                        .and_then(|s| s.parse::<usize>().ok())
                        .or(value.as_u64().map(|n| n as usize))
                };
                let (page, per_page) = (number(&params["page"])?, number(&params["per_page"])?);
                let txs: Vec<Json> = list
                    .iter()
                    .skip((page - 1) * per_page)
                    .take(per_page)
                    .cloned()
                    .collect();
                Some(json!({ "txs": txs, "total_count": list.len().to_string() }))
            },
            "abci_query" => {
                let path = params["path"].as_str()?;
                let data = hex::decode(params["data"].as_str().unwrap_or_default()).ok()?;
                if path == "/cosmos.base.tendermint.v1beta1.Service/GetBlockByHeight" {
                    if st.block_failures > 0 {
                        st.block_failures -= 1;
                        return None;
                    }
                    let request = cosmrs::proto::cosmos::base::tendermint::v1beta1::GetBlockByHeightRequest::decode(
                        data.as_slice(),
                    )
                    .ok()?;
                    let seconds = match st.block_times.get(&(request.height as u64)) {
                        Some(time) => *time,
                        None => Some(BLOCK_TIME as i64),
                    };
                    let answer = BlockAnswerWire {
                        block: Some(BlockWire {
                            header: Some(HeaderWire {
                                time: seconds.map(|seconds| TimestampWire { seconds, nanos: 0 }),
                            }),
                        }),
                    };
                    Some(abci_value(&answer.encode_to_vec()))
                } else if path == "/cosmos.bank.v1beta1.Query/Balance" {
                    if st.balance_failures > 0 {
                        st.balance_failures -= 1;
                        return None;
                    }
                    let balance = QueryBalanceResponse {
                        balance: Some(CoinProto {
                            denom: "x".to_owned(),
                            amount: st.balance.to_string(),
                        }),
                    };
                    Some(abci_value(&balance.encode_to_vec()))
                } else {
                    None
                }
            },
            _ => None,
        }
    });
    state
}

/// A coin with the token `TOK` activated (its denom is in the coins file under another ticker), a coins file that
/// names more tokens of this platform and one of another platform, and the denom `unew` in the file with no token
/// activated for it.
fn history_fixture() -> Fixture { history_fixture_for(&OWN_KEY, None) }

/// The fixture for the account of `key`; with `extra`, a second configured node.
fn history_fixture_for(key: &[u8], extra: Option<&FakeNode>) -> Fixture {
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({
            "netid": mm2_net_config::SUPPORTED_NETIDS[0],
            "coins": [{
                "coin": "TOK-CFG",
                "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                    "platform": "IRIS", "decimals": 6, "denom": "utoken" } }
            }, {
                "coin": "NEW-CFG",
                "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                    "platform": "IRIS", "decimals": 6, "denom": "unew" } }
            }, {
                "coin": "IRIS-CFG",
                "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                    "platform": "IRIS", "decimals": 6, "denom": "uconfigured" } }
            }, {
                "coin": "OTHER-CFG",
                "protocol": { "type": "TENDERMINTTOKEN", "protocol_data": {
                    "platform": "NUCLEUS", "decimals": 6, "denom": "uother" } }
            }]
        }))
        .into_mm_arc();
    let coin = common::block_on(tendermint_coin_from_conf_and_request(
        &ctx,
        Chain::Iris.ticker().to_owned(),
        &Chain::Iris.conf(),
        std::iter::once(RpcNode::for_test(&node.url()))
            .chain(extra.map(|n| RpcNode::for_test(&n.url())))
            .collect(),
        key,
        false,
    ))
    .unwrap_or_else(|e| panic!("cannot build the test coin: {}", e));
    coin.add_activated_token_info("TOK".to_owned(), 6, "utoken".parse().expect("a denom"));
    Fixture { coin, node, ctx }
}

fn me(fx: &Fixture) -> String { fx.coin.account_id.to_string() }

fn peer(_fx: &Fixture) -> String { account(Chain::Iris, &pubkey(&PEER_KEY)).to_string() }

/// A signed transaction of the coin's key with a distinguishing memo.
fn tx_bytes(fx: &Fixture, memo: &str) -> Vec<u8> {
    let me_id = fx.coin.account_id.clone();
    let peer_id = account(Chain::Iris, &pubkey(&PEER_KEY));
    let payload = msg_send_any(&me_id, &peer_id, vec![coin_proto("uiris", 1)]);
    signed_tx_with(&fx.coin, Chain::Iris, vec![payload], memo)
}

fn event_json(kind: &str, attributes: &[(&str, &str)]) -> Json {
    let attributes: Vec<Json> = attributes
        .iter()
        .map(|(key, value)| json!({ "key": key, "value": value, "index": true }))
        .collect();
    json!({ "type": kind, "attributes": attributes })
}

fn transfer_json(sender: &str, recipient: &str, amount: &str) -> Json {
    event_json("transfer", &[
        ("sender", sender),
        ("recipient", recipient),
        ("amount", amount),
    ])
}

/// One `tx_search` entry.
fn entry(tx: &[u8], height: u64, code: u32, events: Vec<Json>) -> Json {
    json!({
        "hash": sha256_upper(&[tx]),
        "height": height.to_string(),
        "index": 0,
        "tx_result": { "code": code, "events": events },
        "tx": base64::encode(tx),
    })
}

/// An outgoing send of 1 coin (events of the transaction included).
fn outgoing(fx: &Fixture, memo: &str, height: u64) -> Json {
    let (me, peer) = (me(fx), peer(fx));
    entry(&tx_bytes(fx, memo), height, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer, "1000000uiris"),
    ])
}

fn incoming(fx: &Fixture, memo: &str, height: u64) -> Json {
    let (me, peer) = (me(fx), peer(fx));
    entry(&tx_bytes(fx, memo), height, 0, vec![
        transfer_json(&peer, COLLECTOR, "5000uiris"),
        transfer_json(&peer, &me, "2000000uiris"),
    ])
}

fn storage_with_init() -> SqliteTxHistoryStorage {
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    storage
}

fn platform_history(fx: &Fixture, storage: &SqliteTxHistoryStorage) -> Vec<crate::TransactionDetails> {
    let page = common::block_on(storage.get_history(
        HistoryCoinType::AccountCoin {
            ticker: "IRIS".to_owned(),
            address: me(fx),
        },
        PagingOptionsEnum::default(),
        100,
    ))
    .unwrap();
    page.transactions
}

fn state_name(coin: &TendermintCoin) -> String {
    serde_json::to_value(coin.history_sync_state.lock().unwrap().clone()).unwrap()["state"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_pass_stores_each_row_once_and_the_state_follows_d36_10() {
    // T36.30, T36.36, T36.38 (b).
    let fx = history_fixture();
    let state = script_history(&fx);
    let (out, inc) = (outgoing(&fx, "one", 10), incoming(&fx, "two", 11));
    {
        let mut st = state.lock().unwrap();
        st.spender = vec![out.clone()];
        // The first transaction is returned by both queries.
        st.receiver = vec![out, inc];
    }
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    // Before the first pass: in progress.
    assert_eq!(state_name(&fx.coin), "InProgress");
    common::block_on(fetcher.pass()).expect("a pass");
    assert_eq!(state_name(&fx.coin), "Finished");

    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 2, "a transaction found by both queries is stored once");
    // The newest height first.
    assert_eq!(rows[0].block_height, 11);
    assert_eq!(rows[1].block_height, 10);
    assert_eq!(rows[0].memo.as_deref(), Some("two"));
    assert_eq!(rows[0].timestamp, BLOCK_TIME);
    // Stored in base units.
    assert_eq!(rows[1].total_amount.to_string(), "1005000");

    // Every `tx_search` of the first pass saw `InProgress`.
    assert!(state.lock().unwrap().states.iter().all(|s| s["state"] == "InProgress"));

    // A second pass and a restart (a new fetcher on the same database) add nothing.
    common::block_on(fetcher.pass()).expect("a second pass");
    let mut restarted = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(restarted.pass()).expect("a pass after a restart");
    assert_eq!(platform_history(&fx, &storage).len(), 2);
    // The later passes report `Finished` while they fetch.
    let states = state.lock().unwrap().states.clone();
    assert!(states.iter().skip(2).any(|s| s["state"] == "Finished"), "{states:?}");
}

#[test]
fn transactions_without_a_row_do_not_stop_the_pass() {
    // T36.35.
    let fx = history_fixture();
    let state = script_history(&fx);
    let me = me(&fx);
    let good = outgoing(&fx, "good", 10);
    let failed = entry(&tx_bytes(&fx, "failed"), 10, 5, vec![transfer_json(
        &me,
        COLLECTOR,
        "5000uiris",
    )]);
    let no_time = outgoing(&fx, "no time", 20);
    let unknown_denom = entry(&tx_bytes(&fx, "foreign"), 10, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer(&fx), "1000000ufoo"),
    ]);
    let undecodable = json!({
        "hash": sha256_upper(&[b"garbage".as_slice()]),
        "height": "10", "index": 0,
        "tx_result": { "code": 0, "events": [transfer_json(&me, &peer(&fx), "1000000uiris")] },
        "tx": base64::encode(b"garbage"),
    });
    // A transaction with an empty fee list.
    let no_fee_tx = {
        let signing_key = SigningKey::from_slice(&OWN_KEY).expect("a signing key");
        let payload = msg_send_any(&fx.coin.account_id, &account(Chain::Iris, &pubkey(&PEER_KEY)), vec![
            coin_proto("uiris", 1),
        ]);
        let body = tx::Body::new(vec![payload], "no fee", 100u32);
        let fee = Fee {
            amount: vec![],
            gas_limit: 125_000,
            payer: None,
            granter: None,
        };
        let auth_info = SignerInfo::single_direct(Some(signing_key.public_key()), 1).auth_info(fee);
        let doc = SignDoc::new(&body, &auth_info, &fx.coin.protocol_info.chain_id, 1).expect("a sign doc");
        doc.sign(&signing_key).expect("signed").to_bytes().expect("bytes")
    };
    let no_fee = entry(&no_fee_tx, 10, 0, vec![transfer_json(&me, &peer(&fx), "1000000uiris")]);
    {
        let mut st = state.lock().unwrap();
        st.block_times.insert(20, None);
        st.spender = vec![failed, no_time, unknown_denom, undecodable, no_fee, good];
    }
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    // D36.20 (b): the transaction with the empty fee list has its row, with a fee of zero and no fee row.
    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.memo.as_deref() == Some("good")));
    let no_fee_row = rows
        .iter()
        .find(|row| row.memo.as_deref() == Some("no fee"))
        .expect("the row of the transaction without a fee");
    assert_eq!(no_fee_row.total_amount.to_string(), "1000000");
    match no_fee_row.fee_details.clone() {
        Some(TxFeeDetails::Tendermint(fee)) => {
            assert_eq!(fee.coin, "IRIS");
            assert_eq!(fee.amount, dec("0"));
        },
        other => panic!("fee details: {other:?}"),
    }

    // T36.48 (iv): a second pass does not write it again.
    common::block_on(fetcher.pass()).expect("a second pass");
    assert_eq!(platform_history(&fx, &storage).len(), 2);
}

#[test]
fn a_node_failure_in_a_page_is_retried_and_the_rows_are_present_once() {
    // T36.36, R36.14.3: the block query of the first transaction fails once.
    let fx = history_fixture();
    let state = script_history(&fx);
    {
        let mut st = state.lock().unwrap();
        st.spender = vec![outgoing(&fx, "a", 10), outgoing(&fx, "b", 11)];
        st.block_failures = 1;
    }
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    assert_eq!(platform_history(&fx, &storage).len(), 2);
    // The failed page was read again: the spender query was asked at least twice for page 1.
    let pages = fx
        .node
        .requests("tx_search")
        .into_iter()
        .filter(|p| p["query"].as_str().is_some_and(|q| q.contains("coin_spent")))
        .count();
    assert!(pages >= 2, "{pages}");
    assert_eq!(state_name(&fx.coin), "Finished");
}

#[test]
fn token_rows_and_the_fee_row_are_split_by_scope() {
    // T36.31, R36.14.2, D36.13 (a'): a coins-file denom counts whatever its platform: the activated token's row
    // carries the activated ticker, a configured token's row the entry's ticker, also for a token of another
    // platform.
    let fx = history_fixture();
    let state = script_history(&fx);
    let (me, peer) = (me(&fx), peer(&fx));
    let token_send = entry(&tx_bytes(&fx, "token"), 10, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer, "2500000utoken"),
    ]);
    let configured = entry(&tx_bytes(&fx, "configured"), 11, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer, "1000000uconfigured"),
    ]);
    let other_platform = entry(&tx_bytes(&fx, "other"), 12, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer, "1000000uother"),
    ]);
    state.lock().unwrap().spender = vec![token_send, configured, other_platform];
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");

    let platform = platform_history(&fx, &storage);
    assert_eq!(platform.len(), 3, "three fee rows and no token row");
    assert!(platform
        .iter()
        .all(|row| row.transaction_type == TransactionType::FeeForTokenTx));
    let token_page = common::block_on(storage.get_history(
        HistoryCoinType::AccountToken {
            platform: "IRIS".to_owned(),
            token_id: token_id_of_denom("utoken"),
            address: me.clone(),
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!(token_page.total, 1);
    assert_eq!(token_page.transactions[0].coin, "TOK");
    let configured_page = common::block_on(storage.get_history(
        HistoryCoinType::AccountToken {
            platform: "IRIS".to_owned(),
            token_id: token_id_of_denom("uconfigured"),
            address: me.clone(),
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!(configured_page.total, 1);
    assert_eq!(configured_page.transactions[0].coin, "IRIS-CFG");
    let other_page = common::block_on(storage.get_history(
        HistoryCoinType::AccountToken {
            platform: "IRIS".to_owned(),
            token_id: token_id_of_denom("uother"),
            address: me,
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!(other_page.total, 1);
    assert_eq!(other_page.transactions[0].coin, "OTHER-CFG");
}

fn ids_of(rows: &[crate::TransactionDetails]) -> Vec<BytesJson> {
    let mut ids: Vec<BytesJson> = rows.iter().map(|row| row.internal_id.clone()).collect();
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    ids
}

/// Every row of the platform coin's store in the account scope: the platform coin's and the given token's.
fn all_rows(fx: &Fixture, storage: &SqliteTxHistoryStorage, denoms: &[&str]) -> Vec<crate::TransactionDetails> {
    let mut rows = platform_history(fx, storage);
    for denom in denoms {
        let page = common::block_on(storage.get_history(
            HistoryCoinType::AccountToken {
                platform: "IRIS".to_owned(),
                token_id: token_id_of_denom(denom),
                address: me(fx),
            },
            PagingOptionsEnum::default(),
            100,
        ))
        .unwrap();
        rows.extend(page.transactions);
    }
    rows
}

#[test]
fn a_custom_token_absent_from_the_coins_file_takes_no_position_and_gets_no_row() {
    // T36.45 (ii) with a custom token, D36.13 (a'): the activated token `CUSTOM` is not in the coins file, so its
    // transfer is not counted and the two other rows keep the identifiers of T36.45 (i).
    let fx = history_fixture();
    fx.coin
        .add_activated_token_info("CUSTOM".to_owned(), 6, "ucustom".parse().expect("a denom"));
    let state = script_history(&fx);
    let (me, peer) = (me(&fx), peer(&fx));
    let tx = entry(&tx_bytes(&fx, "custom"), 10, 0, vec![
        transfer_json(&me, COLLECTOR, "5000uiris"),
        transfer_json(&me, &peer, "1000000uiris"),
        transfer_json(&me, &peer, "7000000ucustom"),
        transfer_json(&me, STRANGER, "2000000uiris"),
    ]);
    let hash = tx["hash"].as_str().expect("a hash").to_owned();
    state.lock().unwrap().spender = vec![tx];
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");

    let rows = all_rows(&fx, &storage, &["ucustom"]);
    assert_eq!(rows.len(), 2, "no row for the custom token");
    assert!(rows
        .iter()
        .all(|row| row.transaction_type == TransactionType::StandardTransfer));
    let ids = ids_of(&rows);
    let mut expected = vec![history_internal_id(0, &hash), history_internal_id(1, &hash)];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(ids, expected);
}

fn sentinel_survives(fx: &Fixture, storage: &SqliteTxHistoryStorage) -> bool {
    platform_history(fx, storage)
        .iter()
        .any(|row| row.internal_id == BytesJson::from(vec![200u8]))
}

#[test]
fn the_resolved_denominations_are_recorded_and_an_unchanged_set_does_not_rebuild() {
    // D36.21 (b).
    let fx = history_fixture();
    let state = script_history(&fx);
    state.lock().unwrap().spender = vec![outgoing(&fx, "a", 10)];
    let storage = storage_with_init();
    assert_eq!(common::block_on(storage.get_resolved_denoms("IRIS")).unwrap(), None);
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    let record = common::block_on(storage.get_resolved_denoms("IRIS"))
        .unwrap()
        .expect("a record after the first pass");
    for denom in ["uiris", "utoken", "uconfigured", "uother", "unew"] {
        assert!(record.contains(denom), "{denom} in {record}");
    }

    // A row the walk cannot produce stays through the next passes and a restart: nothing is rebuilt.
    common::block_on(storage.add_transactions_to_history("IRIS", vec![details(200, 5, &me(&fx), None)])).unwrap();
    common::block_on(fetcher.pass()).expect("a second pass");
    let mut restarted = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(restarted.pass()).expect("a pass after a restart");
    assert!(sentinel_survives(&fx, &storage));
    assert_eq!(platform_history(&fx, &storage).len(), 2);
}

#[test]
fn a_changed_resolution_rebuilds_the_store_once_to_what_an_empty_store_gets() {
    // D36.21 (b): a token activated for a denom the coins file names changes the ticker of its rows, so the store
    // is rebuilt from the chain, once.
    let fx = history_fixture();
    let state = script_history(&fx);
    let (me_address, peer_address) = (me(&fx), peer(&fx));
    let token_tx = entry(&tx_bytes(&fx, "new token"), 10, 0, vec![
        transfer_json(&me_address, COLLECTOR, "5000uiris"),
        transfer_json(&me_address, &peer_address, "3000000unew"),
    ]);
    state.lock().unwrap().spender = vec![outgoing(&fx, "plain", 11), token_tx];
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    let before = all_rows(&fx, &storage, &["unew"]);
    assert_eq!(before.len(), 3, "the plain row, the token row and its fee row");
    assert!(before.iter().any(|row| row.coin == "NEW-CFG"));

    // The token is activated: the same denom now resolves to another ticker.
    common::block_on(storage.add_transactions_to_history("IRIS", vec![details(200, 5, &me_address, None)])).unwrap();
    fx.coin
        .add_activated_token_info("NEW".to_owned(), 6, "unew".parse().expect("a denom"));
    common::block_on(fetcher.pass()).expect("a pass after the activation");
    assert!(!sentinel_survives(&fx, &storage), "the store was rebuilt");
    let after = all_rows(&fx, &storage, &["unew"]);
    assert_eq!(after.len(), 3, "each row is present once");
    assert!(after.iter().any(|row| row.coin == "NEW"));
    assert!(after.iter().all(|row| row.coin != "NEW-CFG"));

    // The rebuilt store equals what a fresh fetcher writes into an empty store under the same configuration.
    let fresh = storage_with_init();
    common::block_on(HistoryFetcher::for_test(fx.coin.clone(), fresh.clone()).pass()).expect("a fresh pass");
    assert_eq!(ids_of(&after), ids_of(&all_rows(&fx, &fresh, &["unew"])));

    // No further change: no further rebuild.
    common::block_on(storage.add_transactions_to_history("IRIS", vec![details(200, 5, &me_address, None)])).unwrap();
    common::block_on(fetcher.pass()).expect("a pass without a change");
    assert!(sentinel_survives(&fx, &storage));
    assert_eq!(state_name(&fx.coin), "Finished");
}

#[test]
fn a_store_that_cannot_be_written_stops_the_fetcher_with_an_error_state() {
    // T36.38: `Error` with a message; the fetcher has stopped for good.
    let fx = history_fixture();
    let state = script_history(&fx);
    state.lock().unwrap().spender = vec![outgoing(&fx, "a", 10)];
    let storage = SqliteTxHistoryStorage::in_memory();
    // A table of the right name with the wrong columns: the store is prepared (`IF NOT EXISTS`) but cannot be
    // written.
    {
        let conn = storage.0.lock().unwrap();
        conn.execute("CREATE TABLE IRIS_tx_history (x INTEGER)", []).unwrap();
    }
    let result = common::block_on(HistoryFetcher::for_test(fx.coin.clone(), storage).run());
    assert!(result.is_err());
    let state = serde_json::to_value(fx.coin.history_sync_state.lock().unwrap().clone()).unwrap();
    assert_eq!(state["state"], "Error");
    assert!(state["additional_info"]["message"]
        .as_str()
        .is_some_and(|m| !m.is_empty()));
}

#[test]
fn a_failed_first_balance_read_is_retried_and_a_balance_change_starts_another_pass() {
    // D36.10 (b), R36.14.3.
    let fx = history_fixture();
    let state = script_history(&fx);
    {
        let mut st = state.lock().unwrap();
        st.spender = vec![outgoing(&fx, "first", 10)];
        st.balance_failures = 2;
    }
    let storage = storage_with_init();
    let fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    let (task, handle) = futures::future::abortable(fetcher.run());
    common::executor::spawn(async move {
        let _ = task.await;
    });

    let wait_for = |what: &str, condition: &dyn Fn() -> bool| {
        for _ in 0..500 {
            if condition() {
                return;
            }
            common::block_on(common::executor::Timer::sleep(0.02));
        }
        panic!("timed out waiting for {what}");
    };
    wait_for("the first pass", &|| {
        platform_history(&fx, &storage).len() == 1 && state_name(&fx.coin) == "Finished"
    });
    assert_eq!(state_name(&fx.coin), "Finished");

    // A new transaction and a changed balance: another pass.
    {
        let mut st = state.lock().unwrap();
        let second = incoming(&fx, "second", 11);
        st.receiver = vec![second];
        st.balance += 1;
    }
    wait_for("the second pass", &|| platform_history(&fx, &storage).len() == 2);
    assert_eq!(state_name(&fx.coin), "Finished");
    handle.abort();
}

// ---------------------------------------------------------------------------
// The history method (R36.14.1, R36.14.12)
// ---------------------------------------------------------------------------

fn history_request(coin: &str) -> crate::my_tx_history_v2::MyTxHistoryRequestV2 {
    serde_json::from_value(json!({ "coin": coin })).expect("a request")
}

#[test]
fn the_history_method_answers_with_and_without_a_store() {
    // T36.39.
    let fx = history_fixture();
    let _state = script_history(&fx);
    let coins_ctx = crate::CoinsContext::from_ctx(&fx.ctx).expect("the coins context");
    common::block_on(coins_ctx.add_coin(fx.coin.clone().into())).expect("register the platform");
    let token = TendermintToken::new("TOK".to_owned(), fx.coin.clone(), 6, "utoken".parse().expect("a denom"));
    common::block_on(coins_ctx.add_coin(token.into())).expect("register the token");
    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("a database");
    fx.ctx
        .sqlite_connection
        .pin(Arc::new(Mutex::new(connection)))
        .expect("pin the database");

    // No store for the platform ticker: StorageIsNotInitialized for the coin and for the token.
    for coin in ["IRIS", "TOK"] {
        let err = common::block_on(my_tx_history_v2_rpc(fx.ctx.clone(), history_request(coin)))
            .err()
            .expect("an error")
            .into_inner();
        assert!(matches!(err, MyTxHistoryErrorV2::StorageIsNotInitialized(_)), "{coin}");
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // A store from an earlier activation with `tx_history: true`: the rows are served, in display units, the
    // state is `NotEnabled`, and a token sees its own rows.
    let storage = SqliteTxHistoryStorage(fx.ctx.sqlite_connection.as_option().expect("pinned").clone());
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let me = me(&fx);
    let mut platform_row = details(1, 10, &me, None);
    platform_row.total_amount = BigDecimal::from(1_005_000);
    platform_row.spent_by_me = BigDecimal::from(1_005_000);
    let mut token_row = details(2, 11, &me, Some("utoken"));
    token_row.coin = "TOK".to_owned();
    token_row.total_amount = BigDecimal::from(2_500_000);
    token_row.spent_by_me = BigDecimal::from(2_500_000);
    common::block_on(storage.add_transactions_to_history("IRIS", vec![platform_row, token_row])).unwrap();

    let answer = common::block_on(my_tx_history_v2_rpc(fx.ctx.clone(), history_request("IRIS")))
        .ok()
        .expect("an answer");
    let answer = serde_json::to_value(&answer).unwrap();
    assert_eq!(answer["total"], 1);
    assert_eq!(answer["sync_status"]["state"], "NotEnabled");
    assert_eq!(answer["current_block"], 100);
    let row = &answer["transactions"][0];
    assert_eq!(row["total_amount"], "1.005");
    assert_eq!(row["my_balance_change"], "-1.005");
    assert_eq!(row["confirmations"], 91);
    assert_eq!(row["memo"], "");

    let token_answer = common::block_on(my_tx_history_v2_rpc(fx.ctx.clone(), history_request("TOK")))
        .ok()
        .expect("an answer");
    let token_answer = serde_json::to_value(&token_answer).unwrap();
    assert_eq!(token_answer["total"], 1);
    assert_eq!(token_answer["transactions"][0]["total_amount"], "2.5");
    assert_eq!(token_answer["transactions"][0]["coin"], "TOK");

    // A stored amount that cannot be scaled is a storage error.
    let mut bad = details(3, 12, &me, None);
    bad.total_amount = dec("1.5");
    common::block_on(storage.add_transactions_to_history("IRIS", vec![bad])).unwrap();
    let err = common::block_on(my_tx_history_v2_rpc(fx.ctx.clone(), history_request("IRIS")))
        .err()
        .expect("an error")
        .into_inner();
    assert!(matches!(err, MyTxHistoryErrorV2::StorageError(_)));

    // The legacy method reads a store this family never writes: an empty list, whatever `tx_history` was.
    let legacy = common::block_on(crate::my_tx_history(fx.ctx.clone(), json!({ "coin": "IRIS" }))).expect("an answer");
    let legacy: Json = serde_json::from_slice(legacy.body()).unwrap();
    assert_eq!(legacy["result"]["total"], 0);
    assert_eq!(legacy["result"]["transactions"], json!([]));
    assert_eq!(legacy["result"]["sync_status"]["state"], "NotEnabled");
}

#[test]
fn history_state_values_serialize_as_the_chapter_binds() {
    // R36.14.11.
    assert_eq!(
        serde_json::to_value(HistorySyncState::InProgress(json!({ "transactions_left": 3 }))).unwrap(),
        json!({ "state": "InProgress", "additional_info": { "transactions_left": 3 } })
    );
    assert_eq!(
        serde_json::to_value(HistorySyncState::Error(json!({ "message": "x" }))).unwrap(),
        json!({ "state": "Error", "additional_info": { "message": "x" } })
    );
    assert_eq!(
        serde_json::to_value(HistorySyncState::NotEnabled).unwrap(),
        json!({ "state": "NotEnabled" })
    );
}

#[test]
fn the_new_transaction_types_have_the_bound_wire_shapes() {
    // R36.14.6.
    let token_id = token_id_of_denom("utoken");
    assert_eq!(
        serde_json::to_value(TransactionType::FeeForTokenTx).unwrap(),
        json!("FeeForTokenTx")
    );
    assert_eq!(
        serde_json::to_value(TransactionType::TendermintIBCTransfer { token_id: None }).unwrap(),
        json!({ "TendermintIBCTransfer": { "token_id": null } })
    );
    assert_eq!(
        serde_json::to_value(TransactionType::CustomTendermintMsg {
            msg_type: CustomTendermintMsgType::SignClaimHtlc,
            token_id: Some(token_id.clone())
        })
        .unwrap(),
        json!({ "CustomTendermintMsg": { "msg_type": "SignClaimHtlc", "token_id": hex::encode(&token_id.0) } })
    );
    assert_eq!(token_id.0.len(), 32);
    assert_eq!(hex::encode(&token_id.0).len(), 64);
}

#[test]
fn the_history_method_pages_through_the_store_with_exact_totals() {
    // R36.14.1, ch.53 paging, through the method itself: the store cuts the page, the method does not cut it again.
    let fx = history_fixture();
    let _state = script_history(&fx);
    let coins_ctx = crate::CoinsContext::from_ctx(&fx.ctx).expect("the coins context");
    common::block_on(coins_ctx.add_coin(fx.coin.clone().into())).expect("register the platform");
    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("a database");
    fx.ctx
        .sqlite_connection
        .pin(Arc::new(Mutex::new(connection)))
        .expect("pin the database");
    let storage = SqliteTxHistoryStorage(fx.ctx.sqlite_connection.as_option().expect("pinned").clone());
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let me = me(&fx);
    // Heights 10 to 13, served newest first: ids 4, 3, 2, 1.
    let rows: Vec<_> = (1u8..=4).map(|id| details(id, 9 + u64::from(id), &me, None)).collect();
    common::block_on(storage.add_transactions_to_history("IRIS", rows)).unwrap();

    let ask = |paging: Json| {
        let request =
            serde_json::from_value(json!({ "coin": "IRIS", "limit": 2, "paging_options": paging })).expect("a request");
        let answer = common::block_on(my_tx_history_v2_rpc(fx.ctx.clone(), request))
            .ok()
            .expect("an answer");
        serde_json::to_value(&answer).unwrap()
    };
    let ids = |answer: &Json| -> Vec<String> {
        answer["transactions"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["internal_id"].as_str().expect("an id").to_owned())
            .collect()
    };

    let first = ask(json!({ "PageNumber": 1 }));
    assert_eq!(first["total"], 4);
    assert_eq!(first["skipped"], 0);
    assert_eq!(first["total_pages"], 2);
    assert_eq!(ids(&first), vec!["04", "03"]);

    let second = ask(json!({ "PageNumber": 2 }));
    assert_eq!(second["total"], 4);
    assert_eq!(second["skipped"], 2);
    assert_eq!(ids(&second), vec!["02", "01"]);

    let from_id = ask(json!({ "FromId": "03" }));
    assert_eq!(from_id["total"], 4);
    assert_eq!(from_id["skipped"], 2);
    assert_eq!(ids(&from_id), vec!["02", "01"]);

    let unknown = ask(json!({ "FromId": "63" }));
    assert_eq!(unknown["total"], 4);
    assert_eq!(unknown["skipped"], 0);
    assert!(ids(&unknown).is_empty());
}

#[test]
fn a_relayed_ibc_receive_with_a_tip_gives_two_rows_by_order_pairing() {
    // The observed form of D36.22 (a): a packet received through a relayer whose fee is paid by a fee granter and
    // whose tip is paid on to a payee. The fee list is one coin of 1,928 base units (1,473 fee, 455 tip). Events in
    // the order of the result; no event involves a payment of this account.
    const GRANTER: &str = "iaa1granter";
    const ESCROW: &str = "iaa1escrow";
    const TIP_PAYEE: &str = "iaa1tippayee";
    let events = vec![
        event("coin_spent", &[("spender", GRANTER), ("amount", "1928uiris")]),
        transfer(GRANTER, COLLECTOR, "1928uiris"),
        event("message", &[("sender", GRANTER)]),
        event("coin_spent", &[("spender", ESCROW), ("amount", "696597uiris")]),
        event("coin_received", &[("receiver", ME), ("amount", "696597uiris")]),
        transfer(ESCROW, ME, "696597uiris"),
        event("fungible_token_packet", &[
            ("sender", OTHER_CHAIN),
            ("receiver", ME),
            ("denom", "transfer/channel-0/uiris"),
            ("amount", "696597"),
            ("success", "true"),
        ]),
        event("write_acknowledgement", &[("packet_sequence", "7")]),
        event("coin_spent", &[("spender", COLLECTOR), ("amount", "455uiris")]),
        transfer(COLLECTOR, TIP_PAYEE, "455uiris"),
        event("fee_pay", &[("fee", "1473uiris")]),
        event("tip_pay", &[("tip", "455uiris")]),
    ];
    let rows = rows_with_fee_list(events, vec![], &[(1_928, "uiris")]);

    // The fee payment (the transfer equal to the fee text) is left out. The remaining candidates are read last to
    // first: the tip transfer is position 0 and takes the packet event, the escrow transfer is position 1 and stays
    // a plain transfer. Both name this account after the correction, so both are served.
    assert_eq!(rows.len(), 2);
    assert!(at_position(&rows[0], 0));
    assert_eq!(
        rows[0].details.transaction_type,
        TransactionType::TendermintIBCTransfer { token_id: None }
    );
    assert_eq!(rows[0].details.from, vec![OTHER_CHAIN.to_owned()]);
    assert_eq!(rows[0].details.to, vec![ME.to_owned()]);
    assert_amounts(&rows[0], ["0.000455", "0", "0.000455", "0.000455"]);

    assert!(at_position(&rows[1], 1));
    assert_eq!(rows[1].details.transaction_type, TransactionType::StandardTransfer);
    assert_eq!(rows[1].details.from, vec![ESCROW.to_owned()]);
    assert_eq!(rows[1].details.to, vec![ME.to_owned()]);
    assert_amounts(&rows[1], ["0.696597", "0", "0.696597", "0.696597"]);

    // The fee was paid by the granter: it is in `fee_details` and in no amount of this account's rows.
    for row in &rows {
        assert_eq!(fee_details_of_row(row), ("IRIS".to_owned(), dec("0.001928")));
    }
}

// ---------------------------------------------------------------------------
// The address index of the store (R36.14.23 -- R36.14.26; T36.49 -- T36.52)
// ---------------------------------------------------------------------------

/// The entries of the address table of `ticker`, by identifier (lower-case hex), as sets of addresses.
fn entry_sets(storage: &SqliteTxHistoryStorage, ticker: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn = storage.0.lock().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT internal_id, address FROM {}_tx_address ORDER BY address;",
            ticker.replace('-', "_")
        ))
        .unwrap();
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .unwrap();
    let mut sets: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for row in rows {
        let (id, address) = row.unwrap();
        sets.entry(id).or_default().push(address);
    }
    sets
}

fn sorted(addresses: &[&str]) -> Vec<String> {
    let mut list: Vec<String> = addresses.iter().map(|a| (*a).to_owned()).collect();
    list.sort();
    list
}

/// Stores the drafts as one page of an index-enabled store and returns the entries of each draft, in draft order.
fn entries_of_drafts(drafts: &[RowDraft]) -> Vec<Vec<String>> {
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let stored: Vec<_> = drafts
        .iter()
        .map(|d| stored_form(d.details.clone(), d.decimals))
        .collect();
    common::block_on(storage.store_page("IRIS", vec![], stored.clone())).unwrap();
    let sets = entry_sets(&storage, "IRIS");
    stored
        .iter()
        .map(|row| {
            sets.get(&format!("{:02x}", row.internal_id))
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn index_entries_follow_the_rows_of_each_kind() {
    // T36.49 (i) -- (vii).
    // (i) a send from this account to another has the entries of both.
    let sent = rows(vec![fee_from(ME), transfer(ME, PEER, "1000000uiris")]);
    assert_eq!(entries_of_drafts(&sent), vec![sorted(&[ME, PEER])]);
    // (ii) a send to itself has one.
    let itself = rows(vec![fee_from(ME), transfer(ME, ME, "1000000uiris")]);
    assert_eq!(entries_of_drafts(&itself), vec![sorted(&[ME])]);
    // (iii) a transfer between two other accounts has theirs and none for this account.
    let others = rows(vec![
        fee_from(PEER),
        transfer(PEER, "iaa1y", "1000000uiris"),
        transfer(PEER, ME, "1000000uiris"),
    ]);
    let entries = entries_of_drafts(&others);
    let index_of = |to: &str| others.iter().position(|d| d.details.to == vec![to.to_owned()]).unwrap();
    assert_eq!(entries[index_of("iaa1y")], sorted(&[PEER, "iaa1y"]));
    assert!(entries[index_of(ME)].contains(&ME.to_owned()));
    // (iv) a token send: the token row has both parties, the fee row this account under its own identifier.
    let token = rows(vec![fee_from(ME), transfer(ME, PEER, "2500000utoken")]);
    assert_eq!(entries_of_drafts(&token), vec![sorted(&[ME, PEER]), sorted(&[ME])]);
    // (v) an HTLC creation by this account, and a claim by another account that pays it, have this account only.
    let create = rows(vec![
        fee_from(ME),
        transfer(ME, MODULE, "1000000uiris"),
        event("create_htlc", &[("sender", ME), ("receiver", PEER)]),
    ]);
    assert_eq!(entries_of_drafts(&create), vec![sorted(&[ME])]);
    let claim = rows(vec![
        fee_from(PEER),
        transfer(MODULE, ME, "1000000uiris"),
        event("claim_htlc", &[("sender", PEER)]),
    ]);
    assert_eq!(entries_of_drafts(&claim), vec![sorted(&[ME])]);
    // (vi) staking: delegation has the delegator and the validator, undelegation the delegator, a reward claim the
    // validator and the delegator.
    let delegation = rows(vec![event("delegate", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ])]);
    assert_eq!(entries_of_drafts(&delegation), vec![sorted(&[ME, VALIDATOR])]);
    let undelegation = rows(vec![event("unbond", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "1000000uiris"),
    ])]);
    assert_eq!(entries_of_drafts(&undelegation), vec![sorted(&[ME])]);
    let reward = rows(vec![event("withdraw_rewards", &[
        ("delegator", ME),
        ("validator", VALIDATOR),
        ("amount", "12345uiris"),
    ])]);
    assert_eq!(entries_of_drafts(&reward), vec![sorted(&[ME, VALIDATOR])]);
    // (vii) an incoming IBC transfer has the other chain's address and this account.
    let ibc = rows(vec![
        fee_from("iaa1relayer"),
        transfer("iaa1escrow", ME, "1000000uiris"),
        event("fungible_token_packet", &[("sender", OTHER_CHAIN), ("receiver", ME)]),
    ]);
    assert_eq!(entries_of_drafts(&ibc), vec![sorted(&[ME, OTHER_CHAIN])]);
}

/// Sets the entries of the address table to exactly `entries` (the contract applied by hand).
fn set_entries(storage: &SqliteTxHistoryStorage, entries: &[(&str, &str)]) {
    let conn = storage.0.lock().unwrap();
    conn.execute("DELETE FROM IRIS_tx_address", []).unwrap();
    for (id, address) in entries {
        conn.execute("INSERT INTO IRIS_tx_address (internal_id, address) VALUES (?1, ?2)", [
            *id, *address,
        ])
        .unwrap();
    }
}

#[test]
fn selection_total_and_paging_go_through_the_address_index() {
    // T36.50.
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let rows = vec![
        details(1, 12, ME, None),
        details(2, 12, ME, None),
        details(3, 10, "iaa1x", None),
        details(4, 11, ME, None),
        details(5, 11, ME, Some("utoken")),
        details(6, 9, ME, None),
    ];
    common::block_on(storage.add_transactions_to_history("IRIS", rows)).unwrap();
    let x = "iaa1x";
    set_entries(&storage, &[
        ("01", ME),
        ("01", x),
        ("02", ME),
        ("03", x),
        ("03", "iaa1y"),
        ("05", ME),
        ("05", x),
        ("06", ME),
        ("06", ME),
    ]);
    let scope = || HistoryCoinType::AccountCoin {
        ticker: "IRIS".to_owned(),
        address: ME.to_owned(),
    };
    let ids = |page: &crate::my_tx_history_v2::GetHistoryResult| -> Vec<u8> {
        page.transactions.iter().map(|d| d.internal_id.0[0]).collect()
    };
    let page = |paging, limit| common::block_on(storage.get_history(scope(), paging, limit)).unwrap();
    let nth = |n: usize| PagingOptionsEnum::PageNumber(std::num::NonZeroUsize::new(n).unwrap());

    // The row without an entry (4) is not served and not counted; the entry stored twice counts once.
    let all = page(PagingOptionsEnum::default(), 10);
    assert_eq!(all.total, 3);
    assert_eq!(ids(&all), vec![1, 2, 6]);
    let second = page(nth(2), 2);
    assert_eq!((ids(&second), second.skipped, second.total), (vec![6], 2, 3));
    let third = page(nth(3), 2);
    assert_eq!((ids(&third), third.skipped, third.total), (vec![], 4, 3));
    let after_first = page(PagingOptionsEnum::FromId(BytesJson::from(vec![1u8])), 10);
    assert_eq!((ids(&after_first), after_first.skipped), (vec![2, 6], 1));
    // A row of other parties, a row without an entry, a row of another token and an unknown identifier.
    for anchor in [3u8, 4, 5, 99] {
        let none = page(PagingOptionsEnum::FromId(BytesJson::from(vec![anchor])), 10);
        assert_eq!(
            (ids(&none), none.skipped, none.total),
            (vec![], 0, 3),
            "anchor {anchor}"
        );
    }
    let token = common::block_on(storage.get_history(
        HistoryCoinType::AccountToken {
            platform: "IRIS".to_owned(),
            token_id: token_id_of_denom("utoken"),
            address: ME.to_owned(),
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!((ids(&token), token.total), (vec![5], 1));

    // An entry for this account is added to row 4: it stands between 2 and 6. An entry whose row does not exist
    // changes no answer.
    {
        let conn = storage.0.lock().unwrap();
        conn.execute(
            "INSERT INTO IRIS_tx_address (internal_id, address) VALUES ('04', ?1)",
            [ME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO IRIS_tx_address (internal_id, address) VALUES ('63', ?1)",
            [ME],
        )
        .unwrap();
    }
    let with_four = page(PagingOptionsEnum::default(), 10);
    assert_eq!((ids(&with_four), with_four.total), (vec![1, 2, 4, 6], 4));
}

#[test]
fn a_store_without_the_index_is_listed_after_the_preparation_and_a_second_one_adds_nothing() {
    // T36.51 (iv), R36.14.26 items 2 and 5.
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    // Rows written without the index (the state of a store of another program).
    common::block_on(
        storage.add_transactions_to_history("IRIS", vec![details(1, 12, ME, None), details(2, 11, "iaa1x", None)]),
    )
    .unwrap();
    assert!(common::block_on(storage.get_history(
        HistoryCoinType::AccountCoin {
            ticker: "IRIS".to_owned(),
            address: ME.to_owned()
        },
        PagingOptionsEnum::default(),
        10
    ))
    .is_err());

    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    let before = entry_sets(&storage, "IRIS");
    assert_eq!(before.get("01"), Some(&sorted(&[ME, PEER])));
    assert_eq!(platform_ids(&storage), vec![1]);
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    assert_eq!(entry_sets(&storage, "IRIS"), before);
}

fn platform_ids(storage: &SqliteTxHistoryStorage) -> Vec<u8> {
    let page = common::block_on(storage.get_history(
        HistoryCoinType::AccountCoin {
            ticker: "IRIS".to_owned(),
            address: ME.to_owned(),
        },
        PagingOptionsEnum::default(),
        100,
    ))
    .unwrap();
    page.transactions.iter().map(|d| d.internal_id.0[0]).collect()
}

#[test]
fn preparing_the_store_removes_entries_without_a_row_and_keeps_a_partly_indexed_row() {
    // R36.14.26 items 2 and 5.
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    common::block_on(storage.add_transactions_to_history("IRIS", vec![details(1, 12, ME, None)])).unwrap();
    set_entries(&storage, &[("01", ME), ("63", ME), ("63", "iaa1x")]);
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();
    // The entries of the missing row are gone; the row that has one entry is not topped up.
    assert_eq!(entry_sets(&storage, "IRIS").into_iter().collect::<Vec<_>>(), vec![(
        "01".to_owned(),
        sorted(&[ME])
    )]);
}

#[test]
fn an_earlier_identifier_form_is_removed_with_its_entries_by_the_pass() {
    // T36.51 (v), R36.14.24 item 4.
    let fx = history_fixture();
    let state = script_history(&fx);
    let sent = outgoing(&fx, "old form", 10);
    let hash = sent["hash"].as_str().unwrap().to_owned();
    state.lock().unwrap().spender = vec![sent];
    let storage = storage_with_init();
    let [old_id, _] = obsolete_internal_ids(0, &hash);
    let mut old = details(1, 10, &me(&fx), None);
    old.tx_hash = hash.clone();
    old.internal_id = old_id.clone();
    common::block_on(storage.add_transactions_to_history("IRIS", vec![old])).unwrap();
    let old_text = format!("{:02x}", old_id);
    assert!(entry_sets(&storage, "IRIS").contains_key(&old_text));

    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    assert!(common::block_on(storage.get_tx_from_history("IRIS", &old_id))
        .unwrap()
        .is_none());
    let sets = entry_sets(&storage, "IRIS");
    assert!(!sets.contains_key(&old_text));
    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 1);
    let new_id = format!("{:02x}", rows[0].internal_id);
    assert!(sets.get(&new_id).is_some_and(|entries| entries.contains(&me(&fx))));
}

#[test]
fn a_first_pass_with_one_received_transfer_asks_spender_then_receiver_and_stores_the_ibc_row() {
    // T36.52 (i).
    let fx = history_fixture();
    let state = script_history(&fx);
    let me = me(&fx);
    let relayed = entry(&tx_bytes(&fx, "relayed"), 10, 0, vec![
        transfer_json("iaa1relayer", COLLECTOR, "5000uiris"),
        transfer_json("iaa1escrow", me.as_str(), "1000000uiris"),
        event_json("fungible_token_packet", &[
            ("sender", OTHER_CHAIN),
            ("receiver", me.as_str()),
        ]),
    ]);
    state.lock().unwrap().receiver = vec![relayed];
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    assert_eq!(state_name(&fx.coin), "InProgress");
    common::block_on(fetcher.pass()).expect("a pass");
    assert_eq!(state_name(&fx.coin), "Finished");

    // One request for page 1 of each query, the spender first, one block read, no second page.
    let searches: Vec<(String, String)> = fx
        .node
        .requests("tx_search")
        .iter()
        .map(|p| {
            let query = p["query"].as_str().unwrap_or_default();
            let kind = if query.contains("coin_spent") {
                "spender"
            } else {
                "receiver"
            };
            (kind.to_owned(), p["page"].to_string().replace('"', ""))
        })
        .collect();
    assert_eq!(searches, vec![
        ("spender".to_owned(), "1".to_owned()),
        ("receiver".to_owned(), "1".to_owned())
    ]);
    let block_reads = fx
        .node
        .requests("abci_query")
        .iter()
        .filter(|p| {
            p["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("GetBlockByHeight"))
        })
        .count();
    assert_eq!(block_reads, 1);

    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].transaction_type, TransactionType::TendermintIBCTransfer {
        token_id: None
    });
    assert_eq!(rows[0].from, vec![OTHER_CHAIN.to_owned()]);
    assert_eq!(rows[0].to, vec![me.clone()]);
    assert_eq!(rows[0].total_amount.to_string(), "1000000");
    let sets = entry_sets(&storage, "IRIS");
    assert_eq!(sets.len(), 1);
    assert_eq!(sets.values().next().unwrap(), &sorted(&[me.as_str(), OTHER_CHAIN]));
}

#[test]
fn a_received_bank_send_gives_one_standard_row_and_a_long_second_query_is_read_page_by_page() {
    // T36.52 (ii), (iii).
    let fx = history_fixture();
    let state = script_history(&fx);
    let me = me(&fx);
    let from_other = entry(&tx_bytes(&fx, "from x"), 10, 0, vec![
        transfer_json(&peer(&fx), COLLECTOR, "5000uiris"),
        transfer_json(&peer(&fx), me.as_str(), "2000000uiris"),
    ]);
    state.lock().unwrap().receiver = vec![from_other];
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].transaction_type, TransactionType::StandardTransfer);

    // 120 transactions in the second query: pages 1 to 3 of it, one request each.
    let many = history_fixture();
    let state = script_history(&many);
    state.lock().unwrap().receiver = (0..120).map(|n| incoming(&many, &format!("m{n}"), 10 + n)).collect();
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(many.coin.clone(), storage);
    common::block_on(fetcher.pass()).expect("a pass");
    let receiver_pages: Vec<String> = many
        .node
        .requests("tx_search")
        .iter()
        .filter(|p| p["query"].as_str().is_some_and(|q| q.contains("coin_received")))
        .map(|p| p["page"].to_string().replace('"', ""))
        .collect();
    assert_eq!(receiver_pages, vec!["1", "2", "3"]);
}

#[test]
fn a_query_that_ended_on_a_full_page_is_read_again_from_that_page_without_asking_past_the_end() {
    // T36.53 (iv) under D36.25 (b).
    let fx = history_fixture();
    let state = script_history(&fx);
    state.lock().unwrap().spender = (0..50).map(|n| outgoing(&fx, &format!("s{n}"), 10 + n)).collect();
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());

    // The requests made since the last call, as (query, page).
    let seen = std::cell::Cell::new(0usize);
    let requests_since = || -> Vec<(String, String)> {
        let all = fx.node.requests("tx_search");
        let new: Vec<(String, String)> = all
            .iter()
            .skip(seen.get())
            .map(|p| {
                let kind = if p["query"].as_str().is_some_and(|q| q.contains("coin_spent")) {
                    "spender"
                } else {
                    "receiver"
                };
                (kind.to_owned(), p["page"].to_string().replace('"', ""))
            })
            .collect();
        seen.set(all.len());
        new
    };
    let request = |kind: &str, page: &str| (kind.to_owned(), page.to_owned());

    // First pass: the first query is exactly one full page; no page 2 is asked for.
    common::block_on(fetcher.pass()).expect("the first pass");
    assert_eq!(requests_since(), vec![
        request("spender", "1"),
        request("receiver", "1")
    ]);
    assert_eq!(platform_history(&fx, &storage).len(), 50);

    // Second pass, unchanged answers: the same page again, the stored rows are passed over, the second query runs.
    common::block_on(fetcher.pass()).expect("the second pass");
    assert_eq!(requests_since(), vec![
        request("spender", "1"),
        request("receiver", "1")
    ]);
    assert_eq!(platform_history(&fx, &storage).len(), 50);

    // A further transaction: `total_count` now shows a second page, which is asked for once.
    state.lock().unwrap().spender.push(outgoing(&fx, "s50", 60));
    common::block_on(fetcher.pass()).expect("the third pass");
    assert_eq!(requests_since(), vec![
        request("spender", "1"),
        request("spender", "2"),
        request("receiver", "1")
    ]);
    assert_eq!(platform_history(&fx, &storage).len(), 51);

    // Nothing new: the pass starts on page 2 and asks for nothing past it.
    common::block_on(fetcher.pass()).expect("the fourth pass");
    assert_eq!(requests_since(), vec![
        request("spender", "2"),
        request("receiver", "1")
    ]);
    assert_eq!(platform_history(&fx, &storage).len(), 51);
}

fn history_total_of(storage: &SqliteTxHistoryStorage, address: &str) -> usize {
    common::block_on(storage.get_history(
        HistoryCoinType::AccountCoin {
            ticker: "IRIS".to_owned(),
            address: address.to_owned(),
        },
        PagingOptionsEnum::default(),
        1,
    ))
    .unwrap()
    .total
}

#[test]
fn a_block_that_the_first_node_lacks_is_asked_of_the_other_nodes() {
    // T36.53 (iii), D36.24 (b): the first node answers without a block time, the second has the block.
    let second = FakeNode::start();
    let fx = history_fixture_for(&OWN_KEY, Some(&second));
    let state_a = script_history(&fx);
    let _state_b = script_history_on(&second, &fx.coin);
    {
        let mut st = state_a.lock().unwrap();
        st.block_times.insert(20, None);
        st.spender = vec![outgoing(&fx, "has time", 10), outgoing(&fx, "first node lacks it", 20)];
    }
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("a pass");
    assert_eq!(history_total_of(&storage, &me(&fx)), 2);
    assert!(
        !second.requests("abci_query").is_empty(),
        "the second node was asked for the block"
    );
}

#[test]
fn a_block_that_no_node_has_keeps_the_earliest_such_page_for_the_next_pass() {
    // T36.53 (iii), D36.24 (b): no row is written for it, and every later pass reads from the earliest page that
    // held such a transaction. Transactions at positions 60 (page 2) and 110 (page 3) of 120 lack their block time.
    let second = FakeNode::start();
    let fx = history_fixture_for(&OWN_KEY, Some(&second));
    let state_a = script_history(&fx);
    let state_b = script_history_on(&second, &fx.coin);
    let heights = |n: u64| match n {
        60 => 500,
        110 => 600,
        _ => 10 + n,
    };
    {
        let mut a = state_a.lock().unwrap();
        a.spender = (0..120).map(|n| outgoing(&fx, &format!("s{n}"), heights(n))).collect();
        a.block_times.insert(500, None);
        a.block_times.insert(600, None);
        let mut b = state_b.lock().unwrap();
        b.block_times.insert(500, None);
        b.block_times.insert(600, None);
    }
    let storage = storage_with_init();
    let mut fetcher = HistoryFetcher::for_test(fx.coin.clone(), storage.clone());
    common::block_on(fetcher.pass()).expect("the first pass");
    assert_eq!(
        history_total_of(&storage, &me(&fx)),
        118,
        "the two transactions have no row"
    );

    // The blocks become known. The next pass starts the first query on page 2, the earliest page that lost a row.
    let before = fx.node.requests("tx_search").len();
    state_a.lock().unwrap().block_times.clear();
    state_b.lock().unwrap().block_times.clear();
    common::block_on(fetcher.pass()).expect("the second pass");
    let spender_pages: Vec<String> = fx
        .node
        .requests("tx_search")
        .iter()
        .skip(before)
        .filter(|p| p["query"].as_str().is_some_and(|q| q.contains("coin_spent")))
        .map(|p| p["page"].to_string().replace('"', ""))
        .collect();
    assert_eq!(spender_pages, vec!["2", "3"]);
    assert_eq!(history_total_of(&storage, &me(&fx)), 120);
}

#[test]
fn a_second_account_on_the_same_store_is_listed_with_the_rows_of_the_first() {
    // T36.54 under D36.23 (a): no account record and no rebuild; a row carries the values of the account that wrote
    // it, and the second account's own rows are written beside it without duplicates.
    let storage = SqliteTxHistoryStorage::in_memory();
    common::block_on(storage.init("IRIS")).unwrap();
    common::block_on(storage.backfill_address_index("IRIS")).unwrap();

    // The store is filled for the first account with one send of 1,000,000 base units to the second account.
    let first = history_fixture();
    let first_state = script_history(&first);
    let sent = outgoing(&first, "from the first", 10);
    first_state.lock().unwrap().spender = vec![sent.clone()];
    let mut first_fetcher = HistoryFetcher::for_test(first.coin.clone(), storage.clone());
    common::block_on(first_fetcher.pass()).expect("the first account's pass");
    let (a, b) = (me(&first), peer(&first));
    assert_eq!(history_total_of(&storage, &a), 1);

    // The coin is activated for the second account on the same database. Its node returns the same transaction as a
    // received one, and one transaction of its own.
    let second = history_fixture_for(&PEER_KEY, None);
    assert_eq!(me(&second), b);
    let second_state = script_history(&second);
    {
        let mut st = second_state.lock().unwrap();
        st.receiver = vec![sent];
        st.spender = vec![outgoing(&second, "from the second", 11)];
    }
    let mut second_fetcher = HistoryFetcher::for_test(second.coin.clone(), storage.clone());
    common::block_on(second_fetcher.pass()).expect("the second account's pass");

    // The second account's list holds its own row and the first account's row, with the first account's values.
    let rows = platform_history(&second, &storage);
    assert_eq!(rows.len(), 2);
    let carried = rows
        .iter()
        .find(|row| row.memo.as_deref() == Some("from the first"))
        .expect("the row of the first account");
    // The store holds base units (1.005 in display units); the method scales when it serves.
    assert_eq!(carried.spent_by_me.to_string(), "1005000");
    assert_eq!(carried.received_by_me.to_string(), "0");
    assert_eq!(carried.from, vec![a.clone()]);
    // The first account still lists its row, and no row exists twice.
    assert_eq!(history_total_of(&storage, &a), 1);
    let all_rows: i64 = storage
        .0
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM IRIS_tx_history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(all_rows, 2);
}
