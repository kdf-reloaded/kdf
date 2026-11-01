use async_trait::async_trait;
use core::convert::{TryFrom, TryInto};
use core::str::FromStr;
use cosmrs::tendermint::block::Height;
use cosmrs::tendermint::evidence::Evidence;
use cosmrs::tendermint::Genesis;
use cosmrs::tendermint::Hash;
use http::Uri;
use serde::{de::DeserializeOwned, Serialize};
use std::fmt;
use std::time::Duration;
use tendermint_rpc::endpoint::validators::DEFAULT_VALIDATORS_PER_PAGE;
use tendermint_rpc::endpoint::*;
pub use tendermint_rpc::endpoint::{abci_query::Request as AbciRequest, health::Request as HealthRequest,
                                   tx_search::Request as TxSearchRequest};
use tendermint_rpc::Paging;
pub use tendermint_rpc::{query::Query as TendermintQuery, Error, Order, Scheme, SimpleRequest, Url};
use tokio::time;

use crate::node_url::host_only;

/// Provides lightweight access to the Tendermint RPC. It gives access to all
/// endpoints with the exception of the event subscription-related ones.
#[async_trait]
#[allow(dead_code)]
pub trait Client {
    /// `/abci_info`: get information about the ABCI application.
    async fn abci_info(&self) -> Result<abci_info::Response, Error> { self.perform(abci_info::Request).await }

    /// `/abci_query`: query the ABCI application
    async fn abci_query<V>(
        &self,
        path: Option<String>,
        data: V,
        height: Option<Height>,
        prove: bool,
    ) -> Result<abci_query::AbciQuery, Error>
    where
        V: Into<Vec<u8>> + Send,
    {
        Ok(self
            .perform(abci_query::Request::new(path, data, height, prove))
            .await?
            .response)
    }

    /// `/block`: get block at a given height.
    async fn block<H>(&self, height: H) -> Result<block::Response, Error>
    where
        H: Into<Height> + Send,
    {
        self.perform(block::Request::new(height.into())).await
    }

    /// `/block`: get the latest block.
    async fn latest_block(&self) -> Result<block::Response, Error> { self.perform(block::Request::default()).await }

    /// `/block_results`: get ABCI results for a block at a particular height.
    async fn block_results<H>(&self, height: H) -> Result<block_results::Response, Error>
    where
        H: Into<Height> + Send,
    {
        self.perform(block_results::Request::new(height.into())).await
    }

    /// `/block_results`: get ABCI results for the latest block.
    async fn latest_block_results(&self) -> Result<block_results::Response, Error> {
        self.perform(block_results::Request::default()).await
    }

    /// `/block_search`: search for blocks by BeginBlock and EndBlock events.
    async fn block_search(
        &self,
        query: TendermintQuery,
        page: u32,
        per_page: u8,
        order: Order,
    ) -> Result<block_search::Response, Error> {
        self.perform(block_search::Request::new(query, page, per_page, order))
            .await
    }

    /// `/blockchain`: get block headers for `min` <= `height` <= `max`.
    async fn blockchain<H>(&self, min: H, max: H) -> Result<blockchain::Response, Error>
    where
        H: Into<Height> + Send,
    {
        self.perform(blockchain::Request::new(min.into(), max.into())).await
    }

    /// `/broadcast_tx_async`: broadcast a transaction, returning immediately.
    async fn broadcast_tx_async(&self, tx: Vec<u8>) -> Result<broadcast::tx_async::Response, Error> {
        self.perform(broadcast::tx_async::Request::new(tx)).await
    }

    /// `/broadcast_tx_sync`: broadcast a transaction, returning the response
    /// from `CheckTx`.
    async fn broadcast_tx_sync(&self, tx: Vec<u8>) -> Result<broadcast::tx_sync::Response, Error> {
        self.perform(broadcast::tx_sync::Request::new(tx)).await
    }

    /// `/broadcast_tx_commit`: broadcast a transaction, returning the response
    /// from `DeliverTx`.
    async fn broadcast_tx_commit(&self, tx: Vec<u8>) -> Result<broadcast::tx_commit::Response, Error> {
        self.perform(broadcast::tx_commit::Request::new(tx)).await
    }

    /// `/commit`: get block commit at a given height.
    async fn commit<H>(&self, height: H) -> Result<commit::Response, Error>
    where
        H: Into<Height> + Send,
    {
        self.perform(commit::Request::new(height.into())).await
    }

