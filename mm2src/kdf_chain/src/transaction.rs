//! KDF transaction primitives covering the full multi-coin matrix.
//!
//! Wire format reference:
//!   * Bitcoin standard (with optional SegWit witness marker / flag)
//!   * Zcash overwintered (v3, v4 Sapling) — `JoinSplit` PHGR/Groth, shielded
//!     spends/outputs, `binding_sig`
//!   * PoS-style coins with `nTime` (PPC, BLK, …)
//!   * NavCoin `str_d_zeel` trailer
//!
//! The exact byte layout produced by `Serializable` and accepted by
//! `Deserializable` is the cross-compat contract validated by
//! `mm2_bitcoin_wire_tests`. Field order MUST NOT change.

use crypto::{dhash256, sha256};
use hex::FromHex;
use primitives::bytes::Bytes;
use primitives::hash::{CipherText, EncCipherText, OutCipherText, ZkProof, ZkProofSapling, H256, H512, H64};
use serialization::{deserialize, serialize, serialize_with_flags, CompactInteger, Deserializable, Error, Reader,
                    Serializable, Stream, SERIALIZE_TRANSACTION_WITNESS};
use std::io;
use std::io::Read;

use crate::constants_::{LOCKTIME_THRESHOLD, SEQUENCE_FINAL};

/// SegWit witness marker byte. Constant 0 per BIP-141.
const WITNESS_MARKER: u8 = 0;
/// SegWit witness flag byte. Constant 1 per BIP-141.
const WITNESS_FLAG: u8 = 1;
/// Hard cap on list lengths during deserialization to avoid hostile peer DoS.
const MAX_LIST_SIZE: usize = 8192;

/// Reference to a previous transaction output.
#[derive(Clone, Copy, Debug, Default, Deserializable, Eq, Hash, PartialEq, Serializable)]
pub struct OutPoint {
    pub hash: H256,
    pub index: u32,
}

impl OutPoint {
    /// Coinbase sentinel: zero hash, `u32::MAX` index.
    pub fn null() -> Self {
        OutPoint {
            hash: H256::default(),
            index: u32::MAX,
        }
    }

    pub fn is_null(&self) -> bool { self.hash.is_zero() && self.index == u32::MAX }
}

#[derive(Debug, Default, PartialEq, Clone)]
pub struct TransactionInput {
    pub previous_output: OutPoint,
    pub script_sig: Bytes,
    pub sequence: u32,
    /// SegWit witness stack. Serialized OUTSIDE of `Serializable for
    /// TransactionInput`: see `Serializable for Transaction` for the location.
    pub script_witness: Vec<Bytes>,
}

impl TransactionInput {
    pub fn coinbase(script_sig: Bytes) -> Self {
        TransactionInput {
            previous_output: OutPoint::null(),
            script_sig,
            sequence: SEQUENCE_FINAL,
            script_witness: vec![],
        }
    }

    pub fn is_final(&self) -> bool { self.sequence == SEQUENCE_FINAL }

    pub fn has_witness(&self) -> bool { !self.script_witness.is_empty() }
}

impl Serializable for TransactionInput {
    fn serialize(&self, stream: &mut Stream) {
        stream
            .append(&self.previous_output)
            .append(&self.script_sig)
            .append(&self.sequence);
    }
}

impl Deserializable for TransactionInput {
    fn deserialize<T>(reader: &mut Reader<T>) -> Result<Self, Error>
    where
        Self: Sized,
        T: io::Read,
    {
        Ok(TransactionInput {
            previous_output: reader.read()?,
            script_sig: reader.read()?,
            sequence: reader.read()?,
            script_witness: vec![],
        })
    }
}

#[derive(Debug, PartialEq, Clone, Serializable, Deserializable)]
pub struct TransactionOutput {
    pub value: u64,
    pub script_pubkey: Bytes,
}

