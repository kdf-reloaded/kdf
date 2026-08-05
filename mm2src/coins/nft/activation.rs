//! `enable_nft` activation entry point.
//!
//! `enable_nft` is the activation counterpart to `update_nft`: it brings
//! the NFT subsystem into existence for an EVM platform coin and performs
//! the first inventory crawl, whereas `update_nft` re-crawls an
//! already-active subsystem. The wire shape (mmrpc 2.0 envelope, request
//! fields and response fields) is dictated by the Komodo DeFi SDK / GUI
//! clients that call the method, so the structs below mirror that contract.
//!
//! Reloaded models NFT support as a per-context slot ([`NftCtx`]) rather
//! than as a coin in the registry; activation therefore registers and
//! seeds that slot instead of enabling a pseudo-coin, while keeping the
//! dictated request/response shapes intact for the clients.

use crate::nft::context::NftCtx;
use crate::nft::errors::EnableNftError;
use crate::nft::model::{Chain, ChainTicker, NftInfo};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use crate::nft::providers::{update_chain, HttpCrawlProvider};
#[cfg(not(target_arch = "wasm32"))]
use crate::nft::store::{ensure_initialised, NftListStore};
#[cfg(not(target_arch = "wasm32"))]
use crate::{lp_coinfind, MmCoinEnum};

/// `enable_nft` request payload (mmrpc 2.0 `params`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnableNftRequest {
    /// NFT pseudo-coin ticker (e.g. `NFT_ETH`) identifying the EVM chain
    /// whose NFT support is being activated.
    pub ticker: String,
    /// Optional inline protocol descriptor for a custom (non-config) NFT
    /// entry. When present its declared platform must agree with the
    /// platform resolved from `ticker`.
    #[serde(default)]
    pub protocol: Option<NftActivationProtocol>,
    /// NFT activation parameters.
    pub activation_params: NftActivationParams,
    /// Legacy activation envelopes can duplicate the provider at the top
    /// level. The canonical provider remains `activation_params.provider`.
    #[serde(default)]
    provider: Option<NftProvider>,
    /// Legacy coin-activation field accepted for wire compatibility.
    #[serde(default)]
    requires_notarization: Option<bool>,
    /// Legacy coin-activation field accepted for wire compatibility.
    #[serde(default)]
    priv_key_policy: Option<serde_json::Value>,
}

/// Inline NFT protocol descriptor. Left lenient (no `deny_unknown_fields`)
/// because SDK clients carry additional protocol metadata that the
/// context-slot model does not consume.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct NftActivationProtocol {
    /// Protocol tag; expected to be the NFT protocol marker.
    #[serde(rename = "type")]
    pub protocol_type: String,
    /// Protocol payload carrying the platform-coin ticker.
    pub protocol_data: NftProtocolData,
}

/// Payload of an inline [`NftActivationProtocol`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct NftProtocolData {
    /// Platform-coin ticker the NFT protocol is bound to.
    pub platform: String,
}

/// NFT activation parameters.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NftActivationParams {
    /// Indexer provider descriptor used for the initial inventory crawl.
    pub provider: NftProvider,
    /// Legacy coin-activation field accepted for wire compatibility.
    #[serde(default)]
    requires_notarization: Option<bool>,
    /// Legacy coin-activation field accepted for wire compatibility.
    #[serde(default)]
    priv_key_policy: Option<serde_json::Value>,
}

/// Indexer provider descriptor. Externally tagged: a `type` discriminant
/// selects the variant and an `info` object carries its configuration.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", content = "info", deny_unknown_fields)]
pub enum NftProvider {
    /// The vendor discriminant emitted by SDK/GUI clients (a vendor name).
    Moralis(NftProviderInfo),
}

/// Configuration carried by a [`NftProvider`] variant.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NftProviderInfo {
    /// Caller-supplied indexer base URL used for the initial crawl. No
    /// default is embedded; the caller supplies it at RPC time.
    pub url: Url,
    /// Reserved signed-proxy flag.
    #[serde(default)]
    pub komodo_proxy: bool,
}

/// `enable_nft` response payload.
#[derive(Debug, PartialEq, Serialize)]
pub struct EnableNftResponse {
    /// Owned-NFT snapshot observed during the initial crawl, keyed by
    /// token-id string.
    pub nfts: HashMap<String, NftInfo>,
    /// Platform-coin ticker the NFT subsystem was activated under.
    pub platform_coin: String,
}

/// Handler for the mmrpc 2.0 `enable_nft` method.
pub async fn enable_nft(ctx: MmArc, req: EnableNftRequest) -> MmResult<EnableNftResponse, EnableNftError> {
    let chain = Chain::from_nft_ticker(&req.ticker).map_to_mm(|_| EnableNftError::InvalidNftTicker {
        ticker: req.ticker.clone(),
    })?;
    let platform = chain.coin_ticker();

    if let Some(protocol) = &req.protocol {
        if !protocol.protocol_data.platform.eq_ignore_ascii_case(platform) {
            return MmError::err(EnableNftError::PlatformMismatch {
                declared: protocol.protocol_data.platform.clone(),
                resolved: platform.to_owned(),
            });
        }
    }

    let nft_ctx = NftCtx::from_mm_ctx(&ctx).map_to_mm(EnableNftError::Internal)?;
    if nft_ctx.is_activated(chain) {
        return MmError::err(EnableNftError::AlreadyActivated {
            ticker: req.ticker.clone(),
        });
    }

    let nfts = activate_chain(&ctx, &nft_ctx, chain, &req.activation_params.provider).await?;
    nft_ctx.mark_activated(chain);

    Ok(EnableNftResponse {
        nfts,
        platform_coin: platform.to_owned(),
    })
}

