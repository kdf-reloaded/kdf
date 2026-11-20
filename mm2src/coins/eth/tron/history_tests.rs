//! Unit tests of the TRON transaction history (CRD ch.21 §21.10C): the record mapping, the fee figures, paging,
//! the merge over several own addresses, the token rows and their receipts, the history states, and the answers
//! of both `my_tx_history` methods.
//!
//! The listings and receipts are public Nile chain data, captured from the provider's account endpoints and a
//! node's `gettransactioninfobyid` (`test_fixtures/history/`). A stub node on the loopback interface serves them,
//! so the real HTTP layer and the real node client run. A loopback stub can never be a keyed TronGrid node by the
//! production rule, so the client of these tests is built with the stub marked as one
//! (`TronApiClient::new_with_history_nodes`); the rule itself is tested in `nodes.rs`.

use super::api::{TransactionInfoResponse, TronApiClient};
use super::history::{apply_report, bandwidth_used, collect_history, fee_beyond_resources, receipt_figures,
                     signed_transaction_bytes, trc20_history_row, tron_history_loop, trx_history_row, ReceiptFigures,
                     Trc20Event, WalkState};
use super::nodes::TronNode;
use super::proto;
use super::test_node::{Reply, StubNode};
use super::{parse_trc20_contract, tron_coin_from_conf_and_request, Network, TronAddress};
use crate::eth::{Erc20TokenInfo, EthCoin};
use crate::my_tx_history_v2::{my_tx_history_v2_rpc, MyTxHistoryErrorV2, MyTxHistoryRequestV2};
use crate::{lp_register_coin, CoinProtocol, HistorySyncState, MarketCoinOps, MmCoin, MmCoinEnum, RegisterCoinParams,
            TransactionDetails, TransactionType};
use bigdecimal::BigDecimal;
use common::{block_on, HttpStatusCode, StatusCode};
use crypto::CryptoCtx;
use ethereum_types::Address;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use prost::Message as _;
use serde_json::{json, Value as Json};
use std::collections::HashSet;
use std::num::NonZeroU64;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const W1: &str = "TEseHkQBKctJLvKJZexjwYshuGbudyzje2";
const W2: &str = "TA5PLpWVfwz9Z9yGYaQfbRMSYuV5WckEtp";
const HD_0_1: &str = "TGjsKJihzkBsBunBGH2dCMo3ZBGLfYNDoA";
const HD_0_3: &str = "TAr3S8gg3WJoRS9W3DwVhnj8pPNPtTfMzH";
/// The token of the fixtures (a 6-decimal test token on Nile).
const TOKEN: &str = "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf";
const TOKEN_TICKER: &str = "USDT-TRC20";
/// A second token contract, for a transaction that moves two tokens (not a token of the fixtures).
const OTHER_TOKEN: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const OTHER_TOKEN_TICKER: &str = "OTHER-TRC20";
/// A transaction id of this test file's own: it moves the same value between the same addresses in two tokens.
const TX_TWO_TOKENS: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// A TRX transfer of W1 to an address the chain had not created: 2 TRX, fee 1.1 TRX.
const TX_NEW_ACCOUNT: &str = "03105c110ff580f83dc72ae4f9d2aaf238edb2dc5492bc141c5d279377efbe1e";
/// A TRC20 `transfer` call of W1 that paid 0.345 TRX for bandwidth.
const TX_TOKEN_CALL: &str = "bddf4dced710ab154ba51f5c61c1b943143db3b064da61ee8cfe42b7d82c2c9b";
/// A TRC20 `transfer` call of W1 that paid nothing (bandwidth from the owner's own).
const TX_FREE_TOKEN_CALL: &str = "7f047458482637f693ba40b83a89f7a779b4bc522b8f3824d43d4c5e8520b7ce";
/// 3 TRX from W1 to the HD address /0/1, which the chain had not created.
const TX_W1_TO_HD: &str = "d4fef715a7b623f391beb741496ea2f05691e9fc72fce547a2a4a5d4399cf290";
/// 1 TRX from W1 to W2 (lab row W4), W2 not yet created.
const TX_W4: &str = "6d5af5f390b86ee67dcafdeaa173833dd583fd65937d203d1155625bc5401e7e";
/// 1 token from W1 to W2 (lab row W5).
const TX_W5: &str = "a419ff8fa0da5a83921ad11d3fa5190616cac50365ea27f48d55418fd57f3620";
/// A token transfer of another owner's asset type that names W2 (`TransferAssetContract`).
const TX_FOREIGN_ASSET: &str = "df8b906e";

const W1_TRANSACTIONS: &str = include_str!("test_fixtures/history/W1-transactions.json");
const W1_TRC20: &str = include_str!("test_fixtures/history/W1-transactions_trc20.json");
const W2_TRANSACTIONS: &str = include_str!("test_fixtures/history/W2-transactions.json");
const W2_TRC20: &str = include_str!("test_fixtures/history/W2-transactions_trc20.json");
const HD_0_1_TRANSACTIONS: &str = include_str!("test_fixtures/history/HD-0-1-transactions.json");
const HD_0_1_TRC20: &str = include_str!("test_fixtures/history/HD-0-1-transactions_trc20.json");
const HD_0_3_TRANSACTIONS: &str = include_str!("test_fixtures/history/HD-0-3-transactions.json");
const HD_0_3_TRC20: &str = include_str!("test_fixtures/history/HD-0-3-transactions_trc20.json");
const RECEIPTS: [&str; 7] = [
    include_str!("test_fixtures/history/info-H2_fund_trc20.json"),
    include_str!("test_fixtures/history/info-H2_fund_trx.json"),
    include_str!("test_fixtures/history/info-H4_trc20.json"),
    include_str!("test_fixtures/history/info-H4_trx.json"),
    include_str!("test_fixtures/history/info-H7_trc20.json"),
    include_str!("test_fixtures/history/info-W4_trx.json"),
    include_str!("test_fixtures/history/info-W5_trc20.json"),
];

/// The head block the stub reports: four blocks after the newest transaction of the fixtures (72862686).
const HEAD: u64 = 72_862_690;
/// The bandwidth price of the stub and of the mapping tests, in SUN per byte.
const PRICE: u64 = 1000;
/// A secret part of a node URL and a secret header value, for the tests that look for leaks.
const URL_SECRET: &str = "PATHSECRET";
const HEADER_SECRET: &str = "HEADER-SECRET-VALUE";

fn price() -> NonZeroU64 { NonZeroU64::new(PRICE).expect("a positive price") }

fn evm(base58: &str) -> Address { TronAddress::from_base58(base58).expect("valid").to_evm_address() }

fn mine(addresses: &[&str]) -> HashSet<Address> { addresses.iter().map(|address| evm(address)).collect() }

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).expect("a decimal") }

fn rows_of(fixture: &str) -> Vec<Json> {
    let listing: Json = serde_json::from_str(fixture).expect("a fixture is JSON");
    listing["data"].as_array().expect("a data array").clone()
}

fn listed_transaction(fixture: &str, tx_id_prefix: &str) -> Json {
    rows_of(fixture)
        .into_iter()
        .find(|row| row["txID"].as_str().is_some_and(|id| id.starts_with(tx_id_prefix)))
        .unwrap_or_else(|| panic!("the fixture lists {tx_id_prefix}"))
}

