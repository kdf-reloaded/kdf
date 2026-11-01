// Transaction signature-hash computation.
//
// Implements three sighash modes:
//   * "original"  — pre-segwit Bitcoin (single-SHA256 over a
//     trimmed-down transaction; output is then DSHA256-folded
//     into the digest passed to ECDSA).
//   * "witness v0" — BIP-143 segwit / Bitcoin Cash (with optional
//     fork-id flag for replay protection).
//   * "overwintered" — ZIP-243 Zcash Sapling (BLAKE2b personalised
//     digest tree).
//
// Plus the legacy KDF "stake-input" extension (PoS chains with
// `nTime`) and KMD "Verus" version XOR — these don't change the
// sighash tree itself; they only change how the underlying transaction
// serializes, which is handled inside `kdf_chain`.

use crate::bytes::Bytes;
use crate::hash::{H256, H512};
use crate::{Builder, Script};
use blake2b_simd::Params as Blake2b;
use chain::{JoinSplit, OutPoint, ShieldedOutput, ShieldedSpend, Transaction, TransactionInput, TransactionOutput,
            TxHashAlgo};
use crypto::{dhash256, sha256};
use keys::KeyPair;
use serde::Deserialize;
use serialization::Stream;
use std::collections::BTreeSet;

// --- ZIP-243 personalisation tags (16 bytes each, last 4 bytes
// optionally carry the consensus branch id at runtime) -------------

const PERS_PREVOUTS: &[u8] = b"ZcashPrevoutHash";
const PERS_SEQUENCE: &[u8] = b"ZcashSequencHash";
const PERS_OUTPUTS: &[u8] = b"ZcashOutputsHash";
const PERS_JOINSPLITS: &[u8] = b"ZcashJSplitsHash";
const PERS_SAPLING_SPENDS: &[u8] = b"ZcashSSpendsHash";
const PERS_SAPLING_OUTPUTS: &[u8] = b"ZcashSOutputHash";
const PERS_SIGHASH: &[u8] = b"ZcashSigHash";

// --- Sighash flag bits (Bitcoin script reference) -----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SignatureVersion {
    #[serde(rename = "base")]
    Base,
    #[serde(rename = "witness_v0")]
    WitnessV0,
    #[serde(rename = "fork_id")]
    ForkId,
    /// Radiant's fork-id dialect (CRD ch.58 §58.3): BIP-143 plus an output-summaries digest.
    #[serde(rename = "fork_id_rxd")]
    ForkIdRxd,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SighashBase {
    All = 1,
    None = 2,
    Single = 3,
}

