//! JSON helpers for fields that need to travel as decimal strings.
//!
//! `BigUint` values such as ERC-721/ERC-1155 token identifiers and
//! ERC-1155 amounts can easily overflow JavaScript's safe-integer range,
//! so the GUI clients exchange them as base-10 strings. The helpers in
//! this module are wired into the model structs through `#[serde(with = …)]`
//! and `#[serde(serialize_with = …, deserialize_with = …)]` attributes.

use mm2_number::BigUint;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serializer};
use std::str::FromStr;

/// Serialize a `BigUint` as a decimal string.
pub(crate) fn token_id_to_string<S>(value: &BigUint, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

/// Deserialize a decimal string into a `BigUint`.
pub(crate) fn token_id_from_string<'de, D>(deserializer: D) -> Result<BigUint, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    BigUint::from_str(&raw).map_err(DeError::custom)
}

/// Deserialize an optional `BigUint` from `Option<String>`.
///
/// Used for ERC-1155 amounts where the field is optional and defaults to `1`
/// when omitted.
pub(crate) fn optional_token_amount<'de, D>(deserializer: D) -> Result<Option<BigUint>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(text) => BigUint::from_str(&text).map(Some).map_err(DeError::custom),
    }
}

/// Default page size used by the NFT list and transfer history requests
/// when no explicit `limit` is provided in the JSON payload.
pub(crate) const fn default_page_size() -> usize { 10 }

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Deserialize, Serialize, PartialEq, Debug)]
    struct Wrap {
        #[serde(serialize_with = "token_id_to_string", deserialize_with = "token_id_from_string")]
        id: BigUint,
    }

    #[derive(Deserialize, Serialize, PartialEq, Debug)]
    struct OptWrap {
        #[serde(default, deserialize_with = "optional_token_amount")]
        amount: Option<BigUint>,
    }

    #[test]
    fn token_id_round_trips_through_string() {
        let value = Wrap {
            id: BigUint::from(123_456_789_012_345_678_901_234u128),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"123456789012345678901234\""));
        let back: Wrap = serde_json::from_str(&json).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn optional_amount_accepts_none_and_string() {
        let none: OptWrap = serde_json::from_str("{}").unwrap();
        assert_eq!(none.amount, None);
        let some: OptWrap = serde_json::from_str("{\"amount\":\"42\"}").unwrap();
        assert_eq!(some.amount, Some(BigUint::from(42u32)));
    }

    #[test]
    fn optional_amount_rejects_garbage() {
        let err = serde_json::from_str::<OptWrap>("{\"amount\":\"abc\"}");
        assert!(err.is_err());
    }

    #[test]
    fn default_limit_is_ten() {
        assert_eq!(default_page_size(), 10);
    }
}

/// Deserialize a `u64` that the indexer contract sends as a decimal string.
///
/// Block heights and timestamps arrive as strings from the deployed indexer
/// (CRD ch.19 §19.5.1) but as JSON numbers from our own persisted payloads and
/// from GUI clients, so both are accepted. Serialization is left alone: the
/// model keeps emitting numbers, which is what existing consumers read.
pub(crate) fn u64_from_string_or_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::Number(value) => Ok(value),
        StringOrNumber::Text(raw) => u64::from_str(raw.trim()).map_err(DeError::custom),
    }
}

/// Optional counterpart of [`u64_from_string_or_number`].
pub(crate) fn optional_u64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<StringOrNumber>::deserialize(deserializer)? {
        None => Ok(None),
        Some(StringOrNumber::Number(value)) => Ok(Some(value)),
        Some(StringOrNumber::Text(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            u64::from_str(trimmed).map(Some).map_err(DeError::custom)
        },
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    Number(u64),
    Text(String),
}
