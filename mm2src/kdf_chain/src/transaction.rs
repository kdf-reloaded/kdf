//! KDF transaction primitives covering the full multi-coin matrix.
//!
//! Wire format reference:
//!   * Bitcoin standard (with optional SegWit witness marker / flag)
//!   * Zcash overwintered (v3, v4 Sapling) — `JoinSplit` PHGR/Groth, shielded
//!     spends/outputs, `binding_sig`
//!   * Zcash version 5 (ZIP 225): transparent, Sapling and Orchard bundles, with the
//!     ZIP 244 transaction id (see [`crate::zip244`])
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
use crate::zip244;

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

/// The only overwintered transaction version with the ZIP 225 layout.
const ZIP225_VERSION: i32 = 5;

/// One Sapling spend of a version 5 transaction (ZIP 225). The fields are stored together, but the
/// wire carries them in three groups: `cv`, `nullifier` and `rk` with the spends, the proof in a
/// block of proofs and the authorization signature in a block of signatures. The anchor is shared
/// by all spends and lives in [`SaplingBundleV5`].
#[derive(Debug, PartialEq, Clone)]
pub struct SaplingSpendV5 {
    pub cv: H256,
    pub nullifier: H256,
    pub rk: H256,
    pub zkproof: ZkProofSapling,
    pub spend_auth_sig: H512,
}

/// The Sapling part of a version 5 transaction (ZIP 225). The outputs reuse [`ShieldedOutput`]: the
/// description and its proof are one value here, the wire splits them.
///
/// `value_balance` and `binding_sig` are on the wire only when there is at least one spend or
/// output, `anchor` only when there is at least one spend; a bundle without them keeps the
/// default values, which are never written.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct SaplingBundleV5 {
    pub spends: Vec<SaplingSpendV5>,
    pub outputs: Vec<ShieldedOutput>,
    pub value_balance: i64,
    pub anchor: H256,
    pub binding_sig: H512,
}

impl SaplingBundleV5 {
    /// Whether the transaction carries any Sapling spend or output.
    pub fn is_present(&self) -> bool { !self.spends.is_empty() || !self.outputs.is_empty() }
}

/// One Orchard action of a version 5 transaction (ZIP 225). The authorization signature is stored
/// with the action; the wire carries it in a block of signatures after the proofs.
#[derive(Debug, PartialEq, Clone)]
pub struct OrchardAction {
    pub cv: H256,
    pub nullifier: H256,
    pub rk: H256,
    pub cmx: H256,
    pub ephemeral_key: H256,
    pub enc_cipher_text: EncCipherText,
    pub out_cipher_text: OutCipherText,
    pub spend_auth_sig: H512,
}

/// The Orchard part of a version 5 transaction (ZIP 225). Everything but the action count is on the
/// wire only when there is at least one action; a bundle without actions keeps the default values,
/// which are never written. `proofs` is the aggregated proof, the bytes behind its length prefix.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct OrchardBundle {
    pub actions: Vec<OrchardAction>,
    pub flags: u8,
    pub value_balance: i64,
    pub anchor: H256,
    pub proofs: Bytes,
    pub binding_sig: H512,
}

impl OrchardBundle {
    /// Whether the transaction carries any Orchard action.
    pub fn is_present(&self) -> bool { !self.actions.is_empty() }
}

fn serialize_sapling_v5(bundle: &SaplingBundleV5, stream: &mut Stream) {
    let present = bundle.is_present();
    stream.append(&CompactInteger::from(bundle.spends.len()));
    for spend in &bundle.spends {
        stream.append(&spend.cv).append(&spend.nullifier).append(&spend.rk);
    }
    stream.append(&CompactInteger::from(bundle.outputs.len()));
    for output in &bundle.outputs {
        stream
            .append(&output.cv)
            .append(&output.cmu)
            .append(&output.ephemeral_key)
            .append(&output.enc_cipher_text)
            .append(&output.out_cipher_text);
    }
    if present {
        stream.append(&bundle.value_balance);
    }
    if !bundle.spends.is_empty() {
        stream.append(&bundle.anchor);
    }
    for spend in &bundle.spends {
        stream.append(&spend.zkproof);
    }
    for spend in &bundle.spends {
        stream.append(&spend.spend_auth_sig);
    }
    for output in &bundle.outputs {
        stream.append(&output.zkproof);
    }
    if present {
        stream.append(&bundle.binding_sig);
    }
}

