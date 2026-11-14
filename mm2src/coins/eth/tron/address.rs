//! TRON address handling.
//!
//! TRON addresses are 21 bytes: a 0x41 prefix byte followed by a 20-byte
//! Ethereum-style address. They are displayed as Base58Check-encoded strings
//! starting with 'T' (34 characters).

use base58::{FromBase58, ToBase58};
use derive_more::Display;
use ethereum_types::Address as EthAddress;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

/// The 0x41 prefix byte identifying TRON mainnet addresses.
const TRON_ADDRESS_PREFIX: u8 = 0x41;

/// Raw 21-byte TRON address (prefix + 20-byte EVM address).
const TRON_ADDRESS_LEN: usize = 21;

/// Base58Check-encoded TRON address length.
const TRON_BASE58_LEN: usize = 34;

#[derive(Debug, Display, Clone, PartialEq)]
pub enum TronAddressError {
    #[display(fmt = "Invalid Base58: {}", _0)]
    InvalidBase58(String),
    #[display(fmt = "Invalid checksum")]
    InvalidChecksum,
    #[display(fmt = "Invalid length: expected {} bytes, got {}", expected, got)]
    InvalidLength { expected: usize, got: usize },
    #[display(fmt = "Invalid prefix: expected 0x41, got 0x{:02x}", _0)]
    InvalidPrefix(u8),
    #[display(fmt = "Invalid hex: {}", _0)]
    InvalidHex(String),
}

/// A TRON blockchain address.
///
/// Internally stores the 20-byte EVM address (without the 0x41 prefix).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TronAddress(EthAddress);

impl TronAddress {
    /// Create a TronAddress from a 20-byte EVM address.
    pub fn from_evm_address(addr: EthAddress) -> Self { TronAddress(addr) }

    /// Create from raw 21-byte TRON address (0x41 prefix + 20 bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TronAddressError> {
        if bytes.len() != TRON_ADDRESS_LEN {
            return Err(TronAddressError::InvalidLength {
                expected: TRON_ADDRESS_LEN,
                got: bytes.len(),
            });
        }
        if bytes[0] != TRON_ADDRESS_PREFIX {
            return Err(TronAddressError::InvalidPrefix(bytes[0]));
        }
        Ok(TronAddress(EthAddress::from_slice(&bytes[1..])))
    }

    /// Decode from a Base58Check-encoded string (starts with 'T').
    pub fn from_base58(s: &str) -> Result<Self, TronAddressError> {
        let decoded = s
            .from_base58()
            .map_err(|_| TronAddressError::InvalidBase58(s.to_string()))?;

        // Base58Check: payload + 4-byte checksum
        if decoded.len() != TRON_ADDRESS_LEN + 4 {
            return Err(TronAddressError::InvalidLength {
                expected: TRON_ADDRESS_LEN + 4,
                got: decoded.len(),
            });
        }

        let (payload, checksum) = decoded.split_at(TRON_ADDRESS_LEN);
        let expected_checksum = double_sha256_checksum(payload);
        if checksum != expected_checksum {
            return Err(TronAddressError::InvalidChecksum);
        }

        Self::from_bytes(payload)
    }

    /// Decode from hex string (with or without "0x" prefix, 42 chars with prefix).
    pub fn from_hex(hex: &str) -> Result<Self, TronAddressError> {
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex).map_err(|e| TronAddressError::InvalidHex(e.to_string()))?;
        Self::from_bytes(&bytes)
    }

    /// Parse an address as a caller supplies it: Base58Check (`T...`), or the 21-byte wire form in hex
    /// (starting with `41`, optionally behind `0x`).
    ///
    /// # Errors
    ///
    /// The [`TronAddressError`] of the form the string looks like (Base58Check unless it starts with `0x` or `41`).
    pub fn parse(s: &str) -> Result<Self, TronAddressError> {
        if s.starts_with("0x") || s.starts_with("41") {
            Self::from_hex(s)
        } else {
            Self::from_base58(s)
        }
    }

    /// Return the 20-byte EVM address.
    pub fn to_evm_address(&self) -> EthAddress { self.0 }

    /// Return the 21-byte TRON address (0x41 prefix + 20 bytes).
    pub fn to_bytes(&self) -> [u8; TRON_ADDRESS_LEN] {
        let mut bytes = [0u8; TRON_ADDRESS_LEN];
        bytes[0] = TRON_ADDRESS_PREFIX;
        bytes[1..].copy_from_slice(self.0.as_ref());
        bytes
    }

    /// Encode as Base58Check string.
    pub fn to_base58(&self) -> String {
        let payload = self.to_bytes();
        let checksum = double_sha256_checksum(&payload);
        let mut full = Vec::with_capacity(TRON_ADDRESS_LEN + 4);
        full.extend_from_slice(&payload);
        full.extend_from_slice(&checksum);
        full.to_base58()
    }

    /// Encode as hex string (with 0x41 prefix, no "0x").
    pub fn to_hex(&self) -> String { hex::encode(self.to_bytes()) }
}