fn listed_event(fixture: &str, tx_id: &str) -> Trc20Event {
    let row = rows_of(fixture)
        .into_iter()
        .find(|row| row["transaction_id"] == tx_id)
        .unwrap_or_else(|| panic!("the fixture lists {tx_id}"));
    Trc20Event::read(&row)
        .expect("a readable event")
        .expect("a transfer event")
}

fn receipt_fixture(tx_id: &str) -> Option<&'static str> {
    RECEIPTS
        .iter()
        .copied()
        .find(|receipt| serde_json::from_str::<Json>(receipt).is_ok_and(|parsed| parsed["id"] == tx_id))
}

fn receipt_of(tx_id: &str) -> TransactionInfoResponse {
    serde_json::from_str(receipt_fixture(tx_id).expect("a captured receipt")).expect("a receipt decodes")
}

/// A member of the fee details as a decimal, whether it is serialized as a string or a number.
fn fee_decimal(details: &TransactionDetails, member: &str) -> BigDecimal {
    let fee = serde_json::to_value(details).expect("a row serializes")["fee_details"][member].clone();
    match fee {
        Json::String(text) => dec(&text),
        other => dec(&other.to_string()),
    }
}

fn fee_member(details: &TransactionDetails, member: &str) -> Json {
    serde_json::to_value(details).expect("a row serializes")["fee_details"][member].clone()
}

// ---------------------------------------------------------------------------------------------------------------
// The fee figures
// ---------------------------------------------------------------------------------------------------------------

/// `bandwidth_used` is the bytes the transaction consumed, whatever paid for them: the bytes drawn from the
/// owner's bandwidth plus the bytes the burned fee bought (bytes = net_usage + net_fee / getTransactionFee).
#[test]
fn bandwidth_used_counts_the_bytes_from_the_owners_bandwidth_and_the_bytes_paid_by_burning() {
    let figures = |net_usage, net_fee_sun| ReceiptFigures {
        fee_sun: net_fee_sun,
        net_fee_sun,
        energy_fee_sun: 0,
        net_usage,
        energy_usage_total: 0,
    };
    for (name, net_usage, net_fee_sun, expected) in [
        ("staked or free bandwidth only", 267, 0, 267),
        ("burned only", 0, 267_000, 267),
        ("mixed", 100, 167_000, 267),
        ("nothing", 0, 0, 0),
    ] {
        // The fee is all bandwidth, so a TRX transfer with these figures is no transfer to a new account.
        for is_trx_transfer in [false, true] {
            assert_eq!(
                bandwidth_used(&figures(net_usage, net_fee_sun), price(), is_trx_transfer),
                expected,
                "{name}"
            );
        }
    }
    // Another price: the same fee buys fewer bytes.
    let double = NonZeroU64::new(2000).expect("positive");
    assert_eq!(bandwidth_used(&figures(0, 267_000), double, false), 133);
}

/// A TRX transfer to a new account: `net_fee` is the fixed create-account charge, not bytes bought, so only the
/// bytes drawn from the owner's bandwidth count. Any other row with the same figures keeps the burned-bytes term.
#[test]
fn bandwidth_used_of_a_transfer_to_a_new_account_has_no_burned_bytes_term() {
    let figures = |net_usage| ReceiptFigures {
        fee_sun: 1_100_000,
        net_fee_sun: 100_000,
        energy_fee_sun: 0,
        net_usage,
        energy_usage_total: 0,
    };
    // Not covered by the owner's bandwidth.
    assert_eq!(bandwidth_used(&figures(0), price(), true), 0);
    // Covered by staked bandwidth.
    assert_eq!(bandwidth_used(&figures(268), price(), true), 268);
    // Not a TRX transfer: the general rule.
    assert_eq!(bandwidth_used(&figures(0), price(), false), 100);
}

/// The part of the fee beyond bandwidth and energy, which a transfer row reports as `account_creation_fee`.
#[test]
fn the_fee_beyond_bandwidth_and_energy_is_reported_only_when_positive() {
    let figures = |fee_sun, net_fee_sun, energy_fee_sun| ReceiptFigures {
        fee_sun,
        net_fee_sun,
        energy_fee_sun,
        net_usage: 0,
        energy_usage_total: 0,
    };
    assert_eq!(fee_beyond_resources(&figures(1_100_000, 100_000, 0)), Some(1_000_000));
    assert_eq!(fee_beyond_resources(&figures(1_302_100, 345_000, 957_100)), None);
    assert_eq!(fee_beyond_resources(&figures(0, 0, 0)), None);
    // A fee below its parts is not a remainder.
    assert_eq!(fee_beyond_resources(&figures(100, 345_000, 0)), None);
}

/// A node leaves a zero figure out of a receipt: a transfer paid from the owner's bandwidth answers `net_usage`
/// only.
#[test]
fn a_receipt_without_a_fee_reads_as_zero_fee_and_keeps_its_bandwidth() {
    let free = receipt_of("4f428ffbf96c562cba5e85de1f16db72e28ec6743df92344daf3eb91e0e4cc1d");
    assert_eq!(receipt_figures(&free).expect("figures"), ReceiptFigures {
        fee_sun: 0,
        net_fee_sun: 0,
        energy_fee_sun: 0,
        net_usage: 267,
        energy_usage_total: 0,
    });
    let token = receipt_of(TX_W5);
    assert_eq!(receipt_figures(&token).expect("figures"), ReceiptFigures {
        fee_sun: 1_302_100,
        net_fee_sun: 345_000,
        energy_fee_sun: 957_100,
        net_usage: 0,
        energy_usage_total: 14_650,
    });
}

// ---------------------------------------------------------------------------------------------------------------
// The record mapping, from the captured listings
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_trx_transfer_out_to_a_new_account_carries_the_amount_the_fee_and_the_creation_fee() {
    let row = listed_transaction(W1_TRANSACTIONS, TX_NEW_ACCOUNT);
    let details = trx_history_row("TRX", &row, &mine(&[W1]), price())
        .expect("a readable row")
        .expect("a row of the wallet");

    let receiver = TronAddress::from_hex("41adfeb5cd0ece274c700fd7ff360557ba248fdb8a")
        .expect("valid")
        .to_base58();
    assert_eq!(details.coin, "TRX");
    assert_eq!(details.tx_hash, TX_NEW_ACCOUNT);
    assert_eq!(details.internal_id.0, hex::decode(TX_NEW_ACCOUNT).expect("hex"));
    assert_eq!(details.from, [W1]);
    assert_eq!(details.to, [receiver]);
    assert_eq!(details.total_amount, dec("2"));
    // The sender of the wallet spends the amount and the fee.
    assert_eq!(details.spent_by_me, dec("3.1"));
    assert_eq!(details.received_by_me, dec("0"));
    assert_eq!(details.my_balance_change, dec("-3.1"));
    assert_eq!(details.block_height, 72_861_998);
    assert_eq!(details.timestamp, 1_795_127_967);
    assert_eq!(details.transaction_type, TransactionType::StandardTransfer);
    assert!(details.memo.is_none());

    assert_eq!(fee_member(&details, "type"), "Tron");
    assert_eq!(fee_member(&details, "coin"), "TRX");
    assert_eq!(fee_decimal(&details, "total_fee"), dec("1.1"));
    assert_eq!(fee_decimal(&details, "bandwidth_fee"), dec("0.1"));
    assert_eq!(fee_decimal(&details, "energy_fee"), dec("0"));
    assert_eq!(fee_decimal(&details, "account_creation_fee"), dec("1"));
    // A transfer to a new account: `net_fee` is the create-account charge, so no bytes are derived from it.
    assert_eq!(fee_member(&details, "bandwidth_used"), 0);
    assert_eq!(fee_member(&details, "energy_used"), 0);
}

