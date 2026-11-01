//! TRON HTTP API client with multi-node failover.
//!
//! All TRON full-node HTTP API endpoints use POST with JSON bodies.
//! The client rotates through configured nodes, promoting successful
//! nodes to the front of the list.

use derive_more::Display;
use mm2_err_handle::prelude::*;
use mm2_net::transport::{post_json, SlurpError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::address::TronAddress;
use super::proto::TaposBlockData;
use crate::node_url::scrub_node_url;

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

impl TronApiError {
    /// The error with the node's URL removed from its text (CRD ch.35 R35.1.14 via ch.21 R-N1): the lower layer
    /// prints the full URL it was given, and a node URL can embed an API key. `urls` are the strings the
    /// request was built from.
    fn without_node_urls(self, urls: &[&str]) -> Self {
        let scrub = |text: String| urls.iter().fold(text, |text, url| scrub_node_url(&text, url));
        match self {
            TronApiError::Transport(text) => TronApiError::Transport(scrub(text)),
            TronApiError::Timeout(text) => TronApiError::Timeout(scrub(text)),
            TronApiError::RemoteError { code, message } => TronApiError::RemoteError {
                code,
                message: scrub(message),
            },
            TronApiError::InvalidResponse(text) => TronApiError::InvalidResponse(scrub(text)),
            TronApiError::AllNodesFailed(text) => TronApiError::AllNodesFailed(scrub(text)),
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
// API Client
// ---------------------------------------------------------------------------

/// A TRON HTTP API client that rotates through configured nodes.
pub struct TronApiClient {
    /// List of TRON full-node API URLs (e.g., `https://api.trongrid.io`).
    nodes: Arc<async_std::sync::Mutex<Vec<String>>>,
}

impl std::fmt::Debug for TronApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // We can't lock the mutex synchronously here; just record the type.
        write!(f, "TronApiClient {{ <nodes hidden> }}")
    }
}

impl TronApiClient {
    /// Create a new client from a list of node base URLs.
    pub fn new(node_urls: Vec<String>) -> Self {
        TronApiClient {
            nodes: Arc::new(async_std::sync::Mutex::new(node_urls)),
        }
    }

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
                    let api_err = TronApiError::from(e).without_node_urls(&[&url, &nodes[i]]);
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
}

impl Clone for TronApiClient {
    fn clone(&self) -> Self {
        TronApiClient {
            nodes: Arc::clone(&self.nodes),
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

    #[test]
    fn node_urls_are_scrubbed_from_every_lower_layer_text() {
        let node = "https://user:secret@tron.example.org:8090/APIKEY";
        let url = format!("{node}/wallet/getnowblock");
        let secrets = ["user", "secret", "APIKEY"];
        for slurp in [
            SlurpError::Transport {
                uri: url.clone(),
                error: format!("connection refused for {url}"),
            },
            SlurpError::Timeout {
                uri: url.clone(),
                error: format!("timed out on {node}"),
            },
            SlurpError::Timeout {
                uri: url.clone(),
                error: "timed out".to_owned(),
            },
            SlurpError::ErrorDeserializing {
                uri: url.clone(),
                error: "expected value".to_owned(),
            },
        ] {
            let err = TronApiError::from(MmError::new(slurp)).without_node_urls(&[&url, node]);
            let text = err.to_string();
            for secret in secrets {
                assert!(!text.contains(secret), "a credential part is rendered");
            }
        }
    }

    #[test]
    fn all_nodes_failed_error_names_no_key() {
        // Port 1 on the loopback is closed: every attempt fails at connect.
        let client = TronApiClient::new(vec![
            "http://user:secret@127.0.0.1:1/APIKEY".to_owned(),
            "http://127.0.0.1:1/OTHERKEY".to_owned(),
        ]);
        let err = common::block_on(client.get_now_block()).unwrap_err().to_string();
        for secret in ["user", "secret", "APIKEY", "OTHERKEY"] {
            assert!(!err.contains(secret), "a credential part is rendered");
        }
    }
}
