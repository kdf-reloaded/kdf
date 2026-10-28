//! CRD R35.1.14, last paragraph: the URLs of the fee-estimation services (the `gas_station_url` of R35.1.1 and
//! the URL of the configured gas-API provider) are named host-only in every error text and log line, because
//! they carry a provider API key just as a node URL does.
//!
//! The texts are asserted as formatted strings: the log lines are built by [`gas_station_failed_text`] and by the
//! error mapping of the fetchers, so what is checked is exactly what `error!` receives. The loopback tests drive
//! the real HTTP layer against a local socket, so the embedded lower-layer text is the real one.

use super::fee_estimation::eip1559::block_native::BlocknativeFeeFetcher;
use super::fee_estimation::eip1559::infura::InfuraFeeFetcher;
use super::fee_estimation::eip1559::{fee_service_status_error, fee_service_transport_error};
use super::*;
use common::block_on;
use mm2_net::transport::SlurpError;

const URL: &str = "https://user:secret@gas.example.org:8443/v3/APIKEY?key=X";
const HOST_ONLY: &str = "https://gas.example.org:8443";
const SECRETS: [&str; 5] = ["user", "secret", "/v3/APIKEY", "APIKEY", "key=X"];

fn assert_host_only_text(text: &str, host_only: &str, secrets: &[&str]) {
    assert!(text.contains(host_only), "the host-only form is missing from the text");
    for secret in secrets {
        assert!(!text.contains(secret), "a credential part is rendered");
    }
}

/// A lower-layer error as the HTTP layer renders it, with the URL in several forms.
fn lower_layer_errors(url: &str) -> Vec<SlurpError> {
    vec![
        SlurpError::Transport {
            uri: url.to_owned(),
            error: format!("error sending request for url ({url}): dns error for {url}"),
        },
        SlurpError::Timeout {
            uri: url.to_owned(),
            error: format!("no complete response within 30 s from {url}"),
        },
        SlurpError::ErrorDeserializing {
            uri: url.to_owned(),
            error: "expected value".to_owned(),
        },
    ]
}

#[test]
fn a_gas_station_failure_names_the_station_host_only() {
    for slurp_err in lower_layer_errors(URL) {
        let err = gas_station_error_host_only(GasStationReqErr::from(slurp_err), URL);
        let text = err.to_string();
        assert_host_only_text(&text, if text.contains("Invalid") { "" } else { HOST_ONLY }, &SECRETS);
        // The same error as it reaches the caller of the gas-price estimation.
        let web3_text = Web3RpcError::from(err).to_string();
        for secret in SECRETS {
            assert!(!web3_text.contains(secret), "a credential part is rendered");
        }
    }
}

#[test]
fn the_gas_station_log_line_names_the_station_host_only() {
    // The line as the activation-time default builds it, and with a lower-layer text that embeds the URL.
    let embedded = format!("Transport '{URL}' error: error sending request for url ({URL})");
    let line = gas_station_failed_text(&embedded, URL);
    assert_host_only_text(&line, HOST_ONLY, &SECRETS);
    assert!(
        line.ends_with(&format!("on request to gas station url {HOST_ONLY}")),
        "{line}"
    );

    // A string that is not an absolute URL is never echoed.
    let line = gas_station_failed_text(&"boom", "gas.example.org:8443/v3/APIKEY");
    assert!(!line.contains("APIKEY"), "{line}");
}

#[test]
fn the_fee_service_texts_name_the_service_host_only() {
    // Infura and Blocknative append their endpoint to the configured base URL; the base URL carries the key.
    for request_url in [
        format!("{URL}/networks/1/suggestedGasFees"),
        format!("{URL}/gasprices/blockprices?confidenceLevels=10&withBaseFees=true"),
    ] {
        for slurp_err in lower_layer_errors(&request_url) {
            let text = fee_service_transport_error(&slurp_err, &request_url);
            let must_name = !matches!(slurp_err, SlurpError::ErrorDeserializing { .. });
            assert_host_only_text(&text, if must_name { HOST_ONLY } else { "" }, &SECRETS);
        }
        let text = fee_service_status_error(&request_url, http::StatusCode::BAD_GATEWAY);
        assert_eq!(text, format!("{HOST_ONLY} failed with status code 502 Bad Gateway"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod loopback {
    use super::*;
    use std::io::{Read, Write};

    /// Serves one canned answer and returns the port.
    fn serve_once(response: &'static [u8]) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response);
        });
        port
    }

    /// A port on which nothing listens.
    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn keyed_url(port: u16) -> String { format!("http://user:secret@127.0.0.1:{port}/v3/APIKEY?key=X") }

    const BAD_GATEWAY: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[test]
    fn the_gas_station_request_errors_name_the_station_host_only() {
        let port = closed_port();
        let url = keyed_url(port);
        let refused = block_on(make_gas_station_request(&url)).unwrap_err().to_string();
        assert_host_only_text(&refused, &format!("http://127.0.0.1:{port}"), &SECRETS);

        let port = serve_once(BAD_GATEWAY);
        let url = keyed_url(port);
        let status = block_on(make_gas_station_request(&url)).unwrap_err().to_string();
        assert!(status.contains("status code 502"), "{status}");
        assert_host_only_text(&status, &format!("http://127.0.0.1:{port}"), &SECRETS);
        // And as the caller of the gas-price estimation sees it.
        let web3 = Web3RpcError::from(
            block_on(make_gas_station_request(&keyed_url(closed_port())).map_err(|e| e.into_inner())).unwrap_err(),
        );
        for secret in SECRETS {
            assert!(!web3.to_string().contains(secret), "a credential part is rendered");
        }
    }

    #[test]
    fn the_infura_request_errors_name_the_service_host_only() {
        let port = closed_port();
        let refused = block_on(InfuraFeeFetcher::fetch_fee_estimation(&keyed_url(port)))
            .unwrap_err()
            .to_string();
        assert_host_only_text(&refused, &format!("http://127.0.0.1:{port}"), &SECRETS);

        let port = serve_once(BAD_GATEWAY);
        let status = block_on(InfuraFeeFetcher::fetch_fee_estimation(&keyed_url(port)))
            .unwrap_err()
            .to_string();
        assert!(status.contains("status code 502"), "{status}");
        assert_host_only_text(&status, &format!("http://127.0.0.1:{port}"), &SECRETS);
    }

    #[test]
    fn the_blocknative_request_errors_name_the_service_host_only() {
        let port = closed_port();
        let refused = block_on(BlocknativeFeeFetcher::fetch_fee_estimation(&keyed_url(port)))
            .unwrap_err()
            .to_string();
        assert_host_only_text(&refused, &format!("http://127.0.0.1:{port}"), &SECRETS);

        let port = serve_once(BAD_GATEWAY);
        let status = block_on(BlocknativeFeeFetcher::fetch_fee_estimation(&keyed_url(port)))
            .unwrap_err()
            .to_string();
        assert!(status.contains("status code 502"), "{status}");
        assert_host_only_text(&status, &format!("http://127.0.0.1:{port}"), &SECRETS);
    }
}
