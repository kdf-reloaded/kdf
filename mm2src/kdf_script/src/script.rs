// Bitcoin script wrapper, parser, and classifier.
//
// `Script` is a thin newtype around a byte buffer. We provide:
//   * iteration over (opcode, optional pushdata payload) pairs
//   * standard P2PK/P2PKH/P2SH/P2WPKH/P2WSH/multisig/null-data
//     classification
//   * destination extraction for the standard types
//
// The shape of the output matches what KDF consumers expect (their
// import lines `use script::{Script, ScriptType, ScriptAddress, ...};`
// drive this surface). The interpreter — script execution, signature
// verification — is intentionally not part of this crate.

use crate::bytes::Bytes;
use crate::{Error, Opcode};
use keys::{self, AddressHashEnum, Public};
use std::{fmt, ops};

/// Maximum number of public keys allowed in a Bitcoin standard
/// multisig script (consensus rule from Bitcoin Core).
pub const MAX_PUBKEYS_PER_MULTISIG: usize = 20;

/// Standard script classification. Variants correspond 1:1 to the
/// classifier predicates on `Script`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ScriptType {
    NonStandard,
    PubKey,
    PubKeyHash,
    ScriptHash,
    Multisig,
    NullData,
    WitnessScript,
    WitnessKey,
    WitnessV1Taproot,
    // Qtum AAL (account-abstraction layer) scripts.
    CallSender,
    CreateSender,
    Call,
    Create,
}

/// Address derived from a classified script.
#[derive(Debug, PartialEq, Clone)]
pub struct ScriptAddress {
    pub kind: keys::Type,
    pub hash: AddressHashEnum,
}

impl ScriptAddress {
    pub fn new_p2pkh(hash: AddressHashEnum) -> Self {
        Self {
            kind: keys::Type::P2PKH,
            hash,
        }
    }
    pub fn new_p2sh(hash: AddressHashEnum) -> Self {
        Self {
            kind: keys::Type::P2SH,
            hash,
        }
    }
    pub fn new_p2wpkh(hash: AddressHashEnum) -> Self {
        Self {
            kind: keys::Type::P2WPKH,
            hash,
        }
    }
    pub fn new_p2wsh(hash: AddressHashEnum) -> Self {
        Self {
            kind: keys::Type::P2WSH,
            hash,
        }
    }
}

/// Witness stack carried by a segwit input.
pub type ScriptWitness = Vec<Bytes>;

/// Serialized script bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    data: Bytes,
}

impl Script {
    pub fn new(data: Bytes) -> Self { Self { data } }
    pub fn to_bytes(&self) -> Bytes { self.data.clone() }
    pub fn is_empty(&self) -> bool { self.data.is_empty() }

    // --- Standard pattern predicates ---------------------------------

    /// `OP_DUP OP_HASH160 <20> OP_EQUALVERIFY OP_CHECKSIG`
    pub fn is_pay_to_public_key_hash(&self) -> bool {
        self.data.len() == 25
            && self.data[0] == Opcode::OP_DUP as u8
            && self.data[1] == Opcode::OP_HASH160 as u8
            && self.data[2] == Opcode::OP_PUSHBYTES_20 as u8
            && self.data[23] == Opcode::OP_EQUALVERIFY as u8
            && self.data[24] == Opcode::OP_CHECKSIG as u8
    }

    /// `<33|65> OP_CHECKSIG`
    pub fn is_pay_to_public_key(&self) -> bool {
        match self.data.first().copied() {
            Some(b) if b == Opcode::OP_PUSHBYTES_33 as u8 => {
                self.data.len() == 35 && self.data[34] == Opcode::OP_CHECKSIG as u8
            },
            Some(b) if b == Opcode::OP_PUSHBYTES_65 as u8 => {
                self.data.len() == 67 && self.data[66] == Opcode::OP_CHECKSIG as u8
            },
            _ => false,
        }
    }

    /// `OP_HASH160 <20> OP_EQUAL`
    pub fn is_pay_to_script_hash(&self) -> bool {
        self.data.len() == 23
            && self.data[0] == Opcode::OP_HASH160 as u8
            && self.data[1] == Opcode::OP_PUSHBYTES_20 as u8
            && self.data[22] == Opcode::OP_EQUAL as u8
    }

    /// `OP_0 <20>`
    pub fn is_pay_to_witness_key_hash(&self) -> bool {
        self.data.len() == 22 && self.data[0] == Opcode::OP_0 as u8 && self.data[1] == Opcode::OP_PUSHBYTES_20 as u8
    }

