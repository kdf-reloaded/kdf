//! ZIP 244 digests of a version 5 transaction (ZIP 225 layout).
//!
//! Written against the public ZIP 244 text and checked against the ZIP 244 test vectors. Every
//! digest is a BLAKE2b-256 hash with a 16-byte personalization; a part that has no elements hashes
//! the empty byte string under its own personalization. The transaction id and the signature
//! digest share the header, Sapling and Orchard digests and the final combination, so the signer
//! (`kdf_script`) takes them from here and only builds the transparent signature digest itself.
//!
//! The consensus branch id is always an argument, never a constant of this module.

use crate::transaction::{OrchardBundle, OutPoint, SaplingBundleV5, Transaction, TransactionOutput};
use blake2b_simd::Params;
use primitives::hash::H256;
use serialization::Stream;

/// The overwintered flag in the header word (ZIP 225).
const OVERWINTERED_FLAG: u32 = 1 << 31;

const PERS_HEADER: &[u8; 16] = b"ZTxIdHeadersHash";
const PERS_TRANSPARENT: &[u8; 16] = b"ZTxIdTranspaHash";
const PERS_PREVOUTS: &[u8; 16] = b"ZTxIdPrevoutHash";
const PERS_SEQUENCE: &[u8; 16] = b"ZTxIdSequencHash";
const PERS_OUTPUTS: &[u8; 16] = b"ZTxIdOutputsHash";
const PERS_SAPLING: &[u8; 16] = b"ZTxIdSaplingHash";
const PERS_SAPLING_SPENDS: &[u8; 16] = b"ZTxIdSSpendsHash";
const PERS_SAPLING_SPENDS_COMPACT: &[u8; 16] = b"ZTxIdSSpendCHash";
const PERS_SAPLING_SPENDS_NONCOMPACT: &[u8; 16] = b"ZTxIdSSpendNHash";
const PERS_SAPLING_OUTPUTS: &[u8; 16] = b"ZTxIdSOutputHash";
const PERS_SAPLING_OUTPUTS_COMPACT: &[u8; 16] = b"ZTxIdSOutC__Hash";
const PERS_SAPLING_OUTPUTS_MEMOS: &[u8; 16] = b"ZTxIdSOutM__Hash";
const PERS_SAPLING_OUTPUTS_NONCOMPACT: &[u8; 16] = b"ZTxIdSOutN__Hash";
const PERS_ORCHARD: &[u8; 16] = b"ZTxIdOrchardHash";
const PERS_ORCHARD_COMPACT: &[u8; 16] = b"ZTxIdOrcActCHash";
const PERS_ORCHARD_MEMOS: &[u8; 16] = b"ZTxIdOrcActMHash";
const PERS_ORCHARD_NONCOMPACT: &[u8; 16] = b"ZTxIdOrcActNHash";
/// First 12 bytes of the personalization of the root digest; the consensus branch id, little-endian,
/// completes it. The transaction id and the signature digest use the same one.
const PERS_ROOT_PREFIX: &[u8; 12] = b"ZcashTxHash_";

/// Offsets inside the 580-byte `encCiphertext` (ZIP 244 T.3b, T.4a-c): the first 52 bytes are the
/// compact part, then 512 bytes of memo, then the rest (the AEAD tag).
const ENC_COMPACT_LEN: usize = 52;
const ENC_MEMO_END: usize = 564;

/// BLAKE2b-256 of `data` under the 16-byte `personal`.
fn blake2b_256(personal: &[u8; 16], data: &[u8]) -> H256 {
    let hash = Params::new()
        .hash_length(32)
        .personal(personal)
        .to_state()
        .update(data)
        .finalize();
    H256::from(hash.as_bytes())
}

/// ZIP 244 T.1: the header digest. `header` is the version with the overwintered flag, as written
/// on the wire (`version | 1 << 31`).
pub fn header_digest(
    header: u32,
    version_group_id: u32,
    consensus_branch_id: u32,
    lock_time: u32,
    expiry_height: u32,
) -> H256 {
    let mut stream = Stream::default();
    stream
        .append(&header)
        .append(&version_group_id)
        .append(&consensus_branch_id)
        .append(&lock_time)
        .append(&expiry_height);
    blake2b_256(PERS_HEADER, &stream.out())
}

/// The header word of a version `version` transaction: the version with the overwintered flag.
pub fn header_word(version: i32) -> u32 { (version as u32) | OVERWINTERED_FLAG }

/// ZIP 244 T.2a: the digest of all outpoints. The empty list gives the digest of no data.
pub fn prevouts_digest(prevouts: &[OutPoint]) -> H256 {
    let mut stream = Stream::default();
    for prevout in prevouts {
        stream.append(prevout);
    }
    blake2b_256(PERS_PREVOUTS, &stream.out())
}

