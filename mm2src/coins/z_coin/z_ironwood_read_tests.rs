//! Reading and verifying mainnet transactions that carry Ironwood actions (CRD ch.39 T39.6.5a, R39.8.0bb).
//!
//! Two transactions of the dictated chain, fetched once from a lightwalletd endpoint with
//! `GetTransaction` and stored as the raw bytes the endpoint returned, serve every reader of full
//! transaction bytes in this project:
//!
//! * a **withdrawal** from a wallet that uses the Ironwood pool by default: one Sapling spend, two
//!   Sapling outputs, two Ironwood actions;
//! * a **deposit** into a Sapling address: no Sapling spend, two Sapling outputs, two Ironwood actions.
//!
//! The tests here cover the readers that can be driven without a coin: the Zcash library reader of
//! `z_tx_read`, the commitment-tree loop's reader, the reader behind `tx_enum_from_bytes`, the
//! broadcast record of D39.8.0e, and the Halo2 proof and signatures of the Ironwood bundle. The
//! readers that need a transport are covered next to their fakes (`z_lightwalletd/tests.rs`,
//! `z_lightwalletd/htlc_index/tests.rs`), which import the fixtures from here.

use super::z_coin_ops::record_broadcast_in_wallet_db;
use super::z_coin_wallet_db::tests::test_params_with_zip212;
use super::z_tx_read::{read_tx, txid_display, txid_internal};
use crate::{Transaction, TransactionEnum};
use orchard::bundle::BatchValidator;
use orchard::circuit::{OrchardCircuitVersion, VerifyingKey};
use orchard::ValuePool;
use sapling::Node;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::Instant;
use zcash_primitives::transaction::components::orchard::bundle_version_for_branch;
use zcash_primitives::transaction::{Transaction as ZTransaction, TxVersion};
use zcash_protocol::consensus::BranchId;

/// A recorded mainnet transaction and what the chain says about it.
pub(crate) struct Recorded {
    pub(crate) hex: &'static str,
    /// Mined height, from the compact block that contained it.
    pub(crate) height: u32,
    /// The identifier in display order, the order block explorers resolve.
    pub(crate) txid_display: &'static str,
    /// SHA-256 of the raw bytes, to catch an edited fixture.
    pub(crate) sha256: &'static str,
    pub(crate) sapling_spends: usize,
    pub(crate) sapling_outputs: usize,
    pub(crate) ironwood_actions: usize,
}

impl Recorded {
    pub(crate) fn bytes(&self) -> Vec<u8> { hex::decode(self.hex.trim()).expect("a fixture is valid hex") }
}

/// Mainnet ARRR, height 4198585: one Sapling spend, two Sapling outputs, two Ironwood actions.
pub(crate) const WITHDRAW: Recorded = Recorded {
    hex: include_str!("test-fixtures/arrr-ironwood-withdraw-0f1c0232.hex"),
    height: 4_198_585,
    txid_display: "0f1c0232703b3eaa030998ef181c3317f42f89a77de70f5898e4212ec43c5bca",
    sha256: "c581bba2a70b97a1d7a322f3b5e1903c33a88dcb1ec8caa1fe7758e1f35e7a48",
    sapling_spends: 1,
    sapling_outputs: 2,
    ironwood_actions: 2,
};

/// Mainnet ARRR, height 4198378: no Sapling spend, two Sapling outputs, two Ironwood actions.
pub(crate) const DEPOSIT: Recorded = Recorded {
    hex: include_str!("test-fixtures/arrr-ironwood-deposit-4b0d4b2f.hex"),
    height: 4_198_378,
    txid_display: "4b0d4b2f10bd1887c087cd5d5cc6110a74ef9244f0e4d1b110753f64b51e8299",
    sha256: "7098c7a05706c56543b9229a62ec7cd50c6aed5cf77c9c1790dbd13413c76f73",
    sapling_spends: 0,
    sapling_outputs: 2,
    ironwood_actions: 2,
};

const ALL: [&Recorded; 2] = [&WITHDRAW, &DEPOSIT];

fn serialize(tx: &ZTransaction) -> Vec<u8> {
    let mut out = Vec::new();
    tx.write(&mut out).expect("writing to a Vec cannot fail");
    out
}

/// The fixture files are the raw bytes the endpoint returned: not edited, and the shapes the tests
/// below claim.
#[test]
fn t39_6_5a_the_fixtures_are_the_recorded_bytes() {
    for recorded in ALL {
        let bytes = recorded.bytes();
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            recorded.sha256,
            "{}",
            recorded.txid_display
        );
        // Version-6 header (0x80000006) and version-group id (0xD884B698), little endian.
        assert_eq!(bytes[..8], [0x06, 0x00, 0x00, 0x80, 0x98, 0xb6, 0x84, 0xd8]);
        assert!(recorded.height > 4_134_000, "recorded after Ironwood activated");
    }
}

