//! Version-6 transaction construction (CRD ch.39 R39.6.4e, plan step E).
//!
//! Two kinds of test live here. The ones that need nothing run everywhere: they build a
//! transparent-only transaction, which exercises the version, branch and wrapper selection without
//! Sapling proofs. The ones that build or verify a *Sapling* bundle need the Sapling parameter files
//! (`~/.zcash-params`, or the directory named by `ARRR_SAPLING_PARAMS`); where those are absent they
//! say so on stderr and return, because a proof cannot be made or checked without them. They are
//! still tests that run by default wherever the files are, which is every developer machine that has
//! ever started the wallet.

use super::z_tx_read::{read_tx, txid_internal};
use super::z_tx_read_fixtures::{V6_HEIGHT_389_HEX, V6_HEIGHT_68370_HEX};
use super::*;
use incrementalmerkletree::Hashable;
use kdf_crypto::dhash256;
use sapling::value::NoteValue;
use sapling::Rseed;
use std::convert::Infallible;
use zcash_primitives::transaction::TxVersion;
use zcash_transparent::address::TransparentAddress;
use zcash_transparent::bundle::OutPoint;

const IRONWOOD_HEIGHT: u32 = 297;

/// Header bytes a version-6 transaction starts with: `0x80000006`, version-group id `0xD884B698`,
/// consensus branch `0x37A5165B`, all little-endian (confirmed on the wire against the test chain).
const V6_HEADER: [u8; 12] = [0x06, 0x00, 0x00, 0x80, 0x98, 0xb6, 0x84, 0xd8, 0x5b, 0x16, 0xa5, 0x37];

/// Parameters shaped like Pirate's own test chain: mainnet address prefixes, Sapling at 61.
pub(super) fn params(ironwood_height: Option<u32>) -> ZcoinConsensusParams {
    let mut cfg = json!({
        "overwinter_activation_height": 61,
        "sapling_activation_height": 61,
        "blossom_activation_height": null,
        "heartwood_activation_height": null,
        "canopy_activation_height": null,
        "coin_type": 133,
        "hrp_sapling_extended_spending_key": "secret-extended-key-main",
        "hrp_sapling_extended_full_viewing_key": "zxviews",
        "hrp_sapling_payment_address": "zs",
        "b58_pubkey_address_prefix": [0x1c, 0xb8],
        "b58_script_address_prefix": [0x1c, 0xbd]
    });
    if let Some(height) = ironwood_height {
        cfg["ironwood_activation_height"] = json!(height);
    }
    serde_json::from_value(cfg).unwrap()
}

pub(super) fn standard_config(sapling_anchor: Option<sapling::Anchor>) -> BuildConfig {
    BuildConfig::Standard {
        sapling_anchor,
        orchard_anchor: None,
        ironwood_anchor: None,
        orchard_padding: BundlePadding::DEFAULT,
        ironwood_padding: BundlePadding::DEFAULT,
    }
}

/// A transparent-only transaction built at `height`: one P2PKH input, one output, a fixed fee.
fn build_transparent_only(params: &ZcoinConsensusParams, height: u32) -> ZTransaction {
    let secp = secp256k1::Secp256k1::new();
    let secret = secp256k1::SecretKey::from_slice(&[0x11; 32]).unwrap();
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret);
    let script = TransparentAddress::from_pubkey(&pubkey).script();
    let coin = TxOut::new(Amount::const_from_u64(100_000), script.clone().into());
    let mut builder = ZTxBuilder::new(params.clone(), height.into(), standard_config(None));
    builder
        .add_transparent_p2pkh_input(pubkey, OutPoint::new([7; 32], 0), coin)
        .unwrap();
    builder.add_transparent_output_raw(TxOut::new(Amount::const_from_u64(90_000), script.into()));
    let mut signing = TransparentSigningSet::new();
    signing.add_key(secret);
    builder
        .build(
            &signing,
            &[],
            &[],
            rand::rngs::OsRng,
            &NoProver,
            &NoProver,
            &FixedFeeRule::non_standard(Amount::const_from_u64(10_000)),
        )
        .unwrap()
        .into_transaction()
}

/// A prover for builds that contain no Sapling bundle, which must never be asked for a proof.
pub(super) struct NoProver;

impl sapling::prover::SpendProver for NoProver {
    type Proof = ();

    fn prepare_circuit(
        _proof_generation_key: sapling::ProofGenerationKey,
        _diversifier: sapling::Diversifier,
        _rseed: Rseed,
        _value: NoteValue,
        _alpha: jubjub::Fr,
        _rcv: sapling::value::ValueCommitTrapdoor,
        _anchor: bls12_381::Scalar,
        _merkle_path: sapling::MerklePath,
    ) -> Option<sapling::circuit::Spend> {
        None
    }

