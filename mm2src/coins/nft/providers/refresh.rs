//! Single-token metadata refresh orchestration.
//!
//! Powers the `refresh_nft_metadata` RPC handler. Given a cached NFT
//! the orchestrator:
//!
//! 1. Fetches a fresh [`RefreshedMetadata`] payload from the configured
//!    metadata provider (HTTP by default; the [`MetadataProvider`] trait
//!    lets unit tests inject a deterministic stub).
//! 2. Normalises any URL fields and re-merges them into the cached
//!    [`Nft`]'s [`UriMeta`] block, leaving non-metadata fields
//!    (`amount`, `block_number`, ownership) untouched.
//! 3. Re-runs the spam-protection helpers so the freshly-pulled fields
//!    pick up the same flags as the originally-cached payload.
//! 4. Writes the merged record back through
//!    [`NftListStore::merge_metadata`] and propagates the new metadata
//!    into the historical transfer log via
//!    [`NftHistoryStore::attach_metadata_to_transfers`] so the GUI sees
//!    consistent values across the inventory and history endpoints.

use crate::nft::errors::UpdateNftError;
use crate::nft::model::{Chain, Nft, TransferMeta, UriMeta};
use crate::nft::providers::http::{fetch_json, FetchError};
use crate::nft::providers::spam::apply_spam_protection_to_nft;
use crate::nft::providers::url_helpers::{decamouflage_legacy_ipfs_url, domain_of, normalise_metadata_urls};
use crate::nft::store::history::NftHistoryStore;
use crate::nft::store::list::NftListStore;
use async_trait::async_trait;
use ethereum_types::Address;
use mm2_err_handle::prelude::*;
use mm2_number::BigUint;
use serde::{Deserialize, Serialize};
use url::Url;

/// Fresh metadata as returned by the configured provider.
///
/// The shape is deliberately permissive: every field is optional so the
/// caller does not need to know which provider populated which fields.
/// `uri_meta` is `#[serde(flatten)]`-ed so the provider can either embed
/// a nested object or hoist its keys (`image`, `name`, …) into the root.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RefreshedMetadata {
    /// Fresh `tokenURI(...)` URL, when the provider re-resolved it.
    #[serde(default)]
    pub token_uri: Option<String>,
    /// Raw on-chain metadata blob, if the provider exposes one.
    #[serde(default)]
    pub metadata: Option<String>,
    /// Updated collection name (some providers refresh this on every poll).
    #[serde(default)]
    pub collection_name: Option<String>,
    /// Updated minter address.
    #[serde(default)]
    pub minter_address: Option<String>,
    /// RFC3339 timestamp of the metadata refresh on the provider side.
    #[serde(default)]
    pub last_metadata_sync: Option<String>,
    /// RFC3339 timestamp of the last `tokenURI(...)` poll on the provider
    /// side.
    #[serde(default)]
    pub last_token_uri_sync: Option<String>,
    /// Image / animation / external URL block. Flattened so the provider
    /// can hoist its keys into the root JSON object.
    #[serde(flatten, default)]
    pub uri_meta: UriMeta,
}

/// Source of fresh NFT metadata. Abstracted into a trait so unit tests
/// can replace the HTTP fetch with a deterministic stub.
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// Fetch the fresh metadata payload for a single token.
    async fn fetch_metadata(
        &self,
        chain: Chain,
        token_address: Address,
        token_id: &BigUint,
    ) -> MmResult<RefreshedMetadata, FetchError>;
}

/// Default HTTP-backed [`MetadataProvider`].
///
/// Builds the request URL as `{base}/<chain-lowercase>/<contract>/<token_id>`
/// and `GET`s it as JSON. The optional Komodo proxy switch on the RPC
/// request is currently a no-op; signed-proxy support will land alongside
/// the rest of the proxy integration.
pub struct HttpMetadataProvider {
    /// Base URL (already validated by the RPC layer).
    base_url: Url,
    /// When true, route the request through the Komodo proxy. Reserved
    /// for the proxy-signing integration; currently unused.
    #[allow(dead_code)]
    pub komodo_proxy: bool,
}

impl HttpMetadataProvider {
    /// Construct a new HTTP-backed provider rooted at `base_url`.
    pub fn new(base_url: Url, komodo_proxy: bool) -> Self { HttpMetadataProvider { base_url, komodo_proxy } }
}

