//! Withdrawals to a Pirate1 (Ironwood) address through a real Light-mode coin (CRD ch.39 T39.6.5b,
//! T39.6.5c, T39.6.5d, T39.6.5e).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport, here with an Ironwood activation height. The tests make real Sapling spend proofs and a real Halo2 Ironwood proof, so
//! they need the Sapling parameter files and say so where those are absent.

use super::*;
use crate::z_coin::z_coin_ops::record_broadcast_in_wallet_db;
use crate::z_coin::z_ironwood_read_tests::{ironwood_bundle_verifies, shielded_sighash, WITHDRAW};
use crate::z_coin::z_pirate1_tests::{ironwood_anchor_and_flags, MAIN_ADDRESS, REGTEST_ADDRESS, TEST_ADDRESS};
use crate::z_coin::z_tx_read::read_tx;
use crate::{TransactionDetails, TxFeeDetails, WithdrawError, WithdrawRequest};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use sapling::note_encryption::{try_sapling_note_decryption, PreparedIncomingViewingKey, Zip212Enforcement};
use zcash_primitives::transaction::TxVersion;

const IRONWOOD_HEIGHT: u32 = 5;
/// The coin's configured fee per kilobyte in these tests, which is not the fixed Ironwood fee.
const CONFIGURED_FEE_SAT: u64 = 1_000;
const IRONWOOD_FEE_SAT: u64 = 10_000;
const SEND_SAT: u64 = 1_000_000;

fn params_with_ironwood(height: Option<u32>) -> ZcoinConsensusParams {
    let mut params = test_params_with_zip212();
    params.ironwood_activation_height = height;
    params
}

/// A wallet with one note of 1.25 ARRR whose Ironwood activation height is `height`. The scripted backend
/// answers no tree-state request: none exists any more, a Pirate1 withdrawal reads none (R39.6.5e).
fn wallet(height: Option<u32>) -> Option<LightCoinFixture> {
    LightCoinFixture::with_notes_under(1, 2, params_with_ironwood(height), CONFIGURED_FEE_SAT)
}

/// How many requests of a kind the scripted backend saw that read a tree state. The transport has no
/// such method, so this is the number of recorded calls whose name says "tree"; it must stay zero.
fn tree_reads(fixture: &LightCoinFixture) -> usize {
    fixture
        .transport
        .calls
        .lock()
        .iter()
        .filter(|(_, method)| method.contains("tree"))
        .count()
}

fn request(to: &str, amount: &str, max: bool) -> WithdrawRequest {
    serde_json::from_value(json!({ "coin": "ARRR", "to": to, "amount": amount, "max": max })).unwrap()
}

async fn withdraw(
    fixture: &LightCoinFixture,
    to: &str,
    amount: &str,
    max: bool,
) -> Result<TransactionDetails, MmError<WithdrawError>> {
    fixture.coin.withdraw(request(to, amount, max)).compat().await
}

fn recipient_ivk() -> orchard::keys::IncomingViewingKey {
    let sk = SpendingKey::from_bytes([7u8; 32]).unwrap();
    FullViewingKey::from(&sk).to_ivk(Scope::External)
}

fn sapling_balance(tx: &ZTransaction) -> i64 { i64::from(*tx.sapling_bundle().unwrap().value_balance()) }

fn ironwood_balance(tx: &ZTransaction) -> i64 { i64::from(*tx.ironwood_bundle().unwrap().value_balance()) }

fn fee_of(details: &TransactionDetails) -> BigDecimal {
    match details.fee_details.as_ref().expect("a fee") {
        TxFeeDetails::Utxo(fee) => fee.amount.clone(),
        other => panic!("unexpected fee details {:?}", other),
    }
}

/// The Sapling notes of `tx` this wallet can decrypt, as (value, address).
fn own_sapling_notes(fixture: &LightCoinFixture, tx: &ZTransaction) -> Vec<(u64, sapling::PaymentAddress)> {
    #[allow(deprecated)]
    let extfvk = fixture.extsk.to_extended_full_viewing_key();
    let ivk = PreparedIncomingViewingKey::new(&extfvk.fvk.vk.ivk());
    tx.sapling_bundle()
        .unwrap()
        .shielded_outputs()
        .iter()
        .filter_map(|output| {
            try_sapling_note_decryption::<_>(&ivk, output, Zip212Enforcement::On)
                .map(|(note, address, _)| (note.value().inner(), address))
        })
        .collect()
}

