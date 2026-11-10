//! TRON protobuf transaction types.
//!
//! TRON transactions are serialized via Protocol Buffers. The field tag numbers
//! must match the upstream TRON protocol exactly for signature compatibility.
//! Reference: https://github.com/tronprotocol/protocol/blob/master/core/Tron.proto

use prost::Message;

/// Top-level TRON transaction containing raw data and signatures.
#[derive(Clone, PartialEq, Message)]
pub struct Transaction {
    /// The raw transaction data (protobuf-encoded).
    #[prost(message, optional, tag = "1")]
    pub raw_data: Option<TransactionRaw>,
    /// Signatures: each is r(32) || s(32) || v(1) = 65 bytes. v ∈ {0, 1}.
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub signature: Vec<Vec<u8>>,
}

/// Raw transaction data. Field tags are non-sequential and MUST match
/// the upstream TRON protocol for correct signing.
#[derive(Clone, PartialEq, Message)]
pub struct TransactionRaw {
    /// Last 2 bytes of a recent block number (TAPOS anti-replay).
    #[prost(bytes = "vec", tag = "1")]
    pub ref_block_bytes: Vec<u8>,
    /// Bytes 8..16 of a recent block ID (TAPOS anti-replay).
    #[prost(bytes = "vec", tag = "4")]
    pub ref_block_hash: Vec<u8>,
    /// Transaction expiration timestamp (ms since epoch).
    #[prost(int64, tag = "8")]
    pub expiration: i64,
    /// Contract calls within this transaction (typically exactly one).
    #[prost(message, repeated, tag = "11")]
    pub contract: Vec<TransactionContract>,
    /// Transaction creation timestamp (ms since epoch).
    #[prost(int64, tag = "14")]
    pub timestamp: i64,
    /// Maximum TRX fee in SUN for smart contract execution.
    #[prost(int64, tag = "18")]
    pub fee_limit: i64,
}

/// A contract call within a transaction.
#[derive(Clone, PartialEq, Message)]
pub struct TransactionContract {
    /// Contract type identifier.
    #[prost(enumeration = "ContractType", tag = "1")]
    pub r#type: i32,
    /// Serialized contract parameter (as protobuf Any).
    #[prost(message, optional, tag = "2")]
    pub parameter: Option<prost_types::Any>,
}

/// TRON contract type identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum ContractType {
    /// Account creation. Value 0 is the protocol's default for this field, and
    /// prost omits a field equal to its default, so this MUST be the `Default`.
    AccountCreateContract = 0,
    /// TRX native transfer.
    TransferContract = 1,
    /// TRC10 token transfer.
    TransferAssetContract = 2,
    /// Smart contract invocation (TRC20 transfers, HTLC, etc.).
    TriggerSmartContract = 31,
}

impl Default for ContractType {
    fn default() -> Self { ContractType::AccountCreateContract }
}

impl TryFrom<i32> for ContractType {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ContractType::AccountCreateContract),
            1 => Ok(ContractType::TransferContract),
            2 => Ok(ContractType::TransferAssetContract),
            31 => Ok(ContractType::TriggerSmartContract),
            other => Err(other),
        }
    }
}

/// Native TRX transfer contract.
#[derive(Clone, PartialEq, Message)]
pub struct TransferContract {
    /// Sender address (21-byte TRON address).
    #[prost(bytes = "vec", tag = "1")]
    pub owner_address: Vec<u8>,
    /// Recipient address (21-byte TRON address).
    #[prost(bytes = "vec", tag = "2")]
    pub to_address: Vec<u8>,
    /// Amount in SUN (1 TRX = 1,000,000 SUN).
    #[prost(int64, tag = "3")]
    pub amount: i64,
}

/// Smart contract invocation (used for TRC20 transfers, HTLC, etc.).
#[derive(Clone, PartialEq, Message)]
pub struct TriggerSmartContract {
    /// Caller address (21-byte TRON address).
    #[prost(bytes = "vec", tag = "1")]
    pub owner_address: Vec<u8>,
    /// Contract address (21-byte TRON address).
    #[prost(bytes = "vec", tag = "2")]
    pub contract_address: Vec<u8>,
    /// Value in SUN to send with the call (0 for pure contract calls).
    #[prost(int64, tag = "3")]
    pub call_value: i64,
    /// ABI-encoded call data.
    #[prost(bytes = "vec", tag = "4")]
    pub data: Vec<u8>,
}

