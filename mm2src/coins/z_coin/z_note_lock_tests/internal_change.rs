//! Notes the wallet received on the internal (change) scope of its account are spendable: they are
//! counted in the balance, and every build that selects one spends it with the key of that scope.
//!
//! Since the Ironwood upgrade, the chain's own wallets send their change to the ZIP 32 internal address
//! of the account. The wallet database scans both scopes and offers notes of both to note selection.
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport, with the Ironwood upgrade active, so every build is
//! a version-6 transaction. The tests make real Sapling spend proofs and verify them with the Sapling
//! verifying keys (the Pirate1 one also a real Halo2 proof), so they need the Sapling parameter files and
//! say so where those are absent.

use super::*;
use crate::z_coin::z_coin_wallet_db::tests::compact_block_paying;
use crate::z_coin::z_ironwood_read_tests::{ironwood_bundle_verifies, shielded_sighash};
use crate::z_coin::z_pirate1_tests::MAIN_ADDRESS;
use crate::z_coin::z_tx_read::read_tx;
use crate::{TransactionDetails, WithdrawError, WithdrawRequest};
use keys::Public;
use zcash_primitives::transaction::TxVersion;
use zip32::Scope;

const IRONWOOD_HEIGHT: u32 = 5;
const SEND_SAT: u64 = 1_000_000;
const TIME_LOCK: u32 = 1_790_000_000;
/// The compressed secp256k1 generator and its double: two valid public keys.
const MY_KEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const OTHER_KEY: &str = "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
const SECRET_HASH: [u8; 20] = [5; 20];

/// The test parameters with the Ironwood upgrade active from height 5: the build height 12 is on the
/// Ironwood branch.
fn ironwood_params() -> ZcoinConsensusParams {
    let mut params = test_params_with_zip212();
    params.ironwood_activation_height = Some(IRONWOOD_HEIGHT);
    params
}

fn test_spending_key() -> ExtendedSpendingKey { ExtendedSpendingKey::master(&[15; 32]).expect("a valid master key") }

/// The address of `scope`: the wallet's own address, or the change address the chain's wallets pay
/// their change to.
fn address_of(extsk: &ExtendedSpendingKey, scope: Scope) -> PaymentAddress {
    let dfvk = extsk.to_diversifiable_full_viewing_key();
    match scope {
        Scope::External => dfvk.default_address().1,
        Scope::Internal => dfvk.change_address().1,
    }
}

/// A wallet paid one note of `NOTE_VALUE` per `(scope, txid byte, rseed byte)` entry, all in the block at
/// height 11, the note of entry `i` at tree position `i`.
fn wallet_paid(notes: &[(Scope, u8, u8)]) -> Option<LightCoinFixture> {
    let params_dir = sapling_params_dir()?;
    let params = ironwood_params();
    let extsk = test_spending_key();
    #[allow(deprecated)]
    let extfvk = extsk.to_extended_full_viewing_key();
    let db_dir = test_db_dir("internal-change");

    let history = LightCoinFixture::open_history(&db_dir, &params, &extfvk);
    let paid = notes
        .iter()
        .map(|(scope, txid_byte, rseed_byte)| (address_of(&extsk, *scope), NOTE_VALUE, *txid_byte, *rseed_byte))
        .collect::<Vec<_>>();
    history
        .insert_compact_blocks_for_test(&[compact_block_paying(
            u64::from(NOTES_HEIGHT),
            NOTES_HEIGHT as u8,
            9,
            extfvk.fvk().ovk,
            &paid,
        )])
        .unwrap();
    assert_eq!(
        history
            .scan_cached_blocks_to_height(params.clone(), u64::from(NOTES_HEIGHT), 1_000, 0, |_, _| {})
            .unwrap(),
        u64::from(NOTES_HEIGHT)
    );

    Some(LightCoinFixture::assemble(
        db_dir,
        params_dir,
        params,
        extsk,
        history,
        2,
        u64::from(NOTES_HEIGHT),
        FEE_SAT,
    ))
}

