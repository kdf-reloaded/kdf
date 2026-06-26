//! TRON HTTP API client with multi-node failover.
//!
//! All TRON full-node HTTP API endpoints use POST with JSON bodies.
//! The client rotates through configured nodes, promoting successful
//! nodes to the front of the list.

use derive_more::Display;
use mm2_err_handle::prelude::*;
use mm2_net::transport::{fetch_json, post_json, SlurpError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::address::TronAddress;
use super::proto::TaposBlockData;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors specific to TRON HTTP API calls.
#[derive(Debug, Display)]
pub enum TronApiError {
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Timeout: {}", _0)]
    Timeout(String),
    #[display(fmt = "Remote error ({}): {}", code, message)]
    RemoteError { code: String, message: String },
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    #[display(fmt = "All nodes failed. Last error: {}", _0)]
    AllNodesFailed(String),
    #[display(fmt = "No event-indexer endpoint configured")]
    NoEventIndexer,
}

impl std::error::Error for TronApiError {}

impl From<MmError<SlurpError>> for TronApiError {
    fn from(e: MmError<SlurpError>) -> Self {
        match e.into_inner() {
            SlurpError::Transport { error, .. } => TronApiError::Transport(error),
            SlurpError::Timeout { error, .. } => TronApiError::Timeout(error),
            other => TronApiError::InvalidResponse(other.to_string()),
        }
    }
}

/// Whether a TRON API error is retryable on the next node.
fn is_retryable(e: &TronApiError) -> bool { matches!(e, TronApiError::Transport(_) | TronApiError::Timeout(_)) }

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// `/wallet/getnowblock` response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetNowBlockResponse {
    #[serde(rename = "blockID")]
    pub block_id: String,
    pub block_header: BlockHeader,
}

#[derive(Debug, Deserialize)]
pub struct BlockHeader {
    pub raw_data: BlockHeaderRawData,
}

#[derive(Debug, Deserialize)]
pub struct BlockHeaderRawData {
    pub number: u64,
    pub timestamp: i64,
}

