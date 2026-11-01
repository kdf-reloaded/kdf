use super::*;

#[derive(Clone, Deserialize, Debug)]
#[cfg_attr(test, derive(Default))]
pub struct NativeUnspent {
    pub txid: H256Json,
    pub vout: u32,
    pub address: String,
    pub account: Option<String>,
    #[serde(rename = "scriptPubKey")]
    pub script_pub_key: BytesJson,
    pub amount: MmNumber,
    pub confirmations: u64,
    pub spendable: bool,
}

#[derive(Clone, Deserialize, Debug)]
pub struct ValidateAddressRes {
    #[serde(rename = "isvalid")]
    pub is_valid: bool,
    pub address: String,
    #[serde(rename = "scriptPubKey")]
    pub script_pub_key: BytesJson,
    #[serde(rename = "segid")]
    pub seg_id: Option<u32>,
    #[serde(rename = "ismine")]
    pub is_mine: Option<bool>,
    #[serde(rename = "iswatchonly")]
    pub is_watch_only: Option<bool>,
    #[serde(rename = "isscript")]
    pub is_script: bool,
    pub account: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct ListTransactionsItem {
    pub account: Option<String>,
    #[serde(default)]
    pub address: String,
    pub category: String,
    pub amount: f64,
    pub vout: u64,
    #[serde(default)]
    pub fee: f64,
    #[serde(default)]
    pub confirmations: i64,
    #[serde(default)]
    pub blockhash: H256Json,
    #[serde(default)]
    pub blockindex: u64,
    /// Height of the block that holds the transaction; absent for an unconfirmed transaction and
    /// for a daemon that does not report it.
    #[serde(default)]
    pub blockheight: Option<u64>,
    #[serde(default)]
    pub txid: H256Json,
    pub timereceived: u64,
    #[serde(default)]
    pub walletconflicts: Vec<String>,
}

impl ListTransactionsItem {
    /// The block height of the listed transaction, or 0 (the "unconfirmed or unknown" value of
    /// a history row, which makes the history loop look the real height up) when the daemon
    /// does not report one.
    pub fn block_height(&self) -> u64 { self.blockheight.unwrap_or(0) }

