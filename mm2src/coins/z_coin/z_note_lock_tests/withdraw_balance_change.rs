//! The shielded withdraw response reports `my_balance_change` as received minus spent, the orientation
//! of the history row (CRD ch.39 R39.5A.3, R39.6.5c, T39.5A.c).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport. A withdrawal is built by `withdraw`, submitted
//! through `send_raw_tx` and mined by feeding the scanner a compact block made of the transaction. The
//! tests make real Sapling spend proofs (the Pirate1 one also a real Halo2 proof), so they need the
//! Sapling parameter files and say so where those are absent.

use super::*;
use crate::z_coin::z_coin_wallet_db::ZCoinTxHistoryDetails;
use crate::z_coin::z_pirate1_tests::MAIN_ADDRESS;
use crate::z_coin::z_tx_read::read_tx;
use crate::{TransactionDetails, TxFeeDetails, WithdrawRequest};

const IRONWOOD_HEIGHT: u32 = 5;
/// The Pirate1 fee: fixed whatever the coin's configured fee is.
const IRONWOOD_FEE_SAT: u64 = 10_000;
/// The value sent, `0.01` in the request.
const VALUE_SAT: u64 = 1_000_000;

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

/// T39.5A.c: a withdrawal of `VALUE_SAT` with fee `fee` and change from one note of `NOTE_VALUE`. The
/// response carries every member the test names, and the row of the same transaction in the history
/// carries the same `my_balance_change`, unmined and mined.
async fn assert_response_and_row(mut fixture: LightCoinFixture, to: &str, fee: u64) {
    let change = NOTE_VALUE - VALUE_SAT - fee;

    let details: TransactionDetails = fixture.coin.withdraw(request(to)).compat().await.unwrap();
    assert_eq!(details.from, vec![fixture.coin.z_fields.my_z_addr_encoded.clone()]);
    assert_eq!(details.to, vec![to.to_owned()]);
    assert_eq!(details.spent_by_me, sat(NOTE_VALUE));
    assert_eq!(details.received_by_me, sat(change));
    assert_eq!(details.my_balance_change, -sat(VALUE_SAT + fee));
    assert_eq!(
        details.my_balance_change,
        details.received_by_me.clone() - details.spent_by_me.clone(),
        "received minus spent"
    );
    assert!(
        details.my_balance_change < BigDecimal::from(0),
        "negative for a withdrawal"
    );
    assert_eq!(details.total_amount, sat(NOTE_VALUE));
    match details.fee_details.as_ref().expect("a fee") {
        TxFeeDetails::Utxo(fee_details) => {
            assert_eq!(fee_details.coin, Some(fixture.coin.utxo_arc.conf.ticker.clone()));
            assert_eq!(fee_details.amount, sat(fee));
        },
        other => panic!("unexpected fee details {:?}", other),
    }
    assert_eq!((details.block_height, details.timestamp), (0, 0));
    assert!(details.kmd_rewards.is_none());

    // The row of the same transaction carries the same figures, before and after the block mines it.
    let tx = read_tx(&details.tx_hex.0).unwrap();
    fixture
        .coin
        .send_raw_tx(&hex::encode(&details.tx_hex.0))
        .compat()
        .await
        .unwrap();

    let unmined_page = fixture.history_page(true);
    let unmined = row_of(&unmined_page, &tx);
    assert_eq!(unmined.block_height, 0);
    assert_eq!(unmined.my_balance_change, details.my_balance_change);
    assert_eq!(unmined.spent_by_me, details.spent_by_me);
    assert_eq!(unmined.received_by_me, details.received_by_me);

    fixture.scan_block_containing(&tx, 12);
    let mined_page = fixture.history_page(true);
    let mined = row_of(&mined_page, &tx);
    assert_eq!(mined.block_height, 12);
    assert_eq!(mined.my_balance_change, details.my_balance_change);
    assert_eq!(mined.spent_by_me, details.spent_by_me);
    assert_eq!(mined.received_by_me, details.received_by_me);
}

/// T39.5A.c for a Sapling destination: a withdrawal with change answers a negative `my_balance_change`,
/// `-(V + F)`, the one its history row carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_5a_c_a_sapling_withdrawal_reports_received_minus_spent() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let to = encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &ExtendedSpendingKey::master(&[97; 32]).default_address().1,
    );
    let fee = fixture.fee_sat;
    assert_response_and_row(fixture, &to, fee).await;
}

/// T39.5A.c, R39.6.5c for a Pirate1 destination: the same orientation, with the fixed fee of 10,000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_5a_c_a_pirate1_withdrawal_reports_received_minus_spent() {
    let mut params = test_params_with_zip212();
    params.ironwood_activation_height = Some(IRONWOOD_HEIGHT);
    let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, params, 1_000) else {
        return;
    };
    assert_response_and_row(fixture, MAIN_ADDRESS, IRONWOOD_FEE_SAT).await;
}