    fn create_proof<R: rand_core::RngCore>(&self, _circuit: sapling::circuit::Spend, _rng: &mut R) -> Self::Proof {
        panic!("a transparent-only build asked for a spend proof")
    }

    fn encode_proof(_proof: Self::Proof) -> sapling::bundle::GrothProofBytes { [0; 192] }
}

impl sapling::prover::OutputProver for NoProver {
    type Proof = ();

    fn prepare_circuit(
        _esk: &sapling::keys::EphemeralSecretKey,
        _payment_address: PaymentAddress,
        _rcm: jubjub::Fr,
        _value: NoteValue,
        _rcv: sapling::value::ValueCommitTrapdoor,
    ) -> sapling::circuit::Output {
        panic!("a transparent-only build asked for an output circuit")
    }

    fn create_proof<R: rand_core::RngCore>(&self, _circuit: sapling::circuit::Output, _rng: &mut R) -> Self::Proof {
        panic!("a transparent-only build asked for an output proof")
    }

    fn encode_proof(_proof: Self::Proof) -> sapling::bundle::GrothProofBytes { [0; 192] }
}

fn serialize(tx: &ZTransaction) -> Vec<u8> {
    let mut bytes = Vec::new();
    tx.write(&mut bytes).unwrap();
    bytes
}

// ---------------------------------------------------------------------------------------------
// No Sapling parameters needed
// ---------------------------------------------------------------------------------------------

/// The mapping itself: before the height the builder is on the Sapling branch and version 4, from
/// the height on it is on Ironwood's branch and version 6.
#[test]
fn the_activation_height_selects_the_branch_and_version() {
    let configured = params(Some(IRONWOOD_HEIGHT));
    for (height, branch, version) in [
        (IRONWOOD_HEIGHT - 1, BranchId::Sapling, TxVersion::V4),
        (IRONWOOD_HEIGHT, BranchId::Nu6_3, TxVersion::V6),
        (IRONWOOD_HEIGHT + 1_000_000, BranchId::Nu6_3, TxVersion::V6),
    ] {
        let at = BlockHeight::from_u32(height);
        assert_eq!(BranchId::for_height(&configured, at), branch, "height {height}");
        assert_eq!(TxVersion::suggested_for_branch(branch), version, "height {height}");
    }
}

/// A coin with no Ironwood height anywhere must keep building version 4 at every height: v6 is not
/// unconditional.
#[test]
fn a_coin_without_an_ironwood_height_still_builds_version_4() {
    let unconfigured = params(None);
    for height in [100u32, IRONWOOD_HEIGHT, 4_000_000] {
        let tx = build_transparent_only(&unconfigured, height);
        assert_eq!(tx.version(), TxVersion::V4, "height {height}");
        assert_eq!(tx.consensus_branch_id(), BranchId::Sapling, "height {height}");
        assert_eq!(serialize(&tx)[..4], [0x04, 0x00, 0x00, 0x80], "height {height}");
    }
}

/// Just below the height the builder still emits version 4 -- the pre-Ironwood chain case.
#[test]
fn a_build_below_the_activation_height_is_version_4() {
    let tx = build_transparent_only(&params(Some(IRONWOOD_HEIGHT)), IRONWOOD_HEIGHT - 1);
    assert_eq!(tx.version(), TxVersion::V4);
    assert_eq!(tx.consensus_branch_id(), BranchId::Sapling);
    // And the bytes read back as the same version-4 transaction.
    let bytes = serialize(&tx);
    let again = read_tx(&bytes).unwrap();
    assert_eq!(again.version(), TxVersion::V4);
    assert_eq!(serialize(&again), bytes);
}

