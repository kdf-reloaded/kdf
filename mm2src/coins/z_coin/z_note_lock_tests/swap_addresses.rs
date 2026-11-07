//! The history rows of the transactions the node builds for a swap list their addresses (CRD ch.39
//! R39.8.9d3, T39.8.9d3).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport. The transactions are the ones the swap code builds:
//! `z_send_htlc` (the payment), `z_send_dex_fee` (the fee payment) and `z_p2sh_spend` (the spend of the
//! counterparty's payment and the refund of the node's own). A transaction is mined by feeding the scanner
//! a compact block made of it; a "restart" is a new coin over the same database directory. The tests make
//! real Sapling proofs, so they need the Sapling parameter files and say so where those are absent.

use super::withdraw_recipient::{own_address, page, row_of, sorted};
use super::*;
use crate::utxo::utxo_common::payment_script;
use crate::z_coin::z_htlc::{dex_fee_address_string, p2sh_address, z_p2sh_spend, z_send_dex_fee, z_send_htlc};
use chain::constants::SEQUENCE_FINAL;
use crypto::privkey::key_pair_from_secret;
use kdf_crypto::dhash160;
use keys::Address;
use script::{Builder as ScriptBuilder, Opcode};
use std::str::FromStr;

const TIME_LOCK: u32 = 1_800_000_000;
const SECRET: [u8; 32] = [7; 32];

/// The two parties of the fixture swap: the node's own key and the counterparty's.
fn parties() -> (KeyPair, KeyPair) {
    (
        key_pair_from_secret(&[21; 32]).unwrap(),
        key_pair_from_secret(&[22; 32]).unwrap(),
    )
}

/// The redeem script of the node's own payment: the node sends, the counterparty may redeem with the
/// secret, the node may refund after the lock time.
fn redeem_script(mine: &KeyPair, other: &KeyPair) -> script::Script {
    payment_script(TIME_LOCK, dhash160(&SECRET).as_slice(), mine.public(), other.public())
}

async fn send_payment(fixture: &LightCoinFixture, mine: &KeyPair, other: &KeyPair) -> ZTransaction {
    z_send_htlc(
        &fixture.coin,
        TIME_LOCK,
        mine.public(),
        other.public(),
        dhash160(&SECRET).as_slice(),
        "0.001".parse().unwrap(),
    )
    .await
    .unwrap()
}

/// The string `address` is the transparent pay-to-script-hash address of `redeem`: it parses as such, with
/// the coin's pay-to-script-hash prefix and the hash of the script.
fn assert_is_script_address(fixture: &LightCoinFixture, address: &str, redeem: &script::Script) {
    let parsed = Address::from_str(address).expect("a transparent address");
    assert_eq!(parsed.prefix, fixture.coin.utxo_arc.conf.p2sh_addr_prefix);
    assert_eq!(parsed.hash.to_vec(), dhash160(redeem).as_slice().to_vec());
    assert_eq!(p2sh_address(&fixture.coin, redeem).to_string(), address);
}

/// The transaction has a row in the page.
fn has_row(page: &ZCoinTxHistoryPage, tx: &ZTransaction) -> bool {
    let hash = tx.txid().to_string();
    page.transactions.iter().any(|row| row.tx_hash == hash)
}

/// T39.8.9d3 (a): the row of a payment to the swap's script lists the script's address in `to`, beside the
/// wallet's own address for the change, and nothing for the data-carrier output that publishes the script;
/// unmined, mined and after each restart. The sender is the wallet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d3_a_payment_lists_the_script_address_and_nothing_for_the_data_carrier() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let (mine, other) = parties();
    let redeem = redeem_script(&mine, &other);
    let tx = send_payment(&fixture, &mine, &other).await;
    assert_eq!(
        tx.transparent_bundle().unwrap().vout.len(),
        2,
        "the script output and the data-carrier output"
    );

    let own = own_address(&fixture);
    let script_address = p2sh_address(&fixture.coin, &redeem).to_string();
    assert_is_script_address(&fixture, &script_address, &redeem);
    let expected_to = sorted(vec![script_address, own.clone()]);

    let check = |fixture: &LightCoinFixture, mined: bool| {
        let page = page(fixture);
        assert_eq!(page.total, 2, "the funding note and the payment, mined: {mined}");
        let row = row_of(&page, &tx);
        assert_eq!(row.to, expected_to, "mined: {mined}");
        assert_eq!(row.from, vec![own.clone()], "mined: {mined}");
        assert_eq!(row.block_height != 0, mined);
    };

    // The unmined row, and after a restart.
    check(&fixture, false);
    let mut fixture = fixture.reopen();
    check(&fixture, false);

    // The mined row, and after a restart.
    fixture.scan_block_containing(&tx, 12);
    check(&fixture, true);
    let fixture = fixture.reopen();
    check(&fixture, true);
}