impl Default for TransactionOutput {
    fn default() -> Self {
        // u64::MAX is the canonical "uninitialized" amount used by signing code
        // (matches the previous parity-derived behaviour).
        TransactionOutput {
            value: u64::MAX,
            script_pubkey: Bytes::default(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Serializable, Deserializable)]
pub struct ShieldedSpend {
    pub cv: H256,
    pub anchor: H256,
    pub nullifier: H256,
    pub rk: H256,
    pub zkproof: ZkProofSapling,
    pub spend_auth_sig: H512,
}

#[derive(Debug, PartialEq, Clone, Serializable, Deserializable)]
pub struct ShieldedOutput {
    pub cv: H256,
    pub cmu: H256,
    pub ephemeral_key: H256,
    pub enc_cipher_text: EncCipherText,
    pub out_cipher_text: OutCipherText,
    pub zkproof: ZkProofSapling,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, PartialEq, Clone)]
pub enum JoinSplitProof {
    PHGR(ZkProof),
    Groth(ZkProofSapling),
}

impl Serializable for JoinSplitProof {
    fn serialize(&self, stream: &mut Stream) {
        match self {
            JoinSplitProof::PHGR(p) => stream.append(p),
            JoinSplitProof::Groth(p) => stream.append(p),
        };
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct JoinSplit {
    pub v_pub_old: H64,
    pub v_pub_new: H64,
    pub anchor: H256,
    pub nullifiers: [H256; 2],
    pub commitments: [H256; 2],
    pub ephemeral_key: H256,
    pub random_seed: H256,
    pub macs: [H256; 2],
    pub zkproof: JoinSplitProof,
    pub ciphertexts: [CipherText; 2],
}

impl Serializable for JoinSplit {
    fn serialize(&self, stream: &mut Stream) {
        // Fixed-size arrays are not supported by the Serializable derive, so
        // each element is emitted explicitly. The order is wire-critical.
        stream
            .append(&self.v_pub_old)
            .append(&self.v_pub_new)
            .append(&self.anchor)
            .append(&self.nullifiers[0])
            .append(&self.nullifiers[1])
            .append(&self.commitments[0])
            .append(&self.commitments[1])
            .append(&self.ephemeral_key)
            .append(&self.random_seed)
            .append(&self.macs[0])
            .append(&self.macs[1])
            .append(&self.zkproof)
            .append(&self.ciphertexts[0])
            .append(&self.ciphertexts[1]);
    }
}

fn deserialize_join_split<T: io::Read>(reader: &mut Reader<T>, use_groth: bool) -> Result<JoinSplit, Error> {
    Ok(JoinSplit {
        v_pub_old: reader.read()?,
        v_pub_new: reader.read()?,
        anchor: reader.read()?,
        nullifiers: [reader.read()?, reader.read()?],
        commitments: [reader.read()?, reader.read()?],
        ephemeral_key: reader.read()?,
        random_seed: reader.read()?,
        macs: [reader.read()?, reader.read()?],
        zkproof: if use_groth {
            JoinSplitProof::Groth(reader.read()?)
        } else {
            JoinSplitProof::PHGR(reader.read()?)
        },
        ciphertexts: [reader.read()?, reader.read()?],
    })
}

/// Hash algorithm used for the txid (most chains use double-SHA256; some
/// PoW-shielded chains use single SHA-256).
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum TxHashAlgo {
    #[default]
    DSHA256,
    SHA256,
}

/// Universal transaction record covering Bitcoin, Zcash overwintered, PoS
/// nTime variants, and NavCoin str_d_zeel.
///
/// Field defaults (`Default`) produce an empty Bitcoin-style transaction
/// (`version=0`, no witnesses, no zcash). The `Serializable`/`Deserializable`
/// impls below switch behaviour based on `overwintered`, `n_time.is_some()`,
/// `zcash`, and the hash-witness state.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct Transaction {
    pub version: i32,
    pub n_time: Option<u32>,
    pub overwintered: bool,
    pub version_group_id: u32,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub lock_time: u32,
    pub expiry_height: u32,
    pub shielded_spends: Vec<ShieldedSpend>,
    pub shielded_outputs: Vec<ShieldedOutput>,
    pub join_splits: Vec<JoinSplit>,
    pub value_balance: i64,
    pub join_split_pubkey: H256,
    pub join_split_sig: H512,
    pub binding_sig: H512,
    pub zcash: bool,
    /// NavCoin trailer: <https://github.com/navcoin/navcoin-core/blob/556250920fef9dc3eddd28996329ba316de5f909/src/primitives/transaction.h#L497>
    pub str_d_zeel: Option<String>,
    pub tx_hash_algo: TxHashAlgo,
}

impl From<&'static str> for Transaction {
    fn from(s: &'static str) -> Self {
        let bytes: Vec<u8> = s.from_hex().expect("valid hex");
        deserialize(bytes.as_slice()).expect("valid tx hex")
    }
}

impl Transaction {
    /// Computes the txid using `tx_hash_algo` over the witness-stripped
    /// serialization.
    pub fn hash(&self) -> H256 {
        let bytes = serialize(self);
        match self.tx_hash_algo {
            TxHashAlgo::DSHA256 => dhash256(&bytes),
            TxHashAlgo::SHA256 => sha256(&bytes),
        }
    }