impl fmt::Debug for TronAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "TronAddress({})", self.to_base58()) }
}

impl fmt::Display for TronAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.to_base58()) }
}

impl Serialize for TronAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base58())
    }
}

impl<'de> Deserialize<'de> for TronAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        TronAddress::from_base58(&s).map_err(serde::de::Error::custom)
    }
}

/// Compute the first 4 bytes of double-SHA256 hash (Bitcoin-style checksum).
fn double_sha256_checksum(data: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first.as_slice());
    let mut checksum = [0u8; 4];
    checksum.copy_from_slice(&second[..4]);
    checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    // Well-known TRON foundation address
    const TEST_BASE58: &str = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";
    const TEST_HEX: &str = "418840e6c55b9ada326d211d818c34a994aeced808";

    #[test]
    fn test_base58_roundtrip() {
        let addr = TronAddress::from_base58(TEST_BASE58).unwrap();
        assert_eq!(addr.to_base58(), TEST_BASE58);
    }

    #[test]
    fn test_hex_roundtrip() {
        let addr = TronAddress::from_hex(TEST_HEX).unwrap();
        assert_eq!(addr.to_hex(), TEST_HEX);
    }

    #[test]
    fn test_base58_and_hex_same_address() {
        let from_b58 = TronAddress::from_base58(TEST_BASE58).unwrap();
        let from_hex = TronAddress::from_hex(TEST_HEX).unwrap();
        assert_eq!(from_b58, from_hex);
    }

    #[test]
    fn test_evm_address_conversion() {
        let addr = TronAddress::from_base58(TEST_BASE58).unwrap();
        let evm = addr.to_evm_address();
        let back = TronAddress::from_evm_address(evm);
        assert_eq!(addr, back);
    }

    #[test]
    fn test_invalid_base58() {
        assert!(TronAddress::from_base58("invalid!@#").is_err());
    }

    #[test]
    fn test_invalid_prefix() {
        // Valid length but wrong prefix byte
        let mut bytes = [0u8; 21];
        bytes[0] = 0x42; // should be 0x41
        assert!(matches!(
            TronAddress::from_bytes(&bytes),
            Err(TronAddressError::InvalidPrefix(0x42))
        ));
    }

    #[test]
    fn test_invalid_length() {
        assert!(matches!(
            TronAddress::from_bytes(&[0x41; 20]),
            Err(TronAddressError::InvalidLength { expected: 21, got: 20 })
        ));
    }

    #[test]
    fn test_serde_roundtrip() {
        let addr = TronAddress::from_base58(TEST_BASE58).unwrap();
        let json = serde_json::to_string(&addr).unwrap();
        assert_eq!(json, format!("\"{}\"", TEST_BASE58));
        let deserialized: TronAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(addr, deserialized);
    }

    #[test]
    fn test_display() {
        let addr = TronAddress::from_base58(TEST_BASE58).unwrap();
        assert_eq!(format!("{}", addr), TEST_BASE58);
    }

    #[test]
    fn parse_accepts_base58_and_hex_forms_and_refuses_the_rest() {
        let expected = TronAddress::from_base58(TEST_BASE58).unwrap();
        assert_eq!(TronAddress::parse(TEST_BASE58).unwrap(), expected);
        assert_eq!(TronAddress::parse(TEST_HEX).unwrap(), expected);
        assert_eq!(TronAddress::parse(&format!("0x{TEST_HEX}")).unwrap(), expected);
        // The 20-byte EVM form is not a TRON address.
        assert!(TronAddress::parse("0x8840e6c55b9ada326d211d818c34a994aeced808").is_err());
        assert!(TronAddress::parse("").is_err());
        assert!(TronAddress::parse("not-an-address").is_err());
    }

    #[test]
    fn test_base58_starts_with_t() {
        let addr = TronAddress::from_base58(TEST_BASE58).unwrap();
        assert!(addr.to_base58().starts_with('T'));
    }
}