    /// `OP_0 <32>`
    pub fn is_pay_to_witness_script_hash(&self) -> bool {
        self.data.len() == 34 && self.data[0] == Opcode::OP_0 as u8 && self.data[1] == Opcode::OP_PUSHBYTES_32 as u8
    }

    /// `OP_1 <32>` — a witness v1 (Taproot, BIP-341) output. Recognition
    /// only: KDF does not yet spend Taproot inputs, but it must classify
    /// such outputs so verbose transactions carrying them parse cleanly.
    pub fn is_pay_to_witness_v1_taproot(&self) -> bool {
        self.data.len() == 34 && self.data[0] == Opcode::OP_1 as u8 && self.data[1] == Opcode::OP_PUSHBYTES_32 as u8
    }

    /// Decode a witness program: returns `Some((version, program_bytes))`
    /// for any 4..=42-byte script whose layout matches `OP_n PUSHBYTES_k <k bytes>`
    /// where `n` is the witness version and the push length matches the trailing payload.
    pub fn parse_witness_program(&self) -> Option<(u8, &[u8])> {
        let len = self.data.len();
        if !(4..=42).contains(&len) {
            return None;
        }
        let advertised = self.data[1] as usize;
        if advertised + 2 != len {
            return None;
        }
        let version = match Opcode::from_u8(self.data[0])? {
            Opcode::OP_0 => 0,
            op if op >= Opcode::OP_1 && op <= Opcode::OP_16 => (op as u8) - (Opcode::OP_1 as u8) + 1,
            _ => return None,
        };
        Some((version, &self.data[2..]))
    }

    /// Bitcoin standard multisig: `OP_M <pk1>...<pkN> OP_N OP_CHECKMULTISIG`
    pub fn is_multisig_script(&self) -> bool {
        if self.data.len() < 3 {
            return false;
        }
        let head = match self.opcode_at(0) {
            Ok(Opcode::OP_0) => 0u8,
            Ok(op) if op.is_within_op_n() => op.decode_op_n(),
            _ => return false,
        };
        let tail = match self.opcode_at(self.data.len() - 2) {
            Ok(Opcode::OP_0) => 0u8,
            Ok(op) if op.is_within_op_n() => op.decode_op_n(),
            _ => return false,
        };
        if head > tail {
            return false;
        }
        if *self.data.last().unwrap() != Opcode::OP_CHECKMULTISIG as u8 {
            return false;
        }
        // Walk the body: every push must be 33 or 65 bytes (a pubkey).
        let mut pc = 1;
        let mut keys_seen = 0u8;
        while pc + 1 < self.data.len() - 1 {
            let instr = match self.instruction_at(pc) {
                Ok(i) => i,
                Err(_) => return false,
            };
            match instr.opcode {
                Opcode::OP_PUSHBYTES_33 | Opcode::OP_PUSHBYTES_65 => keys_seen += 1,
                _ => return false,
            }
            pc += instr.step;
        }
        keys_seen == tail
    }

    /// `OP_RETURN <push-only payload>`
    pub fn is_null_data_script(&self) -> bool {
        !self.data.is_empty() && self.data[0] == Opcode::OP_RETURN as u8 && self.subscript(1).is_push_only()
    }

    /// True if every instruction in this script is a pure data push.
    pub fn is_push_only(&self) -> bool {
        let mut pc = 0;
        while pc < self.data.len() {
            let instr = match self.instruction_at(pc) {
                Ok(i) => i,
                Err(_) => return false,
            };
            if instr.opcode > Opcode::OP_16 {
                return false;
            }
            pc += instr.step;
        }
        true
    }

    /// Run all classifier predicates and return the first match.
    pub fn script_type(&self) -> ScriptType {
        if self.is_pay_to_public_key() {
            ScriptType::PubKey
        } else if self.is_pay_to_public_key_hash() {
            ScriptType::PubKeyHash
        } else if self.is_pay_to_script_hash() {
            ScriptType::ScriptHash
        } else if self.is_multisig_script() {
            ScriptType::Multisig
        } else if self.is_null_data_script() {
            ScriptType::NullData
        } else if self.is_pay_to_witness_key_hash() {
            ScriptType::WitnessKey
        } else if self.is_pay_to_witness_script_hash() {
            ScriptType::WitnessScript
        } else if self.is_pay_to_witness_v1_taproot() {
            ScriptType::WitnessV1Taproot
        } else {
            ScriptType::NonStandard
        }
    }

