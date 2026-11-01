// SPDX-License-Identifier: GPL-2.0-only
//! `Bytes` — a hex-string-serialized wrapper around `Vec<u8>` matching the
//! Bitcoin Core JSON convention.

use primitives::bytes::Bytes as PrimitivesBytes;
use rustc_hex::{FromHex, ToHex};
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;

#[derive(Debug, Default, Clone, Eq, Hash, PartialEq)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    #[inline]
    pub fn new(bytes: Vec<u8>) -> Self { Self(bytes) }

    #[inline]
    pub fn into_vec(self) -> Vec<u8> { self.0 }

    #[inline]
    pub fn as_slice(&self) -> &[u8] { &self.0 }
}

impl<T> From<T> for Bytes
where
    PrimitivesBytes: From<T>,
{
    fn from(value: T) -> Self { Bytes(PrimitivesBytes::from(value).take()) }
}

impl From<Bytes> for Vec<u8> {
    fn from(value: Bytes) -> Self { value.0 }
}

impl Deref for Bytes {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl fmt::LowerHex for Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex_string: String = self.0.to_hex();
        serializer.serialize_str(&hex_string)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(BytesHexVisitor)
    }
}

struct BytesHexVisitor;

impl<'de> Visitor<'de> for BytesHexVisitor {
    type Value = Bytes;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a lowercase hex string with an even number of characters")
    }

    fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
        if value.is_empty() {
            return Ok(Bytes::new(Vec::new()));
        }
        if !value.len().is_multiple_of(2) {
            return Err(DeError::custom("invalid format"));
        }
        let parsed: Vec<u8> = value.from_hex().map_err(|_| DeError::custom("invalid hex"))?;
        Ok(Bytes::new(parsed))
    }

    fn visit_string<E: DeError>(self, value: String) -> Result<Self::Value, E> { self.visit_str(&value) }
}

#[cfg(test)]
mod tests {
    use super::Bytes;
    use rustc_hex::FromHex;
    use serde_json;

    #[test]
    fn serializes_to_lowercase_hex_string() {
        let payload = Bytes(FromHex::from_hex::<Vec<u8>>("0123456789abcdef").unwrap());
        assert_eq!(serde_json::to_string(&payload).unwrap(), r#""0123456789abcdef""#);
    }

    #[test]
    fn deserializes_empty_odd_and_even_payloads() {
        let empty: Bytes = serde_json::from_str(r#""""#).unwrap();
        assert_eq!(empty, Bytes(vec![]));
        assert!(serde_json::from_str::<Bytes>(r#""123""#).is_err());
        assert!(serde_json::from_str::<Bytes>(r#""gg""#).is_err());
        assert_eq!(serde_json::from_str::<Bytes>(r#""12""#).unwrap(), Bytes(vec![0x12]));
        assert_eq!(
            serde_json::from_str::<Bytes>(r#""0123""#).unwrap(),
            Bytes(vec![0x01, 0x23])
        );
    }
}