impl GetNowBlockResponse {
    /// Extract TAPOS data from this block.
    pub fn to_tapos(&self) -> Result<TaposBlockData, TronApiError> {
        let block_id_bytes = hex::decode(&self.block_id)
            .map_err(|e| TronApiError::InvalidResponse(format!("bad blockID hex: {}", e)))?;
        if block_id_bytes.len() != 32 {
            return Err(TronApiError::InvalidResponse(format!(
                "blockID must be 32 bytes, got {}",
                block_id_bytes.len()
            )));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(&block_id_bytes);
        Ok(super::tx_builder::tapos_from_block(
            self.block_header.raw_data.number,
            &id,
        ))
    }
}

/// `/wallet/getaccount` request.
#[derive(Serialize)]
pub struct GetAccountRequest {
    pub address: String,
    pub visible: bool,
}

/// `/wallet/getaccount` response (existing account).
#[derive(Debug, Deserialize)]
pub struct AccountInfo {
    /// TRX balance in SUN.
    #[serde(default)]
    pub balance: i64,
    /// Account creation timestamp.
    pub create_time: Option<i64>,
}

/// `/wallet/triggerconstantcontract` request (read-only call).
#[derive(Serialize)]
pub struct TriggerConstantContractRequest {
    pub owner_address: String,
    pub contract_address: String,
    pub function_selector: String,
    pub parameter: String,
    pub visible: bool,
}

/// `/wallet/triggerconstantcontract` response.
#[derive(Debug, Deserialize)]
pub struct TriggerConstantContractResponse {
    pub constant_result: Option<Vec<String>>,
    pub energy_used: Option<u64>,
}

/// `/wallet/broadcasthex` request.
#[derive(Serialize)]
pub struct BroadcastHexRequest {
    pub transaction: String,
}

/// `/wallet/broadcasthex` response.
#[derive(Debug, Deserialize)]
pub struct BroadcastHexResponse {
    pub result: Option<bool>,
    pub txid: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// `/wallet/gettransactionbyid` request.
#[derive(Serialize)]
pub struct TxByIdRequest {
    pub value: String,
}

/// `/wallet/gettransactioninfobyid` response.
#[derive(Debug, Deserialize)]
pub struct TransactionInfoResponse {
    pub id: Option<String>,
    #[serde(rename = "blockNumber")]
    pub block_number: Option<u64>,
    pub receipt: Option<TransactionReceipt>,
    pub fee: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct TransactionReceipt {
    pub energy_usage_total: Option<u64>,
    pub net_fee: Option<i64>,
    pub energy_fee: Option<i64>,
    pub result: Option<String>,
}

/// `/wallet/getchainparameters` response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChainParametersResponse {
    pub chain_parameter: Vec<ChainParameter>,
}

#[derive(Debug, Deserialize)]
pub struct ChainParameter {
    pub key: String,
    pub value: Option<i64>,
}

/// `/wallet/getaccountresource` request.
#[derive(Serialize)]
pub struct GetAccountResourceRequest {
    pub address: String,
    pub visible: bool,
}

// ---------------------------------------------------------------------------
// Indexed contract-event query (§21.9, R-L6)
// ---------------------------------------------------------------------------

/// Filter parameters for the indexed contract-event query
/// `GET /v1/contracts/{address}/events` (the public TronGrid-style event-indexer
/// HTTP API). All fields are optional filters; `fingerprint` is the opaque
/// cursor returned by a previous page for forward pagination.
#[derive(Clone, Debug, Default)]
pub struct ContractEventsQuery<'a> {
    /// Restrict to a single event name (e.g. `ReceiverSpent`).
    pub event_name: Option<&'a str>,
    /// Only return events from blocks the indexer considers solidified/confirmed.
    pub only_confirmed: bool,
    /// Lower bound on block number.
    pub min_block: Option<u64>,
    /// Page size (the indexer caps this server-side; we request a sane default).
    pub limit: u32,
    /// Opaque pagination cursor from a previous response's `meta.fingerprint`.
    pub fingerprint: Option<&'a str>,
}

/// A single decoded contract event as returned by the event indexer. The
/// `result` map carries each event parameter as a named hex/decimal string
/// (e.g. `{"id": "..", "secret": ".."}`), normalised by
/// [`super::swap::decode_event_from_indexer`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContractEvent {
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default)]
    pub event_name: String,
    #[serde(default)]
    pub block_number: u64,
    #[serde(default)]
    pub result: HashMap<String, String>,
}

/// Pagination metadata of a contract-event query response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContractEventsMeta {
    /// Cursor for the next page; absent once the last page has been returned.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// `GET /v1/contracts/{address}/events` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContractEventsResponse {
    #[serde(default)]
    pub data: Vec<ContractEvent>,
    #[serde(default)]
    pub meta: ContractEventsMeta,
}

/// Build the `GET /v1/contracts/{address}/events` request URL with its query
/// string from the event-indexer base URL, the contract address, and the
/// filter parameters. Pure (no I/O) so the request construction is unit-testable
/// offline.
pub(crate) fn build_contract_events_url(base: &str, contract: &str, query: &ContractEventsQuery<'_>) -> String {
    let base = base.trim_end_matches('/');
    let mut url = format!("{base}/v1/contracts/{contract}/events");
    let mut params: Vec<String> = Vec::new();
    if let Some(name) = query.event_name {
        params.push(format!("event_name={name}"));
    }
    if query.only_confirmed {
        params.push("only_confirmed=true".to_owned());
    }
    if let Some(min_block) = query.min_block {
        params.push(format!("min_block_number={min_block}"));
    }
    if query.limit > 0 {
        params.push(format!("limit={}", query.limit));
    }
    if let Some(fp) = query.fingerprint {
        params.push(format!("fingerprint={fp}"));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

// ---------------------------------------------------------------------------
// API Client
// ---------------------------------------------------------------------------

/// A TRON HTTP API client that rotates through configured nodes.
pub struct TronApiClient {
    /// List of TRON full-node API URLs (e.g., `https://api.trongrid.io`).
    nodes: Arc<async_std::sync::Mutex<Vec<String>>>,
    /// Base URL of the indexed contract-event service (the TronGrid-style event
    /// indexer, distinct from the bare full node), used for swap spend/refund
    /// discovery (§21.9, R-L6). `None` when no event endpoint is configured, in
    /// which case discovery fails with a typed error rather than silently
    /// reporting "not found".
    event_indexer: Option<String>,
}

impl std::fmt::Debug for TronApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // We can't lock the mutex synchronously here; just record the type.
        write!(f, "TronApiClient {{ <nodes hidden> }}")
    }
}

