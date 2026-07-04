// SPDX-License-Identifier: GPL-2.0-only
//! Bitcoin/Komodo `getrawtransaction verbose=1` JSON wire types.
//!
//! Field names and shapes mirror the documented Bitcoin Core RPC surface (with
//! Komodo / Qtum / FIRO extensions). Only the data layout is reproduced; no
//! source code from any prior implementation is used.

use super::address::AddressVisitor;
use super::bytes::Bytes;
use super::hash::H256;
use super::script::ScriptType;
use keys::Address;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A raw transaction (hex-encoded bytes) — wire alias around [`Bytes`].
pub type RawTransaction = Bytes;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct TransactionInput {
    pub txid: H256,
    pub vout: u32,
    pub sequence: Option<u32>,
}

#[derive(Debug, PartialEq)]
pub struct TransactionOutputWithAddress {
    pub address: Address,
    pub amount: f64,
}

#[derive(Debug, PartialEq)]
pub struct TransactionOutputWithScriptData {
    pub script_data: Bytes,
}

#[derive(Debug, PartialEq)]
pub enum TransactionOutput {
    Address(TransactionOutputWithAddress),
    ScriptData(TransactionOutputWithScriptData),
}

/// Outputs serialized as a JSON map keyed by destination (`address` literal or `"data"`).
#[derive(Debug, PartialEq)]
pub struct TransactionOutputs {
    pub outputs: Vec<TransactionOutput>,
}

impl TransactionOutputs {
    pub fn len(&self) -> usize { self.outputs.len() }
    pub fn is_empty(&self) -> bool { self.outputs.is_empty() }
}

