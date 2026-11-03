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
///
/// A lower layer may also repeat the URL in a normalized spelling (a browser does): percent-encoded, in another
/// letter case, with a default port added or dropped, or with a trailing slash added (CRD ch.35 R35.1.14). The
/// secret parts are therefore also removed in their percent-encoded spelling (either hex case, and any mix of
/// encoded and literal characters), and every `http`, `https`, `ws` or `wss` URL in `text` whose host equals
/// the host of `url` (ignoring letter case) is replaced by its own host-only form (its scheme, host and port as
/// the lower layer wrote them), or by the host-only form of `url` when it has none. Sentence punctuation after
/// a URL (`.`, `:`, `!`, `?`) is kept. A URL of another host is left as it is.
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
    // The spellings a lower layer may normalize the URL into.
    if let Some(userinfo) = uri
        .authority()
        .and_then(|a| a.as_str().rsplit_once('@'))
        .map(|(u, _)| u)
    {
        out = remove_percent_tolerant(&out, &format!("{userinfo}@"));
    }
    if let Some(path_and_query) = uri.path_and_query().map(|p| p.as_str()) {
        if path_and_query.len() > 1 {
            out = remove_percent_tolerant(&out, path_and_query);
        }
    }
    if label != UNNAMED_NODE {
        if let Some(host) = uri.host() {
            out = replace_same_host_urls(&out, host, &label);
        }
    }
    out
}

/// The value of one hex digit.
fn hex_value(digit: u8) -> Option<u8> { (digit as char).to_digit(16).map(|v| v as u8) }

