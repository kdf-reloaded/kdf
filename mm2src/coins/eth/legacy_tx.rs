// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// Copyright 2026 Komodo DeFi Framework / Rincoin contributors.
//
// This file is a derivative work of Parity Technologies' `ethcore-transaction`
// (from `parity-ethereum`), which is distributed under the GNU General Public
// License v3.0. As a derivative work it remains under the GPL-3.0. It was
// vendored locally to remove the
// `ethcore-transaction = artemii235/parity-ethereum.git` git dependency
// (LP-17 Phase 5b) and is wire-format-compatible with the upstream
// `ethcore_transaction` crate (legacy 9-RLP EIP-155 transactions).

//! Local replacement for `ethcore_transaction::{Transaction,
//! UnverifiedTransaction, SignedTransaction, Action}` that drops the GPLv3
//! `parity-ethereum` git fork. The on-the-wire representation (RLP byte
//! sequence and `keccak256` hash) is byte-identical to the upstream
//! implementation; this is verified by the EIP-155 spec test vector
//! contained in `tests`.
//!
//! Cryptographic operations defer to `mm2_eth::keys` (`secp256k1` 0.20 +
//! `tiny-keccak` 2.0) which itself replaced the `ethkey` git fork.

use alloy::rlp::{Buf, BufMut, Decodable, Encodable, Header, EMPTY_STRING_CODE};
use ethereum_types::{Address, H256, U256};
use mm2_eth::keys::{public_to_address, recover_public_key, sign as eth_sign, EthKeyError, Public, Secret, Signature,
                    H520};
use std::ops::Deref;

pub type Bytes = Vec<u8>;

/// Sender for unsigned transactions (EIP-86 / `0xff..ff`-style).
const UNSIGNED_SENDER: Address = ethereum_types::H160([0xff; 20]);

/// `to` field of an Ethereum transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Contract creation tx (no recipient).
    Create,
    /// Call to an existing address.
    Call(Address),
}

impl Default for Action {
    fn default() -> Self { Action::Create }
}

impl Encodable for Action {
    fn encode(&self, out: &mut dyn BufMut) {
        match self {
            Action::Create => out.put_u8(EMPTY_STRING_CODE),
            Action::Call(addr) => {
                let bytes: &[u8; 20] = &addr.0;
                bytes[..].encode(out);
            },
        }
    }

    fn length(&self) -> usize {
        match self {
            Action::Create => 1,
            // 1-byte header (0x94) + 20 bytes address.
            Action::Call(_) => 21,
        }
    }
}

impl Decodable for Action {
    fn decode(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        if buf.is_empty() {
            return Err(alloy::rlp::Error::InputTooShort);
        }
        if buf[0] == EMPTY_STRING_CODE {
            buf.advance(1);
            return Ok(Action::Create);
        }
        let header = Header::decode(buf)?;
        if header.list || header.payload_length != 20 {
            return Err(alloy::rlp::Error::Custom("invalid Action: expected 20-byte address"));
        }
        let addr = Address::from_slice(&buf[..20]);
        buf.advance(20);
        Ok(Action::Call(addr))
    }
}

/// Unsigned legacy Ethereum transaction.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub nonce: U256,
    pub gas_price: U256,
    pub gas: U256,
    pub action: Action,
    pub value: U256,
    pub data: Bytes,
}

impl Transaction {
    /// Returns the keccak-256 of the EIP-155 signing payload.
    ///
    /// For `chain_id = Some(n)` the payload is
    /// `RLP([nonce, gas_price, gas, to, value, data, n, 0, 0])`; for
    /// `None` the payload omits the chain-id triplet (pre-EIP-155).
    pub fn hash(&self, chain_id: Option<u64>) -> H256 {
        let mut buf = Vec::new();
        let payload_len = self.signing_payload_len(chain_id);
        Header {
            list: true,
            payload_length: payload_len,
        }
        .encode(&mut buf);
        u256_to_alloy(&self.nonce).encode(&mut buf);
        u256_to_alloy(&self.gas_price).encode(&mut buf);
        u256_to_alloy(&self.gas).encode(&mut buf);
        self.action.encode(&mut buf);
        u256_to_alloy(&self.value).encode(&mut buf);
        self.data.as_slice().encode(&mut buf);
        if let Some(n) = chain_id {
            n.encode(&mut buf);
            0u8.encode(&mut buf);
            0u8.encode(&mut buf);
        }
        keccak256_h256(&buf)
    }