    /// Checks if the transaction is conflicting.
    /// It means the transaction has conflicts or has negative confirmations.
    pub fn is_conflicting(&self) -> bool { self.confirmations < 0 || !self.walletconflicts.is_empty() }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReceivedByAddressItem {
    #[serde(default)]
    pub account: String,
    pub address: String,
    pub txids: Vec<H256Json>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EstimateSmartFeeRes {
    #[serde(rename = "feerate")]
    #[serde(default)]
    pub fee_rate: f64,
    #[serde(default)]
    pub errors: Vec<String>,
    pub blocks: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListSinceBlockRes {
    transactions: Vec<ListTransactionsItem>,
    #[serde(rename = "lastblock")]
    #[allow(dead_code)]
    last_block: H256Json,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct NetworkInfoLocalAddress {
    address: String,
    port: u16,
    score: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct NetworkInfoNetwork {
    name: String,
    limited: bool,
    reachable: bool,
    proxy: String,
    proxy_randomize_credentials: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct NetworkInfo {
    connections: u64,
    #[serde(rename = "localaddresses")]
    local_addresses: Vec<NetworkInfoLocalAddress>,
    #[serde(rename = "localservices")]
    local_services: String,
    networks: Vec<NetworkInfoNetwork>,
    #[serde(rename = "protocolversion")]
    protocol_version: u64,
    #[serde(rename = "relayfee")]
    relay_fee: BigDecimal,
    subversion: String,
    #[serde(rename = "timeoffset")]
    time_offset: i64,
    version: u64,
    warnings: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GetAddressInfoRes {
    // as of now we are interested in ismine and iswatchonly fields only, but this response contains much more info
    #[serde(rename = "ismine")]
    pub is_mine: bool,
    #[serde(rename = "iswatchonly")]
    pub is_watch_only: bool,
}

#[derive(Debug)]
pub enum EstimateFeeMethod {
    /// estimatefee, deprecated in many coins: https://bitcoincore.org/en/doc/0.16.0/rpc/util/estimatefee/
    Standard,
    /// estimatesmartfee added since 0.16.0 bitcoind RPC: https://bitcoincore.org/en/doc/0.16.0/rpc/util/estimatesmartfee/
    SmartFee,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BlockNonce {
    String(String),
    U64(u64),
}

#[derive(Debug, Deserialize)]
pub struct VerboseBlock {
    /// Block hash
    pub hash: H256Json,
    /// Number of confirmations. -1 if block is on the side chain
    pub confirmations: i64,
    /// Block size
    pub size: u32,
    /// Block size, excluding witness data
    pub strippedsize: Option<u32>,
    /// Block weight
    pub weight: Option<u32>,
    /// Block height
    pub height: Option<u32>,
    /// Block version
    pub version: u32,
    /// Block version as hex
    #[serde(rename = "versionHex")]
    pub version_hex: Option<String>,
    /// Merkle root of this block
    pub merkleroot: H256Json,
    /// Transactions ids
    pub tx: Vec<H256Json>,
    /// Block time in seconds since epoch (Jan 1 1970 GMT)
    pub time: u32,
    /// Median block time in seconds since epoch (Jan 1 1970 GMT)
    pub mediantime: Option<u32>,
    /// Block nonce
    pub nonce: BlockNonce,
    /// Block nbits
    pub bits: String,
    /// Block difficulty
    pub difficulty: f64,
    /// Expected number of hashes required to produce the chain up to this block (in hex)
    pub chainwork: H256Json,
    /// Hash of previous block
    pub previousblockhash: Option<H256Json>,
    /// Hash of next block
    pub nextblockhash: Option<H256Json>,
    #[serde(rename = "finalsaplingroot")]
    pub final_sapling_root: Option<H256Json>,
}

pub type RpcReqSub<T> = async_oneshot::Sender<Result<T, JsonRpcError>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ListUnspentArgs {
    min_conf: i32,
    max_conf: i32,
    addresses: Vec<String>,
}

/// RPC client for UTXO based coins
/// https://developer.bitcoin.org/reference/rpc/index.html - Bitcoin RPC API reference
/// Other coins have additional methods or miss some of these
/// This description will be updated with more info
pub struct NativeClientImpl {
    /// Name of coin the rpc client is intended to work with
    pub coin_ticker: String,
    /// The uri to send requests to
    pub uri: String,
    /// Value of Authorization header, e.g. "Basic base64(user:password)"
    pub auth: String,
    /// Transport event handlers
    pub event_handlers: Vec<RpcTransportEventHandlerShared>,
    pub request_id: AtomicU64,
    pub list_unspent_concurrent_map: ConcurrentRequestMap<ListUnspentArgs, Vec<NativeUnspent>>,
}

/// The debug form shows the daemon in its host-only form and the credentials as set, never their value (CRD ch.38
/// R38.6.9b, ch.35 R35.1.17).
impl fmt::Debug for NativeClientImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeClientImpl")
            .field("coin_ticker", &self.coin_ticker)
            .field("uri", &host_only(&self.uri))
            .field("auth", &SECRET_SET)
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl Default for NativeClientImpl {
    fn default() -> Self {
        NativeClientImpl {
            coin_ticker: "TEST".to_string(),
            uri: "".to_string(),
            auth: "".to_string(),
            event_handlers: vec![],
            request_id: Default::default(),
            list_unspent_concurrent_map: ConcurrentRequestMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeClient(pub Arc<NativeClientImpl>);
impl Deref for NativeClient {
    type Target = NativeClientImpl;
    fn deref(&self) -> &NativeClientImpl { &*self.0 }
}

/// The trait provides methods to generate the JsonRpcClient instance info such as name of coin.
pub trait UtxoJsonRpcClientInfo: JsonRpcClient {
    /// Name of coin the rpc client is intended to work with
    fn coin_name(&self) -> &str;

    /// Generate client info from coin name
    fn client_info(&self) -> String { format!("coin: {}", self.coin_name()) }
}

impl UtxoJsonRpcClientInfo for NativeClientImpl {
    fn coin_name(&self) -> &str { self.coin_ticker.as_str() }
}

impl JsonRpcClient for NativeClientImpl {
    fn version(&self) -> &'static str { "1.0" }

    fn next_id(&self) -> String { self.request_id.fetch_add(1, AtomicOrdering::Relaxed).to_string() }

    fn client_info(&self) -> String { UtxoJsonRpcClientInfo::client_info(self) }

    #[cfg(target_arch = "wasm32")]
    fn transport(&self, _request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        Box::new(futures01::future::err(ERRL!(
            "'NativeClientImpl' must be used in native mode only"
        )))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn transport(&self, request: JsonRpcRequestEnum) -> JsonRpcResponseFut {
        use mm2_net::transport::slurp_req;

        let request_body = try_fus!(json::to_string(&request));
        // measure now only body length, because the `hyper` crate doesn't allow to get total HTTP packet length
        self.event_handlers.on_outgoing_request(request_body.as_bytes());

        let uri = self.uri.clone();

        let http_request = try_fus!(Request::builder()
            .method("POST")
            .header(AUTHORIZATION, self.auth.clone())
            .uri(uri.clone())
            .body(Vec::from(request_body)));

        let event_handles = self.event_handlers.clone();
        let named_uri = uri.clone();
        Box::new(slurp_req(http_request).boxed().compat().then(
            move |result| -> Result<(JsonRpcRemoteAddr, JsonRpcResponseEnum), String> {
                // The daemon is named in its host-only form in every text (CRD ch.38 R38.6.9b item 1).
                let res = result.map_err(|e| scrub_node_url(&e.to_string(), &named_uri))?;
                // measure now only body length, because the `hyper` crate doesn't allow to get total HTTP packet length
                event_handles.on_incoming_response(&res.2);

                // The daemon's answer is never quoted in a text: only its status, size, and for a JSON-RPC error
                // answer the daemon's own error code and message (CRD ch.38 R38.6.9b item 4).
                if res.0 != StatusCode::OK {
                    return Err(ERRL!("{}", failed_answer_text(&request, res.0, &res.2, &named_uri)));
                }

                let body = unreadable_answer_text(&request, res.0, &res.2)?;
                let response = json::from_str(body).map_err(|e| {
                    ERRL!(
                        "Rpc request {} got an answer of {} bytes that is not a JSON-RPC response: {:?} error at line {} column {}",
                        request_name(&request),
                        res.2.len(),
                        e.classify(),
                        e.line(),
                        e.column()
                    )
                })?;
                Ok((uri.into(), response))
            },
        ))
    }
}

/// What a text names of a request: the method of a single request, or the count and the distinct methods of a
/// batch. Never its parameters (CRD ch.38 R38.6.9b item 4).
#[cfg(not(target_arch = "wasm32"))]
fn request_name(request: &JsonRpcRequestEnum) -> String {
    match request {
        JsonRpcRequestEnum::Single(single) => single.method.clone(),
        JsonRpcRequestEnum::Batch(batch) => {
            let methods: std::collections::BTreeSet<&str> = batch.methods().collect();
            format!("batch of {} requests ({})", batch.len(), methods.into_iter().join(", "))
        },
    }
}

/// The error text for a daemon's answer with a non-success HTTP status: the request's name, the status and the
/// size of the body, plus the `code` and `message` of the body's JSON-RPC `error` object when it has one. Nothing
/// else of the body is quoted, and nothing of the request's parameters (CRD ch.38 R38.6.9b item 4). The message is
/// scrubbed of the daemon's URL like every other text (R38.6.9b item 1).
#[cfg(not(target_arch = "wasm32"))]
fn failed_answer_text(request: &JsonRpcRequestEnum, status: StatusCode, body: &[u8], daemon_url: &str) -> String {
    let mut text = format!(
        "Rpc request {} failed with HTTP status code {}, response body of {} bytes",
        request_name(request),
        status,
        body.len()
    );
    // `from_slice` takes bytes that are not UTF-8 too: such a body just has no readable error object.
    if let Ok(Json::Object(answer)) = json::from_slice::<Json>(body) {
        if let Some(Json::Object(error)) = answer.get("error") {
            if let Some(code) = error.get("code").and_then(Json::as_i64) {
                text.push_str(&format!(", daemon error code {}", code));
            }
            if let Some(message) = error.get("message").and_then(Json::as_str) {
                text.push_str(&format!(
                    ", daemon error message: {}",
                    scrub_node_url(message, daemon_url)
                ));
            }
        }
    }
    text
}

/// The body of a success answer as text, or the error text when it is not UTF-8. The bytes are not quoted.
#[cfg(not(target_arch = "wasm32"))]
fn unreadable_answer_text<'a>(
    request: &JsonRpcRequestEnum,
    status: StatusCode,
    body: &'a [u8],
) -> Result<&'a str, String> {
    std::str::from_utf8(body).map_err(|_| {
        ERRL!(
            "Rpc request {} got an answer of {} bytes (HTTP status code {}) that is not UTF-8 text",
            request_name(request),
            body.len(),
            status
        )
    })
}

impl JsonRpcBatchClient for NativeClientImpl {}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoRpcClientOps for NativeClient {
    fn list_unspent(&self, address: &Address, decimals: u8) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let fut = self
            .list_unspent_impl(0, std::i32::MAX, vec![address.to_string()])
            .map_to_mm_fut(UtxoRpcError::from)
            .and_then(move |unspents| {
                let unspents: UtxoRpcResult<Vec<_>> = unspents
                    .into_iter()
                    .map(|unspent| {
                        Ok(UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.txid.reversed().into(),
                                index: unspent.vout,
                            },
                            value: sat_from_big_decimal(&unspent.amount.to_decimal(), decimals).mm_err(Into::into)?,
                            height: None,
                        })
                    })
                    .collect();
                unspents
            });
        Box::new(fut)
    }

    fn list_unspent_group(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<UnspentMap> {
        let mut addresses_str = Vec::with_capacity(addresses.len());
        let mut addresses_map = HashMap::with_capacity(addresses.len());
        for addr in addresses {
            let addr_str = addr.to_string();
            addresses_str.push(addr_str.clone());
            addresses_map.insert(addr_str, addr);
        }

        let fut = self
            .list_unspent_impl(0, std::i32::MAX, addresses_str)
            .map_to_mm_fut(UtxoRpcError::from)
            .and_then(move |unspents| {
                unspents
                    .into_iter()
                    // Convert `Vec<NativeUnspent>` into `UnspentMap`.
                    .map(|unspent| {
                        let orig_address = addresses_map
                            .get(&unspent.address)
                            .or_mm_err(|| {
                                UtxoRpcError::InvalidResponse(format!("Unexpected address '{}'", unspent.address))
                            })?
                            .clone();
                        let unspent_info = UnspentInfo {
                            outpoint: OutPoint {
                                hash: unspent.txid.reversed().into(),
                                index: unspent.vout,
                            },
                            value: sat_from_big_decimal(&unspent.amount.to_decimal(), decimals).mm_err(Into::into)?,
                            height: None,
                        };
                        Ok((orig_address, unspent_info))
                    })
                    // Collect `(Address, UnspentInfo)` items into `HashMap<Address, Vec<UnspentInfo>>` grouped by the addresses.
                    .try_into_group_map()
            });
        Box::new(fut)
    }

    fn send_transaction(&self, tx: &UtxoTx) -> UtxoRpcFut<H256Json> {
        let tx_bytes = if tx.has_witness() {
            BytesJson::from(serialize_with_flags(tx, SERIALIZE_TRANSACTION_WITNESS))
        } else {
            BytesJson::from(serialize(tx))
        };
        Box::new(self.send_raw_transaction(tx_bytes))
    }

    /// https://developer.bitcoin.org/reference/rpc/sendrawtransaction
    fn send_raw_transaction(&self, tx: BytesJson) -> UtxoRpcFut<H256Json> {
        Box::new(rpc_func!(self, "sendrawtransaction", tx).map_to_mm_fut(UtxoRpcError::from))
    }

    fn get_transaction_bytes(&self, txid: &H256Json) -> UtxoRpcFut<BytesJson> {
        Box::new(self.get_raw_transaction_bytes(txid).map_to_mm_fut(UtxoRpcError::from))
    }

    fn get_verbose_transaction(&self, txid: &H256Json) -> UtxoRpcFut<RpcTransaction> {
        Box::new(self.get_raw_transaction_verbose(txid).map_to_mm_fut(UtxoRpcError::from))
    }

    fn get_verbose_transactions(&self, tx_ids: &[H256Json]) -> UtxoRpcFut<Vec<RpcTransaction>> {
        Box::new(
            self.get_raw_transaction_verbose_batch(tx_ids)
                .map_to_mm_fut(UtxoRpcError::from),
        )
    }

    fn get_block_count(&self) -> UtxoRpcFut<u64> {
        Box::new(self.0.get_block_count().map_to_mm_fut(UtxoRpcError::from))
    }

    fn display_balance(&self, address: Address, _decimals: u8) -> RpcRes<BigDecimal> {
        Box::new(
            self.list_unspent_impl(0, std::i32::MAX, vec![address.to_string()])
                .map(|unspents| {
                    unspents
                        .iter()
                        .fold(BigDecimal::from(0), |sum, unspent| sum + unspent.amount.to_decimal())
                }),
        )
    }

    fn display_balances(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<Vec<(Address, BigDecimal)>> {
        let this = self.clone();
        let fut = async move {
            let unspent_map = this.list_unspent_group(addresses.clone(), decimals).compat().await?;
            let balances = addresses
                .into_iter()
                .map(|address| {
                    let balance = address_balance_from_unspent_map(&address, &unspent_map, decimals);
                    (address, balance)
                })
                .collect();
            Ok(balances)
        };
        Box::new(fut.boxed().compat())
    }

    fn estimate_fee_sat(
        &self,
        decimals: u8,
        fee_method: &EstimateFeeMethod,
        mode: &Option<EstimateFeeMode>,
        n_blocks: u32,
    ) -> UtxoRpcFut<u64> {
        match fee_method {
            EstimateFeeMethod::Standard => Box::new(self.estimate_fee(n_blocks).map(move |fee| {
                if fee > 0.00001 {
                    (fee * 10.0_f64.powf(decimals as f64)) as u64
                } else {
                    1000
                }
            })),
            EstimateFeeMethod::SmartFee => Box::new(self.estimate_smart_fee(mode, n_blocks).map(move |res| {
                if res.fee_rate > 0.00001 {
                    (res.fee_rate * 10.0_f64.powf(decimals as f64)) as u64
                } else {
                    1000
                }
            })),
        }
    }

    fn get_relay_fee(&self) -> RpcRes<BigDecimal> { Box::new(self.get_network_info().map(|info| info.relay_fee)) }

    fn find_output_spend(
        &self,
        tx_hash: H256,
        _script_pubkey: &[u8],
        vout: usize,
        from_block: BlockHashOrHeight,
    ) -> Box<dyn Future<Item = Option<SpentOutputInfo>, Error = String> + Send> {
        let selfi = self.clone();
        let fut = async move {
            let from_block_hash = match from_block {
                BlockHashOrHeight::Height(h) => try_s!(selfi.get_block_hash(h as u64).compat().await),
                BlockHashOrHeight::Hash(h) => h,
            };
            let list_since_block: ListSinceBlockRes = try_s!(selfi.list_since_block(from_block_hash).compat().await);
            for transaction in list_since_block
                .transactions
                .into_iter()
                .filter(|tx| !tx.is_conflicting())
            {
                let maybe_spend_tx_bytes = try_s!(selfi.get_raw_transaction_bytes(&transaction.txid).compat().await);
                let maybe_spend_tx: UtxoTx =
                    try_s!(deserialize(maybe_spend_tx_bytes.as_slice()).map_err(|e| ERRL!("{:?}", e)));

                for (index, input) in maybe_spend_tx.inputs.iter().enumerate() {
                    if input.previous_output.hash == tx_hash && input.previous_output.index == vout as u32 {
                        return Ok(Some(SpentOutputInfo {
                            spending_tx: maybe_spend_tx,
                            input_index: index,
                            spent_in_block: BlockHashOrHeight::Hash(transaction.blockhash),
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
        _coin_variant: CoinVariant,
    ) -> UtxoRpcFut<u32> {
        let selfi = self.clone();
        let fut = async move {
            let starting_block_hash = selfi.get_block_hash(starting_block).compat().await?;
            let starting_block_data = selfi.get_block(starting_block_hash).compat().await?;
            if let Some(median) = starting_block_data.mediantime {
                return Ok(median);
            }

            let mut block_timestamps = vec![starting_block_data.time];
            let from = if starting_block <= count.get() {
                0
            } else {
                starting_block - count.get() + 1
            };
            for block_n in from..starting_block {
                let block_hash = selfi.get_block_hash(block_n).compat().await?;
                let block_data = selfi.get_block(block_hash).compat().await?;
                block_timestamps.push(block_data.time);
            }
            // can unwrap because count is non zero
            Ok(median(block_timestamps.as_mut_slice()).unwrap())
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>> {
        let block = self.get_block_by_height(height).await?;
        Ok(block.time as u64)
    }
}

#[cfg_attr(test, mockable)]
impl NativeClient {
    /// https://developer.bitcoin.org/reference/rpc/listunspent
    pub fn list_unspent_impl(
        &self,
        min_conf: i32,
        max_conf: i32,
        addresses: Vec<String>,
    ) -> RpcRes<Vec<NativeUnspent>> {
        let request_fut = rpc_func!(self, "listunspent", &min_conf, &max_conf, &addresses);
        let arc = self.clone();
        let args = ListUnspentArgs {
            min_conf,
            max_conf,
            addresses,
        };
        let fut = async move { arc.list_unspent_concurrent_map.wrap_request(args, request_fut).await };
        Box::new(fut.boxed().compat())
    }

    pub fn list_all_transactions(&self, step: u64) -> RpcRes<Vec<ListTransactionsItem>> {
        let selfi = self.clone();
        let fut = async move {
            let mut from = 0;
            let mut transaction_list = Vec::new();

            loop {
                let transactions = selfi.list_transactions(step, from).compat().await?;
                if transactions.is_empty() {
                    return Ok(transaction_list);
                }

                transaction_list.extend(transactions.into_iter());
                from += step;
            }
        };
        Box::new(fut.boxed().compat())
    }
}

impl NativeClient {
    pub async fn get_block_by_height(&self, height: u64) -> UtxoRpcResult<VerboseBlock> {
        let block_hash = self.get_block_hash(height).compat().await?;
        self.get_block(block_hash).compat().await
    }
}

#[cfg_attr(test, mockable)]
impl NativeClientImpl {
    /// https://developer.bitcoin.org/reference/rpc/importaddress
    pub fn import_address(&self, address: &str, label: &str, rescan: bool) -> RpcRes<()> {
        rpc_func!(self, "importaddress", address, label, rescan)
    }

    /// https://developer.bitcoin.org/reference/rpc/validateaddress
    pub fn validate_address(&self, address: &str) -> RpcRes<ValidateAddressRes> {
        rpc_func!(self, "validateaddress", address)
    }

    pub fn output_amount(
        &self,
        txid: H256Json,
        index: usize,
    ) -> Box<dyn Future<Item = u64, Error = String> + Send + 'static> {
        let fut = self.get_raw_transaction_bytes(&txid).map_err(|e| ERRL!("{}", e));
        Box::new(fut.and_then(move |bytes| {
            let tx: UtxoTx = try_s!(deserialize(bytes.as_slice()).map_err(|e| ERRL!(
                "Error {:?} trying to deserialize the transaction {:?}",
                e,
                bytes
            )));
            Ok(tx.outputs[index].value)
        }))
    }

    /// https://developer.bitcoin.org/reference/rpc/getblock.html
    /// Always returns verbose block
    pub fn get_block(&self, hash: H256Json) -> UtxoRpcFut<VerboseBlock> {
        let verbose = true;
        Box::new(rpc_func!(self, "getblock", hash, verbose).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://developer.bitcoin.org/reference/rpc/getblockhash.html
    pub fn get_block_hash(&self, height: u64) -> UtxoRpcFut<H256Json> {
        Box::new(rpc_func!(self, "getblockhash", height).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://developer.bitcoin.org/reference/rpc/getblockcount.html
    pub fn get_block_count(&self) -> RpcRes<u64> { rpc_func!(self, "getblockcount") }

    /// https://developer.bitcoin.org/reference/rpc/getrawtransaction.html
    /// Always returns verbose transaction
    fn get_raw_transaction_verbose(&self, txid: &H256Json) -> RpcRes<RpcTransaction> {
        let verbose = 1;
        rpc_func!(self, "getrawtransaction", txid, verbose)
    }

    /// https://developer.bitcoin.org/reference/rpc/getrawtransaction.html
    /// Always returns verbose transactions in the same order they were requested.
    fn get_raw_transaction_verbose_batch(&self, tx_ids: &[H256Json]) -> RpcRes<Vec<RpcTransaction>> {
        let verbose = 1;
        let requests = tx_ids
            .iter()
            .map(|txid| rpc_req!(self, "getrawtransaction", txid, verbose));
        self.batch_rpc(requests)
    }

    /// https://developer.bitcoin.org/reference/rpc/getrawtransaction.html
    /// Always returns transaction bytes
    pub fn get_raw_transaction_bytes(&self, txid: &H256Json) -> RpcRes<BytesJson> {
        let verbose = 0;
        rpc_func!(self, "getrawtransaction", txid, verbose)
    }

    /// https://developer.bitcoin.org/reference/rpc/estimatefee.html
    /// It is recommended to set n_blocks as low as possible.
    /// However, in some cases, n_blocks = 1 leads to an unreasonably high fee estimation.
    /// https://github.com/KomodoPlatform/atomicDEX-API/issues/656#issuecomment-743759659
    pub fn estimate_fee(&self, n_blocks: u32) -> UtxoRpcFut<f64> {
        Box::new(rpc_func!(self, "estimatefee", n_blocks).map_to_mm_fut(UtxoRpcError::from))
    }

    /// https://developer.bitcoin.org/reference/rpc/estimatesmartfee.html
    /// It is recommended to set n_blocks as low as possible.
    /// However, in some cases, n_blocks = 1 leads to an unreasonably high fee estimation.
    /// https://github.com/KomodoPlatform/atomicDEX-API/issues/656#issuecomment-743759659
    pub fn estimate_smart_fee(&self, mode: &Option<EstimateFeeMode>, n_blocks: u32) -> UtxoRpcFut<EstimateSmartFeeRes> {
        match mode {
            Some(m) => Box::new(rpc_func!(self, "estimatesmartfee", n_blocks, m).map_to_mm_fut(UtxoRpcError::from)),
            None => Box::new(rpc_func!(self, "estimatesmartfee", n_blocks).map_to_mm_fut(UtxoRpcError::from)),
        }
    }

    /// https://developer.bitcoin.org/reference/rpc/listtransactions.html
    pub fn list_transactions(&self, count: u64, from: u64) -> RpcRes<Vec<ListTransactionsItem>> {
        let account = "*";
        let watch_only = true;
        rpc_func!(self, "listtransactions", account, count, from, watch_only)
    }

    /// https://developer.bitcoin.org/reference/rpc/listreceivedbyaddress.html
    pub fn list_received_by_address(
        &self,
        min_conf: u64,
        include_empty: bool,
        include_watch_only: bool,
    ) -> RpcRes<Vec<ReceivedByAddressItem>> {
        rpc_func!(
            self,
            "listreceivedbyaddress",
            min_conf,
            include_empty,
            include_watch_only
        )
    }

    pub fn detect_fee_method(&self) -> impl Future<Item = EstimateFeeMethod, Error = String> + Send {
        let estimate_fee_fut = self.estimate_fee(1);
        self.estimate_smart_fee(&None, 1).then(move |res| -> Box<dyn Future<Item=EstimateFeeMethod, Error=String> + Send> {
            match res {
                Ok(smart_fee) => if smart_fee.fee_rate > 0. {
                    Box::new(futures01::future::ok(EstimateFeeMethod::SmartFee))
                } else {
                    info!("fee_rate from smart fee should be above zero, but got {:?}, trying estimatefee", smart_fee);
                    Box::new(estimate_fee_fut.map_err(|e| ERRL!("{}", e)).and_then(|res| if res > 0. {
                        Ok(EstimateFeeMethod::Standard)
                    } else {
                        ERR!("Estimate fee result should be above zero, but got {}, consider setting txfee in config", res)
                    }))
                },
                Err(e) => {
                    error!("Error {} on estimate smart fee, trying estimatefee", e);
                    Box::new(estimate_fee_fut.map_err(|e| ERRL!("{}", e)).and_then(|res| if res > 0. {
                        Ok(EstimateFeeMethod::Standard)
                    } else {
                        ERR!("Estimate fee result should be above zero, but got {}, consider setting txfee in config", res)
                    }))
                }
            }
        })
    }

    /// https://developer.bitcoin.org/reference/rpc/listsinceblock.html
    /// uses default target confirmations 1 and always includes watch_only addresses
    pub fn list_since_block(&self, block_hash: H256Json) -> RpcRes<ListSinceBlockRes> {
        let target_confirmations = 1;
        let include_watch_only = true;
        rpc_func!(
            self,
            "listsinceblock",
            block_hash,
            target_confirmations,
            include_watch_only
        )
    }

    /// https://developer.bitcoin.org/reference/rpc/sendtoaddress.html
    pub fn send_to_address(&self, addr: &str, amount: &BigDecimal) -> RpcRes<H256Json> {
        rpc_func!(self, "sendtoaddress", addr, amount)
    }

    /// Returns the list of addresses assigned the specified label.
    /// https://developer.bitcoin.org/reference/rpc/getaddressesbylabel.html
    pub fn get_addresses_by_label(&self, label: &str) -> RpcRes<AddressesByLabelResult> {
        rpc_func!(self, "getaddressesbylabel", label)
    }

    /// https://developer.bitcoin.org/reference/rpc/getnetworkinfo.html
    pub fn get_network_info(&self) -> RpcRes<NetworkInfo> { rpc_func!(self, "getnetworkinfo") }

    /// https://developer.bitcoin.org/reference/rpc/getaddressinfo.html
    pub fn get_address_info(&self, address: &str) -> RpcRes<GetAddressInfoRes> {
        rpc_func!(self, "getaddressinfo", address)
    }

    /// https://developer.bitcoin.org/reference/rpc/getblockheader.html
    pub fn get_block_header_bytes(&self, block_hash: H256Json) -> RpcRes<BytesJson> {
        let verbose = 0;
        rpc_func!(self, "getblockheader", block_hash, verbose)
    }
}

impl NativeClientImpl {
    /// Check whether input address is imported to daemon
    pub async fn is_address_imported(&self, address: &str) -> Result<bool, String> {
        let validate_res = try_s!(self.validate_address(address).compat().await);
        match (validate_res.is_mine, validate_res.is_watch_only) {
            (Some(is_mine), Some(is_watch_only)) => Ok(is_mine || is_watch_only),
            // ignoring (Some(_), None) and (None, Some(_)) variants, there seem to be no known daemons that return is_mine,
            // but do not return is_watch_only, so it's ok to fallback to getaddressinfo
            _ => {
                let address_info = try_s!(self.get_address_info(address).compat().await);
                Ok(address_info.is_mine || address_info.is_watch_only)
            },
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod node_naming_tests {
    use super::*;

    fn client() -> NativeClient {
        NativeClient(Arc::new(NativeClientImpl {
            coin_ticker: "RIN".to_owned(),
            uri: "http://127.0.0.1:1".to_owned(),
            // "Basic base64(rpcuser:rpcsecret)"
            auth: "Basic cnBjdXNlcjpycGNzZWNyZXQ=".to_owned(),
            ..Default::default()
        }))
    }

    /// T38.6.9.2 (R38.6.9b item 2): the debug form shows neither the credentials nor the header value.
    #[test]
    fn the_debug_form_of_the_native_client_hides_the_credentials() {
        let client = client();
        let utxo_client = UtxoRpcClientEnum::Native(client.clone());
        for rendered in [
            format!("{client:?}"),
            format!("{:#?}", client.0),
            format!("{utxo_client:?}"),
        ] {
            assert!(rendered.contains("http://127.0.0.1:1"), "{rendered}");
            for secret in ["rpcuser", "rpcsecret", "cnBjdXNlcjpycGNzZWNyZXQ=", "Basic"] {
                assert!(!rendered.contains(secret), "a credential is rendered");
            }
        }
    }

    /// T38.6.9.2 (R38.6.9b item 1): a request to a closed port names the daemon host-only and nothing more.
    #[test]
    fn a_failing_request_names_the_daemon_host_only() {
        let error = common::block_on(client().get_block_count().compat())
            .unwrap_err()
            .to_string();
        assert!(error.contains("http://127.0.0.1:1"), "{error}");
        assert!(!error.contains("http://127.0.0.1:1/"), "{error}");
    }
}

/// T38.6.9.3 (CRD ch.38 R38.6.9b item 4): an error text names the method, the status and the size of the daemon's
/// answer, and the daemon's own error code and message; it never quotes the answer or the request's parameters.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod daemon_answer_tests {
    use super::*;
    use crate::answer_stub::{http_response, serve};
    use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};

    /// Carried by the request's parameters, never to appear in a text.
    const REQUEST_MARKER: &str = "REQPARAMMARKER";
    /// Carried by the daemon's answer, never to appear in a text.
    const BODY_MARKER: &str = "DAEMONBODYMARKER";

    fn client_for(port: u16) -> NativeClient {
        NativeClient(Arc::new(NativeClientImpl {
            coin_ticker: "RIN".to_owned(),
            uri: format!("http://127.0.0.1:{port}/?key=URLMARKER"),
            auth: "Basic cnBjdXNlcjpycGNzZWNyZXQ=".to_owned(),
            ..Default::default()
        }))
    }

    /// The error text of a request that carries [`REQUEST_MARKER`] to a daemon that answers with `reply`.
    fn error_text(reply: Vec<u8>) -> String {
        let port = serve(move |_| reply.clone());
        let client = client_for(port);
        common::block_on(client.get_address_info(REQUEST_MARKER).compat())
            .unwrap_err()
            .to_string()
    }

    fn assert_no_marker_logged() {
        for line in captured_log_lines() {
            assert!(
                !line.contains(BODY_MARKER) && !line.contains(REQUEST_MARKER),
                "a log line carries a marker: {line}"
            );
        }
    }

    #[test]
    fn t38_6_9_3_a_json_rpc_error_answer_gives_status_code_message_and_size_only() {
        install_log_capture();
        let body = format!(
            r#"{{"result":null,"error":{{"code":-26,"message":"rejected by the daemon","data":"{BODY_MARKER}"}},"id":"0","extra":"{BODY_MARKER}"}}"#
        );
        let error = error_text(http_response(
            "500 Internal Server Error",
            "application/json",
            body.as_bytes(),
        ));
        assert!(error.contains("getaddressinfo"), "{error}");
        assert!(error.contains("500"), "{error}");
        assert!(error.contains("-26"), "{error}");
        assert!(error.contains("rejected by the daemon"), "{error}");
        assert!(error.contains(&format!("{} bytes", body.len())), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
        assert!(!error.contains(REQUEST_MARKER), "{error}");
        assert!(!error.contains("URLMARKER"), "{error}");
        assert_no_marker_logged();
    }

    #[test]
    fn t38_6_9_3_an_html_answer_gives_status_and_size_only() {
        install_log_capture();
        let body = format!("<html><body>Unauthorized {BODY_MARKER}</body></html>");
        let error = error_text(http_response("401 Unauthorized", "text/html", body.as_bytes()));
        assert!(error.contains("getaddressinfo"), "{error}");
        assert!(error.contains("401"), "{error}");
        assert!(error.contains(&format!("{} bytes", body.len())), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
        assert!(!error.contains(REQUEST_MARKER), "{error}");
        assert_no_marker_logged();
    }

    #[test]
    fn t38_6_9_3_an_error_object_without_a_code_or_message_gives_neither() {
        let body = format!(r#"{{"result":null,"error":{{"data":"{BODY_MARKER}"}},"id":"0"}}"#);
        let error = error_text(http_response(
            "500 Internal Server Error",
            "application/json",
            body.as_bytes(),
        ));
        assert!(
            error.contains("500") && error.contains(&format!("{} bytes", body.len())),
            "{error}"
        );
        assert!(!error.contains("daemon error"), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
    }

    #[test]
    fn t38_6_9_3_a_success_answer_that_is_not_json_is_not_quoted() {
        install_log_capture();
        let body = format!("this is not json {BODY_MARKER}");
        let error = error_text(http_response("200 OK", "text/plain", body.as_bytes()));
        assert!(error.contains("getaddressinfo"), "{error}");
        assert!(error.contains(&format!("{} bytes", body.len())), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
        assert!(!error.contains(REQUEST_MARKER), "{error}");
        assert_no_marker_logged();
    }

    #[test]
    fn t38_6_9_3_a_success_answer_that_is_not_utf8_is_not_quoted() {
        let mut body = BODY_MARKER.as_bytes().to_vec();
        body.extend_from_slice(&[0xff, 0xfe]);
        let error = error_text(http_response("200 OK", "application/octet-stream", &body));
        assert!(error.contains(&format!("{} bytes", body.len())), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
        // The same holds for a failing status.
        let error = error_text(http_response(
            "500 Internal Server Error",
            "application/octet-stream",
            &body,
        ));
        assert!(
            error.contains("500") && error.contains(&format!("{} bytes", body.len())),
            "{error}"
        );
        assert!(!error.contains(BODY_MARKER), "{error}");
    }

    #[test]
    fn t38_6_9_3_a_result_of_the_wrong_shape_is_not_quoted() {
        install_log_capture();
        let body = format!(r#"{{"result":"{BODY_MARKER}","error":null,"id":"0"}}"#);
        let error = error_text(http_response("200 OK", "application/json", body.as_bytes()));
        assert!(error.contains("getaddressinfo"), "{error}");
        assert!(!error.contains(BODY_MARKER), "{error}");
        assert_no_marker_logged();
    }

    #[test]
    fn t38_6_9_3_a_batch_request_is_named_by_its_count_and_methods() {
        let body = format!(
            r#"{{"result":null,"error":{{"code":-32600,"message":"bad batch"}},"id":null,"x":"{BODY_MARKER}"}}"#
        );
        let reply = http_response("500 Internal Server Error", "application/json", body.as_bytes());
        let port = serve(move |_| reply.clone());
        let client = client_for(port);
        let requests = vec![
            rpc_req!(client, "getblockhash", REQUEST_MARKER),
            rpc_req!(client, "getblockhash", REQUEST_MARKER),
            rpc_req!(client, "getblock", REQUEST_MARKER),
        ];
        let error = common::block_on(client.batch_rpc::<_, Json>(requests).compat())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("batch of 3 requests (getblock, getblockhash)"),
            "{error}"
        );
        assert!(
            error.contains("500") && error.contains("bad batch") && error.contains("-32600"),
            "{error}"
        );
        assert!(
            !error.contains(BODY_MARKER) && !error.contains(REQUEST_MARKER),
            "{error}"
        );
    }

    /// The message of the daemon's error is subject to R38.6.9b item 1 like any text: the daemon's URL in it is
    /// reduced to its host-only form.
    #[test]
    fn t38_6_9_3_the_daemon_url_in_its_message_is_host_only() {
        let port = serve(|_| Vec::new());
        let url = format!("http://127.0.0.1:{port}/?key=URLMARKER");
        let body = format!(r#"{{"error":{{"code":-1,"message":"cannot reach {url}"}}}}"#);
        let text = failed_answer_text(
            &JsonRpcRequestEnum::Single(JsonRpcRequest {
                jsonrpc: "1.0".into(),
                id: "0".into(),
                method: "getinfo".into(),
                params: vec![],
            }),
            StatusCode::INTERNAL_SERVER_ERROR,
            body.as_bytes(),
            &url,
        );
        assert!(text.contains(&format!("http://127.0.0.1:{port}")), "{text}");
        assert!(!text.contains("URLMARKER"), "{text}");
    }
}