/// `tx_hex` is the signed transaction in its protobuf form: the raw data as the chain has it and the signature.
#[test]
fn tx_hex_of_a_trx_row_is_the_signed_protobuf_transaction() {
    let row = listed_transaction(W1_TRANSACTIONS, TX_NEW_ACCOUNT);
    let raw_hex = row["raw_data_hex"].as_str().expect("raw data");
    let signature_hex = row["signature"][0].as_str().expect("a signature");
    let details = trx_history_row("TRX", &row, &mine(&[W1]), price())
        .expect("a readable row")
        .expect("a row of the wallet");

    // Field 1 (the 133 raw bytes, length varint `85 01`) and field 2 (the 65 signature bytes, length `41`).
    let expected = hex::decode(format!("0a8501{raw_hex}1241{signature_hex}")).expect("hex");
    assert_eq!(details.tx_hex.0, expected);

    // The bytes are a transaction: the signature is carried, and the id is the hash of the raw part.
    let decoded = proto::Transaction::decode(details.tx_hex.0.as_slice()).expect("a protobuf transaction");
    assert_eq!(decoded.signature, [hex::decode(signature_hex).expect("hex")]);
    let raw = hex::decode(raw_hex).expect("hex");
    assert_eq!(hex::encode(kdf_crypto::sha256(&raw).to_vec()), TX_NEW_ACCOUNT);

    // A length below 128 takes one length byte.
    assert_eq!(
        signed_transaction_bytes("0a02abcd", &["ff".to_owned()]).expect("bytes"),
        [0x0a, 0x04, 0x0a, 0x02, 0xab, 0xcd, 0x12, 0x01, 0xff]
    );
    assert!(signed_transaction_bytes("not hex", &[]).is_err());
}

#[test]
fn a_trx_transfer_in_is_received_and_its_fee_is_not_spent_by_the_receiver() {
    let row = listed_transaction(W2_TRANSACTIONS, TX_W4);
    let details = trx_history_row("TRX", &row, &mine(&[W2]), price())
        .expect("a readable row")
        .expect("a row of the wallet");
    assert_eq!(details.from, [W1]);
    assert_eq!(details.to, [W2]);
    assert_eq!(details.total_amount, dec("1"));
    assert_eq!(details.spent_by_me, dec("0"));
    assert_eq!(details.received_by_me, dec("1"));
    assert_eq!(details.my_balance_change, dec("1"));
    assert_eq!(details.block_height, 72_595_415);
    // The fee of the transaction is shown; the sender paid it.
    assert_eq!(fee_decimal(&details, "total_fee"), dec("1.1"));
}

#[test]
fn a_trx_transfer_paid_from_the_owners_bandwidth_has_a_zero_fee() {
    let row = listed_transaction(W1_TRANSACTIONS, "f39d0290");
    let details = trx_history_row("TRX", &row, &mine(&[W1]), price())
        .expect("a readable row")
        .expect("a row of the wallet");
    assert_eq!(details.spent_by_me, dec("1"));
    assert_eq!(details.my_balance_change, dec("-1"));
    assert_eq!(fee_decimal(&details, "total_fee"), dec("0"));
    assert_eq!(fee_member(&details, "bandwidth_used"), 267);
    assert!(fee_member(&details, "account_creation_fee").is_null());
}

/// An own TRC20 `transfer` call is a fee-only row of the TRX coin: the token amount is in the token's history.
#[test]
fn an_own_token_transfer_call_is_a_fee_only_row_on_the_trx_ticker() {
    let row = listed_transaction(W1_TRANSACTIONS, TX_TOKEN_CALL);
    let details = trx_history_row("TRX", &row, &mine(&[W1]), price())
        .expect("a readable row")
        .expect("a row of the wallet");
    assert_eq!(details.transaction_type, TransactionType::FeeForTokenTx);
    assert_eq!(details.from, [W1]);
    assert_eq!(details.to, [TOKEN]);
    assert_eq!(details.total_amount, dec("0"));
    assert_eq!(details.spent_by_me, dec("0.345"));
    assert_eq!(details.received_by_me, dec("0"));
    assert_eq!(details.my_balance_change, dec("-0.345"));
    assert_eq!(details.internal_id.0, hex::decode(TX_TOKEN_CALL).expect("hex"));
    assert_eq!(fee_member(&details, "bandwidth_used"), 345);
    assert_eq!(fee_member(&details, "energy_used"), 29_650);
    // Not a TRX transfer: the fee has no creation part.
    assert!(fee_member(&details, "account_creation_fee").is_null());
}

#[test]
fn rows_that_change_no_trx_balance_of_the_wallet_are_left_out() {
    // An own call that paid no fee.
    let free = listed_transaction(W1_TRANSACTIONS, TX_FREE_TOKEN_CALL);
    assert!(trx_history_row("TRX", &free, &mine(&[W1]), price())
        .expect("a readable row")
        .is_none());
    // Another owner's asset transfer that names the wallet.
    let foreign = listed_transaction(W2_TRANSACTIONS, TX_FOREIGN_ASSET);
    assert!(trx_history_row("TRX", &foreign, &mine(&[W2]), price())
        .expect("a readable row")
        .is_none());
    // A transfer between two addresses that are not the wallet's.
    let others = listed_transaction(W1_TRANSACTIONS, TX_NEW_ACCOUNT);
    assert!(trx_history_row("TRX", &others, &mine(&[HD_0_3]), price())
        .expect("a readable row")
        .is_none());
}

#[test]
fn a_row_without_a_member_the_history_needs_is_an_error_and_never_a_made_up_value() {
    let mut row = listed_transaction(W1_TRANSACTIONS, TX_NEW_ACCOUNT);
    row.as_object_mut().expect("an object").remove("blockNumber");
    assert!(trx_history_row("TRX", &row, &mine(&[W1]), price()).is_err());
    let mut row = listed_transaction(W1_TRANSACTIONS, TX_NEW_ACCOUNT);
    row["ret"] = json!([]);
    assert!(trx_history_row("TRX", &row, &mine(&[W1]), price()).is_err());
}

