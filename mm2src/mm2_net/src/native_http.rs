use crate::host_label::host_only_uri;
use crate::transport::{SlurpError, SlurpResult};
use common::wio::{drive03, HYPER};
use futures::channel::oneshot::Canceled;
use http::{header, HeaderValue, Request, StatusCode};
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

/// How a spawned request ended, as far as the line about an abandoned request tells it. It holds no URL and no
/// error text: a [`SlurpError`] renders the full URI, which can carry a credential (CRD ch.35 R35.1.17).
enum Ended {
    /// The exchange reached its own bound, in seconds.
    BoundReached(f64),
    /// An answer arrived (the status of its head).
    Answered(StatusCode),
    /// The request failed; the kind of the failure.
    Failed(&'static str),
}

impl SlurpError {
    /// The kind of the error, with none of its text.
    fn kind(&self) -> &'static str {
        match self {
            SlurpError::ErrorDeserializing { .. } => "deserializing",
            SlurpError::InvalidRequest(_) => "invalid request",
            SlurpError::Timeout { .. } => "timeout",
            SlurpError::Transport { .. } => "transport",
            SlurpError::Internal(_) => "internal",
        }
    }
}

/// The log line for a spawned request that finished after its caller had stopped waiting. `host` is the
/// host-only label of the request URI.
fn abandoned_line(host: &str, ended: &Ended) -> String {
    let after = "after its caller had stopped waiting";
    match ended {
        Ended::BoundReached(bound_s) => format!("HTTP request to {host} ended at its {bound_s} s bound {after}"),
        Ended::Answered(status) => {
            format!("HTTP request to {host} got an answer (status {status}) {after}; the answer is discarded")
        },
        Ended::Failed(kind) => format!("HTTP request to {host} failed ({kind}) {after}"),
    }
}

/// `http::Error` can appear on an HTTP request [`http::Builder::build`] building.
impl From<http::Error> for SlurpError {
    fn from(e: http::Error) -> Self { SlurpError::InvalidRequest(e.to_string()) }
}

