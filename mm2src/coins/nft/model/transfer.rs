//! Historical NFT transfer entries and the response containers for them.

use crate::eth::EthTxFeeDetails;
use crate::nft::errors::ParseTransferStatusError;
use crate::nft::model::chain::{Chain, ContractType};
use crate::nft::model::nft::Nft;
use crate::nft::serde_helpers::{token_id_from_string, token_id_to_string};
use ethereum_types::Address;
use mm2_number::{BigDecimal, BigUint};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Direction of a recorded transfer relative to the wallet owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TransferStatus {
    /// The wallet was on the receiving end of the transfer.
    Receive,
    /// The wallet was on the sending end of the transfer.
    Send,
}

impl fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TransferStatus::Receive => "Receive",
            TransferStatus::Send => "Send",
        })
    }
}

impl FromStr for TransferStatus {
    type Err = ParseTransferStatusError;

    fn from_str(text: &str) -> Result<TransferStatus, ParseTransferStatusError> {
        match text {
            "Receive" => Ok(TransferStatus::Receive),
            "Send" => Ok(TransferStatus::Send),
            _ => Err(ParseTransferStatusError::Unsupported),
        }
    }
}

/// Provider-agnostic core fields shared between stored transfer entries
/// and raw payloads from external providers.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NftTransferCommon {
    /// Hash of the block the transfer was included in.
    pub block_hash: Option<String>,
    /// Hash of the transaction that performed the transfer.
    pub transaction_hash: String,
    /// Index of the transaction within its block.
    pub transaction_index: Option<u32>,
    /// Index of the `Transfer*` log within the transaction receipt.
    pub log_index: u32,
    /// Optional ETH value attached to the transaction.
    pub value: Option<BigDecimal>,
    /// Provider-supplied transaction type label.
    pub transaction_type: Option<String>,
    /// Address of the NFT contract.
    pub token_address: Address,
    /// Sender of the transfer.
    pub from_address: Address,
    /// Recipient of the transfer.
    pub to_address: Address,
    /// Quantity transferred (always 1 for ERC-721).
    pub amount: BigDecimal,
    /// Provider verification flag (1/0 if known).
    pub verified: Option<u32>,
    /// Optional ERC-1155 operator address.
    pub operator: Option<String>,
    /// Spam-detection flag set by the providers layer.
    #[serde(default)]
    pub possible_spam: bool,
}

/// A single historical NFT transfer record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NftTransfer {
    /// Provider-agnostic core fields.
    #[serde(flatten)]
    pub common: NftTransferCommon,
    /// Chain the transfer happened on.
    pub chain: Chain,
    /// Token identifier within the contract.
    #[serde(serialize_with = "token_id_to_string", deserialize_with = "token_id_from_string")]
    pub token_id: BigUint,
    /// Block number that included the transfer.
    pub block_number: u64,
    /// UNIX timestamp of the block.
    pub block_timestamp: u64,
    /// ERC-721 vs ERC-1155.
    pub contract_type: ContractType,
    /// Cached `tokenURI(...)`.
    pub token_uri: Option<String>,
    /// Domain extracted from `token_uri`.
    pub token_domain: Option<String>,
    /// Cached collection name.
    pub collection_name: Option<String>,
    /// Cached image URL.
    pub image_url: Option<String>,
    /// Cached image domain.
    pub image_domain: Option<String>,
    /// Cached token name.
    pub token_name: Option<String>,
    /// Direction relative to the owner.
    pub status: TransferStatus,
    /// Phishing-detection flag set by the providers layer.
    #[serde(default)]
    pub possible_phishing: bool,
    /// EVM fee details extracted from the transaction receipt.
    pub fee_details: Option<EthTxFeeDetails>,
    /// Number of confirmations the transfer has accumulated.
    pub confirmations: u64,
}

/// Response payload for `get_nft_transfers`.
#[derive(Debug, PartialEq, Serialize)]
pub struct NftTransferList {
    /// Transfer records returned by the query.
    pub transfer_history: Vec<NftTransfer>,
    /// Number of records skipped due to spam/phishing filters.
    pub skipped: usize,
    /// Total number of records that matched the query, ignoring pagination.
    pub total: usize,
}

