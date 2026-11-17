//! The TRON node list of an activation request, with the request headers each node is sent (CRD ch.21 §21.9
//! R-N9).
//!
//! # Purpose
//!
//! An entry of the legacy `urls` array is a plain URL string or an object `{"url": "...", "headers": {...}}`. The
//! headers of an object entry go to that node only. The optional top-level `MM2.json` key `tron_api_key` is sent as
//! the `TRON-PRO-API-KEY` header to the three TronGrid hosts over HTTPS only, and never to any other host in the same list. A
//! header of the same name on the node entry wins over the key for that node.
//!
//! # Invariants
//!
//! - Header values and the API key are secrets (CRD ch.35 R35.1.17): no `Debug` form, error text or log line
//!   renders one (`headers: <set>`, `tron_api_key: <set>`), and no error of this module echoes a configured string.
//! - The headers of a node are final when the [`TronNode`] is built: [`TronNode::new`] applies the key rule once, so
//!   the request path only copies them.
//! - Host matching is an exact, case-insensitive comparison of the URL's host with a fixed list: never a prefix, a
//!   suffix or a substring match, and the userinfo of a URL is never taken for its host.

use crate::node_url::{host_only, SECRET_SET};
use derive_more::Display;
use http::header::{HeaderName, HeaderValue};
use http::HeaderMap;
use serde_json::Value as Json;
use std::fmt;
use std::str::FromStr;

/// The request header TronGrid reads the API key from (`TRON-PRO-API-KEY`; header names are case-insensitive).
const TRON_API_KEY_HEADER: HeaderName = HeaderName::from_static("tron-pro-api-key");

/// The hosts that receive `tron_api_key`, compared exactly and without regard to letter case.
const TRONGRID_HOSTS: [&str; 3] = ["api.trongrid.io", "nile.trongrid.io", "api.shasta.trongrid.io"];

/// Why a node list or the API key of an activation request is refused. No text names a configured string.
#[derive(Debug, Display, PartialEq, Eq)]
pub enum TronNodeConfigError {
    #[display(fmt = "Failed to parse 'urls' for TRON activation: expected an array")]
    UrlsNotAnArray,
    #[display(fmt = "Enable request for TRON coin must have at least 1 node URL")]
    NoNodes,
    #[display(
        fmt = "Failed to parse 'urls' for TRON activation: entry {} is neither a URL string nor an object with 'url' \
               and an optional 'headers' object",
        position
    )]
    BadEntry { position: usize },
    /// `host` is the host-only form of the URL.
    #[display(fmt = "TRON node URL '{}' is not a valid URI", host)]
    InvalidUrl { host: String },
    #[display(fmt = "TRON node {} has a URL with a scheme other than http or https", position)]
    BadScheme { position: usize },
    #[display(
        fmt = "TRON node {} has a header name that is not a valid HTTP header name",
        position
    )]
    BadHeaderName { position: usize },
    #[display(
        fmt = "TRON node {} has a header value that is not a valid HTTP header value",
        position
    )]
    BadHeaderValue { position: usize },
    #[display(fmt = "'tron_api_key' must be a non-empty string that is a valid HTTP header value")]
    BadApiKey,
}

impl std::error::Error for TronNodeConfigError {}

/// The `tron_api_key` of `MM2.json`. A secret: its `Debug` form is `<set>`.
#[derive(Clone)]
pub struct TronApiKey(HeaderValue);

impl fmt::Debug for TronApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(SECRET_SET) }
}

impl TronApiKey {
    /// Reads `tron_api_key` from the `MM2.json` object. `Ok(None)` when the key is absent or `null`.
    ///
    /// # Errors
    ///
    /// [`TronNodeConfigError::BadApiKey`] when the key is present but not a non-empty string that is a valid HTTP
    /// header value. The error does not echo the key.
    pub fn from_conf(conf: &Json) -> Result<Option<Self>, TronNodeConfigError> {
        match conf.get("tron_api_key") {
            None | Some(Json::Null) => Ok(None),
            Some(Json::String(key)) if !key.is_empty() => {
                let mut value = HeaderValue::from_str(key).map_err(|_| TronNodeConfigError::BadApiKey)?;
                value.set_sensitive(true);
                Ok(Some(TronApiKey(value)))
            },
            Some(_) => Err(TronNodeConfigError::BadApiKey),
        }
    }
}

