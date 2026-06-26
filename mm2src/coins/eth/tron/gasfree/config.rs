//! GasFree configuration: per-network constants, provider binding, and the
//! per-token gasless block, plus their activation-time validation.
//!
//! The provider binding ([`TronGaslessProviderConfig`]) carries credentials
//! (`api_key` / `api_secret`); they are never printed (`Debug` is redacted,
//! §49.3.1 R-credential / R9) nor serialized.
//!
//! Per-network contract artifacts (controller, beacon, proxy creation
//! bytecode, §49.5) are *published GasFree SDK constants*. They are hard-bound
//! per network and are **not** caller-configurable (R2). The controller and
//! beacon are the published per-network base58 Tron addresses (decoded to their
//! EVM form here). The proxy creation bytecode is a GasFree SDK artifact that
//! this chapter references rather than embeds; it remains a placeholder until
//! the official per-network bytes are pinned (see the `TODO(gasfree)` in
//! `network_artifacts` and the chapter's §49.13 external references).

use super::error::GasFreeConfigError;
use crate::eth::tron::address::TronAddress;
use crate::eth::tron::Network;
use bigdecimal::BigDecimal;
use ethereum_types::Address as EthAddress;
use serde::Deserialize;
use std::fmt;

/// Default authorization validity window (seconds) when the withdraw request
/// omits `deadline_seconds` (§49.3.4 / §49.8 step 3).
pub const DEFAULT_DEADLINE_SECONDS: u64 = 600;

/// Per-network published GasFree contract artifacts (§49.5).
///
/// These three constants are dictated by the GasFree SDK and are hard-bound per
/// network (R2). `creation_bytecode` is the proxy creation bytecode; it is
/// referenced rather than embedded here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GasFreeArtifacts {
    /// GasFree controller contract (EVM-form 20-byte address). Also the
    /// `verifyingContract` of the TIP-712 domain (§49.6).
    pub controller: EthAddress,
    /// GasFree beacon contract (EVM-form 20-byte address).
    pub beacon: EthAddress,
    /// Proxy creation bytecode (the GasFree SDK artifact).
    pub creation_bytecode: &'static [u8],
}

/// TIP-712 chain id for the GasFree domain (§49.6). These are the dictated
/// per-network Tron EIP-712 chain ids: mainnet `728126428`, Shasta
/// `2494104990`, Nile `3448148188`.
pub fn tip712_chain_id(network: &Network) -> u64 {
    match network {
        Network::Mainnet => 728_126_428,
        Network::Shasta => 2_494_104_990,
        Network::Nile => 3_448_148_188,
    }
}

/// Per-network REST path segment prepended before the `api/v1` prefix
/// (§49.4.1): `tron` for mainnet, `nile` / `shasta` for the testnets.
pub fn network_path_segment(network: &Network) -> &'static str {
    match network {
        Network::Mainnet => "tron",
        Network::Shasta => "shasta",
        Network::Nile => "nile",
    }
}

/// Per-network published GasFree controller/beacon addresses (§49.5, dictated
/// interop). These are the base58 Tron addresses published by the GasFree SDK.
fn controller_beacon_base58(network: &Network) -> (&'static str, &'static str) {
    match network {
        Network::Mainnet => (
            "TFFAMQLZybALaLb4uxHA9RBE7pxhUAjF3U",
            "TSP9UW6FQhT76XD2jWA6ipGMx3yGbjDffP",
        ),
        Network::Nile => (
            "THQGuFzL87ZqhxkgqYEryRAd7gqFqL5rdc",
            "TLtCGmaxH3PbuaF6kbybwteZcHptEdgQGC",
        ),
        Network::Shasta => (
            "TQghdCeVDA6CnuNVTUhfaAyPfTetqZWNpm",
            "TQ1jvA3nLDMDNbJoMPLzTPoqAg8NvZ5CCW",
        ),
    }
}

/// Decode a hard-coded base58 Tron address to its 20-byte EVM form.
///
/// The input is a compile-time-constant published GasFree contract address, so
/// a decode failure is a programming error (a bad constant), not a runtime
/// input fault; the unit tests exercise every network so a bad constant is
/// caught immediately.
fn decode_const_tron_evm(base58: &str) -> EthAddress {
    TronAddress::from_base58(base58)
        .expect("hard-coded GasFree contract address must be valid base58")
        .to_evm_address()
}