/// T39.6.5c: building a Pirate1 withdrawal against the wallet fixture. The coin's configured fee is 1,000
/// base units; the transaction pays 10,000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5c_a_withdrawal_to_a_pirate1_address_builds_the_version_6_transaction() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };

    let started = Instant::now();
    let details = withdraw(&fixture, MAIN_ADDRESS, "0.01", false).await.unwrap();
    eprintln!(
        "t39_6_5c: the first Ironwood build of the process took {:?} (debug build)",
        started.elapsed()
    );

    // The response keeps its shape (R39.6.5c).
    let change = NOTE_VALUE - SEND_SAT - IRONWOOD_FEE_SAT;
    assert_eq!(details.from, vec![fixture.coin.z_fields.my_z_addr_encoded.clone()]);
    assert_eq!(details.to, vec![MAIN_ADDRESS.to_owned()]);
    assert_eq!(details.spent_by_me, sat(NOTE_VALUE));
    assert_eq!(details.received_by_me, sat(change));
    // Received minus spent, negative for a withdrawal (R39.5A.3, R39.6.5c).
    assert_eq!(details.my_balance_change, -sat(SEND_SAT + IRONWOOD_FEE_SAT));
    assert_eq!(
        fee_of(&details),
        sat(IRONWOOD_FEE_SAT),
        "not the configured {CONFIGURED_FEE_SAT}"
    );

    // The transaction reads back through the project's reader.
    let bytes = details.tx_hex.0.clone();
    let tx = read_tx(&bytes).unwrap();
    let mut written = Vec::new();
    tx.write(&mut written).unwrap();
    assert_eq!(written, bytes, "re-serializes byte-identical");

    // Version 6 on the Ironwood branch (R39.6.4e), the selected Sapling spend, two Ironwood actions, no
    // transparent part and the empty Orchard slot.
    assert_eq!(tx.version(), TxVersion::V6);
    assert_eq!(tx.consensus_branch_id(), BranchId::Nu6_3);
    assert!(tx.transparent_bundle().is_none());
    assert!(tx.orchard_bundle().is_none());
    assert_eq!(tx.sapling_bundle().unwrap().shielded_spends().len(), 1);
    let ironwood = tx.ironwood_bundle().unwrap();
    assert_eq!(
        ironwood.actions().len(),
        2,
        "one output padded to the default two actions"
    );
    // An output-only bundle (spends disabled, outputs enabled) anchored at the empty-tree root, which is
    // the form of the chain's own payment into Ironwood (R39.6.5d, R39.6.5e).
    assert_eq!(ironwood_anchor_and_flags(&tx), (true, false, true));

    // The Sapling change is a single note to the wallet's own address; the Sapling bundle is padded.
    let notes = own_sapling_notes(&fixture, &tx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, change);
    assert_eq!(notes[0].1, fixture.coin.z_fields.my_z_addr);

    // Value balances: the selected note less the change leaves Sapling, the amount enters Ironwood,
    // and their sum is the fee (R39.6.5d).
    assert_eq!(sapling_balance(&tx), (NOTE_VALUE - change) as i64);
    assert_eq!(ironwood_balance(&tx), -(SEND_SAT as i64));
    assert_eq!(sapling_balance(&tx) + ironwood_balance(&tx), IRONWOOD_FEE_SAT as i64);

    // The recipient decrypts exactly one output: the amount, to its address, with an empty memo; and the
    // other action is a dummy. The whole value that enters Ironwood is that amount (checked above through
    // the value balance): the change is the Sapling note found above and never enters Ironwood.
    let decrypted = ironwood.decrypt_outputs_with_keys(&[recipient_ivk()]);
    assert_eq!(decrypted.len(), 1);
    let (_, _, note, address, memo) = &decrypted[0];
    assert_eq!(note.value().inner(), SEND_SAT);
    assert_eq!(
        address.to_raw_address_bytes().to_vec(),
        hex::decode(crate::z_coin::z_pirate1_tests::RAW_ADDRESS).unwrap()
    );
    assert_eq!(memo[0], 0xf6, "the empty memo");
    assert!(memo[1..].iter().all(|b| *b == 0));

    // No tree state is read for the anchor.
    assert_eq!(tree_reads(&fixture), 0);

    // The Ironwood proof verifies with the project's verifying key; a changed signature hash does not.
    let sighash = shielded_sighash(&tx);
    let started = Instant::now();
    assert!(
        ironwood_bundle_verifies(&tx, sighash),
        "the Halo2 proof and the signatures"
    );
    eprintln!("t39_6_5c: verified the Ironwood bundle in {:?}", started.elapsed());
    let mut wrong = sighash;
    wrong[0] ^= 1;
    assert!(!ironwood_bundle_verifies(&tx, wrong));
}

