//! # Purpose
//!
//! Host-only naming of node and server addresses, and the scrubbing of lower-layer texts, shared by every coin
//! family (CRD ch.35 R35.1.14 and R35.1.17, ch.21 §21.9, ch.36 §36.5A, ch.38 §38.6.9, ch.39 §39.11.6, ch.40
//! §40.5A, ch.20 §20.8).
//!
//! A node URL is a credential: hosted providers put the API key in the path, the query string or the
//! userinfo. Any log line, error text or debug form that names a node therefore renders it as
//! `scheme://host[:port]` and nothing else; an Electrum server is rendered as `host:port`.
//!
//! # Public exports
//!
//! - [`host_only`] — the host-only form of one configured URL.
//! - [`NodeSlot`] / [`ListedNode`] / [`listed_nodes`] — the ticker and list position (R35.1.16) that travel with a
//!   node URL, and the one place that renders a node log line.
//! - [`scrub_node_url`] — rewrites a lower-layer message so that it no longer carries that URL.
//! - [`scrub_urls_in_text`] — rewrites every URL found in a message, for texts whose configured URL is not at hand.
//! - [`redact_urls_in_text`] — keeps the scheme, host and port of every URL found in a text and marks the rest
//!   (the redacted RPC wire dump).
//! - [`electrum_host_port`] — the host (and port) of one Electrum server address.
//! - [`electrum_server_name`] — the `host:port` form in which a text names an Electrum server.
//! - [`lightwalletd_endpoint_url`] / [`lightwalletd_endpoint_label`] — the URL a lightwalletd entry connects with,
//!   and the only form in which a text names it.
//! - [`UNNAMED_NODE`] / [`UNNAMED_SERVER`] — what stands in for an address that cannot be parsed.
//! - [`SECRET_SET`] — what a debug form shows in place of a configured secret.
//! - [`ServiceUrl`] — a configured service URL whose debug form is the host-only form.
//!
//! # Invariants
//!
//! - Pure functions; no I/O and no logging.
//! - An input that is not an absolute URL with a host is never echoed: it renders as [`UNNAMED_NODE`].

use http::Uri;
/// [`host_only`] and [`scrub_node_url`] are defined in `mm2_net`, which the shared HTTP layer names hosts with, and
/// shared from here, together with [`UNNAMED_NODE`], the text that stands in for a node URL that has no scheme or
/// no host, so that the raw string is never echoed.
pub use mm2_net::host_label::{host_only, scrub_node_url, UNNAMED_NODE};
use serde::Deserialize;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// What a debug form shows in place of a configured secret (a password, an API key, a header value): the fact that
/// it is set, never a part of it.
pub const SECRET_SET: &str = "<set>";

/// Where a node stands (CRD ch.35 R35.1.16): the ticker of the coin whose activation request supplied the node
/// list, the node's 1-based position in that list **as the request gave it**, and the length of that list.
///
/// It travels with the node URL (see [`ListedNode`]) so that a reordering of the list, such as the shuffle at
/// activation (R35.1.7), cannot change what a log line says. Every log line that names an EVM node is built by
/// [`NodeSlot::line`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSlot {
    ticker: Arc<str>,
    position: usize,
    of: usize,
}

impl NodeSlot {
    /// `[<ticker>] <before><host> (node <i> of <n>)<after>`, `host` being the R35.1.14 host-only form.
    pub fn line(&self, before: &str, host: &str, after: &str) -> String {
        format!(
            "[{}] {before}{host} (node {} of {}){after}",
            self.ticker, self.position, self.of
        )
    }

    #[cfg(test)]
    pub(crate) fn position(&self) -> usize { self.position }
}

/// A configured node URL together with its [`NodeSlot`].
#[derive(Clone, Debug)]
pub struct ListedNode {
    pub url: String,
    pub slot: NodeSlot,
}

/// The nodes of a coin's activation request in the order the request gave them: node `i` of `n` is at position
/// `i` (1-based) and keeps it wherever the returned vector is shuffled or sliced afterwards.
pub fn listed_nodes(ticker: &str, urls: Vec<String>) -> Vec<ListedNode> {
    let ticker: Arc<str> = Arc::from(ticker);
    let of = urls.len();
    urls.into_iter()
        .enumerate()
        .map(|(i, url)| ListedNode {
            url,
            slot: NodeSlot {
                ticker: ticker.clone(),
                position: i + 1,
                of,
            },
        })
        .collect()
}