/// The GasFree proxy creation bytecode used by the `CREATE2` derivation
/// (§49.5 step 3).
///
// TODO(gasfree): pin official GasFree SDK proxy creation bytecode per network.
// This is the only remaining placeholder of the §49.5 artifacts: until the
// exact SDK bytes are embedded, the derived custody address will not match the
// published per-network vectors. The derivation [`derive_gasfree_address`]
// already takes the bytecode through [`GasFreeArtifacts::creation_bytecode`], so
// swapping in the real bytes here is the only change required.
const GASFREE_PROXY_CREATION_BYTECODE: &[u8] = &[];

/// Published GasFree contract artifacts for `network` (§49.5).
///
/// The controller and beacon are the real published per-network addresses. The
/// `creation_bytecode` is still a placeholder (see
/// [`GASFREE_PROXY_CREATION_BYTECODE`]); pin the official SDK bytes for the
/// derivation to reproduce the published address vectors.
pub fn network_artifacts(network: &Network) -> GasFreeArtifacts {
    let (controller_b58, beacon_b58) = controller_beacon_base58(network);
    GasFreeArtifacts {
        controller: decode_const_tron_evm(controller_b58),
        beacon: decode_const_tron_evm(beacon_b58),
        creation_bytecode: GASFREE_PROXY_CREATION_BYTECODE,
    }
}

/// The activation-time GasFree provider binding (§49.3.1). Valid only on a Tron
/// chain (R1).
#[derive(Clone)]
pub struct TronGaslessProviderConfig {
    /// Host-only provider URL (no path, R13). The network segment + `api/v1`
    /// prefix are appended by the REST client (§49.4.1).
    pub base_url: String,
    /// Provider API key — credential, never logged/serialized (R9).
    pub api_key: String,
    /// Provider API secret — credential, never logged/serialized (R9).
    pub api_secret: String,
    /// The provider's Tron address, bound into the signed authorization
    /// (§49.6) and validated as parseable at activation.
    pub service_provider: TronAddress,
    /// Per-call HTTP timeout (ms).
    pub request_timeout_ms: Option<u64>,
    /// Settlement poll interval (ms); reserved for the deferred submission flow
    /// (D-submit).
    pub status_poll_interval_ms: Option<u64>,
}

/// `Debug` redacts both credentials (R9).
impl fmt::Debug for TronGaslessProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TronGaslessProviderConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("api_secret", &"<redacted>")
            .field("service_provider", &self.service_provider)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("status_poll_interval_ms", &self.status_poll_interval_ms)
            .finish()
    }
}

/// Raw activation payload for `tron_gasless_provider` (§49.3.1), before
/// validation.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TronGaslessProviderRequest {
    pub base_url: String,
    pub api_key: String,
    pub api_secret: String,
    pub service_provider: String,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub status_poll_interval_ms: Option<u64>,
}

impl TronGaslessProviderConfig {
    /// Validate and build a provider binding from its activation payload. Only
    /// valid on a Tron chain (R1); a path-bearing `base_url` is rejected (R13);
    /// `service_provider` must parse as a Tron address.
    pub fn from_request(req: TronGaslessProviderRequest, is_tron: bool) -> Result<Self, GasFreeConfigError> {
        if !is_tron {
            return Err(GasFreeConfigError::NotTron);
        }
        let base_url = validate_host_only_base_url(&req.base_url)?;
        let service_provider = TronAddress::from_base58(req.service_provider.trim())
            .or_else(|_| TronAddress::from_hex(req.service_provider.trim()))
            .map_err(|e| GasFreeConfigError::InvalidServiceProvider(e.to_string()))?;
        Ok(TronGaslessProviderConfig {
            base_url,
            api_key: req.api_key,
            api_secret: req.api_secret,
            service_provider,
            request_timeout_ms: req.request_timeout_ms,
            status_poll_interval_ms: req.status_poll_interval_ms,
        })
    }