/// T39.6.5a, reader 1: the project's transaction reader (`z_tx_read::read_tx`) parses each
/// transaction as version 6 with the recorded shape, re-serializes it byte-identical, and computes
/// the chain's identifier.
#[test]
fn t39_6_5a_the_project_reader_round_trips_and_names_each_mainnet_ironwood_transaction() {
    for recorded in ALL {
        let bytes = recorded.bytes();
        let tx = read_tx(&bytes).unwrap_or_else(|e| panic!("{}: {}", recorded.txid_display, e));

        assert_eq!(tx.version(), TxVersion::V6);
        assert_eq!(tx.consensus_branch_id(), BranchId::Nu6_3);
        assert!(tx
            .transparent_bundle()
            .is_none_or(|bundle| bundle.vin.is_empty() && bundle.vout.is_empty()));
        assert!(tx.orchard_bundle().is_none(), "the Orchard slot is empty");
        let sapling = tx.sapling_bundle().expect("a Sapling bundle");
        assert_eq!(sapling.shielded_spends().len(), recorded.sapling_spends);
        assert_eq!(sapling.shielded_outputs().len(), recorded.sapling_outputs);
        let ironwood = tx.ironwood_bundle().expect("an Ironwood bundle");
        assert_eq!(ironwood.actions().len(), recorded.ironwood_actions);

        assert_eq!(
            serialize(&tx),
            bytes,
            "{} must re-serialize byte-identical",
            recorded.txid_display
        );

        // The identifier is the ZIP-244 digest, in the order the caller-facing surface uses.
        assert_eq!(hex::encode(txid_display(&tx).0), recorded.txid_display);
        let mut internal = txid_internal(&tx).take();
        internal.reverse();
        assert_eq!(hex::encode(internal), recorded.txid_display);
    }
}

/// T39.6.5a, the other full-byte readers that need no coin or transport. Each reads with
/// `BranchId::Sapling` (the argument matters only below version 5) or with a branch derived from the
/// height; every one must accept the transaction and agree on its identifier.
#[test]
fn t39_6_5a_every_other_reader_of_full_bytes_accepts_the_mainnet_transactions() {
    for recorded in ALL {
        let bytes = recorded.bytes();
        let expected = read_tx(&bytes).unwrap();

        // `tx_enum_from_bytes` (ZCoin's `SwapOps` surface), the reader of `gen_tx` for a previous
        // transaction, and the reader of the commitment-tree loop all call `Transaction::read`
        // directly. The branch argument is ignored for version 6.
        for branch in [BranchId::Sapling, BranchId::Nu6_3] {
            let tx = ZTransaction::read(bytes.as_slice(), branch).unwrap();
            assert_eq!(tx.txid(), expected.txid());
            assert_eq!(serialize(&tx), bytes);
        }
        let as_enum: TransactionEnum = ZTransaction::read(bytes.as_slice(), BranchId::Sapling)
            .map(|tx| tx.into())
            .unwrap();
        assert!(matches!(as_enum, TransactionEnum::ZTransaction(_)));
        assert_eq!(as_enum.tx_hex(), bytes);
        assert_eq!(hex::encode(as_enum.tx_hash().0), recorded.txid_display);

        // The commitment-tree loop takes the Sapling outputs' commitments and nothing else: both are
        // valid nodes and the Ironwood actions add none.
        let cmus: Vec<[u8; 32]> = expected
            .sapling_bundle()
            .unwrap()
            .shielded_outputs()
            .iter()
            .map(|output| output.cmu().to_bytes())
            .collect();
        assert_eq!(cmus.len(), recorded.sapling_outputs);
        for cmu in cmus {
            assert!(Option::<Node>::from(Node::from_bytes(cmu)).is_some());
        }
    }
}

/// T39.6.5a / D39.8.0e: the broadcast record accepts a version-6 transaction with a non-empty Ironwood
/// bundle. A transaction that spends no Sapling note (a deposit) has nothing to record. A transaction
/// that spends a Sapling note reaches the fee derivation and is refused there: its fee depends on the
/// Ironwood value balance too, which `broadcast_fee_sat` does not read (the Pirate1 withdrawal work).
#[test]
fn t39_6_5a_the_broadcast_record_reads_the_mainnet_transactions() {
    let params = test_params_with_zip212();
    let no_database = std::path::Path::new("/nonexistent/the-record-must-not-open-the-database.sqlite");

    let recorded = record_broadcast_in_wallet_db(no_database, &params, &DEPOSIT.bytes());
    assert_eq!(
        recorded.map(|tx| tx.map(|t| t.txid())),
        Ok(None),
        "a deposit spends no Sapling note: read, nothing to record"
    );

    let refused = record_broadcast_in_wallet_db(no_database, &params, &WITHDRAW.bytes()).unwrap_err();
    assert!(
        refused.contains("spends shielded notes but its fee cannot be derived from its bytes"),
        "{refused}"
    );
}