#[test]
fn a_token_transfer_out_and_in_takes_its_height_and_fee_from_the_receipt() {
    let event = listed_event(W1_TRC20, TX_W5);
    let receipt = receipt_of(TX_W5);
    let id = event.internal_id(0);

    let sent = trc20_history_row(
        TOKEN_TICKER,
        "TRX",
        6,
        &event,
        id.clone(),
        &receipt,
        &mine(&[W1]),
        price(),
    )
    .expect("a row");
    assert_eq!(sent.coin, TOKEN_TICKER);
    assert_eq!(sent.tx_hash, TX_W5);
    assert_eq!(sent.from, [W1]);
    assert_eq!(sent.to, [W2]);
    assert_eq!(sent.total_amount, dec("1"));
    // The fee is paid in TRX: it is not part of what the token wallet spent.
    assert_eq!(sent.spent_by_me, dec("1"));
    assert_eq!(sent.received_by_me, dec("0"));
    assert_eq!(sent.my_balance_change, dec("-1"));
    assert_eq!(sent.block_height, 72_595_425);
    assert_eq!(sent.timestamp, 1_794_325_578);
    assert_eq!(sent.transaction_type, TransactionType::StandardTransfer);
    assert_eq!(sent.internal_id.0, id);
    assert_ne!(sent.internal_id.0, hex::decode(TX_W5).expect("hex"));
    // The listing carries no raw transaction.
    assert!(sent.tx_hex.0.is_empty());
    assert_eq!(fee_member(&sent, "coin"), "TRX");
    assert_eq!(fee_decimal(&sent, "total_fee"), dec("1.3021"));
    assert_eq!(fee_decimal(&sent, "bandwidth_fee"), dec("0.345"));
    assert_eq!(fee_decimal(&sent, "energy_fee"), dec("0.9571"));
    assert_eq!(fee_member(&sent, "bandwidth_used"), 345);
    assert_eq!(fee_member(&sent, "energy_used"), 14_650);
    assert!(fee_member(&sent, "account_creation_fee").is_null());

    let received =
        trc20_history_row(TOKEN_TICKER, "TRX", 6, &event, id, &receipt, &mine(&[W2]), price()).expect("a row");
    assert_eq!(received.spent_by_me, dec("0"));
    assert_eq!(received.received_by_me, dec("1"));
    assert_eq!(received.my_balance_change, dec("1"));
}

#[test]
fn a_token_row_is_not_built_from_a_receipt_without_a_block_number() {
    let event = listed_event(W1_TRC20, TX_W5);
    let unknown: TransactionInfoResponse = serde_json::from_str("{}").expect("the empty answer decodes");
    let built = trc20_history_row(
        TOKEN_TICKER,
        "TRX",
        6,
        &event,
        event.internal_id(0),
        &unknown,
        &mine(&[W1]),
        price(),
    );
    assert!(built.is_err());
}

#[test]
fn equal_events_of_one_transaction_get_different_row_ids() {
    let event = listed_event(W1_TRC20, TX_W5);
    assert_eq!(event.internal_id(0), event.internal_id(0));
    assert_ne!(event.internal_id(0), event.internal_id(1));
    assert_eq!(event.internal_id(0).len(), 32);
    // A row that is not a transfer event is not a history row.
    let mut approval = rows_of(W1_TRC20).remove(0);
    approval["type"] = json!("Approval");
    assert!(Trc20Event::read(&approval).expect("readable").is_none());
}

// ---------------------------------------------------------------------------------------------------------------
// The stub node and the coins of the pass-level tests
// ---------------------------------------------------------------------------------------------------------------

/// What a test changes in the stub's answers while it runs.
#[derive(Default)]
struct Script {
    /// The transaction listing of W1 has a second page (the rows of the W2 fixture).
    paged: AtomicBool,
    /// Every account listing answers HTTP 500.
    listings_fail: AtomicBool,
    /// The transactions the node does not know (it answers the empty object).
    unknown_receipts: Mutex<HashSet<String>>,
    /// Rows put in front of the token listing of W1.
    extra_token_rows: Mutex<Vec<Json>>,
}

fn without_cursor(fixture: &str) -> Json {
    let mut listing: Json = serde_json::from_str(fixture).expect("a fixture is JSON");
    if let Some(meta) = listing["meta"].as_object_mut() {
        meta.remove("fingerprint");
        meta.remove("links");
    }
    listing
}

fn empty_listing() -> Json { json!({ "data": [], "success": true, "meta": { "at": 1, "page_size": 0 } }) }

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

fn transaction_listing(script: &Script, address: &str, query: &str) -> Json {
    let paged = script.paged.load(Ordering::SeqCst);
    match (address, query_value(query, "fingerprint")) {
        (W1, None) if paged => serde_json::from_str(W1_TRANSACTIONS).expect("a fixture is JSON"),
        (W1, Some(_)) if paged => without_cursor(W2_TRANSACTIONS),
        (_, Some(_)) => empty_listing(),
        (W1, None) => without_cursor(W1_TRANSACTIONS),
        (W2, None) => without_cursor(W2_TRANSACTIONS),
        (HD_0_1, None) => without_cursor(HD_0_1_TRANSACTIONS),
        (HD_0_3, None) => without_cursor(HD_0_3_TRANSACTIONS),
        _ => empty_listing(),
    }
}

fn token_listing(script: &Script, address: &str, query: &str) -> Json {
    let mut listing = match address {
        W1 => without_cursor(W1_TRC20),
        W2 => without_cursor(W2_TRC20),
        HD_0_1 => without_cursor(HD_0_1_TRC20),
        HD_0_3 => without_cursor(HD_0_3_TRC20),
        _ => empty_listing(),
    };
    if let Some(rows) = listing["data"].as_array_mut() {
        if address == W1 {
            let extra = script
                .extra_token_rows
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            rows.splice(0..0, extra);
        }
        if let Some(contract) = query_value(query, "contract_address") {
            rows.retain(|row| row["token_info"]["address"] == contract);
        }
    }
    listing
}

/// A node that serves the captured listings and receipts. A receipt that was not captured is answered in the
/// node's form with figures of this test's own (block 72000000, 0.345 TRX burned for bandwidth).
fn history_node(script: Arc<Script>) -> StubNode {
    StubNode::start(move |path, body| {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        // A node URL with a path part: the route is what follows it.
        let route = route.strip_prefix(&format!("/{URL_SECRET}")).unwrap_or(route);
        if let Some(rest) = route.strip_prefix("/v1/accounts/") {
            if script.listings_fail.load(Ordering::SeqCst) {
                return Reply::status(500, r#"{"success":false,"error":"down"}"#);
            }
            return match rest.split_once('/') {
                Some((address, "transactions")) => Reply::ok(&transaction_listing(&script, address, query).to_string()),
                Some((address, "transactions/trc20")) => Reply::ok(&token_listing(&script, address, query).to_string()),
                _ => Reply::status(404, "{}"),
            };
        }
        match route {
            "/wallet/getnowblock" => Reply::ok(
                &json!({
                    "blockID": "ab".repeat(32),
                    "block_header": { "raw_data": { "number": HEAD, "timestamp": 1_795_130_100_000i64 } }
                })
                .to_string(),
            ),
            "/wallet/getchainparameters" => Reply::ok(
                &json!({ "chainParameter": [
                    { "key": "getTransactionFee", "value": PRICE },
                    { "key": "getEnergyFee", "value": 420 }
                ]})
                .to_string(),
            ),
            "/wallet/gettransactioninfobyid" => {
                let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
                let tx_id = request["value"].as_str().unwrap_or_default();
                let unknown = script
                    .unknown_receipts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(tx_id);
                if unknown {
                    return Reply::ok("{}");
                }
                match receipt_fixture(tx_id) {
                    Some(receipt) => Reply::ok(receipt),
                    None => Reply::ok(
                        &json!({
                            "id": tx_id,
                            "fee": 345_000,
                            "blockNumber": 72_000_000,
                            "receipt": { "net_fee": 345_000, "energy_usage_total": 14_650, "result": "SUCCESS" }
                        })
                        .to_string(),
                    ),
                }
            },
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        }
    })
}

fn priv_key() -> [u8; 32] {
    // Deterministic non-zero key (do not use in production).
    let mut key = [0u8; 32];
    key[31] = 1;
    key
}

static CONTEXTS: AtomicUsize = AtomicUsize::new(0);

/// A context with a history directory of its own and the two coins of the fixtures in its coins config.
fn history_ctx() -> MmArc {
    let root = std::env::temp_dir().join(format!(
        "kdf-tron-history-{}-{}",
        std::process::id(),
        CONTEXTS.fetch_add(1, Ordering::SeqCst)
    ));
    let conf = json!({
        "dbdir": root.to_str().expect("a UTF-8 path"),
        "coins": [
            {
                "coin": "TRX",
                "name": "tron",
                "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
                "wallet_only": true,
                "mm2": 1
            },
            {
                "coin": TOKEN_TICKER,
                "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": TOKEN } },
                "decimals": 6,
                "mm2": 1
            },
            {
                "coin": OTHER_TOKEN_TICKER,
                "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": OTHER_TOKEN } },
                "decimals": 6,
                "mm2": 1
            }
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "tron history tests").expect("a crypto context");
    std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).expect("the history directory");
    ctx
}