/// R39.6.5f: a withdrawal to a Sapling address keeps the configured fee, reads no tree state and carries
/// no Ironwood bundle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5c_a_withdrawal_to_a_sapling_address_is_unchanged() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let to = encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &fixture.to_addr,
    );
    let details = withdraw(&fixture, &to, "0.01", false).await.unwrap();

    assert_eq!(
        fee_of(&details),
        sat(CONFIGURED_FEE_SAT),
        "the configured fee, not 10,000"
    );
    let tx = read_tx(&details.tx_hex.0).unwrap();
    assert_eq!(tx.version(), TxVersion::V6);
    assert!(tx.ironwood_bundle().is_none(), "the Ironwood slot is empty");
    assert_eq!(
        sapling_balance(&tx),
        CONFIGURED_FEE_SAT as i64,
        "nothing leaves Sapling but the fee"
    );
    assert_eq!(tree_reads(&fixture), 0, "no tree-state read for any other transaction");

    // `max` takes the configured fee off, too.
    let max = withdraw(&fixture, &to, "0", true).await.unwrap();
    assert_eq!(fee_of(&max), sat(CONFIGURED_FEE_SAT));
    assert_eq!(max.spent_by_me, sat(NOTE_VALUE));
    assert_eq!(max.received_by_me, sat(0));
}

/// T39.6.5c: with `max: true` the amount sent is the spendable balance less 10,000 base units.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5c_max_sends_the_spendable_balance_less_the_fixed_fee() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let details = withdraw(&fixture, MAIN_ADDRESS, "0", true).await.unwrap();

    assert_eq!(fee_of(&details), sat(IRONWOOD_FEE_SAT));
    assert_eq!(details.spent_by_me, sat(NOTE_VALUE));
    assert_eq!(details.received_by_me, sat(0), "no change");
    let tx = read_tx(&details.tx_hex.0).unwrap();
    assert_eq!(ironwood_balance(&tx), -((NOTE_VALUE - IRONWOOD_FEE_SAT) as i64));
    assert_eq!(sapling_balance(&tx), NOTE_VALUE as i64);
    assert!(own_sapling_notes(&fixture, &tx).is_empty());
    let decrypted = tx
        .ironwood_bundle()
        .unwrap()
        .decrypt_outputs_with_keys(&[recipient_ivk()]);
    assert_eq!(decrypted[0].2.value().inner(), NOTE_VALUE - IRONWOOD_FEE_SAT);
}

/// T39.6.5c: a target height below the Ironwood activation height is refused before note selection, with
/// an error that names the coin and the reason. Nothing is read from the backend and nothing is built.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5c_a_target_height_below_the_activation_height_is_refused() {
    // The chain is at 11 and the next block is 12; Ironwood activates at 20.
    let Some(fixture) = wallet(Some(20)) else {
        return;
    };
    let refused = withdraw(&fixture, MAIN_ADDRESS, "0.01", false).await.unwrap_err();
    let WithdrawError::InvalidAddress(reason) = refused.get_inner() else {
        panic!("{refused:?}");
    };
    assert!(reason.contains("ARRR"), "{reason}");
    assert!(reason.contains("below the Ironwood activation height 20"), "{reason}");
    assert_eq!(tree_reads(&fixture), 0);
    assert_eq!(fixture.broadcasts(), 0);
    assert_eq!(fixture.wallet_balance(), NOTE_VALUE);
    assert!(fixture.in_flight().is_empty());

    // The address is still an acceptable form for a coin that has an Ironwood pool.
    assert!(fixture.coin.validate_address(MAIN_ADDRESS).is_valid);
}