/// The nullifier of the note a [`wallet_paid`] wallet holds at tree position `position`, derived with the
/// nullifier key of `scope`: the one the network expects when the note is spent.
fn expected_nullifier(extsk: &ExtendedSpendingKey, scope: Scope, rseed_byte: u8, position: u64) -> [u8; 32] {
    let note = Note::from_parts(
        address_of(extsk, scope),
        NoteValue::from_raw(NOTE_VALUE),
        Rseed::AfterZip212([rseed_byte; 32]),
    );
    note.nf(&extsk.to_diversifiable_full_viewing_key().to_nk(scope), position)
        .0
}

/// Whether every Sapling spend proof, output proof and signature of `tx` verifies, with the Sapling
/// verifying keys, over `sighash`.
fn sapling_verifies_over(fixture: &LightCoinFixture, tx: &ZTransaction, sighash: [u8; 32]) -> bool {
    let bundle = tx.sapling_bundle().expect("a Sapling bundle");
    let (spend_vk, output_vk) = fixture.coin.z_fields.z_tx_prover.verifying_keys();
    let mut batch = sapling::BatchValidator::new();
    batch.check_bundle(bundle.clone(), sighash) && batch.validate(&spend_vk, &output_vk, crate::z_coin::os_rng())
}

/// [`sapling_verifies_over`] the transaction's own shielded signature hash: a version-6 transaction
/// without transparent inputs signs its identifier. A sighash off by one bit must not verify, so a pass
/// is not a verifier that accepts anything.
fn sapling_verifies(fixture: &LightCoinFixture, tx: &ZTransaction) -> bool {
    assert_eq!(tx.version(), TxVersion::V6);
    let sighash = shielded_sighash(tx);
    let mut wrong = sighash;
    wrong[0] ^= 1;
    assert!(
        !sapling_verifies_over(fixture, tx, wrong),
        "a wrong sighash must not verify"
    );
    sapling_verifies_over(fixture, tx, sighash)
}

async fn withdraw(
    fixture: &LightCoinFixture,
    to: &str,
    amount: &str,
    max: bool,
) -> Result<TransactionDetails, MmError<WithdrawError>> {
    let request: WithdrawRequest =
        serde_json::from_value(json!({ "coin": "ARRR", "to": to, "amount": amount, "max": max })).unwrap();
    fixture.coin.withdraw(request).compat().await
}

fn sapling_to(fixture: &LightCoinFixture) -> String {
    encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &fixture.to_addr,
    )
}

/// The fixture's internal address is the ZIP 32 internal address of the account, which is not the
/// wallet's own address.
#[test]
fn the_change_address_is_the_default_address_of_the_internal_key() {
    let extsk = test_spending_key();
    let internal = extsk.derive_internal().expect("an internal key");
    assert_eq!(address_of(&extsk, Scope::Internal), internal.default_address().1);
    assert_ne!(address_of(&extsk, Scope::Internal), extsk.default_address().1);
    assert_eq!(address_of(&extsk, Scope::External), extsk.default_address().1);
}

/// The balance counts a note of either scope, and a `max` withdrawal spends exactly that balance: there
/// is no value the balance shows that a build cannot spend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_balance_and_a_max_withdrawal_agree_for_a_wallet_with_internal_change() {
    let Some(fixture) = wallet_paid(&[(Scope::External, 68, 42), (Scope::Internal, 69, 43)]) else {
        return;
    };
    assert_eq!(fixture.wallet_balance(), 2 * NOTE_VALUE);
    let (spendable, unspendable) = fixture.reported_balance().await;
    assert_eq!(spendable, sat(2 * NOTE_VALUE));
    assert_eq!(unspendable, sat(0));

    let details = withdraw(&fixture, &sapling_to(&fixture), "0", true).await.unwrap();
    assert_eq!(details.spent_by_me, spendable, "the whole balance is spent");
    assert_eq!(details.received_by_me, sat(0), "no change");
    let tx = read_tx(&details.tx_hex.0).unwrap();
    assert_eq!(
        nullifiers(&tx),
        BTreeSet::from([
            expected_nullifier(&fixture.extsk, Scope::External, 42, 0),
            expected_nullifier(&fixture.extsk, Scope::Internal, 43, 1),
        ])
    );
    assert!(sapling_verifies(&fixture, &tx));
}

