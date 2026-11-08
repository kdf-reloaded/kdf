//! The history row of an own send reports its fee (CRD ch.39 R39.8.9e1, T39.8.9e1).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport. A send is made by `send_outputs` or by `withdraw`
//! followed by `send_raw_tx`, and is mined by feeding the scanner a compact block made of the
//! transaction; a "restart" is a new coin over the same database directory. The tests make real Sapling
//! spend proofs (the Pirate1 one also a real Halo2 proof), so they need the Sapling parameter files and
//! say so where those are absent.

use super::*;
use crate::z_coin::z_coin_wallet_db::ZCoinTxHistoryDetails;
use crate::z_coin::z_pirate1_tests::MAIN_ADDRESS;
use crate::z_coin::z_tx_read::read_tx;
use crate::{TransactionDetails, TxFeeDetails, WithdrawRequest};

const IRONWOOD_HEIGHT: u32 = 5;
/// The Pirate1 fee: fixed whatever the coin's configured fee is.
const IRONWOOD_FEE_SAT: u64 = 10_000;

fn request(to: &str) -> WithdrawRequest {
    serde_json::from_value(json!({ "coin": "ARRR", "to": to, "amount": "0.01", "max": false })).unwrap()
}

fn row_of<'a>(page: &'a ZCoinTxHistoryPage, tx: &ZTransaction) -> &'a ZCoinTxHistoryDetails {
    let hash = tx.txid().to_string();
    page.transactions
        .iter()
        .find(|row| row.tx_hash == hash)
        .expect("the row of the transaction is served")
}

/// The fee a transaction of this wallet pays, read from its bytes: the value that leaves the Sapling pool
/// less what its transparent outputs take. The transaction must spend Sapling notes.
pub(super) fn paid_fee(tx: &ZTransaction) -> u64 {
    let leaving = i64::from(*tx.sapling_bundle().expect("a Sapling transaction").value_balance());
    let transparent: u64 = tx
        .transparent_bundle()
        .map_or(0, |bundle| bundle.vout.iter().map(|out| u64::from(out.value())).sum());
    u64::try_from(leaving - i64::try_from(transparent).unwrap()).expect("a fee is not negative")
}

/// The row of every other transaction of the page: the receipt of the funding note, a deposit from
/// another wallet.
fn deposits<'a>(page: &'a ZCoinTxHistoryPage, tx: &ZTransaction) -> Vec<&'a ZCoinTxHistoryDetails> {
    let hash = tx.txid().to_string();
    page.transactions.iter().filter(|row| row.tx_hash != hash).collect()
}

/// The fee of the row of `tx` and of every other row, as the first history page serves them.
fn fees(fixture: &LightCoinFixture, tx: &ZTransaction) -> (BigDecimal, Vec<BigDecimal>) {
    let page = fixture.history_page(true);
    let own = row_of(&page, tx).transaction_fee.clone();
    let others = deposits(&page, tx)
        .into_iter()
        .map(|row| row.transaction_fee.clone())
        .collect();
    (own, others)
}

/// The row of `tx` reports `fee`, and the deposit row beside it reports `0`.
fn assert_fee(fixture: &LightCoinFixture, tx: &ZTransaction, fee: u64, state: &str) {
    let (own, others) = fees(fixture, tx);
    assert_eq!(own, sat(fee), "{state}");
    assert_eq!(
        others,
        vec![BigDecimal::from(0)],
        "{state}: the deposit of the funding note"
    );
}

/// T39.8.9e1 (a) over `send_outputs`, the path of a swap payment and a swap fee payment: the fee of the
/// send is reported by the unmined row, by the mined row, and after a restart of each. The receipt of
/// the funding note, a deposit from another wallet, reports `0` throughout (R39.8.9e1 (b)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9e1_an_own_send_reports_its_fee_unmined_mined_and_after_a_restart() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();

    let page = fixture.history_page(true);
    assert_eq!(page.total, 2, "the receipt of the funding note and the send");
    assert_eq!(row_of(&page, &tx).block_height, 0, "unmined");
    assert_fee(&fixture, &tx, FEE_SAT, "unmined");

    // After a restart, still unmined.
    let mut fixture = fixture.reopen();
    assert_fee(&fixture, &tx, FEE_SAT, "unmined, after a restart");

    // The scan that mines the transaction does not change the fee.
    fixture.scan_block_containing(&tx, 12);
    let page = fixture.history_page(true);
    assert_eq!(page.total, 2, "no second row appears for the mined transaction");
    assert_eq!(row_of(&page, &tx).block_height, 12, "mined");
    assert_fee(&fixture, &tx, FEE_SAT, "mined");

    // After a restart, mined.
    let fixture = fixture.reopen();
    assert_fee(&fixture, &tx, FEE_SAT, "mined, after a restart");
}

