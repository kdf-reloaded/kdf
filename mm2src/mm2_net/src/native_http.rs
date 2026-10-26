use crate::transport::{SlurpError, SlurpResult};
use common::wio::{drive03, HYPER};
use futures::channel::oneshot::Canceled;
use http::{header, HeaderValue, Request};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Error as ClientError;
use mm2_err_handle::prelude::*;
use std::error::Error as StdError;
use std::time::Duration;

impl From<Canceled> for SlurpError {
    fn from(_: Canceled) -> Self { SlurpError::Internal("Spawned Slurp future has been canceled".to_owned()) }
}

impl SlurpError {
    /// Classifies a `hyper` error. `hyper-util`'s legacy client wraps the `hyper::Error` raised by the
    /// connection in its own error type, so the source chain is searched for it.
    fn from_error(e: &(dyn StdError + 'static), uri: String) -> SlurpError {
        // `hyper-util`'s client error displays only its kind ("client error (Connect)"), the cause is in the
        // source chain, so the whole chain goes into the message.
        let mut error = e.to_string();
        let mut hyper_err = None;
        let mut source = e.source();
        if let Some(err) = e.downcast_ref::<hyper::Error>() {
            hyper_err = Some(err);
        }
        while let Some(err) = source {
            error.push_str(": ");
            error.push_str(&err.to_string());
            if hyper_err.is_none() {
                hyper_err = err.downcast_ref::<hyper::Error>();
            }
            source = err.source();
        }
        match hyper_err {
            Some(hyper_err) => Self::from_hyper_error(hyper_err, uri, error),
            None => SlurpError::Transport { uri, error },
        }
    }

    fn from_hyper_error(e: &hyper::Error, uri: String, error: String) -> SlurpError {
        if e.is_parse() || e.is_parse_status() || e.is_parse_too_large() {
            SlurpError::ErrorDeserializing { uri, error }
        } else if e.is_user() {
            SlurpError::InvalidRequest(error)
        } else if e.is_timeout() {
            SlurpError::Timeout { uri, error }
        } else {
            SlurpError::Transport { uri, error }
        }
    }
}

/// `http::Error` can appear on an HTTP request [`http::Builder::build`] building.
impl From<http::Error> for SlurpError {
    fn from(e: http::Error) -> Self { SlurpError::InvalidRequest(e.to_string()) }
}

/// Executes a Hyper request, returning the response status, headers and body.
pub async fn slurp_req(request: Request<Vec<u8>>) -> SlurpResult {
    let uri = request.uri().to_string();
    let (head, body) = request.into_parts();
    let request = Request::from_parts(head, Full::new(Bytes::from(body)));

    let request_f = HYPER.request(request);
    let response = drive03(request_f)
        .await?
        .map_to_mm(|e: ClientError| SlurpError::from_error(&e, uri.clone()))?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body();
    let output = body
        .collect()
        .await
        .map_to_mm(|e: hyper::Error| SlurpError::from_error(&e, uri.clone()))?
        .to_bytes();
    Ok((status, headers, output.to_vec()))
}

/// Like [`slurp_req`], but the whole exchange (connecting, the request, the response head, the full body) is bounded
/// by `timeout_s` seconds, and the bound is applied *inside* the future that runs on the shared runtime.
///
/// [`slurp_req`] hands the request to a spawned task, so dropping its future (for example when a caller's own
/// timer wins a race) does not stop the request: a peer that accepts the connection and never answers keeps the
/// task and the socket forever, and the shared client has no timeout of its own. Here the spawned task ends by
/// itself when the bound elapses, which drops the request and closes the socket, and the caller gets
/// [`SlurpError::Timeout`]. The timer is the tokio timer, which is the one that runs on the shared runtime the task
/// is spawned on.
pub async fn slurp_req_bounded(request: Request<Vec<u8>>, timeout_s: f64) -> SlurpResult {
    use futures::future::{select, Either};
    use hyper::rt::Timer as _;
    use hyper_util::rt::TokioTimer;

    let uri = request.uri().to_string();
    let (head, body) = request.into_parts();
    let request = Request::from_parts(head, Full::new(Bytes::from(body)));
    let bound = Duration::from_secs_f64(timeout_s.max(0.));

    let exchange_uri = uri.clone();
    let exchange = async move {
        let response = HYPER
            .request(request)
            .await
            .map_to_mm(|e: ClientError| SlurpError::from_error(&e, exchange_uri.clone()))?;
        let status = response.status();
        let headers = response.headers().clone();
        let output = response
            .into_body()
            .collect()
            .await
            .map_to_mm(|e: hyper::Error| SlurpError::from_error(&e, exchange_uri))?
            .to_bytes();
        Ok((status, headers, output.to_vec()))
    };
    let bounded = async move {
        // The timer is created inside the spawned task, so it is registered with the runtime that polls it.
        let timer = TokioTimer::new().sleep(bound);
        match select(Box::pin(exchange), timer).await {
            Either::Left((result, _)) => result,
            Either::Right(_) => MmError::err(SlurpError::Timeout {
                uri,
                error: format!("no complete response within {timeout_s} s"),
            }),
        }
    };
    drive03(bounded).await?
}

/// Executes a GET request, returning the response status, headers and body.
pub async fn slurp_url(url: &str) -> SlurpResult {
    let req = Request::builder().uri(url).body(Vec::new())?;
    slurp_req(req).await
}

/// Executes a GET request with custom headers, returning the response status, headers and body.
pub async fn slurp_url_with_headers(url: &str, headers: Vec<(&str, &str)>) -> SlurpResult {
    let mut builder = Request::builder().uri(url);
    for (key, value) in headers {
        builder = builder.header(key, value);
    }
    let req = builder.body(Vec::new())?;
    slurp_req(req).await
}

/// Executes a POST request, returning the response status, headers and body.
pub async fn slurp_post_json(url: &str, body: String) -> SlurpResult {
    let request = Request::builder()
        .method("POST")
        .uri(url)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .body(body.into())?;
    slurp_req(request).await
}

#[cfg(test)]
mod tests {
    use crate::native_http::{slurp_post_json, slurp_req_bounded, slurp_url};
    use crate::transport::SlurpError;
    use common::block_on;