fn deserialize_sapling_v5<T: io::Read>(reader: &mut Reader<T>) -> Result<SaplingBundleV5, Error> {
    let mut bundle = SaplingBundleV5::default();

    let spends_len: usize = reader.read::<CompactInteger>()?.into();
    if spends_len > MAX_LIST_SIZE {
        return Err(Error::MalformedData);
    }
    for _ in 0..spends_len {
        bundle.spends.push(SaplingSpendV5 {
            cv: reader.read()?,
            nullifier: reader.read()?,
            rk: reader.read()?,
            zkproof: ZkProofSapling::default(),
            spend_auth_sig: H512::default(),
        });
    }

    let outputs_len: usize = reader.read::<CompactInteger>()?.into();
    if outputs_len > MAX_LIST_SIZE {
        return Err(Error::MalformedData);
    }
    for _ in 0..outputs_len {
        bundle.outputs.push(ShieldedOutput {
            cv: reader.read()?,
            cmu: reader.read()?,
            ephemeral_key: reader.read()?,
            enc_cipher_text: reader.read()?,
            out_cipher_text: reader.read()?,
            zkproof: ZkProofSapling::default(),
        });
    }

    if bundle.is_present() {
        bundle.value_balance = reader.read()?;
    }
    if !bundle.spends.is_empty() {
        bundle.anchor = reader.read()?;
    }
    for spend in bundle.spends.iter_mut() {
        spend.zkproof = reader.read()?;
    }
    for spend in bundle.spends.iter_mut() {
        spend.spend_auth_sig = reader.read()?;
    }
    for output in bundle.outputs.iter_mut() {
        output.zkproof = reader.read()?;
    }
    if bundle.is_present() {
        bundle.binding_sig = reader.read()?;
    }
    Ok(bundle)
}

fn serialize_orchard(bundle: &OrchardBundle, stream: &mut Stream) {
    stream.append(&CompactInteger::from(bundle.actions.len()));
    if !bundle.is_present() {
        return;
    }
    for action in &bundle.actions {
        stream
            .append(&action.cv)
            .append(&action.nullifier)
            .append(&action.rk)
            .append(&action.cmx)
            .append(&action.ephemeral_key)
            .append(&action.enc_cipher_text)
            .append(&action.out_cipher_text);
    }
    stream
        .append(&bundle.flags)
        .append(&bundle.value_balance)
        .append(&bundle.anchor)
        .append(&CompactInteger::from(bundle.proofs.len()))
        .append_slice(&bundle.proofs);
    for action in &bundle.actions {
        stream.append(&action.spend_auth_sig);
    }
    stream.append(&bundle.binding_sig);
}

fn deserialize_orchard<T: io::Read>(reader: &mut Reader<T>) -> Result<OrchardBundle, Error> {
    let mut bundle = OrchardBundle::default();

    let actions_len: usize = reader.read::<CompactInteger>()?.into();
    if actions_len == 0 {
        return Ok(bundle);
    }
    if actions_len > MAX_LIST_SIZE {
        return Err(Error::MalformedData);
    }
    for _ in 0..actions_len {
        bundle.actions.push(OrchardAction {
            cv: reader.read()?,
            nullifier: reader.read()?,
            rk: reader.read()?,
            cmx: reader.read()?,
            ephemeral_key: reader.read()?,
            enc_cipher_text: reader.read()?,
            out_cipher_text: reader.read()?,
            spend_auth_sig: H512::default(),
        });
    }
    bundle.flags = reader.read()?;
    bundle.value_balance = reader.read()?;
    bundle.anchor = reader.read()?;

    // The aggregated proof can exceed the 64 KiB limit of `Bytes`. Its length comes from the wire, so
    // the buffer grows only with the bytes that are actually there.
    let proofs_len: u64 = reader.read::<CompactInteger>()?.into();
    let mut proofs = Vec::new();
    reader.by_ref().take(proofs_len).read_to_end(&mut proofs)?;
    if proofs.len() as u64 != proofs_len {
        return Err(Error::UnexpectedEnd);
    }
    bundle.proofs = proofs.into();

    for action in bundle.actions.iter_mut() {
        action.spend_auth_sig = reader.read()?;
    }
    bundle.binding_sig = reader.read()?;
    Ok(bundle)
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
    /// ZIP 225 `nConsensusBranchId`. Only a version 5 transaction carries it (it is part of the
    /// ZIP 244 digests); it stays zero for every other version.
    pub consensus_branch_id: u32,
    /// The Sapling part of a version 5 transaction. Empty for every other version, which keep their
    /// Sapling data in `shielded_spends`, `shielded_outputs`, `value_balance` and `binding_sig`.
    pub sapling_v5: SaplingBundleV5,
    /// The Orchard part of a version 5 transaction. Empty for every other version.
    pub orchard: OrchardBundle,
    /// Set when an external reader (the Zcash library, for a version the layouts above do not cover)
    /// produced this transaction: the original bytes and the identifier that reader computed. See
    /// [`OpaqueTx`]. `None` for everything the parser of this module reads or a signer builds.
    pub opaque: Option<OpaqueTx>,
    /// The extra payload of a Dash-layout special transaction (R38.6.10.5): `None` for every other transaction and
    /// for a special-shaped one with nothing after its lock time, `Some` (possibly empty) when bytes follow the
    /// lock time. The payload is kept and written back, not interpreted.
    pub extra_payload: Option<Bytes>,
}

