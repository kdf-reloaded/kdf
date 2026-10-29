//! # Purpose
//!
//! The host-only label of a URI: the one form in which a log line or error text may name a node or a service whose
//! URL can carry a credential (CRD ch.35 R35.1.14, R35.1.17). It lives here, below `coins`, so that the shared
//! HTTP layer can name the host it talked to; `coins::node_url` builds on it.
//!
//! # Invariants
//!
//! - Pure; no I/O and no logging.
//! - Only the scheme, host and port are rendered. Userinfo, path, query and fragment are never part of the result.
//! - A URI or string without a scheme or without a host is never echoed: it renders as [`UNNAMED_NODE`].

use http::Uri;
use std::str::FromStr;

/// Stands in for a node URL that has no scheme or no host, so that the raw string is never echoed.
pub const UNNAMED_NODE: &str = "<invalid node URL>";

/// Renders `uri` as `scheme://host` or `scheme://host:port` (IPv6 hosts keep their brackets).
///
/// Userinfo, path, query and fragment are dropped. A URI that is not absolute or has no host renders as
/// [`UNNAMED_NODE`].
pub fn host_only_uri(uri: &Uri) -> String {
    match (uri.scheme_str(), uri.host()) {
        (Some(scheme), Some(host)) if !host.is_empty() => match uri.port_u16() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        },
        _ => UNNAMED_NODE.to_owned(),
    }
}

/// Renders `url` as `scheme://host` or `scheme://host:port` (IPv6 hosts keep their brackets).
///
/// Userinfo, path, query and fragment are dropped. A string that is not an absolute URL with a host renders as
/// [`UNNAMED_NODE`].
pub fn host_only(url: &str) -> String {
    match Uri::from_str(url) {
        Ok(uri) => host_only_uri(&uri),
        Err(_) => UNNAMED_NODE.to_owned(),
    }
}

/// Rewrites `text`, a message produced by a lower layer (HTTP client, TLS, DNS) for the node `url`, so that it
/// no longer contains that node's URL.
///
/// The configured string and the form [`Uri`] displays are replaced by the host-only form; as a safety net for
/// a lower layer that prints only a part of the URL, the userinfo and the path-and-query are removed wherever
/// they appear.
pub fn scrub_node_url(text: &str, url: &str) -> String {
    if url.is_empty() {
        return text.to_owned();
    }
    let label = host_only(url);
    let mut out = text.replace(url, &label);
    let Ok(uri) = Uri::from_str(url) else {
        return out;
    };
    out = out.replace(&uri.to_string(), &label);
    if let Some(authority) = uri.authority().map(|a| a.as_str()) {
        if let Some((userinfo, host_port)) = authority.rsplit_once('@') {
            out = out.replace(authority, host_port);
            out = out.replace(&format!("{userinfo}@"), "");
        }
    }
    if let Some(path_and_query) = uri.path_and_query().map(|p| p.as_str()) {
        if path_and_query.len() > 1 {
            out = out.replace(path_and_query, "");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const SECRETS: [&str; 5] = ["bob", "hunter2", "APIKEY", "token=X", "frag"];

    fn label(uri: &str) -> String { host_only_uri(&Uri::from_str(uri).unwrap()) }

    /// Failure messages are static text (CRD ch.35 R35.1.17): never the rendered text or a secret part.
    fn assert_label(actual: &str, expected: &str) {
        assert!(actual == expected, "the label differs from the expected host-only form");
    }

    fn assert_no_secret(text: &str) {
        for (i, secret) in SECRETS.iter().enumerate() {
            assert!(!text.contains(secret), "secret part {i} leaked into the rendered text");
        }
    }

    #[test]
    fn userinfo_path_query_and_fragment_are_dropped() {
        let text = label("https://bob:hunter2@node.example:8545/v3/APIKEY?token=X#frag");
        assert_label(&text, "https://node.example:8545");
        assert_no_secret(&text);
    }

    #[test]
    fn a_missing_port_stays_missing() {
        assert_label(&label("https://node.example/v3/APIKEY"), "https://node.example");
    }

    #[test]
    fn ipv6_keeps_its_brackets() {
        assert_label(&label("http://[::1]:8545/key/APIKEY"), "http://[::1]:8545");
        assert_label(&label("https://[2001:db8::1]/key/APIKEY"), "https://[2001:db8::1]");
    }

    #[test]
    fn a_uri_without_a_host_is_never_echoed() {
        for raw in ["/v3/APIKEY?token=X", "*", "node.example:8545/APIKEY"] {
            if let Ok(uri) = Uri::from_str(raw) {
                let text = host_only_uri(&uri);
                assert_label(&text, UNNAMED_NODE);
                assert_no_secret(&text);
            }
        }
    }

    #[test]
    fn a_string_is_named_by_its_host_only_and_an_unparsable_one_is_never_echoed() {
        assert_label(
            &host_only("https://bob:hunter2@node.example:8545/v3/APIKEY?token=X"),
            "https://node.example:8545",
        );
        for raw in ["hunter2 APIKEY token=X", "bob:hunter2@/APIKEY", ""] {
            let text = host_only(raw);
            assert_label(&text, UNNAMED_NODE);
            assert_no_secret(&text);
        }
    }

    /// The shape of the message a browser gives for a request it cannot build; the wasm fetch path scrubs its
    /// lower-layer texts with this function.
    #[test]
    fn a_lower_layer_text_loses_the_url_it_repeats() {
        let url = "https://bob:hunter2@node.example:8545/v3/APIKEY?token=X";
        let text = format!("Failed to construct 'Request': Failed to parse URL from {url}");
        let scrubbed = scrub_node_url(&text, url);
        assert!(
            scrubbed.contains("https://node.example:8545"),
            "the host-only form is missing from the scrubbed text"
        );
        assert_no_secret(&scrubbed);
        // A string that is not a URL is replaced as a whole.
        let raw = "hunter2 APIKEY token=X";
        let scrubbed = scrub_node_url(&format!("cannot parse {raw}"), raw);
        assert!(
            scrubbed == format!("cannot parse {UNNAMED_NODE}"),
            "the scrubbed text differs from the expected one"
        );
    }
}