/// T39.8.9d3 (b): the row of a swap fee payment lists the fee address in `to`, as a Sapling address with
/// the coin's prefix, beside the wallet's own address for the change; unmined, mined and after each
/// restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d3_a_fee_payment_lists_the_fee_address_in_the_sapling_form() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = z_send_dex_fee(&fixture.coin, "0.01".parse().unwrap(), &b"swap-uuid"[..])
        .await
        .unwrap();

    let own = own_address(&fixture);
    let hrp = fixture.params.hrp_sapling_payment_address().to_owned();
    let fee_address = encode_payment_address(&hrp, &fixture.coin.z_fields.dex_fee_addr);
    assert_eq!(fee_address, dex_fee_address_string(&fixture.coin));
    assert!(fee_address.starts_with(&hrp), "the coin's prefix: {fee_address}");
    assert_ne!(fee_address, own, "the fee address is not the wallet's own");
    let expected_to = sorted(vec![fee_address, own.clone()]);

    let check = |fixture: &LightCoinFixture, mined: bool| {
        let page = page(fixture);
        assert_eq!(page.total, 2, "the funding note and the fee payment, mined: {mined}");
        let row = row_of(&page, &tx);
        assert_eq!(row.to, expected_to, "mined: {mined}");
        assert_eq!(row.from, vec![own.clone()], "mined: {mined}");
        assert_eq!(row.block_height != 0, mined);
    };

    check(&fixture, false);
    let mut fixture = fixture.reopen();
    check(&fixture, false);

    fixture.scan_block_containing(&tx, 12);
    check(&fixture, true);
    let fixture = fixture.reopen();
    check(&fixture, true);
}

/// T39.8.9d3 (c): the rows of a spend of the counterparty's payment and of a refund of the node's own list
/// the script's address in `from` and the wallet's own address in `to`. Neither spends a wallet note, so
/// neither has an unmined row (R39.8.9i): each shows up when it is mined, and the same after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d3_a_spend_and_a_refund_list_the_script_address_as_the_sender() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let (mine, other) = parties();
    let payment = send_payment(&fixture, &mine, &other).await;
    let script_address = p2sh_address(&fixture.coin, &redeem_script(&mine, &other)).to_string();
    assert_is_script_address(&fixture, &script_address, &redeem_script(&mine, &other));

    // The counterparty redeems with the secret; the node refunds after the lock time.
    let spend = z_p2sh_spend(
        &fixture.coin,
        payment.clone(),
        TIME_LOCK,
        SEQUENCE_FINAL,
        redeem_script(&mine, &other),
        ScriptBuilder::default()
            .push_data(&SECRET)
            .push_opcode(Opcode::OP_0)
            .into_script(),
        other.private().secret.as_slice(),
    )
    .await
    .unwrap();
    let refund = z_p2sh_spend(
        &fixture.coin,
        payment,
        TIME_LOCK,
        SEQUENCE_FINAL - 1,
        redeem_script(&mine, &other),
        ScriptBuilder::default().push_opcode(Opcode::OP_1).into_script(),
        mine.private().secret.as_slice(),
    )
    .await
    .unwrap();
    assert_ne!(spend.txid(), refund.txid());

    // Built and broadcast, not mined: no row for either.
    let unmined = page(&fixture);
    assert!(!has_row(&unmined, &spend));
    assert!(!has_row(&unmined, &refund));

    fixture.scan_block_containing(&spend, 12);
    fixture.scan_block_containing(&refund, 13);

    let own = own_address(&fixture);
    let check = |fixture: &LightCoinFixture| {
        let page = page(fixture);
        for (name, tx) in [("spend", &spend), ("refund", &refund)] {
            let row = row_of(&page, tx);
            assert_eq!(row.from, vec![script_address.clone()], "{name}");
            assert_eq!(row.to, vec![own.clone()], "{name}");
            assert_ne!(row.block_height, 0, "{name} is mined");
        }
    };
    check(&fixture);
    let fixture = fixture.reopen();
    check(&fixture);
}

/// T39.8.9d3, R39.8.9d2 (d): a record that cannot be written costs the row its address and does not fail
/// the swap step. The record file is replaced by a directory, so every write fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d3_a_failing_record_leaves_the_swap_step_whole_and_the_row_without_the_address() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    std::fs::create_dir(fixture.history().tx_addresses_path()).unwrap();
    let (mine, other) = parties();

    let payment = send_payment(&fixture, &mine, &other).await;
    let spend = z_p2sh_spend(
        &fixture.coin,
        payment.clone(),
        TIME_LOCK,
        SEQUENCE_FINAL,
        redeem_script(&mine, &other),
        ScriptBuilder::default()
            .push_data(&SECRET)
            .push_opcode(Opcode::OP_0)
            .into_script(),
        other.private().secret.as_slice(),
    )
    .await
    .unwrap();
    fixture.scan_block_containing(&spend, 12);

    let own = own_address(&fixture);
    let page = page(&fixture);
    let payment_row = row_of(&page, &payment);
    assert_eq!(payment_row.to, vec![own.clone()], "no script address");
    let spend_row = row_of(&page, &spend);
    assert!(spend_row.from.is_empty(), "no script address");
    assert_eq!(spend_row.to, vec![own]);
}