/// The emitted bytes: header, version-group id and branch id on the wire; both the Orchard slot and
/// the Ironwood bundle empty; the whole round-tripping through the reader the swap path uses; and the
/// identifier being the ZIP-244 digest rather than a double SHA-256 of the bytes.
#[test]
fn a_build_at_the_activation_height_is_well_formed_version_6() {
    let tx = build_transparent_only(&params(Some(IRONWOOD_HEIGHT)), IRONWOOD_HEIGHT);
    let bytes = serialize(&tx);

    assert_eq!(bytes[..12], V6_HEADER, "header, version-group id, branch id");
    assert_eq!(tx.version(), TxVersion::V6);
    assert_eq!(tx.consensus_branch_id(), BranchId::Nu6_3);
    // Nothing is a Sapling bundle here, so the tail is: empty Sapling (two CompactSize zeros), the
    // empty Orchard slot (a single 0x00) and the empty Ironwood bundle (a single 0x00).
    assert_eq!(bytes[bytes.len() - 4..], [0, 0, 0, 0]);
    assert!(tx.orchard_bundle().is_none());
    assert!(tx.ironwood_bundle().is_none());

    let read = read_tx(&bytes).unwrap();
    assert_eq!(read.version(), TxVersion::V6);
    assert!(read.orchard_bundle().is_none());
    assert!(read.ironwood_bundle().is_none());
    assert_eq!(serialize(&read), bytes, "the reader round-trips the builder's bytes");
    assert_eq!(read.txid(), tx.txid());
    assert_eq!(txid_internal(&read), txid_internal(&tx));
    assert_ne!(
        txid_internal(&read),
        dhash256(&bytes),
        "a version-6 identifier is the ZIP-244 digest, not a double-SHA256 of the bytes"
    );
}

/// A height discovered after the parameters were cloned into the builder's owner reaches every clone,
/// and until then nothing changes.
#[test]
fn a_discovered_height_reaches_every_copy_of_the_parameters() {
    let coin_side = params(None);
    let builder_side = coin_side.clone();
    assert_eq!(builder_side.activation_height(NetworkUpgrade::Nu6_3), None);
    assert_eq!(build_transparent_only(&builder_side, 500).version(), TxVersion::V4);

    coin_side.record_discovered_ironwood_height(400);
    assert_eq!(
        builder_side.activation_height(NetworkUpgrade::Nu6_3),
        Some(BlockHeight::from_u32(400))
    );
    assert_eq!(build_transparent_only(&builder_side, 399).version(), TxVersion::V4);
    assert_eq!(build_transparent_only(&builder_side, 400).version(), TxVersion::V6);
}

/// A configured height always wins over a discovered one, and a discovered height is never
/// serialized (it is a fact about the live chain, not configuration).
#[test]
fn a_configured_height_wins_and_a_discovered_one_is_not_serialized() {
    let configured = params(Some(IRONWOOD_HEIGHT));
    configured.record_discovered_ironwood_height(5_000);
    assert_eq!(
        configured.activation_height(NetworkUpgrade::Nu6_3),
        Some(BlockHeight::from_u32(IRONWOOD_HEIGHT))
    );

    let discovered = params(None);
    discovered.record_discovered_ironwood_height(5_000);
    let json = serde_json::to_value(&discovered).unwrap();
    assert!(json.get("ironwood_discovered_height").is_none(), "{json}");
    let reloaded: ZcoinConsensusParams = serde_json::from_value(json).unwrap();
    assert_eq!(reloaded.activation_height(NetworkUpgrade::Nu6_3), None);

    // Zero is not a height the chain can activate at.
    let zero = params(None);
    zero.record_discovered_ironwood_height(0);
    assert_eq!(zero.activation_height(NetworkUpgrade::Nu6_3), None);
}

/// The built-in activation time belongs to the real ARRR chain. Pirate's test chain carries the same
/// address prefixes (`zs`, `pirate`) and reports `chainName` `main`, so those cannot tell the two
/// apart; the Sapling activation height can.
#[test]
fn the_builtin_time_is_not_applied_to_a_chain_that_only_looks_like_mainnet() {
    let test_chain = params(None); // Sapling at 61, mainnet prefixes
    assert!(matches!(test_chain.network_type_hint(), NetworkType::Main));
    assert_eq!(effective_ironwood_activation_time(&test_chain, "ARRR"), None);
    assert_eq!(configured_or_builtin_ironwood_time(&test_chain, "ARRR"), None);

    let mut real = json!({
        "overwinter_activation_height": 152855,
        "sapling_activation_height": 152855,
        "blossom_activation_height": null,
        "heartwood_activation_height": null,
        "canopy_activation_height": null,
        "coin_type": 133,
        "hrp_sapling_extended_spending_key": "secret-extended-key-main",
        "hrp_sapling_extended_full_viewing_key": "zxviews",
        "hrp_sapling_payment_address": "zs",
        "b58_pubkey_address_prefix": [0x1c, 0xb8],
        "b58_script_address_prefix": [0x1c, 0xbd]
    });
    let real_chain: ZcoinConsensusParams = serde_json::from_value(real.clone()).unwrap();
    assert_eq!(
        effective_ironwood_activation_time(&real_chain, "ARRR"),
        Some(ARRR_IRONWOOD_ACTIVATION_TIME)
    );

    // A configured time still applies to the look-alike chain: the withholding is of the default.
    real["sapling_activation_height"] = json!(61);
    real["ironwood_activation_time"] = json!(1_700_000_000u32);
    let configured: ZcoinConsensusParams = serde_json::from_value(real).unwrap();
    assert_eq!(
        effective_ironwood_activation_time(&configured, "ARRR"),
        Some(1_700_000_000)
    );
}

