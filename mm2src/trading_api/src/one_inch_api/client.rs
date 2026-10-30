use super::errors::ApiClientError;
use crate::one_inch_api::errors::NativeError;
use common::{log, StatusCode};
#[cfg(feature = "test-ext-api")] use lazy_static::lazy_static;
use mm2_core::mm_ctx::MmArc;
// crd:pin-begin
use mm2_err_handle::{map_mm_error::MapMmError,
                     map_to_mm::MapToMmResult,
                     mm_error::{MmError, MmResult}};
// crd:pin-end
use mm2_net::host_label::host_only;
use mm2_net::transport::slurp_url_with_headers;
use serde::de::DeserializeOwned;
use url::Url;

#[cfg(feature = "test-ext-api")] use common::executor::Timer;

#[cfg(feature = "test-ext-api")]
use futures::lock::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

#[cfg(any(test, feature = "for-tests"))]
use mocktopus::macros::*;

/// 1inch v6.0 aggregation-router contract address (published provider constant).
const AGGREGATION_ROUTER_V6_0: &str = "0x111111125421ca6dc452d289314280a0f8842a65"; // crd:pin
/// Sentinel address the provider uses for a chain's native coin in a token slot.
const NATIVE_ASSET_SENTINEL: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"; // crd:pin

#[cfg(test)]
const ONE_INCH_API_TEST_URL: &str = "https://api.1inch.dev"; // crd:pin

#[cfg(feature = "test-ext-api")]
lazy_static! {
    /// Bearer token attached to test-build requests, read from the environment.
    static ref ONE_INCH_API_TEST_AUTH: String = std::env::var("ONE_INCH_API_TEST_AUTH").unwrap_or_default();
}

pub(crate) type QueryParams = Vec<(&'static str, String)>;

/// `(ecosystem, chain id)` pairs that carry a 1inch v6.0 classic-swap
/// deployment. A request targeting any chain id outside this set is rejected
/// before a URL is ever composed.
const SUPPORTED_CHAINS: &[(&str, u64)] = &[
    // crd:pin-begin
    ("Ethereum", 1),
    ("Optimism", 10),
    ("BSC", 56),
    ("Gnosis", 100),
    ("Polygon", 137),
    ("Fantom", 250),
    ("ZkSync", 324),
    ("Klaytn", 8217),
    ("Base", 8453),
    ("Arbitrum", 42161),
    ("Avalanche", 43114),
    ("Aurora", 1313161554),
    // crd:pin-end
];

/// Generic builder for a 1inch request URL. The provider-specific builders
/// below seed it with the right endpoint prefix and method token; callers then
/// attach query parameters and finalise it with [`UrlBuilder::build`].
pub struct UrlBuilder {
    base: Url,
    prefix: &'static str,
    chain: Option<u64>,
    method: String,
    query: QueryParams,
}

impl UrlBuilder {
    /// Seed a builder from its component parts. Private because only the
    /// provider-specific wrappers below construct one. The classic-swap
    /// endpoints place the chain id in the path, whereas the portfolio endpoint
    /// carries it as a query parameter, hence the optional.
    fn new(base: Url, chain: Option<u64>, prefix: &'static str, method: String) -> Self {
        UrlBuilder {
            base,
            prefix,
            chain,
            method,
            query: Vec::new(),
        }
    }

    pub fn with_query_params(mut self, mut more_params: QueryParams) -> Self {
        self.query.append(&mut more_params);
        self
    }

    #[allow(clippy::result_large_err)]
    pub fn build(&self) -> MmResult<Url, ApiClientError> {
        // Assemble the relative path `<prefix>[<chain id>/]<method>` and resolve
        // it against the base in a single join. The chain-id segment is only
        // present on the classic-swap routes; the portfolio route omits it.
        let chain_segment = match self.chain {
            Some(chain_id) => format!("{chain_id}/"),
            None => String::new(),
        };
        let relative = format!("{}{}{}", self.prefix, chain_segment, self.method);
        let endpoint = self.base.join(&relative)?;

        let query = self.query.iter().map(|(name, value)| (*name, value.as_str()));
        Ok(Url::parse_with_params(endpoint.as_str(), query)?)
    }
}

/// Classic-swap endpoints exposed by the 1inch v6.0 API.
pub enum SwapApiMethods {
    ClassicSwapQuote,
    ClassicSwapCreate,
    LiquiditySources,
    Tokens,
}

impl SwapApiMethods {
    fn name(&self) -> &'static str {
        match self {
            // crd:pin-begin
            SwapApiMethods::ClassicSwapQuote => "quote",
            SwapApiMethods::ClassicSwapCreate => "swap",
            SwapApiMethods::LiquiditySources => "liquidity-sources",
            SwapApiMethods::Tokens => "tokens",
            // crd:pin-end
        }
    }
}

/// Builds URLs against the 1inch classic-swap endpoint group.
pub struct SwapUrlBuilder;

impl SwapUrlBuilder {
    #[allow(clippy::result_large_err)]
    // crd:pin-begin
    pub fn create_api_url_builder(
        ctx: &MmArc,
        chain_id: u64,
        method: SwapApiMethods,
    ) -> MmResult<UrlBuilder, ApiClientError> {
        // crd:pin-end
        Ok(UrlBuilder::new(
            ApiClient::base_url(ctx)?,
            Some(chain_id),
            "swap/v6.0/", // crd:pin
            method.name().to_owned(),
        ))
    }
}

/// Portfolio price-history endpoints exposed by the 1inch API.
pub enum PortfolioApiMethods {
    CrossPrices,
}

impl PortfolioApiMethods {
    fn name(&self) -> &'static str {
        match self {
            PortfolioApiMethods::CrossPrices => "time_range/cross_prices", // crd:pin
        }
    }
}