/// A configured service URL (for example a gas-station endpoint). Its debug form is the host-only form, so a
/// request or a parameter object that holds it can derive `Debug` without printing a key (CRD ch.35 R35.1.17).
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ServiceUrl(String);

impl ServiceUrl {
    /// The configured URL. It is a credential: use it to send requests, never to name the service.
    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Debug for ServiceUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&host_only(&self.0)) }
}

/// The URL the client connects a configured lightwalletd entry with: an entry without a scheme takes `https://`
/// (CRD ch.39 R39.11.7b).
pub fn lightwalletd_endpoint_url(server: &str) -> String {
    if server.starts_with("http://") || server.starts_with("https://") {
        server.to_owned()
    } else {
        format!("https://{}", server)
    }
}

/// Stands in for a configured lightwalletd entry that cannot be parsed into a scheme and a host.
const UNNAMED_LIGHTWALLETD_ENDPOINT: &str = "<invalid lightwalletd endpoint>";

/// The only form in which a log line, error text or status message names a configured lightwalletd endpoint
/// (CRD ch.39 R39.11.29): scheme, host and port of the URL the client connects with, so an entry without a scheme
/// takes the scheme the client would use. An entry that cannot be parsed is named by a fixed placeholder and its
/// `position` (counted from 0) in the configured list, never by its raw string.
pub fn lightwalletd_endpoint_label(server: &str, position: usize) -> String {
    let name = host_only(&lightwalletd_endpoint_url(server));
    if name == UNNAMED_NODE {
        format!("{} #{}", UNNAMED_LIGHTWALLETD_ENDPOINT, position + 1)
    } else {
        name
    }
}

/// Stands in for an Electrum server address that cannot be reduced to a host and a port.
pub const UNNAMED_SERVER: &str = "<invalid server address>";

/// Renders an Electrum server address as `host:port` (or `host` when it names no port; IPv6 hosts keep their
/// brackets). A scheme, the userinfo, a path, a query and a fragment are dropped (CRD ch.38 R38.6.9a). `None`
/// when no host can be read, so that the raw string is never echoed.
pub fn electrum_host_port(addr: &str) -> Option<String> {
    let rest = addr.split_once("://").map_or(addr, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host_port)| host_port);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') => (host, Some(port)),
        _ => (host_port, None),
    };
    // A bare IPv6 literal without a port has colons in `host` and is not a `host:port` address.
    let bracketed = host.starts_with('[') && host.ends_with(']');
    if host.is_empty() || (!bracketed && (host.contains(':') || host.contains('['))) {
        return None;
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'))
    {
        return None;
    }
    match port {
        Some(port) if port.parse::<u16>().is_ok() => Some(format!("{host}:{port}")),
        Some(_) => None,
        None => Some(host.to_owned()),
    }
}

/// The form in which a text names an Electrum server: `host:port`. `None` when the address names no host or no
/// port, so that a bare word, which may be a secret, is never echoed (CRD ch.38 R38.6.9a).
pub fn electrum_server_name(addr: &str) -> Option<String> {
    electrum_host_port(addr).filter(|name| {
        name.rsplit_once(':')
            .map_or(false, |(_, port)| port.parse::<u16>().is_ok())
    })
}

/// Rewrites `text` so that every URL in it (`scheme://...` up to the next delimiter) is replaced by its
/// host-only form. For lower-layer texts whose configured URL is not at hand; a URL without a usable host is
/// replaced by [`UNNAMED_NODE`].
pub fn scrub_urls_in_text(text: &str) -> String { map_urls_in_text(text, host_only) }

/// Stands in for the userinfo, path, query and fragment of a URL in [`redact_urls_in_text`].
pub const REDACTED_URL_PART: &str = "<redacted>";

/// Rewrites `text` so that every URL with a scheme and a host in it keeps its scheme, host and port, and its
/// userinfo, path, query and fragment are each replaced by [`REDACTED_URL_PART`], for example
/// `https://<redacted>@host:8545/<redacted>?<redacted>`. A path of `/` alone is kept. Everything outside a URL, and a URL without a host, is
/// left as it is (CRD ch.35, the rule after R35.1.17, D35.10).
pub fn redact_urls_in_text(text: &str) -> String { map_urls_in_text(text, redact_url_parts) }

