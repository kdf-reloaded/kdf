use super::*;
use script::Script;

// Response/request data types and the `electrum_script_hash` helper live in
// the sibling `electrum_types` module (carved out via P13.5 follow-up to keep
// this file focused on the connection/transport layer). They are re-exported
// from `rpc_clients::*` so existing call sites are unaffected.

/// Electrum client configuration
#[allow(clippy::upper_case_acronyms)]
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Serialize)]
enum ElectrumConfig {
    TCP,
    SSL { dns_name: String, skip_validation: bool },
}

/// Electrum client configuration
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, Serialize)]
enum ElectrumConfig {
    WS,
    WSS,
}

fn addr_to_socket_addr(input: &str) -> Result<SocketAddr, String> {
    let mut addr = match input.to_socket_addrs() {
        Ok(a) => a,
        Err(e) => return ERR!("{} resolve error {:?}", input, e),
    };
    match addr.next() {
        Some(a) => Ok(a),
        None => ERR!("{} resolved to None.", input),
    }
}

/// Attempts to process the request (parse url, etc), build up the config and create new electrum connection
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_electrum(
    req: &ElectrumRpcRequest,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<ElectrumConnection, String> {
    let config = match req.protocol {
        ElectrumProtocol::TCP => ElectrumConfig::TCP,
        ElectrumProtocol::SSL => {
            let uri: Uri = try_s!(req.url.parse());
            let host = uri
                .host()
                .ok_or(ERRL!("Couldn't retrieve host from addr {}", req.url))?;

            // check the dns name
            try_s!(DnsNameRef::try_from_ascii_str(host));

            ElectrumConfig::SSL {
                dns_name: host.into(),
                skip_validation: req.disable_cert_verification,
            }
        },
        ElectrumProtocol::WS | ElectrumProtocol::WSS => {
            return ERR!("'ws' and 'wss' protocols are not supported yet. Consider using 'TCP' or 'SSL'")
        },
    };

    Ok(electrum_connect(req.url.clone(), config, event_handlers))
}

/// Attempts to process the request (parse url, etc), build up the config and create new electrum connection
#[cfg(target_arch = "wasm32")]
pub fn spawn_electrum(
    req: &ElectrumRpcRequest,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<ElectrumConnection, String> {
    let mut url = req.url.clone();
    let uri: Uri = try_s!(req.url.parse());

    if uri.scheme().is_some() {
        return ERR!(
            "There has not to be a scheme in the url: {}. \
            'ws://' scheme is used by default. \
            Consider using 'protocol: \"WSS\"' in the electrum request to switch to the 'wss://' scheme.",
            url
        );
    }

    let config = match req.protocol {
        ElectrumProtocol::WS => {
            url.insert_str(0, "ws://");
            ElectrumConfig::WS
        },
        ElectrumProtocol::WSS => {
            url.insert_str(0, "wss://");
            ElectrumConfig::WSS
        },
        ElectrumProtocol::TCP | ElectrumProtocol::SSL => {
            return ERR!("'TCP' and 'SSL' are not supported in a browser. Please use 'WS' or 'WSS' protocols");
        },
    };

    Ok(electrum_connect(url, config, event_handlers))
}

#[derive(Debug)]
/// Represents the active Electrum connection to selected address
pub struct ElectrumConnection {
    /// The client connected to this SocketAddr
    addr: String,
    /// Configuration
    #[allow(dead_code)]
    config: ElectrumConfig,
    /// The Sender forwarding requests to writing part of underlying stream
    tx: Arc<AsyncMutex<Option<mpsc::Sender<Vec<u8>>>>>,
    /// The Sender used to shutdown the background connection loop when ElectrumConnection is dropped
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Responses are stored here
    responses: JsonRpcPendingRequestsShared,
    /// Selected protocol version. The value is initialized after the server.version RPC call.
    protocol_version: AsyncMutex<Option<f32>>,
}

impl ElectrumConnection {
    async fn is_connected(&self) -> bool { self.tx.lock().await.is_some() }

    async fn set_protocol_version(&self, version: f32) { self.protocol_version.lock().await.replace(version); }
}

impl Drop for ElectrumConnection {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            if shutdown_tx.send(()).is_err() {
                warn!("electrum_connection_drop] Warning, shutdown_tx already closed");
            }
        }
    }
}

#[derive(Debug)]
struct ConcurrentRequestState<V> {
    is_running: bool,
    subscribers: Vec<RpcReqSub<V>>,
}

