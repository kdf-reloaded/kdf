//! Tests of the Tendermint transaction history (CRD ch.36 §36.14.1: T36.30 -- T36.39).
//!
//! The classification tests feed the events of a transaction to the pure function and need no node. The fetcher
//! tests run it against the scripted node of the swap-flow tests, which serves `tx_search`, the block query and the
//! balance query from a list kept in the test. The platform coin has 6 decimals, the token `TOK` (denom `utoken`)
//! 6 decimals, and every fee is 5,000 base units of the platform denom (0.005). The figures are computed from the
//! rules. No network is used.

use super::super::htlc::CustomTendermintMsgType;
use super::super::tendermint_tx_history::{fee_row_internal_id, history_internal_id, obsolete_internal_ids,
                                          rows_for_tx, served_form, stored_form, token_id_of_denom, ClassifyContext,
                                          DenomInfo, HistoryEvent, HistoryFetcher, MessagePayment, RowDraft, TxFacts};
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
const HASH: &str = "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB";
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
        fee_amount: 5_000,
        fee_denom: "uiris".to_owned(),
        gas_limit: 125_000,
        events,
        raw_tx: vec![1, 2, 3],
        payments,
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

// provisional reading, pending ch.36 §36.14.6
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

// provisional reading, pending ch.36 §36.14.6
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

// provisional reading, pending ch.36 §36.14.6
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

// provisional reading, pending ch.36 §36.14.6
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

// provisional reading, pending ch.36 §36.14.6
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
    // Three transfers: the two sends of the same parties and denom are merged, so the first row holds 3 coins and
    // bears the fee; the token row follows, and the fee has no row of its own.
    assert_eq!(split.len(), 3);
    assert_amounts(&split[0], ["3.005", "3.005", "0", "-3.005"]);
    assert_eq!(
        split[1].details.transaction_type,
        TransactionType::TokenTransfer(token_id_of_denom("utoken"))
    );
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

// ---------------------------------------------------------------------------
// Identifier, stored form and order (R36.14.10; D36.11 (a))
// ---------------------------------------------------------------------------

// provisional reading, pending ch.36 §36.14.6
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

fn script_history(fx: &Fixture) -> SharedState {
    let state = Arc::new(Mutex::new(NodeState {
        spender: Vec::new(),
        receiver: Vec::new(),
        block_times: HashMap::new(),
        block_failures: 0,
        balance_failures: 0,
        balance: 1_000_000,
        states: Vec::new(),
    }));
    let (node_state, coin) = (state.clone(), fx.coin.clone());
    fx.node.answer(move |method, params| {
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

/// A coin with the token `TOK` activated and the coins file naming one more token of this platform.
fn history_fixture() -> Fixture {
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({
            "netid": mm2_net_config::SUPPORTED_NETIDS[0],
            "coins": [{
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
        vec![RpcNode::for_test(&node.url())],
        &OWN_KEY,
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
    let rows = platform_history(&fx, &storage);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].memo.as_deref(), Some("good"));
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
    // T36.31, R36.14.2, D36.13 (b): an activated token and a configured token of this platform resolve; a
    // configured token of another platform does not.
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
    assert_eq!(
        platform.len(),
        2,
        "two fee rows, no token row, no row of the other platform's token"
    );
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
            address: me,
        },
        PagingOptionsEnum::default(),
        10,
    ))
    .unwrap();
    assert_eq!(configured_page.total, 1);
    assert_eq!(configured_page.transactions[0].coin, "IRIS-CFG");
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