    /// Computes the wtxid (DSHA-256 of the SegWit-with-witness serialization).
    pub fn witness_hash(&self) -> H256 { dhash256(&serialize_with_flags(self, SERIALIZE_TRANSACTION_WITNESS)) }

    pub fn inputs(&self) -> &[TransactionInput] { &self.inputs }

    pub fn outputs(&self) -> &[TransactionOutput] { &self.outputs }

    pub fn is_empty(&self) -> bool { self.inputs.is_empty() || self.outputs.is_empty() }

    pub fn is_null(&self) -> bool { self.inputs.iter().any(|input| input.previous_output.is_null()) }

    pub fn is_coinbase(&self) -> bool { self.inputs.len() == 1 && self.inputs[0].previous_output.is_null() }

    pub fn is_final(&self) -> bool {
        // Locktime of 0 means always final; otherwise at least one input must
        // have a non-final sequence for the locktime to actually engage.
        self.lock_time == 0 || self.inputs.iter().all(TransactionInput::is_final)
    }

    pub fn is_final_in_block(&self, block_height: u32, block_time: u32) -> bool {
        if self.lock_time == 0 {
            return true;
        }
        let max_lock_time = if self.lock_time < LOCKTIME_THRESHOLD {
            block_height
        } else {
            block_time
        };
        if self.lock_time < max_lock_time {
            return true;
        }
        self.inputs.iter().all(TransactionInput::is_final)
    }

    pub fn has_witness(&self) -> bool { self.inputs.iter().any(TransactionInput::has_witness) }