    /// Slice from `from` to end, as a fresh `Script`.
    pub fn subscript(&self, from: usize) -> Script { self.data[from..].to_vec().into() }

    /// Strip every occurrence of `data` from the script bytes. Used by
    /// the legacy SIGHASH algorithm to splice out the matching
    /// signature before hashing the script.
    pub fn find_and_delete(&self, needle: &[u8]) -> Script {
        if needle.is_empty() || needle.len() > self.data.len() {
            return self.data.to_vec().into();
        }
        let mut out = Vec::with_capacity(self.data.len());
        let mut i = 0;
        let last = self.data.len().saturating_sub(needle.len());
        while i <= last {
            if &self.data[i..i + needle.len()] == needle {
                i += needle.len();
            } else {
                out.push(self.data[i]);
                i += 1;
            }
        }
        out.extend_from_slice(&self.data[i..]);
        out.into()
    }

    /// Strip every `OP_CODESEPARATOR` opcode from the script.
    pub fn without_separators(&self) -> Script {
        let mut out = Vec::with_capacity(self.data.len());
        let mut pc = 0;
        while pc < self.data.len() {
            match self.instruction_at(pc) {
                Ok(instr) => {
                    if instr.opcode != Opcode::OP_CODESEPARATOR {
                        out.extend_from_slice(&self.data[pc..pc + instr.step]);
                    }
                    pc += instr.step;
                },
                Err(_) => {
                    out.push(self.data[pc]);
                    pc += 1;
                },
            }
        }
        out.into()
    }

    /// Decode the opcode byte at `pos` (no payload parsing).
    pub fn get_opcode(&self, pos: usize) -> Result<Opcode, Error> { self.opcode_at(pos) }
    fn opcode_at(&self, pos: usize) -> Result<Opcode, Error> {
        self.data
            .get(pos)
            .copied()
            .and_then(Opcode::from_u8)
            .ok_or(Error::BadOpcode)
    }

