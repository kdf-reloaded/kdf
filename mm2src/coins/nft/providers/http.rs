//! Thin HTTP layer used by the NFT providers.
//!
//! Wraps `mm2_net::transport` so the providers module never has to import
//! `mm2_net` directly and so that future authentication paths (proxy
//! signing, signed bearer headers, …) have one obvious place to live.

use derive_more::Display;
use http::StatusCode;
use mm2_err_handle::prelude::*;
use mm2_net::host_label::host_only;
use mm2_net::transport::{slurp_url_with_headers, SlurpError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as Json;

/// Errors produced by the providers HTTP layer. A `uri` field holds the host-only form of the request URL, which
/// can carry a provider key in its path or query (CRD ch.35 R35.1.17).
#[derive(Debug, Display, Serialize)]
pub enum FetchError {
    /// Underlying HTTP transport returned an error.
    #[display(fmt = "HTTP transport error fetching '{}': {}", uri, error)]
    Transport { uri: String, error: String },
    /// Server returned a non-2xx status code.
    #[display(fmt = "HTTP {} response from '{}'", status, uri)]
    HttpStatus { uri: String, status: u16 },
    /// Response body was not valid JSON or did not match the expected shape.
    #[display(fmt = "Could not deserialize response from '{}': {}", uri, error)]
    Deserialize { uri: String, error: String },
}

impl From<SlurpError> for FetchError {
    fn from(err: SlurpError) -> Self {
        match err {
            SlurpError::Timeout { uri, error }
            | SlurpError::Transport { uri, error }
            | SlurpError::ErrorDeserializing { uri, error } => FetchError::Transport { uri, error },
            SlurpError::InvalidRequest(msg) | SlurpError::Internal(msg) => FetchError::Transport {
                uri: String::new(),
                error: msg,
            },
        }
    }
}

/// Issue a GET request to `uri` with the supplied `headers`, treat any
/// non-2xx status code as an error and parse the body as JSON.
///
/// The result is generic over the destination type so the same helper
/// is usable both for fully-typed schemas (in unit tests) and for raw
/// `serde_json::Value` (when a provider response shape is dynamic).
pub async fn fetch_json<T>(uri: &str, headers: &[(&str, &str)]) -> MmResult<T, FetchError>
where
    T: DeserializeOwned + Send + 'static,
{
    let owned = headers.to_vec();
    let (status, _hdrs, body) = slurp_url_with_headers(uri, owned).await.mm_err(FetchError::from)?;
    if !status.is_success() {
        return MmError::err(FetchError::HttpStatus {
            uri: host_only(uri),
            status: status.as_u16(),
        });
    }
    serde_json::from_slice(&body).map_to_mm(|err| FetchError::Deserialize {
        uri: host_only(uri),
        error: err.to_string(),
    })
}

/// Convenience helper that issues a GET to `uri` and returns the response
/// as a `serde_json::Value`. Useful when the caller intends to walk the
/// payload manually (e.g. to follow a `cursor` field for pagination).
pub async fn fetch_value(uri: &str, headers: &[(&str, &str)]) -> MmResult<Json, FetchError> {
    fetch_json(uri, headers).await
}

// Re-exporting the StatusCode helper here so callers can quickly classify
// a returned `FetchError::HttpStatus` without importing `http` directly.
impl FetchError {
    /// Best-effort classification of an [`FetchError::HttpStatus`] payload
    /// into a [`StatusCode`]. Returns `None` for transport/deserialize
    /// variants and for status codes outside the 100..1000 range.
    pub fn status_code(&self) -> Option<StatusCode> {
        match self {
            FetchError::HttpStatus { status, .. } => StatusCode::from_u16(*status).ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_error_is_classifiable() {
        let err = FetchError::HttpStatus {
            uri: "https://example.com".into(),
            status: 404,
        };
        assert_eq!(err.status_code(), Some(StatusCode::NOT_FOUND));
    }

    #[test]
    fn transport_error_has_no_status() {
        let err = FetchError::Transport {
            uri: "https://example.com".into(),
            error: "boom".into(),
        };
        assert!(err.status_code().is_none());
    }

    #[test]
    fn slurp_transport_maps_to_fetch_transport() {
        let slurp_err = SlurpError::Transport {
            uri: "https://example.com".into(),
            error: "connection refused".into(),
        };
        let fetched = FetchError::from(slurp_err);
        match fetched {
            FetchError::Transport { uri, error } => {
                assert_eq!(uri, "https://example.com");
                assert!(error.contains("connection refused"));
            },
            _ => panic!("expected Transport variant"),
        }
    }

    #[test]
    fn slurp_internal_maps_to_transport_with_empty_uri() {
        let fetched = FetchError::from(SlurpError::Internal("kaboom".into()));
        match fetched {
            FetchError::Transport { uri, error } => {
                assert!(uri.is_empty());
                assert_eq!(error, "kaboom");
            },
            _ => panic!("expected Transport variant"),
        }
    }

    const SECRET_PARTS: [&str; 4] = ["bob", "hunter2", "APIKEY", "token=X"];

    fn assert_host_only(err: &FetchError, host: &str) {
        let text = format!("{err} {err:?}");
        assert!(
            text.contains(host),
            "the host-only form is missing from the rendered error"
        );
        for (i, secret) in SECRET_PARTS.iter().enumerate() {
            assert!(!text.contains(secret), "secret part {i} leaked into the rendered error");
        }
    }

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

    fn secret_url(port: u16) -> String { format!("http://bob:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X") }

    #[test]
    fn fetch_errors_name_the_provider_host_only() {
        use common::block_on;

        // Transport: nothing listens on the port.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = block_on(fetch_json::<Json>(&secret_url(port), &[])).unwrap_err();
        assert!(
            matches!(err.get_inner(), FetchError::Transport { .. }),
            "the error is not a transport error"
        );
        assert_host_only(err.get_inner(), &format!("http://127.0.0.1:{port}"));

        // HttpStatus.
        let port = serve_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let err = block_on(fetch_json::<Json>(&secret_url(port), &[])).unwrap_err();
        assert!(
            matches!(err.get_inner(), FetchError::HttpStatus { status: 404, .. }),
            "the error is not a 404 status error"
        );
        assert_host_only(err.get_inner(), &format!("http://127.0.0.1:{port}"));

        // Deserialize.
        let port = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json");
        let err = block_on(fetch_json::<Json>(&secret_url(port), &[])).unwrap_err();
        assert!(
            matches!(err.get_inner(), FetchError::Deserialize { .. }),
            "the error is not a deserialize error"
        );
        assert_host_only(err.get_inner(), &format!("http://127.0.0.1:{port}"));
    }
}