    /// `/consensus_params`: get current consensus parameters at the specified height.
    async fn consensus_params<H>(&self, height: H) -> Result<consensus_params::Response, Error>
    where
        H: Into<Height> + Send,
    {
        self.perform(consensus_params::Request::new(Some(height.into()))).await
    }

    /// `/consensus_state`: get current consensus state
    async fn consensus_state(&self) -> Result<consensus_state::Response, Error> {
        self.perform(consensus_state::Request::new()).await
    }

    /// `/validators`: get validators a given height.
    async fn validators<H>(&self, height: H, paging: Paging) -> Result<validators::Response, Error>
    where
        H: Into<Height> + Send,
    {
        let height = height.into();
        match paging {
            Paging::Default => self.perform(validators::Request::new(Some(height), None, None)).await,
            Paging::Specific { page_number, per_page } => {
                self.perform(validators::Request::new(
                    Some(height),
                    Some(page_number),
                    Some(per_page),
                ))
                .await
            },
            Paging::All => {
                let mut page_num = 1_usize;
                let mut validators = Vec::new();
                let per_page = DEFAULT_VALIDATORS_PER_PAGE.into();
                loop {
                    let response = self
                        .perform(validators::Request::new(
                            Some(height),
                            Some(page_num.into()),
                            Some(per_page),
                        ))
                        .await?;
                    validators.extend(response.validators);
                    if validators.len() as i32 == response.total {
                        return Ok(validators::Response::new(
                            response.block_height,
                            validators,
                            response.total,
                        ));
                    }
                    page_num += 1;
                }
            },
        }
    }

    /// `/consensus_params`: get the latest consensus parameters.
    async fn latest_consensus_params(&self) -> Result<consensus_params::Response, Error> {
        self.perform(consensus_params::Request::new(None)).await
    }

    /// `/commit`: get the latest block commit
    async fn latest_commit(&self) -> Result<commit::Response, Error> { self.perform(commit::Request::default()).await }

    /// `/health`: get node health.
    async fn health(&self) -> Result<(), Error> {
        self.perform(health::Request).await?;
        Ok(())
    }

    /// `/genesis`: get genesis file.
    async fn genesis<AppState>(&self) -> Result<Genesis<AppState>, Error>
    where
        AppState: fmt::Debug + Serialize + DeserializeOwned + Send,
    {
        Ok(self.perform(genesis::Request::default()).await?.genesis)
    }

    /// `/net_info`: obtain information about P2P and other network connections.
    async fn net_info(&self) -> Result<net_info::Response, Error> { self.perform(net_info::Request).await }

    /// `/status`: get Tendermint status including node info, pubkey, latest
    /// block hash, app hash, block height and time.
    async fn status(&self) -> Result<status::Response, Error> { self.perform(status::Request).await }

    /// `/broadcast_evidence`: broadcast an evidence.
    async fn broadcast_evidence(&self, e: Evidence) -> Result<evidence::Response, Error> {
        self.perform(evidence::Request::new(e)).await
    }

    /// `/tx`: find transaction by hash.
    async fn tx(&self, hash: Hash, prove: bool) -> Result<tx::Response, Error> {
        self.perform(tx::Request::new(hash, prove)).await
    }

    /// `/tx_search`: search for transactions with their results.
    async fn tx_search(
        &self,
        query: TendermintQuery,
        prove: bool,
        page: u32,
        per_page: u8,
        order: Order,
    ) -> Result<tx_search::Response, Error> {
        self.perform(tx_search::Request::new(query, prove, page, per_page, order))
            .await
    }

    /// Poll the `/health` endpoint until it returns a successful result or
    /// the given `timeout` has elapsed.
    async fn wait_until_healthy<T>(&self, timeout: T) -> Result<(), Error>
    where
        T: Into<Duration> + Send,
    {
        let timeout = timeout.into();
        let poll_interval = Duration::from_millis(200);
        let mut attempts_remaining = timeout.as_millis() / poll_interval.as_millis();

        while self.health().await.is_err() {
            if attempts_remaining == 0 {
                return Err(Error::timeout(timeout));
            }

            attempts_remaining -= 1;
            time::sleep(poll_interval).await;
        }

        Ok(())
    }

    /// Perform a request against the RPC endpoint
    async fn perform<R>(&self, request: R) -> Result<R::Output, Error>
    where
        R: SimpleRequest;
}

/// A JSON-RPC/HTTP Tendermint RPC client.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: sealed::HttpClient,
}