fn set_state(coin: &EthCoin, state: HistorySyncState) {
    *coin.history_sync_state.lock().unwrap_or_else(|e| e.into_inner()) = state;
}

fn state_json(coin: &EthCoin) -> Json { serde_json::to_value(coin.history_sync_status()).expect("a state") }

async fn register(ctx: &MmArc, coin: &EthCoin) {
    let registered = lp_register_coin(ctx, MmCoinEnum::EthCoin(coin.clone()), RegisterCoinParams {
        ticker: coin.ticker().to_owned(),
        tx_history: false,
    })
    .await;
    assert!(registered.is_ok(), "the coin registers");
}

/// A registered TRX coin over `node_url`. Its history is switched on by hand when `enabled`: the activation
/// switches it on only for a keyed TronGrid node, which a stub is not.
async fn trx_coin(ctx: &MmArc, node_url: &str, enabled: bool) -> EthCoin {
    let conf = crate::coin_conf(ctx, "TRX");
    let request = json!({ "urls": [node_url], "tx_history": true });
    let coin = tron_coin_from_conf_and_request(ctx, "TRX", &conf, &request, &priv_key(), CoinProtocol::TRX {
        network: Network::Nile,
    })
    .await
    .expect("a TRX coin activates against a stub node");
    assert!(
        matches!(coin.history_sync_status(), HistorySyncState::NotEnabled),
        "tx_history without a keyed TronGrid node enables no history"
    );
    if enabled {
        set_state(&coin, HistorySyncState::NotStarted);
    }
    register(ctx, &coin).await;
    coin
}

/// The token of the fixtures, enabled on `platform` and registered.
async fn token_on(ctx: &MmArc, platform: &EthCoin) -> EthCoin { token_of(ctx, platform, TOKEN_TICKER, TOKEN).await }

/// The 6-decimal token `ticker` at `contract`, enabled on `platform` and registered.
async fn token_of(ctx: &MmArc, platform: &EthCoin, ticker: &str, contract: &str) -> EthCoin {
    let contract = parse_trc20_contract(contract).expect("a contract");
    platform.add_erc20_token_info(ticker.to_owned(), Erc20TokenInfo {
        token_addr: contract,
        decimals: 6,
    });
    let token = platform
        .trc20_token_from_conf_or_contract(ticker.to_owned(), contract, 1, None)
        .await
        .unwrap_or_else(|e| panic!("the token builds: {}", e.get_inner()));
    register(ctx, &token).await;
    token
}

/// The client of the history: the stub counts as a keyed TronGrid node.
fn keyed_client(node: &StubNode) -> TronApiClient {
    TronApiClient::new_with_history_nodes("TRX", vec![TronNode::plain(node.url())], &[true], 5.)
}

async fn stored(ctx: &MmArc, coin: &EthCoin) -> Vec<TransactionDetails> {
    coin.load_history_from_file(ctx)
        .compat()
        .await
        .expect("the stored history")
}

fn listing_requests(node: &StubNode) -> Vec<String> {
    node.requests()
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path.contains("/v1/accounts/"))
        .collect()
}

fn receipt_requests(node: &StubNode) -> usize { node.bodies_for("/wallet/gettransactioninfobyid").len() }

fn row<'a>(rows: &'a [TransactionDetails], tx_hash: &str) -> &'a TransactionDetails {
    let mut found = rows.iter().filter(|row| row.tx_hash == tx_hash);
    let first = found.next().unwrap_or_else(|| panic!("a row of {tx_hash}"));
    assert!(
        found.next().is_none(),
        "the transaction {tx_hash} has more than one row"
    );
    first
}

// ---------------------------------------------------------------------------------------------------------------
// A pass
// ---------------------------------------------------------------------------------------------------------------

/// One address, one page: every row that changes the TRX balance is stored, newest first, and a second pass
/// stores nothing again.
#[test]
fn a_pass_stores_the_trx_rows_of_an_address_newest_first() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_ok() && matches!(report.tokens, Ok(0)));
        let rows = stored(&ctx, &coin).await;
        // 20 listed transactions; the call that paid no fee is not a row.
        assert_eq!(rows.len(), 19);
        assert_eq!(rows[0].tx_hash, TX_TOKEN_CALL);
        assert_eq!(rows[1].tx_hash, TX_NEW_ACCOUNT);
        assert_eq!(rows[2].tx_hash, TX_W1_TO_HD);
        assert!(rows.windows(2).all(|pair| pair[0].block_height >= pair[1].block_height));
        assert!(rows.iter().all(|row| row.tx_hash != TX_FREE_TOKEN_CALL));
        // No token is served: no token listing and no receipt is asked.
        assert!(listing_requests(&node).iter().all(|path| !path.contains("/trc20")));
        assert_eq!(receipt_requests(&node), 0);

        let asked = listing_requests(&node).len();
        let again = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(again.trx.is_ok());
        assert_eq!(stored(&ctx, &coin).await.len(), 19);
        assert_eq!(
            listing_requests(&node).len(),
            asked + 1,
            "one page per pass when nothing is new"
        );
    });
}