/// ZIP 244 T.2b: the digest of all input sequence numbers. The empty list gives the digest of no
/// data.
pub fn sequence_digest(sequences: &[u32]) -> H256 {
    let mut stream = Stream::default();
    for sequence in sequences {
        stream.append(sequence);
    }
    blake2b_256(PERS_SEQUENCE, &stream.out())
}

/// ZIP 244 T.2c: the digest of all transparent outputs (amount, then the script with its length
/// prefix, which is the output's wire encoding). The empty list gives the digest of no data.
pub fn outputs_digest(outputs: &[TransactionOutput]) -> H256 {
    let mut stream = Stream::default();
    for output in outputs {
        stream.append(output);
    }
    blake2b_256(PERS_OUTPUTS, &stream.out())
}

/// ZIP 244 T.2: the transparent digest. A transaction with neither inputs nor outputs has the
/// digest of no data (not the digest of three empty sub-digests).
pub fn transparent_digest(prevouts: &[OutPoint], sequences: &[u32], outputs: &[TransactionOutput]) -> H256 {
    if prevouts.is_empty() && outputs.is_empty() {
        return blake2b_256(PERS_TRANSPARENT, &[]);
    }
    let mut stream = Stream::default();
    stream
        .append(&prevouts_digest(prevouts))
        .append(&sequence_digest(sequences))
        .append(&outputs_digest(outputs));
    blake2b_256(PERS_TRANSPARENT, &stream.out())
}

/// ZIP 244 T.3: the Sapling digest. Without spends and outputs it is the digest of no data.
pub fn sapling_digest(bundle: &SaplingBundleV5) -> H256 {
    if !bundle.is_present() {
        return blake2b_256(PERS_SAPLING, &[]);
    }

    let spends = if bundle.spends.is_empty() {
        blake2b_256(PERS_SAPLING_SPENDS, &[])
    } else {
        let mut compact = Stream::default();
        let mut noncompact = Stream::default();
        for spend in &bundle.spends {
            compact.append(&spend.nullifier);
            // The anchor is shared by all spends and hashed with each of them (T.3a.ii).
            noncompact.append(&spend.cv).append(&bundle.anchor).append(&spend.rk);
        }
        let mut stream = Stream::default();
        stream
            .append(&blake2b_256(PERS_SAPLING_SPENDS_COMPACT, &compact.out()))
            .append(&blake2b_256(PERS_SAPLING_SPENDS_NONCOMPACT, &noncompact.out()));
        blake2b_256(PERS_SAPLING_SPENDS, &stream.out())
    };

    let outputs = if bundle.outputs.is_empty() {
        blake2b_256(PERS_SAPLING_OUTPUTS, &[])
    } else {
        let mut compact = Stream::default();
        let mut memos = Stream::default();
        let mut noncompact = Stream::default();
        for output in &bundle.outputs {
            let enc = output.enc_cipher_text.as_slice();
            compact
                .append(&output.cmu)
                .append(&output.ephemeral_key)
                .append_slice(&enc[..ENC_COMPACT_LEN]);
            memos.append_slice(&enc[ENC_COMPACT_LEN..ENC_MEMO_END]);
            noncompact
                .append(&output.cv)
                .append_slice(&enc[ENC_MEMO_END..])
                .append(&output.out_cipher_text);
        }
        let mut stream = Stream::default();
        stream
            .append(&blake2b_256(PERS_SAPLING_OUTPUTS_COMPACT, &compact.out()))
            .append(&blake2b_256(PERS_SAPLING_OUTPUTS_MEMOS, &memos.out()))
            .append(&blake2b_256(PERS_SAPLING_OUTPUTS_NONCOMPACT, &noncompact.out()));
        blake2b_256(PERS_SAPLING_OUTPUTS, &stream.out())
    };

    let mut stream = Stream::default();
    stream.append(&spends).append(&outputs).append(&bundle.value_balance);
    blake2b_256(PERS_SAPLING, &stream.out())
}

/// ZIP 244 T.4: the Orchard digest. Without actions it is the digest of no data.
pub fn orchard_digest(bundle: &OrchardBundle) -> H256 {
    if !bundle.is_present() {
        return blake2b_256(PERS_ORCHARD, &[]);
    }

    let mut compact = Stream::default();
    let mut memos = Stream::default();
    let mut noncompact = Stream::default();
    for action in &bundle.actions {
        let enc = action.enc_cipher_text.as_slice();
        compact
            .append(&action.nullifier)
            .append(&action.cmx)
            .append(&action.ephemeral_key)
            .append_slice(&enc[..ENC_COMPACT_LEN]);
        memos.append_slice(&enc[ENC_COMPACT_LEN..ENC_MEMO_END]);
        noncompact
            .append(&action.cv)
            .append(&action.rk)
            .append_slice(&enc[ENC_MEMO_END..])
            .append(&action.out_cipher_text);
    }

    let mut stream = Stream::default();
    stream
        .append(&blake2b_256(PERS_ORCHARD_COMPACT, &compact.out()))
        .append(&blake2b_256(PERS_ORCHARD_MEMOS, &memos.out()))
        .append(&blake2b_256(PERS_ORCHARD_NONCOMPACT, &noncompact.out()))
        .append(&bundle.flags)
        .append(&bundle.value_balance)
        .append(&bundle.anchor);
    blake2b_256(PERS_ORCHARD, &stream.out())
}