#[async_trait]
impl MetadataProvider for HttpMetadataProvider {
    async fn fetch_metadata(
        &self,
        chain: Chain,
        token_address: Address,
        token_id: &BigUint,
    ) -> MmResult<RefreshedMetadata, FetchError> {
        let chain_label = format!("{}", chain).to_ascii_lowercase();
        let address_hex = format!("{:#x}", token_address);
        let id_str = token_id.to_string();
        let url = format!(
            "{}/{}/{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            chain_label,
            address_hex,
            id_str
        );
        fetch_json::<RefreshedMetadata>(&url, &[]).await
    }
}

/// Refresh metadata for a single token already present in the inventory.
///
/// Returns [`UpdateNftError::TokenNotFoundInWallet`] when the cache does
/// not yet contain the requested token. The orchestrator is generic over
/// the storage backend (so it can run against the SQLite or IndexedDB
/// store interchangeably) and over the metadata provider (so the unit
/// tests can avoid any HTTP traffic).
pub async fn refresh_nft_metadata<S, P>(
    store: &S,
    provider: &P,
    chain: Chain,
    token_address: Address,
    token_id: BigUint,
) -> MmResult<(), UpdateNftError>
where
    S: NftListStore + NftHistoryStore,
    P: MetadataProvider,
{
    let token_address_hex = format!("{:#x}", token_address);
    let mut cached = NftListStore::fetch_token(store, &chain, token_address_hex.clone(), token_id.clone())
        .await
        .mm_err(|err| UpdateNftError::Storage(format!("{err:?}")))?
        .ok_or(UpdateNftError::TokenNotFoundInWallet {
            token_address: token_address_hex.clone(),
            token_id: token_id.to_string(),
        })?;

    let fresh = provider
        .fetch_metadata(chain, token_address, &token_id)
        .await
        .mm_err(|err| UpdateNftError::Provider(err.to_string()))?;

    apply_refreshed_metadata(&mut cached, fresh);

    apply_spam_protection_to_nft(&mut cached, false).map_err(|err| {
        MmError::new(UpdateNftError::Internal(format!(
            "spam scan after metadata refresh: {err}"
        )))
    })?;

    NftListStore::merge_metadata(store, &chain, cached.clone())
        .await
        .mm_err(|err| UpdateNftError::Storage(format!("{err:?}")))?;

    let meta = TransferMeta::from(cached.clone());
    let flag_spam = cached.common.possible_spam;
    NftHistoryStore::attach_metadata_to_transfers(store, &chain, meta, flag_spam)
        .await
        .mm_err(|err| UpdateNftError::Storage(format!("{err:?}")))?;

    Ok(())
}