    #[test]
    fn test_slurp_req() {
        let (status, headers, body) = block_on(slurp_url("https://httpbin.org/get")).unwrap();
        assert!(status.is_success(), "{:?} {:?} {:?}", status, headers, body);
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

    #[test]
    fn test_slurp_post_json_over_loopback() {
        let port =
            serve_once(b"HTTP/1.1 201 Created\r\nX-Probe: yes\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        let (status, headers, body) =
            block_on(slurp_post_json(&format!("http://127.0.0.1:{port}/x"), "{}".to_owned())).unwrap();
        assert_eq!(status.as_u16(), 201);
        assert_eq!(headers.get("x-probe").unwrap(), "yes");
        assert_eq!(body, b"hello");
    }

    #[test]
    fn test_slurp_reports_the_cause_of_a_connect_failure() {
        // Reserve a port and close it again, so that nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = block_on(slurp_url(&format!("http://127.0.0.1:{port}/"))).unwrap_err();
        match err.into_inner() {
            SlurpError::Transport { error, .. } => {
                assert!(error.contains("Connect") && error.contains(": "), "{error}")
            },
            other => panic!("unexpected error {other:?}"),
        }
    }

    /// A peer that accepts the connection and never answers: the bound is applied inside the spawned task, so the
    /// call returns a timeout within the bound and the task has gone (the peer sees its connection closed) too.
    #[test]
    fn test_slurp_req_bounded_ends_the_spawned_request_on_a_silent_peer() {
        use std::io::Read;
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request); // the request, never answered
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // Without the spawned task ending, nothing would ever close the socket and this read would time out.
            stream.read(&mut request).map_err(|e| e.kind())
        });

        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{port}/x"))
            .body(b"{}".to_vec())
            .unwrap();
        let started = Instant::now();
        let err = block_on(slurp_req_bounded(request, 0.5)).unwrap_err();
        let elapsed = started.elapsed();
        match err.into_inner() {
            SlurpError::Timeout { uri, error } => {
                assert!(uri.contains(&port.to_string()));
                assert!(error.contains("0.5"), "{error}");
            },
            other => panic!("unexpected error {other:?}"),
        }
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(3),
            "{elapsed:?}"
        );
        assert_eq!(
            peer.join().unwrap(),
            Ok(0),
            "the peer must see EOF once the bound is over"
        );
    }

    #[test]
    fn test_slurp_req_bounded_returns_the_answer_within_the_bound() {
        let port = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{port}/x"))
            .body(b"{}".to_vec())
            .unwrap();
        let (status, _headers, body) = block_on(slurp_req_bounded(request, 5.)).unwrap();
        assert_eq!(status.as_u16(), 200);
        assert_eq!(body, b"ok");
    }
}