    /// Decode the instruction starting at `pos`.
    pub fn get_instruction_at(&self, pos: usize) -> Result<Instruction<'_>, Error> { self.instruction_at(pos) }
    fn instruction_at(&self, pos: usize) -> Result<Instruction<'_>, Error> {
        let opcode = self.opcode_at(pos)?;
        match opcode {
            Opcode::OP_PUSHDATA1 | Opcode::OP_PUSHDATA2 | Opcode::OP_PUSHDATA4 => {
                let prefix_len = match opcode {
                    Opcode::OP_PUSHDATA1 => 1usize,
                    Opcode::OP_PUSHDATA2 => 2,
                    _ => 4,
                };
                let prefix = self.range(pos + 1, prefix_len)?;
                let payload_len = read_le_usize(prefix);
                let payload = self.range(pos + 1 + prefix_len, payload_len)?;
                Ok(Instruction {
                    opcode,
                    step: 1 + prefix_len + payload_len,
                    data: Some(payload),
                })
            },
            op if op <= Opcode::OP_PUSHBYTES_75 => {
                let n = op as usize;
                let payload = self.range(pos + 1, n)?;
                Ok(Instruction {
                    opcode: op,
                    step: 1 + n,
                    data: Some(payload),
                })
            },
            _ => Ok(Instruction {
                opcode,
                step: 1,
                data: None,
            }),
        }
    }

    fn range(&self, offset: usize, len: usize) -> Result<&[u8], Error> {
        self.data.get(offset..offset + len).ok_or(Error::BadOpcode)
    }

    /// nth instruction (0-indexed) by linear scan, or `None` if out of range.
    pub fn get_instruction(&self, n: usize) -> Option<Result<Instruction<'_>, Error>> { self.iter().nth(n) }

    /// Iterator over decoded `(opcode, optional payload)` pairs. Stops
    /// after yielding the first error.
    pub fn iter(&self) -> Instructions<'_> { Instructions { script: self, pos: 0 } }

    /// Iterator over opcode bytes, ignoring payloads.
    pub fn opcodes(&self) -> Opcodes<'_> { Opcodes { script: self, pos: 0 } }

    /// Count signature operations for fee/policy purposes (Bitcoin
    /// Core compatibility — `accurate=true` is BIP-16's accurate count
    /// inside a redeem script).
    pub fn sigops_count(&self, accurate: bool) -> usize {
        let mut total = 0usize;
        let mut last = Opcode::OP_0;
        for op in self.opcodes() {
            let Ok(op) = op else {
                return total;
            };
            match op {
                Opcode::OP_CHECKSIG | Opcode::OP_CHECKSIGVERIFY => total += 1,
                Opcode::OP_CHECKMULTISIG | Opcode::OP_CHECKMULTISIGVERIFY => {
                    total += if accurate && last.is_within_op_n() {
                        last.decode_op_n() as usize
                    } else {
                        MAX_PUBKEYS_PER_MULTISIG
                    };
                },
                _ => {},
            }
            last = op;
        }
        total
    }

    pub fn pay_to_script_hash_sigops(&self, prev_out: &Script) -> usize {
        if !prev_out.is_pay_to_script_hash() || self.data.is_empty() || !self.is_push_only() {
            return 0;
        }
        // Last push of a P2SH scriptSig is the redeem script.
        let last = self
            .iter()
            .filter_map(Result::ok)
            .filter_map(|i| i.data.map(<[u8]>::to_vec))
            .last();
        match last {
            Some(redeem) => Script::from(redeem).sigops_count(true),
            None => 0,
        }
    }

    /// For multisig scripts, how many signatures the script requires
    /// (`OP_M` value); 1 for everything else.
    pub fn num_signatures_required(&self) -> u8 {
        if self.is_multisig_script() {
            return match self.opcode_at(0) {
                Ok(Opcode::OP_0) => 0,
                Ok(op) if op.is_within_op_n() => op.decode_op_n(),
                _ => 1,
            };
        }
        1
    }

    /// Recover the address(es) that this script pays to, for the
    /// standard types. Empty vector for non-standard / null-data.
    pub fn extract_destinations(&self) -> Result<Vec<ScriptAddress>, keys::Error> {
        let kind = self.script_type();
        let make_p2pkh = |hash: AddressHashEnum| ScriptAddress::new_p2pkh(hash);
        match kind {
            ScriptType::PubKey => {
                // First byte is OP_PUSHBYTES_33 or _65; payload is the
                // pubkey, ending one byte before OP_CHECKSIG.
                let len = self.data.len() - 2;
                let key_bytes = &self.data[1..1 + len];
                let pk = Public::from_slice(key_bytes)?;
                Ok(vec![make_p2pkh(AddressHashEnum::AddressHash(pk.address_hash()))])
            },
            ScriptType::PubKeyHash => Ok(vec![make_p2pkh(AddressHashEnum::AddressHash(self.data[3..23].into()))]),
            ScriptType::ScriptHash => Ok(vec![ScriptAddress::new_p2sh(AddressHashEnum::AddressHash(
                self.data[2..22].into(),
            ))]),
            ScriptType::WitnessKey => Ok(vec![ScriptAddress::new_p2wpkh(AddressHashEnum::AddressHash(
                self.data[2..22].into(),
            ))]),
            ScriptType::WitnessScript => Ok(vec![ScriptAddress::new_p2wsh(AddressHashEnum::WitnessScriptHash(
                self.data[2..34].into(),
            ))]),
            ScriptType::Multisig => {
                let mut out = Vec::new();
                let mut pc = 1;
                while pc + 1 < self.data.len() - 1 {
                    let instr = self.instruction_at(pc).map_err(|_| keys::Error::InvalidPublic)?;
                    let payload = instr.data.ok_or(keys::Error::InvalidPublic)?;
                    let pk = Public::from_slice(payload)?;
                    out.push(make_p2pkh(AddressHashEnum::AddressHash(pk.address_hash())));
                    pc += instr.step;
                }
                Ok(out)
            },
            // Qtum AAL types are recognized at the type level but
            // currently have no address recovery in KDF. Witness v1
            // (Taproot) is likewise recognition-only for now.
            ScriptType::NullData
            | ScriptType::NonStandard
            | ScriptType::WitnessV1Taproot
            | ScriptType::CallSender
            | ScriptType::CreateSender
            | ScriptType::Call
            | ScriptType::Create => Ok(Vec::new()),
        }
    }
}

impl ops::Deref for Script {
    type Target = [u8];
    fn deref(&self) -> &[u8] { &self.data }
}

impl From<Bytes> for Script {
    fn from(b: Bytes) -> Self { Self::new(b) }
}
impl From<Vec<u8>> for Script {
    fn from(v: Vec<u8>) -> Self { Self::new(v.into()) }
}
impl From<Script> for Bytes {
    fn from(s: Script) -> Self { s.data }
}
impl From<&'static str> for Script {
    fn from(s: &'static str) -> Self { Self::new(s.into()) }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut pc = 0;
        while pc < self.data.len() {
            match self.instruction_at(pc) {
                Ok(i) => {
                    match i.data {
                        Some(d) => writeln!(f, "{:?} 0x{:?}", i.opcode, Bytes::from(d.to_vec()))?,
                        None => writeln!(f, "{:?}", i.opcode)?,
                    }
                    pc += i.step;
                },
                Err(e) => return e.fmt(f),
            }
        }
        Ok(())
    }
}