    /// Resolve the full REST base (host + network segment), without the
    /// `api/v1` prefix or the endpoint path. E.g. `https://host/tron`.
    pub fn resolved_network_base(&self, network: &Network) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            network_path_segment(network)
        )
    }
}

/// The per-token gasless block (§49.3.2). Valid only on a Tron chain and only
/// when the platform has a provider (R1).
#[derive(Clone, Debug, PartialEq)]
pub struct GaslessTokenConfig {
    /// Opt the token into the gasless rail.
    pub enabled: bool,
    /// Activation-time cap (token units) on the accepted provider fee.
    pub transfer_max_fee: Option<BigDecimal>,
}

/// Raw activation payload for a token `gasless` block (§49.3.2).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaslessTokenRequest {
    pub enabled: bool,
    #[serde(default)]
    pub transfer_max_fee: Option<BigDecimal>,
}

impl GaslessTokenConfig {
    /// Validate and build the per-token gasless config. Rejected on a non-Tron
    /// chain or when the platform has no provider (R1); `transfer_max_fee` must
    /// be non-negative.
    pub fn from_request(
        req: GaslessTokenRequest,
        is_tron: bool,
        platform_has_provider: bool,
    ) -> Result<Self, GasFreeConfigError> {
        if !is_tron {
            return Err(GasFreeConfigError::NotTron);
        }
        if !platform_has_provider {
            return Err(GasFreeConfigError::MissingProvider);
        }
        if let Some(cap) = &req.transfer_max_fee {
            if cap < &BigDecimal::from(0) {
                return Err(GasFreeConfigError::NegativeFeeCap);
            }
        }
        Ok(GaslessTokenConfig {
            enabled: req.enabled,
            transfer_max_fee: req.transfer_max_fee,
        })
    }
}