/// Removes every occurrence of `secret` from `text`, where each byte of `secret` may stand in `text` as itself or
/// as its percent-encoded form (`%XX`, either hex case). A `secret` that is empty removes nothing.
fn remove_percent_tolerant(text: &str, secret: &str) -> String {
    let (text_bytes, secret_bytes) = (text.as_bytes(), secret.as_bytes());
    if secret_bytes.is_empty() {
        return text.to_owned();
    }
    // The length of the match starting at `start`, if there is one.
    let match_len = |start: usize| -> Option<usize> {
        let mut at = start;
        for &wanted in secret_bytes {
            let &b = text_bytes.get(at)?;
            let encoded = match (b, text_bytes.get(at + 1), text_bytes.get(at + 2)) {
                (b'%', Some(&hi), Some(&lo)) => hex_value(hi).zip(hex_value(lo)).map(|(hi, lo)| hi << 4 | lo),
                _ => None,
            };
            if encoded == Some(wanted) {
                at += 3;
            } else if b == wanted {
                at += 1;
            } else {
                return None;
            }
        }
        Some(at - start)
    };
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while at < text_bytes.len() {
        match match_len(at) {
            Some(len) => {
                out.push_str(&text[copied..at]);
                at += len;
                copied = at;
            },
            None => at += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Whether `c` ends a URL run in a message: whitespace, a quote, or one of `<>)],;`.
fn ends_url_run(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | ')' | ']' | ',' | ';')
}

/// The host of an `http`, `https`, `ws` or `wss` URL run (the text after `://`), brackets of an IPv6 host kept,
/// and the length of the run. Sentence punctuation that trails the run (`.`, `:`, `!`, `?`) is not part of it.
fn url_run(after_scheme: &str) -> (&str, usize) {
    let (mut in_authority, mut in_brackets) = (true, false);
    let mut authority_end = None;
    let mut run_len = after_scheme.len();
    for (i, c) in after_scheme.char_indices() {
        if in_brackets && c == ']' {
            in_brackets = false;
        } else if ends_url_run(c) {
            run_len = i;
            break;
        } else if in_authority && c == '[' {
            in_brackets = true;
        } else if in_authority && matches!(c, '/' | '?' | '#') {
            in_authority = false;
            authority_end = Some(i);
        }
    }
    run_len = after_scheme[..run_len].trim_end_matches(['.', ':', '!', '?']).len();
    let authority = &after_scheme[..authority_end.unwrap_or(run_len).min(run_len)];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host_port)| host_port);
    let host = if host_port.starts_with('[') {
        host_port.find(']').map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    (host, run_len)
}

/// Replaces every URL run in `text` whose scheme is `http`, `https`, `ws` or `wss` (any letter case) and whose host
/// equals `host` (ignoring letter case) by its own host-only form (its scheme, host and port as written), or by
/// `label` when the run gives no usable host-only form. The run ends before whitespace, a quote, or one of
/// `<>)],;` (a `]` inside the brackets of an IPv6 host does not end it), and trailing `.`, `:`, `!` and `?` stay
/// outside it. A run of another host is kept as it is.
fn replace_same_host_urls(text: &str, host: &str, label: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut search_from) = (0, 0);
    while let Some(found) = lower[search_from..].find("://") {
        let colon = search_from + found;
        let after_scheme = colon + 3;
        let scheme_len = ["https", "http", "wss", "ws"]
            .iter()
            .find(|scheme| lower[..colon].ends_with(**scheme))
            .map(|scheme| scheme.len());
        let Some(scheme_len) = scheme_len else {
            search_from = after_scheme;
            continue;
        };
        let start = colon - scheme_len;
        let (run_host, run_len) = url_run(&text[after_scheme..]);
        if run_host.eq_ignore_ascii_case(host) && start >= copied {
            out.push_str(&text[copied..start]);
            let run_label = host_only(&text[start..after_scheme + run_len]);
            out.push_str(if run_label == UNNAMED_NODE { label } else { &run_label });
            copied = after_scheme + run_len;
            search_from = copied;
        } else {
            // The run may hold another URL (in its query), so the search goes on inside it.
            search_from = after_scheme;
        }
    }
    out.push_str(&text[copied..]);
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

    const URL: &str = "https://bob:hunter2@node.example:8545/v3/APIKEY?token=X";
    const LABEL: &str = "https://node.example:8545";

    /// RFC 3986 percent-encoding of everything that is not unreserved, in the given hex case.
    fn percent_encode(text: &str, upper: bool) -> String {
        let mut out = String::new();
        for &b in text.as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(b as char);
            } else if upper {
                out.push_str(&format!("%{b:02X}"));
            } else {
                out.push_str(&format!("%{b:02x}"));
            }
        }
        out
    }

    /// The scrubbed text must equal `expected` and carry no part of the secrets in any letter case.
    fn assert_scrubbed(text: &str, url: &str, expected: &str) {
        let scrubbed = scrub_node_url(text, url);
        assert!(scrubbed == expected, "the scrubbed text differs from the expected one");
        let lower = scrubbed.to_ascii_lowercase();
        for (i, secret) in SECRETS.iter().enumerate() {
            assert!(
                !lower.contains(&secret.to_ascii_lowercase()),
                "secret part {i} survives the scrubbing"
            );
        }
    }

    #[test]
    fn a_percent_encoded_url_loses_its_secret_parts() {
        for (upper, expected) in [
            (true, "cannot parse https%3A%2F%2Fnode.example%3A8545 here"),
            (false, "cannot parse https%3a%2f%2fnode.example%3a8545 here"),
        ] {
            let encoded = percent_encode(URL, upper);
            assert_scrubbed(&format!("cannot parse {encoded} here"), URL, expected);
        }
        // The browser case, spelled out: every reserved character encoded in upper case.
        let spelled = "https%3A%2F%2Fbob%3Ahunter2%40node.example%3A8545%2Fv3%2FAPIKEY%3Ftoken%3DX";
        assert_scrubbed(spelled, URL, "https%3A%2F%2Fnode.example%3A8545");
        // Mixed hex case and a partly encoded spelling.
        assert_scrubbed(
            "https://bob%3Ahunter2%40node.example:8545%2Fv3/APIKEY%3ftoken%3dX",
            URL,
            LABEL,
        );
        // A percent sign in a secret is encoded as `%25`.
        let url = "https://node.example/key%41/PA%SS?k=1";
        assert!(scrub_node_url("see %2Fkey%2541%2FPA%25SS%3Fk%3D1 and /key%41/PA%SS?k=1", url) == "see  and ");
    }

    #[test]
    fn a_url_in_another_letter_case_is_replaced() {
        assert_scrubbed(
            "failed: HTTPS://Node.Example:8545/V3/apikey?TOKEN=x done",
            URL,
            "failed: https://Node.Example:8545 done",
        );
        assert_scrubbed(
            "failed: HtTp://NODE.EXAMPLE:8545 done",
            "http://node.example:8545/v3/APIKEY",
            "failed: http://NODE.EXAMPLE:8545 done",
        );
    }

    #[test]
    fn a_default_port_added_or_dropped_is_followed() {
        let url = "https://bob:hunter2@node.example/v3/APIKEY?token=X";
        assert_scrubbed(
            "failed: https://node.example:443/v3/APIKEY?token=X",
            url,
            "failed: https://node.example:443",
        );
        assert_scrubbed(
            "failed: https://node.example/v3/APIKEY?token=X",
            url,
            "failed: https://node.example",
        );
        let url = "https://bob:hunter2@node.example:443/v3/APIKEY?token=X";
        assert_scrubbed(
            "failed: https://node.example/v3/APIKEY?token=X",
            url,
            "failed: https://node.example",
        );
        let url = "ws://bob:hunter2@node.example:80/v3/APIKEY?token=X";
        assert_scrubbed(
            "failed: ws://node.example/v3/APIKEY?token=X",
            url,
            "failed: ws://node.example",
        );
        let url = "wss://bob:hunter2@node.example/v3/APIKEY?token=X";
        assert_scrubbed(
            "failed: wss://node.example:443/v3/APIKEY?token=X",
            url,
            "failed: wss://node.example:443",
        );
    }

    #[test]
    fn a_trailing_slash_added_is_followed() {
        let url = "https://node.example:8545";
        assert_scrubbed(
            "failed: https://node.example:8545/ here",
            url,
            "failed: https://node.example:8545 here",
        );
        let url = "https://bob:hunter2@node.example:8545";
        assert_scrubbed(
            "failed: https://node.example:8545/ here",
            url,
            "failed: https://node.example:8545 here",
        );
        let url = "https://node.example:8545/v3/APIKEY?token=X";
        assert_scrubbed(
            "failed: https://node.example:8545/v3/APIKEY/?token=X here",
            url,
            "failed: https://node.example:8545 here",
        );
    }

    #[test]
    fn a_run_ends_at_whitespace_a_quote_or_a_delimiter() {
        for (text, expected) in [
            ("(https://Node.Example/v3/APIKEY?token=X)", "(https://Node.Example)"),
            ("\"https://node.example/APIKEY\" and", "\"https://node.example\" and"),
            ("<https://node.example/APIKEY>, then", "<https://node.example>, then"),
            ("[https://node.example/APIKEY]; next", "[https://node.example]; next"),
            ("a https://node.example/APIKEY\tb", "a https://node.example\tb"),
        ] {
            assert_scrubbed(text, URL, expected);
        }
        let url = "http://[::1]:8545/key/APIKEY";
        assert!(scrub_node_url("at [http://[::1]/key/other] x", url) == "at [http://[::1]] x");
        assert!(scrub_node_url("at HTTP://[::1]:80/ x", url) == "at http://[::1]:80 x");
    }

    #[test]
    fn a_same_host_run_gets_its_own_host_only_form() {
        // A different scheme, a different port, a default port written out, and the host in another letter case:
        // the run keeps its scheme, host and port as written, and loses everything else.
        for (text, expected) in [
            ("x wss://Node.Example:8443/v3/APIKEY y", "x wss://Node.Example:8443 y"),
            (
                "x http://node.example:9000/APIKEY?k=v y",
                "x http://node.example:9000 y",
            ),
            ("x https://node.example:443/APIKEY y", "x https://node.example:443 y"),
            ("x https://NODE.example/APIKEY y", "x https://NODE.example y"),
            ("x ws://bob:other@node.example/APIKEY y", "x ws://node.example y"),
        ] {
            assert_scrubbed(text, URL, expected);
        }
        // The configured string itself still renders as the label of the configured URL.
        assert_scrubbed(&format!("x {URL} y"), URL, &format!("x {LABEL} y"));
        // A run that gives no host-only form falls back to the label of the configured URL.
        assert_scrubbed(
            "see https://node.example:1:2/APIKEY here",
            URL,
            &format!("see {LABEL} here"),
        );
    }

    #[test]
    fn trailing_sentence_punctuation_stays_outside_the_run() {
        for tail in [".", ":", "!", "?", "?.", "...", ".:!?"] {
            assert_scrubbed(
                &format!("connect failed: https://node.example:9000/v3/apikey{tail}"),
                URL,
                &format!("connect failed: https://node.example:9000{tail}"),
            );
        }
        for (text, expected) in [
            (
                "(see https://node.example/v3/APIKEY). Next",
                "(see https://node.example). Next",
            ),
            ("(https://node.example/v3/apikey.)", "(https://node.example.)"),
            ("is https://node.example: down", "is https://node.example: down"),
            (
                "is https://node.example:8545! down",
                "is https://node.example:8545! down",
            ),
        ] {
            assert_scrubbed(text, URL, expected);
        }
    }

    #[test]
    fn a_url_of_another_host_is_kept() {
        let other = "https://other.example:8545/v3/other?k=v and wss://node.example.org/x and https://xnode.example/y";
        assert!(scrub_node_url(other, URL) == other);
        let mixed = format!("{URL} then {other}");
        assert!(scrub_node_url(&mixed, URL) == format!("{LABEL} then {other}"));
        // A URL of the same host inside the query of a URL of another host is still found.
        assert_scrubbed(
            "https://other.example/?u=https://node.example/APIKEY",
            URL,
            "https://other.example/?u=https://node.example",
        );
        // Text without a URL, and an unrelated percent sign, stay as they are.
        assert!(scrub_node_url("100% sure, no url ://", URL) == "100% sure, no url ://");
    }

    /// A browser repeats the URL percent-encoded; the wasm fetch path scrubs its lower-layer texts with this
    /// function (CRD ch.35 R35.1.14).
    #[cfg(target_arch = "wasm32")]
    mod wasm {
        use super::*;
        use wasm_bindgen_test::*;

        wasm_bindgen_test_configure!(run_in_browser);

        #[wasm_bindgen_test]
        fn a_browser_text_that_repeats_the_url_percent_encoded_loses_it() {
            let text = format!("Failed to fetch {} (TypeError)", percent_encode(URL, true));
            assert_scrubbed(
                &text,
                URL,
                "Failed to fetch https%3A%2F%2Fnode.example%3A8545 (TypeError)",
            );
            let text = format!("Failed to fetch {} (TypeError)", percent_encode(URL, false));
            assert_scrubbed(
                &text,
                URL,
                "Failed to fetch https%3A%2F%2Fnode.example%3A8545 (TypeError)",
            );
            assert_scrubbed(
                "Failed to fetch HTTPS://NODE.EXAMPLE/V3/apikey",
                URL,
                "Failed to fetch https://NODE.EXAMPLE",
            );
        }
    }
}