/// Merge a [`RefreshedMetadata`] payload into `cached` in place.
///
/// Top-level fields (`token_uri`, `metadata`, …) are overwritten when
/// the provider supplied a value; cached values are preserved otherwise.
/// URL companions (`token_domain`, `image_domain`, …) are recomputed
/// after the merge so they always agree with the URLs they describe.
fn apply_refreshed_metadata(cached: &mut Nft, fresh: RefreshedMetadata) {
    if let Some(uri) = fresh.token_uri {
        let rewritten = decamouflage_legacy_ipfs_url(Some(uri.as_str())).unwrap_or(uri);
        cached.common.token_domain = domain_of(Some(rewritten.as_str()));
        cached.common.token_uri = Some(rewritten);
    }
    if fresh.metadata.is_some() {
        cached.common.metadata = fresh.metadata;
    }
    if let Some(name) = fresh.collection_name {
        cached.common.collection_name = Some(name);
    }
    if let Some(minter) = fresh.minter_address {
        cached.common.minter_address = Some(minter);
    }
    if let Some(ts) = fresh.last_metadata_sync {
        cached.common.last_metadata_sync = Some(ts);
    }
    if let Some(ts) = fresh.last_token_uri_sync {
        cached.common.last_token_uri_sync = Some(ts);
    }

    let mut merged = fresh.uri_meta;
    merged.merge_in(std::mem::take(&mut cached.uri_meta));
    normalise_metadata_urls(&mut merged);
    cached.uri_meta = merged;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nft::model::{ContractType, Nft, NftCommon};
    use mm2_number::BigDecimal;

    fn sample_nft() -> Nft {
        Nft {
            common: NftCommon {
                token_address: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A1").unwrap()),
                amount: BigDecimal::from(1u32),
                owner_of: Address::from_slice(&hex::decode("00000000000000000000000000000000000000A2").unwrap()),
                token_hash: None,
                collection_name: Some("Old Name".into()),
                symbol: None,
                token_uri: Some("ipfs://old".into()),
                token_domain: Some("ipfs".into()),
                metadata: None,
                last_token_uri_sync: None,
                last_metadata_sync: None,
                minter_address: None,
                possible_spam: false,
            },
            chain: Chain::Eth,
            token_id: BigUint::from(1u32),
            block_number_minted: Some(10),
            block_number: 20,
            contract_type: ContractType::Erc721,
            possible_phishing: false,
            uri_meta: UriMeta {
                image_url: Some("https://example.com/old.png".into()),
                image_domain: Some("example.com".into()),
                token_name: Some("Old Token".into()),
                ..UriMeta::default()
            },
        }
    }

    #[test]
    fn apply_refreshed_metadata_overwrites_only_supplied_fields() {
        let mut nft = sample_nft();
        let fresh = RefreshedMetadata {
            token_uri: Some("https://example.com/refresh.json".into()),
            collection_name: Some("New Name".into()),
            uri_meta: UriMeta {
                image_url: Some("https://cdn.example/new.png".into()),
                token_name: Some("New Token".into()),
                ..UriMeta::default()
            },
            ..RefreshedMetadata::default()
        };
        apply_refreshed_metadata(&mut nft, fresh);
        assert_eq!(nft.common.collection_name.as_deref(), Some("New Name"));
        assert_eq!(
            nft.common.token_uri.as_deref(),
            Some("https://example.com/refresh.json")
        );
        assert_eq!(nft.common.token_domain.as_deref(), Some("example.com"));
        // Symbol was not refreshed, original cached value preserved.
        assert_eq!(nft.common.symbol, None);
        // Image is taken from the fresh payload, domain re-derived.
        assert_eq!(nft.uri_meta.image_url.as_deref(), Some("https://cdn.example/new.png"));
        assert_eq!(nft.uri_meta.image_domain.as_deref(), Some("cdn.example"));
        // Token name on the fresh payload wins over the cached value.
        assert_eq!(nft.uri_meta.token_name.as_deref(), Some("New Token"));
    }

    #[test]
    fn apply_refreshed_metadata_keeps_cached_values_when_fresh_is_empty() {
        let mut nft = sample_nft();
        let fresh = RefreshedMetadata::default();
        apply_refreshed_metadata(&mut nft, fresh);
        assert_eq!(nft.common.collection_name.as_deref(), Some("Old Name"));
        assert_eq!(nft.common.token_uri.as_deref(), Some("ipfs://old"));
        assert_eq!(nft.uri_meta.token_name.as_deref(), Some("Old Token"));
        assert_eq!(nft.uri_meta.image_url.as_deref(), Some("https://example.com/old.png"));
    }

    #[test]
    fn apply_refreshed_metadata_rewrites_legacy_ipfs_token_uri() {
        let mut nft = sample_nft();
        let fresh = RefreshedMetadata {
            token_uri: Some("https://ipfs.moralis.io/ipfs/bafyabc/0.json".into()),
            ..RefreshedMetadata::default()
        };
        apply_refreshed_metadata(&mut nft, fresh);
        assert_eq!(
            nft.common.token_uri.as_deref(),
            Some("https://ipfs.io/ipfs/bafyabc/0.json")
        );
        assert_eq!(nft.common.token_domain.as_deref(), Some("ipfs.io"));
    }

    #[test]
    fn refreshed_metadata_deserialises_flattened_uri_meta() {
        let json = serde_json::json!({
            "token_uri": "https://example.com/x.json",
            "image": "https://example.com/x.png",
            "name": "Flat Token"
        });
        let fresh: RefreshedMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(fresh.token_uri.as_deref(), Some("https://example.com/x.json"));
        assert_eq!(
            fresh.uri_meta.raw_image_url.as_deref(),
            Some("https://example.com/x.png")
        );
        assert_eq!(fresh.uri_meta.token_name.as_deref(), Some("Flat Token"));
    }
}