/// Subset of NFT fields needed to back-fill metadata into transfer entries
/// (collection name, image URL, …) once the corresponding NFT record has
/// been fetched.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferMeta {
    /// Token contract address as a hex string.
    pub token_address: String,
    /// Token identifier.
    pub token_id: BigUint,
    /// Cached `tokenURI(...)`.
    pub token_uri: Option<String>,
    /// Domain extracted from `token_uri`.
    pub token_domain: Option<String>,
    /// Collection name.
    pub collection_name: Option<String>,
    /// Cached image URL.
    pub image_url: Option<String>,
    /// Cached image domain.
    pub image_domain: Option<String>,
    /// Cached token name.
    pub token_name: Option<String>,
}

impl From<Nft> for TransferMeta {
    fn from(nft: Nft) -> Self {
        TransferMeta {
            token_address: format!("{:?}", nft.common.token_address),
            token_id: nft.token_id,
            token_uri: nft.common.token_uri,
            token_domain: nft.common.token_domain,
            collection_name: nft.common.collection_name,
            image_url: nft.uri_meta.image_url,
            image_domain: nft.uri_meta.image_domain,
            token_name: nft.uri_meta.token_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nft::model::metadata::UriMeta;
    use crate::nft::model::nft::NftCommon;
    use serde_json::json;

    fn sample_transfer() -> NftTransfer {
        NftTransfer {
            common: NftTransferCommon {
                block_hash: Some("0xabc".into()),
                transaction_hash: "0xdef".into(),
                transaction_index: Some(1),
                log_index: 2,
                value: None,
                transaction_type: Some("eip1559".into()),
                token_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A1").unwrap()),
                from_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A2").unwrap()),
                to_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A3").unwrap()),
                amount: BigDecimal::from(1u32),
                verified: Some(1),
                operator: None,
                possible_spam: false,
            },
            chain: Chain::Bsc,
            token_id: BigUint::from(42u32),
            block_number: 5,
            block_timestamp: 1_700_000_000,
            contract_type: ContractType::Erc721,
            token_uri: Some("ipfs://x".into()),
            token_domain: Some("ipfs".into()),
            collection_name: Some("Cats".into()),
            image_url: None,
            image_domain: None,
            token_name: Some("Cat #42".into()),
            status: TransferStatus::Receive,
            possible_phishing: false,
            fee_details: None,
            confirmations: 12,
        }
    }

    #[test]
    fn transfer_serializes_token_id_as_string() {
        let value = sample_transfer();
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["token_id"], json!("42"));
        assert_eq!(json["chain"], json!("BSC"));
        assert_eq!(json["status"], json!("Receive"));
    }

    #[test]
    fn transfer_round_trips_through_json() {
        let value = sample_transfer();
        let json = serde_json::to_string(&value).unwrap();
        let back: NftTransfer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn transfer_status_parses_known_values() {
        assert_eq!("Send".parse::<TransferStatus>().unwrap(), TransferStatus::Send);
        assert!("send".parse::<TransferStatus>().is_err());
    }

    #[test]
    fn transfer_meta_extracts_token_metadata() {
        let nft = Nft {
            common: NftCommon {
                token_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000C1").unwrap()),
                amount: BigDecimal::from(1u32),
                owner_of: Address::from_slice(&hex::decode("00000000000000000000000000000000000000C2").unwrap()),
                token_hash: None,
                collection_name: Some("Birds".into()),
                symbol: None,
                token_uri: Some("https://x/".into()),
                token_domain: Some("x".into()),
                metadata: None,
                last_token_uri_sync: None,
                last_metadata_sync: None,
                minter_address: None,
                possible_spam: false,
            },
            chain: Chain::Eth,
            token_id: BigUint::from(9u32),
            block_number_minted: None,
            block_number: 0,
            contract_type: ContractType::Erc721,
            possible_phishing: false,
            uri_meta: UriMeta {
                image_url: Some("https://img/1.png".into()),
                image_domain: Some("img".into()),
                token_name: Some("Bird".into()),
                ..Default::default()
            },
        };
        let meta: TransferMeta = nft.into();
        assert_eq!(meta.token_id, BigUint::from(9u32));
        assert_eq!(meta.collection_name.as_deref(), Some("Birds"));
        assert_eq!(meta.image_url.as_deref(), Some("https://img/1.png"));
    }
}