/// Native activation: validate the EVM platform coin, seed the per-chain
/// storage, run the initial crawl through the shared `update_chain` path
/// and return the resulting owned-NFT snapshot.
#[cfg(not(target_arch = "wasm32"))]
async fn activate_chain(
    ctx: &MmArc,
    nft_ctx: &NftCtx,
    chain: Chain,
    provider: &NftProvider,
) -> MmResult<HashMap<String, NftInfo>, EnableNftError> {
    let platform = chain.coin_ticker();
    let coin = match lp_coinfind(ctx, platform).await.map_to_mm(EnableNftError::Internal)? {
        Some(MmCoinEnum::EthCoin(eth)) => eth,
        Some(_) => {
            return MmError::err(EnableNftError::UnsupportedPlatform {
                coin: platform.to_owned(),
            })
        },
        None => {
            return MmError::err(EnableNftError::PlatformCoinIsNotActivated {
                coin: platform.to_owned(),
            })
        },
    };

    let store = nft_ctx.store();
    ensure_initialised(store, store, &chain)
        .await
        .mm_err(|err| EnableNftError::Storage(format!("{err:?}")))?;

    let NftProvider::Moralis(info) = provider;
    let crawl = HttpCrawlProvider::new(info.url.clone(), info.komodo_proxy);
    update_chain(store, &crawl, chain, coin.my_address)
        .await
        .mm_err(|err| EnableNftError::CrawlFailed(format!("{err:?}")))?;

    let list = NftListStore::list_owned(store, vec![chain], true, 0, None, None)
        .await
        .mm_err(|err| EnableNftError::Storage(format!("{err:?}")))?;
    let mut nfts = HashMap::with_capacity(list.nfts.len());
    for nft in list.nfts {
        nfts.insert(nft.token_id.to_string(), NftInfo {
            token_address: nft.common.token_address,
            token_id: nft.token_id,
            chain: nft.chain,
            contract_type: nft.contract_type,
            amount: nft.common.amount,
        });
    }
    Ok(nfts)
}

/// Browser activation: the initial crawl is deferred (D1), so the context
/// is registered and an empty snapshot is returned.
#[cfg(target_arch = "wasm32")]
async fn activate_chain(
    _ctx: &MmArc,
    _nft_ctx: &NftCtx,
    _chain: Chain,
    _provider: &NftProvider,
) -> MmResult<HashMap<String, NftInfo>, EnableNftError> {
    Ok(HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_request_with_defaults() {
        let req: EnableNftRequest = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "activation_params": {
                "provider": {
                    "type": "Moralis",
                    "info": { "url": "https://indexer.example.com/" }
                }
            }
        }))
        .unwrap();
        assert_eq!(req.ticker, "NFT_ETH");
        assert!(req.protocol.is_none());
        let NftProvider::Moralis(info) = &req.activation_params.provider;
        assert_eq!(info.url.as_str(), "https://indexer.example.com/");
        assert!(!info.komodo_proxy);
    }

    #[test]
    fn parses_komodo_proxy_flag() {
        let req: EnableNftRequest = serde_json::from_value(json!({
            "ticker": "NFT_MATIC",
            "activation_params": {
                "provider": {
                    "type": "Moralis",
                    "info": { "url": "https://indexer.example.com/", "komodo_proxy": true }
                }
            }
        }))
        .unwrap();
        let NftProvider::Moralis(info) = &req.activation_params.provider;
        assert!(info.komodo_proxy);
    }

    #[test]
    fn parses_legacy_activation_fields_from_wallet_request() {
        let req: EnableNftRequest = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "requires_notarization": false,
            "priv_key_policy": "Iguana",
            "provider": {
                "type": "Moralis",
                "info": { "url": "https://top-level.example/", "komodo_proxy": true }
            },
            "activation_params": {
                "requires_notarization": false,
                "priv_key_policy": "Iguana",
                "provider": {
                    "type": "Moralis",
                    "info": { "url": "https://indexer.example.com/", "komodo_proxy": true }
                }
            }
        }))
        .unwrap();

        let NftProvider::Moralis(info) = &req.activation_params.provider;
        assert_eq!(info.url.as_str(), "https://indexer.example.com/");
        assert!(info.komodo_proxy);
    }

    #[test]
    fn ticker_is_required() {
        let outcome: Result<EnableNftRequest, _> = serde_json::from_value(json!({
            "activation_params": {
                "provider": { "type": "Moralis", "info": { "url": "https://x.example/" } }
            }
        }));
        assert!(outcome.is_err());
    }

    #[test]
    fn provider_url_is_required() {
        let outcome: Result<EnableNftRequest, _> = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "activation_params": { "provider": { "type": "Moralis", "info": {} } }
        }));
        assert!(outcome.is_err());
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let outcome: Result<EnableNftRequest, _> = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "chain": "ETH",
            "activation_params": {
                "provider": { "type": "Moralis", "info": { "url": "https://x.example/" } }
            }
        }));
        assert!(outcome.is_err());
    }

    #[test]
    fn unknown_info_field_is_rejected() {
        let outcome: Result<EnableNftRequest, _> = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "activation_params": {
                "provider": {
                    "type": "Moralis",
                    "info": { "url": "https://x.example/", "bogus": 1 }
                }
            }
        }));
        assert!(outcome.is_err());
    }

    #[test]
    fn unknown_provider_type_is_rejected() {
        let outcome: Result<EnableNftRequest, _> = serde_json::from_value(json!({
            "ticker": "NFT_ETH",
            "activation_params": {
                "provider": { "type": "Unknown", "info": { "url": "https://x.example/" } }
            }
        }));
        assert!(outcome.is_err());
    }
}