impl<V> ConcurrentRequestState<V> {
    fn new() -> Self {
        ConcurrentRequestState {
            is_running: false,
            subscribers: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ConcurrentRequestMap<K, V> {
    inner: AsyncMutex<HashMap<K, ConcurrentRequestState<V>>>,
}

impl<K, V> Default for ConcurrentRequestMap<K, V> {
    fn default() -> Self {
        ConcurrentRequestMap {
            inner: AsyncMutex::new(HashMap::new()),
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash, V: Clone> ConcurrentRequestMap<K, V> {
    pub fn new() -> ConcurrentRequestMap<K, V> { ConcurrentRequestMap::default() }

    pub(crate) async fn wrap_request(&self, request_arg: K, request_fut: RpcRes<V>) -> Result<V, JsonRpcError> {
        let mut map = self.inner.lock().await;
        let state = map
            .entry(request_arg.clone())
            .or_insert_with(ConcurrentRequestState::new);
        if state.is_running {
            let (tx, rx) = async_oneshot::channel();
            state.subscribers.push(tx);
            // drop here to avoid holding the lock during await
            drop(map);
            rx.await.unwrap()
        } else {
            // drop here to avoid holding the lock during await
            drop(map);
            let request_res = request_fut.compat().await;
            let mut map = self.inner.lock().await;
            let state = map.get_mut(&request_arg).unwrap();
            for sub in state.subscribers.drain(..) {
                if sub.send(request_res.clone()).is_err() {
                    warn!("subscriber is dropped");
                }
            }
            state.is_running = false;
            request_res
        }
    }
}

#[derive(Debug)]
pub struct ElectrumClientImpl {
    coin_ticker: String,
    connections: AsyncMutex<Vec<ElectrumConnection>>,
    next_id: AtomicU64,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
    protocol_version: OrdRange<f32>,
    get_balance_concurrent_map: ConcurrentRequestMap<String, ElectrumBalance>,
    list_unspent_concurrent_map: ConcurrentRequestMap<String, Vec<ElectrumUnspent>>,
}

async fn electrum_request_multi(
    client: ElectrumClient,
    request: JsonRpcRequestEnum,
) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
    let mut futures = vec![];
    let connections = client.connections.lock().await;
    for (i, connection) in connections.iter().enumerate() {
        let connection_addr = connection.addr.clone();
        match &*connection.tx.lock().await {
            Some(tx) => {
                let fut = electrum_request(
                    request.clone(),
                    tx.clone(),
                    connection.responses.clone(),
                    ELECTRUM_TIMEOUT / (connections.len() - i) as u64,
                )
                .map(|response| (JsonRpcRemoteAddr(connection_addr), response));
                futures.push(fut)
            },
            None => (),
        }
    }
    drop(connections);
    if futures.is_empty() {
        return ERR!("All electrums are currently disconnected");
    }

    match request {
        JsonRpcRequestEnum::Single(single) if single.method == "server.ping" => {
            // server.ping must be sent to all servers to keep all connections alive
            return select_ok(futures)
                .map(|(result, _)| result)
                .map_err(|e| ERRL!("{:?}", e))
                .compat()
                .await;
        },
        _ => (),
    }

    let (res, no_of_failed_requests) = select_ok_sequential(futures)
        .compat()
        .await
        .map_err(|e| ERRL!("{:?}", e))?;
    client.rotate_servers(no_of_failed_requests).await;
    Ok(res)
}

async fn electrum_request_to(
    client: ElectrumClient,
    request: JsonRpcRequestEnum,
    to_addr: String,
) -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
    let (tx, responses) = {
        let connections = client.connections.lock().await;
        let connection = connections
            .iter()
            .find(|c| c.addr == to_addr)
            .ok_or(ERRL!("Unknown destination address {}", to_addr))?;
        let responses = connection.responses.clone();
        let tx = {
            match &*connection.tx.lock().await {
                Some(tx) => tx.clone(),
                None => return ERR!("Connection {} is not established yet", to_addr),
            }
        };
        (tx, responses)
    };

    let response = try_s!(
        electrum_request(request.clone(), tx, responses, ELECTRUM_TIMEOUT)
            .compat()
            .await
    );
    Ok((JsonRpcRemoteAddr(to_addr.to_owned()), response))
}

impl ElectrumClientImpl {
    /// Create an Electrum connection and spawn a green thread actor to handle it.
    pub async fn add_server(&self, req: &ElectrumRpcRequest) -> Result<(), String> {
        let connection = try_s!(spawn_electrum(req, self.event_handlers.clone()));
        self.connections.lock().await.push(connection);
        Ok(())
    }

    /// Remove an Electrum connection and stop corresponding spawned actor.
    pub async fn remove_server(&self, server_addr: &str) -> Result<(), String> {
        let mut connections = self.connections.lock().await;
        // do not use retain, we would have to return an error if we did not find connection by the passd address
        let pos = connections
            .iter()
            .position(|con| con.addr == server_addr)
            .ok_or(ERRL!("Unknown electrum address {}", server_addr))?;
        // shutdown_tx will be closed immediately on the connection drop
        connections.remove(pos);
        Ok(())
    }

    /// Moves the Electrum servers that fail in a multi request to the end.
    pub async fn rotate_servers(&self, no_of_rotations: usize) {
        let mut connections = self.connections.lock().await;
        connections.rotate_left(no_of_rotations);
    }

    /// Check if one of the spawned connections is connected.
    pub async fn is_connected(&self) -> bool {
        for connection in self.connections.lock().await.iter() {
            if connection.is_connected().await {
                return true;
            }
        }
        false
    }

    pub async fn count_connections(&self) -> usize { self.connections.lock().await.len() }

    pub async fn count_connected(&self) -> usize {
        let mut connected = 0;
        for connection in self.connections.lock().await.iter() {
            if connection.is_connected().await {
                connected += 1;
            }
        }
        connected
    }

    /// Check if the protocol version was checked for one of the spawned connections.
    pub async fn is_protocol_version_checked(&self) -> bool {
        for connection in self.connections.lock().await.iter() {
            if connection.protocol_version.lock().await.is_some() {
                return true;
            }
        }
        false
    }

    /// Set the protocol version for the specified server.
    pub async fn set_protocol_version(&self, server_addr: &str, version: f32) -> Result<(), String> {
        let connections = self.connections.lock().await;
        let con = connections
            .iter()
            .find(|con| con.addr == server_addr)
            .ok_or(ERRL!("Unknown electrum address {}", server_addr))?;
        con.set_protocol_version(version).await;
        Ok(())
    }

    /// Get available protocol versions.
    pub fn protocol_version(&self) -> &OrdRange<f32> { &self.protocol_version }
}

#[derive(Clone, Debug)]
pub struct ElectrumClient(pub Arc<ElectrumClientImpl>);
impl Deref for ElectrumClient {
    type Target = ElectrumClientImpl;
    fn deref(&self) -> &ElectrumClientImpl { &*self.0 }
}

const BLOCKCHAIN_HEADERS_SUB_ID: &str = "blockchain.headers.subscribe";

impl UtxoJsonRpcClientInfo for ElectrumClient {
    fn coin_name(&self) -> &str { self.coin_ticker.as_str() }
}

impl JsonRpcClient for ElectrumClient {
    fn version(&self) -> &'static str { "2.0" }

    fn next_id(&self) -> String { self.next_id.fetch_add(1, AtomicOrdering::Relaxed).to_string() }

    fn client_info(&self) -> String { UtxoJsonRpcClientInfo::client_info(self) }

    fn transport(&self, request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        Box::new(electrum_request_multi(self.clone(), request).boxed().compat())
    }
}

impl JsonRpcBatchClient for ElectrumClient {}

impl JsonRpcMultiClient for ElectrumClient {
    fn transport_exact(&self, to_addr: String, request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        Box::new(electrum_request_to(self.clone(), request, to_addr).boxed().compat())
    }
}

impl ElectrumClient {
    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#server-ping
    pub fn server_ping(&self) -> RpcRes<()> { rpc_func!(self, "server.ping") }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#server-version
    pub fn server_version(
        &self,
        server_address: &str,
        client_name: &str,
        version: &OrdRange<f32>,
    ) -> RpcRes<ElectrumProtocolVersion> {
        let protocol_version: Vec<String> = version.flatten().into_iter().map(|v| format!("{}", v)).collect();
        rpc_func_from!(self, server_address, "server.version", client_name, protocol_version)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
    /// It can return duplicates sometimes: https://github.com/artemii235/SuperNET/issues/269
    /// We should remove them to build valid transactions
    pub fn scripthash_list_unspent(&self, hash: &str) -> RpcRes<Vec<ElectrumUnspent>> {
        let request_fut = Box::new(rpc_func!(self, "blockchain.scripthash.listunspent", hash).and_then(
            move |unspents: Vec<ElectrumUnspent>| {
                let mut map: HashMap<(H256Json, u32), bool> = HashMap::new();
                let unspents = unspents
                    .into_iter()
                    .filter(|unspent| match map.entry((unspent.tx_hash, unspent.tx_pos)) {
                        Entry::Occupied(_) => false,
                        Entry::Vacant(e) => {
                            e.insert(true);
                            true
                        },
                    })
                    .collect();
                Ok(unspents)
            },
        ));
        let arc = self.clone();
        let hash = hash.to_owned();
        let fut = async move { arc.list_unspent_concurrent_map.wrap_request(hash, request_fut).await };
        Box::new(fut.boxed().compat())
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-listunspent
    /// It can return duplicates sometimes: https://github.com/artemii235/SuperNET/issues/269
    /// We should remove them to build valid transactions.
    /// Please note the function returns `ScriptHashUnspents` elements in the same order in which they were requested.
    pub fn scripthash_list_unspent_batch(&self, hashes: Vec<ElectrumScriptHash>) -> RpcRes<Vec<ScriptHashUnspents>> {
        let requests = hashes
            .iter()
            .map(|hash| rpc_req!(self, "blockchain.scripthash.listunspent", hash));
        Box::new(self.batch_rpc(requests).map(move |unspents: Vec<ScriptHashUnspents>| {
            unspents
                .into_iter()
                .map(|hash_unspents| {
                    hash_unspents
                        .into_iter()
                        .unique_by(|unspent| (unspent.tx_hash, unspent.tx_pos))
                        .collect::<Vec<_>>()
                })
                .collect()
        }))
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-get-history
    pub fn scripthash_get_history(&self, hash: &str) -> RpcRes<Vec<ElectrumTxHistoryItem>> {
        rpc_func!(self, "blockchain.scripthash.get_history", hash)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-gethistory
    pub fn scripthash_get_balance(&self, hash: &str) -> RpcRes<ElectrumBalance> {
        let arc = self.clone();
        let hash = hash.to_owned();
        let fut = async move {
            let request = rpc_func!(arc, "blockchain.scripthash.get_balance", &hash);
            arc.get_balance_concurrent_map.wrap_request(hash, request).await
        };
        Box::new(fut.boxed().compat())
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-scripthash-gethistory
    /// Requests balances in a batch and returns them in the same order they were requested.
    pub fn scripthash_get_balances<I>(&self, hashes: I) -> RpcRes<Vec<ElectrumBalance>>
    where
        I: IntoIterator<Item = String>,
    {
        let requests = hashes
            .into_iter()
            .map(|hash| rpc_req!(self, "blockchain.scripthash.get_balance", &hash));
        self.batch_rpc(requests)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-headers-subscribe
    pub fn blockchain_headers_subscribe(&self) -> RpcRes<ElectrumBlockHeader> {
        rpc_func!(self, "blockchain.headers.subscribe")
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-broadcast
    pub fn blockchain_transaction_broadcast(&self, tx: BytesJson) -> RpcRes<H256Json> {
        rpc_func!(self, "blockchain.transaction.broadcast", tx)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-estimatefee
    /// It is recommended to set n_blocks as low as possible.
    /// However, in some cases, n_blocks = 1 leads to an unreasonably high fee estimation.
    /// https://github.com/KomodoPlatform/atomicDEX-API/issues/656#issuecomment-743759659
    pub fn estimate_fee(&self, mode: &Option<EstimateFeeMode>, n_blocks: u32) -> UtxoRpcFut<f64> {
        match mode {
            Some(m) => {
                Box::new(rpc_func!(self, "blockchain.estimatefee", n_blocks, m).map_to_mm_fut(UtxoRpcError::from))
            },
            None => Box::new(rpc_func!(self, "blockchain.estimatefee", n_blocks).map_to_mm_fut(UtxoRpcError::from)),
        }
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-block-header
    pub fn blockchain_block_header(&self, height: u64) -> RpcRes<BytesJson> {
        rpc_func!(self, "blockchain.block.header", height)
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-block-headers
    pub fn blockchain_block_headers(&self, start_height: u64, count: NonZeroU64) -> RpcRes<ElectrumBlockHeadersRes> {
        rpc_func!(self, "blockchain.block.headers", start_height, count)
    }

    pub fn retrieve_last_headers(
        &self,
        blocks_limit_to_check: NonZeroU64,
        block_height: u64,
    ) -> UtxoRpcFut<(HashMap<u64, BlockHeader>, Vec<BlockHeader>)> {
        let (from, count) = {
            let from = if block_height < blocks_limit_to_check.get() {
                0
            } else {
                block_height - blocks_limit_to_check.get()
            };
            (from, blocks_limit_to_check)
        };
        Box::new(
            self.blockchain_block_headers(from, count)
                .map_to_mm_fut(UtxoRpcError::from)
                .and_then(move |headers| {
                    let (block_registry, block_headers) = {
                        if headers.count == 0 {
                            return MmError::err(UtxoRpcError::Internal("No headers available".to_string()));
                        }
                        let len = CompactInteger::from(headers.count);
                        let mut serialized = serialize(&len).take();
                        serialized.extend(headers.hex.0.into_iter());
                        let mut reader = Reader::new_with_coin_variant(serialized.as_slice(), CoinVariant::Standard);
                        let maybe_block_headers = reader.read_list::<BlockHeader>();
                        let block_headers = match maybe_block_headers {
                            Ok(headers) => headers,
                            Err(e) => return MmError::err(UtxoRpcError::InvalidResponse(format!("{:?}", e))),
                        };
                        let mut block_registry: HashMap<u64, BlockHeader> = HashMap::new();
                        let mut starting_height = from;
                        for block_header in &block_headers {
                            block_registry.insert(starting_height, block_header.clone());
                            starting_height += 1;
                        }
                        (block_registry, block_headers)
                    };
                    Ok((block_registry, block_headers))
                }),
        )
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get-merkle
    pub fn blockchain_transaction_get_merkle(&self, txid: H256Json, height: u64) -> RpcRes<TxMerkleBranch> {
        rpc_func!(self, "blockchain.transaction.get_merkle", txid, height)
    }

    /// Lists unspent outputs locked by the given `script`.
    ///
    /// `list_unspent` only queries the P2PKH script of an address, so non-standard outputs such as
    /// pay-to-pubkey (P2PK) are never discovered through it. This helper queries an arbitrary
    /// scriptPubKey (whose full form is known only where the wallet pubkey is available).
    pub fn list_unspent_for_script(&self, script: &Script) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let script_hash = electrum_script_hash(script);
        Box::new(
            self.scripthash_list_unspent(&hex::encode(script_hash))
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |unspents| {
                    unspents
                        .iter()
                        .map(|unspent| UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.tx_hash.reversed().into(),
                                index: unspent.tx_pos,
                            },
                            value: unspent.value,
                            height: unspent.height,
                        })
                        .collect()
                }),
        )
    }
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoRpcClientOps for ElectrumClient {
    fn list_unspent(&self, address: &Address, _decimals: u8) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let script = output_script(address, ScriptType::P2PKH);
        let script_hash = electrum_script_hash(&script);
        Box::new(
            self.scripthash_list_unspent(&hex::encode(script_hash))
                .map_to_mm_fut(UtxoRpcError::from)
                .map(move |unspents| {
                    unspents
                        .iter()
                        .map(|unspent| UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.tx_hash.reversed().into(),
                                index: unspent.tx_pos,
                            },
                            value: unspent.value,
                            height: unspent.height,
                        })
                        .collect()
                }),
        )
    }

    fn list_unspent_group(&self, addresses: Vec<Address>, _decimals: u8) -> UtxoRpcFut<UnspentMap> {
        let script_hashes = addresses
            .iter()
            .map(|addr| {
                let script = output_script(addr, ScriptType::P2PKH);
                let script_hash = electrum_script_hash(&script);
                hex::encode(script_hash)
            })
            .collect();

        let this = self.clone();
        let fut = async move {
            let unspents = this.scripthash_list_unspent_batch(script_hashes).compat().await?;

            let unspent_map = addresses
                .into_iter()
                // `scripthash_list_unspent_batch` returns `ScriptHashUnspents` elements in the same order in which they were requested.
                // So we can zip `addresses` and `unspents` into one iterator.
                .zip(unspents)
                // Map `(Address, Vec<ElectrumUnspent>)` pairs into `(Address, Vec<UnspentInfo>)`.
                .map(|(address, electrum_unspents)| (address, electrum_unspents.collect_into()))
                .collect();
            Ok(unspent_map)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_transaction(&self, tx: &UtxoTx) -> UtxoRpcFut<H256Json> {
        let bytes = if tx.has_witness() {
            BytesJson::from(serialize_with_flags(tx, SERIALIZE_TRANSACTION_WITNESS))
        } else {
            BytesJson::from(serialize(tx))
        };
        Box::new(
            self.blockchain_transaction_broadcast(bytes)
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    fn send_raw_transaction(&self, tx: BytesJson) -> UtxoRpcFut<H256Json> {
        Box::new(
            self.blockchain_transaction_broadcast(tx)
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// returns transaction bytes by default
    fn get_transaction_bytes(&self, txid: &H256Json) -> UtxoRpcFut<BytesJson> {
        let verbose = false;
        Box::new(rpc_func!(self, "blockchain.transaction.get", txid, verbose).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// returns verbose transaction by default
    fn get_verbose_transaction(&self, txid: &H256Json) -> UtxoRpcFut<RpcTransaction> {
        let verbose = true;
        Box::new(rpc_func!(self, "blockchain.transaction.get", txid, verbose).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://electrumx.readthedocs.io/en/latest/protocol-methods.html#blockchain-transaction-get
    /// Returns verbose transactions in a batch.
    fn get_verbose_transactions(&self, tx_ids: &[H256Json]) -> UtxoRpcFut<Vec<RpcTransaction>> {
        let verbose = true;
        let requests = tx_ids
            .iter()
            .map(|txid| rpc_req!(self, "blockchain.transaction.get", txid, verbose));
        Box::new(self.batch_rpc(requests).map_to_mm_fut(UtxoRpcError::from))
    }

    fn get_block_count(&self) -> UtxoRpcFut<u64> {
        Box::new(
            self.blockchain_headers_subscribe()
                .map(|r| r.block_height())
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    fn display_balance(&self, address: Address, decimals: u8) -> RpcRes<BigDecimal> {
        let hash = electrum_script_hash(&output_script(&address, ScriptType::P2PKH));
        let hash_str = hex::encode(hash);
        Box::new(
            self.scripthash_get_balance(&hash_str)
                .map(move |electrum_balance| electrum_balance.to_big_decimal(decimals)),
        )
    }

    fn display_balances(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<Vec<(Address, BigDecimal)>> {
        let this = self.clone();
        let fut = async move {
            let hashes = addresses.iter().map(|address| {
                let hash = electrum_script_hash(&output_script(address, ScriptType::P2PKH));
                hex::encode(hash)
            });

            let electrum_balances = this.scripthash_get_balances(hashes).compat().await?;
            let balances = electrum_balances
                .into_iter()
                // `scripthash_get_balances` returns `ElectrumBalance` elements in the same order in which they were requested.
                // So we can zip `addresses` and the balances into one iterator.
                .zip(addresses)
                .map(|(electrum_balance, address)| (address, electrum_balance.to_big_decimal(decimals)))
                .collect();
            Ok(balances)
        };

        Box::new(fut.boxed().compat())
    }

    fn estimate_fee_sat(
        &self,
        decimals: u8,
        _fee_method: &EstimateFeeMethod,
        mode: &Option<EstimateFeeMode>,
        n_blocks: u32,
    ) -> UtxoRpcFut<u64> {
        Box::new(self.estimate_fee(mode, n_blocks).map(move |fee| {
            if fee > 0.00001 {
                (fee * 10.0_f64.powf(decimals as f64)) as u64
            } else {
                1000
            }
        }))
    }

    fn get_relay_fee(&self) -> RpcRes<BigDecimal> { rpc_func!(self, "blockchain.relayfee") }

    fn find_output_spend(
        &self,
        tx_hash: H256,
        script_pubkey: &[u8],
        vout: usize,
        _from_block: BlockHashOrHeight,
    ) -> Box<dyn Future<Item = Option<SpentOutputInfo>, Error = String> + Send> {
        let selfi = self.clone();
        let script_hash = hex::encode(electrum_script_hash(script_pubkey));
        let fut = async move {
            let history = try_s!(selfi.scripthash_get_history(&script_hash).compat().await);

            if history.len() < 2 {
                return Ok(None);
            }

            for item in history.iter() {
                let transaction = try_s!(selfi.get_transaction_bytes(&item.tx_hash).compat().await);

                let maybe_spend_tx: UtxoTx = try_s!(deserialize(transaction.as_slice()).map_err(|e| ERRL!("{:?}", e)));

                for (index, input) in maybe_spend_tx.inputs.iter().enumerate() {
                    if input.previous_output.hash == tx_hash && input.previous_output.index == vout as u32 {
                        return Ok(Some(SpentOutputInfo {
                            spending_tx: maybe_spend_tx,
                            input_index: index,
                            spent_in_block: BlockHashOrHeight::Height(item.height),
                        }));
                    }
                }
            }
            Ok(None)
        };
        Box::new(fut.boxed().compat())
    }

    fn get_median_time_past(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        coin_variant: CoinVariant,
    ) -> UtxoRpcFut<u32> {
        let from = if starting_block <= count.get() {
            0
        } else {
            starting_block - count.get() + 1
        };
        Box::new(
            self.blockchain_block_headers(from, count)
                .map_to_mm_fut(UtxoRpcError::from)
                .and_then(|res| {
                    if res.count == 0 {
                        return MmError::err(UtxoRpcError::InvalidResponse("Server returned zero count".to_owned()));
                    }
                    let len = CompactInteger::from(res.count);
                    let mut serialized = serialize(&len).take();
                    serialized.extend(res.hex.0.into_iter());
                    let mut reader = Reader::new_with_coin_variant(serialized.as_slice(), coin_variant);
                    let headers = reader.read_list::<BlockHeader>()?;
                    let mut timestamps: Vec<_> = headers.into_iter().map(|block| block.time).collect();
                    // can unwrap because count is non zero
                    Ok(median(timestamps.as_mut_slice()).unwrap())
                }),
        )
    }

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>> {
        let header_bytes = self.blockchain_block_header(height).compat().await?;
        let header: BlockHeader =
            deserialize(header_bytes.0.as_slice()).map_to_mm(|e| UtxoRpcError::InvalidResponse(format!("{:?}", e)))?;
        Ok(header.time as u64)
    }
}

#[cfg_attr(test, mockable)]
impl ElectrumClientImpl {
    pub fn new(coin_ticker: String, event_handlers: Vec<RpcTransportEventHandlerShared>) -> ElectrumClientImpl {
        let protocol_version = OrdRange::new(1.2, 1.4).unwrap();
        ElectrumClientImpl {
            coin_ticker,
            connections: AsyncMutex::new(vec![]),
            next_id: 0.into(),
            event_handlers,
            protocol_version,
            get_balance_concurrent_map: ConcurrentRequestMap::new(),
            list_unspent_concurrent_map: ConcurrentRequestMap::new(),
        }
    }

    #[cfg(test)]
    pub fn with_protocol_version(
        coin_ticker: String,
        event_handlers: Vec<RpcTransportEventHandlerShared>,
        protocol_version: OrdRange<f32>,
    ) -> ElectrumClientImpl {
        ElectrumClientImpl {
            protocol_version,
            ..ElectrumClientImpl::new(coin_ticker, event_handlers)
        }
    }
}

/// Helper function casting mpsc::Receiver as Stream.
fn rx_to_stream(rx: mpsc::Receiver<Vec<u8>>) -> impl Stream<Item = Vec<u8>, Error = io::Error> {
    rx.map_err(|_| panic!("errors not possible on rx"))
}

async fn electrum_process_json(raw_json: Json, arc: &JsonRpcPendingRequestsShared) {
    // detect if we got standard JSONRPC response or subscription response as JSONRPC request
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ElectrumRpcResponseEnum {
        /// The standard JSONRPC single response.
        SingleResponse(JsonRpcResponse),
        /// The batch of standard JSONRPC responses.
        BatchResponses(JsonRpcBatchResponse),
        /// The subscription response as JSONRPC request.
        SubscriptionNotification(JsonRpcRequest),
    }

    let response: ElectrumRpcResponseEnum = match json::from_value(raw_json) {
        Ok(res) => res,
        Err(e) => {
            error!("{}", e);
            return;
        },
    };

    let response = match response {
        ElectrumRpcResponseEnum::SingleResponse(single) => JsonRpcResponseEnum::Single(single),
        ElectrumRpcResponseEnum::BatchResponses(batch) => JsonRpcResponseEnum::Batch(batch),
        ElectrumRpcResponseEnum::SubscriptionNotification(req) => {
            let id = match req.method.as_ref() {
                BLOCKCHAIN_HEADERS_SUB_ID => BLOCKCHAIN_HEADERS_SUB_ID,
                _ => {
                    error!("Couldn't get id of request {:?}", req);
                    return;
                },
            };
            JsonRpcResponseEnum::Single(JsonRpcResponse {
                id: id.into(),
                jsonrpc: "2.0".into(),
                result: req.params[0].clone(),
                error: Json::Null,
            })
        },
    };

    // the corresponding sender may not exist, receiver may be dropped
    // these situations are not considered as errors so we just silently skip them
    let mut pending = arc.lock().await;
    if let Some(tx) = pending.remove(&response.rpc_id()) {
        tx.send(response).ok();
    }
}

async fn electrum_process_chunk(chunk: &[u8], arc: &JsonRpcPendingRequestsShared) {
    // we should split the received chunk because we can get several responses in 1 chunk.
    let split = chunk.split(|item| *item == b'\n');
    for chunk in split {
        // split returns empty slice if it ends with separator which is our case
        if !chunk.is_empty() {
            let raw_json: Json = match json::from_slice(chunk) {
                Ok(json) => json,
                Err(e) => {
                    error!("{}", e);
                    return;
                },
            };
            electrum_process_json(raw_json, arc).await
        }
    }
}

fn increase_delay(delay: &AtomicU64) {
    if delay.load(AtomicOrdering::Relaxed) < 60 {
        delay.fetch_add(5, AtomicOrdering::Relaxed);
    }
}

macro_rules! try_loop {
    ($e:expr, $addr: ident, $delay: ident) => {
        match $e {
            Ok(res) => res,
            Err(e) => {
                error!("{:?} error {:?}", $addr, e);
                increase_delay(&$delay);
                continue;
            },
        }
    };
}

/// The enum wrapping possible variants of underlying Streams
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::large_enum_variant)]
enum ElectrumStream {
    Tcp(TcpStream),
    Tls(TlsStream<TcpStream>),
}

#[cfg(not(target_arch = "wasm32"))]
impl AsRef<TcpStream> for ElectrumStream {
    fn as_ref(&self) -> &TcpStream {
        match self {
            ElectrumStream::Tcp(stream) => stream,
            ElectrumStream::Tls(stream) => stream.get_ref().0,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AsyncRead for ElectrumStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncRead::poll_read(Pin::new(stream), cx, buf),
            ElectrumStream::Tls(stream) => AsyncRead::poll_read(Pin::new(stream), cx, buf),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AsyncWrite for ElectrumStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_write(Pin::new(stream), cx, buf),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_write(Pin::new(stream), cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_flush(Pin::new(stream), cx),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_flush(Pin::new(stream), cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            ElectrumStream::Tcp(stream) => AsyncWrite::poll_shutdown(Pin::new(stream), cx),
            ElectrumStream::Tls(stream) => AsyncWrite::poll_shutdown(Pin::new(stream), cx),
        }
    }
}

const ELECTRUM_TIMEOUT: u64 = 60;

async fn electrum_last_chunk_loop(last_chunk: Arc<AtomicU64>) {
    loop {
        Timer::sleep(ELECTRUM_TIMEOUT as f64).await;
        let last = (last_chunk.load(AtomicOrdering::Relaxed) / 1000) as f64;
        if now_float() - last > ELECTRUM_TIMEOUT as f64 {
            warn!(
                "Didn't receive any data since {}. Shutting down the connection.",
                last as i64
            );
            break;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn rustls_client_config(unsafe_conf: bool) -> Arc<ClientConfig> {
    let mut cert_store = RootCertStore::empty();

    cert_store.add_server_trust_anchors(
        TLS_SERVER_ROOTS
            .0
            .iter()
            .map(|ta| OwnedTrustAnchor::from_subject_spki_name_constraints(ta.subject, ta.spki, ta.name_constraints)),
    );

    let mut tls_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(cert_store)
        .with_no_client_auth();

    if unsafe_conf {
        tls_config
            .dangerous()
            .set_certificate_verifier(Arc::new(NoCertificateVerification {}));
    }
    Arc::new(tls_config)
}

#[cfg(not(target_arch = "wasm32"))]
lazy_static! {
    static ref SAFE_TLS_CONFIG: Arc<ClientConfig> = rustls_client_config(false);
    static ref UNSAFE_TLS_CONFIG: Arc<ClientConfig> = rustls_client_config(true);
}

#[cfg(not(target_arch = "wasm32"))]
async fn connect_loop(
    config: ElectrumConfig,
    addr: String,
    responses: JsonRpcPendingRequestsShared,
    connection_tx: Arc<AsyncMutex<Option<mpsc::Sender<Vec<u8>>>>>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<(), ()> {
    let delay = Arc::new(AtomicU64::new(0));

    loop {
        let current_delay = delay.load(AtomicOrdering::Relaxed);
        if current_delay > 0 {
            Timer::sleep(current_delay as f64).await;
        };

        let socket_addr = try_loop!(addr_to_socket_addr(&addr), addr, delay);

        let connect_f = match config.clone() {
            ElectrumConfig::TCP => Either::Left(TcpStream::connect(&socket_addr).map_ok(ElectrumStream::Tcp)),
            ElectrumConfig::SSL {
                dns_name,
                skip_validation,
            } => {
                let tls_connector = if skip_validation {
                    TlsConnector::from(UNSAFE_TLS_CONFIG.clone())
                } else {
                    TlsConnector::from(SAFE_TLS_CONFIG.clone())
                };

                Either::Right(TcpStream::connect(&socket_addr).and_then(move |stream| {
                    // Can use `unwrap` cause `dns_name` is pre-checked.
                    let dns = ServerName::try_from(dns_name.as_str())
                        .map_err(|e| fomat!([e]))
                        .unwrap();
                    tls_connector.connect(dns, stream).map_ok(ElectrumStream::Tls)
                }))
            },
        };

        let stream = try_loop!(connect_f.await, addr, delay);
        try_loop!(stream.as_ref().set_nodelay(true), addr, delay);
        info!("Electrum client connected to {}", addr);
        try_loop!(event_handlers.on_connected(addr.clone()), addr, delay);
        let last_chunk = Arc::new(AtomicU64::new(now_ms()));
        let mut last_chunk_f = electrum_last_chunk_loop(last_chunk.clone()).boxed().fuse();

        let (tx, rx) = mpsc::channel(0);
        *connection_tx.lock().await = Some(tx);
        let rx = rx_to_stream(rx).inspect(|data| {
            // measure the length of each sent packet
            event_handlers.on_outgoing_request(data);
        });

        let (read, mut write) = tokio::io::split(stream);
        let recv_f = {
            let delay = delay.clone();
            let addr = addr.clone();
            let responses = responses.clone();
            let event_handlers = event_handlers.clone();
            async move {
                let mut buffer = String::with_capacity(1024);
                let mut buf_reader = BufReader::new(read);
                loop {
                    match buf_reader.read_line(&mut buffer).await {
                        Ok(c) => {
                            if c == 0 {
                                info!("EOF from {}", addr);
                                break;
                            }
                            // reset the delay if we've connected successfully and only if we received some data from connection
                            delay.store(0, AtomicOrdering::Relaxed);
                        },
                        Err(e) => {
                            error!("Error on read {} from {}", e, addr);
                            break;
                        },
                    };
                    // measure the length of each incoming packet
                    event_handlers.on_incoming_response(buffer.as_bytes());
                    last_chunk.store(now_ms(), AtomicOrdering::Relaxed);

                    electrum_process_chunk(buffer.as_bytes(), &responses).await;
                    buffer.clear();
                }
            }
        };
        let mut recv_f = Box::pin(recv_f).fuse();

        let send_f = {
            let addr = addr.clone();
            let mut rx = rx.compat();
            async move {
                while let Some(Ok(bytes)) = rx.next().await {
                    if let Err(e) = write.write_all(&bytes).await {
                        error!("Write error {} to {}", e, addr);
                    }
                }
            }
        };
        let mut send_f = Box::pin(send_f).fuse();
        macro_rules! reset_tx_and_continue {
            () => {
                info!("{} connection dropped", addr);
                *connection_tx.lock().await = None;
                increase_delay(&delay);
                continue;
            };
        }

        select! {
            _last_chunk = last_chunk_f => { reset_tx_and_continue!(); },
            _recv = recv_f => { reset_tx_and_continue!(); },
            _send = send_f => { reset_tx_and_continue!(); },
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn connect_loop(
    _config: ElectrumConfig,
    addr: String,
    responses: JsonRpcPendingRequestsShared,
    connection_tx: Arc<AsyncMutex<Option<mpsc::Sender<Vec<u8>>>>>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<(), ()> {
    use std::sync::atomic::AtomicUsize;

    lazy_static! {
        static ref CONN_IDX: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    }

    use mm2_net::wasm_ws::ws_transport;

    let delay = Arc::new(AtomicU64::new(0));
    loop {
        let current_delay = delay.load(AtomicOrdering::Relaxed);
        if current_delay > 0 {
            Timer::sleep(current_delay as f64).await;
        }

        let conn_idx = CONN_IDX.fetch_add(1, AtomicOrdering::Relaxed);
        let (mut transport_tx, mut transport_rx) = try_loop!(ws_transport(conn_idx, &addr).await, addr, delay);

        info!("Electrum client connected to {}", addr);
        try_loop!(event_handlers.on_connected(addr.clone()), addr, delay);

        let last_chunk = Arc::new(AtomicU64::new(now_ms()));
        let mut last_chunk_fut = electrum_last_chunk_loop(last_chunk.clone()).boxed().fuse();

        let (outgoing_tx, outgoing_rx) = mpsc::channel(0);
        *connection_tx.lock().await = Some(outgoing_tx);

        let incoming_fut = {
            let delay = delay.clone();
            let addr = addr.clone();
            let responses = responses.clone();
            let event_handlers = event_handlers.clone();
            async move {
                while let Some(incoming_res) = transport_rx.next().await {
                    last_chunk.store(now_ms(), AtomicOrdering::Relaxed);
                    match incoming_res {
                        Ok(incoming_json) => {
                            // reset the delay if we've connected successfully and only if we received some data from connection
                            delay.store(0, AtomicOrdering::Relaxed);
                            // measure the length of each incoming packet
                            let incoming_str = incoming_json.to_string();
                            event_handlers.on_incoming_response(incoming_str.as_bytes());

                            electrum_process_json(incoming_json, &responses).await;
                        },
                        Err(e) => {
                            error!("{} error: {:?}", addr, e);
                        },
                    }
                }
            }
        };
        let mut incoming_fut = Box::pin(incoming_fut).fuse();

        let outgoing_fut = {
            let addr = addr.clone();
            let mut outgoing_rx = rx_to_stream(outgoing_rx).compat();
            let event_handlers = event_handlers.clone();
            async move {
                while let Some(Ok(data)) = outgoing_rx.next().await {
                    let raw_json: Json = match json::from_slice(&data) {
                        Ok(js) => js,
                        Err(e) => {
                            error!("Error {} deserializing the outgoing data: {:?}", e, data);
                            continue;
                        },
                    };
                    // measure the length of each sent packet
                    event_handlers.on_outgoing_request(&data);

                    if let Err(e) = transport_tx.send(raw_json).await {
                        error!("Error sending to {}: {:?}", addr, e);
                    }
                }
            }
        };
        let mut outgoing_fut = Box::pin(outgoing_fut).fuse();

        macro_rules! reset_tx_and_continue {
            () => {
                info!("{} connection dropped", addr);
                *connection_tx.lock().await = None;
                increase_delay(&delay);
                continue;
            };
        }

        select! {
            _last_chunk = last_chunk_fut => { reset_tx_and_continue!(); },
            _incoming = incoming_fut => { reset_tx_and_continue!(); },
            _outgoing = outgoing_fut => { reset_tx_and_continue!(); },
        }
    }
}

/// Builds up the electrum connection, spawns endless loop that attempts to reconnect to the server
/// in case of connection errors
fn electrum_connect(
    addr: String,
    config: ElectrumConfig,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> ElectrumConnection {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let responses = Arc::new(AsyncMutex::new(JsonRpcPendingRequests::default()));
    let tx = Arc::new(AsyncMutex::new(None));

    let connect_loop = connect_loop(
        config.clone(),
        addr.clone(),
        responses.clone(),
        tx.clone(),
        event_handlers,
    );

    let connect_loop = select_func(connect_loop.boxed(), shutdown_rx.compat());
    spawn(connect_loop.map(|_| ()));
    ElectrumConnection {
        addr,
        config,
        tx,
        shutdown_tx: Some(shutdown_tx),
        responses,
        protocol_version: AsyncMutex::new(None),
    }
}

fn electrum_request(
    request: JsonRpcRequestEnum,
    tx: mpsc::Sender<Vec<u8>>,
    responses: JsonRpcPendingRequestsShared,
    timeout: u64,
) -> Box<dyn Future<Item = JsonRpcResponseEnum, Error = String> + Send + 'static> {
    let send_fut = async move {
        let mut json = try_s!(json::to_string(&request));
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Electrum request and responses must end with \n
            // https://electrumx.readthedocs.io/en/latest/protocol-basics.html#message-stream
            json.push('\n');
        }

        let (req_tx, resp_rx) = async_oneshot::channel();
        responses.lock().await.insert(request.rpc_id(), req_tx);
        try_s!(tx.send(json.into_bytes()).compat().await);
        let resps = try_s!(resp_rx.await);
        Ok(resps)
    };
    let send_fut = send_fut
        .boxed()
        .timeout(Duration::from_secs(timeout))
        .compat()
        .then(|res| match res {
            Ok(response) => response,
            Err(timeout_error) => ERR!("{}", timeout_error),
        })
        .map_err(|e| ERRL!("{}", e));
    Box::new(send_fut)
}

pub(crate) fn address_balance_from_unspent_map(
    address: &Address,
    unspent_map: &UnspentMap,
    decimals: u8,
) -> BigDecimal {
    let unspents = match unspent_map.get(address) {
        Some(unspents) => unspents,
        // If `balances` doesn't contain `address`, there are no unspents related to the address.
        // Consider the balance of that address equal to 0.
        None => return BigDecimal::from(0),
    };
    unspents.iter().fold(BigDecimal::from(0), |sum, unspent| {
        sum + big_decimal_from_sat_unsigned(unspent.value, decimals)
    })
}