/// The verifying key of the Ironwood epoch's circuit, built once for the whole test binary: building it
/// is the expensive part of the check.
fn ironwood_verifying_key() -> &'static VerifyingKey {
    static KEY: OnceLock<VerifyingKey> = OnceLock::new();
    KEY.get_or_init(|| {
        let started = Instant::now();
        let circuit_version = bundle_version_for_branch(BranchId::Nu6_3, ValuePool::Ironwood)
            .expect("the Ironwood pool exists under the Ironwood branch")
            .circuit_version();
        assert_eq!(circuit_version, OrchardCircuitVersion::PostNu6_3);
        let key = VerifyingKey::build(circuit_version);
        eprintln!("t39_6_5a: built the verifying key in {:?}", started.elapsed());
        key
    })
}

/// The shielded signature hash of a version-6 transaction with no transparent inputs is its
/// identifier (R39.6.4e(b)); both fixtures are of that shape, which is asserted where it is used.
fn shielded_sighash(tx: &ZTransaction) -> [u8; 32] {
    assert!(tx.transparent_bundle().is_none_or(|bundle| bundle.vin.is_empty()));
    *tx.txid().as_ref()
}

/// T39.6.5a, the decisive check: the Ironwood bundle of each mainnet transaction verifies with this
/// project's own verifying key for the Ironwood epoch's circuit -- the Halo2 proof, every
/// spend-authorization signature and the binding signature, over the shielded signature hash. Had this
/// project's circuit version, bundle commitment or signature hash differed from the chain's, nothing
/// would verify; so proofs this project builds verify on the chain's nodes. A signature hash off by
/// one bit must not verify.
#[test]
fn t39_6_5a_the_ironwood_bundle_of_each_mainnet_transaction_verifies() {
    let key = ironwood_verifying_key();
    for recorded in ALL {
        let tx = read_tx(&recorded.bytes()).unwrap();
        let bundle = tx.ironwood_bundle().expect("an Ironwood bundle");
        assert_eq!(
            Some(bundle.bundle_version()),
            bundle_version_for_branch(tx.consensus_branch_id(), ValuePool::Ironwood),
            "the bundle's version follows the transaction's branch"
        );
        let sighash = shielded_sighash(&tx);

        let mut wrong = sighash;
        wrong[0] ^= 1;
        let mut batch = BatchValidator::new(key);
        batch.add_bundle(bundle, wrong).unwrap();
        assert!(
            !batch.validate(rand::rngs::OsRng),
            "{}: a wrong signature hash must not verify",
            recorded.txid_display
        );

        let started = Instant::now();
        let mut batch = BatchValidator::new(key);
        batch.add_bundle(bundle, sighash).unwrap();
        assert!(
            batch.validate(rand::rngs::OsRng),
            "{}: the chain's own Ironwood bundle must verify (proof, spend-authorization and binding signatures)",
            recorded.txid_display
        );
        eprintln!(
            "t39_6_5a: {} ({} actions) verified in {:?}",
            recorded.txid_display,
            bundle.actions().len(),
            started.elapsed()
        );
    }
}

/// T39.6.5a, the proof is really checked: one byte of the Halo2 proof changed, with the signatures still
/// valid over the original signature hash, must not verify. (The bytes still parse: the proof is
/// opaque to the reader, which checks only its length.)
#[test]
fn t39_6_5a_a_changed_proof_byte_does_not_verify() {
    let key = ironwood_verifying_key();
    let recorded = &DEPOSIT;
    let original_bytes = recorded.bytes();
    let original = read_tx(&original_bytes).unwrap();
    let sighash = shielded_sighash(&original);

    let mut bytes = original_bytes.clone();
    let index = bytes.len() * 2 / 3;
    bytes[index] ^= 0x01;
    let changed = read_tx(&bytes).expect("a changed proof byte still parses");
    let (before, after) = (original.ironwood_bundle().unwrap(), changed.ironwood_bundle().unwrap());
    assert_ne!(
        before.authorization().proof().as_ref(),
        after.authorization().proof().as_ref(),
        "the changed byte lies inside the proof"
    );
    assert_eq!(before.actions().len(), after.actions().len());

    let mut batch = BatchValidator::new(key);
    batch.add_bundle(after, sighash).unwrap();
    assert!(!batch.validate(rand::rngs::OsRng), "a changed proof must not verify");
}
