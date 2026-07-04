//! Owned NFT records and their list-response wrappers.

use crate::nft::model::chain::{Chain, ContractType};
use crate::nft::model::metadata::UriMeta;
use crate::nft::serde_helpers::{token_id_from_string, token_id_to_string};
use ethereum_types::Address;
use mm2_number::{BigDecimal, BigUint};
use serde::{Deserialize, Serialize};

/// Fields that are common to both an owned NFT entry and the raw payload
/// returned by an external metadata provider.
///
/// Splitting the common fields off lets the provider layer reuse the same
/// shape when deserializing third-party JSON responses.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NftCommon {
    /// Address of the NFT contract.
    pub token_address: Address,
    /// Quantity owned. Always 1 for ERC-721; can exceed 1 for ERC-1155.
    pub amount: BigDecimal,
    /// Address that currently owns the token.
    pub owner_of: Address,
    /// Hash of the token metadata as published by the provider.
    pub token_hash: Option<String>,
    /// Collection name (mapped from the JSON `name` field).
    #[serde(rename = "name")]
    pub collection_name: Option<String>,
    /// Collection symbol.
    pub symbol: Option<String>,
    /// Raw `tokenURI(...)` value.
    pub token_uri: Option<String>,
    /// Domain extracted from `token_uri` (used by spam/phishing filters).
    pub token_domain: Option<String>,
    /// On-chain metadata blob, if any.
    pub metadata: Option<String>,
    /// RFC3339 timestamp of the last `tokenURI(...)` poll.
    pub last_token_uri_sync: Option<String>,
    /// RFC3339 timestamp of the last metadata refresh.
    pub last_metadata_sync: Option<String>,
    /// Original minter address (kept as a string because providers may
    /// return non-Ethereum-formatted values).
    pub minter_address: Option<String>,
    /// Spam-detection flag set by the providers layer.
    #[serde(default)]
    pub possible_spam: bool,
}

/// A single NFT owned by the wallet on a particular chain.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Nft {
    /// Provider-agnostic core fields.
    #[serde(flatten)]
    pub common: NftCommon,
    /// Chain on which the contract lives.
    pub chain: Chain,
    /// Token identifier within the contract.
    #[serde(serialize_with = "token_id_to_string", deserialize_with = "token_id_from_string")]
    pub token_id: BigUint,
    /// Block number at which the token was minted (when known).
    pub block_number_minted: Option<u64>,
    /// Block number at which the cached entry was last updated.
    pub block_number: u64,
    /// ERC-721 vs ERC-1155.
    pub contract_type: ContractType,
    /// Phishing-detection flag set by the providers layer.
    #[serde(default)]
    pub possible_phishing: bool,
    /// Cached, post-merge token metadata.
    pub uri_meta: UriMeta,
}

/// Compact view of a wallet-owned NFT, used by the swap layer and other
/// callers that do not need the full metadata payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NftInfo {
    /// Address of the NFT contract.
    pub token_address: Address,
    /// Token identifier within the contract.
    #[serde(serialize_with = "token_id_to_string")]
    pub token_id: BigUint,
    /// Chain on which the contract lives.
    pub chain: Chain,
    /// ERC-721 vs ERC-1155.
    pub contract_type: ContractType,
    /// Quantity owned.
    pub amount: BigDecimal,
}

/// Response payload for `get_nft_list`.
#[derive(Debug, PartialEq, Serialize)]
pub struct NftList {
    /// NFT records returned by the query.
    pub nfts: Vec<Nft>,
    /// Number of records skipped due to spam/phishing filters.
    pub skipped: usize,
    /// Total number of records that matched the query, ignoring pagination.
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    fn sample_nft() -> Nft {
        Nft {
            common: NftCommon {
                token_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A1").unwrap()),
                amount: BigDecimal::from(1u32),
                owner_of: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A2").unwrap()),
                token_hash: Some("hash".into()),
                collection_name: Some("Cats".into()),
                symbol: Some("CAT".into()),
                token_uri: Some("ipfs://uri".into()),
                token_domain: Some("ipfs".into()),
                metadata: None,
                last_token_uri_sync: None,
                last_metadata_sync: None,
                minter_address: None,
                possible_spam: false,
            },
            chain: Chain::Eth,
            token_id: BigUint::from(987_654u64),
            block_number_minted: Some(10),
            block_number: 20,
            contract_type: ContractType::Erc721,
            possible_phishing: false,
            uri_meta: UriMeta::default(),
        }
    }

    #[test]
    fn nft_token_id_serializes_as_string() {
        let value = sample_nft();
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["token_id"], json!("987654"));
        assert_eq!(json["chain"], json!("ETH"));
    }

    #[test]
    fn nft_round_trips_through_json() {
        let value = sample_nft();
        let json = serde_json::to_string(&value).unwrap();
        let back: Nft = serde_json::from_str(&json).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn nft_token_id_accepts_huge_values() {
        let huge = BigUint::from_str("12345678901234567890123456789012345678").unwrap();
        let mut value = sample_nft();
        value.token_id = huge.clone();
        let json = serde_json::to_string(&value).unwrap();
        let back: Nft = serde_json::from_str(&json).unwrap();
        assert_eq!(back.token_id, huge);
    }

    #[test]
    fn nft_info_serializes_token_id_as_string() {
        let info = NftInfo {
            token_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000B1").unwrap()),
            token_id: BigUint::from(7u32),
            chain: Chain::Polygon,
            contract_type: ContractType::Erc1155,
            amount: BigDecimal::from(3u32),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["token_id"], json!("7"));
        assert_eq!(json["contract_type"], json!("ERC1155"));
        assert_eq!(json["chain"], json!("POLYGON"));
    }
}