/// Builds URLs against the 1inch portfolio endpoint group.
pub struct PortfolioUrlBuilder;

impl PortfolioUrlBuilder {
    #[allow(clippy::result_large_err)]
    pub fn create_api_url_builder(ctx: &MmArc, method: PortfolioApiMethods) -> MmResult<UrlBuilder, ApiClientError> {
        Ok(UrlBuilder::new(
            ApiClient::base_url(ctx)?,
            None,
            "portfolio/integrations/prices/v1/", // crd:pin
            method.name().to_owned(),
        ))
    }
}

/// Stateless entry point for 1inch API calls.
pub struct ApiClient;

#[allow(clippy::swap_ptr_to_ref)] // need for mocktopus
#[cfg_attr(any(test, feature = "for-tests"), mockable)]
impl ApiClient {
    #[allow(unused_variables)]
    #[allow(clippy::result_large_err)]
    fn base_url(ctx: &MmArc) -> MmResult<Url, ApiClientError> {
        // Never compiled in: production reads it from the daemon config, while
        // unit tests target the public dev host. A failed parse lands in
        // `InvalidParam` through the `from_stringify` derive.
        #[cfg(not(test))]
        let raw_url = ctx.conf["1inch_api"] // crd:pin
            .as_str()
            .ok_or_else(|| ApiClientError::InvalidParam("No API config param".to_owned()))?;

        #[cfg(test)]
        let raw_url = ONE_INCH_API_TEST_URL; // crd:pin

        Ok(Url::parse(raw_url)?)
    }

    pub const fn eth_special_contract() -> &'static str { NATIVE_ASSET_SENTINEL }

    pub const fn classic_swap_contract() -> &'static str { AGGREGATION_ROUTER_V6_0 }

    pub fn is_chain_supported(chain_id: u64) -> bool { SUPPORTED_CHAINS.iter().any(|(_, id)| *id == chain_id) }

    fn get_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            // crd:pin-begin
            #[cfg(feature = "test-ext-api")]
            ("Authorization", ONE_INCH_API_TEST_AUTH.as_str()),
            ("accept", "application/json"),
            ("content-type", "application/json"),
            // crd:pin-end
        ]
    }

    // `ApiClientError` is a large error enum; boxing it here would diverge from
    // the rest of this crate, which returns it by value behind the same allow
    // (see `build` above and the `classic_swap_types` constructors), and would
    // ripple through all 14 `MmResult<_, ApiClientError>` call sites for no gain.
    #[allow(clippy::result_large_err)]
    pub async fn call_api<T>(api_url: Url) -> MmResult<T, ApiClientError>
    where
        T: DeserializeOwned, // crd:pin
    {
        // On test builds serialise calls behind the rate-limit gate; the guard
        // is held until this function returns.
        #[cfg(feature = "test-ext-api")]
        let _rate_guard = ApiClient::one_req_per_sec().await;

        log::debug!("Dispatching 1inch request to {}", host_only(api_url.as_str()));
        let (status_code, _headers, raw_body) = slurp_url_with_headers(api_url.as_str(), ApiClient::get_headers())
            .await
            .mm_err(ApiClientError::TransportError)?;
        log::debug!(
            "1inch responded {status_code} with body {}",
            String::from_utf8_lossy(&raw_body)
        );

        // Success and error envelopes share the same wire framing but decode
        // into different Rust shapes, so parse once into a generic value and
        // branch on the status before the typed decode.
        // TODO: handle text body errors like 'The limit of requests per second has been exceeded'
        let body: serde_json::Value = serde_json::from_slice(&raw_body).map_to_mm(body_parse_error)?;

        // An OK status carries the success-shaped payload; anything else is a
        // provider error envelope that is surfaced instead of decoded as `T`.
        if status_code == StatusCode::OK {
            serde_json::from_value(body).map_to_mm(body_parse_error)
        } else {
            let native_error = NativeError::new(status_code, body);
            Err(MmError::new(ApiClientError::from_native_error(native_error)))
        }
    }

    /// Gate test-tier calls to at most one per second. The returned guard keeps
    /// callers mutually exclusive; the wait itself only covers the unspent
    /// remainder of the previous call's one-second window.
    #[cfg(feature = "test-ext-api")]
    async fn one_req_per_sec<'a>() -> AsyncMutexGuard<'a, ()> {
        use std::time::Instant;
        lazy_static! {
            /// Mutual-exclusion point shared by every test call.
            static ref ONE_INCH_REQ_SYNC: AsyncMutex<()> = AsyncMutex::new(());
            /// Dispatch instant of the previous test call, used to size the wait.
            static ref ONE_INCH_LAST_CALL: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        }
        let guard = ONE_INCH_REQ_SYNC.lock().await;
        // Size the wait against the previous *dispatch* instant. Read it without
        // holding the lock across the sleep.
        let pending_wait = {
            let last_call = ONE_INCH_LAST_CALL.lock().unwrap();
            last_call.map(|prev| 1. - prev.elapsed().as_secs_f64()).unwrap_or(0.)
        };
        if pending_wait > 0. {
            Timer::sleep(pending_wait).await;
        }
        // Record the instant the request is actually dispatched (after the wait),
        // so the next call measures its window from the real send time rather
        // than from this call's lock-acquisition.
        *ONE_INCH_LAST_CALL.lock().unwrap() = Some(Instant::now());
        guard
    }
}

/// Renders a body-decode failure as the body-parse error variant.
fn body_parse_error(err: serde_json::Error) -> ApiClientError {
    ApiClientError::ParseBodyError {
        error_msg: err.to_string(),
    }
}
