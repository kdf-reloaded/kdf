//! Ethereum key operations: address derivation, ECDSA signing and signature
//! recovery.
//!
//! LP-17 Phase 5a/5b: original implementation that replaces the
//! `ethkey = artemii235/parity-ethereum.git` GPL-licensed dependency
//! in favour of the dual-licensed `secp256k1` and `tiny-keccak`
//! crates. The public API mirrors the legacy `ethkey` surface so
//! that downstream consumers (`crypto::metamask_ctx`, the `coins`
//! crate's EVM and Tron paths) can switch over with a path rename
//! only.

use mm2_err_handle::prelude::*;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message as SecpMessage, PublicKey, Secp256k1, SecretKey};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use tiny_keccak::{Hasher, Keccak};
use zeroize::Zeroize;

pub use ethereum_types::{Address, H256, H512, H520};

/// Uncompressed secp256k1 public key (64 bytes, no SEC1 0x04 prefix) — wire
/// shape used by Ethereum (`address = keccak256(pubkey)[12..]`).
pub type Public = H512;

/// 65-byte ECDSA signature (`r || s || v`) with hex parsing.
#[derive(Clone)]
pub struct Signature([u8; 65]);

impl Signature {
    pub fn into_bytes(self) -> [u8; 65] { self.0 }

    /// Slice into the `r` (first 32 bytes) component.
    pub fn r(&self) -> &[u8] { &self.0[0..32] }

    /// Slice into the `s` (second 32 bytes) component.
    pub fn s(&self) -> &[u8] { &self.0[32..64] }

    /// The 1-byte recovery id `v` (0 or 1 in compact form).
    pub fn v(&self) -> u8 { self.0[64] }

    /// Construct from `r || s || v` components.
    pub fn from_rsv(r: &H256, s: &H256, v: u8) -> Self {
        let mut buf = [0u8; 65];
        buf[0..32].copy_from_slice(AsRef::<[u8]>::as_ref(r));
        buf[32..64].copy_from_slice(AsRef::<[u8]>::as_ref(s));
        buf[64] = v;
        Signature(buf)
    }
}

impl Default for Signature {
    fn default() -> Self { Signature([0u8; 65]) }
}

impl Deref for Signature {
    type Target = [u8; 65];
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl DerefMut for Signature {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl FromStr for Signature {
    type Err = EthKeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|_| EthKeyError::InvalidSignature)?;
        if bytes.len() != 65 {
            return Err(EthKeyError::InvalidSignature);
        }
        let mut buf = [0u8; 65];
        buf.copy_from_slice(&bytes);
        Ok(Signature(buf))
    }
}

impl fmt::Display for Signature {
    /// Lower-hex of the 65 raw signature bytes (no `0x` prefix).
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

/// Error category preserved from the legacy `ethkey::Error` for downstream
/// match arms.
#[derive(Debug)]
pub enum EthKeyError {
    InvalidSecret,
    InvalidSignature,
    InvalidMessage,
    InvalidPublic,
}

impl fmt::Display for EthKeyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            EthKeyError::InvalidSecret => f.write_str("Invalid secret"),
            EthKeyError::InvalidSignature => f.write_str("Invalid EC signature"),
            EthKeyError::InvalidMessage => f.write_str("Invalid message"),
            EthKeyError::InvalidPublic => f.write_str("Invalid public"),
        }
    }
}

impl std::error::Error for EthKeyError {}

impl From<secp256k1::Error> for EthKeyError {
    fn from(_: secp256k1::Error) -> Self { EthKeyError::InvalidSignature }
}

/// Compatibility alias matching the legacy `ethkey::Error` re-export.
pub type Error = EthKeyError;

/// Derives an Ethereum address from an uncompressed 65-byte ECDSA public key.
///
/// The first byte (0x04 prefix per SEC1) is stripped; the remaining 64 bytes
/// are Keccak-256-hashed and the last 20 bytes become the address.
pub fn address_from_uncompressed_pubkey(pubkey: H520) -> Address {
    let hash = keccak256(&AsRef::<[u8]>::as_ref(&pubkey)[1..65]);
    let mut addr = Address::default();
    AsMut::<[u8]>::as_mut(&mut addr).copy_from_slice(&hash[12..]);
    addr
}