    fn signing_payload_len(&self, chain_id: Option<u64>) -> usize {
        let mut len = u256_to_alloy(&self.nonce).length()
            + u256_to_alloy(&self.gas_price).length()
            + u256_to_alloy(&self.gas).length()
            + self.action.length()
            + u256_to_alloy(&self.value).length()
            + self.data.as_slice().length();
        if let Some(n) = chain_id {
            len += n.length() + 0u8.length() + 0u8.length();
        }
        len
    }

    /// Sign with `secret`. EIP-155 replay protection is applied when
    /// `chain_id` is `Some`.
    pub fn sign(self, secret: &Secret, chain_id: Option<u64>) -> SignedTransaction {
        let sig = eth_sign(secret, &self.hash(chain_id)).expect("data is valid and context has signing capabilities");
        SignedTransaction::new(self.with_signature(sig, chain_id)).expect("secret is valid so it's recoverable")
    }

    fn with_signature(self, sig: Signature, chain_id: Option<u64>) -> UnverifiedTransaction {
        let r = U256::from_big_endian(sig.r());
        let s = U256::from_big_endian(sig.s());
        let v = add_chain_replay_protection(sig.v() as u64, chain_id);
        UnverifiedTransaction {
            unsigned: self,
            r,
            s,
            v,
            hash: H256::zero(),
        }
        .compute_hash()
    }
}

/// Signed transaction whose signature has not yet been verified
/// (no public key recovered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnverifiedTransaction {
    pub unsigned: Transaction,
    pub v: u64,
    pub r: U256,
    pub s: U256,
    pub hash: H256,
}

impl Deref for UnverifiedTransaction {
    type Target = Transaction;
    fn deref(&self) -> &Self::Target { &self.unsigned }
}

impl Encodable for UnverifiedTransaction {
    fn encode(&self, out: &mut dyn BufMut) { self.rlp_append_sealed(out); }
    fn length(&self) -> usize {
        let payload_len = self.sealed_payload_len();
        Header {
            list: true,
            payload_length: payload_len,
        }
        .length()
            + payload_len
    }
}

impl Decodable for UnverifiedTransaction {
    fn decode(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        let raw = *buf;
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy::rlp::Error::UnexpectedString);
        }
        let payload_start_offset = raw.len() - buf.len();
        let consumed_total = payload_start_offset + header.payload_length;
        let header_len = payload_start_offset;
        let raw_tx = &raw[..consumed_total];

        let nonce = decode_u256(buf)?;
        let gas_price = decode_u256(buf)?;
        let gas = decode_u256(buf)?;
        let action = Action::decode(buf)?;
        let value = decode_u256(buf)?;
        let data: alloy::rlp::Bytes = alloy::rlp::Bytes::decode(buf)?;
        let v = u64::decode(buf)?;
        let r = decode_u256(buf)?;
        let s = decode_u256(buf)?;

        let consumed = (raw_tx.len() - header_len) - buf.len() + payload_start_offset;
        if consumed != consumed_total {
            return Err(alloy::rlp::Error::Custom("trailing bytes in transaction"));
        }

        let hash = keccak256_h256(raw_tx);
        Ok(UnverifiedTransaction {
            unsigned: Transaction {
                nonce,
                gas_price,
                gas,
                action,
                value,
                data: data.to_vec(),
            },
            v,
            r,
            s,
            hash,
        })
    }
}

impl UnverifiedTransaction {
    fn compute_hash(mut self) -> Self {
        let bytes = alloy::rlp::encode(&self);
        self.hash = keccak256_h256(&bytes);
        self
    }

    fn sealed_payload_len(&self) -> usize {
        u256_to_alloy(&self.nonce).length()
            + u256_to_alloy(&self.gas_price).length()
            + u256_to_alloy(&self.gas).length()
            + self.action.length()
            + u256_to_alloy(&self.value).length()
            + self.data.as_slice().length()
            + self.v.length()
            + u256_to_alloy(&self.r).length()
            + u256_to_alloy(&self.s).length()
    }

    fn rlp_append_sealed(&self, out: &mut dyn BufMut) {
        let payload_len = self.sealed_payload_len();
        Header {
            list: true,
            payload_length: payload_len,
        }
        .encode(out);
        u256_to_alloy(&self.nonce).encode(out);
        u256_to_alloy(&self.gas_price).encode(out);
        u256_to_alloy(&self.gas).encode(out);
        self.action.encode(out);
        u256_to_alloy(&self.value).encode(out);
        self.data.as_slice().encode(out);
        self.v.encode(out);
        u256_to_alloy(&self.r).encode(out);
        u256_to_alloy(&self.s).encode(out);
    }