/// T39.8.9e1 (a) for a withdrawal submitted through `send_raw_transaction` (R39.6.5g): the fee the
/// response names is the fee the row reports, unmined, after a restart, mined and after a restart. A
/// withdrawal that is built and not submitted has no row.
async fn withdrawal_fee_lifecycle(fixture: LightCoinFixture, to: &str, fee: u64) {
    let details: TransactionDetails = fixture.coin.withdraw(request(to)).compat().await.unwrap();
    match details.fee_details.as_ref().expect("a fee") {
        TxFeeDetails::Utxo(fee_details) => assert_eq!(fee_details.amount, sat(fee)),
        other => panic!("unexpected fee details {:?}", other),
    }
    let tx = read_tx(&details.tx_hex.0).unwrap();

    let before = fixture.history_page(true);
    assert_eq!(before.total, 1, "the receipt of the funding note only");

    fixture
        .coin
        .send_raw_tx(&hex::encode(&details.tx_hex.0))
        .compat()
        .await
        .unwrap();

    let page = fixture.history_page(true);
    assert_eq!(page.total, 2, "the funding note and the withdrawal");
    assert_eq!(row_of(&page, &tx).block_height, 0, "unmined");
    assert_fee(&fixture, &tx, fee, "unmined");

    let mut fixture = fixture.reopen();
    assert_fee(&fixture, &tx, fee, "unmined, after a restart");

    fixture.scan_block_containing(&tx, 12);
    let page = fixture.history_page(true);
    assert_eq!(page.total, 2, "no second row appears for the mined transaction");
    assert_eq!(row_of(&page, &tx).block_height, 12, "mined");
    assert_fee(&fixture, &tx, fee, "mined");

    let fixture = fixture.reopen();
    assert_fee(&fixture, &tx, fee, "mined, after a restart");
}

/// T39.8.9e1 for a withdrawal to a Sapling address: the coin's configured fee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9e1_a_sapling_withdrawal_reports_its_fee_in_every_state() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let to = encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &ExtendedSpendingKey::master(&[97; 32]).default_address().1,
    );
    let fee = fixture.fee_sat;
    withdrawal_fee_lifecycle(fixture, &to, fee).await;
}

/// T39.8.9e1 for a withdrawal to a Pirate1 address: the fixed fee of 10,000, not the configured one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9e1_a_pirate1_withdrawal_reports_its_fee_in_every_state() {
    let mut params = test_params_with_zip212();
    params.ironwood_activation_height = Some(IRONWOOD_HEIGHT);
    let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, params, 1_000) else {
        return;
    };
    withdrawal_fee_lifecycle(fixture, MAIN_ADDRESS, IRONWOOD_FEE_SAT).await;
}

/// T39.8.9e1 (c): after a rebuild of the wallet database the own send, found again by scanning, reports
/// `0`: the rebuilt database holds no broadcast record (R39.8.9e1 (d): nothing is fetched). The
/// rebuild is the one of a chain divergence; the rescan reads the funding block and the block that
/// mines the send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9e1_a_rescanned_own_send_reports_zero_after_a_wallet_database_rebuild() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    fixture.scan_block_containing(&tx, 12);
    assert_fee(&fixture, &tx, FEE_SAT, "before the rebuild");

    // The rebuild: a fresh database anchored where the first one was, an empty compact-block cache.
    fixture
        .history()
        .rebuild_wallet_at_empty_anchor_for_test(WALLET_BIRTHDAY, [9u8; 32])
        .unwrap();
    assert_eq!(fixture.history().balance(fixture.params.clone()).unwrap(), 0);

    // The rescan: the block that paid the wallet its note, then the block that mines the send.
    #[allow(deprecated)]
    let extfvk = fixture.extsk.to_extended_full_viewing_key();
    fixture
        .history()
        .insert_compact_blocks_for_test(&[compact_block_with_received_notes(
            u64::from(NOTES_HEIGHT),
            NOTES_HEIGHT as u8,
            9,
            &extfvk,
            &[(NOTE_VALUE, 68, 42)],
        )])
        .unwrap();
    assert_eq!(
        fixture
            .history()
            .scan_cached_blocks_to_height(fixture.params.clone(), u64::from(NOTES_HEIGHT), 1_000, 0, |_, _| {})
            .unwrap(),
        u64::from(NOTES_HEIGHT)
    );
    fixture.scan_block_containing(&tx, 12);

    let page = fixture.history_page(true);
    assert_eq!(page.total, 2, "the receipt of the funding note and the send");
    let row = row_of(&page, &tx);
    assert_eq!(row.block_height, 12);
    assert_eq!(row.spent_by_me, sat(NOTE_VALUE), "the row is the own send");
    assert_eq!(row.transaction_fee, BigDecimal::from(0), "no fee is held for it");
    assert_eq!(
        deposits(&page, &tx)[0].transaction_fee,
        BigDecimal::from(0),
        "the deposit"
    );
}
