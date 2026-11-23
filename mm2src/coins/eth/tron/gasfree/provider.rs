//! The client of the GasFree provider's REST API (CRD ch.63 §63.4.1 to §63.4.3, R9 to R11, R13, R20).
//!
//! A request goes to `<base>/<network segment>/api/v1/<operation>`, where `<base>` is the configured scheme and
//! host (R13). It carries the two authentication headers of §63.4.2 and is bounded as a whole by the per-call
//! timeout (R20). The payloads are in [`super::provider_payloads`].
//!
//! # Invariants
//!
//! - Every call is one request: nothing here repeats a request after a failed connection, a timeout or any answer
//!   (§63.8B R31).
//! - The credentials go into the request headers and nowhere else: no `Debug` rendering and no error of this
//!   module carries them (R9).
//! - Provider text that reaches an error is collapsed and bounded first (R10).

use super::super::{Network, TronAddress};
use super::authorization::PERMIT_VERSION;
use super::failure::{sanitize_provider_text, GasFreeError};
use super::networks::gasfree_network;
use super::provider_payloads::{ProviderAccount, SubmitRequest, SupportedToken, SupportedTokenList, TransferRecord};
use common::now_ms;
use hmac::{Hmac, Mac};
use http::header::{HeaderName, AUTHORIZATION};
use http::{HeaderMap, HeaderValue};
use mm2_err_handle::prelude::MmError;
use mm2_net::transport::{slurp_post_json_with_headers_bounded, slurp_url_with_headers_bounded, SlurpError};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value as Json;
use sha2_010::Sha256;
use std::fmt;
use url::Url;
use uuid::Uuid;

/// The per-call timeout of a provider request when the activation gives none, in milliseconds (R20).
pub(crate) const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 15_000;

/// The settlement poll interval when the activation gives none, in milliseconds (R20).
pub(crate) const DEFAULT_STATUS_POLL_INTERVAL_MS: u64 = 3_000;

/// The prefix every operation path shares, after the network segment (§63.4.1).
const API_PREFIX: &str = "api/v1";

/// The business-success value of the envelope's `code` (§63.4.3).
const ENVELOPE_SUCCESS: i64 = 200;

/// The provider's API key and secret (§63.3.1). Secrets: `Debug` shows neither.
#[derive(Clone)]
pub(crate) struct ProviderCredentials {
    api_key: String,
    api_secret: String,
}

impl ProviderCredentials {
    pub(crate) fn new(api_key: String, api_secret: String) -> Self { ProviderCredentials { api_key, api_secret } }
}

impl fmt::Debug for ProviderCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProviderCredentials {{ api_key: <set>, api_secret: <set> }}")
    }
}

/// The value of the `Authorization` header of a request (§63.4.2): `ApiKey {api_key}:{signature}`, the signature
/// being the Base64 of the HMAC-SHA256, keyed by the API secret, of `method`, `path` and `timestamp_s` joined
/// without separators. `path` is the whole request path, the network segment included.
///
/// # Errors
///
/// [`GasFreeError::InvalidCredentials`] when the secret cannot key the HMAC.
pub(crate) fn authorization_value(
    credentials: &ProviderCredentials,
    method: &str,
    path: &str,
    timestamp_s: u64,
) -> Result<String, GasFreeError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(credentials.api_secret.as_bytes())
        .map_err(|_| GasFreeError::InvalidCredentials)?;
    mac.update(method.as_bytes());
    mac.update(path.as_bytes());
    mac.update(timestamp_s.to_string().as_bytes());
    let signature = base64::encode(mac.finalize().into_bytes().as_slice());
    Ok(format!("ApiKey {}:{}", credentials.api_key, signature))
}

/// The two authentication headers of a request (§63.4.2).
fn auth_headers(
    credentials: &ProviderCredentials,
    method: &str,
    path: &str,
    timestamp_s: u64,
) -> Result<HeaderMap, GasFreeError> {
    let mut authorization = HeaderValue::from_str(&authorization_value(credentials, method, path, timestamp_s)?)
        .map_err(|_| GasFreeError::InvalidCredentials)?;
    authorization.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(HeaderName::from_static("timestamp"), HeaderValue::from(timestamp_s));
    headers.insert(AUTHORIZATION, authorization);
    Ok(headers)
}