impl Serialize for TransactionOutputs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.outputs.len()))?;
        for entry in &self.outputs {
            match entry {
                TransactionOutput::Address(addr_out) => {
                    map.serialize_entry(&addr_out.address.to_string(), &addr_out.amount)?;
                },
                TransactionOutput::ScriptData(script_out) => {
                    map.serialize_entry("data", &script_out.script_data)?;
                },
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TransactionOutputs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OutputsMapVisitor;

        impl<'de> Visitor<'de> for OutputsMapVisitor {
            type Value = TransactionOutputs;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a transaction outputs object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut accumulated: Vec<TransactionOutput> =
                    Vec::with_capacity(access.size_hint().unwrap_or_default());
                while let Some(field_name) = access.next_key::<String>()? {
                    if field_name == "data" {
                        let script_data: Bytes = access.next_value()?;
                        accumulated.push(TransactionOutput::ScriptData(TransactionOutputWithScriptData {
                            script_data,
                        }));
                    } else {
                        let address = AddressVisitor.visit_str(&field_name)?;
                        let amount: f64 = access.next_value()?;
                        accumulated.push(TransactionOutput::Address(TransactionOutputWithAddress {
                            address,
                            amount,
                        }));
                    }
                }
                Ok(TransactionOutputs { outputs: accumulated })
            }
        }

        deserializer.deserialize_identifier(OutputsMapVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransactionInputScript {
    pub asm: String,
    pub hex: Bytes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransactionOutputScript {
    pub asm: String,
    pub hex: Bytes,
    #[serde(default, rename = "reqSigs")]
    pub req_sigs: u32,
    #[serde(rename = "type")]
    pub script_type: ScriptType,
    #[serde(default)]
    pub addresses: Vec<String>,
}

impl TransactionOutputScript {
    pub fn is_empty(&self) -> bool { self.asm.is_empty() && self.hex.is_empty() }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransactionInputEnum {
    Signed(SignedTransactionInput),
    Coinbase(CoinbaseTransactionInput),
    /// FIRO Sigma spend
    Sigma(SigmaInput),
    /// FIRO Lelantus spend
    Lelantus(LelantusInput),
}

impl TransactionInputEnum {
    pub fn is_coinbase(&self) -> bool { matches!(self, TransactionInputEnum::Coinbase(_)) }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SigmaInput {
    #[serde(rename = "anonymityGroup")]
    anonymity_group: i64,
    #[serde(rename = "scriptSig")]
    pub script_sig: TransactionInputScript,
    value: f64,
    #[serde(rename = "valueSat")]
    value_sat: u64,
    sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LelantusInput {
    #[serde(rename = "scriptSig")]
    pub script_sig: TransactionInputScript,
    #[serde(rename = "nFees")]
    pub n_fees: f64,
    serials: Vec<String>,
    sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignedTransactionInput {
    pub txid: H256,
    pub vout: u32,
    #[serde(rename = "scriptSig")]
    pub script_sig: TransactionInputScript,
    pub sequence: u32,
    pub txinwitness: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoinbaseTransactionInput {
    pub coinbase: Bytes,
    pub sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignedTransactionOutput {
    pub value: Option<f64>,
    pub n: u32,
    #[serde(rename = "scriptPubKey")]
    pub script: TransactionOutputScript,
}

impl SignedTransactionOutput {
    pub fn is_empty(&self) -> bool { self.value == Some(0.0) && self.script.is_empty() }
}

/// Coerce JSON `null` into the type's `Default::default()` value during deserialization.
fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt: Option<T> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub hex: RawTransaction,
    pub txid: H256,
    pub hash: Option<H256>,
    pub size: Option<usize>,
    pub vsize: Option<usize>,
    pub version: i32,
    pub locktime: u32,
    pub vin: Vec<TransactionInputEnum>,
    pub vout: Vec<SignedTransactionOutput>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub blockhash: H256,
    #[serde(default, deserialize_with = "null_or_default")]
    pub confirmations: u32,
    /// KMD-only: present on assetchains that report notarisation depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rawconfirmations: Option<u32>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub time: u32,
    #[serde(default, deserialize_with = "null_or_default")]
    pub blocktime: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
}

impl Transaction {
    pub fn is_coinbase(&self) -> bool { self.vin.iter().any(TransactionInputEnum::is_coinbase) }
}

/// Result of `getrawtransaction` — either the raw hex form or the verbose object.
#[derive(Debug, PartialEq)]
pub enum GetRawTransactionResponse {
    Raw(RawTransaction),
    Verbose(Box<Transaction>),
}

impl Serialize for GetRawTransactionResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            GetRawTransactionResponse::Raw(raw) => raw.serialize(serializer),
            GetRawTransactionResponse::Verbose(verbose) => verbose.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn transaction_input_round_trip() {
        let payload = TransactionInput {
            txid: H256::from(7),
            vout: 33,
            sequence: Some(88),
        };
        let json =
            r#"{"txid":"0700000000000000000000000000000000000000000000000000000000000000","vout":33,"sequence":88}"#;
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
        assert_eq!(serde_json::from_str::<TransactionInput>(json).unwrap(), payload);
    }

    #[test]
    fn outputs_serialize_as_address_amount_map() {
        let payload = TransactionOutputs {
            outputs: vec![
                TransactionOutput::Address(TransactionOutputWithAddress {
                    address: "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into(),
                    amount: 123.45,
                }),
                TransactionOutput::Address(TransactionOutputWithAddress {
                    address: "1H5m1XzvHsjWX3wwU781ubctznEpNACrNC".into(),
                    amount: 67.89,
                }),
                TransactionOutput::ScriptData(TransactionOutputWithScriptData {
                    script_data: Bytes::new(vec![1, 2, 3, 4]),
                }),
                TransactionOutput::ScriptData(TransactionOutputWithScriptData {
                    script_data: Bytes::new(vec![5, 6, 7, 8]),
                }),
            ],
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa":123.45,"1H5m1XzvHsjWX3wwU781ubctznEpNACrNC":67.89,"data":"01020304","data":"05060708"}"#
        );
    }

    #[test]
    fn input_script_round_trip() {
        let payload = TransactionInputScript {
            asm: "Hello, world!!!".to_owned(),
            hex: Bytes::new(vec![1, 2, 3, 4]),
        };
        let json = r#"{"asm":"Hello, world!!!","hex":"01020304"}"#;
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
        assert_eq!(serde_json::from_str::<TransactionInputScript>(json).unwrap(), payload);
    }

    #[test]
    fn output_script_round_trip() {
        let payload = TransactionOutputScript {
            asm: "Hello, world!!!".to_owned(),
            hex: Bytes::new(vec![1, 2, 3, 4]),
            req_sigs: 777,
            script_type: ScriptType::Multisig,
            addresses: vec![
                "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into(),
                "1H5m1XzvHsjWX3wwU781ubctznEpNACrNC".into(),
            ],
        };
        let json = r#"{"asm":"Hello, world!!!","hex":"01020304","reqSigs":777,"type":"multisig","addresses":["1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa","1H5m1XzvHsjWX3wwU781ubctznEpNACrNC"]}"#;
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
        assert_eq!(serde_json::from_str::<TransactionOutputScript>(json).unwrap(), payload);
    }

    #[test]
    fn signed_input_serializes_with_null_witness() {
        let payload = SignedTransactionInput {
            txid: H256::from(77),
            vout: 13,
            script_sig: TransactionInputScript {
                asm: "Hello, world!!!".to_owned(),
                hex: Bytes::new(vec![1, 2, 3, 4]),
            },
            sequence: 123,
            txinwitness: None,
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"txid":"4d00000000000000000000000000000000000000000000000000000000000000","vout":13,"scriptSig":{"asm":"Hello, world!!!","hex":"01020304"},"sequence":123,"txinwitness":null}"#
        );
    }

    #[test]
    fn signed_input_deserializes_with_array_witness() {
        let parsed: SignedTransactionInput = serde_json::from_str(
            r#"{"txid":"4d00000000000000000000000000000000000000000000000000000000000000","vout":13,"scriptSig":{"asm":"Hello, world!!!","hex":"01020304"},"sequence":123,"txinwitness":[]}"#,
        )
        .unwrap();
        assert_eq!(parsed.txinwitness, Some(vec![]));
    }

    #[test]
    fn signed_output_round_trip() {
        let payload = SignedTransactionOutput {
            value: Some(777.79),
            n: 12,
            script: TransactionOutputScript {
                asm: "Hello, world!!!".to_owned(),
                hex: Bytes::new(vec![1, 2, 3, 4]),
                req_sigs: 777,
                script_type: ScriptType::Multisig,
                addresses: vec![
                    "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into(),
                    "1H5m1XzvHsjWX3wwU781ubctznEpNACrNC".into(),
                ],
            },
        };
        let json = r#"{"value":777.79,"n":12,"scriptPubKey":{"asm":"Hello, world!!!","hex":"01020304","reqSigs":777,"type":"multisig","addresses":["1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa","1H5m1XzvHsjWX3wwU781ubctznEpNACrNC"]}}"#;
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
        assert_eq!(serde_json::from_str::<SignedTransactionOutput>(json).unwrap(), payload);
    }

    #[test]
    fn full_transaction_serializes_with_height() {
        let payload = Transaction {
            hex: "DEADBEEF".into(),
            txid: H256::from(4),
            hash: Some(H256::from(5)),
            size: Some(33),
            vsize: Some(44),
            version: 55,
            locktime: 66,
            vin: vec![],
            vout: vec![],
            blockhash: H256::from(6),
            confirmations: 77,
            rawconfirmations: None,
            time: 88,
            blocktime: 99,
            height: Some(0),
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"hex":"deadbeef","txid":"0400000000000000000000000000000000000000000000000000000000000000","hash":"0500000000000000000000000000000000000000000000000000000000000000","size":33,"vsize":44,"version":55,"locktime":66,"vin":[],"vout":[],"blockhash":"0600000000000000000000000000000000000000000000000000000000000000","confirmations":77,"time":88,"blocktime":99,"height":0}"#
        );
    }

    #[test]
    fn full_transaction_deserializes_without_height() {
        let parsed: Transaction = serde_json::from_str(
            r#"{"hex":"deadbeef","txid":"0400000000000000000000000000000000000000000000000000000000000000","hash":"0500000000000000000000000000000000000000000000000000000000000000","size":33,"vsize":44,"version":55,"locktime":66,"vin":[],"vout":[],"blockhash":"0600000000000000000000000000000000000000000000000000000000000000","confirmations":77,"time":88,"blocktime":99}"#,
        )
        .unwrap();
        assert_eq!(parsed.height, None);
        assert_eq!(parsed.confirmations, 77);
        assert_eq!(parsed.blockhash, H256::from(6));
    }

    /// KMD verbose getrawtransaction (88893f...) — exercises null/missing fields and address arrays.
    /// https://kmdexplorer.io/tx/88893f05764f5a781f2e555a5b492c064f2269a4a44c51afdbe98fab54361bb5
    #[test]
    fn parses_kmd_verbose_transaction() {
        let json = r#"{
            "hex":"0100000001ebca38fa14b1ec029c3e08a2e87940c1f796b1588674b4c386f09626ee702576010000006a4730440220070963b9460d9bafe7865563574594fc3f823e5cdf7c49a5642dade76502547f022023fd90d41e34e514237f4b5967f83c9af27673d6de2eae3d88079a988fa5be3e012103668e3368c9fb67d8fc808a5fe74d5a8d21b6eed726838122d5f7716fb3328998ffffffff03e87006060000000017a914fef59ae800bb89050d25f67be432b231097e1849878758c100000000001976a91473122bcec852f394e51496e39fca5111c3d7ae5688ac00000000000000000a6a08303764643135633400000000",
            "txid":"88893f05764f5a781f2e555a5b492c064f2269a4a44c51afdbe98fab54361bb5",
            "version":1,
            "locktime":0,
            "vin":[{"txid":"762570ee2696f086c3b4748658b196f7c14079e8a2083e9c02ecb114fa38caeb","vout":1,"scriptSig":{"asm":"sig","hex":"4730"},"sequence":4294967295}],
            "vout":[{"value":1.01085416,"n":0,"scriptPubKey":{"asm":"OP_HASH160","hex":"a914","reqSigs":1,"type":"scripthash","addresses":["bbyNYu11Qs3PowiPr1Su4ozQk7hsVmv821"]}}],
            "blockhash":"086c0807a67d8411743f7eaf0a687721eadaa6c8190dfd36f4de9d939c796e82",
            "height":865648,
            "confirmations":549608,
            "rawconfirmations":549608,
            "time":1528215344,
            "blocktime":1528215344
        }"#;
        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.rawconfirmations, Some(549608));
        assert_eq!(parsed.height, Some(865648));
        assert_eq!(parsed.vin.len(), 1);
        assert_eq!(parsed.vout.len(), 1);
    }

    /// KMD coinbase deserialization — verifies `TransactionInputEnum::Coinbase` matches.
    #[test]
    fn parses_kmd_coinbase_transaction() {
        let json = r#"{
            "hex":"04",
            "txid":"6f173d96987e765b0fd8a47fdb976e8edc767207f3c0028e17a224380d9a14a3",
            "version":4,
            "locktime":1563416858,
            "vin":[{"coinbase":"030a4b020101","sequence":4294967295}],
            "vout":[],
            "blockhash":"04b08f77065a70c86fd47e92cbff2cd73b1768428da7c8e328d903d76e8dc37e",
            "height":150282,
            "confirmations":1,
            "rawconfirmations":6,
            "time":1563416858,
            "blocktime":1563416858
        }"#;
        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert!(parsed.is_coinbase());
    }

    /// Qtum `call` script type round-trips through ScriptType::Call.
    #[test]
    fn parses_qtum_call_script_pubkey() {
        let json = r#"{
            "hex":"01",
            "txid":"fad39a18206633258a0e77cc59d4606553bab374d05bb3d56cf3f0a701bacfaf",
            "version":1,
            "locktime":1589537755,
            "vin":[],
            "vout":[{"n":0,"value":10.0,"scriptPubKey":{"asm":"OP_CALL","hex":"5403","type":"call"}}],
            "blockhash":"b81f26a919bc9d792aeb056d6eea5340b7e334aa3f21144cd0f3c663286ff870",
            "confirmations":2457,
            "time":1589537936,
            "blocktime":1589537936
        }"#;
        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.vout[0].script.script_type, ScriptType::Call);
    }

    /// Verus / VRSC-family verbose responses use the `cryptocondition` script label.
    #[test]
    fn parses_verus_cryptocondition_script_pubkey() {
        let json = r#"{
            "hex":"01",
            "txid":"09d0ab79a668ae95ef4ab0ff9c8c9419b641a097c7acec5fcf1a862bdde39a92",
            "version":4,
            "locktime":0,
            "vin":[],
            "vout":[{"value":0.0,"n":0,"scriptPubKey":{"asm":"0403000101 OP_CHECKCRYPTOCONDITION 04030d0101 OP_DROP","hex":"270403000101cc4c75","reqSigs":1,"type":"cryptocondition","addresses":["RKLN7wFhbrJFkPG8XkKteErAe5CjqoddTm"]}}],
            "blockhash":"00000000000834c580bc7ce98e14c7ce487d435957ad4b3324b5c58b09cbafb7",
            "height":510665,
            "confirmations":1,
            "time":1743720786,
            "blocktime":1743720786
        }"#;
        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.vout[0].script.script_type, ScriptType::CryptoCondition);
    }

    #[test]
    fn parses_firo_sigma_input() {
        let json = r#"{
            "hex":"01",
            "txid":"d4b9f5a01a43b1d592999f9fd6fe64aa8f63ac42abab43090938321064c1ec1f",
            "version":1,
            "locktime":327895,
            "vin":[{"anonymityGroup":1,"scriptSig":{"asm":"OP_SIGMASPEND","hex":"c4"},"value":100.0,"valueSat":10000000000,"sequence":4294967295}],
            "vout":[],
            "blockhash":"0000000000000000000000000000000000000000000000000000000000000000",
            "confirmations":1,
            "time":1,
            "blocktime":1
        }"#;
        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert!(matches!(parsed.vin[0], TransactionInputEnum::Sigma(_)));
    }

    #[test]
    fn parses_firo_lelantus_jmint_output() {
        let json = r#"{
            "hex":"01",
            "txid":"06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba8",
            "version":1,
            "locktime":368918,
            "vin":[],
            "vout":[{"value":0.0,"n":1,"scriptPubKey":{"asm":"OP_LELANTUSJMINT","hex":"c6","type":"lelantusjmint","addresses":["Lelantusjmint"]}}],
            "blockhash":"0000000000000000000000000000000000000000000000000000000000000000",
            "confirmations":1,
            "time":1,
            "blocktime":1
        }"#;

        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.vout[0].script.script_type, ScriptType::LelantusJMint);
    }

    #[test]
    fn parses_firo_sparkmint_output() {
        let json = r#"{
            "hex":"01",
            "txid":"06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba1",
            "version":1,
            "locktime":368919,
            "vin":[],
            "vout":[{"value":0.0,"n":1,"scriptPubKey":{"asm":"OP_SPARKMINT","hex":"c7","type":"sparkmint","addresses":["Sparkmint"]}}],
            "blockhash":"0000000000000000000000000000000000000000000000000000000000000000",
            "confirmations":1,
            "time":1,
            "blocktime":1
        }"#;

        let parsed: Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.vout[0].script.script_type, ScriptType::SparkMint);
    }
}