/// A configured node: its base URL and the headers every request to it carries.
#[derive(Clone)]
pub struct TronNode {
    url: String,
    headers: HeaderMap,
}

/// The debug form names the node host-only and never renders a header (CRD ch.35 R35.1.17).
impl fmt::Debug for TronNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers = if self.headers.is_empty() { "<none>" } else { SECRET_SET };
        f.debug_struct("TronNode")
            .field("node", &host_only(&self.url))
            .field("headers", &format_args!("{headers}"))
            .finish()
    }
}

impl TronNode {
    /// A node with the headers of its own entry (`entry_headers`) and, when `api_key` is set and the node is a
    /// TronGrid host, the `TRON-PRO-API-KEY` header. A header of the entry with that name (letter case ignored)
    /// wins over the key.
    pub fn new(url: String, entry_headers: HeaderMap, api_key: Option<&TronApiKey>) -> Self {
        let mut headers = entry_headers;
        if let Some(TronApiKey(key)) = api_key {
            if is_trongrid_host(&url) && !headers.contains_key(&TRON_API_KEY_HEADER) {
                headers.insert(TRON_API_KEY_HEADER, key.clone());
            }
        }
        TronNode { url, headers }
    }

    /// A node with no headers of its own and no API key.
    pub fn plain(url: String) -> Self { TronNode::new(url, HeaderMap::new(), None) }

    /// The configured URL. It can carry a credential: it is only ever used to send and to scrub.
    pub(crate) fn url(&self) -> &str { &self.url }

    pub(crate) fn headers(&self) -> &HeaderMap { &self.headers }
}

/// Whether `url` is an `https` URL whose host is one of the TronGrid hosts: an exact match, letter case ignored.
/// A TronGrid host given as `http` is not one, so the key never travels in clear text.
fn is_trongrid_host(url: &str) -> bool {
    let Ok(uri) = http::Uri::from_str(url) else {
        return false;
    };
    uri.scheme_str()
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
        && uri
            .host()
            .is_some_and(|host| TRONGRID_HOSTS.iter().any(|known| known.eq_ignore_ascii_case(host)))
}

/// The nodes of the `urls` array of an activation request, in the order given. Each entry is a URL string or an
/// object `{"url": "...", "headers": {"Name": "value", ...}}`; `api_key` is applied per [`TronNode::new`].
///
/// # Errors
///
/// A [`TronNodeConfigError`] when `urls` is not an array or is empty, an entry has another form or an unknown key,
/// a URL is not a valid URI, or a header name or value is invalid. No error names a configured string; a URL is
/// named host-only and a header only by the node's position (counted from 1).
pub fn parse_nodes(urls: &Json, api_key: Option<&TronApiKey>) -> Result<Vec<TronNode>, TronNodeConfigError> {
    let entries = urls.as_array().ok_or(TronNodeConfigError::UrlsNotAnArray)?;
    if entries.is_empty() {
        return Err(TronNodeConfigError::NoNodes);
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let position = index + 1;
            let (url, headers) = match entry {
                Json::String(url) => (url.clone(), HeaderMap::new()),
                Json::Object(object) => {
                    if object.keys().any(|key| key != "url" && key != "headers") {
                        return Err(TronNodeConfigError::BadEntry { position });
                    }
                    let url = object
                        .get("url")
                        .and_then(Json::as_str)
                        .ok_or(TronNodeConfigError::BadEntry { position })?;
                    let headers = match object.get("headers") {
                        None | Some(Json::Null) => HeaderMap::new(),
                        Some(Json::Object(entries)) => parse_headers(entries, position)?,
                        Some(_) => return Err(TronNodeConfigError::BadEntry { position }),
                    };
                    (url.to_owned(), headers)
                },
                _ => return Err(TronNodeConfigError::BadEntry { position }),
            };
            let Ok(uri) = http::Uri::from_str(&url) else {
                // Never echo the configured string: it may carry an API key (CRD ch.35 R35.1.14).
                return Err(TronNodeConfigError::InvalidUrl { host: host_only(&url) });
            };
            // Only `http` and `https` (CRD ch.21 R21.10B.2); the position is named, never the URL.
            if !uri
                .scheme_str()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
            {
                return Err(TronNodeConfigError::BadScheme { position });
            }
            Ok(TronNode::new(url, headers, api_key))
        })
        .collect()
}