impl TronApiClient {
    /// Create a new client from a list of node base URLs and an optional
    /// event-indexer base URL (§21.9, R-L6).
    pub fn new(node_urls: Vec<String>, event_indexer: Option<String>) -> Self {
        TronApiClient {
            nodes: Arc::new(async_std::sync::Mutex::new(node_urls)),
            event_indexer,
        }
    }

    /// Whether an indexed contract-event endpoint is configured (R-L6).
    pub fn has_event_indexer(&self) -> bool { self.event_indexer.is_some() }

    /// Execute an HTTP POST to a TRON endpoint, rotating through nodes on
    /// retryable errors. On success, the successful node is promoted to
    /// the front of the list.
    async fn try_post<Req, Resp>(&self, path: &str, request: &Req) -> Result<Resp, TronApiError>
    where
        Req: Serialize,
        Resp: serde::de::DeserializeOwned + Send + 'static,
    {
        let json_body = serde_json::to_string(request).map_err(|e| TronApiError::InvalidResponse(e.to_string()))?;

        let mut nodes = self.nodes.lock().await;
        let mut last_err = TronApiError::AllNodesFailed("no nodes configured".to_string());

        for i in 0..nodes.len() {
            let url = format!("{}{}", nodes[i], path);
            match post_json::<Resp>(&url, json_body.clone()).await {
                Ok(resp) => {
                    // Promote this node to front.
                    if i > 0 {
                        nodes.rotate_left(i);
                    }
                    return Ok(resp);
                },
                Err(e) => {
                    let api_err = TronApiError::from(e);
                    if !is_retryable(&api_err) {
                        return Err(api_err);
                    }
                    last_err = api_err;
                },
            }
        }

        Err(TronApiError::AllNodesFailed(last_err.to_string()))
    }

    /// Get the current block (for TAPOS data and timestamp).
    pub async fn get_now_block(&self) -> Result<GetNowBlockResponse, TronApiError> {
        #[derive(Serialize)]
        struct Empty {}
        self.try_post("/wallet/getnowblock", &Empty {}).await
    }

    /// Get account info. Returns `None` if the account does not exist on-chain.
    pub async fn get_account(&self, address: &TronAddress) -> Result<Option<AccountInfo>, TronApiError> {
        let req = GetAccountRequest {
            address: address.to_base58(),
            visible: true,
        };
        // TRON returns `{}` for non-existent accounts, which deserializes to
        // AccountInfo with defaults. We detect this via missing create_time.
        let resp: AccountInfo = self.try_post("/wallet/getaccount", &req).await?;
        if resp.create_time.is_none() {
            Ok(None)
        } else {
            Ok(Some(resp))
        }
    }

    /// Call a smart contract in read-only mode (no broadcast).
    pub async fn trigger_constant_contract(
        &self,
        owner_address: &TronAddress,
        contract_address: &TronAddress,
        function_selector: &str,
        parameter_hex: &str,
    ) -> Result<TriggerConstantContractResponse, TronApiError> {
        let req = TriggerConstantContractRequest {
            owner_address: owner_address.to_base58(),
            contract_address: contract_address.to_base58(),
            function_selector: function_selector.to_string(),
            parameter: parameter_hex.to_string(),
            visible: true,
        };
        self.try_post("/wallet/triggerconstantcontract", &req).await
    }

