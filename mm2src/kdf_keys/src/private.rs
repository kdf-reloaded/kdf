// Wallet Import Format private key.
//
// Layout: <prefix><32-byte secret>[<0x01 if compressed>]<4-byte checksum>
// Checksum algo is pluggable (Bitcoin DSHA256, Groestlcoin DGROESTL512,
// SmartCash KECCAK256) — selected by the chain's coin config.

use crate::address::detect_checksum;
use crate::{DisplayLayout, Error, Message, Secret, Signature, SECP_SIGN};
use base58::{FromBase58, ToBase58};
use crypto::{checksum, ChecksumType};
use rustc_hex::ToHex;
use secp256k1::{Message as SecpMessage, SecretKey};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Private {
    pub prefix: u8,
    pub secret: Secret,
    pub compressed: bool,
    pub checksum_type: ChecksumType,
}

impl Private {
    /// Sign `message` and return a DER-encoded ECDSA signature.
    pub fn sign(&self, message: &Message) -> Result<Signature, Error> {
        let secret = SecretKey::from_slice(&*self.secret)?;
        let msg = SecpMessage::from_digest_slice(&**message)?;
        let sig = SECP_SIGN.sign_ecdsa(&msg, &secret);
        Ok(sig.serialize_der().as_ref().to_vec().into())
    }

    /// Sign `message` producing a 65-byte recoverable signature.
    /// Layout: `[header(1) | r(32) | s(32)]` where the header encodes
    /// recovery id and compressed flag (Bitcoin / Qtum signmessage convention).
    pub fn sign_compact(&self, message: &Message) -> Result<Signature, Error> {
        let secret = SecretKey::from_slice(&*self.secret)?;
        let msg = SecpMessage::from_digest_slice(&**message)?;
        let recoverable = SECP_SIGN.sign_ecdsa_recoverable(&msg, &secret);
        let (recovery_id, body) = recoverable.serialize_compact();
        let mut out: Vec<u8> = body.to_vec();
        let header = 27u8 + recovery_id.to_i32() as u8 + if self.compressed { 4 } else { 0 };
        out.insert(0, header);
        Ok(out.into())
    }
}

impl DisplayLayout for Private {
    type Target = Vec<u8>;

    fn layout(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(38);
        buf.push(self.prefix);
        buf.extend_from_slice(&*self.secret);
        if self.compressed {
            buf.push(0x01);
        }
        buf.extend_from_slice(&*checksum(&buf, &self.checksum_type));
        buf
    }

    fn from_layout(data: &[u8]) -> Result<Self, Error> {
        let compressed = match data.len() {
            37 => false,
            38 => true,
            _ => return Err(Error::InvalidPrivate),
        };
        if compressed && data[data.len() - 5] != 0x01 {
            return Err(Error::InvalidPrivate);
        }
        let split = data.len() - 4;
        let checksum_type = detect_checksum(&data[..split], &data[split..])?;
        let mut secret = Secret::default();
        secret.copy_from_slice(&data[1..33]);
        Ok(Private {
            prefix: data[0],
            secret,
            compressed,
            checksum_type,
        })
    }
}

impl fmt::Debug for Private {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "prefix: {}", self.prefix)?;
        writeln!(f, "secret: {}", self.secret.to_hex::<String>())?;
        writeln!(f, "compressed: {}", self.compressed)
    }
}

impl fmt::Display for Private {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.layout().to_base58().fmt(f) }
}

impl FromStr for Private {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let raw = s.from_base58().map_err(|_| Error::InvalidPrivate)?;
        Private::from_layout(&raw)
    }
}

impl From<&'static str> for Private {
    fn from(s: &'static str) -> Self { s.parse().expect("valid WIF literal") }
}

#[cfg(test)]
mod tests {
    use super::Private;
    use crate::hash::H256;
    use crypto::ChecksumType;

    fn p(prefix: u8, hex: &'static str, compressed: bool, ct: ChecksumType) -> Private {
        Private {
            prefix,
            secret: H256::from_reversed_str(hex),
            compressed,
            checksum_type: ct,
        }
    }

    #[test]
    fn wif_roundtrip_btc_uncompressed() {
        let pk = p(
            128,
            "063377054c25f98bc538ac8dd2cf9064dd5d253a725ece0628a34e2f84803bd5",
            false,
            ChecksumType::DSHA256,
        );
        let s = "5KSCKP8NUyBZPCCQusxRwgmz9sfvJQEgbGukmmHepWw5Bzp95mu";
        assert_eq!(pk.to_string(), s);
        assert_eq!(pk, s.into());
    }

    #[test]
    fn wif_roundtrip_komodo_compressed() {
        let pk = p(
            188,
            "063377054c25f98bc538ac8dd2cf9064dd5d253a725ece0628a34e2f84803bd5",
            true,
            ChecksumType::DSHA256,
        );
        let s = "UwA3FpHWKfwrQ1DTiwbErpEnCEhvLuq1WnbfmqGBPSLNNvXtzYd5";
        assert_eq!(pk.to_string(), s);
        assert_eq!(pk, s.into());
    }

    #[test]
    fn wif_roundtrip_zec_testnet() {
        let pk = p(
            239,
            "063377054c25f98bc538ac8dd2cf9064dd5d253a725ece0628a34e2f84803bd5",
            true,
            ChecksumType::DSHA256,
        );
        let s = "cUjCR3fPFWfs6PtdvoinTh4ctPxBvFf5pKNKJzw1RqmfjogL7GuU";
        assert_eq!(pk.to_string(), s);
        assert_eq!(pk, s.into());
    }

    #[test]
    fn wif_roundtrip_groestlcoin() {
        let pk = p(
            128,
            "cbc8853bd3617a5fcecfcc97f4a68853481657fc575cf85e04a64a2d1a78f974",
            true,
            ChecksumType::DGROESTL512,
        );
        let s = "L196QUb5fAcBVvZizvx66ABsU7iVTS4iAz15YEgB8QWY35KfD6ox";
        assert_eq!(pk, s.into());
        assert_eq!(pk.to_string(), s);
    }

    #[test]
    fn wif_roundtrip_smartcash() {
        let pk = p(
            191,
            "48688b0cd9440864b95916f53d6e06cdab5f50dc3abfa74b5c6a176620daa302",
            true,
            ChecksumType::KECCAK256,
        );
        let s = "VFqZrZNzkJEk29Kzp87J7eXDuQFMh1UsqYcMmi9bfdAZ522nz1mv";
        assert_eq!(pk, s.into());
        assert_eq!(pk.to_string(), s);
    }
}