/// Block data used for TAPOS (Transaction as Proof of Stake) anti-replay.
#[derive(Clone, Debug)]
pub struct TaposBlockData {
    /// Last 2 bytes of block number.
    pub ref_block_bytes: Vec<u8>,
    /// Bytes 8..16 of block ID.
    pub ref_block_hash: Vec<u8>,
}

/// Default transaction expiration: 60 seconds.
pub const DEFAULT_EXPIRATION_SEC: u64 = 60;

/// Type URL for TransferContract in protobuf Any.
pub const TRANSFER_CONTRACT_TYPE_URL: &str = "type.googleapis.com/protocol.TransferContract";

/// Type URL for TriggerSmartContract in protobuf Any.
pub const TRIGGER_SMART_CONTRACT_TYPE_URL: &str = "type.googleapis.com/protocol.TriggerSmartContract";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_contract_roundtrip() {
        let contract = TransferContract {
            owner_address: vec![0x41; 21],
            to_address: vec![0x41; 21],
            amount: 1_000_000,
        };
        let encoded = contract.encode_to_vec();
        let decoded = TransferContract::decode(encoded.as_slice()).unwrap();
        assert_eq!(contract, decoded);
    }

    #[test]
    fn test_trigger_smart_contract_roundtrip() {
        let contract = TriggerSmartContract {
            owner_address: vec![0x41; 21],
            contract_address: vec![0x41; 21],
            call_value: 0,
            data: vec![0xa9, 0x05, 0x9c, 0xbb], // transfer(address,uint256) selector
        };
        let encoded = contract.encode_to_vec();
        let decoded = TriggerSmartContract::decode(encoded.as_slice()).unwrap();
        assert_eq!(contract, decoded);
    }

    #[test]
    fn test_transaction_raw_field_tags() {
        // Verify correct protobuf field ordering by encoding and checking
        // that a TransactionRaw with all fields set can round-trip correctly.
        let raw = TransactionRaw {
            ref_block_bytes: vec![0x01, 0x02],
            ref_block_hash: vec![0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a],
            expiration: 1700000000000,
            contract: vec![],
            timestamp: 1699999900000,
            fee_limit: 100_000_000,
        };
        let encoded = raw.encode_to_vec();
        let decoded = TransactionRaw::decode(encoded.as_slice()).unwrap();
        assert_eq!(raw, decoded);
    }

    #[test]
    fn test_full_transaction_roundtrip() {
        let raw = TransactionRaw {
            ref_block_bytes: vec![0xAB, 0xCD],
            ref_block_hash: vec![1, 2, 3, 4, 5, 6, 7, 8],
            expiration: 1700000060000,
            contract: vec![],
            timestamp: 1700000000000,
            fee_limit: 0,
        };
        let signature = vec![0u8; 65]; // dummy 65-byte signature
        let tx = Transaction {
            raw_data: Some(raw.clone()),
            signature: vec![signature],
        };
        let encoded = tx.encode_to_vec();
        let decoded = Transaction::decode(encoded.as_slice()).unwrap();
        assert_eq!(tx, decoded);
        assert_eq!(decoded.signature.len(), 1);
        assert_eq!(decoded.signature[0].len(), 65);
    }

    #[test]
    fn test_contract_type_default_is_protocol_zero() {
        assert_eq!(ContractType::default() as i32, 0);
        assert_eq!(ContractType::try_from(0), Ok(ContractType::AccountCreateContract));
        assert_eq!(ContractType::try_from(1), Ok(ContractType::TransferContract));
        assert_eq!(ContractType::try_from(31), Ok(ContractType::TriggerSmartContract));
    }

    #[test]
    fn test_transaction_contract_type_field_is_on_the_wire() {
        // Regression: with a non-zero enum default, field 1 (type = 1) was
        // skipped and the encoding started with the parameter tag 0x12.
        let contract = TransactionContract {
            r#type: ContractType::TransferContract as i32,
            parameter: Some(prost_types::Any {
                type_url: TRANSFER_CONTRACT_TYPE_URL.to_string(),
                value: vec![0x0a, 0x01, 0x41],
            }),
        };
        let encoded = contract.encode_to_vec();
        assert_ne!(encoded[0], 0x12, "type field omitted from the wire");
        assert_eq!(&encoded[..3], &[0x08, 0x01, 0x12]);
        let decoded = TransactionContract::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.r#type, 1);
    }
}