    /// Broadcast a signed transaction (protobuf hex).
    pub async fn broadcast_hex(&self, tx_hex: &str) -> Result<BroadcastHexResponse, TronApiError> {
        let req = BroadcastHexRequest {
            transaction: tx_hex.to_string(),
        };
        let resp: BroadcastHexResponse = self.try_post("/wallet/broadcasthex", &req).await?;
        if resp.result == Some(false) {
            return Err(TronApiError::RemoteError {
                code: resp.code.unwrap_or_default(),
                message: resp.message.unwrap_or_default(),
            });
        }
        Ok(resp)
    }

    /// Get transaction receipt/execution info by hash.
    pub async fn get_transaction_info_by_id(&self, tx_hash: &str) -> Result<TransactionInfoResponse, TronApiError> {
        let req = TxByIdRequest {
            value: tx_hash.to_string(),
        };
        self.try_post("/wallet/gettransactioninfobyid", &req).await
    }

    /// Get chain parameters (fee prices).
    pub async fn get_chain_parameters(&self) -> Result<GetChainParametersResponse, TronApiError> {
        #[derive(Serialize)]
        struct Empty {}
        self.try_post("/wallet/getchainparameters", &Empty {}).await
    }

    /// Get account resource quotas (bandwidth, energy).
    pub async fn get_account_resource(&self, address: &TronAddress) -> Result<serde_json::Value, TronApiError> {
        let req = GetAccountResourceRequest {
            address: address.to_base58(),
            visible: true,
        };
        self.try_post("/wallet/getaccountresource", &req).await
    }

    /// Query the indexed contract-event endpoint for a contract's events
    /// (§21.9, R-L6): `GET /v1/contracts/{address}/events`. TRON does not expose
    /// Ethereum-style log filtering, so swap spend/refund discovery is
    /// event-indexer based. Fails with [`TronApiError::NoEventIndexer`] when no
    /// event endpoint is configured.
    pub async fn get_contract_events(
        &self,
        contract_base58: &str,
        query: &ContractEventsQuery<'_>,
    ) -> Result<ContractEventsResponse, TronApiError> {
        let base = self.event_indexer.as_deref().ok_or(TronApiError::NoEventIndexer)?;
        let url = build_contract_events_url(base, contract_base58, query);
        fetch_json::<ContractEventsResponse>(&url)
            .await
            .map_err(TronApiError::from)
    }
}

