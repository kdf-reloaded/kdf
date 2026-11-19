// utxo_common_read: reading a transaction of a version the shared UTXO parser does not build
// (CRD ch.39 D39.11.4V.3: ZIP 229 version 6 is read through the Zcash library reader).

use chain::{OpaqueTx, OutPoint, TransactionInput, TransactionOutput};
use keys::hash::H256;
use primitives::bytes::Bytes;
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus::BranchId;

use super::UtxoTx;

/// The lowest overwintered transaction version that is read by the Zcash library reader first. The shared
/// parser reads the versions below it (up to 5) and keeps them; a switch to "every version 5 and 6 read
/// goes through the library" (the literal wording of D39.11.4V.3) is a change of this one value to 5.
pub(crate) const LIBRARY_READER_FROM_VERSION: u32 = 6;

/// The version number of an overwintered transaction header, or `None` for a transaction that is not
/// overwintered (or too short to tell).
fn overwintered_version(bytes: &[u8]) -> Option<u32> {
    let header = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    (header >> 31 == 1).then_some(header & 0x7FFF_FFFF)
}

/// Whether the Zcash library reader reads these bytes before the shared parser does.
pub(crate) fn library_reads_first(bytes: &[u8]) -> bool {
    overwintered_version(bytes).map_or(false, |version| version >= LIBRARY_READER_FROM_VERSION)
}

/// Reads a transaction into the shared `UtxoTx`.
///
/// The shared parser reads everything it has a layout for, and what it refuses stays refused with its own
/// error. Only an overwintered transaction of version [`LIBRARY_READER_FROM_VERSION`] or above is read by the
/// Zcash library instead. The library result is
/// carried as an [`OpaqueTx`] (the bytes as received and the library's transaction id) next to the
/// fields call sites read: inputs, outputs, lock time and expiry height. The branch-identifier argument
/// of the library reader only matters for versions below 5, so one fixed value reads the others.
///
/// Not for a site that signs or modifies the transaction: those keep calling `deserialize`, which
/// refuses a version above 5.
pub fn read_utxo_tx(bytes: &[u8]) -> Result<UtxoTx, serialization::Error> {
    if library_reads_first(bytes) {
        return read_with_library(bytes);
    }
    serialization::deserialize::<_, UtxoTx>(bytes)
}

fn read_with_library(bytes: &[u8]) -> Result<UtxoTx, serialization::Error> {
    // The library reader stops after the transaction; the shared parser refuses bytes behind it, so does this.
    let mut rest: &[u8] = bytes;
    let ztx = ZTransaction::read(&mut rest, BranchId::Sapling)
        .map_err(|e| serialization::Error::Custom(format!("Zcash reader: {e}")))?;
    if !rest.is_empty() {
        return Err(serialization::Error::UnreadData);
    }
    let word = |range: std::ops::Range<usize>| -> Result<u32, serialization::Error> {
        let raw: [u8; 4] = bytes
            .get(range)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(serialization::Error::UnexpectedEnd)?;
        Ok(u32::from_le_bytes(raw))
    };
    let header = word(0..4)?;
    let version_group_id = word(4..8)?;
    let version = header & 0x7FFF_FFFF;

    let (inputs, outputs) = match ztx.transparent_bundle() {
        Some(bundle) => (
            bundle
                .vin
                .iter()
                .map(|input| TransactionInput {
                    previous_output: OutPoint {
                        hash: H256::from(*input.prevout().hash()),
                        index: input.prevout().n(),
                    },
                    script_sig: Bytes::from(input.script_sig().0 .0.clone()),
                    sequence: input.sequence(),
                    script_witness: vec![],
                })
                .collect(),
            bundle
                .vout
                .iter()
                .map(|output| TransactionOutput {
                    value: u64::from(output.value()),
                    script_pubkey: Bytes::from(output.script_pubkey().0 .0.clone()),
                })
                .collect(),
        ),
        None => (vec![], vec![]),
    };
    // Only versions 5 and above carry the branch id; the shared parser keeps it zero for the others.
    let consensus_branch_id = if version >= 5 {
        u32::from(ztx.consensus_branch_id())
    } else {
        0
    };

    Ok(UtxoTx {
        version: version as i32,
        overwintered: true,
        version_group_id,
        inputs,
        outputs,
        lock_time: ztx.lock_time(),
        expiry_height: u32::from(ztx.expiry_height()),
        zcash: true,
        consensus_branch_id,
        opaque: Some(OpaqueTx {
            raw: Bytes::from(bytes.to_vec()),
            txid: H256::from(*ztx.txid().as_ref()),
        }),
        ..Default::default()
    })
}