/// Paging: the cursor of a page (`meta.fingerprint`) asks the next page; a later pass stops at the first page that
/// has nothing new.
#[test]
fn a_walk_follows_the_cursor_and_a_later_walk_stops_at_a_page_without_news() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    script.paged.store(true, Ordering::SeqCst);
    let node = history_node(script);
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_ok());
        // 19 rows of the first page and three more transactions of W1 on the second.
        assert_eq!(stored(&ctx, &coin).await.len(), 22);
        let asked = listing_requests(&node);
        assert_eq!(asked.len(), 2);
        assert!(asked[0].contains("only_confirmed=true") && asked[0].contains("limit=200"));
        assert!(!asked[0].contains("fingerprint="));
        let cursor = serde_json::from_str::<Json>(W1_TRANSACTIONS).expect("JSON")["meta"]["fingerprint"]
            .as_str()
            .expect("the fixture has a cursor")
            .to_owned();
        assert!(asked[1].contains(&format!("fingerprint={cursor}")));
        assert!(asked[1].contains("only_confirmed=true"));

        // The second pass: the first page has nothing new, so the second page is not asked.
        let again = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(again.trx.is_ok());
        assert_eq!(listing_requests(&node).len(), 3);
        assert_eq!(stored(&ctx, &coin).await.len(), 22);

        // A new process (a fresh walk state) reads the whole listing again and stores nothing twice.
        let mut restarted = WalkState::default();
        let after_restart = collect_history(&coin, &api, &ctx, &mut restarted, &[evm(W1)]).await;
        assert!(after_restart.trx.is_ok());
        assert_eq!(listing_requests(&node).len(), 5);
        assert_eq!(stored(&ctx, &coin).await.len(), 22);
    });
}

/// The addresses of an HD wallet are merged into one history: a transfer between two own addresses is one row,
/// spent and received.
#[test]
fn the_rows_of_several_own_addresses_are_merged_and_a_transfer_between_them_is_one_row() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();
        let addresses = [evm(W1), evm(HD_0_1), evm(HD_0_3)];

        let report = collect_history(&coin, &api, &ctx, &mut state, &addresses).await;
        assert!(report.trx.is_ok());
        let rows = stored(&ctx, &coin).await;
        // 19 rows of W1, and from the listing of /0/1 its own token call (a fee row) and its transfer to W2.
        assert_eq!(rows.len(), 21);
        let between = row(&rows, TX_W1_TO_HD);
        assert_eq!(between.from, [W1]);
        assert_eq!(between.to, [HD_0_1]);
        assert_eq!(between.spent_by_me, dec("4.1"));
        assert_eq!(between.received_by_me, dec("3"));
        assert_eq!(between.my_balance_change, dec("-1.1"));
        let sent_by_hd = row(
            &rows,
            "4f428ffbf96c562cba5e85de1f16db72e28ec6743df92344daf3eb91e0e4cc1d",
        );
        assert_eq!(sent_by_hd.from, [HD_0_1]);
        assert_eq!(sent_by_hd.to, [W2]);
        assert_eq!(sent_by_hd.my_balance_change, dec("-1"));
    });
}

/// A platform's pass also writes the history of its enabled token: over every own address, one receipt per
/// transaction, asked once.
#[test]
fn a_token_row_gets_its_height_and_fee_from_the_receipt_once() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let token = token_on(&ctx, &platform).await;
        assert!(
            token.tron_history_enabled(),
            "a token of a platform with history has history"
        );
        let api = keyed_client(&node);
        let mut state = WalkState::default();
        let addresses = [evm(W1), evm(HD_0_1), evm(HD_0_3)];

        let report = collect_history(&platform, &api, &ctx, &mut state, &addresses).await;
        assert!(report.trx.is_ok());
        assert!(matches!(report.tokens, Ok(0)));
        let rows = stored(&ctx, &token).await;
        // Six transfers of the token at W1 (three more events there are other tokens), one more at /0/1; the
        // events /0/1 and /0/3 share with W1 are the same rows.
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|row| row.coin == TOKEN_TICKER));
        assert!(rows.iter().all(|row| row.block_height > 0));
        assert_eq!(receipt_requests(&node), 7, "one receipt per transaction");
        // The provider filters by the one served token.
        assert!(listing_requests(&node)
            .iter()
            .filter(|path| path.contains("/trc20"))
            .all(|path| path.contains(&format!("contract_address={TOKEN}"))));

        let funding = row(&rows, TX_FREE_TOKEN_CALL);
        assert_eq!(funding.from, [W1]);
        assert_eq!(funding.to, [HD_0_1]);
        assert_eq!(funding.spent_by_me, dec("1"));
        assert_eq!(funding.received_by_me, dec("1"));
        assert_eq!(funding.my_balance_change, dec("0"));
        assert_eq!(funding.block_height, 72_861_935);
        let to_token_only = row(&rows, TX_TOKEN_CALL);
        assert_eq!(to_token_only.to, [HD_0_3]);
        assert_eq!(to_token_only.block_height, 72_862_686);
        assert_eq!(fee_decimal(to_token_only, "total_fee"), dec("0.345"));

        // The TRX history of the same pass has the fee rows of the own token calls, and no token amount.
        let trx_rows = stored(&ctx, &platform).await;
        let fee_row = row(&trx_rows, TX_TOKEN_CALL);
        assert_eq!(fee_row.transaction_type, TransactionType::FeeForTokenTx);
        assert_eq!(fee_row.total_amount, dec("0"));

        // A second pass asks no receipt again and stores nothing twice.
        let again = collect_history(&platform, &api, &ctx, &mut state, &addresses).await;
        assert!(matches!(again.tokens, Ok(0)));
        assert_eq!(receipt_requests(&node), 7);
        assert_eq!(stored(&ctx, &token).await.len(), 7);
    });
}

/// A token row whose receipt the node does not have yet is not stored; it is asked again on the next pass, and the
/// token stays `InProgress` until then.
#[test]
fn a_token_row_without_a_receipt_is_left_out_and_asked_again() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    script
        .unknown_receipts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(TX_W5.to_owned());
    let node = history_node(script.clone());
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let token = token_on(&ctx, &platform).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(matches!(report.tokens, Ok(1)));
        apply_report(&platform, &ctx, &report).await;
        let rows = stored(&ctx, &token).await;
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row.tx_hash != TX_W5));
        assert_eq!(
            state_json(&token),
            json!({ "state": "InProgress", "additional_info": { "transactions_left": 1 } })
        );
        assert_eq!(state_json(&platform), json!({ "state": "Finished" }));
        let asked = receipt_requests(&node);
        assert_eq!(asked, 6);

        // The node has the transaction now: only the missing receipt is asked.
        script
            .unknown_receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(matches!(report.tokens, Ok(0)));
        apply_report(&platform, &ctx, &report).await;
        let rows = stored(&ctx, &token).await;
        assert_eq!(rows.len(), 6);
        assert_eq!(row(&rows, TX_W5).block_height, 72_595_425);
        assert_eq!(receipt_requests(&node), asked + 1);
        assert_eq!(state_json(&token), json!({ "state": "Finished" }));
    });
}

/// A transfer event of 1 unit of `token` from W1 to W2 in the transaction [`TX_TWO_TOKENS`], as the listing has it.
fn two_token_event(token: &str) -> Json {
    json!({
        "transaction_id": TX_TWO_TOKENS,
        "token_info": { "symbol": "T", "address": token, "decimals": 6, "name": "T" },
        "block_timestamp": 1_795_130_050_000u64,
        "from": W1,
        "to": W2,
        "type": "Transfer",
        "value": "1000000"
    })
}