/// Single decoded instruction.
pub struct Instruction<'a> {
    pub opcode: Opcode,
    /// Total bytes consumed from the script (opcode + length prefix +
    /// payload).
    pub step: usize,
    /// Pushed payload, present for any push opcode.
    pub data: Option<&'a [u8]>,
}

pub struct Instructions<'a> {
    script: &'a Script,
    pos: usize,
}
impl<'a> Iterator for Instructions<'a> {
    type Item = Result<Instruction<'a>, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.script.data.len() {
            return None;
        }
        match self.script.instruction_at(self.pos) {
            Ok(i) => {
                self.pos += i.step;
                Some(Ok(i))
            },
            Err(e) => {
                self.pos = self.script.data.len();
                Some(Err(e))
            },
        }
    }
}

pub struct Opcodes<'a> {
    script: &'a Script,
    pos: usize,
}
impl<'a> Iterator for Opcodes<'a> {
    type Item = Result<Opcode, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.script.data.len() {
            return None;
        }
        match self.script.instruction_at(self.pos) {
            Ok(i) => {
                self.pos += i.step;
                Some(Ok(i.opcode))
            },
            Err(e) => {
                self.pos = self.script.data.len();
                Some(Err(e))
            },
        }
    }
}

fn read_le_usize(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .enumerate()
        .fold(0usize, |acc, (i, &b)| acc | ((b as usize) << (i * 8)))
}

/// BIP-141 witness commitment marker:
/// `OP_RETURN OP_PUSHBYTES_36 0xaa21a9ed <32-byte commitment>`
pub fn is_witness_commitment_script(script: &[u8]) -> bool {
    script.len() >= 36
        && script[0] == Opcode::OP_RETURN as u8
        && script[1] == 0x24
        && script[2..6] == [0xaa, 0x21, 0xa9, 0xed]
}

#[cfg(test)]
mod tests {
    use super::{Script, ScriptAddress, ScriptType};
    use crate::{Builder, Opcode};
    use crypto::ChecksumType;
    use keys::{Address, Public};

    #[test]
    fn detects_p2sh() {
        let s: Script = "a9143b80842f4ea32806ce5e723a255ddd6490cfd28d87".into();
        assert!(s.is_pay_to_script_hash());
        let bad: Script = "a9143b80842f4ea32806ce5e723a255ddd6490cfd28d88".into();
        assert!(!bad.is_pay_to_script_hash());
    }

    #[test]
    fn detects_p2wpkh_and_p2wsh() {
        let pkh: Script = "00140000000000000000000000000000000000000000".into();
        assert!(pkh.is_pay_to_witness_key_hash());
        let wsh: Script = "00203b80842f4ea32806ce5e723a255ddd6490cfd28dac38c58bf9254c0577330693".into();
        assert!(wsh.is_pay_to_witness_script_hash());
    }

    #[test]
    fn detects_and_classifies_p2tr() {
        // BIP-86 account 0, first receive address: `OP_1 <32-byte output key>`.
        // scriptPubKey = 5120 || tweaked output key.
        let p2tr: Script = "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c".into();
        assert!(p2tr.is_pay_to_witness_v1_taproot());
        assert!(!p2tr.is_pay_to_witness_script_hash());
        assert_eq!(p2tr.script_type(), ScriptType::WitnessV1Taproot);
    }

    #[test]
    fn classifies_standard_types() {
        let pkh = Script::from("76a914aab76ba4877d696590d94ea3e02948b55294815188ac");
        assert_eq!(pkh.script_type(), ScriptType::PubKeyHash);
        let p2sh = Script::from("a9146262b64aec1f4a4c1d21b32e9c2811dd2171fd7587");
        assert_eq!(p2sh.script_type(), ScriptType::ScriptHash);
        let pk = Script::from("4104ae1a62fe09c5f51b13905f07f06b99a2f7159b2225f374cd378d71302fa28414e7aab37397f554a7df5f142c21c1b7303b8a0626f1baded5c72a704f7e6cd84cac");
        assert_eq!(pk.script_type(), ScriptType::PubKey);
    }