/// The scheme and host of a configured provider URL, as `scheme://host[:port]` (R13).
///
/// # Errors
///
/// [`GasFreeError::InvalidBaseUrl`], with a text that never repeats the URL, when it is not an `http` or `https`
/// URL with a host, or carries userinfo, a path, a query or a fragment.
fn host_only_base(base_url: &str) -> Result<String, GasFreeError> {
    let url = Url::parse(base_url).map_err(|_| GasFreeError::InvalidBaseUrl("it is not a URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(GasFreeError::InvalidBaseUrl("the scheme is neither http nor https"));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(GasFreeError::InvalidBaseUrl("it has no host"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GasFreeError::InvalidBaseUrl("it carries userinfo"));
    }
    if !matches!(url.path(), "" | "/") {
        return Err(GasFreeError::InvalidBaseUrl(
            "it carries a path; the network segment and the API prefix are added by the node",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(GasFreeError::InvalidBaseUrl("it carries a query or a fragment"));
    }
    Ok(match url.port() {
        Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
        None => format!("{}://{}", url.scheme(), host),
    })
}

/// A client of one GasFree provider on one network.
#[derive(Clone, Debug)]
pub(crate) struct GasFreeProvider {
    /// `scheme://host[:port]`, without a trailing slash.
    base: String,
    /// The network segment every request path starts with (§63.4.1).
    path_segment: &'static str,
    credentials: ProviderCredentials,
    /// The bound of one call, in milliseconds (R20).
    request_timeout_ms: u64,
}

impl GasFreeProvider {
    /// A client for the provider at `base_url` on `network`. `request_timeout_ms` bounds each call and defaults to
    /// [`DEFAULT_REQUEST_TIMEOUT_MS`].
    ///
    /// # Errors
    ///
    /// [`GasFreeError::InvalidBaseUrl`] when `base_url` is more than a scheme and a host (R13).
    pub(crate) fn new(
        network: &Network,
        base_url: &str,
        credentials: ProviderCredentials,
        request_timeout_ms: Option<u64>,
    ) -> Result<Self, GasFreeError> {
        Ok(GasFreeProvider {
            base: host_only_base(base_url)?,
            path_segment: gasfree_network(network).path_segment,
            credentials,
            // An absent setting means the documented default (R20).
            request_timeout_ms: request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        })
    }

    /// The bound of one call, in milliseconds (R20).
    pub(crate) fn request_timeout_ms(&self) -> u64 { self.request_timeout_ms }

    /// The request path of `operation`: `/<network segment>/api/v1/<operation>` (§63.4.1). This is the path the
    /// request is sent to and the path its authentication signs.
    fn request_path(&self, operation: &str) -> String { format!("/{}/{}/{}", self.path_segment, API_PREFIX, operation) }

    /// The provider's record of `account` (§63.4.4).
    ///
    /// # Errors
    ///
    /// The [`GasFreeError`] of the failed exchange, see [`decode_answer`].
    pub(crate) async fn account_info(&self, account: &TronAddress) -> Result<ProviderAccount, GasFreeError> {
        self.get(&format!("address/{}", account.to_base58())).await
    }

    /// The tokens the provider supports, with its fees (§63.4.5).
    ///
    /// # Errors
    ///
    /// The [`GasFreeError`] of the failed exchange, see [`decode_answer`].
    pub(crate) async fn supported_tokens(&self) -> Result<Vec<SupportedToken>, GasFreeError> {
        let list: SupportedTokenList = self.get("config/token/all").await?;
        Ok(list.tokens)
    }

    /// Submits a signed authorization (§63.4.6) and returns the provider's record of the transfer (§63.4.7). One
    /// request, never repeated (§63.8B R31).
    ///
    /// # Errors
    ///
    /// The [`GasFreeError`] of the failed exchange, see [`decode_answer`]; a 404 is [`GasFreeError::Rejected`]
    /// here. [`GasFreeError::Internal`], with no request made, for a body that cannot be written or whose
    /// `version` is not 1 (R5).
    pub(crate) async fn submit(&self, request: &SubmitRequest) -> Result<TransferRecord, GasFreeError> {
        // The text of a serialization error is not repeated: the body holds the signature (R9).
        let body = serde_json::to_value(request)
            .map_err(|_| GasFreeError::Internal("the submit payload cannot be written as JSON".to_owned()))?;
        self.post_submit(&body).await
    }

    /// Sends `body` as the submit request, unless its `version` is not 1 (R5, §63.8B R31).
    async fn post_submit(&self, body: &Json) -> Result<TransferRecord, GasFreeError> {
        if body["version"].as_str() != Some(PERMIT_VERSION.to_string().as_str()) {
            return Err(GasFreeError::Internal(
                "the submit payload does not carry the authorization version 1".to_owned(),
            ));
        }
        self.post("gasfree/submit", body.to_string()).await
    }

    /// The provider's record of the transfer with the trace id `trace_id` (§63.4.7). One request, never repeated
    /// (§63.8B R31). The id goes into the request path as it is given.
    ///
    /// # Errors
    ///
    /// - [`GasFreeError::Internal`], with no request made, when `trace_id` does not read as a UUID.
    /// - [`GasFreeError::TraceNotFound`] for a 404, as the HTTP status or as the envelope code (D63.9 item 5).
    /// - Otherwise the [`GasFreeError`] of the failed exchange, see [`decode_answer`].
    pub(crate) async fn trace(&self, trace_id: &str) -> Result<TransferRecord, GasFreeError> {
        if Uuid::parse_str(trace_id).is_err() {
            return Err(GasFreeError::Internal("the trace id is not a UUID".to_owned()));
        }
        match self.get(&format!("gasfree/{trace_id}")).await {
            Err(GasFreeError::Rejected { status: 404, message }) => Err(GasFreeError::TraceNotFound(message)),
            answer => answer,
        }
    }

    /// One authenticated POST of the JSON text `body`, bounded by the per-call timeout, decoded as the `data` of a
    /// success envelope.
    async fn post<T: DeserializeOwned>(&self, operation: &str, body: String) -> Result<T, GasFreeError> {
        let path = self.request_path(operation);
        let url = format!("{}{}", self.base, path);
        let headers = auth_headers(&self.credentials, "POST", &path, now_ms() / 1000)?;
        let timeout_s = self.request_timeout_ms as f64 / 1000.;
        let (status, _headers, body) = slurp_post_json_with_headers_bounded(&url, body, &headers, timeout_s)
            .await
            .map_err(transport_failure)?;
        decode_answer(status.as_u16(), &body)
    }

    /// One authenticated GET, bounded by the per-call timeout, decoded as the `data` of a success envelope.
    async fn get<T: DeserializeOwned>(&self, operation: &str) -> Result<T, GasFreeError> {
        let path = self.request_path(operation);
        let url = format!("{}{}", self.base, path);
        let headers = auth_headers(&self.credentials, "GET", &path, now_ms() / 1000)?;
        let timeout_s = self.request_timeout_ms as f64 / 1000.;
        let (status, _headers, body) = slurp_url_with_headers_bounded(&url, &headers, timeout_s)
            .await
            .map_err(transport_failure)?;
        decode_answer(status.as_u16(), &body)
    }
}

/// The failure of an exchange that got no answer. The transport's texts name the host only and no header value.
fn transport_failure(error: MmError<SlurpError>) -> GasFreeError {
    match error.into_inner() {
        SlurpError::Timeout { error, .. } => GasFreeError::Timeout(error),
        SlurpError::Transport { error, .. } => GasFreeError::Transport(error),
        SlurpError::ErrorDeserializing { error, .. } => GasFreeError::InvalidResponse(sanitize_provider_text(&error)),
        SlurpError::InvalidRequest(error) | SlurpError::Internal(error) => GasFreeError::Internal(error),
    }
}

/// The provider's response envelope (§63.4.3).
#[derive(Deserialize)]
struct Envelope {
    code: i64,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Json>,
}

impl Envelope {
    /// The provider's own words about the answer, made fit for an error (R10).
    fn text(&self) -> String {
        let parts: Vec<&str> = [self.reason.as_deref(), self.message.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        sanitize_provider_text(&parts.join(": "))
    }

    /// The `code` when it is a status-like value, 100 to 599 (§63.9.3).
    fn status_like_code(&self) -> Option<u16> {
        u16::try_from(self.code).ok().filter(|code| (100..=599).contains(code))
    }
}

/// The failure a status, or a status-like envelope code, stands for (§63.9.3). `text` is sanitized provider text.
fn status_failure(status: u16, text: String) -> GasFreeError {
    match status {
        401 | 403 => GasFreeError::Unauthorized { status },
        429 => GasFreeError::RateLimited,
        400..=499 => GasFreeError::Rejected { status, message: text },
        500..=599 => GasFreeError::ProviderFailed { status, message: text },
        _ => GasFreeError::InvalidResponse(format!("the provider answered with status {status}")),
    }
}

/// Decodes one provider answer as the `data` of a success envelope (§63.4.3, §63.9.3, R11).
///
/// # Errors
///
/// - An envelope whose `code` is a status-like value other than 200 fails as that status does, whatever the HTTP
///   status was; otherwise an HTTP status outside 2xx fails as itself: [`GasFreeError::Unauthorized`] (401, 403),
///   [`GasFreeError::RateLimited`] (429), [`GasFreeError::Rejected`] (another 4xx),
///   [`GasFreeError::ProviderFailed`] (5xx).
/// - [`GasFreeError::InvalidResponse`] for a 2xx answer that is not a success envelope with a `data` that decodes
///   as `T`, and for a status that is neither 2xx, 4xx nor 5xx.
pub(crate) fn decode_answer<T: DeserializeOwned>(http_status: u16, body: &[u8]) -> Result<T, GasFreeError> {
    let envelope: Option<Envelope> = serde_json::from_slice(body).ok();
    if let Some(code) = envelope.as_ref().and_then(Envelope::status_like_code) {
        if i64::from(code) != ENVELOPE_SUCCESS {
            let text = envelope.as_ref().map_or_else(String::new, Envelope::text);
            return Err(status_failure(code, text));
        }
    }
    if !(200..300).contains(&http_status) {
        let text = match &envelope {
            Some(envelope) => envelope.text(),
            None => sanitize_provider_text(&String::from_utf8_lossy(body)),
        };
        return Err(status_failure(http_status, text));
    }
    let envelope =
        envelope.ok_or_else(|| GasFreeError::InvalidResponse("the body is not a response envelope".to_owned()))?;
    if envelope.code != ENVELOPE_SUCCESS {
        return Err(GasFreeError::InvalidResponse(format!(
            "the envelope code {} is neither 200 nor a status",
            envelope.code
        )));
    }
    let data = envelope
        .data
        .ok_or_else(|| GasFreeError::InvalidResponse("a success envelope carries no data".to_owned()))?;
    decode_data(&data)
}

/// Decodes the `data` of a success envelope as `T`.
///
/// # Errors
///
/// [`GasFreeError::InvalidResponse`], whose text starts with the path of the member that does not decode, as
/// `data.createdAt` or `data.assets[].decimal` (`data` alone for the value as a whole, and the object that lacks
/// a member for a missing one). The decoding error follows; like every provider text it is collapsed and bounded
/// (R10).
fn decode_data<T: DeserializeOwned>(data: &Json) -> Result<T, GasFreeError> {
    T::deserialize(data).map_err(|error| {
        let path = failing_member::<T>(data).unwrap_or_else(|| "data".to_owned());
        GasFreeError::InvalidResponse(sanitize_provider_text(&format!("{path}: {error}")))
    })
}

/// The name of the member [`failing_member`] adds at the end of every object. It sorts after every name of the
/// provider's payloads, and the payload types ignore a member they do not name.
const LAST_MEMBER: &str = "~";

/// `value` with [`LAST_MEMBER`] added to every object in it.
fn with_last_members(value: &Json) -> Json {
    match value {
        Json::Object(object) => {
            let mut marked: serde_json::Map<String, Json> = object
                .iter()
                .map(|(name, member)| (name.clone(), with_last_members(member)))
                .collect();
            marked.insert(LAST_MEMBER.to_owned(), Json::Null);
            Json::Object(marked)
        },
        Json::Array(elements) => Json::Array(elements.iter().map(with_last_members).collect()),
        other => other.clone(),
    }
}

/// The path of the member of `data` that does not decode as `T`, for the text of an error. `None` when it cannot
/// be told.
///
/// The decoder of a JSON text reports the line where it fails, so `data` is written with one member per line and
/// decoded again. A failure of the last member of an object would be reported at the end of the object, where a
/// missing member is reported too; [`LAST_MEMBER`] keeps every real member from being the last one.
fn failing_member<T: DeserializeOwned>(data: &Json) -> Option<String> {
    let lines = serde_json::to_string_pretty(&with_last_members(data)).ok()?;
    let error = serde_json::from_str::<T>(&lines).err()?;
    Some(member_path(&lines, error.line()))
}

/// The path of the member that stands on line `line` (counted from 1) of `lines`, a JSON value written with one
/// member per line and nested values indented: `data`, then the names from the outermost object inwards, with
/// `[]` after the name of an array. Only names are taken from the text, never a value. A line without a member
/// name ends a value and gives the path of that value.
///
/// The path is where the decoder of the JSON text stops. For a rule that is checked after a whole nested value
/// was read, that is the end of that value, so the path names the value and not a member inside it.
fn member_path(lines: &str, line: usize) -> String {
    /// The member name a line starts with, and whether its value is an array.
    /// A line is a member line when it starts with a JSON string that `: ` follows directly. The string ends at
    /// its first quote that no backslash escapes, so a string element of an array is never taken for a name,
    /// whatever it contains.
    fn member(text: &str) -> Option<String> {
        let rest = text.trim_start().strip_prefix('"')?;
        let mut escaped = false;
        let end = rest.char_indices().find_map(|(at, character)| match character {
            _ if escaped => {
                escaped = false;
                None
            },
            '\\' => {
                escaped = true;
                None
            },
            '"' => Some(at),
            _ => None,
        })?;
        let value = rest[end + 1..].strip_prefix(": ")?;
        let array = if value.starts_with('[') { "[]" } else { "" };
        Some(format!("{}{}", &rest[..end], array))
    }
    let indent = |text: &str| text.len() - text.trim_start().len();

    let upto: Vec<&str> = lines.lines().take(line).collect();
    let Some((last, above)) = upto.split_last() else {
        return "data".to_owned();
    };
    let mut names = Vec::new();
    // Walking upwards, the next line with less indentation opens the value the current line is in. The end of a
    // value has the indentation of the line that opens it, so the walk starts above it.
    let mut depth = indent(last) + 1;
    let walked = if member(last).is_some() { upto.as_slice() } else { above };
    for text in walked.iter().rev() {
        if indent(text) < depth {
            depth = indent(text);
            if let Some(name) = member(text) {
                names.push(name);
            }
        }
    }
    names.push("data".to_owned());
    names.reverse();
    names.join(".")
}

#[cfg(test)]
mod tests {
    use super::super::failure::{GaslessMember, RpcArm, PROVIDER_TEXT_MAX_CHARS};
    use super::*;

    const API_KEY: &str = "k-test";
    const API_SECRET: &str = "s-test";

    fn credentials(api_key: &str, api_secret: &str) -> ProviderCredentials {
        ProviderCredentials::new(api_key.to_owned(), api_secret.to_owned())
    }

    /// ch.63 T3: the request-auth construction, with the network segment in the signed path.
    #[test]
    fn the_auth_vectors_are_reproduced() {
        let cases = [
            (
                "GET",
                "/nile/api/v1/config/token/all",
                1_731_912_286u64,
                credentials("YOUR_API_KEY", "YOUR_API_SECRET"),
                "ApiKey YOUR_API_KEY:EiO3aMzkZ4g1gVFHSwovaBtZQ/0xQVL13fo1jP42loo=",
            ),
            (
                "GET",
                "/tron/api/v1/address/TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC",
                1_726_207_632,
                credentials(API_KEY, API_SECRET),
                "ApiKey k-test:paSCVyI9tJxy3Xds26kDKg6lwF03g/s7oaO44ghvQRk=",
            ),
            (
                "POST",
                "/nile/api/v1/gasfree/submit",
                1_726_207_632,
                credentials(API_KEY, API_SECRET),
                "ApiKey k-test:aB2HINNMVwFYm7E10SVF+zuej3busTJw78z+rPQYgtA=",
            ),
        ];
        for (index, (method, path, timestamp_s, credentials, expected)) in cases.into_iter().enumerate() {
            let value = authorization_value(&credentials, method, path, timestamp_s);
            assert!(
                matches!(&value, Ok(value) if value == expected),
                "auth vector {index} is not reproduced"
            );
        }
    }

    #[test]
    fn the_auth_headers_are_the_timestamp_and_the_authorization() {
        let credentials = credentials(API_KEY, API_SECRET);
        let path = "/tron/api/v1/address/TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC";
        let headers = auth_headers(&credentials, "GET", path, 1_726_207_632).expect("headers");
        assert_eq!(headers.len(), 2);
        assert!(headers.get("timestamp").is_some_and(|value| value == "1726207632"));
        let expected = authorization_value(&credentials, "GET", path, 1_726_207_632).expect("a value");
        let authorization = headers.get(AUTHORIZATION).expect("an authorization header");
        assert!(
            authorization.is_sensitive(),
            "the authorization header is not marked sensitive"
        );
        assert!(
            authorization.as_bytes() == expected.as_bytes(),
            "the authorization header is not the signed value"
        );
        // A credential that cannot be a header value is refused without being repeated.
        let unsendable = ProviderCredentials::new("line\nbreak".to_owned(), API_SECRET.to_owned());
        let error = auth_headers(&unsendable, "GET", path, 1).expect_err("an unsendable key");
        assert!(matches!(error, GasFreeError::InvalidCredentials));
        assert!(!error.to_string().contains("break"), "a credential is repeated");
    }

    /// ch.63 T3: the credentials are absent from every `Debug` rendering.
    #[test]
    fn debug_shows_no_credential() {
        let credentials = credentials("key-0f3a9c", "secret-7b1d4e");
        let provider = GasFreeProvider::new(&Network::Nile, "https://provider.example", credentials.clone(), None)
            .expect("a host-only URL");
        for rendered in [format!("{credentials:?}"), format!("{provider:?}")] {
            assert!(!rendered.contains("key-0f3a9c"), "the API key is rendered");
            assert!(!rendered.contains("secret-7b1d4e"), "the API secret is rendered");
        }
    }

    /// ch.63 T9 (the part this module holds): the request base, the path rule and the defaults of R20.
    #[test]
    fn the_base_url_is_host_only_and_the_paths_carry_the_segment() {
        let new = |network: &Network, base_url: &str| {
            GasFreeProvider::new(network, base_url, credentials(API_KEY, API_SECRET), None)
        };
        for (network, base_url, base, tokens_path) in [
            (
                Network::Mainnet,
                "https://open.gasfree.io",
                "https://open.gasfree.io",
                "/tron/api/v1/config/token/all",
            ),
            (
                Network::Nile,
                "https://open-test.gasfree.io/",
                "https://open-test.gasfree.io",
                "/nile/api/v1/config/token/all",
            ),
            (
                Network::Shasta,
                "http://127.0.0.1:8080/",
                "http://127.0.0.1:8080",
                "/shasta/api/v1/config/token/all",
            ),
        ] {
            let provider = new(&network, base_url).expect("a host-only URL");
            assert_eq!(provider.base, base);
            assert_eq!(provider.request_path("config/token/all"), tokens_path);
            assert_eq!(provider.request_timeout_ms, 15_000);
        }
        let provider = GasFreeProvider::new(
            &Network::Nile,
            "https://provider.example",
            credentials(API_KEY, API_SECRET),
            Some(2_500),
        )
        .expect("a host-only URL");
        assert_eq!(provider.request_timeout_ms, 2_500);
        assert_eq!(DEFAULT_REQUEST_TIMEOUT_MS, 15_000);
        assert_eq!(DEFAULT_STATUS_POLL_INTERVAL_MS, 3_000);

        for base_url in [
            "https://open.gasfree.io/tron",
            "https://open.gasfree.io/tron/",
            "https://open.gasfree.io//",
            "https://open.gasfree.io/?token=hunter2",
            "https://open.gasfree.io/#hunter2",
            "https://bob:hunter2@open.gasfree.io",
            "ftp://open.gasfree.io",
            "open.gasfree.io",
            "hunter2",
            "",
        ] {
            let error = new(&Network::Mainnet, base_url).expect_err("more or less than a scheme and a host");
            assert!(matches!(error, GasFreeError::InvalidBaseUrl(_)));
            assert_eq!(error.rpc_arm(), RpcArm::InternalError);
            let text = format!("{error} {error:?}");
            assert!(
                !text.contains("hunter2") && !text.contains("gasfree.io"),
                "the URL is repeated"
            );
        }
    }

    fn decode(http_status: u16, body: &str) -> Result<Json, GasFreeError> {
        decode_answer(http_status, body.as_bytes())
    }

    /// ch.63 T4: the success envelope.
    #[test]
    fn only_a_success_envelope_with_data_is_an_answer() {
        let data = decode(200, r#"{"code":200,"reason":null,"message":"","data":{"x":1}}"#).expect("an answer");
        assert_eq!(data["x"], 1);
        let data = decode(200, r#"{"code":200,"data":[]}"#).expect("an answer without the optional members");
        assert_eq!(data, serde_json::json!([]));

        for body in [
            r#"{"code":200,"message":"ok"}"#,
            r#"{"code":200,"message":"ok","data":null}"#,
            r#"{"code":0,"data":{"x":1}}"#,
            r#"{"code":99,"data":{"x":1}}"#,
            r#"{"code":600,"data":{"x":1}}"#,
            r#"{"code":-400,"data":{"x":1}}"#,
            r#"{"code":"200","data":{"x":1}}"#,
            r#"{"data":{"x":1}}"#,
            "<html>not json</html>",
            "",
        ] {
            let error = decode(200, body).expect_err("not a success envelope");
            assert!(matches!(error, GasFreeError::InvalidResponse(_)));
            assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::InvalidProviderResponse));
        }

        // A data of the wrong shape for the call.
        let error = decode_answer::<SupportedTokenList>(200, br#"{"code":200,"data":{"x":1}}"#).expect_err("no list");
        assert!(matches!(error, GasFreeError::InvalidResponse(_)));
    }

    /// §63.4.7, status note of 2026-11-22: a `data` that does not decode names the member, and no value of a
    /// signature member.
    #[test]
    fn a_data_that_does_not_decode_names_the_member() {
        use super::super::provider_payloads::{ProviderAccount, TransferRecord};
        let sig = "ab".repeat(64);
        let transfer = serde_json::json!({
            "id": "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0",
            "createdAt": 1_795_354_473_207u64,
            "accountAddress": "TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC",
            "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "providerAddress": "TDbJyQ6g1Lx9BAfEEeN5S5TMjjDRAVFCaA",
            "targetAddress": "TDvSsdrNM5eeXNL3czpa6AxLDHZA9nwe9K",
            "tokenAddress": "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
            "amount": 10000,
            "nonce": 2,
            "state": "WAITING",
            "estimatedActivateFee": 0,
            "estimatedTransferFee": 2000000
        });
        let failure = |data: &Json| {
            let body = serde_json::json!({ "code": 200, "data": data }).to_string();
            match decode_answer::<TransferRecord>(200, body.as_bytes()) {
                Err(GasFreeError::InvalidResponse(text)) => text,
                _ => panic!("the data must not decode"),
            }
        };
        assert!(decode_data::<TransferRecord>(&transfer).is_ok());

        let with = |member: &str, value: Json| {
            let mut changed = transfer.clone();
            changed[member] = value;
            failure(&changed)
        };
        assert!(with("createdAt", serde_json::json!({ "at": 1 })).starts_with("data.createdAt: "));
        assert!(with("nonce", serde_json::json!("-1")).starts_with("data.nonce: "));
        assert!(with("amount", serde_json::json!(true)).starts_with("data.amount: "));
        assert!(with("state", serde_json::json!("DONE")).starts_with("data.state: "));
        assert!(with("txnHash", serde_json::json!("0x00")).starts_with("data.txnHash: "));
        let text = with("signature", Json::String(sig.clone()));
        assert!(text.starts_with("data.signature: "));
        assert!(!text.contains(&sig), "the signature is repeated");
        let text = with("signature", serde_json::json!([sig]));
        assert!(text.starts_with("data.signature[]: ") && !text.contains(&sig));
        // A missing member is named by the decoder; the path is the object that lacks it.
        let mut without = transfer.clone();
        without.as_object_mut().expect("an object").remove("state");
        let text = failure(&without);
        assert!(text.starts_with("data: ") && text.contains("state"));
        assert!(failure(&serde_json::json!([1])).starts_with("data: "));
        assert!(!with("nonce", serde_json::json!("-1")).contains(" at line "));

        // A member of an array element.
        let account = serde_json::json!({
            "accountAddress": "TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC",
            "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "active": true,
            "nonce": 7,
            "allowSubmit": true,
            "assets": [{
                "tokenAddress": "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
                "tokenSymbol": "USDT",
                "activateFee": 1000000,
                "transferFee": 2000000,
                "decimal": "six",
                "frozen": 0
            }]
        });
        match decode_data::<ProviderAccount>(&account) {
            Err(GasFreeError::InvalidResponse(text)) => assert!(text.starts_with("data.assets[].decimal: ")),
            _ => panic!("the account record must not decode"),
        }
    }

    /// A string element of an array is never read as a member name, also when it holds an escaped quote followed
    /// by a colon and a space; a member name with an escaped quote is read whole.
    #[test]
    fn no_part_of_an_array_element_enters_the_member_path() {
        #[derive(Debug, Deserialize)]
        struct Listing {
            #[allow(dead_code)]
            items: Vec<u64>,
        }
        for element in ["leak\": value", "\": leak", "a\\\": leak", "leak\": [", "plain"] {
            let data = serde_json::json!({ "items": [element, element], "other": 1 });
            match decode_data::<Listing>(&data) {
                Err(GasFreeError::InvalidResponse(text)) => {
                    assert!(text.starts_with("data.items[]: "), "the path is not the array");
                    let path = text.split(": ").next().unwrap_or_default();
                    assert!(!path.contains("leak"), "a part of the value is in the path");
                },
                _ => panic!("the data must not decode"),
            }
            assert_eq!(failing_member::<Listing>(&data).as_deref(), Some("data.items[]"));
        }

        // The lines themselves.
        let lines = "{\n  \"a\\\"b\": [\n    \"x\\\": y\",\n    1\n  ],\n  \"c\": 2\n}";
        assert_eq!(member_path(lines, 3), "data.a\\\"b[]");
        assert_eq!(member_path(lines, 6), "data.c");
        assert_eq!(member_path(lines, 1), "data");
    }

    /// ch.63 T4: a 200 HTTP answer with a non-200 envelope code is rejected, by the class of the code.
    #[test]
    fn a_status_like_envelope_code_takes_the_place_of_the_http_status() {
        let envelope =
            |code: u16| format!(r#"{{"code":{code},"reason":"Reason","message":"a message","data":{{"x":1}}}}"#);
        for http_status in [200u16, 404, 500] {
            for code in [400u16, 404, 409, 422, 499] {
                let error = decode(http_status, &envelope(code)).expect_err("a refusal");
                assert!(matches!(&error, GasFreeError::Rejected { status, message }
                    if *status == code && message == "Reason: a message"));
                assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::ProviderRejected));
            }
            for code in [401u16, 403] {
                let error = decode(http_status, &envelope(code)).expect_err("a refusal");
                assert!(matches!(error, GasFreeError::Unauthorized { status } if status == code));
                assert_eq!(error.rpc_arm(), RpcArm::Transport);
            }
            let error = decode(http_status, &envelope(429)).expect_err("a rate limit");
            assert!(matches!(error, GasFreeError::RateLimited));
            assert_eq!(error.rpc_arm(), RpcArm::Transport);
            for code in [500u16, 503, 599] {
                let error = decode(http_status, &envelope(code)).expect_err("a provider failure");
                assert!(matches!(error, GasFreeError::ProviderFailed { status, .. } if status == code));
                assert_eq!(error.rpc_arm(), RpcArm::Transport);
            }
            // A status-like code that is neither a refusal nor a failure nor the success value.
            for code in [100u16, 201, 302] {
                let error = decode(http_status, &envelope(code)).expect_err("not an answer");
                assert!(matches!(error, GasFreeError::InvalidResponse(_)));
            }
        }
    }

    /// ch.63 T4: the HTTP status classes, for a body that is no envelope or whose code is not status-like.
    #[test]
    fn an_http_status_outside_2xx_fails_by_its_class() {
        for body in [
            "Too Many Requests",
            r#"{"code":0,"message":"m"}"#,
            r#"{"code":200,"data":{"x":1}}"#,
            "",
        ] {
            for (http_status, arm) in [
                (400u16, RpcArm::Gasless(GaslessMember::ProviderRejected)),
                (404, RpcArm::Gasless(GaslessMember::ProviderRejected)),
                (401, RpcArm::Transport),
                (403, RpcArm::Transport),
                (429, RpcArm::Transport),
                (500, RpcArm::Transport),
                (503, RpcArm::Transport),
                (302, RpcArm::Gasless(GaslessMember::InvalidProviderResponse)),
            ] {
                let error = decode(http_status, body).expect_err("a status outside 2xx");
                assert_eq!(error.rpc_arm(), arm);
            }
        }
        let error = decode(400, "plain  text\nof the\tprovider").expect_err("a refusal");
        assert!(
            matches!(&error, GasFreeError::Rejected { status: 400, message } if message == "plain text of the provider")
        );
    }

    /// ch.63 T4: provider text is collapsed and bounded to 256 characters.
    #[test]
    fn provider_text_in_an_error_is_collapsed_and_bounded() {
        let long = "word \n\t ".repeat(200);
        let body = serde_json::json!({ "code": 400, "reason": " Bad\tRequest ", "message": long }).to_string();
        match decode(200, &body).expect_err("a refusal") {
            GasFreeError::Rejected { message, .. } => {
                assert!(message.starts_with("Bad Request : word word"));
                assert_eq!(message.chars().count(), PROVIDER_TEXT_MAX_CHARS);
                assert!(!message.contains("  ") && !message.contains('\n') && !message.contains('\t'));
            },
            other => panic!("unexpected failure: {other}"),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod loopback_tests {
    use super::super::super::test_node::{Reply, StubNode};
    use super::super::authorization::{sign_permit, PermitTransfer, SignedPermit};
    use super::super::failure::{GaslessMember, RpcArm};
    use super::super::test_vectors::{address, MESSAGES, MESSAGE_MAX_FEE, MESSAGE_SERVICE_PROVIDER, USERS};
    use super::*;
    use crate::closed_port::closed_url;
    use common::block_on;
    use ethereum_types::U256;
    use mm2_eth::keys::Secret;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const TRACE_ID: &str = "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0";
    const REQUEST_ID: &str = "3f2b8c1e-5d4a-4f6b-9c7d-8e9f0a1b2c3d";

    const API_KEY: &str = "key-0f3a9c";
    const API_SECRET: &str = "secret-7b1d4e";

    fn provider(network: &Network, node: &StubNode, request_timeout_ms: Option<u64>) -> GasFreeProvider {
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        // A trailing slash on the configured URL must not double the slash of the path.
        GasFreeProvider::new(network, &format!("{}/", node.url()), credentials, request_timeout_ms)
            .expect("a host-only URL")
    }

    fn assert_no_credential(text: &str) {
        assert!(!text.contains(API_KEY), "the API key is rendered");
        assert!(!text.contains(API_SECRET), "the API secret is rendered");
    }

    fn account_envelope() -> String {
        serde_json::json!({
            "code": 200,
            "reason": null,
            "message": "",
            "data": {
                "accountAddress": USERS[0].address,
                "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
                "active": true,
                "nonce": "7",
                "allowSubmit": true,
                "assets": [{
                    "tokenAddress": MESSAGES[0].token,
                    "tokenSymbol": "USDT",
                    "activateFee": 1000000,
                    "transferFee": 2000000,
                    "decimal": 6,
                    "frozen": 0
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn account_info_is_an_authenticated_get_of_the_account_path() {
        let body = account_envelope();
        let node = StubNode::start(move |_, _| Reply::ok(&body));
        let user = address(USERS[0].address);
        let before_s = now_ms() / 1000;
        let account = block_on(provider(&Network::Nile, &node, None).account_info(&user)).expect("an account record");
        let after_s = now_ms() / 1000;
        assert_eq!(account.account_address, user);
        assert_eq!(account.nonce, 7);
        assert_eq!(account.assets[0].transfer_fee, U256::from(2_000_000u64));

        // One request, to the path with the network segment and without a doubled slash, with no body.
        let path = format!("/nile/api/v1/address/{}", USERS[0].address);
        let requests = node.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, path);
        assert!(requests[0].1.is_empty());

        // The two headers: the time of the request, and the signature over the method, that path and that time.
        let timestamp = node.header_values("timestamp")[0].clone().expect("a Timestamp header");
        let timestamp_s: u64 = timestamp.parse().expect("Unix seconds");
        assert!((before_s..=after_s).contains(&timestamp_s));
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        let expected = authorization_value(&credentials, "GET", &path, timestamp_s).expect("a value");
        let sent = node.header_values("authorization")[0]
            .clone()
            .expect("an Authorization header");
        assert!(sent == expected, "the Authorization header is not the signed value");
    }

    #[test]
    fn supported_tokens_is_a_get_of_the_token_path() {
        let node = StubNode::start(|_, _| {
            Reply::ok(
                r#"{"code":200,"message":"","data":{"tokens":[{"tokenAddress":"TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
                "createdAt":"2024-09-13T06:07:12.000+00:00","updatedAt":"2024-09-13T06:07:12.000+00:00",
                "activateFee":1000000,"transferFee":2000000,"symbol":"USDT","decimal":6,"supported":true}]}}"#,
            )
        });
        let tokens = block_on(provider(&Network::Mainnet, &node, None).supported_tokens()).expect("a token list");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].symbol, "USDT");
        assert!(tokens[0].supported);
        assert_eq!(node.requests()[0].0, "/tron/api/v1/config/token/all");
    }

    #[test]
    fn a_failed_exchange_is_typed_and_names_no_credential() {
        let user = address(USERS[0].address);

        let unauthorized =
            StubNode::start(|_, _| Reply::ok(r#"{"code":401,"reason":"Unauthorized","message":"bad key"}"#));
        let error = block_on(provider(&Network::Nile, &unauthorized, None).account_info(&user)).expect_err("a refusal");
        assert!(matches!(error, GasFreeError::Unauthorized { status: 401 }));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
        assert_no_credential(&format!("{error} {error:?}"));

        let rejecting = StubNode::start(|_, _| Reply::status(400, r#"{"code":400,"message":"no such account"}"#));
        let error = block_on(provider(&Network::Nile, &rejecting, None).account_info(&user)).expect_err("a refusal");
        assert!(matches!(&error, GasFreeError::Rejected { status: 400, message } if message == "no such account"));
        assert_no_credential(&format!("{error} {error:?}"));

        let drifting = StubNode::start(|_, _| Reply::ok(r#"{"code":200,"data":{"accountAddress":"x"}}"#));
        let error = block_on(provider(&Network::Nile, &drifting, None).account_info(&user)).expect_err("no record");
        assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::InvalidProviderResponse));
        assert_no_credential(&format!("{error} {error:?}"));

        // Nothing listens.
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        let unreachable =
            GasFreeProvider::new(&Network::Nile, &closed_url(), credentials, None).expect("a host-only URL");
        let error = block_on(unreachable.account_info(&user)).expect_err("no connection");
        assert!(matches!(error, GasFreeError::Transport(_)));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
        assert_no_credential(&format!("{error} {error:?}"));
    }

    #[test]
    fn a_call_is_bounded_by_the_request_timeout() {
        let slow = StubNode::start(|_, _| {
            std::thread::sleep(Duration::from_secs(3));
            Reply::ok("{}")
        });
        let user = address(USERS[0].address);
        let started = Instant::now();
        let error = block_on(provider(&Network::Nile, &slow, Some(300)).account_info(&user)).expect_err("a timeout");
        let elapsed = started.elapsed();
        assert!(matches!(error, GasFreeError::Timeout(_)));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
        assert!(
            elapsed >= Duration::from_millis(250) && elapsed < Duration::from_millis(2_500),
            "the call did not end at its bound"
        );
        assert_no_credential(&format!("{error} {error:?}"));
    }

    /// The first message vector, signed by its user.
    fn signed_first_message() -> SignedPermit {
        let message = &MESSAGES[0];
        let permit = PermitTransfer {
            token: address(message.token),
            service_provider: address(MESSAGE_SERVICE_PROVIDER),
            user: address(message.user),
            receiver: address(message.receiver),
            value: U256::from(message.value),
            max_fee: U256::from(MESSAGE_MAX_FEE),
            deadline: message.deadline,
            nonce: message.nonce,
        };
        let mut key = [0u8; 32];
        key[31] = 1;
        let secret = Secret::from_slice(&key).expect("a valid test key");
        sign_permit(&Network::Nile, permit, &secret, message.deadline - 1).expect("the user signs")
    }

    fn submit_request() -> SubmitRequest {
        SubmitRequest::new(signed_first_message(), Some(REQUEST_ID)).expect("a version 4 request id")
    }

    /// The success envelope with the provider's record of the first message vector in `state`.
    fn transfer_envelope(state: &str) -> String {
        serde_json::json!({
            "code": 200,
            "reason": null,
            "message": "",
            "data": {
                "id": TRACE_ID,
                "createdAt": "2024-09-13T06:07:12.000Z",
                "updatedAt": "2024-09-13T06:07:13.000Z",
                "expiredAt": "2024-09-13T06:12:12.000Z",
                "maxFee": MESSAGE_MAX_FEE,
                "signature": signed_first_message().sig_hex(),
                "accountAddress": MESSAGES[0].user,
                "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
                "providerAddress": MESSAGE_SERVICE_PROVIDER,
                "targetAddress": MESSAGES[0].receiver,
                "tokenAddress": MESSAGES[0].token,
                "amount": MESSAGES[0].value,
                "estimatedActivateFee": 1000000,
                "estimatedTransferFee": 2000000,
                "nonce": MESSAGES[0].nonce,
                "version": 1,
                "state": state
            }
        })
        .to_string()
    }

    /// A loopback port that takes a connection, reads what arrives and closes it without an answer. Returns its
    /// URL and the number of connections taken so far.
    fn closing_port() -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("a local address"));
        let taken = Arc::new(AtomicUsize::new(0));
        let counter = taken.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut chunk = [0u8; 4096];
                let _ = stream.read(&mut chunk);
            }
        });
        (url, taken)
    }

    /// ch.63 T18: a submit is one authenticated POST of the payload to the submit path.
    #[test]
    fn t18_submit_is_one_authenticated_post_of_the_payload() {
        let body = transfer_envelope("WAITING");
        let node = StubNode::start(move |_, _| Reply::ok(&body));
        let request = submit_request();
        let record = block_on(provider(&Network::Nile, &node, None).submit(&request)).expect("a transfer record");
        assert_eq!(record.id, TRACE_ID);
        assert_eq!(record.nonce, MESSAGES[0].nonce);

        let path = "/nile/api/v1/gasfree/submit";
        let requests = node.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, path);
        let sent: Json = serde_json::from_str(&requests[0].1).expect("a JSON body");
        let expected = serde_json::to_value(&request).expect("a document");
        assert!(sent == expected, "the body is not the submit payload");
        assert!(sent["requestId"] == REQUEST_ID && sent["version"] == "1");
        assert!(node.header_values("content-type")[0]
            .as_deref()
            .is_some_and(|value| value.starts_with("application/json")));

        // The signature is over the method POST, that path and the time of the request.
        let timestamp = node.header_values("timestamp")[0].clone().expect("a Timestamp header");
        let timestamp_s: u64 = timestamp.parse().expect("Unix seconds");
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        let expected = authorization_value(&credentials, "POST", path, timestamp_s).expect("a value");
        let sent = node.header_values("authorization")[0]
            .clone()
            .expect("an Authorization header");
        assert!(sent == expected, "the Authorization header is not the signed value");
    }

    /// ch.63 T18: a trace is one authenticated GET of the trace path.
    #[test]
    fn t18_trace_is_one_authenticated_get_of_the_trace_path() {
        let body = transfer_envelope("CONFIRMING");
        let node = StubNode::start(move |_, _| Reply::ok(&body));
        let record = block_on(provider(&Network::Nile, &node, None).trace(TRACE_ID)).expect("a transfer record");
        assert_eq!(record.id, TRACE_ID);

        let path = format!("/nile/api/v1/gasfree/{TRACE_ID}");
        let requests = node.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, path);
        assert!(requests[0].1.is_empty());
        let timestamp = node.header_values("timestamp")[0].clone().expect("a Timestamp header");
        let timestamp_s: u64 = timestamp.parse().expect("Unix seconds");
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        let expected = authorization_value(&credentials, "GET", &path, timestamp_s).expect("a value");
        let sent = node.header_values("authorization")[0]
            .clone()
            .expect("an Authorization header");
        assert!(sent == expected, "the Authorization header is not the signed value");
    }

    /// ch.63 T18, R31: a 5xx answer, a timeout and a failed connection are followed by no further request, for a
    /// submit and for a trace.
    #[test]
    fn t18_a_failed_submit_or_trace_is_not_repeated() {
        let request = submit_request();

        let failing = StubNode::start(|_, _| Reply::status(503, r#"{"code":503,"message":"maintenance"}"#));
        let client = provider(&Network::Nile, &failing, None);
        let error = block_on(client.submit(&request)).err().expect("a provider failure");
        assert!(matches!(error, GasFreeError::ProviderFailed { status: 503, .. }));
        assert_no_credential(&format!("{error} {error:?}"));
        let error = block_on(client.trace(TRACE_ID)).err().expect("a provider failure");
        assert!(matches!(error, GasFreeError::ProviderFailed { status: 503, .. }));

        let slow = StubNode::start(|_, _| {
            std::thread::sleep(Duration::from_millis(1_500));
            Reply::ok("{}")
        });
        let client = provider(&Network::Nile, &slow, Some(300));
        let error = block_on(client.submit(&request)).err().expect("a timeout");
        assert!(matches!(error, GasFreeError::Timeout(_)));
        let error = block_on(client.trace(TRACE_ID)).err().expect("a timeout");
        assert!(matches!(error, GasFreeError::Timeout(_)));

        let (url, taken) = closing_port();
        let credentials = ProviderCredentials::new(API_KEY.to_owned(), API_SECRET.to_owned());
        let client = GasFreeProvider::new(&Network::Nile, &url, credentials, None).expect("a host-only URL");
        let error = block_on(client.submit(&request)).err().expect("a failed connection");
        assert!(matches!(error, GasFreeError::Transport(_)));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
        assert_no_credential(&format!("{error} {error:?}"));
        let error = block_on(client.trace(TRACE_ID)).err().expect("a failed connection");
        assert!(matches!(error, GasFreeError::Transport(_)));

        // Time for a repeated request to arrive, if one were made.
        std::thread::sleep(Duration::from_millis(1_700));
        assert_eq!(failing.requests().len(), 2, "a request was repeated after a 5xx answer");
        assert_eq!(slow.requests().len(), 2, "a request was repeated after a timeout");
        assert_eq!(
            taken.load(Ordering::SeqCst),
            2,
            "a request was repeated after a failed connection"
        );
    }

    /// ch.63 T18, R31: a submit payload with another version than 1 and a trace id that is not a UUID are refused
    /// with no request made.
    #[test]
    fn t18_the_pre_send_refusals_make_no_request() {
        let node = StubNode::start(|_, _| Reply::ok(&transfer_envelope("WAITING")));
        let client = provider(&Network::Nile, &node, None);

        let mut body = serde_json::to_value(submit_request()).expect("a document");
        for version in [
            serde_json::json!("2"),
            serde_json::json!("0"),
            serde_json::json!(1),
            Json::Null,
        ] {
            body["version"] = version;
            let error = block_on(client.post_submit(&body)).err().expect("a refusal");
            assert!(matches!(error, GasFreeError::Internal(_)));
            assert_eq!(error.rpc_arm(), RpcArm::InternalError);
        }

        for trace_id in [
            "",
            "not-a-uuid",
            "6c3ff67e-0bf4-4c09-91ca",
            "../address/x",
            "6c3ff67e?x=1",
        ] {
            let error = block_on(client.trace(trace_id)).err().expect("a refusal");
            assert!(matches!(error, GasFreeError::Internal(_)));
            assert_eq!(error.rpc_arm(), RpcArm::InternalError);
        }
        assert!(node.requests().is_empty(), "a refused request reached the provider");
    }

    /// ch.63 T20: a 404 of the trace request, as the HTTP status and as the envelope code, is the unknown trace
    /// id; a 404 of the submit request is a refusal like every other 4xx.
    #[test]
    fn t20_a_404_is_the_unknown_trace_for_the_trace_request_only() {
        let request = submit_request();
        for (status, body) in [
            (404u16, "Not Found".to_owned()),
            (
                404,
                r#"{"code":404,"reason":"NotFound","message":"no such transfer"}"#.to_owned(),
            ),
            (
                200,
                r#"{"code":404,"reason":"NotFound","message":"no such transfer"}"#.to_owned(),
            ),
        ] {
            let node = StubNode::start(move |_, _| Reply::status(status, &body));
            let client = provider(&Network::Nile, &node, None);

            let error = block_on(client.trace(TRACE_ID)).err().expect("an unknown trace");
            assert!(matches!(error, GasFreeError::TraceNotFound(_)));
            assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::TraceNotFound));
            assert_eq!(GaslessMember::TraceNotFound.http_status(), 404);

            let error = block_on(client.submit(&request)).err().expect("a refusal");
            assert!(matches!(error, GasFreeError::Rejected { status: 404, .. }));
            assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::ProviderRejected));
            assert_eq!(GaslessMember::ProviderRejected.http_status(), 502);
            assert_eq!(node.requests().len(), 2);
        }

        // Another 4xx of the trace request stays a refusal.
        let node = StubNode::start(|_, _| Reply::status(400, r#"{"code":400,"message":"bad id"}"#));
        let error = block_on(provider(&Network::Nile, &node, None).trace(TRACE_ID))
            .err()
            .expect("a refusal");
        assert!(matches!(error, GasFreeError::Rejected { status: 400, .. }));
    }
}