fn redact_url_parts(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => (Some(userinfo), host_port),
        None => (None, authority),
    };
    if host_port.is_empty() || host_port.starts_with(':') {
        return url.to_owned();
    }
    let (path_and_query, fragment) = match tail.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (tail, None),
    };
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };
    let mut out = format!("{scheme}://");
    if userinfo.is_some() {
        out.push_str(REDACTED_URL_PART);
        out.push('@');
    }
    out.push_str(host_port);
    match path {
        "" | "/" => out.push_str(path),
        _ => {
            out.push('/');
            out.push_str(REDACTED_URL_PART);
        },
    }
    if query.is_some() {
        out.push('?');
        out.push_str(REDACTED_URL_PART);
    }
    if fragment.is_some() {
        out.push('#');
        out.push_str(REDACTED_URL_PART);
    }
    out
}

/// Replaces every `scheme://...` token of `text` (up to the next delimiter, trailing punctuation excluded) by
/// what `rewrite` makes of it.
fn map_urls_in_text(text: &str, rewrite: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let scheme_start = rest[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map_or(0, |i| i + 1);
        let url_end = rest[at + 3..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '<' | '>' | '(' | ')' | '{' | '}' | ',')
            })
            .map_or(rest.len(), |i| at + 3 + i);
        let token = rest[scheme_start..url_end].trim_end_matches(['.', ':', ';']);
        out.push_str(&rest[..scheme_start]);
        out.push_str(&rewrite(token));
        rest = &rest[scheme_start + token.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T35.34: the exact shape of a node log line.
    #[test]
    fn t35_34_a_node_line_starts_with_the_ticker_and_carries_the_list_position_after_the_host() {
        let nodes = listed_nodes("ETC", vec![
            "https://rpc.example.org/v3/KEY1".to_owned(),
            "https://rpc.example.org/v3/KEY2".to_owned(),
            "https://rpc.example.org/v3/KEY3".to_owned(),
            "https://rpc.example.org/v3/KEY4".to_owned(),
        ]);
        let host = host_only(&nodes[1].url);
        assert_eq!(
            nodes[1]
                .slot
                .line("Couldn't get client version for node ", &host, ", no node answered"),
            "[ETC] Couldn't get client version for node https://rpc.example.org (node 2 of 4), no node answered"
        );
        assert_eq!(
            nodes[1]
                .slot
                .line("EVM RPC ", &host, " answered eth_call with error 3: reverted"),
            "[ETC] EVM RPC https://rpc.example.org (node 2 of 4) answered eth_call with error 3: reverted"
        );
    }

    /// T35.34: two nodes of one coin on one host differ in the position, two coins on one host differ in the
    /// ticker.
    #[test]
    fn t35_34_same_host_different_paths_are_told_apart_by_position_and_by_ticker() {
        let urls = vec![
            "https://rpc.swiftnodes.io/etc/KEY".to_owned(),
            "https://rpc.swiftnodes.io/xpl/KEY".to_owned(),
        ];
        let etc = listed_nodes("ETC", urls.clone());
        let xpl = listed_nodes("XPL", urls);
        let host = host_only(&etc[0].url);
        assert_eq!(host, host_only(&etc[1].url));
        let line = |node: &ListedNode| node.slot.line("node ", &host, "");
        assert_ne!(line(&etc[0]), line(&etc[1]));
        assert_ne!(line(&etc[0]), line(&xpl[0]));
        assert_eq!(line(&etc[1]), "[ETC] node https://rpc.swiftnodes.io (node 2 of 2)");
        assert_eq!(line(&xpl[1]), "[XPL] node https://rpc.swiftnodes.io (node 2 of 2)");
    }

    #[test]
    fn listed_nodes_numbers_the_list_as_given_from_one() {
        let nodes = listed_nodes("ETC", vec![
            "http://a".to_owned(),
            "http://b".to_owned(),
            "http://c".to_owned(),
        ]);
        assert_eq!(nodes.iter().map(|n| n.slot.position()).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(nodes[2].url, "http://c");
    }

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
                assert!(!scrubbed.contains(secret), "a credential part is rendered");
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

    #[test]
    fn electrum_host_port_keeps_only_host_and_port() {
        for (addr, expected) in [
            ("electrum.example.org:50002", Some("electrum.example.org:50002")),
            (
                "user:secret@electrum.example.org:50002",
                Some("electrum.example.org:50002"),
            ),
            ("user@electrum.example.org:50002", Some("electrum.example.org:50002")),
            (
                "wss://electrum.example.org:443/ws?token=SECRET",
                Some("electrum.example.org:443"),
            ),
            ("electrum.example.org/ws/SECRET", Some("electrum.example.org")),
            ("[::1]:50002", Some("[::1]:50002")),
            ("tcp://[2001:db8::1]:50001/x", Some("[2001:db8::1]:50001")),
        ] {
            assert_eq!(
                electrum_host_port(addr).as_deref(),
                expected,
                "an address is reduced wrongly"
            );
        }
    }

    #[test]
    fn electrum_host_port_never_echoes_an_address_without_a_host() {
        for addr in [
            "",
            "user:secret",
            "a:b:c",
            ":50002",
            "host:notaport",
            "@",
            "ho st:1",
            "::1",
            "[::1",
        ] {
            assert_eq!(electrum_host_port(addr), None, "an address without a host is named");
        }
    }

    #[test]
    fn scrub_urls_in_text_reduces_every_url_to_its_host_only_form() {
        let text = "error sending request for url (https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X): \
                    dns error; retry http://127.0.0.1:1/wallet/api, then https://[::1]:8545/key/SECRET.";
        let scrubbed = scrub_urls_in_text(text);
        assert_eq!(
            scrubbed,
            "error sending request for url (https://rpc.example.org:8545): dns error; retry http://127.0.0.1:1, \
             then https://[::1]:8545."
        );
        for secret in ["user", "secret", "APIKEY", "token=X", "wallet/api", "SECRET"] {
            assert!(!scrubbed.contains(secret), "a credential part survives the scrubbing");
        }
        assert_eq!(scrub_urls_in_text("no url here"), "no url here");
        assert_eq!(scrub_urls_in_text("scheme:// only"), format!("{UNNAMED_NODE} only"));
    }

    #[test]
    fn redact_urls_in_text_keeps_scheme_host_and_port_and_marks_each_removed_part() {
        for (input, expected) in [
            (
                "https://user:pw@rpc.example.org:8545/v3/KEY?token=X#frag",
                "https://<redacted>@rpc.example.org:8545/<redacted>?<redacted>#<redacted>",
            ),
            ("https://rpc.example.org/v3/KEY", "https://rpc.example.org/<redacted>"),
            ("https://rpc.example.org:8545/", "https://rpc.example.org:8545/"),
            ("https://rpc.example.org:8545", "https://rpc.example.org:8545"),
            ("https://rpc.example.org/?k=v", "https://rpc.example.org/?<redacted>"),
            ("wss://[::1]:8546/ws/KEY", "wss://[::1]:8546/<redacted>"),
            ("electrum.example.org:50002", "electrum.example.org:50002"),
            ("scheme:// only", "scheme:// only"),
        ] {
            assert_eq!(redact_urls_in_text(input), expected);
        }
    }

    #[test]
    fn redact_urls_in_text_rewrites_a_url_inside_a_longer_text_only() {
        let text = "error sending request for url (https://rpc.example.org:8545/v3/KEY?token=X): dns error; \
                    retry http://127.0.0.1:1/ then.";
        assert_eq!(
            redact_urls_in_text(text),
            "error sending request for url (https://rpc.example.org:8545/<redacted>?<redacted>): dns error; \
             retry http://127.0.0.1:1/ then."
        );
        assert_eq!(redact_urls_in_text("no url here"), "no url here");
    }

    #[test]
    fn a_service_url_debugs_as_its_host_only_form() {
        let url: ServiceUrl = serde_json::from_str("\"https://gas.example.org:8443/v3/APIKEY?key=X\"").unwrap();
        assert_eq!(format!("{url:?}"), "https://gas.example.org:8443");
        assert_eq!(url.as_str(), "https://gas.example.org:8443/v3/APIKEY?key=X");
    }

    #[test]
    fn a_lightwalletd_entry_takes_the_scheme_the_client_uses() {
        assert_eq!(
            lightwalletd_endpoint_label("lightd.example.org:9067", 0),
            "https://lightd.example.org:9067"
        );
        assert_eq!(
            lightwalletd_endpoint_label("http://user:secret@lightd.example.org:9067/path?q=SECRET", 0),
            "http://lightd.example.org:9067"
        );
        let unnamed = lightwalletd_endpoint_label("not a url SECRET", 2);
        assert_eq!(unnamed, "<invalid lightwalletd endpoint> #3");
        assert!(!unnamed.contains("SECRET"), "the raw entry is echoed");
    }
}