/// A withdrawal that selects an internal note spends it with the internal key: the nullifier is the
/// one of the internal nullifier key and the bundle verifies. Once sent and mined, the wallet database
/// sees the note spent and holds the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawal_spends_an_internal_note() {
    let Some(mut fixture) = wallet_paid(&[(Scope::Internal, 68, 42)]) else {
        return;
    };
    assert_eq!(fixture.wallet_balance(), NOTE_VALUE);
    let internal_nf = expected_nullifier(&fixture.extsk, Scope::Internal, 42, 0);
    assert_ne!(
        internal_nf,
        expected_nullifier(&fixture.extsk, Scope::External, 42, 0),
        "the scopes have different nullifier keys"
    );

    let (built, data) = fixture.build(SEND_SAT).await.expect("the internal note is spendable");
    assert_eq!(data.spent_by_me, NOTE_VALUE);
    assert_eq!(nullifiers(&built), BTreeSet::from([internal_nf]));
    assert!(sapling_verifies(&fixture, &built));

    let sent = fixture.send(SEND_SAT).await.expect("the send succeeds");
    assert_eq!(nullifiers(&sent), BTreeSet::from([internal_nf]));
    assert!(sapling_verifies(&fixture, &sent));
    fixture.scan_block_containing(&sent, 12);
    assert_eq!(
        fixture.wallet_balance(),
        NOTE_VALUE - SEND_SAT - FEE_SAT,
        "the internal note is spent and the change is held"
    );
}

/// A swap payment (a transparent P2SH output funded from shielded notes) spends an internal note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swap_payment_spends_an_internal_note() {
    let Some(fixture) = wallet_paid(&[(Scope::Internal, 68, 42)]) else {
        return;
    };
    let payment = super::super::z_htlc::z_send_htlc(
        &fixture.coin,
        TIME_LOCK,
        &Public::from_slice(&hex::decode(MY_KEY).unwrap()).unwrap(),
        &Public::from_slice(&hex::decode(OTHER_KEY).unwrap()).unwrap(),
        &SECRET_HASH,
        BigDecimal::from(1),
    )
    .await
    .expect("the HTLC is funded");

    let vout = &payment.transparent_bundle().expect("transparent outputs").vout;
    assert_eq!(u64::from(vout[0].value()), 100_000_000, "output 0 pays the HTLC");
    assert_eq!(
        nullifiers(&payment),
        BTreeSet::from([expected_nullifier(&fixture.extsk, Scope::Internal, 42, 0)])
    );
    assert!(sapling_verifies(&fixture, &payment));
}

/// An external and an internal note spent together, each with the key of its own scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_external_and_an_internal_note_are_spent_together() {
    let Some(fixture) = wallet_paid(&[(Scope::External, 68, 42), (Scope::Internal, 69, 43)]) else {
        return;
    };
    let (tx, data) = fixture
        .build(NOTE_VALUE + SEND_SAT)
        .await
        .expect("two notes cover the amount");
    assert_eq!(data.spent_by_me, 2 * NOTE_VALUE);
    assert_eq!(
        nullifiers(&tx),
        BTreeSet::from([
            expected_nullifier(&fixture.extsk, Scope::External, 42, 0),
            expected_nullifier(&fixture.extsk, Scope::Internal, 43, 1),
        ])
    );
    assert!(sapling_verifies(&fixture, &tx));
}

/// A withdrawal to a Pirate1 address whose Sapling input is an internal note: the Sapling bundle and the
/// Ironwood bundle both verify.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pirate1_withdrawal_spends_an_internal_note() {
    let Some(fixture) = wallet_paid(&[(Scope::Internal, 68, 42)]) else {
        return;
    };
    let details = withdraw(&fixture, MAIN_ADDRESS, "0.01", false).await.unwrap();
    let tx = read_tx(&details.tx_hex.0).unwrap();
    assert!(tx.ironwood_bundle().is_some());
    assert_eq!(
        nullifiers(&tx),
        BTreeSet::from([expected_nullifier(&fixture.extsk, Scope::Internal, 42, 0)])
    );
    assert!(sapling_verifies(&fixture, &tx));
    assert!(
        ironwood_bundle_verifies(&tx, shielded_sighash(&tx)),
        "the Halo2 proof and the signatures"
    );
}