/// The id of a token row does not depend on the shape of the listing: a transaction that moves the same value
/// between the same addresses in two tokens is stored once per token, whether the listing was filtered by one
/// token's contract (one token served) or lists every token (two served), and whatever the order of its events.
#[test]
fn a_two_token_transaction_has_the_same_row_ids_in_a_filtered_and_an_unfiltered_listing() {
    // The ids themselves: the token is part of them, and the position is counted per token.
    let first = Trc20Event::read(&two_token_event(TOKEN))
        .expect("readable")
        .expect("a transfer");
    let second = Trc20Event::read(&two_token_event(OTHER_TOKEN))
        .expect("readable")
        .expect("a transfer");
    assert_ne!(first.internal_id(0), second.internal_id(0));

    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    // The other token's event comes first in the unfiltered listing.
    *script.extra_token_rows.lock().unwrap_or_else(|e| e.into_inner()) =
        vec![two_token_event(OTHER_TOKEN), two_token_event(TOKEN)];
    let node = history_node(script);
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let token = token_on(&ctx, &platform).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        // One token served: the provider filters by its contract.
        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(matches!(report.tokens, Ok(0)));
        let rows = stored(&ctx, &token).await;
        assert_eq!(rows.len(), 7);
        let filtered_id = row(&rows, TX_TWO_TOKENS).internal_id.clone();
        assert_eq!(filtered_id.0, first.internal_id(0));

        // Two tokens served: the listing is not filtered any more and is read again as a whole.
        let other = token_of(&ctx, &platform, OTHER_TOKEN_TICKER, OTHER_TOKEN).await;
        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(matches!(report.tokens, Ok(0)));
        assert!(listing_requests(&node)
            .last()
            .is_some_and(|path| path.contains("/trc20") && !path.contains("contract_address=")));
        let rows = stored(&ctx, &token).await;
        assert_eq!(rows.len(), 7, "the row of the first token is not stored a second time");
        assert_eq!(row(&rows, TX_TWO_TOKENS).internal_id, filtered_id);
        let other_rows = stored(&ctx, &other).await;
        assert_eq!(other_rows.len(), 1);
        assert_eq!(row(&other_rows, TX_TWO_TOKENS).internal_id.0, second.internal_id(0));
        assert_eq!(other_rows[0].coin, OTHER_TOKEN_TICKER);
    });
}

/// A row built before its receiver was a known address of the wallet is a plain send; once the address is known
/// (a new HD address), the stored row reads as a transfer between own addresses, and stays one row. The same
/// holds after a restart, when the rows on disk are older than the addresses known now.
#[test]
fn a_stored_send_becomes_an_own_transfer_when_its_receiver_becomes_known() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let token = token_on(&ctx, &platform).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_ok() && matches!(report.tokens, Ok(0)));
        let rows = stored(&ctx, &platform).await;
        assert_eq!(rows.len(), 19);
        let send = row(&rows, TX_W1_TO_HD);
        assert_eq!(send.spent_by_me, dec("4.1"));
        assert_eq!(send.received_by_me, dec("0"));
        assert_eq!(send.my_balance_change, dec("-4.1"));
        let fee_row_before = row(&rows, TX_TOKEN_CALL).clone();
        let token_rows = stored(&ctx, &token).await;
        let token_send = row(&token_rows, TX_FREE_TOKEN_CALL);
        assert_eq!(token_send.received_by_me, dec("0"));
        assert_eq!(token_send.my_balance_change, dec("-1"));

        // The wallet now knows the receiving address too.
        let known = [evm(W1), evm(HD_0_1)];
        let report = collect_history(&platform, &api, &ctx, &mut state, &known).await;
        assert!(report.trx.is_ok() && matches!(report.tokens, Ok(0)));
        let rows = stored(&ctx, &platform).await;
        // The two rows the listing of the new address adds; the transfer between the two is still one row.
        assert_eq!(rows.len(), 21);
        let transfer = row(&rows, TX_W1_TO_HD);
        assert_eq!(transfer.spent_by_me, dec("4.1"));
        assert_eq!(transfer.received_by_me, dec("3"));
        assert_eq!(transfer.my_balance_change, dec("-1.1"));
        // A fee-only row is left as it is.
        assert_eq!(row(&rows, TX_TOKEN_CALL), &fee_row_before);
        let token_rows = stored(&ctx, &token).await;
        let token_transfer = row(&token_rows, TX_FREE_TOKEN_CALL);
        assert_eq!(token_transfer.spent_by_me, dec("1"));
        assert_eq!(token_transfer.received_by_me, dec("1"));
        assert_eq!(token_transfer.my_balance_change, dec("0"));
    });

    // A restart: rows stored for one address, read by a process that knows two.
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let report = collect_history(&platform, &api, &ctx, &mut WalkState::default(), &[evm(W1)]).await;
        assert!(report.trx.is_ok());
        let known = [evm(W1), evm(HD_0_1)];
        let report = collect_history(&platform, &api, &ctx, &mut WalkState::default(), &known).await;
        assert!(report.trx.is_ok());
        let rows = stored(&ctx, &platform).await;
        assert_eq!(rows.len(), 21);
        assert_eq!(row(&rows, TX_W1_TO_HD).received_by_me, dec("3"));
        assert_eq!(row(&rows, TX_W1_TO_HD).my_balance_change, dec("-1.1"));
    });
}

// ---------------------------------------------------------------------------------------------------------------
// The history states
// ---------------------------------------------------------------------------------------------------------------

/// A pass that fails before the history has finished once shows `Error` with a text that carries no part of a
/// node URL beyond its host and no header value.
#[test]
fn a_pass_that_fails_before_the_first_finish_is_an_error_state_without_credentials() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    script.listings_fail.store(true, Ordering::SeqCst);
    let node = history_node(script);
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let mut headers = http::HeaderMap::new();
        headers.insert("tron-pro-api-key", http::HeaderValue::from_static(HEADER_SECRET));
        let secret_url = format!("{}/{URL_SECRET}?token=QUERYSECRET", node.url());
        let api =
            TronApiClient::new_with_history_nodes("TRX", vec![TronNode::new(secret_url, headers, None)], &[true], 5.);
        let mut state = WalkState::default();

        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_err());
        apply_report(&coin, &ctx, &report).await;
        let shown = state_json(&coin);
        assert_eq!(shown["state"], "Error");
        let text = shown.to_string();
        assert!(text.contains("HTTP status 500"), "the failure is named");
        for secret in [URL_SECRET, "QUERYSECRET", HEADER_SECRET] {
            assert!(!text.contains(secret), "a credential part is rendered");
        }
        let failure = match &report.trx {
            Err(e) => e.to_string(),
            Ok(()) => String::new(),
        };
        for secret in [URL_SECRET, "QUERYSECRET", HEADER_SECRET] {
            assert!(!failure.contains(secret), "a credential part is rendered");
        }
        // The key did reach the node it belongs to.
        assert!(node
            .header_values("tron-pro-api-key")
            .iter()
            .any(|value| value.as_deref() == Some(HEADER_SECRET)));
        assert!(stored(&ctx, &coin).await.is_empty());
    });
}

/// A pass that fails after the history has finished once leaves the state and the stored rows as they are.
#[test]
fn a_pass_that_fails_after_a_finish_keeps_the_finished_state_and_the_rows() {
    let ctx = history_ctx();
    let script = Arc::new(Script::default());
    let node = history_node(script.clone());
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();

        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        apply_report(&coin, &ctx, &report).await;
        assert_eq!(state_json(&coin), json!({ "state": "Finished" }));

        script.listings_fail.store(true, Ordering::SeqCst);
        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_err());
        apply_report(&coin, &ctx, &report).await;
        assert_eq!(state_json(&coin), json!({ "state": "Finished" }));
        assert_eq!(stored(&ctx, &coin).await.len(), 19);
    });
}