    /// Saturating sum of all output amounts.
    pub fn total_spends(&self) -> u64 {
        let mut sum = 0u64;
        for output in &self.outputs {
            if u64::MAX - sum < output.value {
                return u64::MAX;
            }
            sum += output.value;
        }
        sum
    }
}

impl Serializable for Transaction {
    fn serialize(&self, stream: &mut Stream) {
        let with_witness = stream.include_transaction_witness() && self.has_witness();
        if with_witness {
            // BIP-141 SegWit envelope.
            stream
                .append(&self.version)
                .append(&WITNESS_MARKER)
                .append(&WITNESS_FLAG)
                .append_list(&self.inputs)
                .append_list(&self.outputs);
            for input in &self.inputs {
                stream.append_list(&input.script_witness);
            }
            stream.append(&self.lock_time);
            return;
        }

        // Witness-stripped path, with all coin-specific extensions in their
        // canonical positions.
        let mut header = self.version;
        if self.overwintered {
            header |= 1 << 31;
        }
        stream.append(&header);

        if self.overwintered {
            stream.append(&self.version_group_id);
        }
        if let Some(n_time) = self.n_time {
            stream.append(&n_time);
        }

        stream
            .append_list(&self.inputs)
            .append_list(&self.outputs)
            .append(&self.lock_time);

        if self.overwintered && self.version >= 3 {
            stream.append(&self.expiry_height);
            if self.version >= 4 {
                stream
                    .append(&self.value_balance)
                    .append_list(&self.shielded_spends)
                    .append_list(&self.shielded_outputs);
            }
        }
        if self.zcash {
            if self.version == 2 || self.overwintered {
                stream.append_list(&self.join_splits);
                if !self.join_splits.is_empty() {
                    stream.append(&self.join_split_pubkey).append(&self.join_split_sig);
                }
            }
            if self.version >= 4
                && self.overwintered
                && !(self.shielded_outputs.is_empty() && self.shielded_spends.is_empty())
            {
                stream.append(&self.binding_sig);
            }
        }
        if let Some(ref s) = self.str_d_zeel {
            let len: CompactInteger = s.len().into();
            stream.append(&len);
            stream.append_slice(s.as_bytes());
        }
    }
}

/// Hint to `deserialize_tx` describing which wire variant the caller expects.
/// The top-level `Deserializable for Transaction` impl tries each in order.
#[derive(Eq, PartialEq)]
pub enum TxType {
    StandardWithWitness,
    Zcash,
    PosWithNTime,
}

/// Decodes a transaction under the given `tx_type` hint. Used directly by
/// `BlockHeader` AuxPoW deserialization (which knows the parent coinbase is a
/// standard-with-witness Bitcoin transaction) and indirectly by the top-level
/// multi-path `Deserializable for Transaction`.
pub fn deserialize_tx<T: io::Read>(reader: &mut Reader<T>, tx_type: TxType) -> Result<Transaction, Error> {
    let header: i32 = reader.read()?;
    let overwintered = (header >> 31) != 0;
    let version = if overwintered { header & 0x7FFF_FFFF } else { header };

    let version_group_id = if overwintered { reader.read()? } else { 0 };

    let n_time = if tx_type == TxType::PosWithNTime {
        Some(reader.read()?)
    } else {
        None
    };

    let mut inputs: Vec<TransactionInput> = reader.read_list_max(MAX_LIST_SIZE)?;
    // Detect SegWit: zero inputs followed by a non-zero witness flag indicates
    // the BIP-141 envelope. Only legal in StandardWithWitness mode; older
    // overwintered/PoS layouts would mis-parse here.
    let read_witness = if inputs.is_empty() && !overwintered && tx_type == TxType::StandardWithWitness {
        let witness_flag: u8 = reader.read()?;
        if witness_flag != WITNESS_FLAG {
            return Err(Error::MalformedData);
        }
        inputs = reader.read_list_max(MAX_LIST_SIZE)?;
        true
    } else {
        false
    };

    let outputs: Vec<TransactionOutput> = reader.read_list_max(MAX_LIST_SIZE)?;
    if outputs.is_empty() && tx_type == TxType::StandardWithWitness {
        return Err(Error::Custom("Transaction has no output".into()));
    }
    if read_witness {
        for input in inputs.iter_mut() {
            input.script_witness = reader.read_list_max(MAX_LIST_SIZE)?;
        }
    }

    let lock_time = reader.read()?;

    let mut expiry_height = 0u32;
    let mut value_balance = 0i64;
    let mut shielded_spends: Vec<ShieldedSpend> = vec![];
    let mut shielded_outputs: Vec<ShieldedOutput> = vec![];
    if overwintered && version >= 3 {
        expiry_height = reader.read()?;
        if version >= 4 {
            value_balance = reader.read()?;
            shielded_spends = reader.read_list_max(MAX_LIST_SIZE)?;
            shielded_outputs = reader.read_list_max(MAX_LIST_SIZE)?;
        }
    }

    let mut join_splits = vec![];
    let mut join_split_pubkey = H256::default();
    let mut join_split_sig = H512::default();
    let mut binding_sig = H512::default();
    let zcash = overwintered || tx_type == TxType::Zcash;
    if zcash {
        if version == 2 || overwintered {
            let len: usize = reader.read::<CompactInteger>()?.into();
            if len > 0 {
                if len > MAX_LIST_SIZE {
                    return Err(Error::MalformedData);
                }
                let use_groth = version > 2;
                for _ in 0..len {
                    join_splits.push(deserialize_join_split(reader, use_groth)?);
                }
                join_split_pubkey = reader.read()?;
                join_split_sig = reader.read()?;
            }
        }
        if overwintered && version >= 4 && !(shielded_spends.is_empty() && shielded_outputs.is_empty()) {
            binding_sig = reader.read()?;
        }
    }

    let str_d_zeel = if tx_type == TxType::PosWithNTime && !reader.is_finished() {
        let len: CompactInteger = reader.read()?;
        let mut buf = vec![0u8; len.into()];
        reader.read_slice(&mut buf)?;
        let s = std::str::from_utf8(&buf).map_err(|_| Error::MalformedData)?;
        Some(s.to_owned())
    } else {
        None
    };

    Ok(Transaction {
        version,
        n_time,
        overwintered,
        version_group_id,
        inputs,
        outputs,
        lock_time,
        expiry_height,
        shielded_spends,
        shielded_outputs,
        join_splits,
        value_balance,
        join_split_pubkey,
        join_split_sig,
        binding_sig,
        zcash,
        str_d_zeel,
        tx_hash_algo: TxHashAlgo::DSHA256,
    })
}

impl Deserializable for Transaction {
    fn deserialize<T>(reader: &mut Reader<T>) -> Result<Self, Error>
    where
        Self: Sized,
        T: io::Read,
    {
        // We need the entire byte slice so we can retry across the three
        // mutually-exclusive wire formats. This impl assumes the reader holds
        // exactly one transaction — adequate for AtomicDEX use cases (it
        // breaks block-streaming, which we don't need).
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        if let Ok(t) = deserialize_tx(&mut Reader::from_read(buf.as_slice()), TxType::StandardWithWitness) {
            return Ok(t);
        }
        if let Ok(t) = deserialize_tx(&mut Reader::from_read(buf.as_slice()), TxType::PosWithNTime) {
            return Ok(t);
        }
        deserialize_tx(&mut Reader::from_read(buf.as_slice()), TxType::Zcash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PoS / PoSV coins (selected by the `isPoS` coins-config flag) build
    /// transactions carrying an `nTime` field. This locks the wire contract:
    /// `n_time` is serialized into the byte stream and decodes back under the
    /// PoS layout.
    #[test]
    fn pos_n_time_round_trips() {
        let tx = Transaction {
            version: 1,
            n_time: Some(0x6655_4433),
            overwintered: false,
            version_group_id: 0,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            expiry_height: 0,
            shielded_spends: vec![],
            shielded_outputs: vec![],
            join_splits: vec![],
            value_balance: 0,
            join_split_pubkey: H256::default(),
            join_split_sig: H512::default(),
            binding_sig: H512::default(),
            zcash: false,
            str_d_zeel: None,
            tx_hash_algo: TxHashAlgo::DSHA256,
        };

        let bytes = serialize(&tx);
        let decoded = deserialize_tx(&mut Reader::from_read(bytes.as_ref()), TxType::PosWithNTime)
            .expect("a PoS transaction must decode under the PosWithNTime layout");
        assert_eq!(
            decoded.n_time,
            Some(0x6655_4433),
            "n_time must survive the serialize/deserialize round-trip"
        );
    }
}
