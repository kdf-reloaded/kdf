//! # Purpose
//!
//! Host-only naming of EVM and TRON node URLs (CRD ch.35 R35.1.14, ch.21 R-N1).
//!
//! A node URL is a credential: hosted providers put the API key in the path, the query string or the
//! userinfo. Any log line or error text that names a node therefore renders it as `scheme://host[:port]`
//! and nothing else.
//!
//! # Public exports
//!
//! - [`host_only`] — the host-only form of one configured URL.
//! - [`scrub_node_url`] — rewrites a lower-layer message so that it no longer carries that URL.
//! - [`UNNAMED_NODE`] — what stands in for a URL that cannot be parsed.
//!
//! # Invariants
//!
//! - Pure functions; no I/O and no logging.
//! - An input that is not an absolute URL with a host is never echoed: it renders as [`UNNAMED_NODE`].

use http::Uri;
use std::str::FromStr;

/// Stands in for a node URL that has no scheme or no host, so that the raw string is never echoed.
pub(crate) const UNNAMED_NODE: &str = "<invalid node URL>";

/// Renders `url` as `scheme://host` or `scheme://host:port` (IPv6 hosts keep their brackets).
///
/// Userinfo, path, query and fragment are dropped. A string that is not an absolute URL with a host renders as
/// [`UNNAMED_NODE`].
pub(crate) fn host_only(url: &str) -> String {
    match Uri::from_str(url) {
        Ok(uri) => host_only_uri(&uri),
        Err(_) => UNNAMED_NODE.to_owned(),
    }
}

fn host_only_uri(uri: &Uri) -> String {
    match (uri.scheme_str(), uri.host()) {
        (Some(scheme), Some(host)) if !host.is_empty() => match uri.port_u16() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        },
        _ => UNNAMED_NODE.to_owned(),
    }
}

/// Rewrites `text`, a message produced by a lower layer (HTTP client, TLS, DNS) for the node `url`, so that it
/// no longer contains that node's URL.
///
/// The configured string and the form [`Uri`] displays are replaced by the host-only form; as a safety net for
/// a lower layer that prints only a part of the URL, the userinfo and the path-and-query are removed wherever
/// they appear.
pub(crate) fn scrub_node_url(text: &str, url: &str) -> String {
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

    #[test]
    fn host_only_strips_userinfo_path_query_and_fragment() {
        assert_eq!(
            host_only("https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X#frag"),
            "https://rpc.example.org:8545"
        );
    }

    #[test]
    fn host_only_without_port_or_path() {
        assert_eq!(host_only("https://rpc.example.org"), "https://rpc.example.org");
        assert_eq!(host_only("http://rpc.example.org/"), "http://rpc.example.org");
        assert_eq!(host_only("http://localhost:8545"), "http://localhost:8545");
    }

    #[test]
    fn host_only_keeps_ipv6_brackets() {
        assert_eq!(host_only("http://[::1]:8545/key/SECRET"), "http://[::1]:8545");
        assert_eq!(host_only("https://[2001:db8::1]/key/SECRET"), "https://[2001:db8::1]");
    }

    #[test]
    fn host_only_never_echoes_an_unparsable_or_hostless_input() {
        for raw in [
            "not a url",
            "",
            "rpc.example.org:8545/SECRETKEY",
            "/just/a/SECRETKEY",
            "https://",
            "SECRETKEY",
        ] {
            let label = host_only(raw);
            assert_eq!(label, UNNAMED_NODE, "{raw:?}");
            assert!(!label.contains("SECRETKEY"));
        }
    }

    #[test]
    fn scrub_replaces_the_full_url_in_a_lower_layer_message() {
        let url = "https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X";
        let text = format!("Transport '{url}' error: connection refused; Request '{url}' timeout");
        let scrubbed = scrub_node_url(&text, url);
        assert_eq!(
            scrubbed,
            "Transport 'https://rpc.example.org:8545' error: connection refused; Request \
             'https://rpc.example.org:8545' timeout"
        );
    }

    #[test]
    fn scrub_replaces_the_uri_display_form_and_partial_forms() {
        let url = "https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X";
        let uri_form = url.parse::<Uri>().unwrap().to_string();
        for text in [
            format!("error sending request for url ({uri_form})"),
            "dns error for user:secret@rpc.example.org:8545".to_owned(),
            "GET /v3/APIKEY?token=X failed".to_owned(),
        ] {
            let scrubbed = scrub_node_url(&text, url);
            for secret in ["user", "secret", "/v3/APIKEY", "APIKEY", "token=X"] {
                assert!(!scrubbed.contains(secret), "{secret} survives in {scrubbed:?}");
            }
        }
    }

    #[test]
    fn scrub_leaves_an_unrelated_message_and_an_empty_url_alone() {
        assert_eq!(
            scrub_node_url("HTTP 502 Bad Gateway", "https://rpc.example.org"),
            "HTTP 502 Bad Gateway"
        );
        assert_eq!(scrub_node_url("anything", ""), "anything");
    }

    #[test]
    fn scrub_of_an_unparsable_url_still_removes_the_raw_string() {
        let scrubbed = scrub_node_url("bad url SECRETKEY here", "SECRETKEY");
        assert!(!scrubbed.contains("SECRETKEY"));
    }
}