    /// `r == 0 && s == 0` (EIP-86 unsigned).
    pub fn is_unsigned(&self) -> bool { self.r.is_zero() && self.s.is_zero() }

    /// Recovery id `v` mapped to its standard form (`0` or `1`).
    pub fn standard_v(&self) -> u8 { check_replay_protection(self.v) }

    /// EIP-155 chain id encoded in `v`, or `None` for pre-EIP-155.
    pub fn chain_id(&self) -> Option<u64> {
        match self.v {
            v if self.is_unsigned() => Some(v),
            v if v > 36 => Some((v - 35) / 2),
            _ => None,
        }
    }

    /// 65-byte signature `(r || s || v_standard)`.
    pub fn signature(&self) -> Signature {
        let mut r_bytes = [0u8; 32];
        let mut s_bytes = [0u8; 32];
        self.r.to_big_endian(&mut r_bytes);
        self.s.to_big_endian(&mut s_bytes);
        Signature::from_rsv(&H256::from(r_bytes), &H256::from(s_bytes), self.standard_v())
    }

    pub fn hash(&self) -> H256 { self.hash }

    /// Recover the secp256k1 public key from the signature.
    pub fn recover_public(&self) -> Result<Public, EthKeyError> {
        let pubkey_h520: H520 =
            recover_public_key(self.unsigned.hash(self.chain_id()), self.signature()).map_err(|e| e.into_inner())?;
        // mm2_eth::keys returns the 65-byte SEC1-prefixed pubkey; the
        // legacy `Public` (H512) is the 64 bytes following the 0x04 prefix.
        let mut public = Public::default();
        public.0.copy_from_slice(&pubkey_h520.0[1..]);
        Ok(public)
    }
}

/// `UnverifiedTransaction` with a successfully recovered sender address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTransaction {
    pub transaction: UnverifiedTransaction,
    pub sender: Address,
    pub public: Option<Public>,
}

impl Deref for SignedTransaction {
    type Target = UnverifiedTransaction;
    fn deref(&self) -> &Self::Target { &self.transaction }
}

impl Encodable for SignedTransaction {
    fn encode(&self, out: &mut dyn BufMut) { self.transaction.encode(out); }
    fn length(&self) -> usize { self.transaction.length() }
}

impl From<SignedTransaction> for UnverifiedTransaction {
    fn from(tx: SignedTransaction) -> Self { tx.transaction }
}

impl SignedTransaction {
    pub fn new(transaction: UnverifiedTransaction) -> Result<Self, EthKeyError> {
        if transaction.is_unsigned() {
            Ok(SignedTransaction {
                transaction,
                sender: UNSIGNED_SENDER,
                public: None,
            })
        } else {
            let public = transaction.recover_public()?;
            let sender = public_to_address(&public);
            Ok(SignedTransaction {
                transaction,
                sender,
                public: Some(public),
            })
        }
    }

    pub fn sender(&self) -> Address { self.sender }

    pub fn public_key(&self) -> Option<Public> { self.public }

    pub fn is_unsigned(&self) -> bool { self.transaction.is_unsigned() }

    /// Tx-hash as keccak256 of the RLP-encoded sealed transaction.
    pub fn tx_hash(&self) -> H256 { self.transaction.hash }
}

/// Assemble a signed EIP-155 legacy transaction from a device-returned signature.
///
/// `v`, `r`, `s` are the raw signature components returned by an external signer
/// (e.g. a Trezor device). The device applies EIP-155 replay protection when
/// computing `v`, so this helper normalizes `v` back to the canonical recovery
/// parameter (`0` / `1`) and then re-applies EIP-155 replay protection through
/// [`Transaction::with_signature`] with the same `chain_id`, producing a signed
/// transaction whose RLP encoding and hash are byte-identical to the local
/// signer's output for the same unsigned transaction and chain id (CRD R50.7 /
/// R50.12 / R50.22).
///
/// `r` and `s` are big-endian, minimally trimmed (`<= 32` bytes) as returned by
/// the device. A structurally invalid recovery value or oversized component
/// yields [`EthKeyError::InvalidSignature`] (CRD R50.18); a signature that does
/// not recover a public key is surfaced by [`SignedTransaction::new`].
pub fn signed_eth_tx_from_rsv(
    unsigned: Transaction,
    v: u32,
    r: &[u8],
    s: &[u8],
    chain_id: Option<u64>,
) -> Result<SignedTransaction, EthKeyError> {
    let standard_v = normalize_recovery_v(v, chain_id)?;
    let r_h256 = h256_from_be_slice(r)?;
    let s_h256 = h256_from_be_slice(s)?;
    let sig = Signature::from_rsv(&r_h256, &s_h256, standard_v);
    SignedTransaction::new(unsigned.with_signature(sig, chain_id))
}