/// The raw form of a transaction that a reader outside this crate has parsed (CRD ch.39 D39.11.4V.3: a
/// ZIP 229 version 6 transaction is read by the Zcash library). The ordinary fields of the
/// [`Transaction`] hold what call sites read (inputs, outputs, lock time, expiry height); this keeps
/// the bytes exactly as received and the identifier the reader derived, because the layout is not
/// built here: `serialize` writes `raw` and `hash` answers `txid`.
///
/// Never set on a transaction that is going to be modified or signed: the ordinary fields would then
/// disagree with `raw`. The signer refuses such a transaction through its version check.
#[derive(Debug, PartialEq, Clone)]
pub struct OpaqueTx {
    /// The transaction exactly as it was read.
    pub raw: Bytes,
    /// The transaction identifier in digest (internal) byte order, like [`Transaction::hash`].
    pub txid: H256,
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
    ///
    /// A version 5 Zcash transaction (ZIP 225) has the ZIP 244 transaction id instead: a tree of
    /// personalized BLAKE2b-256 digests that covers the transparent, Sapling and Orchard parts and
    /// is personalized with `consensus_branch_id`. The 32 bytes are in digest order, like the
    /// double-SHA256 of the other versions; the usual txid string is their reverse.
    pub fn hash(&self) -> H256 {
        if let Some(opaque) = &self.opaque {
            return opaque.txid;
        }
        if self.is_v5() {
            return zip244::txid(self);
        }
        let bytes = serialize(self);
        match self.tx_hash_algo {
            TxHashAlgo::DSHA256 => dhash256(&bytes),
            TxHashAlgo::SHA256 => sha256(&bytes),
        }
    }

    /// Computes the wtxid (DSHA-256 of the SegWit-with-witness serialization).
    pub fn witness_hash(&self) -> H256 { dhash256(&serialize_with_flags(self, SERIALIZE_TRANSACTION_WITNESS)) }

    /// Whether the transaction has the ZIP 225 version 5 layout.
    pub fn is_v5(&self) -> bool { self.overwintered && self.version == ZIP225_VERSION }

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
        if let Some(opaque) = &self.opaque {
            stream.append_slice(&opaque.raw);
            return;
        }
        if self.is_v5() {
            // ZIP 225 layout: header, group id, branch id, lock time, expiry height, then the
            // transparent, Sapling and Orchard bundles.
            stream
                .append(&(self.version | (1 << 31)))
                .append(&self.version_group_id)
                .append(&self.consensus_branch_id)
                .append(&self.lock_time)
                .append(&self.expiry_height)
                .append_list(&self.inputs)
                .append_list(&self.outputs);
            serialize_sapling_v5(&self.sapling_v5, stream);
            serialize_orchard(&self.orchard, stream);
            return;
        }

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