impl HttpClient {
    /// Construct a new Tendermint RPC HTTP/S client connecting to the given URL.
    pub fn new<U>(url: U) -> Result<Self, Error>
    where
        U: TryInto<HttpClientUrl, Error = Error>,
    {
        let url = url.try_into()?;
        Ok(Self {
            inner: if url.0.is_secure() {
                sealed::HttpClient::new_https(url.try_into()?)
            } else {
                sealed::HttpClient::new_http(url.try_into()?)
            },
        })
    }

    #[inline]
    pub fn uri(&self) -> Uri { self.inner.uri() }
}

#[async_trait]
impl Client for HttpClient {
    async fn perform<R>(&self, request: R) -> Result<R::Output, Error>
    where
        R: SimpleRequest,
    {
        self.inner.perform(request).await.map(From::from)
    }
}

/// A URL limited to use with HTTP clients.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HttpClientUrl(Url);

/// The debug form names the node by its host-only form (CRD ch.36 R36.5A.2).
impl fmt::Debug for HttpClientUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HttpClientUrl")
            .field(&host_only(&self.0.to_string()))
            .finish()
    }
}

impl TryFrom<Url> for HttpClientUrl {
    type Error = Error;

    fn try_from(value: Url) -> Result<Self, Error> {
        match value.scheme() {
            Scheme::Http | Scheme::Https => Ok(Self(value)),
            _ => Err(Error::invalid_url(value)),
        }
    }
}

impl FromStr for HttpClientUrl {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let url: Url = s.parse()?;
        url.try_into()
    }
}

impl TryFrom<&str> for HttpClientUrl {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> { value.parse() }
}

impl From<HttpClientUrl> for Url {
    fn from(url: HttpClientUrl) -> Self { url.0 }
}

impl TryFrom<HttpClientUrl> for http::Uri {
    type Error = Error;

    fn try_from(value: HttpClientUrl) -> Result<Self, Error> {
        value
            .0
            .to_string()
            .parse()
            .map_err(|e: http::uri::InvalidUri| Error::parse(e.to_string()))
    }
}

mod sealed {
    use crate::node_url::{host_only, scrub_node_url};
    use common::log::debug;
    use common::wio::https_connector;
    use http::{header, HeaderValue, Uri};
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_rustls::HttpsConnector;
    use hyper_util::client::legacy::connect::{Connect, HttpConnector};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::{TokioExecutor, TokioTimer};
    use std::fmt;
    use tendermint_rpc::{Error, Response, SimpleRequest};

    type RequestBody = Full<Bytes>;

    /// The pool timer is what makes idle connections expire (hyper 0.14 had it built in).
    fn client_builder() -> hyper_util::client::legacy::Builder {
        let mut builder = Client::builder(TokioExecutor::new());
        builder.pool_timer(TokioTimer::new());
        builder
    }

    /// A wrapper for a `hyper`-based client, generic over the connector type.
    #[derive(Clone)]
    pub struct HyperClient<C> {
        uri: Uri,
        inner: Client<C, RequestBody>,
    }