/// Library-built transactions for the tests of this module and of the history collector.
#[cfg(test)]
pub(crate) mod test_support {
    use zcash_primitives::transaction::{Authorized, Transaction as ZTransaction, TransactionData, TxVersion};
    use zcash_protocol::consensus::{BlockHeight, BranchId};
    use zcash_protocol::value::Zatoshis;
    use zcash_script::script::Code;
    use zcash_transparent::address::Script;
    use zcash_transparent::bundle::{Authorized as TransparentAuthorized, Bundle, OutPoint, TxIn, TxOut};

    /// A transparent-only transaction built with the Zcash library: version 6 on the NU7 branch, or version 5 on
    /// the NU5 branch. `prevout` is `None` for a null (coinbase-style) input.
    pub(crate) fn transparent_tx(
        version: TxVersion,
        prevout: Option<([u8; 32], u32)>,
        script_sig: Vec<u8>,
        outputs: &[(u64, Vec<u8>)],
    ) -> ZTransaction {
        let (hash, index) = prevout.unwrap_or(([0u8; 32], u32::MAX));
        let bundle = Bundle {
            vin: vec![TxIn::from_parts(
                OutPoint::new(hash, index),
                Script(Code(script_sig)),
                u32::MAX,
            )],
            vout: outputs
                .iter()
                .map(|(value, script)| TxOut::new(Zatoshis::from_u64(*value).unwrap(), Script(Code(script.clone()))))
                .collect(),
            authorization: TransparentAuthorized,
        };
        let expiry = BlockHeight::from_u32(2_000_000);
        let data = if version == TxVersion::V6 {
            TransactionData::<Authorized>::from_parts_v6(BranchId::Nu7, 0, expiry, Some(bundle), None, None, None)
        } else {
            TransactionData::<Authorized>::from_parts(version, BranchId::Nu5, 0, expiry, Some(bundle), None, None, None)
        };
        data.freeze().unwrap()
    }