/// T39.6.5b on a real coin: `validateaddress` and the legacy `withdraw` (which the task route reaches,
/// `init_withdraw` calling `withdraw`) accept the address of the coin's network and refuse the rest, and
/// a coin without an Ironwood pool refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5b_validateaddress_and_withdraw_answer_alike_on_a_coin() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let coin = &fixture.coin;
    assert!(coin.validate_address(MAIN_ADDRESS).is_valid);
    let sapling = encode_payment_address(
        coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &fixture.to_addr,
    );
    assert!(coin.validate_address(&sapling).is_valid);
    for (address, cause) in [(TEST_ADDRESS, "another network"), (REGTEST_ADDRESS, "another network")] {
        let answer = coin.validate_address(address);
        assert!(!answer.is_valid);
        assert!(
            answer.reason.as_deref().unwrap_or_default().contains(cause),
            "{:?}",
            answer.reason
        );
        // The withdrawal refuses the same string before anything else, naming both accepted forms.
        let refused = withdraw(&fixture, address, "0.01", false).await.unwrap_err();
        let WithdrawError::InvalidAddress(reason) = refused.get_inner() else {
            panic!("{refused:?}");
        };
        assert!(
            reason.contains("Sapling address of ARRR") && reason.contains("Pirate1 (Ironwood)"),
            "{reason}"
        );
    }
    assert_eq!(fixture.broadcasts() + tree_reads(&fixture), 0);

    // A coin without an Ironwood pool (no height, time or built-in value for these parameters).
    let Some(without_pool) = wallet(None) else {
        return;
    };
    let answer = without_pool.coin.validate_address(MAIN_ADDRESS);
    assert!(!answer.is_valid);
    assert!(answer.reason.unwrap_or_default().contains("ARRR has no Ironwood pool"));
    let refused = withdraw(&without_pool, MAIN_ADDRESS, "0.01", false).await.unwrap_err();
    assert!(
        matches!(refused.get_inner(), WithdrawError::InvalidAddress(_)),
        "{refused:?}"
    );
}

/// T39.6.5d: the Ironwood anchor of a built Pirate1 withdrawal is the empty-tree root and its flags
/// disable spends and enable outputs; the mainnet payment from Sapling into Ironwood (the fixture of
/// T39.6.5a) has the same anchor and flags, and so do the built ones, with and without change. The
/// change of the first never enters Ironwood: the Ironwood value balance is minus the amount sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5d_the_built_anchor_and_flags_match_the_chain_wallets_form() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let mainnet = ironwood_anchor_and_flags(&read_tx(&WITHDRAW.bytes()).unwrap());
    assert_eq!(
        mainnet,
        (true, false, true),
        "the mainnet payment into Ironwood from Sapling"
    );

    for (amount, max) in [("0.01", false), ("0", true)] {
        let details = withdraw(&fixture, MAIN_ADDRESS, amount, max).await.unwrap();
        let tx = read_tx(&details.tx_hex.0).unwrap();
        assert_eq!(ironwood_anchor_and_flags(&tx), mainnet, "max: {max}");
        let ironwood = tx.ironwood_bundle().unwrap();
        assert_eq!(ironwood.actions().len(), 2);
        assert!(!ironwood.flags().spends_enabled() && ironwood.flags().outputs_enabled());
        assert_eq!(ironwood.anchor(), &orchard::Anchor::empty_tree());
        // Everything that enters Ironwood is the one payment to the recipient.
        let decrypted = ironwood.decrypt_outputs_with_keys(&[recipient_ivk()]);
        assert_eq!(decrypted.len(), 1);
        assert_eq!(
            -ironwood_balance(&tx),
            i64::try_from(decrypted[0].2.value().inner()).unwrap(),
            "max: {max}"
        );
        assert_eq!(tree_reads(&fixture), 0);
    }
}