/// Without the bandwidth price no row is built: the pass fails like any node failure, and no price is made up.
#[test]
fn a_pass_without_the_bandwidth_price_fails_and_stores_nothing() {
    let ctx = history_ctx();
    let listings = history_node(Arc::default());
    // A node whose chain parameters lack the price: the node client refuses the answer.
    let node = StubNode::start(|path, _| match path {
        "/wallet/getchainparameters" => Reply::ok(r#"{"chainParameter":[{"key":"getEnergyFee","value":420}]}"#),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    });
    block_on(async {
        let coin = trx_coin(&ctx, &listings.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();
        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        assert!(report.trx.is_err() && report.tokens.is_err());
        assert!(stored(&ctx, &coin).await.is_empty());
        assert!(
            listing_requests(&node).is_empty(),
            "no listing is asked without the price"
        );
    });
}

/// The process does nothing for a coin whose history is not enabled, and nothing for an enabled coin whose client
/// has no keyed TronGrid node: it ends at once and asks no node.
#[test]
fn the_process_ends_at_once_without_an_enabled_history_or_without_a_keyed_node() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), false).await;
        assert!(!coin.tron_history_enabled());
        let before = node.requests().len();
        tron_history_loop(coin.clone(), ctx.clone()).await;
        assert_eq!(node.requests().len(), before);
        assert!(matches!(coin.history_sync_status(), HistorySyncState::NotEnabled));

        // Switched on by hand, but the coin's own client was built over a stub: no keyed TronGrid node.
        set_state(&coin, HistorySyncState::NotStarted);
        tron_history_loop(coin.clone(), ctx.clone()).await;
        assert_eq!(node.requests().len(), before);
    });
}

// ---------------------------------------------------------------------------------------------------------------
// The answers of the two history methods
// ---------------------------------------------------------------------------------------------------------------

async fn legacy_history(ctx: &MmArc, request: Json) -> Json {
    let response = crate::my_tx_history(ctx.clone(), request)
        .await
        .expect("the legacy method answers");
    serde_json::from_slice::<Json>(response.body()).expect("a JSON body")["result"].clone()
}

async fn versioned_history(ctx: &MmArc, request: Json) -> Result<Json, MyTxHistoryErrorV2> {
    let request: MyTxHistoryRequestV2 = serde_json::from_value(request).expect("a request");
    my_tx_history_v2_rpc(ctx.clone(), request)
        .await
        .map(|response| serde_json::to_value(response).expect("a serializable answer"))
        .map_err(|e| e.into_inner())
}

/// With an enabled history both methods answer the stored rows, with the head block of the TRON node as
/// `current_block` and the confirmations counted from it.
#[test]
fn both_methods_answer_the_stored_rows_with_the_nodes_head_block() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let coin = trx_coin(&ctx, &node.url(), true).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();
        let report = collect_history(&coin, &api, &ctx, &mut state, &[evm(W1)]).await;
        apply_report(&coin, &ctx, &report).await;

        let legacy = legacy_history(&ctx, json!({ "coin": "TRX", "limit": 2 })).await;
        assert_eq!(legacy["total"], 19);
        assert_eq!(legacy["current_block"], HEAD);
        assert_eq!(legacy["sync_status"], json!({ "state": "Finished" }));
        let transactions = legacy["transactions"].as_array().expect("transactions");
        assert_eq!(transactions.len(), 2);
        assert_eq!(transactions[0]["tx_hash"], TX_TOKEN_CALL);
        assert_eq!(transactions[0]["block_height"], 72_862_686u64);
        // HEAD - height + 1.
        assert_eq!(transactions[0]["confirmations"], 5);
        assert_eq!(transactions[0]["from"], json!([W1]));
        assert_eq!(transactions[0]["transaction_type"], "FeeForTokenTx");
        assert_eq!(transactions[1]["tx_hash"], TX_NEW_ACCOUNT);
        assert_eq!(transactions[1]["fee_details"]["type"], "Tron");

        let versioned = versioned_history(&ctx, json!({ "coin": "TRX", "limit": 2 }))
            .await
            .expect("the versioned method serves a TRON coin with history");
        assert_eq!(versioned["coin"], "TRX");
        assert_eq!(versioned["total"], 19);
        assert_eq!(versioned["total_pages"], 10);
        assert_eq!(versioned["current_block"], HEAD);
        assert_eq!(versioned["sync_status"], json!({ "state": "Finished" }));
        let transactions = versioned["transactions"].as_array().expect("transactions");
        assert_eq!(transactions.len(), 2);
        assert_eq!(transactions[0]["tx_hash"], TX_TOKEN_CALL);
        assert_eq!(transactions[0]["confirmations"], 5);
        assert_eq!(transactions[1]["tx_hash"], TX_NEW_ACCOUNT);
    });
}

/// Both methods also serve a token whose history its platform writes.
#[test]
fn both_methods_answer_the_rows_of_a_token() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let platform = trx_coin(&ctx, &node.url(), true).await;
        let _token = token_on(&ctx, &platform).await;
        let api = keyed_client(&node);
        let mut state = WalkState::default();
        let report = collect_history(&platform, &api, &ctx, &mut state, &[evm(W1)]).await;
        apply_report(&platform, &ctx, &report).await;

        let legacy = legacy_history(&ctx, json!({ "coin": TOKEN_TICKER, "limit": 10 })).await;
        assert_eq!(legacy["total"], 6);
        assert_eq!(legacy["sync_status"], json!({ "state": "Finished" }));
        assert_eq!(legacy["transactions"][0]["coin"], TOKEN_TICKER);
        let versioned = versioned_history(&ctx, json!({ "coin": TOKEN_TICKER }))
            .await
            .expect("the versioned method serves a token with history");
        assert_eq!(versioned["total"], 6);
        assert_eq!(versioned["transactions"][0]["tx_hash"], TX_TOKEN_CALL);
    });
}

/// T21.10B.10, kept for a coin without a key: the versioned method answers `NotSupportedFor` (HTTP 400) and the
/// legacy method an empty list with `current_block` and `NotEnabled` (CRD ch.21 R21.10B.6).
#[test]
fn t21_10b_10_without_a_key_the_reference_answers_stay() {
    let ctx = history_ctx();
    let node = history_node(Arc::default());
    block_on(async {
        let _coin = trx_coin(&ctx, &node.url(), false).await;

        let error = versioned_history(&ctx, json!({ "coin": "TRX" }))
            .await
            .expect_err("no history without a key");
        assert!(matches!(&error, MyTxHistoryErrorV2::NotSupportedFor(ticker) if ticker == "TRX"));
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);

        let legacy = legacy_history(&ctx, json!({ "coin": "TRX" })).await;
        assert_eq!(legacy["transactions"], json!([]));
        assert_eq!(legacy["total"], 0);
        assert_eq!(legacy["current_block"], HEAD);
        assert_eq!(legacy["sync_status"], json!({ "state": "NotEnabled" }));
        assert!(listing_requests(&node).is_empty());
    });
}