/// Executes a Hyper request, returning the response status, headers and body.
pub async fn slurp_req(request: Request<Vec<u8>>) -> SlurpResult {
    // The only form of the request URI that leaves this function (CRD ch.35 R35.1.17): the URI can carry a
    // credential, and the errors below, their `Display` and everything that copies them are rendered in logs.
    let host = host_only_uri(request.uri());
    let uri = host.clone();
    let (head, body) = request.into_parts();
    let request = Request::from_parts(head, Full::new(Bytes::from(body)));

    let request_f = HYPER.request(request);
    let response = drive03(request_f, move |answer| {
        let ended = match answer {
            Ok(response) => Ended::Answered(response.status()),
            Err(e) => Ended::Failed(SlurpError::from_error(e, String::new()).kind()),
        };
        abandoned_line(&host, &ended)
    })
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

    // The only form of the request URI that leaves this function (CRD ch.35 R35.1.17), see `slurp_req`.
    let host = host_only_uri(request.uri());
    let uri = host.clone();
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
            Either::Left((result, _)) => (result, false),
            Either::Right(_) => (
                MmError::err(SlurpError::Timeout {
                    uri,
                    error: format!("no complete response within {timeout_s} s"),
                }),
                true,
            ),
        }
    };
    // The flag tells the exchange's own bound from any other failure, for the line about an abandoned request.
    let (result, _) = drive03(bounded, move |(result, bound_reached): &(SlurpResult, bool)| {
        let ended = match result {
            _ if *bound_reached => Ended::BoundReached(timeout_s),
            Ok((status, ..)) => Ended::Answered(*status),
            Err(e) => Ended::Failed(e.get_inner().kind()),
        };
        abandoned_line(&host, &ended)
    })
    .await?;
    result
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
    use crate::native_http::{abandoned_line, slurp_post_json, slurp_req, slurp_req_bounded, slurp_url, Ended};
    use crate::transport::SlurpError;
    use common::block_on;
    use mm2_err_handle::prelude::*;

    #[test]
    fn test_slurp_req() {
        let (status, _headers, _body) = block_on(slurp_url("https://httpbin.org/get")).unwrap();
        assert!(status.is_success(), "the request did not succeed");
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
                assert!(
                    error.contains("Connect") && error.contains(": "),
                    "the error text does not carry the cause chain"
                )
            },
            _ => panic!("unexpected kind of error"),
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
                assert!(error.contains("0.5"), "the timeout text does not name the bound");
            },
            _ => panic!("unexpected kind of error"),
        }
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(3),
            "the call did not end within the bound"
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

    /// A URL as a configured node or service has it: a credential in the userinfo, the path and the query.
    const SECRET_PARTS: [&str; 4] = ["bob", "hunter2", "APIKEY", "token=X"];

    fn assert_no_secret(text: &str) {
        for (i, secret) in SECRET_PARTS.iter().enumerate() {
            assert!(!text.contains(secret), "secret part {i} leaked into the rendered text");
        }
    }

    fn label_of(uri: &str) -> String { crate::host_label::host_only_uri(&uri.parse().unwrap()) }

    #[test]
    fn test_abandoned_line_for_each_outcome() {
        let host = label_of("https://user:hunter2@node.example:8545/v3/APIKEY?token=X");
        assert!(
            host == "https://node.example:8545",
            "the label differs from the host-only form"
        );
        let cases = [
            (
                Ended::BoundReached(11.),
                "HTTP request to https://node.example:8545 ended at its 11 s bound after its caller had stopped waiting",
            ),
            (
                Ended::Answered(http::StatusCode::OK),
                "HTTP request to https://node.example:8545 got an answer (status 200 OK) after its caller had stopped \
                 waiting; the answer is discarded",
            ),
            (
                Ended::Failed("transport"),
                "HTTP request to https://node.example:8545 failed (transport) after its caller had stopped waiting",
            ),
        ];
        for (ended, expected) in cases {
            let line = abandoned_line(&host, &ended);
            assert!(
                line == expected,
                "the line for an outcome differs from its expected text"
            );
            assert_no_secret(&line);
        }
    }

    #[test]
    fn test_abandoned_line_names_no_host_when_the_uri_has_none() {
        let host = crate::host_label::host_only_uri(&"/v3/APIKEY?token=X".parse().unwrap());
        let line = abandoned_line(&host, &Ended::Failed("invalid request"));
        assert!(
            line == "HTTP request to <invalid node URL> failed (invalid request) after its caller had stopped waiting",
            "the line for a URI without a host differs from its expected text"
        );
        assert_no_secret(&line);
    }

    #[test]
    fn test_error_kinds_carry_no_text() {
        let err = SlurpError::Transport {
            uri: "https://node.example/APIKEY".to_owned(),
            error: "token=X".to_owned(),
        };
        assert_eq!(err.kind(), "transport");
        assert_eq!(
            SlurpError::InvalidRequest("hunter2".to_owned()).kind(),
            "invalid request"
        );
        assert_eq!(SlurpError::Internal("hunter2".to_owned()).kind(), "internal");
    }

    /// Collects every line logged through `log!`.
    struct Collector;
    static LOGGED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    impl common::log::LogCallback for Collector {
        fn callback(&mut self, _level: common::log::LogLevel, line: String) { LOGGED.lock().unwrap().push(line); }
    }

    /// Polls `fut` once, so that it hands its request to the shared runtime, and drops it: the caller stops waiting.
    fn start_and_abandon<F: std::future::Future>(fut: F) {
        futures::executor::block_on(async {
            let mut fut = Box::pin(fut);
            let _ = futures::poll!(fut.as_mut());
        });
    }

    fn wait_for_line_with(needle: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(line) = LOGGED.lock().unwrap().iter().find(|l| l.contains(needle)) {
                return line.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the expected log line did not appear"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A caller that stops waiting leaves the spawned request running; the line it logs when it ends names the
    /// host and nothing of the credential in the URL.
    #[test]
    fn test_an_abandoned_request_logs_the_host_only() {
        use std::io::Read;

        common::log::register_callback(Collector);

        // The bounded request: a peer that never answers, so the exchange ends at its own bound.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.read(&mut request); // returns once the bound closes the socket
        });
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://user:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X"))
            .body(b"{}".to_vec())
            .unwrap();
        start_and_abandon(slurp_req_bounded(request, 0.5));
        let line = wait_for_line_with(&format!("127.0.0.1:{port} ended at its 0.5 s bound"));
        assert!(
            line.ends_with("after its caller had stopped waiting"),
            "the line does not end as expected"
        );
        assert_no_secret(&line);
        peer.join().unwrap();

        // The unbounded request: the peer answers after the caller has gone.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://user:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X"))
            .body(b"{}".to_vec())
            .unwrap();
        start_and_abandon(slurp_req(request));
        let line = wait_for_line_with(&format!("127.0.0.1:{port} got an answer"));
        assert!(
            line.contains("the answer is discarded"),
            "the line does not say the answer is discarded"
        );
        assert_no_secret(&line);
        peer.join().unwrap();

        for line in LOGGED.lock().unwrap().iter() {
            assert_no_secret(line);
        }
    }

    /// Every text built from the error of a request to a URL that carries secrets names `host` and none of them.
    fn assert_error_names_the_host_only(err: &MmError<SlurpError>, host: &str) {
        for text in [err.to_string(), format!("{err:?}"), format!("{:?}", err.get_inner())] {
            assert!(
                text.contains(host),
                "the host-only form is missing from the rendered error"
            );
            assert_no_secret(&text);
        }
        match err.get_inner() {
            SlurpError::Timeout { uri, .. }
            | SlurpError::Transport { uri, .. }
            | SlurpError::ErrorDeserializing { uri, .. } => {
                assert!(uri == host, "the uri field is not the host-only form")
            },
            _ => panic!("unexpected kind of error"),
        }
    }

    /// The URL of a node or service as it is configured: a credential in the userinfo, the path and the query.
    fn secret_url(scheme: &str, port: u16) -> String {
        format!("{scheme}://bob:hunter2@127.0.0.1:{port}/v3/APIKEY?token=X")
    }

    /// The `error` text is built from the source chain of the lower layers. Connecting (refused, and a TLS
    /// handshake against a plain-text peer) is the path on which they could repeat the URL.
    #[test]
    fn test_a_slurp_error_names_the_host_only_and_so_does_the_error_chain_under_it() {
        // Nothing listens on the port.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let request = || {
            http::Request::builder()
                .uri(secret_url("http", port))
                .body(b"{}".to_vec())
                .unwrap()
        };
        let err = block_on(slurp_req(request())).unwrap_err();
        assert_error_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));
        let err = block_on(slurp_req_bounded(request(), 5.)).unwrap_err();
        assert_error_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));

        // A TLS client against a peer that answers in plain text.
        let port = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let tls = http::Request::builder()
            .uri(secret_url("https", port))
            .body(b"{}".to_vec())
            .unwrap();
        let err = block_on(slurp_req(tls)).unwrap_err();
        assert_error_names_the_host_only(&err, &format!("https://127.0.0.1:{port}"));
    }

    #[test]
    fn test_a_bounded_timeout_names_the_host_only() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.read(&mut request);
        });
        let request = http::Request::builder()
            .uri(secret_url("http", port))
            .body(b"{}".to_vec())
            .unwrap();
        let err = block_on(slurp_req_bounded(request, 0.3)).unwrap_err();
        assert!(matches!(err.get_inner(), SlurpError::Timeout { .. }));
        assert_error_names_the_host_only(&err, &format!("http://127.0.0.1:{port}"));
        peer.join().unwrap();
    }
}