/// T39.6.5e: a submitted Pirate1 withdrawal is recorded exactly as a Sapling one is: its fee is derived
/// from the bytes, its note is excluded from the next selection, its unmined history row carries the final
/// amounts and keeps them when the block arrives, and its change counts as non-spendable balance. (The
/// vendored `zcash_client_sqlite` skips the Orchard and Ironwood nullifier marking instead of panicking.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5e_a_submitted_pirate1_withdrawal_is_recorded() {
    let Some(mut fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let details = withdraw(&fixture, MAIN_ADDRESS, "0.01", false).await.unwrap();
    let change = NOTE_VALUE - SEND_SAT - IRONWOOD_FEE_SAT;
    let tx = read_tx(&details.tx_hex.0).unwrap();

    // Before the second step nothing is recorded: an abandoned build strands no note.
    assert!(fixture.in_flight().is_empty());
    assert_eq!(fixture.reported_balance().await, (sat(NOTE_VALUE), BigDecimal::from(0)));

    // The second step: the caller submits the bytes, the backend accepts, the coin records.
    fixture
        .coin
        .send_raw_tx(&hex::encode(&details.tx_hex.0))
        .compat()
        .await
        .unwrap();
    assert_eq!(fixture.broadcasts(), 1);

    // The fee was read from the bytes: the Sapling and Ironwood value balances.
    let conn = db_common::sqlite::rusqlite::Connection::open(fixture.history().wallet_db_path()).unwrap();
    let fee: Option<i64> = conn
        .query_row(
            "SELECT fee FROM transactions WHERE txid = ?1",
            db_common::sqlite::rusqlite::params![&tx.txid().as_ref()[..]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fee, Some(IRONWOOD_FEE_SAT as i64));

    // The note is excluded from the next selection.
    assert_eq!(fixture.recorded_spend(&tx), NOTE_VALUE);
    assert!(fixture.in_flight().contains(tx.txid().as_ref()));
    let to = encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &fixture.to_addr,
    );
    let second = withdraw(&fixture, &to, "0.01", false).await.unwrap_err();
    assert!(
        matches!(second.get_inner(), WithdrawError::BalanceLockedByUnconfirmedTransaction { available, .. } if *available == sat(0)),
        "{second:?}"
    );

    // The unmined history row carries the final amounts, and the change is non-spendable balance.
    let unmined_page = fixture.history_page(true);
    assert_eq!(unmined_page.total, 2, "the receipt of the note and the withdrawal");
    let unmined = unmined_page.transactions[0].clone();
    assert_eq!(unmined.tx_hash, tx.txid().to_string());
    assert_eq!((unmined.block_height, unmined.timestamp), (0, 0));
    assert_eq!(unmined.spent_by_me, sat(NOTE_VALUE));
    assert_eq!(unmined.received_by_me, sat(change));
    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));

    // The block that mines it moves the same row, with the same amounts, and the change becomes spendable.
    fixture.scan_block_containing(&tx, 12);
    let mined_page = fixture.history_page(true);
    assert_eq!(mined_page.total, 2);
    let mined = &mined_page.transactions[0];
    assert_eq!(mined.internal_id, unmined.internal_id);
    assert_eq!(mined.spent_by_me, unmined.spent_by_me);
    assert_eq!(mined.received_by_me, unmined.received_by_me);
    assert_eq!(mined.my_balance_change, unmined.my_balance_change);
    assert_eq!(mined.block_height, 12);
    assert_eq!(fixture.wallet_balance(), change);
    assert_eq!(fixture.reported_balance().await, (sat(change), BigDecimal::from(0)));
}

/// T39.6.5a / R39.6.5g on the recorded mainnet withdrawal (one Sapling spend, two Ironwood actions): the
/// recorder derives its fee from the bytes and stores it in a real wallet database without panicking. The
/// note it spends is not this wallet's, so no note is marked spent and nothing is held in flight; the
/// transaction row carries the derived fee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_6_5a_the_recorder_stores_the_mainnet_withdrawal_with_its_derived_fee() {
    let Some(fixture) = wallet(Some(IRONWOOD_HEIGHT)) else {
        return;
    };
    let recorded = record_broadcast_in_wallet_db(
        fixture.history().wallet_db_path(),
        &fixture.coin.z_fields.consensus_params,
        &WITHDRAW.bytes(),
    )
    .unwrap()
    .expect("a Sapling spender is recorded");
    let conn = db_common::sqlite::rusqlite::Connection::open(fixture.history().wallet_db_path()).unwrap();
    let fee: Option<i64> = conn
        .query_row(
            "SELECT fee FROM transactions WHERE txid = ?1",
            db_common::sqlite::rusqlite::params![&recorded.txid().as_ref()[..]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fee, Some(IRONWOOD_FEE_SAT as i64));
    assert_eq!(fixture.recorded_spend(&recorded), 0, "none of this wallet's notes");
    assert_eq!(fixture.wallet_balance(), NOTE_VALUE);
}
