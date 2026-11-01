// secp256k1 public key with optional compression flag.

use crate::hash::{H160, H264, H520};
use crate::{AddressHashEnum, CompactSignature, Error, Message, Signature, SECP_VERIFY};
use crypto::dhash160;
use rustc_hex::ToHex;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId, Signature as SecpSignature};
use secp256k1::{Message as SecpMessage, PublicKey};
use std::{fmt, ops};

#[derive(Copy, Clone, Eq)]
pub enum Public {
    Normal(H520),
    Compressed(H264),
}

impl Default for Public {
    fn default() -> Self { Public::Compressed(H264::default()) }
}

impl Public {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        match bytes.len() {
            33 => {
                let mut h = H264::default();
                h.copy_from_slice(bytes);
                Ok(Public::Compressed(h))
            },
            65 => {
                let mut h = H520::default();
                h.copy_from_slice(bytes);
                Ok(Public::Normal(h))
            },
            _ => Err(Error::InvalidPublic),
        }
    }

    pub fn address_hash(&self) -> H160 { dhash160(self) }

    pub fn verify(&self, message: &Message, signature: &Signature) -> Result<bool, Error> {
        let pk = match self {
            Public::Compressed(p) => PublicKey::from_slice(&**p)?,
            Public::Normal(p) => PublicKey::from_slice(&**p)?,
        };
        let mut sig = SecpSignature::from_der_lax(signature)?;
        sig.normalize_s();
        let msg = SecpMessage::from_digest_slice(&**message)?;
        Ok(SECP_VERIFY.verify_ecdsa(&msg, &sig, &pk).is_ok())
    }

    pub fn recover_compact(message: &Message, signature: &CompactSignature) -> Result<Self, Error> {
        if signature[0] < 27 {
            return Err(Error::InvalidSignature);
        }
        let rec = (signature[0] - 27) & 3;
        let compressed = (signature[0] - 27) & 4 != 0;
        let recovery_id = RecoveryId::from_i32(rec as i32)?;
        let recoverable = RecoverableSignature::from_compact(&signature[1..65], recovery_id)?;
        let msg = SecpMessage::from_digest_slice(&**message)?;
        let pk = SECP_VERIFY.recover_ecdsa(&msg, &recoverable)?;
        if compressed {
            Ok(Public::Compressed(pk.serialize().into()))
        } else {
            Ok(Public::Normal(pk.serialize_uncompressed().into()))
        }
    }

    pub fn to_vec(&self) -> Vec<u8> { (**self).to_vec() }
}

impl ops::Deref for Public {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Public::Normal(h) => &**h,
            Public::Compressed(h) => &**h,
        }
    }
}

impl AsRef<[u8]> for Public {
    fn as_ref(&self) -> &[u8] { self }
}

impl PartialEq for Public {
    fn eq(&self, other: &Self) -> bool {
        let s: &[u8] = self;
        let o: &[u8] = other;
        s == o
    }
}

impl fmt::Debug for Public {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Public::Normal(h) => writeln!(f, "normal: {}", h.to_hex::<String>()),
            Public::Compressed(h) => writeln!(f, "compressed: {}", h.to_hex::<String>()),
        }
    }
}

impl fmt::Display for Public {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&(**self).to_hex::<String>()) }
}

impl From<Public> for AddressHashEnum {
    fn from(p: Public) -> AddressHashEnum { AddressHashEnum::AddressHash(p.address_hash()) }
}