/// The top digest, personalized with the consensus branch id: the transaction id when `transparent`
/// is the transparent digest (T.2), the signature digest when it is the transparent signature
/// digest (S.2). The two share this combination (ZIP 244 T and S).
pub fn root_digest(
    consensus_branch_id: u32,
    header: &H256,
    transparent: &H256,
    sapling: &H256,
    orchard: &H256,
) -> H256 {
    let mut personal = [0u8; 16];
    personal[..12].copy_from_slice(PERS_ROOT_PREFIX);
    personal[12..].copy_from_slice(&consensus_branch_id.to_le_bytes());

    let mut stream = Stream::default();
    stream
        .append(header)
        .append(transparent)
        .append(sapling)
        .append(orchard);
    blake2b_256(&personal, &stream.out())
}

/// The ZIP 244 transaction id of a version 5 transaction, in digest byte order. The branch id is the
/// transaction's own `consensus_branch_id`.
pub fn txid(tx: &Transaction) -> H256 {
    let prevouts: Vec<OutPoint> = tx.inputs.iter().map(|input| input.previous_output).collect();
    let sequences: Vec<u32> = tx.inputs.iter().map(|input| input.sequence).collect();
    let header = header_digest(
        header_word(tx.version),
        tx.version_group_id,
        tx.consensus_branch_id,
        tx.lock_time,
        tx.expiry_height,
    );
    root_digest(
        tx.consensus_branch_id,
        &header,
        &transparent_digest(&prevouts, &sequences, &tx.outputs),
        &sapling_digest(&tx.sapling_v5),
        &orchard_digest(&tx.orchard),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{TransactionInput, TxHashAlgo};
    use crypto::dhash256;
    use hex::FromHex;
    use primitives::bytes::Bytes;
    use serialization::{deserialize, serialize};

    /// Consensus branch id of NU5, which the ZIP 244 vectors carry.
    const NU5_BRANCH_ID: u32 = 0xc2d6_d0b4;
    /// Consensus branch id and version group id of NU7 (ZIP 2003, ZIP 259).
    const NU7_BRANCH_ID: u32 = 0x7719_0ad9;
    const NU7_VERSION_GROUP_ID: u32 = 0x26a7_270a;

    /// The raw transaction and its ZIP 244 transaction id from one line of the vectors file (see the
    /// file header for the source and the license).
    struct Vector {
        tx: Vec<u8>,
        txid: H256,
    }

    fn vectors() -> Vec<Vector> {
        include_str!("test_data/zip_0244_vectors.txt")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.split(' ');
                let tx = fields.next().expect("a transaction field");
                let txid = fields.next().expect("a txid field");
                Vector {
                    tx: tx.from_hex().expect("the transaction is hex"),
                    txid: txid.parse().expect("a txid is 64 hex digits"),
                }
            })
            .collect()
    }

    #[test]
    fn vector_transactions_round_trip_and_have_the_vector_txid() {
        let vectors = vectors();
        assert_eq!(vectors.len(), 10);

        let (mut with_spends, mut with_outputs, mut with_actions) = (0, 0, 0);
        for (n, vector) in vectors.iter().enumerate() {
            let tx: Transaction = deserialize(vector.tx.as_slice()).expect("a vector transaction parses");
            assert!(tx.is_v5(), "vector {n}: version 5 layout");
            assert_eq!(
                tx.consensus_branch_id, NU5_BRANCH_ID,
                "vector {n}: the vector's own branch id"
            );
            assert_eq!(
                serialize(&tx).to_vec(),
                vector.tx,
                "vector {n}: serialization must reproduce the parsed bytes"
            );
            assert_eq!(tx.hash(), vector.txid, "vector {n}: the ZIP 244 transaction id");
            assert_eq!(
                txid(&tx),
                vector.txid,
                "vector {n}: the digest function gives the same id"
            );

            with_spends += usize::from(!tx.sapling_v5.spends.is_empty());
            with_outputs += usize::from(!tx.sapling_v5.outputs.is_empty());
            with_actions += usize::from(tx.orchard.is_present());
        }
        // The vectors exercise every bundle.
        assert!(with_spends > 0 && with_outputs > 0 && with_actions > 0);
    }

    fn transparent_only(branch_id: u32) -> Transaction {
        Transaction {
            version: 5,
            overwintered: true,
            zcash: true,
            version_group_id: NU7_VERSION_GROUP_ID,
            consensus_branch_id: branch_id,
            inputs: vec![TransactionInput {
                previous_output: OutPoint {
                    hash: H256::from([7u8; 32]),
                    index: 1,
                },
                script_sig: Bytes::from(vec![0x51]),
                sequence: 0xffff_fffe,
                script_witness: vec![],
            }],
            outputs: vec![
                TransactionOutput {
                    value: 12_345,
                    script_pubkey: Bytes::from(vec![0x76, 0xa9]),
                },
                TransactionOutput {
                    value: 1,
                    script_pubkey: Bytes::from(vec![0x6a]),
                },
            ],
            lock_time: 500,
            expiry_height: 2_000,
            ..Transaction::default()
        }
    }

    #[test]
    fn transparent_only_nu7_transaction_round_trips() {
        let tx = transparent_only(NU7_BRANCH_ID);
        let bytes = serialize(&tx);

        // Header with the overwintered flag, version group id, consensus branch id: little-endian.
        assert_eq!(&bytes[..12], &[
            0x05, 0x00, 0x00, 0x80, 0x0a, 0x27, 0xa7, 0x26, 0xd9, 0x0a, 0x19, 0x77
        ]);
        // No Sapling spends, no Sapling outputs, no Orchard actions: three zero counts at the end.
        assert_eq!(&bytes[bytes.len() - 3..], &[0x00, 0x00, 0x00]);

        let decoded: Transaction = deserialize(&bytes[..]).expect("the transaction parses");
        assert_eq!(decoded, tx);
        assert_eq!(decoded.consensus_branch_id, NU7_BRANCH_ID);
        assert_eq!(serialize(&decoded), bytes);
    }

    #[test]
    fn transaction_id_is_zip244_and_personalized_by_the_branch_id() {
        let tx = transparent_only(NU7_BRANCH_ID);
        assert_ne!(
            tx.hash(),
            dhash256(&serialize(&tx)),
            "not the double SHA-256 of the bytes"
        );
        assert_ne!(
            tx.hash(),
            transparent_only(NU5_BRANCH_ID).hash(),
            "the branch id personalizes the id"
        );

        // ZIP 244 leaves the scriptSigs out of the id (they are authorizing data).
        let mut other_script_sig = tx.clone();
        other_script_sig.inputs[0].script_sig = Bytes::from(vec![0x52, 0x53]);
        assert_eq!(tx.hash(), other_script_sig.hash());

        let mut other_lock_time = tx.clone();
        other_lock_time.lock_time += 1;
        assert_ne!(tx.hash(), other_lock_time.hash());
    }

    #[test]
    fn older_versions_keep_the_double_sha256_id() {
        let tx = Transaction {
            version: 4,
            overwintered: true,
            zcash: true,
            version_group_id: 0x892f_2085,
            ..Transaction::default()
        };
        assert!(!tx.is_v5());
        assert_eq!(tx.hash(), dhash256(&serialize(&tx)));

        // Version 5 without the overwintered flag is not the ZIP 225 layout.
        let plain = Transaction {
            version: 5,
            tx_hash_algo: TxHashAlgo::DSHA256,
            ..Transaction::default()
        };
        assert!(!plain.is_v5());
        assert_eq!(&serialize(&plain)[..4], &[0x05, 0x00, 0x00, 0x00]);
        assert_eq!(plain.hash(), dhash256(&serialize(&plain)));
    }

    /// The version 5 bytes up to the transparent bundle, for hand-made hostile inputs.
    fn v5_prefix() -> Vec<u8> {
        let mut bytes = serialize(&transparent_only(NU7_BRANCH_ID)).to_vec();
        // Cut after the expiry height (20 bytes): the transparent and shielded counts follow.
        bytes.truncate(20);
        bytes
    }

    #[test]
    fn hostile_counts_and_lengths_end_in_an_error() {
        // Transparent counts 0 and 0, Sapling counts 0 and 0, then an Orchard action count of 9000.
        let mut bytes = v5_prefix();
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0xfd, 0x28, 0x23]);
        assert!(deserialize::<_, Transaction>(&bytes[..]).is_err());

        // One Orchard action whose proof length claims a million bytes that are not there.
        let mut bytes = v5_prefix();
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(&[0u8; 5 * 32 + 580 + 80]); // the action
        bytes.push(0x03); // flags
        bytes.extend_from_slice(&[0u8; 8 + 32]); // value balance, anchor
        bytes.extend_from_slice(&[0xfe, 0x40, 0x42, 0x0f, 0x00]); // proof length 1_000_000
        bytes.extend_from_slice(&[0u8; 10]);
        assert!(deserialize::<_, Transaction>(&bytes[..]).is_err());
    }
}