/// Normalize a device-returned recovery value to the canonical `0` / `1` form.
///
/// For an EIP-155 chain id `n`, `standard_v = v - (2n + 35)`. For a pre-EIP-155
/// signature (`chain_id == None`), `standard_v = v - 27`. Any value that does not
/// fall in `{0, 1}` after normalization is treated as an invalid recovery value
/// (CRD R50.7 / R50.18).
fn normalize_recovery_v(v: u32, chain_id: Option<u64>) -> Result<u8, EthKeyError> {
    let v = v as u64;
    let standard = match chain_id {
        Some(n) => v.checked_sub(2u64.saturating_mul(n).saturating_add(35)),
        None => v.checked_sub(27),
    };
    match standard {
        Some(sv @ 0..=1) => Ok(sv as u8),
        _ => Err(EthKeyError::InvalidSignature),
    }
}

/// Left-pad a big-endian, minimally-trimmed byte slice (`<= 32` bytes) into an
/// `H256`. An oversized slice is rejected as an invalid signature component.
fn h256_from_be_slice(bytes: &[u8]) -> Result<H256, EthKeyError> {
    if bytes.len() > 32 {
        return Err(EthKeyError::InvalidSignature);
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(bytes);
    Ok(H256::from(buf))
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn keccak256_h256(bytes: &[u8]) -> H256 {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    H256::from_slice(out.as_slice())
}

fn u256_to_alloy(v: &U256) -> alloy::primitives::U256 {
    let mut buf = [0u8; 32];
    v.to_big_endian(&mut buf);
    alloy::primitives::U256::from_be_bytes(buf)
}

fn decode_u256(buf: &mut &[u8]) -> alloy::rlp::Result<U256> {
    let v = alloy::primitives::U256::decode(buf)?;
    let bytes: [u8; 32] = v.to_be_bytes();
    Ok(U256::from_big_endian(&bytes))
}

/// EIP-155 v adjustment.
fn add_chain_replay_protection(v: u64, chain_id: Option<u64>) -> u64 {
    v + match chain_id {
        Some(n) => 35 + n * 2,
        None => 27,
    }
}

fn check_replay_protection(v: u64) -> u8 {
    match v {
        27 => 0,
        28 => 1,
        v if v > 36 => ((v - 1) % 2) as u8,
        _ => 4,
    }
}

/// Drop-in replacement for the `rlp::{encode, decode}` free functions used
/// by the existing call sites — preserves the `crate::eth::rlp::encode(...)`
/// and `crate::eth::rlp::decode::<T>(...)` syntax.
pub mod rlp {
    pub fn encode<T: alloy::rlp::Encodable>(value: &T) -> Vec<u8> { alloy::rlp::encode(value) }
    pub fn decode<T: alloy::rlp::Decodable>(bytes: &[u8]) -> Result<T, alloy::rlp::Error> {
        let mut buf = bytes;
        T::decode(&mut buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm2_eth::keys::Secret;
    use std::str::FromStr;

    /// Canonical EIP-155 test vector.
    /// https://eips.ethereum.org/EIPS/eip-155
    #[test]
    fn eip155_spec_vector_signs_to_canonical_rlp() {
        let secret = Secret::from_str("4646464646464646464646464646464646464646464646464646464646464646").unwrap();
        let to = Address::from_str("3535353535353535353535353535353535353535").unwrap();

        let tx = Transaction {
            nonce: U256::from(9u64),
            gas_price: U256::from(20_000_000_000u64),
            gas: U256::from(21000u64),
            action: Action::Call(to),
            value: U256::from(1_000_000_000_000_000_000u64),
            data: Vec::new(),
        };

        // Spec signing hash:
        let expected_signing_hash =
            H256::from_slice(&hex::decode("daf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53").unwrap());
        assert_eq!(
            tx.clone().hash(Some(1)),
            expected_signing_hash,
            "EIP-155 signing hash mismatch"
        );

        let signed = tx.sign(&secret, Some(1));
        let bytes = alloy::rlp::encode(&signed);

        let expected = hex::decode(
            "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83",
        )
        .unwrap();
        assert_eq!(bytes, expected, "signed tx RLP must match EIP-155 vector byte-for-byte");

        // tx_hash = keccak256(signed_rlp).
        let expected_tx_hash =
            H256::from_slice(&hex::decode("33469b22e9f636356c4160a87eb19df52b7412e8eac32a4a55ffe88ea8350788").unwrap());
        assert_eq!(signed.tx_hash(), expected_tx_hash, "tx_hash mismatch");
    }

    #[test]
    fn round_trip_decode_encode() {
        let secret = Secret::from_str("4646464646464646464646464646464646464646464646464646464646464646").unwrap();
        let to = Address::from_str("3535353535353535353535353535353535353535").unwrap();
        let tx = Transaction {
            nonce: U256::from(9u64),
            gas_price: U256::from(20_000_000_000u64),
            gas: U256::from(21000u64),
            action: Action::Call(to),
            value: U256::from(1_000_000_000_000_000_000u64),
            data: vec![],
        };
        let signed = tx.sign(&secret, Some(1));
        let bytes = alloy::rlp::encode(&signed);
        let mut slice = bytes.as_slice();
        let decoded = UnverifiedTransaction::decode(&mut slice).unwrap();
        let resigned = SignedTransaction::new(decoded).unwrap();
        assert_eq!(resigned.sender(), signed.sender());
        assert_eq!(resigned.tx_hash(), signed.tx_hash());
        assert_eq!(alloy::rlp::encode(&resigned), bytes);
    }

    #[test]
    fn create_action_round_trips() {
        let secret = Secret::from_str("0000000000000000000000000000000000000000000000000000000000000001").unwrap();
        let tx = Transaction {
            nonce: U256::from(0u64),
            gas_price: U256::from(1u64),
            gas: U256::from(100_000u64),
            action: Action::Create,
            value: U256::from(0u64),
            data: vec![0x60, 0x80, 0x60, 0x40, 0x52],
        };
        let signed = tx.sign(&secret, Some(1));
        let bytes = alloy::rlp::encode(&signed);
        let mut slice = bytes.as_slice();
        let decoded = UnverifiedTransaction::decode(&mut slice).unwrap();
        assert_eq!(decoded.action, Action::Create);
        assert_eq!(decoded.data, vec![0x60, 0x80, 0x60, 0x40, 0x52]);
    }

    /// Feeding the local signer's own `(v, r, s)` back through
    /// `signed_eth_tx_from_rsv` must reproduce byte-identical RLP and hash
    /// (CRD R50.7 / R50.12 / R50.22).
    #[test]
    fn signed_eth_tx_from_rsv_matches_local_signer() {
        let secret = Secret::from_str("4646464646464646464646464646464646464646464646464646464646464646").unwrap();
        let to = Address::from_str("3535353535353535353535353535353535353535").unwrap();
        let chain_id = Some(1u64);
        let tx = Transaction {
            nonce: U256::from(9u64),
            gas_price: U256::from(20_000_000_000u64),
            gas: U256::from(21000u64),
            action: Action::Call(to),
            value: U256::from(1_000_000_000_000_000_000u64),
            data: Vec::new(),
        };

        let local = tx.clone().sign(&secret, chain_id);
        // The local signed tx carries the EIP-155-adjusted `v` (device-equivalent
        // recovery value) and the `r`/`s` components in big-endian form.
        let mut r_be = [0u8; 32];
        let mut s_be = [0u8; 32];
        local.transaction.r.to_big_endian(&mut r_be);
        local.transaction.s.to_big_endian(&mut s_be);
        let device_v = local.transaction.v as u32;

        let assembled = signed_eth_tx_from_rsv(tx, device_v, &r_be, &s_be, chain_id).unwrap();

        assert_eq!(
            alloy::rlp::encode(&assembled),
            alloy::rlp::encode(&local),
            "RLP must be byte-identical to the local signer"
        );
        assert_eq!(
            assembled.tx_hash(),
            local.tx_hash(),
            "tx_hash must match the local signer"
        );
        assert_eq!(assembled.sender(), local.sender(), "recovered sender must match");
    }

    /// A structurally invalid recovery value must be rejected (CRD R50.18).
    #[test]
    fn signed_eth_tx_from_rsv_rejects_invalid_recovery() {
        let tx = Transaction {
            nonce: U256::from(0u64),
            gas_price: U256::from(1u64),
            gas: U256::from(21000u64),
            action: Action::Call(Address::from([1u8; 20])),
            value: U256::from(0u64),
            data: Vec::new(),
        };
        // For chain_id 1 the valid device `v` values are `2*1 + 35 + {0,1}` = 37/38.
        let err = signed_eth_tx_from_rsv(tx, 99, &[1u8], &[1u8], Some(1)).unwrap_err();
        assert!(matches!(err, EthKeyError::InvalidSignature));
    }
}