    #[test]
    fn sigops_counted() {
        let pkh = Script::from("76a914aab76ba4877d696590d94ea3e02948b55294815188ac");
        assert_eq!(pkh.sigops_count(false), 1);
        let multisig = Script::from("522102004525da5546e7603eefad5ef971e82f7dad2272b34e6b3036ab1fe3d299c22f21037d7f2227e6c646707d1c61ecceb821794124363a2cf2c1d2a6f28cf01e5d6abe52ae");
        assert_eq!(multisig.sigops_count(true), 2);
        assert_eq!(multisig.sigops_count(false), 20);
    }

    #[test]
    fn extracts_p2pkh_destination() {
        let address = Address::from("13NMTpfNVVJQTNH4spP4UeqBGqLdqDo27S").hash;
        let s = Builder::build_p2pkh(&address);
        assert_eq!(s.script_type(), ScriptType::PubKeyHash);
        assert_eq!(s.extract_destinations(), Ok(vec![ScriptAddress::new_p2pkh(address)]));
    }

    #[test]
    fn extracts_p2sh_destination() {
        let address = Address::from("13NMTpfNVVJQTNH4spP4UeqBGqLdqDo27S").hash;
        let s = Builder::build_p2sh(&address);
        assert_eq!(s.script_type(), ScriptType::ScriptHash);
        assert_eq!(s.extract_destinations(), Ok(vec![ScriptAddress::new_p2sh(address)]));
    }

    #[test]
    fn extracts_multisig_destinations() {
        let pk1 = [0u8; 33];
        let a1 = Public::from_slice(&pk1).unwrap().address_hash();
        let pk2 = [1u8; 65];
        let a2 = Public::from_slice(&pk2).unwrap().address_hash();
        let s = Builder::default()
            .push_opcode(Opcode::OP_2)
            .push_bytes(&pk1)
            .push_bytes(&pk2)
            .push_opcode(Opcode::OP_2)
            .push_opcode(Opcode::OP_CHECKMULTISIG)
            .into_script();
        assert_eq!(s.script_type(), ScriptType::Multisig);
        assert_eq!(
            s.extract_destinations(),
            Ok(vec![
                ScriptAddress::new_p2pkh(a1.into()),
                ScriptAddress::new_p2pkh(a2.into()),
            ])
        );
    }

    #[test]
    fn extracts_p2wpkh_p2wsh() {
        let a = Address::from_segwitaddress(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            ChecksumType::DSHA256,
            0,
            0,
        )
        .unwrap()
        .hash;
        let s = Builder::build_witness_script(&a);
        assert_eq!(s.script_type(), ScriptType::WitnessKey);
        assert_eq!(s.extract_destinations(), Ok(vec![ScriptAddress::new_p2wpkh(a)]));

        let a2 = Address::from_segwitaddress(
            "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
            ChecksumType::DSHA256,
            0,
            0,
        )
        .unwrap()
        .hash;
        let s2 = Builder::build_witness_script(&a2);
        assert_eq!(s2.script_type(), ScriptType::WitnessScript);
        assert_eq!(s2.extract_destinations(), Ok(vec![ScriptAddress::new_p2wsh(a2)]));
    }

    #[test]
    fn find_and_delete_no_match_is_noop() {
        let s: Script = vec![Opcode::OP_0 as u8].into();
        assert_eq!(s.find_and_delete(&[0xff]), s);
    }

    #[test]
    fn instructions_iterator_stops_on_error() {
        // OP_4, OP_HASH160, push20 zeros, BAD(0xf9), push20 ones, OP_EQUAL
        let s: Script =
            "54a9140000000000000000000000000000000000000000f914010101010101010101010101010101010101010187".into();
        let mut count = 0;
        let mut errs = 0;
        for r in s.iter() {
            count += 1;
            if r.is_err() {
                errs += 1;
            }
        }
        assert_eq!(count, 4);
        assert_eq!(errs, 1);
    }

    #[test]
    fn witness_commitment_marker() {
        let mut bytes = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
        bytes.extend_from_slice(&[0u8; 30]);
        assert!(super::is_witness_commitment_script(&bytes));
        bytes[5] = 0xee;
        assert!(!super::is_witness_commitment_script(&bytes));
    }

    #[test]
    fn debug_format_is_stable() {
        let s = Builder::default()
            .push_num(3.into())
            .push_num(2.into())
            .push_opcode(Opcode::OP_ADD)
            .into_script();
        assert_eq!(format!("{:?}", s), "Script { data: 0103010293 }");
    }
}