        if let Some(payload) = &self.extra_payload {
            stream
                .append(&CompactInteger::from(payload.len()))
                .append_slice(payload);
        }

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

/// A transaction qualifies as a Dash-layout special transaction (R38.6.10.5) when the top bit of its leading word is
/// clear, its version (low 16 bits) is 3 or higher and its type (high 16 bits) is not 0. Such a transaction with
/// nothing after its lock time is read without a payload (`extra_payload == None`).
fn is_special_header(header: i32) -> bool { header >= 0 && (header & 0xFFFF) >= 3 && (header >> 16) != 0 }

/// Decodes a transaction under the given `tx_type` hint. Used directly by
/// `BlockHeader` AuxPoW deserialization (which knows the parent coinbase is a
/// standard-with-witness Bitcoin transaction) and indirectly by the top-level
/// multi-path `Deserializable for Transaction`.
pub fn deserialize_tx<T: io::Read>(reader: &mut Reader<T>, tx_type: TxType) -> Result<Transaction, Error> {
    let header: i32 = reader.read()?;
    let overwintered = (header >> 31) != 0;
    let version = if overwintered { header & 0x7FFF_FFFF } else { header };

    // Only the overwintered layouts up to version 5 are built; a later one would be read in the wrong layout. The
    // UTXO coins' reader in `coins` (`read_utxo_tx`) gives those to the Zcash library; this crate does not depend on it.
    if overwintered && version > ZIP225_VERSION {
        return Err(Error::Custom(format!("unsupported transaction version {version}")));
    }

    let version_group_id = if overwintered { reader.read()? } else { 0 };

    if overwintered && version == ZIP225_VERSION {
        return deserialize_tx_v5(reader, version_group_id);
    }

    let special = is_special_header(header);

    // A special transaction has the Dash layout in every path: no time field, no witness marker, no trailing string.
    let n_time = if tx_type == TxType::PosWithNTime && !special {
        Some(reader.read()?)
    } else {
        None
    };

    let mut inputs: Vec<TransactionInput> = reader.read_list_max(MAX_LIST_SIZE)?;
    // Detect SegWit: zero inputs followed by a non-zero witness flag indicates
    // the BIP-141 envelope. Only legal in StandardWithWitness mode; older
    // overwintered/PoS layouts would mis-parse here.
    let read_witness = if inputs.is_empty() && !overwintered && !special && tx_type == TxType::StandardWithWitness {
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

    let extra_payload = if special && !reader.is_finished() {
        let len: u64 = reader.read::<CompactInteger>()?.into();
        let mut payload = Vec::new();
        reader.by_ref().take(len).read_to_end(&mut payload)?;
        if payload.len() as u64 != len {
            return Err(Error::UnexpectedEnd);
        }
        Some(Bytes::from(payload))
    } else {
        None
    };

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

    let str_d_zeel = if tx_type == TxType::PosWithNTime && !special && !reader.is_finished() {
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
        consensus_branch_id: 0,
        sapling_v5: SaplingBundleV5::default(),
        orchard: OrchardBundle::default(),
        opaque: None,
        extra_payload,
    })
}

/// Decodes the rest of a ZIP 225 transaction after its header and version group id. Unlike the
/// older layouts, a transaction without transparent outputs is legal here (a fully shielded one).
fn deserialize_tx_v5<T: io::Read>(reader: &mut Reader<T>, version_group_id: u32) -> Result<Transaction, Error> {
    let consensus_branch_id = reader.read()?;
    let lock_time = reader.read()?;
    let expiry_height = reader.read()?;
    let inputs: Vec<TransactionInput> = reader.read_list_max(MAX_LIST_SIZE)?;
    let outputs: Vec<TransactionOutput> = reader.read_list_max(MAX_LIST_SIZE)?;
    let sapling_v5 = deserialize_sapling_v5(reader)?;
    let orchard = deserialize_orchard(reader)?;

    Ok(Transaction {
        version: ZIP225_VERSION,
        n_time: None,
        overwintered: true,
        version_group_id,
        inputs,
        outputs,
        lock_time,
        expiry_height,
        shielded_spends: vec![],
        shielded_outputs: vec![],
        join_splits: vec![],
        value_balance: 0,
        join_split_pubkey: H256::default(),
        join_split_sig: H512::default(),
        binding_sig: H512::default(),
        zcash: true,
        str_d_zeel: None,
        tx_hash_algo: TxHashAlgo::DSHA256,
        consensus_branch_id,
        sapling_v5,
        orchard,
        opaque: None,
        extra_payload: None,
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
    use hex::ToHex;

    /// An overwintered transaction of a version above 5 is refused, not read in the version 4 layout;
    /// version 4 still parses.
    #[test]
    fn overwintered_versions_above_five_are_refused() {
        let with_version = |version: u32| {
            let mut bytes = (version | (1 << 31)).to_le_bytes().to_vec();
            bytes.extend_from_slice(&0x892f_2085u32.to_le_bytes()); // version group id
            bytes.extend_from_slice(&[0x00, 0x00]); // no inputs, no outputs
            bytes.extend_from_slice(&[0u8; 8]); // lock time, expiry height
            bytes
        };

        for version in [6u32, 7, 0x7fff_ffff] {
            let bytes = with_version(version);
            for tx_type in [TxType::StandardWithWitness, TxType::PosWithNTime, TxType::Zcash] {
                let result = deserialize_tx(&mut Reader::from_read(bytes.as_slice()), tx_type);
                assert_eq!(
                    result,
                    Err(Error::Custom(format!("unsupported transaction version {version}"))),
                    "version {version}"
                );
            }
            assert!(
                deserialize::<_, Transaction>(bytes.as_slice()).is_err(),
                "version {version}"
            );
        }

        // Version 4 has the older layout with a Sapling section of its own: add empty value balance,
        // spends and outputs, and no joinsplits.
        let mut v4 = with_version(4);
        v4.extend_from_slice(&[0u8; 8]); // value balance
        v4.extend_from_slice(&[0x00, 0x00]); // no spends, no outputs
        v4.push(0x00); // no joinsplits
        let tx: Transaction = deserialize(v4.as_slice()).expect("a version 4 transaction parses");
        assert_eq!(tx.version, 4);
    }

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
            consensus_branch_id: 0,
            sapling_v5: SaplingBundleV5::default(),
            orchard: OrchardBundle::default(),
            opaque: None,
            extra_payload: None,
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

    /// A transaction carrying the raw form of an external reader is written as those bytes and answers that
    /// reader's transaction id, whatever its other fields say.
    #[test]
    fn an_opaque_transaction_serializes_as_its_raw_bytes_and_hashes_to_its_txid() {
        let raw = vec![0x06, 0x00, 0x00, 0x80, 1, 2, 3];
        let txid = H256::from([9u8; 32]);
        let tx = Transaction {
            version: 6,
            overwintered: true,
            opaque: Some(OpaqueTx {
                raw: Bytes::from(raw.clone()),
                txid,
            }),
            ..Default::default()
        };
        assert_eq!(serialize(&tx).to_vec(), raw);
        assert_eq!(tx.hash(), txid);
    }

    fn fixture(hex_text: &str) -> Vec<u8> { hex_text.trim().from_hex().expect("fixture is hex") }

    /// T38.6.10.1: public-chain Dash-layout transactions, read, written back byte-exact, with the chain's txid.
    #[test]
    fn dash_layout_special_transactions_read_and_write_back_exactly() {
        // (hex, txid, version/type word, inputs, outputs, payload length)
        let vectors = [
            (
                include_str!("test_data/dash_2576221_type5_cbtx.hex"),
                "82efdfbb4adb79d9c590cd83c316acb8c40d51c975a519733ff2d420d469e7ed",
                0x0005_0003,
                1,
                3,
                Some(175),
            ),
            (
                include_str!("test_data/dash_2576104_type1_proregtx.hex"),
                "8a334dff3c0c0d7616b809a837798a70294c5fbd0e36604191de75d2670caf4e",
                0x0001_0003,
                1,
                1,
                Some(274),
            ),
            (
                include_str!("test_data/dash_2576170_type9_assetunlock.hex"),
                "0bfefcc1c944d1a0c885a1de834d7587c4069d944b6b28e9c2e16b80a07a3cc4",
                0x0009_0003,
                0,
                1,
                Some(145),
            ),
            (
                include_str!("test_data/dash_2576220_v3_type0.hex"),
                "0d9754c759a993b6ca598a2c6dae7935ffaeeceb77498da1e280437946b5ec8b",
                3,
                19,
                2,
                None,
            ),
            (
                include_str!("test_data/krgn_191125_type10_sapling.hex"),
                "6b99a0b216da1e96b2eacc0ed9db4993301342f45d21c3a3dfeafbfeadd2ad11",
                0x000a_0003,
                0,
                1,
                Some(2356),
            ),
        ];
        for (hex_text, txid, word, inputs, outputs, payload_len) in vectors {
            let bytes = fixture(hex_text);
            let tx: Transaction = deserialize(bytes.as_slice()).unwrap_or_else(|e| panic!("{txid}: {e:?}"));
            assert_eq!(tx.version, word, "{txid}");
            assert_eq!(tx.inputs.len(), inputs, "{txid}");
            assert_eq!(tx.outputs.len(), outputs, "{txid}");
            assert_eq!(tx.extra_payload.as_ref().map(|p| p.len()), payload_len, "{txid}");
            assert!(!tx.has_witness(), "{txid}");
            assert_eq!(serialize(&tx).to_vec(), bytes, "{txid}");
            assert_eq!(tx.hash().reversed().to_vec().to_hex::<String>(), txid);
        }
    }

    /// The type-6 quorum commitment has neither inputs nor outputs: report what the readers do.
    #[test]
    fn dash_type6_quorum_commitment_without_outputs() {
        let bytes = fixture(include_str!("test_data/dash_2576219_type6_qc.hex"));
        let txid = "c28c2cf4b6aa304430b0c5ef53763fcd8398451307843e54f5618f4da87f6d07";
        let standard = deserialize_tx(&mut Reader::from_read(bytes.as_slice()), TxType::StandardWithWitness);
        assert_eq!(standard, Err(Error::Custom("Transaction has no output".into())));
        let tx: Transaction = deserialize(bytes.as_slice()).expect("the multi-path reader reads it on its Zcash path");
        assert_eq!(tx.extra_payload.as_ref().map(|p| p.len()), Some(341));
        assert_eq!(serialize(&tx).to_vec(), bytes);
        assert_eq!(tx.hash().reversed().to_vec().to_hex::<String>(), txid);
    }

    /// T38.6.10.2: an overwintered version-3 transaction (low 16 bits of the word are also 3) and a segwit
    /// transaction keep their readings.
    #[test]
    fn overwintered_v3_and_segwit_keep_their_readings() {
        // Overwintered v3: version group id, no inputs, no outputs, lock time, expiry height, no joinsplits.
        let mut ow = (3u32 | (1 << 31)).to_le_bytes().to_vec();
        ow.extend_from_slice(&0x03c4_8270u32.to_le_bytes());
        ow.extend_from_slice(&[0x00, 0x00]);
        ow.extend_from_slice(&7u32.to_le_bytes());
        ow.extend_from_slice(&9u32.to_le_bytes());
        ow.push(0x00);
        let tx: Transaction = deserialize(ow.as_slice()).unwrap();
        assert!(tx.overwintered && tx.zcash);
        assert_eq!((tx.version, tx.lock_time, tx.expiry_height), (3, 7, 9));
        assert_eq!(tx.extra_payload, None);
        assert_eq!(serialize(&tx).to_vec(), ow);
        assert_eq!(tx.hash(), dhash256(&ow));

        // Segwit: version 2, marker and flag, one input, one output, a two-item witness, lock time.
        let mut sw = 2u32.to_le_bytes().to_vec();
        sw.extend_from_slice(&[0x00, 0x01, 0x01]);
        sw.extend_from_slice(&[0x11; 32]);
        sw.extend_from_slice(&0u32.to_le_bytes());
        sw.extend_from_slice(&[0x00]);
        sw.extend_from_slice(&0xffff_fffeu32.to_le_bytes());
        sw.extend_from_slice(&[0x01]);
        sw.extend_from_slice(&5000u64.to_le_bytes());
        sw.extend_from_slice(&[0x02, 0x51, 0x51]);
        sw.extend_from_slice(&[0x02, 0x01, 0xaa, 0x02, 0xbb, 0xcc]);
        sw.extend_from_slice(&0u32.to_le_bytes());
        let tx: Transaction = deserialize(sw.as_slice()).unwrap();
        assert_eq!(tx.extra_payload, None);
        assert!(tx.has_witness());
        assert_eq!(tx.inputs[0].script_witness.len(), 2);
        assert_eq!(serialize_with_flags(&tx, SERIALIZE_TRANSACTION_WITNESS).to_vec(), sw);
        let stripped = serialize(&tx);
        assert_eq!(tx.hash(), dhash256(&stripped));
        assert_ne!(stripped.to_vec(), sw);
    }

    /// T38.6.10.4: a payload prefix longer than the bytes that remain, or a missing payload, is refused in every
    /// path and by the top-level reader.
    #[test]
    fn truncated_special_payload_is_refused_everywhere() {
        let full = fixture(include_str!("test_data/dash_2576170_type9_assetunlock.hex"));
        let payload_len = 145usize;
        let payload_start = full.len() - payload_len; // the prefix (0x91 = 145 < 253) is one byte before it
        assert_eq!(full[payload_start - 1], 145, "prefix byte of the 145-byte payload");

        let mut cases = Vec::new();
        cases.push(full[..full.len() - 1].to_vec()); // one payload byte missing
        cases.push(full[..payload_start].to_vec()); // prefix, no payload
        let mut lying = full[..payload_start - 1].to_vec();
        lying.extend_from_slice(&[0xfd, 0xff, 0xff]); // claims 65535 bytes
        lying.extend_from_slice(&full[payload_start..]);
        cases.push(lying);

        for (i, bytes) in cases.iter().enumerate() {
            for tx_type in [TxType::StandardWithWitness, TxType::PosWithNTime, TxType::Zcash] {
                assert!(
                    deserialize_tx(&mut Reader::from_read(bytes.as_slice()), tx_type).is_err(),
                    "case {i}"
                );
            }
            assert!(deserialize::<_, Transaction>(bytes.as_slice()).is_err(), "case {i}");
        }
        assert!(deserialize::<_, Transaction>(full.as_slice()).is_ok());
    }

    /// R38.6.10.2: a special-shaped transaction with nothing after its lock time is read without a payload and
    /// written back byte-exact.
    #[test]
    fn special_transaction_cut_after_lock_time_reads_without_payload() {
        let full = fixture(include_str!("test_data/dash_2576170_type9_assetunlock.hex"));
        let payload_start = full.len() - 145;
        let bytes = full[..payload_start - 1].to_vec();
        for tx_type in [TxType::StandardWithWitness, TxType::PosWithNTime, TxType::Zcash] {
            let tx = deserialize_tx(&mut Reader::from_read(bytes.as_slice()), tx_type).unwrap();
            assert_eq!(tx.extra_payload, None);
            assert_eq!(serialize(&tx).to_vec(), bytes);
            assert_eq!(tx.hash(), dhash256(&bytes));
        }
        let tx: Transaction = deserialize(bytes.as_slice()).unwrap();
        assert_eq!(tx.extra_payload, None);
        assert_eq!(serialize(&tx).to_vec(), bytes);
        assert_eq!(tx.hash(), dhash256(&bytes));
    }

    /// A non-Dash transaction whose version word has the special shape, with nothing after the lock time, reads
    /// as an ordinary transaction and round-trips byte-exact.
    #[test]
    fn special_shaped_version_without_payload_round_trips() {
        let mut b = 0x0001_0003u32.to_le_bytes().to_vec();
        b.push(0x01);
        b.extend_from_slice(&[0x22; 32]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(0x00);
        b.extend_from_slice(&0xffff_fffeu32.to_le_bytes());
        b.push(0x01);
        b.extend_from_slice(&5000u64.to_le_bytes());
        b.extend_from_slice(&[0x02, 0x51, 0x51]);
        b.extend_from_slice(&0u32.to_le_bytes());
        let check = |tx: Transaction| {
            assert_eq!(tx.version, 0x0001_0003);
            assert_eq!(tx.extra_payload, None);
            assert_eq!((tx.inputs.len(), tx.outputs.len()), (1, 1));
            assert_eq!(tx.outputs[0].value, 5000);
            assert_eq!(serialize(&tx).to_vec(), b);
        };
        for tx_type in [TxType::StandardWithWitness, TxType::PosWithNTime, TxType::Zcash] {
            check(deserialize_tx(&mut Reader::from_read(b.as_slice()), tx_type).unwrap());
        }
        check(deserialize::<_, Transaction>(b.as_slice()).unwrap());
    }
}