    /// The debug form names the node by its host-only form (CRD ch.36 R36.5A.2).
    impl<C> fmt::Debug for HyperClient<C> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HyperClient")
                .field("node", &host_only(&self.uri.to_string()))
                .finish()
        }
    }

    impl<C> HyperClient<C> {
        pub fn new(uri: Uri, inner: Client<C, RequestBody>) -> Self { Self { uri, inner } }
    }

    impl<C> HyperClient<C>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        pub async fn perform<R>(&self, request: R) -> Result<R::Response, Error>
        where
            R: SimpleRequest,
        {
            let kind = request.method().to_string();
            let request = self.build_request(request)?;
            let response = self
                .inner
                .request(request)
                .await
                .map_err(|e| Error::client_internal(scrub_node_url(&e.to_string(), &self.uri.to_string())))?;
            let response_body = response_to_string(response).await?;
            // A node's response body is never logged (CRD ch.36 R36.5A.3): the kind, the node and the size.
            debug!(
                "Tendermint RPC {} answered {} with {} bytes",
                host_only(&self.uri.to_string()),
                kind,
                response_body.len()
            );
            R::Response::from_string(&response_body)
        }
    }

    impl<C> HyperClient<C> {
        /// Build a request using the given Tendermint RPC request.
        pub fn build_request<R: SimpleRequest>(&self, request: R) -> Result<hyper::Request<RequestBody>, Error> {
            let body_bytes = request.into_json().into_bytes();

            let mut request = hyper::Request::builder()
                .method("POST")
                .uri(&self.uri)
                .body(Full::new(Bytes::from(body_bytes)))
                .map_err(|e| Error::client_internal(e.to_string()))?;

            {
                let headers = request.headers_mut();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(
                    header::USER_AGENT,
                    format!("tendermint.rs/{}", env!("CARGO_PKG_VERSION")).parse().unwrap(),
                );
            }

            Ok(request)
        }
    }

    /// We offer several variations of `hyper`-based client.
    #[derive(Debug, Clone)]
    pub enum HttpClient {
        Http(HyperClient<HttpConnector>),
        Https(HyperClient<HttpsConnector<HttpConnector>>),
    }

    impl HttpClient {
        pub fn new_http(uri: Uri) -> Self { Self::Http(HyperClient::new(uri, client_builder().build_http())) }

        pub fn new_https(uri: Uri) -> Self {
            Self::Https(HyperClient::new(uri, client_builder().build(https_connector())))
        }

        pub async fn perform<R>(&self, request: R) -> Result<R::Response, Error>
        where
            R: SimpleRequest,
        {
            match self {
                HttpClient::Http(c) => c.perform(request).await,
                HttpClient::Https(c) => c.perform(request).await,
            }
        }

        pub fn uri(&self) -> Uri {
            match self {
                HttpClient::Http(client) => client.uri.clone(),
                HttpClient::Https(client) => client.uri.clone(),
            }
        }
    }

    async fn response_to_string(response: hyper::Response<hyper::body::Incoming>) -> Result<String, Error> {
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::client_internal(e.to_string()))?
            .to_bytes();
        String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Serves one canned HTTP/1.1 response on a loopback port; returns the port and the received request.
    fn serve_once(body: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // The head and the body may arrive in separate segments: read up to the declared length.
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    if body.len() >= length {
                        break;
                    }
                }
            }
            tx.send(String::from_utf8_lossy(&request).into_owned()).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (port, rx)
    }

    #[test]
    fn http_client_posts_json_rpc_and_parses_the_answer() {
        let (port, request) = serve_once(r#"{"jsonrpc":"2.0","id":"probe","result":{}}"#);
        let client = HttpClient::new(format!("http://127.0.0.1:{port}").as_str()).unwrap();
        common::block_on(async {
            client.health().await.unwrap();
        });
        let request = request.recv().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post / http/1.1"), "{request}");
        assert!(request.contains("content-type: application/json"), "{request}");
        assert!(
            request
                .split_whitespace()
                .collect::<String>()
                .contains("\"method\":\"health\""),
            "{request}"
        );
    }

    #[test]
    fn http_client_reports_a_non_utf8_body_as_an_io_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 8192];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe")
                .unwrap();
        });
        let client = HttpClient::new(format!("http://127.0.0.1:{port}").as_str()).unwrap();
        let err = common::block_on(async { client.health().await.unwrap_err() });
        assert!(err.to_string().to_ascii_lowercase().contains("utf-8"), "{err}");
    }

    /// T36.15: no log line carries the body of a node's response; a line may carry its size.
    #[test]
    fn a_response_body_is_never_logged() {
        use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
        let capturing = install_log_capture();
        let (port, _request) = serve_once(r#"{"jsonrpc":"2.0","id":"probe","result":{"marker":"BODYMARKER"}}"#);
        let client = HttpClient::new(format!("http://127.0.0.1:{port}").as_str()).unwrap();
        common::block_on(async {
            client.health().await.unwrap();
        });
        let lines = captured_log_lines();
        if capturing {
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("Tendermint RPC") && line.contains("bytes")),
                "the answer is reported by its size"
            );
        }
        assert!(
            lines.iter().all(|line| !line.contains("BODYMARKER")),
            "a log line carries the response body"
        );
    }

    /// T36.14 (client part): the debug forms of the client and of its URL name the node host-only.
    #[test]
    fn the_debug_form_of_a_client_names_the_node_host_only() {
        let client = HttpClient::new("http://user:secret@127.0.0.1:26657/rpc/APIKEY?token=X").unwrap();
        let url = HttpClientUrl::from_str("http://user:secret@127.0.0.1:26657/rpc/APIKEY?token=X").unwrap();
        for rendered in [
            format!("{client:?}"),
            format!("{url:?}"),
            format!("{:#?}", client.clone()),
        ] {
            assert!(rendered.contains("http://127.0.0.1:26657"), "{rendered}");
            for secret in ["user", "secret", "/rpc/", "APIKEY", "token=X"] {
                assert!(!rendered.contains(secret), "a credential part is rendered");
            }
        }
    }
}