/// With version 6 supported, neither Ironwood gate engages at any time, on any coin: the swap freeze
/// and the build refusal exist only for a build that cannot construct the format.
#[test]
fn with_version_6_supported_neither_gate_engages() {
    assert!(IRONWOOD_V6_SUPPORTED);
    let activation = ARRR_IRONWOOD_ACTIVATION_TIME;
    for now in [
        0u64,
        u64::from(activation) - IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
        u64::from(activation),
        u64::MAX,
    ] {
        assert!(!ironwood_build_refused_at(Some(activation), IRONWOOD_V6_SUPPORTED, now));
        assert!(!ironwood_swap_freeze_active_at(
            Some(activation),
            IRONWOOD_V6_SUPPORTED,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            now
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// Sapling parameters needed
// ---------------------------------------------------------------------------------------------

fn sapling_prover() -> Option<LocalTxProver> {
    let dir = std::env::var_os("ARRR_SAPLING_PARAMS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::utxo::zcash_params_path);
    let (spend, output) = (dir.join(SAPLING_SPEND_NAME), dir.join(SAPLING_OUTPUT_NAME));
    if spend.exists() && output.exists() {
        Some(LocalTxProver::new(&spend, &output))
    } else {
        eprintln!(
            "SKIPPED: Sapling parameters not found in {} (set ARRR_SAPLING_PARAMS); a Sapling proof can be neither \
             made nor checked without them",
            dir.display()
        );
        None
    }
}

/// The shielded signature hash of a version-5-or-later transaction is its transaction identifier
/// (ZIP 244): the builder signs it, and a verifier recomputes it from the bytes.
fn shielded_sighash(tx: &ZTransaction) -> [u8; 32] { *tx.txid().as_ref() }

/// Verifies every Sapling proof and signature of `tx` against a sighash recomputed from the bytes.
fn sapling_bundle_verifies(tx: &ZTransaction, prover: &LocalTxProver) -> bool {
    let Some(bundle) = tx.sapling_bundle() else {
        return false;
    };
    let (spend_vk, output_vk) = prover.verifying_keys();
    let mut batch = sapling::BatchValidator::new();
    batch.check_bundle(bundle.clone(), shielded_sighash(tx)) && batch.validate(&spend_vk, &output_vk, rand::rngs::OsRng)
}

/// The conformance test with the highest value: a real post-Ironwood transaction mined by the chain,
/// whose Sapling spend-authorization and binding signatures were made by Pirate's own node over *its*
/// version-6 signature hash, must verify under the digest this library computes. The transaction has
/// two Ironwood actions, so the digest's Ironwood component is exercised too; had the library's
/// digest differed from the chain's in any byte, no signature would verify.
///
/// Limits: this transaction has no transparent inputs, so its shielded signature hash equals its
/// identifier. A transaction with transparent inputs signs a digest that also commits to the spent
/// outputs' amounts and scripts, which are not in the recording; that part is upstream's ZIP 244
/// transparent code, unchanged for version 6, and is not verified here.
#[test]
fn a_real_chain_version_6_transaction_verifies_under_the_library_signature_hash() {
    let Some(prover) = sapling_prover() else { return };
    let real = read_tx(&hex::decode(V6_HEIGHT_389_HEX).unwrap()).unwrap();
    assert_eq!(real.version(), TxVersion::V6);
    assert!(real.transparent_bundle().is_none_or(|bundle| bundle.vin.is_empty()));
    assert!(real.ironwood_bundle().is_some());
    let sapling = real.sapling_bundle().unwrap();
    assert_eq!(
        (sapling.shielded_spends().len(), sapling.shielded_outputs().len()),
        (1, 2)
    );
    assert!(
        sapling_bundle_verifies(&real, &prover),
        "the chain's own Sapling bundle must verify under the library's version-6 signature hash"
    );
}

/// A synthetic note of `value` owned by `extsk`, with its (empty-siblings) authentication path and
/// the anchor that path implies.
fn synthetic_note(extsk: &ExtendedSpendingKey, value: u64) -> (Note, sapling::MerklePath, sapling::Anchor) {
    let (_, address) = extsk.default_address();
    let note = Note::from_parts(address, NoteValue::from_raw(value), Rseed::AfterZip212([9; 32]));
    let leaf = Node::from_cmu(&note.cmu());
    let siblings = (0..sapling::NOTE_COMMITMENT_TREE_DEPTH)
        .map(|level| Node::empty_root(incrementalmerkletree::Level::from(level)))
        .collect::<Vec<_>>();
    let path = sapling::MerklePath::from_parts(siblings, incrementalmerkletree::Position::from(0u64)).unwrap();
    let anchor = path.root(leaf).into();
    (note, path, anchor)
}

/// The shape this project builds for an HTLC funding: a Sapling spend, a transparent P2SH output with
/// its redeem-script reveal in `vout[1]`, and Sapling change -- as version 6, and no Orchard or
/// Ironwood bundle. Everything is checked against what a real transaction of that wrapper looks like.
#[test]
fn a_shielded_htlc_funding_builds_as_version_6_and_verifies() {
    let Some(prover) = sapling_prover() else { return };
    let extsk = ExtendedSpendingKey::master(&[3; 32]);
    let (_, my_address) = extsk.default_address();
    let (note, path, anchor) = synthetic_note(&extsk, 200_000);
    let fee = 10_000u64;
    let htlc_value = 100_000u64;

    let redeem = vec![0x51u8; 110]; // stands in for the 110-byte redeem script
    let p2sh = zcash_transparent::address::Script(zcash_script::script::Code(
        [&[0xa9, 0x14][..], &[0x42; 20][..], &[0x87][..]].concat(),
    ));
    let reveal = zcash_transparent::address::Script(zcash_script::script::Code(
        [&[0x6a, 0x4c, redeem.len() as u8][..], &redeem[..]].concat(),
    ));

    let height = IRONWOOD_HEIGHT + 10;
    let mut builder = ZTxBuilder::new(
        params(Some(IRONWOOD_HEIGHT)),
        height.into(),
        standard_config(Some(anchor)),
    );
    let fvk = FullViewingKey::from_expanded_spending_key(&extsk.expsk);
    builder.add_sapling_spend::<Infallible>(fvk, note, path).unwrap();
    builder.add_transparent_output_raw(TxOut::new(Amount::const_from_u64(htlc_value), p2sh));
    builder.add_transparent_output_raw(TxOut::new(Amount::const_from_u64(0), reveal));
    builder
        .add_sapling_output::<Infallible>(
            None,
            my_address,
            Amount::const_from_u64(200_000 - htlc_value - fee),
            MemoBytes::empty(),
        )
        .unwrap();
    let tx = builder
        .build(
            &TransparentSigningSet::new(),
            std::slice::from_ref(&extsk),
            &[],
            rand::rngs::OsRng,
            &prover,
            &prover,
            &FixedFeeRule::non_standard(Amount::const_from_u64(fee)),
        )
        .unwrap()
        .into_transaction();
    let bytes = serialize(&tx);

    // Wrapper: header, empty Orchard slot, empty Ironwood bundle.
    assert_eq!(bytes[..12], V6_HEADER);
    assert_eq!(
        bytes[bytes.len() - 2..],
        [0, 0],
        "empty Orchard slot, empty Ironwood bundle"
    );
    assert!(tx.orchard_bundle().is_none() && tx.ironwood_bundle().is_none());
    let sapling = tx.sapling_bundle().unwrap();
    assert_eq!(
        (sapling.shielded_spends().len(), sapling.shielded_outputs().len()),
        (1, 2)
    );
    assert_eq!(
        tx.expiry_height(),
        BlockHeight::from_u32(height + zcash_primitives::transaction::builder::DEFAULT_TX_EXPIRY_DELTA)
    );

    // The real transaction has the same wrapper: same version, group, branch and the same empty tail.
    let real_bytes = hex::decode(V6_HEIGHT_68370_HEX).unwrap();
    assert_eq!(real_bytes[..12], V6_HEADER);
    assert_eq!(real_bytes[real_bytes.len() - 2..], [0, 0]);

    // Round trip, identifier, and the Sapling bundle's proofs and signatures.
    let read = read_tx(&bytes).unwrap();
    assert_eq!(serialize(&read), bytes);
    assert_eq!(txid_internal(&read), txid_internal(&tx));
    assert_ne!(txid_internal(&read), dhash256(&bytes));
    assert!(
        sapling_bundle_verifies(&read, &prover),
        "the built bundle verifies under the version-6 sighash"
    );
    // The transparent side is what the swap path reads back: output 0 pays the P2SH, output 1 reveals.
    assert_eq!(read.transparent_bundle().unwrap().vout.len(), 2);
    assert_eq!(
        u64::from(read.transparent_bundle().unwrap().vout[0].value()),
        htlc_value
    );
}