impl From<SighashBase> for u32 {
    fn from(b: SighashBase) -> u32 { b as u32 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighash {
    pub base: SighashBase,
    pub anyone_can_pay: bool,
    pub fork_id: bool,
}

impl From<Sighash> for u32 {
    fn from(s: Sighash) -> u32 {
        let mut v = s.base as u32;
        if s.anyone_can_pay {
            v |= 0x80;
        }
        if s.fork_id {
            v |= 0x40;
        }
        v
    }
}

impl Sighash {
    pub fn new(base: SighashBase, anyone_can_pay: bool, fork_id: bool) -> Self {
        Sighash {
            base,
            anyone_can_pay,
            fork_id,
        }
    }

    /// `SCRIPT_VERIFY_STRICTENC` predicate: returns true iff the raw
    /// 32-bit value matches an exact (base | anyone_can_pay | fork_id)
    /// combination valid under the given signature version.
    pub fn is_defined(version: SignatureVersion, raw: u32) -> bool {
        let mask = match version {
            SignatureVersion::ForkId | SignatureVersion::ForkIdRxd => !(0x40 | 0x80),
            _ => !0x80,
        };
        matches!(raw & mask, 1..=3)
    }

    /// Decode flag bits permissively (accepts non-canonical inputs;
    /// callers wanting strict checking should call `is_defined` first).
    pub fn from_u32(version: SignatureVersion, raw: u32) -> Self {
        let base = match raw & 0x1f {
            2 => SighashBase::None,
            3 => SighashBase::Single,
            _ => SighashBase::All,
        };
        let anyone_can_pay = (raw & 0x80) != 0;
        let fork_id = matches!(version, SignatureVersion::ForkId | SignatureVersion::ForkIdRxd) && (raw & 0x40) != 0;
        Sighash {
            base,
            anyone_can_pay,
            fork_id,
        }
    }
}

// --- Signer-side input + transaction shape ------------------------

#[derive(Debug, Clone)]
pub struct UnsignedTransactionInput {
    pub previous_output: OutPoint,
    pub sequence: u32,
    pub amount: u64,
    pub witness: Vec<Vec<u8>>,
}

impl From<TransactionInput> for UnsignedTransactionInput {
    fn from(i: TransactionInput) -> Self {
        UnsignedTransactionInput {
            previous_output: i.previous_output,
            sequence: i.sequence,
            amount: 0,
            witness: i.script_witness.into_iter().map(Vec::from).collect(),
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy)]
pub enum SignerHashAlgo {
    SHA256,
    DSHA256,
}

impl From<TxHashAlgo> for SignerHashAlgo {
    fn from(a: TxHashAlgo) -> Self {
        match a {
            TxHashAlgo::DSHA256 => SignerHashAlgo::DSHA256,
            TxHashAlgo::SHA256 => SignerHashAlgo::SHA256,
        }
    }
}
impl From<SignerHashAlgo> for TxHashAlgo {
    fn from(a: SignerHashAlgo) -> Self {
        match a {
            SignerHashAlgo::DSHA256 => TxHashAlgo::DSHA256,
            SignerHashAlgo::SHA256 => TxHashAlgo::SHA256,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransactionInputSigner {
    pub version: i32,
    pub n_time: Option<u32>,
    pub overwintered: bool,
    pub version_group_id: u32,
    pub consensus_branch_id: u32,
    pub expiry_height: u32,
    pub value_balance: i64,
    pub inputs: Vec<UnsignedTransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub lock_time: u32,
    pub join_splits: Vec<JoinSplit>,
    pub shielded_spends: Vec<ShieldedSpend>,
    pub shielded_outputs: Vec<ShieldedOutput>,
    pub zcash: bool,
    pub str_d_zeel: Option<String>,
    pub hash_algo: SignerHashAlgo,
}

impl From<Transaction> for TransactionInputSigner {
    fn from(t: Transaction) -> Self {
        TransactionInputSigner {
            version: t.version,
            n_time: t.n_time,
            overwintered: t.overwintered,
            version_group_id: t.version_group_id,
            consensus_branch_id: 0,
            expiry_height: t.expiry_height,
            value_balance: t.value_balance,
            inputs: t.inputs.into_iter().map(Into::into).collect(),
            outputs: t.outputs,
            lock_time: t.lock_time,
            join_splits: t.join_splits,
            shielded_spends: t.shielded_spends,
            shielded_outputs: t.shielded_outputs,
            zcash: t.zcash,
            str_d_zeel: t.str_d_zeel,
            hash_algo: t.tx_hash_algo.into(),
        }
    }
}

impl From<TransactionInputSigner> for Transaction {
    fn from(s: TransactionInputSigner) -> Self {
        let inputs = s
            .inputs
            .iter()
            .map(|i| TransactionInput {
                previous_output: i.previous_output,
                script_sig: Bytes::default(),
                sequence: i.sequence,
                script_witness: vec![],
            })
            .collect();
        Transaction {
            version: s.version,
            n_time: s.n_time,
            overwintered: s.overwintered,
            version_group_id: s.version_group_id,
            expiry_height: s.expiry_height,
            value_balance: s.value_balance,
            inputs,
            outputs: s.outputs,
            lock_time: s.lock_time,
            join_splits: s.join_splits,
            shielded_spends: s.shielded_spends,
            shielded_outputs: s.shielded_outputs,
            zcash: s.zcash,
            binding_sig: H512::default(),
            join_split_pubkey: H256::default(),
            join_split_sig: H512::default(),
            str_d_zeel: s.str_d_zeel,
            tx_hash_algo: s.hash_algo.into(),
        }
    }
}

impl TransactionInputSigner {
    /// Compute the digest that ECDSA will sign for one input.
    pub fn signature_hash(
        &self,
        input_index: usize,
        input_amount: u64,
        script_pubkey: &Script,
        sigversion: SignatureVersion,
        sighashtype: u32,
    ) -> H256 {
        let sighash = Sighash::from_u32(sigversion, sighashtype);
        match sigversion {
            SignatureVersion::ForkId if sighash.fork_id => {
                self.sighash_fork_id(input_index, input_amount, script_pubkey, sighashtype, sighash)
            },
            SignatureVersion::ForkIdRxd if sighash.fork_id => {
                self.sighash_fork_id_rxd(input_index, input_amount, script_pubkey, sighashtype, sighash)
            },
            SignatureVersion::Base | SignatureVersion::ForkId | SignatureVersion::ForkIdRxd => {
                self.sighash_legacy(input_index, script_pubkey, sighashtype, sighash)
            },
            SignatureVersion::WitnessV0 => {
                self.sighash_witness_v0(input_index, input_amount, script_pubkey, sighashtype, sighash)
            },
        }
    }

    /// Sign one input; returns the signed `TransactionInput` ready to
    /// drop into the corresponding slot of the final transaction.
    pub fn signed_input(
        &self,
        keypair: &KeyPair,
        input_index: usize,
        input_amount: u64,
        script_pubkey: &Script,
        sigversion: SignatureVersion,
        sighash: u32,
    ) -> TransactionInput {
        let digest = self.signature_hash(input_index, input_amount, script_pubkey, sigversion, sighash);
        let mut sig: Vec<u8> = keypair.private().sign(&digest).expect("ECDSA signing").into();
        sig.push(sighash as u8);
        let script_sig = Builder::default().push_data(&sig).into_script();
        let unsigned = &self.inputs[input_index];
        TransactionInput {
            previous_output: unsigned.previous_output,
            sequence: unsigned.sequence,
            script_sig: script_sig.to_bytes(),
            script_witness: vec![],
        }
    }

    // --- Legacy (pre-segwit) sighash -----------------------------

    pub fn signature_hash_original(
        &self,
        input_index: usize,
        script_pubkey: &Script,
        sighashtype: u32,
        sighash: Sighash,
    ) -> H256 {
        self.sighash_legacy(input_index, script_pubkey, sighashtype, sighash)
    }

    fn sighash_legacy(&self, input_index: usize, script_pubkey: &Script, sighashtype: u32, sighash: Sighash) -> H256 {
        // Out-of-range input index → SIGHASH_ONE_BUG (digest is 0x01...01 little-endian) per Bitcoin Core.
        if input_index >= self.inputs.len() {
            return 1u8.into();
        }
        if sighash.base == SighashBase::Single && input_index >= self.outputs.len() {
            return 1u8.into();
        }
        // Zcash sapling diverts to a completely different algorithm.
        if self.version >= 3 && self.overwintered {
            return self
                .sighash_overwintered(input_index, script_pubkey, sighashtype, sighash)
                .expect("overwintered sighash");
        }

        let script_pubkey = script_pubkey.without_separators();

        // Build the trimmed transaction view that gets serialised + hashed.
        let inputs: Vec<TransactionInput> = if sighash.anyone_can_pay {
            // Only the input we are signing remains — its scriptSig is replaced with the prevout's scriptPubKey.
            let i = &self.inputs[input_index];
            vec![TransactionInput {
                previous_output: i.previous_output,
                script_sig: script_pubkey.to_bytes(),
                sequence: i.sequence,
                script_witness: vec![],
            }]
        } else {
            self.inputs
                .iter()
                .enumerate()
                .map(|(n, i)| TransactionInput {
                    previous_output: i.previous_output,
                    script_sig: if n == input_index {
                        script_pubkey.to_bytes()
                    } else {
                        Bytes::default()
                    },
                    sequence: match sighash.base {
                        SighashBase::Single | SighashBase::None if n != input_index => 0,
                        _ => i.sequence,
                    },
                    script_witness: vec![],
                })
                .collect()
        };

        let outputs: Vec<TransactionOutput> = match sighash.base {
            SighashBase::All => self.outputs.clone(),
            SighashBase::None => Vec::new(),
            SighashBase::Single => self
                .outputs
                .iter()
                .take(input_index + 1)
                .enumerate()
                .map(|(n, o)| {
                    if n == input_index {
                        o.clone()
                    } else {
                        TransactionOutput::default()
                    }
                })
                .collect(),
        };

        let tx = Transaction {
            version: self.version,
            n_time: self.n_time,
            inputs,
            outputs,
            lock_time: self.lock_time,
            zcash: self.zcash,
            str_d_zeel: self.str_d_zeel.clone(),
            tx_hash_algo: self.hash_algo.into(),
            // Sapling fields are zeroed for the legacy preimage.
            overwintered: false,
            version_group_id: 0,
            expiry_height: 0,
            value_balance: 0,
            join_splits: vec![],
            shielded_spends: vec![],
            shielded_outputs: vec![],
            binding_sig: H512::default(),
            join_split_pubkey: H256::default(),
            join_split_sig: H512::default(),
        };

        let mut s = Stream::default();
        s.append(&tx).append(&sighashtype);
        match self.hash_algo {
            SignerHashAlgo::DSHA256 => dhash256(&s.out()),
            SignerHashAlgo::SHA256 => sha256(&s.out()),
        }
    }

    // --- Segwit / BIP-143 sighash ---------------------------------

    fn sighash_witness_v0(
        &self,
        input_index: usize,
        input_amount: u64,
        script_pubkey: &Script,
        sighashtype: u32,
        sighash: Sighash,
    ) -> H256 {
        let prev = hash_prevouts(sighash, &self.inputs);
        let seq = hash_sequence(sighash, &self.inputs);
        let outs = hash_outputs(sighash, input_index, &self.outputs);
        let i = &self.inputs[input_index];

        let mut s = Stream::default();
        s.append(&self.version)
            .append(&prev)
            .append(&seq)
            .append(&i.previous_output)
            .append_list(script_pubkey)
            .append(&input_amount)
            .append(&i.sequence)
            .append(&outs)
            .append(&self.lock_time)
            .append(&sighashtype); // includes the high 24 bits as fork-id (zero for BCH 0-byte forkid).
        dhash256(&s.out())
    }

    // --- BCH / fork-id sighash (BIP-143 with the fork-id bit set) -

    fn sighash_fork_id(
        &self,
        input_index: usize,
        input_amount: u64,
        script_pubkey: &Script,
        sighashtype: u32,
        sighash: Sighash,
    ) -> H256 {
        if input_index >= self.inputs.len() {
            return 1u8.into();
        }
        if sighash.base == SighashBase::Single && input_index >= self.outputs.len() {
            return 1u8.into();
        }
        self.sighash_witness_v0(input_index, input_amount, script_pubkey, sighashtype, sighash)
    }

    // --- Radiant fork-id sighash (CRD ch.58 §58.3) ---------------

    /// BIP-143 preimage with one extra field, the output-summaries digest, between the input's
    /// sequence and the outputs digest.
    fn sighash_fork_id_rxd(
        &self,
        input_index: usize,
        input_amount: u64,
        script_pubkey: &Script,
        sighashtype: u32,
        sighash: Sighash,
    ) -> H256 {
        // Degenerate indices agree with the other fork-id dialect (R58.3.4).
        if input_index >= self.inputs.len() {
            return 1u8.into();
        }
        if sighash.base == SighashBase::Single && input_index >= self.outputs.len() {
            return 1u8.into();
        }
        let prev = hash_prevouts(sighash, &self.inputs);
        let seq = hash_sequence(sighash, &self.inputs);
        let summaries = hash_output_summaries(sighash, input_index, &self.outputs);
        let outs = hash_outputs(sighash, input_index, &self.outputs);
        let i = &self.inputs[input_index];

        let mut s = Stream::default();
        s.append(&self.version)
            .append(&prev)
            .append(&seq)
            .append(&i.previous_output)
            .append_list(script_pubkey)
            .append(&input_amount)
            .append(&i.sequence)
            .append(&summaries)
            .append(&outs)
            .append(&self.lock_time)
            .append(&sighashtype);
        dhash256(&s.out())
    }

    // --- ZIP-243 Zcash Sapling sighash ----------------------------

    /// ZIP-243 (Sapling) — implemented for SIGHASH_ALL only, which is
    /// the only mode KDF ever uses.
    pub fn signature_hash_overwintered(
        &self,
        input_index: usize,
        script_pubkey: &Script,
        sighashtype: u32,
        sighash: Sighash,
    ) -> Result<H256, String> {
        self.sighash_overwintered(input_index, script_pubkey, sighashtype, sighash)
    }

    fn sighash_overwintered(
        &self,
        input_index: usize,
        script_pubkey: &Script,
        sighashtype: u32,
        _sighash: Sighash,
    ) -> Result<H256, String> {
        let mut s = Stream::new();

        // Build personalisation: "ZcashSigHash" + LE consensus_branch_id.
        let mut personal = PERS_SIGHASH.to_vec();
        if self.version >= 3 {
            personal.extend_from_slice(&self.consensus_branch_id.to_le_bytes());
        }

        // Block 1: header + version_group_id.
        let header = self.version | if self.overwintered { 1 << 31 } else { 0 };
        s.append(&header).append(&self.version_group_id);

        // Block 2: hashed prevouts.
        let mut prev_s = Stream::new();
        for i in &self.inputs {
            prev_s.append(&i.previous_output);
        }
        s.append(&blake2b_personal(&prev_s.out(), PERS_PREVOUTS));

        // Block 3: hashed sequences.
        let mut seq_s = Stream::new();
        for i in &self.inputs {
            seq_s.append(&i.sequence);
        }
        s.append(&blake2b_personal(&seq_s.out(), PERS_SEQUENCE));

        // Block 4: hashed outputs.
        let mut out_s = Stream::new();
        for o in &self.outputs {
            out_s.append(o);
        }
        s.append(&blake2b_personal(&out_s.out(), PERS_OUTPUTS));

        // Block 5: joinSplits (empty subtree → 32 zero bytes).
        if self.join_splits.is_empty() {
            s.append(&H256::default());
        } else {
            let mut js = Stream::new();
            for j in &self.join_splits {
                js.append(j);
            }
            s.append(&blake2b_personal(&js.out(), PERS_JOINSPLITS));
        }

        // Block 6: sapling spend descriptions (cv | anchor | nullifier | rk | zkproof, no spendAuthSig).
        if self.shielded_spends.is_empty() {
            s.append(&H256::default());
        } else {
            let mut sp = Stream::new();
            for spend in &self.shielded_spends {
                sp.append(&spend.cv)
                    .append(&spend.anchor)
                    .append(&spend.nullifier)
                    .append(&spend.rk)
                    .append(&spend.zkproof);
            }
            s.append(&blake2b_personal(&sp.out(), PERS_SAPLING_SPENDS));
        }

        // Block 7: sapling output descriptions.
        if self.shielded_outputs.is_empty() {
            s.append(&H256::default());
        } else {
            let mut so = Stream::new();
            for o in &self.shielded_outputs {
                so.append(o);
            }
            s.append(&blake2b_personal(&so.out(), PERS_SAPLING_OUTPUTS));
        }

        // Block 8: locktime, expiry, value_balance, sighashtype.
        s.append(&self.lock_time)
            .append(&self.expiry_height)
            .append(&self.value_balance)
            .append(&sighashtype);

        // Block 9: signed-input descriptor — prevout, scriptcode, value, sequence.
        let i = &self.inputs[input_index];
        s.append(&i.previous_output)
            .append(&script_pubkey.to_bytes())
            .append(&i.amount)
            .append(&i.sequence);

        Ok(blake2b_personal(&s.out(), &personal))
    }
}

// --- BIP-143 sub-hashes -------------------------------------------

fn hash_prevouts(sh: Sighash, inputs: &[UnsignedTransactionInput]) -> H256 {
    if sh.anyone_can_pay {
        return 0u8.into();
    }
    let mut s = Stream::default();
    for i in inputs {
        s.append(&i.previous_output);
    }
    dhash256(&s.out())
}

fn hash_sequence(sh: Sighash, inputs: &[UnsignedTransactionInput]) -> H256 {
    if sh.anyone_can_pay || sh.base != SighashBase::All {
        return 0u8.into();
    }
    let mut s = Stream::default();
    for i in inputs {
        s.append(&i.sequence);
    }
    dhash256(&s.out())
}

fn hash_outputs(sh: Sighash, input_index: usize, outputs: &[TransactionOutput]) -> H256 {
    match sh.base {
        SighashBase::All => {
            let mut s = Stream::default();
            for o in outputs {
                s.append(o);
            }
            dhash256(&s.out())
        },
        SighashBase::Single if input_index < outputs.len() => {
            let mut s = Stream::default();
            s.append(&outputs[input_index]);
            dhash256(&s.out())
        },
        _ => 0u8.into(),
    }
}

/// Radiant output-summaries digest (R58.3.2): double-SHA256 over a 76-byte summary per output in
/// scope, or 32 zero bytes when the base type selects none.
fn hash_output_summaries(sh: Sighash, input_index: usize, outputs: &[TransactionOutput]) -> H256 {
    let mut s = Stream::default();
    match sh.base {
        SighashBase::All => {
            for o in outputs {
                append_output_summary(&mut s, o);
            }
        },
        SighashBase::Single if input_index < outputs.len() => append_output_summary(&mut s, &outputs[input_index]),
        _ => return 0u8.into(),
    }
    dhash256(&s.out())
}

fn append_output_summary(s: &mut Stream, output: &TransactionOutput) {
    let refs = extract_push_refs(&output.script_pubkey);
    s.append(&output.value).append(&dhash256(&output.script_pubkey));
    // The count is the number of distinct references; a script cannot hold 2^32 of them, and the
    // width is the 4-byte field of the summary layout.
    s.append(&(refs.len() as u32));
    if refs.is_empty() {
        // Not dsha256 of the empty string (R58.3.3).
        s.append(&H256::default());
    } else {
        let mut concatenated = Vec::with_capacity(refs.len() * REF_LEN);
        for r in &refs {
            concatenated.extend_from_slice(r);
        }
        s.append(&dhash256(&concatenated));
    }
}

const REF_LEN: usize = 36;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;
const OP_PUSHINPUTREF: u8 = 0xd0;
const OP_REQUIREINPUTREF: u8 = 0xd1;
const OP_DISALLOWPUSHINPUTREF: u8 = 0xd2;
const OP_DISALLOWPUSHINPUTREFSIBLING: u8 = 0xd3;
const OP_PUSHINPUTREFSINGLETON: u8 = 0xd8;

/// Distinct references pushed by a script, ascending by their 36 bytes (R58.3.3). Only
/// `OP_PUSHINPUTREF` and `OP_PUSHINPUTREFSINGLETON` contribute. The walk is tolerant: an operand that
/// runs past the end of the script (or whose length does not fit) stops it, keeping what was found.
fn extract_push_refs(script: &[u8]) -> BTreeSet<[u8; REF_LEN]> {
    let mut refs = BTreeSet::new();
    let mut pc = 0usize;
    while let Some(&op) = script.get(pc) {
        pc += 1;
        let (header, data_len) = match op {
            0x01..=0x4b => (0, Some(usize::from(op))),
            OP_PUSHDATA1 => (1, read_le_len(script, pc, 1)),
            OP_PUSHDATA2 => (2, read_le_len(script, pc, 2)),
            OP_PUSHDATA4 => (4, read_le_len(script, pc, 4)),
            OP_PUSHINPUTREF
            | OP_REQUIREINPUTREF
            | OP_DISALLOWPUSHINPUTREF
            | OP_DISALLOWPUSHINPUTREFSIBLING
            | OP_PUSHINPUTREFSINGLETON => (0, Some(REF_LEN)),
            _ => (0, Some(0)),
        };
        let operand = data_len
            .and_then(|len| pc.checked_add(header)?.checked_add(len).map(|end| (pc + header, end)))
            .and_then(|(start, end)| script.get(start..end).map(|bytes| (end, bytes)));
        let Some((end, bytes)) = operand else { break };
        if matches!(op, OP_PUSHINPUTREF | OP_PUSHINPUTREFSINGLETON) {
            if let Ok(r) = <[u8; REF_LEN]>::try_from(bytes) {
                refs.insert(r);
            }
        }
        pc = end;
    }
    refs
}

/// Little-endian length of `width` bytes at `pos`; `None` when it is cut off or does not fit `usize`.
fn read_le_len(script: &[u8], pos: usize, width: usize) -> Option<usize> {
    let bytes = script.get(pos..pos.checked_add(width)?)?;
    let mut value = 0u64;
    for (i, b) in bytes.iter().enumerate() {
        value |= u64::from(*b) << (8 * i);
    }
    usize::try_from(value).ok()
}

fn blake2b_personal(input: &[u8], personal: &[u8]) -> H256 {
    let bytes = Blake2b::new()
        .hash_length(32)
        .personal(personal)
        .to_state()
        .update(input)
        .finalize();
    H256::from(bytes.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{blake2b_personal, Sighash, SighashBase, SignatureVersion, SignerHashAlgo, TransactionInputSigner,
                UnsignedTransactionInput};
    use crate::bytes::Bytes;
    use crate::hash::{H160, H256};
    use crate::script::Script;
    use chain::{OutPoint, Transaction, TransactionOutput};
    use keys::{Address, AddressHashEnum, Private};

    // Reference vectors lifted from public Bitcoin docs:
    //   * http://www.righto.com/2014/02/bitcoins-hard-way-using-raw-bitcoin.html
    //   * https://github.com/zcash/zips/blob/master/zip-0243.rst (Sapling test vectors)

    #[test]
    fn legacy_sighash_matches_bitcoin_reference() {
        let _priv: Private = "5HusYj2b2x4nroApgfvaSfKYZhRbKFH41bVyPooymbC6KfgSXdD".into();
        let prev_tx = H256::from_reversed_str("81b4c832d70cb56ff957589752eb4125a4cab78a25a8fc52d6a09e5bd4404d48");
        let to: Address = "1KKKK6N21XKo48zWKuQKXdvSsCf95ibHFa".into();
        let prev_script_pubkey: Script = "76a914df3bd30160e6c6145baaf2c88a8844c13a00d1d588ac".into();
        let cur_output: Bytes = "76a914c8e90996c7c6080ee06284600c684ed904d14c5c88ac".into();
        let value = 91_234u64;
        let want: H256 = "5fda68729a6312e17e641e9a49fac2a4a6a680126610af573caab270d232f850".into();

        let mut to_hash = H160::default();
        if let AddressHashEnum::AddressHash(h) = to.hash {
            to_hash = h;
        }
        assert_eq!(&cur_output[3..23], &*to_hash);

        let signer = TransactionInputSigner {
            version: 1,
            n_time: None,
            overwintered: false,
            version_group_id: 0,
            consensus_branch_id: 0,
            expiry_height: 0,
            value_balance: 0,
            lock_time: 0,
            inputs: vec![UnsignedTransactionInput {
                sequence: 0xffff_ffff,
                previous_output: OutPoint {
                    index: 0,
                    hash: prev_tx,
                },
                amount: 0,
                witness: vec![Vec::new()],
            }],
            outputs: vec![TransactionOutput {
                value,
                script_pubkey: cur_output,
            }],
            join_splits: vec![],
            shielded_spends: vec![],
            shielded_outputs: vec![],
            zcash: false,
            str_d_zeel: None,
            hash_algo: SignerHashAlgo::DSHA256,
        };
        let got = signer.signature_hash(
            0,
            0,
            &prev_script_pubkey,
            SignatureVersion::Base,
            SighashBase::All.into(),
        );
        assert_eq!(got, want);
    }

    #[test]
    fn sighash_flag_validation() {
        for (v, raw, ok) in [
            (SignatureVersion::Base, 0xFFFF_FF82, false),
            (SignatureVersion::Base, 0x0000_0182, false),
            (SignatureVersion::Base, 0x0000_0080, false),
            (SignatureVersion::Base, 0x0000_0001, true),
            (SignatureVersion::Base, 0x0000_0082, true),
            (SignatureVersion::Base, 0x0000_0003, true),
            (SignatureVersion::ForkId, 0xFFFF_FFC2, false),
            (SignatureVersion::ForkId, 0x0000_01C2, false),
            (SignatureVersion::ForkId, 0x0000_0081, true),
            (SignatureVersion::ForkId, 0x0000_00C2, true),
            (SignatureVersion::ForkId, 0x0000_0043, true),
        ] {
            assert_eq!(Sighash::is_defined(v, raw), ok, "v={:?} raw=0x{:08x}", v, raw);
        }
    }

    #[test]
    fn blake2b_personal_zip243_anchor() {
        let h = blake2b_personal(b"", b"ZcashPrevoutHash");
        assert_eq!(
            H256::from("d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136"),
            h
        );
    }

    #[test]
    fn sapling_sighash_zip243_vector_1() {
        let tx: Transaction = "0400008085202f8901a8c685478265f4c14dada651969c45a65e1aeb8cd6791f2f5bb6a1d9952104d9010000006b483045022100a61e5d557568c2ddc1d9b03a7173c6ce7c996c4daecab007ac8f34bee01e6b9702204d38fdc0bcf2728a69fde78462a10fb45a9baa27873e6a5fc45fb5c76764202a01210365ffea3efa3908918a8b8627724af852fc9b86d7375b103ab0543cf418bcaa7ffeffffff02005a6202000000001976a9148132712c3ff19f3a151234616777420a6d7ef22688ac8b959800000000001976a9145453e4698f02a38abdaa521cd1ff2dee6fac187188ac29b0040048b004000000000000000000000000".into();
        let mut signer = TransactionInputSigner::from(tx);
        signer.inputs[0].amount = 50_000_000;
        signer.consensus_branch_id = 0x76b8_09bb;

        let sh = Sighash::from_u32(SignatureVersion::Base, 1);
        let got = signer
            .signature_hash_overwintered(
                0,
                &Script::from("1976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac"),
                1,
                sh,
            )
            .unwrap();
        assert_eq!(
            H256::from("f27411aa9bd02879181c763a80bdb6f9ea9158f0de71757e7e12ed17760ebe3f"),
            got
        );
    }

    #[test]
    fn sapling_sighash_dispatch_via_signature_hash() {
        let tx: Transaction = "0400008085202f89012c07a03638d9cf4d2cc837784b3b06aa9a5c8b819f7cb0d373bf711108f4c0f2010000006b483045022100fceec7ffa2686377fa2e13d43aa1d8836c3b5ace5292dd2f65a75befec2660bd02205dc000c13a89975bf3fe85aa9c891fcdea6eb25bd5459ad204fe2946d22e49c3012102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ffffffff0240420f00000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788ac7c288800000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788ac00000000000000000000000000000000000000".into();
        let mut signer = TransactionInputSigner::from(tx);
        signer.inputs[0].amount = 9_924_260;
        signer.consensus_branch_id = 0x76b8_09bb;
        let got = signer.signature_hash(
            0,
            0,
            &Script::from("76a91405aab5342166f8594baf17a7d9bef5d56744332788ac"),
            SignatureVersion::Base,
            1,
        );
        assert_eq!(
            H256::from("047da0d9932545770fc570122c4451b53fadad219650008e5026162e957a46f9"),
            got
        );
    }
}

#[cfg(test)]
mod rxd_tests {
    use super::{append_output_summary, extract_push_refs, hash_output_summaries, hash_outputs, hash_prevouts,
                hash_sequence, Sighash, SignatureVersion, TransactionInputSigner};
    use crate::bytes::Bytes;
    use crate::hash::H256;
    use crate::script::Script;
    use chain::{Transaction, TransactionOutput};
    use crypto::dhash256;
    use keys::{Public, Signature};
    use serialization::Stream;

    // Provenance of the two vectors below: Radiant mainnet transactions the network accepted, fetched
    // 2026-10-25 over the Electrum protocol from electrumx.radiant4people.com:50012. They are
    // public-network artefacts (CRD ch.01 R6). Transaction 10fa6331... is in block 475700 (two P2PKH
    // inputs, outputs push no reference); e9da468d... is in block 475790 (outputs push references in
    // non-ascending script order, with reference opcode bytes hidden inside PUSHDATA1 payloads).
    // The expected digests were computed by an independent hand recomputation from CRD ch.58 and
    // each input's own signature was checked against them.
    const TX1_ID: &str = "10fa6331635fadede304b279cc3f823a65f54a2f592e9c157d7fea6229232521";
    const TX1: &str = "0200000002b0150c136eb6286b0187dc55149b8e6ff26e61af26edbd47caff2e80b965848e010000006a4730440220681d0dabcf9aa083498008cebdcf926d546b56f6c5e676cd3034b9d12452b9a5022038fee529a1d781341b4bec1819ce1e067917c7a3ad2d7bb67454efef96bfdfab412102f1997e7a56d2834d89cf697dd5eb12d228ae3d5a17fdb1c6bcd900b10b5667f4feffffff422f344c8333e920065ced52aea84cd694207f1f6402e8389548c780117a42d9000000006a4730440220266d67624ebb35211961782cb09635f7438bf8a7d616758dc80b4a28093f062402201d16a23728fc67a22ed1c4958bf677281eb1aec93273a01fc526983c62bafd624121021968e5df52efdca3d8ec45c0b838dc132786e89a4c38850ed959f7bf64abc2d6feffffff0270c98900000000001976a91462bcbeed7ef54873c1cefd70fd98af889a020f1288ac8066ab13000000001976a91414514f1c96e1f7c613eec0a49867b029b32fc74f88ac33420700";
    const TX2_ID: &str = "e9da468db4109e5bc3d99edeb82b73d6547642906f37612675095e1b22b9a0fc";
    const TX2: &str = "010000000274a8bfddb4354218bd46e2640f2c5982125fc7525dd256b03b2aa80b40ff07b6000000004c08e2f9b67a00000000204ef39d426cdf48cb61667696d6f28b7c119f5df6c3b5a93cbee21c6ede1b858e201c4df8b93c52c246f6882b7846f16806ff945d5b1f3a7bd74d017ac23179890900ffffffff74a8bfddb4354218bd46e2640f2c5982125fc7525dd256b03b2aa80b40ff07b6030000006b4830450221008e14e1be55fa5d35bb97510e6ae7c83197343026aadc0dbe01705f1ee654efce02202b6fe57a6823e1ce6c002bf80e4ae506a4ecae9ce8b6ed3013453293ba66465f4121022a85db076c6f842c0d054787a459fa9d69441f454bb536ff386287836ad7bc70ffffffff040100000000000000fd5c0203628000d825a683246cf86b74c3ec2336b7c55ce57e0b082d4cc73561dfda2b979db49ebf01000000d025a683246cf86b74c3ec2336b7c55ce57e0b082d4cc73561dfda2b979db49ebf000000000350340301645152013c041b6edd6a08f8ffffffffffff0fbdc0c859797ea85d795d797ea87e5e7a7eeebc01147f77587f040000000088817600a26951797ca269c5527994537994011e967654a06375546876548f9f6375548f687600a0637600a0638c7c7608ffffffffffffff3fa0637508ffffffffffffff7f678d687c687600a0638c7c7608ffffffffffffff3fa0637508ffffffffffffff7f678d687c687600a0638c7c7608ffffffffffffff3fa0637508ffffffffffffff7f678d687c687600a0638c7c7608ffffffffffffff3fa0637508ffffffffffffff7f678d687c68756776009f638f7600a0638c7c8e7c687600a0638c7c8e7c687600a0638c7c8e7c687600a0638c7c8e7c68756775686876519f637551686b75757575577ae500a069567ae600a06901d053797e0cdec0e9aa76e378e4a269e69d7eaa76e47b9d547a818b76537a9c537ade789181547ae6939d636c755279cd01d853797e016a7e886778de519d7676009c63750100677660a163015093518067827c7e68684c54d825a683246cf86b74c3ec2336b7c55ce57e0b082d4cc73561dfda2b979db49ebf01000000d025a683246cf86b74c3ec2336b7c55ce57e0b082d4cc73561dfda2b979db49ebf000000000350340301645152013c7ec55480547c7e7e6c76009c63750100677660a163015093518067827c7e68687e5379ec78885379eac0e9885379cc519d75686d755164000000000000004b76a914ff606b3e464c18478976f141057249e24c51055a88acbdd025a683246cf86b74c3ec2336b7c55ce57e0b082d4cc73561dfda2b979db49ebf00000000dec0e9aa76e378e4a269e69d0000000000000000086a036d7367020035c4e4ac85150000001976a9147a3e2126ae59d36dfb0650b06f53484cac95e3c888ac1b6edd6a";

    struct RxdInput {
        index: usize,
        prev_script: &'static str,
        value: u64,
        digest: &'static str,
    }

    const TX1_INPUTS: &[RxdInput] = &[
        RxdInput {
            index: 0,
            prev_script: "76a9142d9b8aee74718e1c2093516291f38758e410e57d88ac",
            value: 284000000,
            digest: "10299a1f5c52f50fdd24d63d2f489ced66d62392c3a6a7521e72e46eb36f9e70",
        },
        RxdInput {
            index: 1,
            prev_script: "76a914d7bd0bb458d1b05e7c9313bd78beeb406bb9097d88ac",
            value: 58750000,
            digest: "c75df96bf6805c18497d0d59d2e4cba458ac9cc41941db160e60e33530df0bdb",
        },
    ];
    const TX2_INPUTS: &[RxdInput] = &[RxdInput {
        index: 1,
        prev_script: "76a9147a3e2126ae59d36dfb0650b06f53484cac95e3c888ac",
        value: 92447776300,
        digest: "0a49d668be3371153f289706073caeed9245a6d10fefb6c4a247245eb937beb6",
    }];

    fn signer_of(tx_hex: &'static str) -> (Transaction, TransactionInputSigner) {
        let tx: Transaction = tx_hex.into();
        let signer = TransactionInputSigner::from(tx.clone());
        (tx, signer)
    }

    fn bytes_of(hex: &'static str) -> Vec<u8> { Bytes::from(hex).to_vec() }

    /// `[len][signature || sighash type][len][pubkey]` from a P2PKH scriptSig.
    fn sig_and_pubkey(script_sig: &[u8]) -> (Signature, Public) {
        let sig_len = script_sig[0] as usize;
        let sig = &script_sig[1..1 + sig_len];
        let pk_len = script_sig[1 + sig_len] as usize;
        let pk = &script_sig[2 + sig_len..2 + sig_len + pk_len];
        assert_eq!(script_sig.len(), 2 + sig_len + pk_len);
        assert_eq!(sig[sig_len - 1], 0x41);
        (Signature::from(&sig[..sig_len - 1]), Public::from_slice(pk).unwrap())
    }

    fn verifies(tx: &Transaction, index: usize, digest: &H256) -> bool {
        let (sig, pk) = sig_and_pubkey(&tx.inputs[index].script_sig);
        pk.verify(digest, &sig).unwrap()
    }

    fn check_vectors(tx_hex: &'static str, inputs: &[RxdInput]) {
        let (tx, signer) = signer_of(tx_hex);
        for i in inputs {
            let digest = signer.signature_hash(
                i.index,
                i.value,
                &Script::from(i.prev_script),
                SignatureVersion::ForkIdRxd,
                0x41,
            );
            assert_eq!(&digest[..], &bytes_of(i.digest)[..], "input {}", i.index);
            assert!(verifies(&tx, i.index, &digest), "input {} signature", i.index);
        }
    }

    #[test]
    fn t3_mainnet_vector_zero_reference_outputs() { check_vectors(TX1, TX1_INPUTS); }

    #[test]
    fn t3_mainnet_vector_reference_bearing_outputs() { check_vectors(TX2, TX2_INPUTS); }

    /// Preimage with the output-summaries field placed by `place`: before the outputs digest (the
    /// dialect), after the lock time, or omitted (the plain fork-id BIP-143 form).
    fn preimage_digest(signer: &TransactionInputSigner, i: &RxdInput, place: Option<bool>) -> H256 {
        let sh = Sighash::from_u32(SignatureVersion::ForkIdRxd, 0x41);
        let inp = &signer.inputs[i.index];
        let summaries = hash_output_summaries(sh, i.index, &signer.outputs);
        let mut s = Stream::default();
        s.append(&signer.version)
            .append(&hash_prevouts(sh, &signer.inputs))
            .append(&hash_sequence(sh, &signer.inputs))
            .append(&inp.previous_output)
            .append_list(&Script::from(i.prev_script))
            .append(&i.value)
            .append(&inp.sequence);
        if place == Some(true) {
            s.append(&summaries);
        }
        s.append(&hash_outputs(sh, i.index, &signer.outputs))
            .append(&signer.lock_time);
        if place == Some(false) {
            s.append(&summaries);
        }
        s.append(&0x41u32);
        dhash256(&s.out())
    }

    #[test]
    fn t4_field_8_position_matters() {
        for (tx_hex, inputs) in [(TX1, TX1_INPUTS), (TX2, TX2_INPUTS)] {
            let (tx, signer) = signer_of(tx_hex);
            for i in inputs {
                // The hand-built preimage with field 8 in place reproduces the vector (so the helper is sound).
                let right = preimage_digest(&signer, i, Some(true));
                assert_eq!(&right[..], &bytes_of(i.digest)[..]);
                assert!(verifies(&tx, i.index, &right));
                let after_lock_time = preimage_digest(&signer, i, Some(false));
                assert_ne!(after_lock_time, right);
                assert!(
                    !verifies(&tx, i.index, &after_lock_time),
                    "field 8 after lock time must not verify"
                );
                let omitted = preimage_digest(&signer, i, None);
                assert_ne!(omitted, right);
                assert!(!verifies(&tx, i.index, &omitted), "no field 8 must not verify");
                // ... and the omitted form is exactly the existing fork-id result.
                let fork_id = signer.signature_hash(
                    i.index,
                    i.value,
                    &Script::from(i.prev_script),
                    SignatureVersion::ForkId,
                    0x41,
                );
                assert_eq!(fork_id, omitted);
            }
        }
    }

    #[test]
    fn t2_gate_routing_and_strict_encoding() {
        let (_, signer) = signer_of(TX1);
        let i = &TX1_INPUTS[0];
        let script = Script::from(i.prev_script);
        let digest = |v, raw| signer.signature_hash(i.index, i.value, &script, v, raw);
        // Bit 0x40 set: the Radiant dialect, which differs from the plain fork-id digest.
        assert_eq!(&digest(SignatureVersion::ForkIdRxd, 0x41)[..], &bytes_of(i.digest)[..]);
        assert_ne!(
            digest(SignatureVersion::ForkIdRxd, 0x41),
            digest(SignatureVersion::ForkId, 0x41)
        );
        // Bit clear: both fork-id values fall back to the base algorithm.
        assert_eq!(
            digest(SignatureVersion::ForkIdRxd, 0x01),
            digest(SignatureVersion::Base, 0x01)
        );
        assert_eq!(
            digest(SignatureVersion::ForkIdRxd, 0x01),
            digest(SignatureVersion::ForkId, 0x01)
        );

        for raw in [0x41u32, 0x43, 0x81, 0xC2, 0x01, 0x02, 0x03, 0xC3] {
            assert!(Sighash::is_defined(SignatureVersion::ForkIdRxd, raw), "0x{raw:x}");
        }
        for raw in [
            0x00u32,
            0x04,
            0x1F | 0x40,
            0x0000_0141,
            0x0000_01C2,
            0xFFFF_FFC2,
            0x40,
            0x80,
            0xC0,
        ] {
            assert!(!Sighash::is_defined(SignatureVersion::ForkIdRxd, raw), "0x{raw:x}");
        }
        assert!(Sighash::from_u32(SignatureVersion::ForkIdRxd, 0x41).fork_id);
        assert!(!Sighash::from_u32(SignatureVersion::ForkIdRxd, 0x01).fork_id);
        assert!(!Sighash::from_u32(SignatureVersion::WitnessV0, 0x41).fork_id);
    }

    #[test]
    fn t5_zero_reference_summary() {
        let script = bytes_of("76a91462bcbeed7ef54873c1cefd70fd98af889a020f1288ac");
        let output = TransactionOutput {
            value: 0x0102_0304_0506_0708,
            script_pubkey: Bytes::from(script.clone()),
        };
        let mut s = Stream::default();
        append_output_summary(&mut s, &output);
        let got = s.out().to_vec();
        assert_eq!(got.len(), 76);
        let mut want = 0x0102_0304_0506_0708u64.to_le_bytes().to_vec();
        want.extend_from_slice(&dhash256(&script)[..]);
        want.extend_from_slice(&0u32.to_le_bytes());
        want.extend_from_slice(&[0u8; 32]);
        assert_eq!(got, want);
        assert_ne!(&got[44..], &dhash256(b"")[..]);
    }

    // T6 is synthetic: no on-chain vector exercises duplicate references or the skipped opcodes
    // 0xd1-0xd3 (the mainnet vector covers ordering and embedded opcode bytes only).
    fn push36(op: u8, fill: u8) -> Vec<u8> {
        let mut v = vec![op];
        v.extend_from_slice(&[fill; 36]);
        v
    }

    #[test]
    fn t6_reference_extraction() {
        let mut script = Vec::new();
        script.extend(push36(0xd8, 0x30)); // singleton
        script.extend(push36(0xd0, 0x10));
        script.extend(push36(0xd1, 0x01)); // require: skipped
        script.extend(push36(0xd2, 0x02)); // disallow push: skipped
        script.extend(push36(0xd3, 0x03)); // disallow sibling: skipped
        script.extend(push36(0xd0, 0x20));
        script.extend(push36(0xd0, 0x10)); // duplicate of an earlier reference
                                           // Direct data push whose payload looks like a reference push.
        script.push(37);
        script.extend(push36(0xd0, 0x05));
        // PUSHDATA1 payload with the same shape.
        script.extend([0x4c, 37]);
        script.extend(push36(0xd8, 0x06));
        script.extend([0x76, 0xa9, 0x88, 0xac]);

        let refs: Vec<_> = extract_push_refs(&script).into_iter().collect();
        assert_eq!(refs, vec![[0x10u8; 36], [0x20u8; 36], [0x30u8; 36]]);

        // Truncated tail: references collected before the truncation survive, nothing fails.
        let mut cut = script.clone();
        cut.extend(push36(0xd0, 0x40)[..20].to_vec());
        assert_eq!(extract_push_refs(&cut).len(), 3);
        // A PUSHDATA4 whose length cannot fit, a cut-off length field, and an empty script.
        let mut big = push36(0xd0, 0x50);
        big.extend([0x4e, 0xff, 0xff, 0xff, 0xff, 0xaa]);
        big.extend(push36(0xd0, 0x60));
        assert_eq!(extract_push_refs(&big).into_iter().collect::<Vec<_>>(), vec![
            [0x50u8; 36]
        ]);
        assert_eq!(extract_push_refs(&[0x4d, 0x01]).len(), 0);
        assert_eq!(extract_push_refs(&[]).len(), 0);
    }

    #[test]
    fn t7_degenerate_indices_yield_the_sentinel() {
        let (_, mut signer) = signer_of(TX2);
        let i = &TX2_INPUTS[0];
        let script = Script::from(i.prev_script);
        let sentinel: H256 = 1u8.into();
        for v in [SignatureVersion::ForkId, SignatureVersion::ForkIdRxd] {
            assert_eq!(signer.signature_hash(7, i.value, &script, v, 0x41), sentinel);
            let mut s = signer.clone();
            s.outputs.truncate(1);
            assert_eq!(s.signature_hash(i.index, i.value, &script, v, 0x43), sentinel);
        }
        // Single with an in-range output index is not degenerate.
        assert_ne!(
            signer.signature_hash(0, i.value, &script, SignatureVersion::ForkIdRxd, 0x43),
            sentinel
        );
        signer.outputs.clear();
        assert_eq!(
            signer.signature_hash(0, i.value, &script, SignatureVersion::ForkIdRxd, 0x43),
            sentinel
        );
        assert_ne!(
            signer.signature_hash(0, i.value, &script, SignatureVersion::ForkIdRxd, 0x41),
            sentinel
        );
    }

    #[test]
    fn t8_transaction_hash_is_double_sha256() {
        for (raw, id) in [(TX1, TX1_ID), (TX2, TX2_ID)] {
            let tx: Transaction = raw.into();
            let serialized = bytes_of(raw);
            assert_eq!(tx.hash(), H256::from_reversed_str(id));
            assert_eq!(tx.hash(), dhash256(&serialized));
        }
    }
}