/// Recovers the full uncompressed public key (H520, 65 bytes with 0x04 prefix)
/// from a message hash and its ECDSA signature.
///
/// Handles Ethereum's convention where the recovery id may have 27 added.
pub fn recover_public_key(message_hash: H256, mut sig: Signature) -> MmResult<H520, EthKeyError> {
    if sig[64] >= 27 {
        sig[64] -= 27;
    }

    let recovery_id = RecoveryId::from_i32(sig[64] as i32).map_to_mm(EthKeyError::from)?;
    let recoverable = RecoverableSignature::from_compact(&sig[0..64], recovery_id).map_to_mm(EthKeyError::from)?;
    let msg = SecpMessage::from_digest_slice(AsRef::<[u8]>::as_ref(&message_hash))
        .map_to_mm(|_| EthKeyError::InvalidMessage)?;
    let pubkey = Secp256k1::new()
        .recover_ecdsa(&msg, &recoverable)
        .map_to_mm(EthKeyError::from)?;

    let serialized = pubkey.serialize_uncompressed(); // [u8; 65] starting with 0x04
    let mut out = H520::default();
    AsMut::<[u8]>::as_mut(&mut out).copy_from_slice(&serialized);
    Ok(out)
}

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut out = [0u8; 32];
    hasher.update(bytes);
    hasher.finalize(&mut out);
    out
}

/// Derives an Ethereum address from a 64-byte uncompressed public key
/// (without the SEC1 0x04 prefix).
pub fn public_to_address(public: &Public) -> Address {
    let hash = keccak256(AsRef::<[u8]>::as_ref(public));
    let mut addr = Address::default();
    AsMut::<[u8]>::as_mut(&mut addr).copy_from_slice(&hash[12..]);
    addr
}

/// 32-byte secp256k1 secret key. Zeroized on drop.
#[derive(Clone)]
pub struct Secret([u8; 32]);

impl Secret {
    /// Construct from a 32-byte slice. Returns `Err` if the length is wrong
    /// or the key is invalid (zero / >= curve order).
    pub fn from_slice(bytes: &[u8]) -> Result<Self, EthKeyError> {
        if bytes.len() != 32 {
            return Err(EthKeyError::InvalidSecret);
        }
        // Validate against the secp256k1 curve order.
        let _ = SecretKey::from_slice(bytes).map_err(|_| EthKeyError::InvalidSecret)?;
        let mut buf = [0u8; 32];
        buf.copy_from_slice(bytes);
        Ok(Secret(buf))
    }

    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
}

impl FromStr for Secret {
    type Err = EthKeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|_| EthKeyError::InvalidSecret)?;
        Self::from_slice(&bytes)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("Secret(***)") }
}

impl fmt::LowerHex for Secret {
    /// Lower-hex of the 32 raw secret bytes (no `0x` prefix). The `#`
    /// alternate flag is honoured to add the `0x` prefix matching the
    /// behaviour of `ethereum_types::H256`.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if f.alternate() {
            f.write_str("0x")?;
        }
        for byte in self.0.iter() {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl Drop for Secret {
    fn drop(&mut self) { self.0.zeroize(); }
}

/// secp256k1 keypair (`Secret` + derived uncompressed `Public`).
#[derive(Clone, Debug)]
pub struct KeyPair {
    secret: Secret,
    public: Public,
}

impl KeyPair {
    /// Build a keypair from a known `Secret`.
    pub fn from_secret(secret: Secret) -> Result<Self, EthKeyError> {
        let sk = SecretKey::from_slice(secret.as_bytes()).map_err(|_| EthKeyError::InvalidSecret)?;
        let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk);
        let serialized = pk.serialize_uncompressed(); // [u8; 65] starting with 0x04
        let mut public = Public::default();
        AsMut::<[u8]>::as_mut(&mut public).copy_from_slice(&serialized[1..]);
        Ok(KeyPair { secret, public })
    }

    /// Build a keypair from a 32-byte secret slice.
    pub fn from_secret_slice(bytes: &[u8]) -> Result<Self, EthKeyError> {
        let secret = Secret::from_slice(bytes)?;
        Self::from_secret(secret)
    }

    pub fn secret(&self) -> &Secret { &self.secret }
    pub fn public(&self) -> &Public { &self.public }
    pub fn address(&self) -> Address { public_to_address(&self.public) }
}

