// SPDX-License-Identifier: GPL-2.0-only
//! `ScriptType` JSON wire enum (matches Bitcoin Core / Komodo / Qtum / FIRO labels).

use script::ScriptType as ScriptScriptType;
use serde::de::{Error as DeError, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
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
    // Qtum specific
    CallSender,
    CreateSender,
    Call,
    Create,
    // FIRO specific
    LelantusMint,
    LelantusJMint,
    SparkMint,
    SparkSpend,
    // Verus / VRSC family specific
    CryptoCondition,
}

impl ScriptType {
    fn json_label(&self) -> &'static str {
        match self {
            ScriptType::NonStandard => "nonstandard",
            ScriptType::PubKey => "pubkey",
            ScriptType::PubKeyHash => "pubkeyhash",
            ScriptType::ScriptHash => "scripthash",
            ScriptType::Multisig => "multisig",
            ScriptType::NullData => "nulldata",
            ScriptType::WitnessScript => "witness_v0_scripthash",
            ScriptType::WitnessKey => "witness_v0_keyhash",
            ScriptType::WitnessV1Taproot => "witness_v1_taproot",
            ScriptType::CallSender => "call_sender",
            ScriptType::CreateSender => "create_sender",
            ScriptType::Call => "call",
            ScriptType::Create => "create",
            ScriptType::LelantusMint => "lelantusmint",
            ScriptType::LelantusJMint => "lelantusjmint",
            ScriptType::SparkMint => "sparkmint",
            ScriptType::SparkSpend => "sparkspend",
            ScriptType::CryptoCondition => "cryptocondition",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "nonstandard" => ScriptType::NonStandard,
            "pubkey" => ScriptType::PubKey,
            "pubkeyhash" => ScriptType::PubKeyHash,
            "scripthash" => ScriptType::ScriptHash,
            "multisig" => ScriptType::Multisig,
            "nulldata" => ScriptType::NullData,
            "witness_v0_scripthash" => ScriptType::WitnessScript,
            "witness_v0_keyhash" => ScriptType::WitnessKey,
            "witness_v1_taproot" => ScriptType::WitnessV1Taproot,
            "call_sender" => ScriptType::CallSender,
            "create_sender" => ScriptType::CreateSender,
            "call" => ScriptType::Call,
            "create" => ScriptType::Create,
            "lelantusmint" => ScriptType::LelantusMint,
            "lelantusjmint" => ScriptType::LelantusJMint,
            "sparkmint" => ScriptType::SparkMint,
            "sparkspend" => ScriptType::SparkSpend,
            "cryptocondition" => ScriptType::CryptoCondition,
            _ => return None,
        })
    }
}

impl From<ScriptScriptType> for ScriptType {
    fn from(script_type: ScriptScriptType) -> Self {
        match script_type {
            ScriptScriptType::NonStandard => ScriptType::NonStandard,
            ScriptScriptType::PubKey => ScriptType::PubKey,
            ScriptScriptType::PubKeyHash => ScriptType::PubKeyHash,
            ScriptScriptType::ScriptHash => ScriptType::ScriptHash,
            ScriptScriptType::Multisig => ScriptType::Multisig,
            ScriptScriptType::NullData => ScriptType::NullData,
            ScriptScriptType::WitnessScript => ScriptType::WitnessScript,
            ScriptScriptType::WitnessKey => ScriptType::WitnessKey,
            ScriptScriptType::WitnessV1Taproot => ScriptType::WitnessV1Taproot,
            ScriptScriptType::CallSender => ScriptType::CallSender,
            ScriptScriptType::CreateSender => ScriptType::CreateSender,
            ScriptScriptType::Call => ScriptType::Call,
            ScriptScriptType::Create => ScriptType::Create,
        }
    }
}

impl Serialize for ScriptType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.json_label())
    }
}

impl<'de> Deserialize<'de> for ScriptType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ScriptTypeLabelVisitor;

        impl<'de> Visitor<'de> for ScriptTypeLabelVisitor {
            type Value = ScriptType;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a script type label")
            }

            fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
                ScriptType::from_label(value).ok_or_else(|| E::invalid_value(Unexpected::Str(value), &self))
            }
        }

        deserializer.deserialize_identifier(ScriptTypeLabelVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::ScriptType;
    use serde_json;

    #[test]
    fn round_trip_every_label() {
        let cases = [
            (ScriptType::NonStandard, r#""nonstandard""#),
            (ScriptType::PubKey, r#""pubkey""#),
            (ScriptType::PubKeyHash, r#""pubkeyhash""#),
            (ScriptType::ScriptHash, r#""scripthash""#),
            (ScriptType::Multisig, r#""multisig""#),
            (ScriptType::NullData, r#""nulldata""#),
            (ScriptType::WitnessScript, r#""witness_v0_scripthash""#),
            (ScriptType::WitnessKey, r#""witness_v0_keyhash""#),
            (ScriptType::WitnessV1Taproot, r#""witness_v1_taproot""#),
            (ScriptType::CallSender, r#""call_sender""#),
            (ScriptType::CreateSender, r#""create_sender""#),
            (ScriptType::Call, r#""call""#),
            (ScriptType::Create, r#""create""#),
            (ScriptType::LelantusMint, r#""lelantusmint""#),
            (ScriptType::LelantusJMint, r#""lelantusjmint""#),
            (ScriptType::SparkMint, r#""sparkmint""#),
            (ScriptType::SparkSpend, r#""sparkspend""#),
            (ScriptType::CryptoCondition, r#""cryptocondition""#),
        ];
        for (variant, encoded) in cases {
            assert_eq!(serde_json::to_string(&variant).unwrap(), encoded);
            assert_eq!(serde_json::from_str::<ScriptType>(encoded).unwrap(), variant);
        }
    }

    #[test]
    fn unknown_label_is_rejected() {
        assert!(serde_json::from_str::<ScriptType>(r#""mystery""#).is_err());
    }
}