    /// The bytes of [`transparent_tx`] for version 6.
    pub(crate) fn v6_transparent_tx_bytes(
        prevout: Option<([u8; 32], u32)>,
        script_sig: Vec<u8>,
        outputs: &[(u64, Vec<u8>)],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        transparent_tx(TxVersion::V6, prevout, script_sig, outputs)
            .write(&mut bytes)
            .unwrap();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{transparent_tx, v6_transparent_tx_bytes};
    use super::*;
    use crate::utxo::utxo_common::{extract_secret, extract_secret_from_script_sigs};
    use kdf_crypto::dhash160;
    use script::{Builder, Opcode, SignatureVersion, TransactionInputSigner};
    use serialization::serialize;
    use zcash_primitives::transaction::TxVersion;

    // The empty NU7 version-6 transaction and its transaction id are the ones of the zcash_primitives 0.31.1
    // test `nu7_v6_encoding_and_digests_are_feature_independent` (src/transaction/tests.rs of that crate, licensed
    // MIT OR Apache-2.0): header, no transparent, Sapling, Orchard or Ironwood parts, expiry height 1.
    const EMPTY_V6: &str = "0600008098b684d8d90a19770000000001000000000000000000";
    const EMPTY_V6_TXID: &str = "78296c68a370c2e1f058d997c81c7b011c4562c52fc5ca2994ad59cd179fbe9e";

    const P2PKH: [u8; 25] = [
        0x76, 0xa9, 0x14, 0x05, 0xaa, 0xb5, 0x34, 0x21, 0x66, 0xf8, 0x59, 0x4b, 0xaf, 0x17, 0xa7, 0xd9, 0xbe, 0xf5,
        0xd5, 0x67, 0x44, 0x33, 0x27, 0x88, 0xac,
    ];

    #[test]
    fn the_threshold_routes_version_6_to_the_library_and_nothing_below() {
        let header = |version: u32, overwintered: bool| {
            let h = if overwintered { version | (1 << 31) } else { version };
            h.to_le_bytes().to_vec()
        };
        assert!(library_reads_first(&header(6, true)));
        assert!(library_reads_first(&header(7, true)));
        assert!(!library_reads_first(&header(5, true)));
        assert!(!library_reads_first(&header(4, true)));
        assert!(!library_reads_first(&header(6, false)));
        assert!(!library_reads_first(&[0x06, 0x00]));
    }

    #[test]
    fn a_malformed_version_5_is_refused_with_the_shared_error_and_not_retried() {
        let valid = transparent_tx(TxVersion::V5, Some(([9; 32], 0)), vec![0x51], &[(1, P2PKH.to_vec())]);
        let mut bytes = Vec::new();
        valid.write(&mut bytes).unwrap();
        bytes.truncate(bytes.len() - 3);
        let shared = serialization::deserialize::<_, UtxoTx>(bytes.as_slice()).unwrap_err();
        assert_eq!(read_utxo_tx(&bytes), Err(shared));
        // Version 4 behaves the same way.
        let v4 = [&[0x04, 0, 0, 0x80][..], &[0x85, 0x20, 0x2f, 0x89, 0x01]].concat();
        assert_eq!(
            read_utxo_tx(&v4),
            serialization::deserialize::<_, UtxoTx>(v4.as_slice())
        );
    }

    #[test]
    fn the_empty_version_6_vector_is_read_with_the_library_txid_and_its_own_bytes() {
        let bytes = hex::decode(EMPTY_V6).unwrap();
        let tx = read_utxo_tx(&bytes).expect("a version 6 transaction is read");
        assert_eq!((tx.version, tx.overwintered, tx.expiry_height), (6, true, 1));
        assert_eq!(tx.version_group_id, 0xD884_B698);
        assert_eq!(tx.consensus_branch_id, 0x7719_0ad9);
        assert!(tx.inputs.is_empty() && tx.outputs.is_empty());
        assert_eq!(hex::encode(tx.hash().take()), EMPTY_V6_TXID);
        assert_eq!(serialize(&tx).take(), bytes);
        // The shared parser alone still refuses it.
        assert!(serialization::deserialize::<_, UtxoTx>(bytes.as_slice()).is_err());
    }

    #[test]
    fn bytes_behind_a_version_6_transaction_and_a_cut_off_one_are_refused() {
        let mut bytes = hex::decode(EMPTY_V6).unwrap();
        for end in 0..bytes.len() {
            assert!(read_utxo_tx(&bytes[..end]).is_err(), "cut at {end}");
        }
        bytes.push(0);
        assert_eq!(read_utxo_tx(&bytes), Err(serialization::Error::UnreadData));
    }

    #[test]
    fn a_version_6_payment_is_read_for_validation_history_and_spend_detection() {
        let secret = [0x5au8; 32];
        let secret_hash = dhash160(&secret);
        let p2sh = {
            let mut script = vec![0xa9, 0x14];
            script.extend_from_slice(&[0x11; 20]);
            script.push(0x87);
            script
        };
        // A payment: one output to a script hash, one change output.
        let payment = v6_transparent_tx_bytes(Some(([7; 32], 1)), vec![0x51], &[
            (100_000, p2sh.clone()),
            (5_000, P2PKH.to_vec()),
        ]);
        let tx = read_utxo_tx(&payment).unwrap();
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(
            (tx.outputs[0].value, tx.outputs[0].script_pubkey.to_vec()),
            (100_000, p2sh)
        );
        assert_eq!(tx.inputs[0].previous_output.index, 1);
        assert_eq!(tx.expiry_height, 2_000_000);
        // What `validate_payment` compares with the bytes the backend returns, and the id it looks up.
        assert_eq!(serialize(&tx).take(), payment);
        let library = zcash_primitives::transaction::Transaction::read(payment.as_slice(), BranchId::Sapling).unwrap();
        assert_eq!(tx.hash().take(), *library.txid().as_ref());

        // A spend of it: the secret path of the swap input script reveals the secret.
        let script_sig = Builder::default()
            .push_data(&[0x30; 71])
            .push_data(&secret)
            .push_opcode(Opcode::OP_0)
            .push_data(&[0xac])
            .into_script()
            .to_bytes()
            .to_vec();
        let spend = v6_transparent_tx_bytes(Some((tx.hash().take(), 0)), script_sig, &[(90_000, P2PKH.to_vec())]);
        assert_eq!(extract_secret(&*secret_hash, &spend), Ok(secret.to_vec()));
        let spend_tx = read_utxo_tx(&spend).unwrap();
        assert_eq!(spend_tx.inputs[0].previous_output.hash, tx.hash());
        let scripts = spend_tx.inputs.iter().map(|input| input.script_sig.clone().into());
        assert_eq!(
            extract_secret_from_script_sigs(&*secret_hash, scripts),
            Ok(secret.to_vec())
        );
    }

    /// Version 5 stays on the shared parser; both readers agree on the transaction id and on the bytes, for the
    /// ZIP 244 vectors (see the header of that file for source and license) and for a library-built transaction.
    #[test]
    fn version_5_reads_the_same_under_both_readers() {
        let agree = |bytes: &[u8], what: &str| {
            let shared: UtxoTx = serialization::deserialize(bytes).unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert!(shared.opaque.is_none(), "{what}: the shared parser reads version 5");
            let library = zcash_primitives::transaction::Transaction::read(bytes, BranchId::Sapling)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(shared.hash().take(), *library.txid().as_ref(), "{what}: transaction id");
            let mut written = Vec::new();
            library.write(&mut written).unwrap();
            assert_eq!(serialize(&shared).take(), written, "{what}: bytes");
            // The entry point does not hand version 5 to the library.
            assert_eq!(read_utxo_tx(bytes).unwrap(), shared, "{what}: read_utxo_tx");
        };

        let vectors = include_str!("../../../kdf_chain/src/test_data/zip_0244_vectors.txt");
        let mut count = 0;
        for line in vectors.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let tx_hex = line.split(' ').next().unwrap();
            agree(&hex::decode(tx_hex).unwrap(), &format!("vector {count}"));
            count += 1;
        }
        assert_eq!(count, 10);

        let mut built = Vec::new();
        transparent_tx(TxVersion::V5, Some(([9; 32], 0)), vec![0x51], &[(
            12_345,
            P2PKH.to_vec(),
        )])
        .write(&mut built)
        .unwrap();
        agree(&built, "library-built");
    }

    /// A transaction read by the library is never signed: the signer refuses it by its own error.
    #[test]
    fn the_signer_refuses_a_transaction_the_library_read() {
        let bytes = v6_transparent_tx_bytes(Some(([7; 32], 0)), vec![0x51], &[(1_000, P2PKH.to_vec())]);
        let tx = read_utxo_tx(&bytes).unwrap();
        let signer = TransactionInputSigner::from_transaction(tx, 0);
        let result = signer.signature_hash(
            0,
            1_000,
            &script::Script::from(Bytes::default()),
            SignatureVersion::Base,
            1,
        );
        // The refusal is explicit, before and independent of the version check.
        assert_eq!(result, Err(script::SignatureHashError::ReadByLibrary));
    }
}
