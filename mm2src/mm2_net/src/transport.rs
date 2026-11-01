use crate::host_label::host_only;
use derive_more::Display;
use http::{HeaderMap, StatusCode};
use mm2_err_handle::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub use crate::native_http::{slurp_post_json, slurp_req, slurp_req_bounded, slurp_url, slurp_url_with_headers};

#[cfg(target_arch = "wasm32")]
pub use crate::wasm_http::{slurp_post_json, slurp_url, slurp_url_with_headers};

pub type SlurpResult = Result<(StatusCode, HeaderMap, Vec<u8>), MmError<SlurpError>>;

/// The error of an HTTP request. Its `uri` field holds the **host-only** form of the request URI
/// (`scheme://host[:port]`, [`crate::host_label::host_only`]), never the URI itself: a URI can carry a credential in
/// its userinfo, path or query, and this error is displayed and copied into other errors (CRD ch.35 R35.1.17).
/// Nothing may use the field for logic.
#[derive(Debug, Deserialize, Display, Serialize)]
pub enum SlurpError {
    #[display(fmt = "Error deserializing '{}' response: {}", uri, error)]
    ErrorDeserializing { uri: String, error: String },
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Request '{}' timeout: {}", uri, error)]
    Timeout { uri: String, error: String },
    #[display(fmt = "Transport '{}' error: {}", uri, error)]
    Transport { uri: String, error: String },
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

/// Send POST JSON HTTPS request and parse response
pub async fn post_json<T>(url: &str, json: String) -> Result<T, MmError<SlurpError>>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let result = slurp_post_json(url, json).await?;
    serde_json::from_slice(&result.2).map_to_mm(|e| SlurpError::ErrorDeserializing {
        uri: host_only(url),
        error: e.to_string(),
    })
}

/// Fetch URL by HTTPS and parse JSON response
pub async fn fetch_json<T>(url: &str) -> Result<T, MmError<SlurpError>>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let result = slurp_url(url).await?;
    serde_json::from_slice(&result.2).map_to_mm(|e| SlurpError::ErrorDeserializing {
        uri: host_only(url),
        error: e.to_string(),
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use common::block_on;

    const SECRET_PARTS: [&str; 5] = ["bob", "hunter2", "APIKEY", "token=X", "AAHbotTOKEN"];

    /// Serves one canned HTTP/1.1 response on a loopback port and returns the port.
    fn serve_once(response: &'static [u8]) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            stream.write_all(response).unwrap();
        });
        port
    }

    fn assert_names_the_host_only(err: &MmError<SlurpError>, host: &str) {
        for text in [err.to_string(), format!("{err:?}")] {
            assert!(
                text.contains(host),
                "the host-only form is missing from the rendered error"
            );
            for (i, secret) in SECRET_PARTS.iter().enumerate() {
                assert!(!text.contains(secret), "secret part {i} leaked into the rendered error");
            }
        }
        match err.get_inner() {
            SlurpError::ErrorDeserializing { uri, .. } => {
                assert!(uri == host, "the uri field is not the host-only form")
            },
            _ => panic!("the error is not a deserializing error"),
        }
    }

    const NOT_JSON: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json";

    #[test]
    fn post_json_names_the_host_only_when_the_answer_does_not_decode() {
        let port = serve_once(NOT_JSON);
        let url = format!("http://bob:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X");
        let err = block_on(post_json::<serde_json::Value>(&url, "{}".to_owned())).unwrap_err();
        assert_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));
    }

    #[test]
    fn fetch_json_names_the_host_only_when_the_answer_does_not_decode() {
        let port = serve_once(NOT_JSON);
        let url = format!("http://bob:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X");
        let err = block_on(fetch_json::<serde_json::Value>(&url)).unwrap_err();
        assert_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));
    }

    /// The shape of a Telegram bot URL: the bot token is a path segment.
    #[test]
    fn a_bot_token_in_the_path_never_reaches_the_error() {
        let port = serve_once(NOT_JSON);
        let url = format!("http://127.0.0.1:{port}/bot123456:AAHbotTOKEN/sendMessage");
        let err = block_on(post_json::<serde_json::Value>(&url, "{}".to_owned())).unwrap_err();
        assert_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));
    }

    #[test]
    fn an_unparsable_url_is_never_echoed() {
        let url = "bob:hunter2 /v3/APIKEY?token=X AAHbotTOKEN";
        let err = block_on(post_json::<serde_json::Value>(url, "{}".to_owned())).unwrap_err();
        let text = format!("{err} {err:?}");
        for (i, secret) in SECRET_PARTS.iter().enumerate() {
            assert!(!text.contains(secret), "secret part {i} leaked into the rendered error");
        }
    }
}