/// Sign a 32-byte message hash with `secret`.
///
/// Returns a 65-byte signature `[r(32) | s(32) | v(1)]` where `v ∈ {0, 1}`
/// (the secp256k1 recovery id, *not* Ethereum's `v ∈ {27, 28}`).
pub fn sign(secret: &Secret, message: &H256) -> Result<Signature, EthKeyError> {
    let sk = SecretKey::from_slice(secret.as_bytes()).map_err(|_| EthKeyError::InvalidSecret)?;
    let msg =
        SecpMessage::from_digest_slice(AsRef::<[u8]>::as_ref(message)).map_err(|_| EthKeyError::InvalidMessage)?;
    let recoverable = Secp256k1::new().sign_ecdsa_recoverable(&msg, &sk);
    let (recid, compact) = recoverable.serialize_compact();
    let mut buf = [0u8; 65];
    buf[0..64].copy_from_slice(&compact);
    buf[64] = recid.to_i32() as u8;
    Ok(Signature(buf))
}

/// Verify that the given `signature` over `message` recovers to `address`.
pub fn verify_address(address: &Address, signature: &Signature, message: &H256) -> Result<bool, EthKeyError> {
    let mut sig = signature.clone();
    if sig.0[64] >= 27 {
        sig.0[64] -= 27;
    }
    let recid = RecoveryId::from_i32(sig.0[64] as i32)?;
    let recoverable = RecoverableSignature::from_compact(&sig.0[0..64], recid)?;
    let msg =
        SecpMessage::from_digest_slice(AsRef::<[u8]>::as_ref(message)).map_err(|_| EthKeyError::InvalidMessage)?;
    let pubkey = Secp256k1::new().recover_ecdsa(&msg, &recoverable)?;
    let serialized = pubkey.serialize_uncompressed();
    let mut public = Public::default();
    AsMut::<[u8]>::as_mut(&mut public).copy_from_slice(&serialized[1..]);
    Ok(&public_to_address(&public) == address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_from_known_pubkey() {
        // Uncompressed secp256k1 pubkey (65 bytes, 0x04 prefix).
        let pubkey_hex = concat!(
            "04",
            "e68acfc0253a10620dff706b0a1b1f1f5833ea3beb3bde2250d5f271f3563606",
            "672ebc45e0b7ea2e816ecb70ca03137b1c9476eec63d4632e990020b7b6fba39",
        );
        let pubkey = H520::from_str(pubkey_hex).unwrap();
        let addr = address_from_uncompressed_pubkey(pubkey);
        assert_ne!(addr, Address::default());
    }

    #[test]
    fn keypair_address_matches_known_vector() {
        // Test vector: secret = 1, expected address = 7E5F4552091A69125d5DfCb7b8C2659029395Bdf.
        let kp = KeyPair::from_secret_slice(
            &hex::decode("0000000000000000000000000000000000000000000000000000000000000001").unwrap(),
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", kp.address()),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let kp = KeyPair::from_secret_slice(
            &hex::decode("0000000000000000000000000000000000000000000000000000000000000042").unwrap(),
        )
        .unwrap();
        let msg = H256::from([0xAB; 32]);
        let sig = sign(kp.secret(), &msg).unwrap();
        assert_eq!(sig.len(), 65);
        assert!(sig.v() == 0 || sig.v() == 1);
        assert!(verify_address(&kp.address(), &sig, &msg).unwrap());
        // Wrong address must not verify.
        assert!(!verify_address(&Address::default(), &sig, &msg).unwrap());
    }
}