impl Clone for TronApiClient {
    fn clone(&self) -> Self {
        TronApiClient {
            nodes: Arc::clone(&self.nodes),
            event_indexer: self.event_indexer.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_now_block_response_to_tapos() {
        let resp = GetNowBlockResponse {
            block_id: "0000000003456789abcdef0123456789abcdef0123456789abcdef0123456789".to_string(),
            block_header: BlockHeader {
                raw_data: BlockHeaderRawData {
                    number: 0x03456789,
                    timestamp: 1700000000000,
                },
            },
        };
        let tapos = resp.to_tapos().unwrap();
        // block_num = 0x03456789 → big-endian bytes: [00,00,00,00,03,45,67,89]
        // ref_block_bytes = last 2 = [0x67, 0x89]
        assert_eq!(tapos.ref_block_bytes, vec![0x67, 0x89]);
        // ref_block_hash = block_id bytes[8..16]
        let id_bytes = hex::decode(&resp.block_id).unwrap();
        assert_eq!(tapos.ref_block_hash, id_bytes[8..16].to_vec());
    }

    #[test]
    fn test_broadcast_response_serialize() {
        let json = r#"{"result":true,"txid":"abc123"}"#;
        let resp: BroadcastHexResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result, Some(true));
        assert_eq!(resp.txid.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_account_info_empty_json() {
        // TRON returns {} for non-existent accounts.
        let json = "{}";
        let info: AccountInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.balance, 0);
        assert!(info.create_time.is_none());
    }

    #[test]
    fn test_account_info_existing() {
        let json = r#"{"balance":1000000,"create_time":1700000000000}"#;
        let info: AccountInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.balance, 1_000_000);
        assert_eq!(info.create_time, Some(1700000000000));
    }

    #[test]
    fn test_chain_parameters_deserialize() {
        let json =
            r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420}]}"#;
        let resp: GetChainParametersResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.chain_parameter.len(), 2);
        assert_eq!(resp.chain_parameter[0].key, "getTransactionFee");
        assert_eq!(resp.chain_parameter[0].value, Some(1000));
    }

    #[test]
    fn test_transaction_info_response_deserialize() {
        let json = r#"{"id":"abc","blockNumber":64844180,"receipt":{"energy_usage_total":685,"net_fee":345000,"result":"SUCCESS"},"fee":345000}"#;
        let resp: TransactionInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.block_number, Some(64844180));
        assert_eq!(resp.fee, Some(345000));
        let receipt = resp.receipt.unwrap();
        assert_eq!(receipt.result.as_deref(), Some("SUCCESS"));
        assert_eq!(receipt.energy_usage_total, Some(685));
    }

    #[test]
    fn test_trigger_constant_response_deserialize() {
        let json = r#"{"constant_result":["0000000000000000000000000000000000000000000000000000000005f5e100"],"energy_used":685}"#;
        let resp: TriggerConstantContractResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.constant_result.unwrap().len(), 1);
        assert_eq!(resp.energy_used, Some(685));
    }

    // ---- R-L6 indexed contract-event query ----

    #[test]
    fn test_build_contract_events_url_full() {
        let q = ContractEventsQuery {
            event_name: Some("ReceiverSpent"),
            only_confirmed: true,
            min_block: Some(123),
            limit: 50,
            fingerprint: Some("CURSOR"),
        };
        let url = build_contract_events_url("https://api.trongrid.io/", "TSwapContractAddr", &q);
        assert_eq!(
            url,
            "https://api.trongrid.io/v1/contracts/TSwapContractAddr/events?event_name=ReceiverSpent&only_confirmed=true&min_block_number=123&limit=50&fingerprint=CURSOR"
        );
    }

    #[test]
    fn test_build_contract_events_url_minimal() {
        let q = ContractEventsQuery::default();
        let url = build_contract_events_url("https://example.org", "TAddr", &q);
        // No filters => bare events path with no query string.
        assert_eq!(url, "https://example.org/v1/contracts/TAddr/events");
    }

    #[test]
    fn test_build_contract_events_url_event_only() {
        let q = ContractEventsQuery {
            event_name: Some("SenderRefunded"),
            limit: 10,
            ..Default::default()
        };
        let url = build_contract_events_url("https://example.org/", "TAddr", &q);
        assert_eq!(
            url,
            "https://example.org/v1/contracts/TAddr/events?event_name=SenderRefunded&limit=10"
        );
    }

    #[test]
    fn test_contract_events_response_deserialize() {
        let json = r#"{
            "data":[
                {"transaction_id":"abc123","event_name":"ReceiverSpent","block_number":42,
                 "result":{"id":"1111111111111111111111111111111111111111111111111111111111111111",
                           "secret":"2222222222222222222222222222222222222222222222222222222222222222"}}
            ],
            "meta":{"fingerprint":"NEXTCURSOR"}
        }"#;
        let resp: ContractEventsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].transaction_id, "abc123");
        assert_eq!(resp.data[0].event_name, "ReceiverSpent");
        assert_eq!(resp.data[0].block_number, 42);
        assert_eq!(resp.data[0].result.get("secret").unwrap().len(), 64);
        assert_eq!(resp.meta.fingerprint.as_deref(), Some("NEXTCURSOR"));
    }

    #[test]
    fn test_contract_events_response_empty_last_page() {
        // The last page carries no fingerprint cursor.
        let json = r#"{"data":[],"meta":{}}"#;
        let resp: ContractEventsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.is_empty());
        assert!(resp.meta.fingerprint.is_none());
    }
}
