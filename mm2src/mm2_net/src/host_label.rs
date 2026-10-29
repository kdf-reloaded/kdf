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
//! - A URI without a scheme or without a host is never echoed: it renders as [`UNNAMED_NODE`].

use http::Uri;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const SECRETS: [&str; 5] = ["user", "hunter2", "APIKEY", "token=X", "frag"];

    fn label(uri: &str) -> String { host_only_uri(&Uri::from_str(uri).unwrap()) }

    fn assert_no_secret(text: &str) {
        for secret in SECRETS {
            assert!(!text.contains(secret), "'{secret}' leaked into '{text}'");
        }
    }

    #[test]
    fn userinfo_path_query_and_fragment_are_dropped() {
        let text = label("https://user:hunter2@node.example:8545/v3/APIKEY?token=X#frag");
        assert_eq!(text, "https://node.example:8545");
        assert_no_secret(&text);
    }

    #[test]
    fn a_missing_port_stays_missing() {
        assert_eq!(label("https://node.example/v3/APIKEY"), "https://node.example");
    }

    #[test]
    fn ipv6_keeps_its_brackets() {
        assert_eq!(label("http://[::1]:8545/key/APIKEY"), "http://[::1]:8545");
        assert_eq!(label("https://[2001:db8::1]/key/APIKEY"), "https://[2001:db8::1]");
    }

    #[test]
    fn a_uri_without_a_host_is_never_echoed() {
        for raw in ["/v3/APIKEY?token=X", "*", "node.example:8545/APIKEY"] {
            if let Ok(uri) = Uri::from_str(raw) {
                let text = host_only_uri(&uri);
                assert_eq!(text, UNNAMED_NODE, "{raw}");
                assert_no_secret(&text);
            }
        }
    }
}