/// Reject a path-bearing `base_url` (R13). Accepts only a `scheme://host[:port]`
/// origin; returns the normalized origin (trailing slash stripped).
fn validate_host_only_base_url(raw: &str) -> Result<String, GasFreeConfigError> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed).map_err(|e| GasFreeConfigError::InvalidBaseUrl(e.to_string()))?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(GasFreeConfigError::InvalidBaseUrl(format!(
            "unsupported scheme '{}'",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(GasFreeConfigError::InvalidBaseUrl("missing host".to_owned()));
    }
    // Host-only: path must be empty or a bare "/", and no query/fragment.
    if (parsed.path() != "" && parsed.path() != "/") || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(GasFreeConfigError::PathInBaseUrl);
    }

    Ok(trimmed.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A well-known mainnet TRC20 contract used as a parseable Tron address.
    const PROVIDER_ADDR: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

    fn provider_req() -> TronGaslessProviderRequest {
        TronGaslessProviderRequest {
            base_url: "https://open.gasfree.io".to_owned(),
            api_key: "K3Y_V4LUE".to_owned(),
            api_secret: "S3CR3T_V4LUE".to_owned(),
            service_provider: PROVIDER_ADDR.to_owned(),
            request_timeout_ms: None,
            status_poll_interval_ms: None,
        }
    }

    #[test]
    fn host_only_base_url_accepted() {
        let cfg = TronGaslessProviderConfig::from_request(provider_req(), true).unwrap();
        assert_eq!(cfg.base_url, "https://open.gasfree.io");
    }

    #[test]
    fn trailing_slash_base_url_normalized() {
        let mut req = provider_req();
        req.base_url = "https://open.gasfree.io/".to_owned();
        let cfg = TronGaslessProviderConfig::from_request(req, true).unwrap();
        assert_eq!(cfg.base_url, "https://open.gasfree.io");
    }

    #[test]
    fn path_bearing_base_url_rejected() {
        let mut req = provider_req();
        req.base_url = "https://open.gasfree.io/tron".to_owned();
        let err = TronGaslessProviderConfig::from_request(req, true).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::PathInBaseUrl));
    }

    #[test]
    fn query_bearing_base_url_rejected() {
        let mut req = provider_req();
        req.base_url = "https://open.gasfree.io/?x=1".to_owned();
        let err = TronGaslessProviderConfig::from_request(req, true).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::PathInBaseUrl));
    }

    #[test]
    fn provider_rejected_on_non_tron() {
        let err = TronGaslessProviderConfig::from_request(provider_req(), false).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::NotTron));
    }

    #[test]
    fn invalid_service_provider_rejected() {
        let mut req = provider_req();
        req.service_provider = "not-an-address".to_owned();
        let err = TronGaslessProviderConfig::from_request(req, true).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::InvalidServiceProvider(_)));
    }

    #[test]
    fn resolves_network_base_per_network() {
        let cfg = TronGaslessProviderConfig::from_request(provider_req(), true).unwrap();
        assert_eq!(
            cfg.resolved_network_base(&Network::Mainnet),
            "https://open.gasfree.io/tron"
        );
        assert_eq!(
            cfg.resolved_network_base(&Network::Nile),
            "https://open.gasfree.io/nile"
        );
        assert_eq!(
            cfg.resolved_network_base(&Network::Shasta),
            "https://open.gasfree.io/shasta"
        );
    }

    #[test]
    fn token_gasless_requires_provider() {
        let req = GaslessTokenRequest {
            enabled: true,
            transfer_max_fee: None,
        };
        let err = GaslessTokenConfig::from_request(req.clone(), true, false).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::MissingProvider));
        let ok = GaslessTokenConfig::from_request(req, true, true).unwrap();
        assert!(ok.enabled);
    }

    #[test]
    fn token_gasless_rejected_on_non_tron() {
        let req = GaslessTokenRequest {
            enabled: true,
            transfer_max_fee: None,
        };
        let err = GaslessTokenConfig::from_request(req, false, true).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::NotTron));
    }

    #[test]
    fn token_gasless_rejects_negative_cap() {
        let req = GaslessTokenRequest {
            enabled: true,
            transfer_max_fee: Some(BigDecimal::from(-1)),
        };
        let err = GaslessTokenConfig::from_request(req, true, true).unwrap_err();
        assert!(matches!(err, GasFreeConfigError::NegativeFeeCap));
    }

    #[test]
    fn credentials_redacted_in_debug() {
        let cfg = TronGaslessProviderConfig::from_request(provider_req(), true).unwrap();
        let dbg = format!("{:?}", cfg);
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("S3CR3T_V4LUE"), "api_secret leaked: {dbg}");
        assert!(!dbg.contains("K3Y_V4LUE"), "api_key leaked: {dbg}");
    }

    #[test]
    fn tip712_chain_id_nile_is_published_value() {
        assert_eq!(tip712_chain_id(&Network::Nile), 3_448_148_188);
    }

    #[test]
    fn tip712_chain_ids_are_dictated_values() {
        assert_eq!(tip712_chain_id(&Network::Mainnet), 728_126_428);
        assert_eq!(tip712_chain_id(&Network::Shasta), 2_494_104_990);
        assert_eq!(tip712_chain_id(&Network::Nile), 3_448_148_188);
    }

    #[test]
    fn network_artifacts_bind_real_controller_and_beacon() {
        // Every network's published controller/beacon must decode to a non-zero
        // EVM address and round-trip back to the published base58 string (R2).
        for network in [Network::Mainnet, Network::Nile, Network::Shasta] {
            let (controller_b58, beacon_b58) = controller_beacon_base58(&network);
            let artifacts = network_artifacts(&network);

            assert_ne!(
                artifacts.controller,
                EthAddress::zero(),
                "controller is zero for {network:?}"
            );
            assert_ne!(artifacts.beacon, EthAddress::zero(), "beacon is zero for {network:?}");
            assert_ne!(artifacts.controller, artifacts.beacon);

            assert_eq!(
                TronAddress::from_evm_address(artifacts.controller).to_base58(),
                controller_b58
            );
            assert_eq!(TronAddress::from_evm_address(artifacts.beacon).to_base58(), beacon_b58);
        }
    }

    #[test]
    fn network_artifacts_differ_across_networks() {
        let mainnet = network_artifacts(&Network::Mainnet);
        let nile = network_artifacts(&Network::Nile);
        let shasta = network_artifacts(&Network::Shasta);
        assert_ne!(mainnet.controller, nile.controller);
        assert_ne!(mainnet.controller, shasta.controller);
        assert_ne!(nile.controller, shasta.controller);
    }
}