fn parse_headers(entries: &serde_json::Map<String, Json>, position: usize) -> Result<HeaderMap, TronNodeConfigError> {
    let mut headers = HeaderMap::new();
    for (name, value) in entries {
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| TronNodeConfigError::BadHeaderName { position })?;
        let value = value.as_str().ok_or(TronNodeConfigError::BadHeaderValue { position })?;
        let mut value = HeaderValue::from_str(value).map_err(|_| TronNodeConfigError::BadHeaderValue { position })?;
        value.set_sensitive(true);
        // A second spelling of the same name (letter case) replaces the first.
        headers.insert(name, value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "KEY-SECRET-123";

    fn key() -> TronApiKey {
        TronApiKey::from_conf(&json!({ "tron_api_key": KEY }))
            .expect("a valid key")
            .expect("a key")
    }

    fn header_text(node: &TronNode, name: &str) -> Option<String> {
        node.headers()
            .get(HeaderName::from_bytes(name.as_bytes()).expect("a valid name"))
            .map(|value| value.to_str().expect("visible ASCII").to_owned())
    }

    #[test]
    fn the_key_goes_to_every_trongrid_host_only() {
        let key = key();
        for url in [
            "https://api.trongrid.io",
            "https://nile.trongrid.io",
            "https://api.shasta.trongrid.io",
            "https://API.TronGrid.IO",
            "https://api.trongrid.io:443/path",
            "HTTPS://api.trongrid.io",
        ] {
            let node = TronNode::new(url.to_owned(), HeaderMap::new(), Some(&key));
            assert_eq!(header_text(&node, "TRON-PRO-API-KEY").as_deref(), Some(KEY), "{url}");
        }
        for url in [
            "https://tron.example.org",
            "https://trongrid.io",
            "https://api.trongrid.io.evil.example",
            "https://evil-api.trongrid.io",
            "https://x.api.trongrid.io",
            "https://evil.example/api.trongrid.io",
            "https://evil.example/?h=nile.trongrid.io",
            "http://api.trongrid.io",
            "http://nile.trongrid.io:8090",
            "https://api.trongrid.io@evil.example",
            "https://user:pw@evil.example",
            "not a url",
        ] {
            let node = TronNode::new(url.to_owned(), HeaderMap::new(), Some(&key));
            assert!(node.headers().is_empty(), "{url} got a header");
        }
    }

    #[test]
    fn an_absent_key_sends_no_header() {
        let node = TronNode::new("https://api.trongrid.io".to_owned(), HeaderMap::new(), None);
        assert!(node.headers().is_empty());
        assert_eq!(TronApiKey::from_conf(&json!({})).unwrap().map(|_| ()), None);
        assert_eq!(
            TronApiKey::from_conf(&json!({ "tron_api_key": null }))
                .unwrap()
                .map(|_| ()),
            None
        );
    }

    #[test]
    fn a_node_header_of_the_same_name_wins_over_the_key() {
        let nodes = parse_nodes(
            &json!([
                {"url": "https://api.trongrid.io", "headers": {"Tron-Pro-Api-Key": "NODE-OWN"}},
                {"url": "https://nile.trongrid.io", "headers": {"X-Other": "o"}},
            ]),
            Some(&key()),
        )
        .unwrap();
        assert_eq!(header_text(&nodes[0], "TRON-PRO-API-KEY").as_deref(), Some("NODE-OWN"));
        assert_eq!(nodes[0].headers().len(), 1);
        // The other node keeps its own header and gets the key.
        assert_eq!(header_text(&nodes[1], "x-other").as_deref(), Some("o"));
        assert_eq!(header_text(&nodes[1], "TRON-PRO-API-KEY").as_deref(), Some(KEY));
    }

    #[test]
    fn entries_of_both_forms_keep_their_order_and_their_own_headers() {
        let nodes = parse_nodes(
            &json!([
                "https://plain.example",
                {"url": "https://custom.example", "headers": {"X-Token": "t1"}},
                {"url": "https://bare.example"},
            ]),
            None,
        )
        .unwrap();
        assert_eq!(nodes.iter().map(TronNode::url).collect::<Vec<_>>(), [
            "https://plain.example",
            "https://custom.example",
            "https://bare.example"
        ]);
        assert!(nodes[0].headers().is_empty());
        assert_eq!(header_text(&nodes[1], "X-Token").as_deref(), Some("t1"));
        assert!(nodes[2].headers().is_empty());
    }

    #[test]
    fn invalid_entries_are_refused_without_echoing_a_configured_string() {
        let secret_value = "VALUE-SECRET\r\nInjected: 1";
        let cases = [
            (json!({}), "expected an array"),
            (json!([]), "at least 1 node URL"),
            (json!([5]), "entry 1"),
            (json!(["https://ok.example", {"headers": {}}]), "entry 2"),
            (json!([{"url": "https://ok.example", "extra-SECRETKEY": 1}]), "entry 1"),
            (
                json!([{"url": "https://ok.example", "headers": ["SECRETLIST"]}]),
                "entry 1",
            ),
            (
                json!([{"url": "https://ok.example", "headers": {"bad name SECRETNAME": "v"}}]),
                "header name",
            ),
            (
                json!([{"url": "https://ok.example", "headers": {"X-A": 5}}]),
                "header value",
            ),
            (
                json!([{"url": "https://ok.example", "headers": {"X-A": secret_value}}]),
                "header value",
            ),
            (json!(["https://user:SECRETPW@bad host/APIKEY"]), "not a valid URI"),
        ];
        for (urls, expected) in cases {
            let err = parse_nodes(&urls, None).expect_err("must be refused").to_string();
            assert!(err.contains(expected), "{err}");
            for secret in [
                "SECRETKEY",
                "SECRETLIST",
                "SECRETNAME",
                "VALUE-SECRET",
                "Injected",
                "SECRETPW",
                "APIKEY",
            ] {
                assert!(!err.contains(secret), "{err} renders a configured string");
            }
        }
    }

    #[test]
    fn only_http_and_https_node_urls_are_accepted() {
        for url in [
            "http://a.example",
            "https://a.example",
            "HTTPS://a.example",
            "Http://a.example:8090/x",
        ] {
            parse_nodes(&json!([url]), None).expect("accepted");
        }
        for url in [
            "ws://SECRETHOST.example",
            "wss://a.example",
            "ftp://a.example",
            "a.example:8090",
        ] {
            let err = parse_nodes(&json!(["https://ok.example", url]), None).expect_err("refused");
            assert_eq!(err, TronNodeConfigError::BadScheme { position: 2 }, "{url}");
            assert!(!err.to_string().contains("example"), "{err}");
        }
    }

    #[test]
    fn an_invalid_api_key_is_refused_without_echoing_it() {
        for conf in [
            json!({"tron_api_key": ""}),
            json!({"tron_api_key": 5}),
            json!({"tron_api_key": "SECRET\nKEY"}),
        ] {
            let err = TronApiKey::from_conf(&conf).expect_err("refused");
            assert_eq!(err, TronNodeConfigError::BadApiKey);
            assert!(!err.to_string().contains("SECRET"));
        }
    }

    #[test]
    fn debug_forms_render_no_secret() {
        let nodes = parse_nodes(
            &json!([{"url": "https://user:URLSECRET@api.trongrid.io/KEYPATH", "headers": {"X-A": "HEADERSECRET"}}]),
            Some(&key()),
        )
        .unwrap();
        let text = format!("{:?} {:?}", nodes[0], key());
        assert!(text.contains("headers: <set>"), "{text}");
        assert!(text.contains("<set>"), "{text}");
        for secret in ["HEADERSECRET", KEY, "URLSECRET", "KEYPATH"] {
            assert!(!text.contains(secret), "{text} renders a secret");
        }
        assert!(format!("{:?}", TronNode::plain("https://x.example".to_owned())).contains("headers: <none>"));
    }
}
