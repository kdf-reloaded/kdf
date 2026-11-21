//! TRON HTTP API client with multi-node failover (CRD ch.21 §21.9, R-N1 to R-N9).
//!
//! All TRON full-node HTTP API endpoints use POST with JSON bodies. A request walks the nodes in the current order,
//! one node at a time, each attempt bounded by a cutoff (R-N3). The node that answers becomes the first node of the
//! order, the nodes that failed before it move to the tail (R-N5). No lock is held while a node is asked (R-N7).
//!
//! The account listings of the transaction history (CRD ch.21 §21.10C) are GET requests of the TronGrid account
//! endpoints. They walk the same order with the same rules, restricted to the keyed TronGrid nodes: `https` nodes
//! on a TronGrid host that carry the `TRON-PRO-API-KEY` header.
//!
//! # Invariants
//!
//! - Nothing this module logs or returns names a node by more than scheme, host and port, and no header value or
//!   API key is ever rendered (R-N1, R-N8, R-N9).
//! - The node order is shared by a client and its clones, and is always a complete permutation of the nodes.
//! - The configured order is shuffled once, when the client is built, and each request walks from the head
//!   (R-N5, CRD ch.21 R21.10B.3).

use derive_more::Display;
use mm2_err_handle::prelude::*;
use mm2_net::transport::{slurp_post_json_with_headers_bounded, slurp_url_with_headers_bounded, SlurpError, SlurpResult};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};

use super::address::TronAddress;
use super::nodes::TronNode;
use super::proto::TaposBlockData;
use super::TRON_API_TIMEOUT_SEC;
use crate::node_url::{host_only, listed_nodes, scrub_node_url, NodeSlot};
use common::log::debug;

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
    /// A 2xx answer that is the node's error form: a JSON object with a top-level `Error` string.
    #[display(fmt = "Node error: {}", _0)]
    NodeError(String),
    /// A dry run whose answer carries a revert indication (CRD ch.21 R21.10B.13 and its finding), with the node's
    /// revert message or the name of the simulated result. The outcome belongs to the contract and not to the
    /// node, so no other node is asked.
    #[display(fmt = "Contract call reverted: {}", _0)]
    ContractRevert(String),
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
            TronApiError::NodeError(text) => TronApiError::NodeError(scrub(text)),
            TronApiError::ContractRevert(text) => TronApiError::ContractRevert(scrub(text)),
        }
    }
}

/// Whether a TRON API error is a node failure that moves the request on to the next node (R-N4): a connection or
/// transport failure, the cutoff, a status outside 2xx, a body that is not JSON or does not decode into the
/// endpoint's answer, and a 2xx body that is the node's error form.
fn is_retryable(e: &TronApiError) -> bool {
    matches!(
        e,
        TronApiError::Transport(_)
            | TronApiError::Timeout(_)
            | TronApiError::NodeError(_)
            | TronApiError::InvalidResponse(_)
    )
}

/// The URL of the endpoint `path` of the node `base`: the path is appended to the path of `base` and the query of
/// `base` stays at the end, so `https://h/x?key=1` gives `https://h/x/wallet/getaccount?key=1` (CRD ch.21 R-N9, the
/// query-string key of a node URL). A fragment is not sent to a server and is dropped, and trailing slashes of the
/// path are ignored (`https://h/` gives `https://h/wallet/getaccount`, never `https://h//wallet/getaccount`); the
/// `//` after the scheme is never touched.
fn endpoint_url(base: &str, path: &str) -> String {
    let without_fragment = base.split_once('#').map_or(base, |(head, _)| head);
    let (head, query) = match without_fragment.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (without_fragment, None),
    };
    let authority_start = head.find("://").map_or(0, |at| at + 3);
    let (scheme, rest) = head.split_at(authority_start);
    let rest = rest.trim_end_matches('/');
    match query {
        Some(query) => format!("{scheme}{rest}{path}?{query}"),
        None => format!("{scheme}{rest}{path}"),
    }
}

/// Like [`endpoint_url`] for a request that has a query of its own: `query` (without the `?`) goes right after the
/// path and the query of `base` stays at the end, joined by `&`, so `https://h/x?key=1` asked for `/p` with `a=1`
/// gives `https://h/x/p?a=1&key=1`.
fn endpoint_url_with_query(base: &str, path: &str, query: &str) -> String {
    let url = endpoint_url(base, &format!("{path}?{query}"));
    // `endpoint_url` put the query of `base`, if any, behind a second `?`: it becomes a further parameter.
    match url.match_indices('?').nth(1) {
        Some((at, _)) => format!("{}&{}", &url[..at], &url[at + 1..]),
        None => url,
    }
}

/// POST a JSON body to one node, bounded by `cutoff_s` seconds for the whole exchange, and decode a 2xx answer as
/// `Resp`, then check it with `validate` (an [`TronApiError::InvalidResponse`] fails the attempt). `headers` are the node's own headers; redirects are not followed, so a 3xx answer is a non-2xx failure.
///
/// A non-2xx status is a node failure (retryable on the next node), whatever its body says: a rate-limit or
/// error body that happens to fit `Resp` must never be taken for an answer (for example `{}` on `getaccount`
/// means "account does not exist" only as a 2xx answer). The node's own error form is a 2xx answer, a JSON object
/// with a top-level `Error` string (measured on the public Nile node, on every endpoint); it is a
/// [`TronApiError::NodeError`] (retryable) and never decodes as `Resp`. The error texts carry no URL.
async fn post_json_checked<Resp>(
    url: &str,
    json_body: String,
    headers: &http::HeaderMap,
    cutoff_s: f64,
    validate: fn(&Resp) -> Result<(), TronApiError>,
) -> Result<Resp, TronApiError>
where
    Resp: serde::de::DeserializeOwned,
{
    checked_answer(
        slurp_post_json_with_headers_bounded(url, json_body, headers, cutoff_s).await,
        validate,
    )
}

/// GET from one node, bounded by `cutoff_s` seconds for the whole exchange, with the answer rules of
/// [`post_json_checked`]: a non-2xx status (a redirect included) is a node failure, the node's error forms are
/// node failures, and `validate` runs inside the attempt. `headers` are the node's own headers. The error texts
/// carry no URL and no header value.
async fn get_json_checked<Resp>(
    url: &str,
    headers: &http::HeaderMap,
    cutoff_s: f64,
    validate: fn(&Resp) -> Result<(), TronApiError>,
) -> Result<Resp, TronApiError>
where
    Resp: serde::de::DeserializeOwned,
{
    checked_answer(slurp_url_with_headers_bounded(url, headers, cutoff_s).await, validate)
}

/// Judges the exchange with one node (R-N4): the status, the node's error forms, the decoding and the endpoint's
/// own shape check.
fn checked_answer<Resp>(
    exchange: SlurpResult,
    validate: fn(&Resp) -> Result<(), TronApiError>,
) -> Result<Resp, TronApiError>
where
    Resp: serde::de::DeserializeOwned,
{
    let (status, _headers, body) = exchange.map_err(TronApiError::from)?;
    if !status.is_success() {
        return Err(TronApiError::Transport(format!(
            "node answered HTTP status {}",
            status.as_u16()
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| TronApiError::InvalidResponse(format!("node answer does not decode: {e}")))?;
    if let Some(error) = value.as_object().and_then(|object| object.get("Error")) {
        let text = error.as_str().map_or_else(|| error.to_string(), str::to_owned);
        return Err(TronApiError::NodeError(text));
    }
    if let Some(code) = transient_code(&value) {
        // A busy or not-yet-solid node: the next node may answer (R21.10B.3); the code is a public TRON name.
        return Err(TronApiError::NodeError(format!("node answered with code {code}")));
    }
    let answer: Resp = serde_json::from_value(value)
        .map_err(|e| TronApiError::InvalidResponse(format!("node answer does not decode: {e}")))?;
    // The endpoint's own shape check runs inside the attempt, so a wrongly shaped answer fails over (R-N4 (d)).
    validate(&answer)?;
    Ok(answer)
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// `/wallet/getnowblock` response. The node's keys are `blockID` and snake-case `block_header` (measured on the
/// public Nile node); the unused keys (`transactions`, `witness_signature`, ...) are ignored.
#[derive(Debug, Deserialize)]
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
    /// The node's own status object (a contract failure is reported here). Its presence marks an answer; a dry
    /// run also reads its success flag and its message.
    pub result: Option<serde_json::Value>,
    pub constant_result: Option<Vec<String>>,
    pub energy_used: Option<u64>,
    /// The simulated transaction. Only the execution results in its `ret` list are read, and only by a dry run.
    pub transaction: Option<serde_json::Value>,
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
    /// The bandwidth the transaction drew from the owner's free or staked bandwidth, in bytes.
    pub net_usage: Option<u64>,
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

/// One page of a TronGrid account listing (`GET /v1/accounts/{address}/transactions` and
/// `/transactions/trc20`; the shapes are those of TronGrid's public API documentation). The rows are kept as JSON:
/// a listing can carry rows of several kinds, and the history reads each row on its own (CRD ch.21 §21.10C).
#[derive(Debug, Deserialize)]
pub struct AccountListingPage {
    pub success: bool,
    pub data: Vec<serde_json::Value>,
    pub meta: Option<AccountListingMeta>,
}

#[derive(Debug, Deserialize)]
pub struct AccountListingMeta {
    /// The cursor of the next page; absent on the last page.
    pub fingerprint: Option<String>,
}

/// Which listing of an account is asked.
#[derive(Clone, Copy, Debug)]
pub enum AccountListing<'a> {
    /// `GET /v1/accounts/{address}/transactions`.
    Transactions,
    /// `GET /v1/accounts/{address}/transactions/trc20`, of every token or of the token contract given.
    Trc20 { contract: Option<&'a TronAddress> },
}

/// The page size asked of an account listing: the largest the public API documents.
const LISTING_PAGE_LIMIT: u32 = 200;

/// A listing page is an answer only when the provider marks it a success.
fn check_listing_page(page: &AccountListingPage) -> Result<(), TronApiError> {
    if page.success {
        Ok(())
    } else {
        Err(TronApiError::InvalidResponse(
            "account listing answer is not marked a success".to_owned(),
        ))
    }
}

/// A transaction-info answer for a transaction the node has: an answer without a block number (the node's empty
/// object for a transaction it does not know) is a bad answer, so the next node is asked (R-N4 (d)).
fn check_confirmed_info(info: &TransactionInfoResponse) -> Result<(), TronApiError> {
    if info.block_number.is_some() {
        Ok(())
    } else {
        Err(TronApiError::InvalidResponse(
            "gettransactioninfobyid answer carries no block number".to_owned(),
        ))
    }
}

/// The public TRON `response_code` names that mark a node as busy or not yet in sync: the request moves on to the
/// next node (CRD ch.21 R21.10B.3). Any other code in a 2xx error object is final.
const TRANSIENT_CODES: [&str; 4] = [
    "SERVER_BUSY",
    "NO_CONNECTION",
    "NOT_ENOUGH_EFFECTIVE_CONNECTION",
    "BLOCK_UNSOLIDIFIED",
];

/// The transient code of a 2xx error object, if the answer is one: a `code` beside `result: false` (or without a
/// `result`), a `result` object carrying a `code`, or an `error` object carrying a `code`.
fn transient_code(answer: &serde_json::Value) -> Option<&'static str> {
    let object = answer.as_object()?;
    let code_of = |holder: &serde_json::Map<String, serde_json::Value>| -> Option<&'static str> {
        let code = holder.get("code")?.as_str()?;
        TRANSIENT_CODES.iter().copied().find(|known| *known == code)
    };
    let nested = |key: &str| object.get(key).and_then(serde_json::Value::as_object).and_then(code_of);
    let top_level = match object.get("result") {
        None | Some(serde_json::Value::Bool(false)) => code_of(object),
        Some(_) => None,
    };
    top_level.or_else(|| nested("result")).or_else(|| nested("error"))
}

/// A result, a constant result or an energy figure marks a node's own answer (a contract failure is a `result`
/// object); a body with none of them is not an answer to this endpoint.
fn check_trigger_answer(answer: &TriggerConstantContractResponse) -> Result<(), TronApiError> {
    if answer.result.is_some() || answer.constant_result.is_some() || answer.energy_used.is_some() {
        Ok(())
    } else {
        Err(TronApiError::InvalidResponse(
            "triggerconstantcontract answer carries no result".to_owned(),
        ))
    }
}

/// The revert indication of a dry-run answer, if it carries one (CRD ch.21 R21.10B.13 and its finding): the reason
/// to show, which is the node's revert message, or the name of the simulated result when there is no message.
///
/// Either marker suffices. (i) The status object has the success flag `result: true` and a non-empty `message`;
/// a node answers a reverted call so, with a positive `energy_used` (measured on a public mainnet node,
/// 2026-11-20: `{"result": true, "message": "REVERT opcode executed"}`). (ii) An entry of `transaction.ret` names
/// an execution result (`ret`, or `contractRet`) other than `SUCCESS`; a successful call has the entry `{}`. An
/// empty `constant_result` is no marker on its own.
fn dry_run_revert(answer: &TriggerConstantContractResponse) -> Option<String> {
    use serde_json::Value;

    let status = answer.result.as_ref().and_then(Value::as_object);
    let flag_is_true = status.is_some_and(|status| status.get("result") == Some(&Value::Bool(true)));
    let message = status
        .and_then(|status| status.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let message = if flag_is_true { message } else { None };

    let simulated = answer
        .transaction
        .as_ref()
        .and_then(|transaction| transaction.get("ret"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|entry| [entry.get("ret"), entry.get("contractRet")])
        .flatten()
        .filter(|name| !name.is_null())
        .map(|name| name.as_str().map_or_else(|| name.to_string(), str::to_owned))
        .find(|name| name != "SUCCESS");

    match (message, simulated) {
        (Some(message), _) => Some(message.to_owned()),
        (None, Some(simulated)) => Some(format!("simulated result {simulated}")),
        (None, None) => None,
    }
}

/// The check of a dry-run answer: the endpoint's own shape ([`check_trigger_answer`]), then the revert indication,
/// which ends the request at this node ([`TronApiError::ContractRevert`] is not a node failure).
fn check_dry_run_answer(answer: &TriggerConstantContractResponse) -> Result<(), TronApiError> {
    check_trigger_answer(answer)?;
    match dry_run_revert(answer) {
        Some(reason) => Err(TronApiError::ContractRevert(reason)),
        None => Ok(()),
    }
}

/// The shape of a `getaccount` answer: a JSON object that is empty (the account does not exist on chain) or carries
/// a `balance` or a `create_time` and decodes as [`AccountInfo`]. Anything else, such as a proxy's
/// `{"message": "..."}`, is a bad answer.
fn check_account_shape(answer: &serde_json::Value) -> Result<(), TronApiError> {
    let object = answer
        .as_object()
        .ok_or_else(|| TronApiError::InvalidResponse("getaccount answer is not a JSON object".to_owned()))?;
    if object.is_empty() {
        return Ok(());
    }
    if !object.contains_key("balance") && !object.contains_key("create_time") {
        return Err(TronApiError::InvalidResponse(
            "getaccount answer carries neither a balance nor a creation time".to_owned(),
        ));
    }
    AccountInfo::deserialize(answer)
        .map(|_| ())
        .map_err(|e| TronApiError::InvalidResponse(format!("node answer does not decode: {e}")))
}

// ---------------------------------------------------------------------------
// API Client
// ---------------------------------------------------------------------------

/// A TRON HTTP API client that fails over between configured nodes.
///
/// Clones share the node order.
pub struct TronApiClient {
    inner: Arc<Inner>,
}

struct Inner {
    nodes: Vec<Node>,
    /// Indices into `nodes`, in the order the next request starts with. Never held across an `.await` (R-N7).
    order: Mutex<Vec<usize>>,
    /// The cutoff of one node attempt, in seconds (R-N3).
    cutoff_s: f64,
}

struct Node {
    /// The configured URL: a credential, so only used to send and to scrub.
    url: String,
    /// The headers every request to this node carries. Secrets: never rendered.
    headers: http::HeaderMap,
    /// The host-only form of the node (R-N1): the only way the node is named.
    label: String,
    /// The coin's ticker and the node's position in the coin's list as the request gave it (R-N8).
    slot: NodeSlot,
    /// Whether the node is a keyed TronGrid node, the only kind the account listings are asked of (CRD ch.21
    /// §21.10C). Fixed when the client is built.
    history: bool,
}

/// What a walk over the nodes asks of each node.
enum Call<'a> {
    /// A POST of `json_body` to the endpoint `path`, asked of every node.
    Post { path: &'a str, json_body: String },
    /// A GET of the endpoint `path` with `query`, asked of the keyed TronGrid nodes only.
    GetKeyed { path: &'a str, query: &'a str },
}

impl std::fmt::Debug for TronApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TronApiClient {{ <nodes hidden> }}")
    }
}

/// How one node attempt failed, for the all-failed error.
struct Failure {
    /// Index of the node in `Inner::nodes`.
    node: usize,
    error: TronApiError,
}

impl TronApiClient {
    /// Create a client over `nodes`, in the configured order, for the coin `ticker` (used in debug lines). Each node
    /// attempt is bounded by the [`TRON_API_TIMEOUT_SEC`] cutoff.
    ///
    /// The configured list is put into a random order once, here (R-N5, CRD ch.21 R21.10B.3); each node keeps the
    /// list position the request gave it for the log lines.
    pub fn new(ticker: &str, nodes: Vec<TronNode>) -> Self {
        Self::with_cutoff(ticker, nodes, TRON_API_TIMEOUT_SEC as f64, true)
    }

    /// Like [`Self::new`] with another cutoff, and the order shuffled or kept as configured; tests use the kept
    /// order and a short cutoff.
    fn with_cutoff(ticker: &str, nodes: Vec<TronNode>, cutoff_s: f64, shuffle: bool) -> Self {
        let urls: Vec<String> = nodes.iter().map(|node| node.url().to_owned()).collect();
        let nodes: Vec<Node> = nodes
            .into_iter()
            .zip(listed_nodes(ticker, urls))
            .map(|(node, listed)| Node {
                history: node.is_keyed_trongrid(),
                label: host_only(node.url()),
                headers: node.headers().clone(),
                url: listed.url,
                slot: listed.slot,
            })
            .collect();
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        if shuffle {
            order.shuffle(&mut common::small_rng());
        }
        let order = Mutex::new(order);
        TronApiClient {
            inner: Arc::new(Inner { nodes, order, cutoff_s }),
        }
    }

    /// Test constructor with an injected cutoff and the configured order kept (no shuffle).
    #[cfg(test)]
    pub(crate) fn new_with_cutoff_s(ticker: &str, nodes: Vec<TronNode>, cutoff_s: f64) -> Self {
        Self::with_cutoff(ticker, nodes, cutoff_s, false)
    }

    /// Test constructor like [`Self::new_with_cutoff_s`] in which `history[i]` says whether node `i` counts as a
    /// keyed TronGrid node: a loopback stub can never be one by the production rule
    /// ([`TronNode::is_keyed_trongrid`], which has its own tests).
    #[cfg(test)]
    pub(crate) fn new_with_history_nodes(ticker: &str, nodes: Vec<TronNode>, history: &[bool], cutoff_s: f64) -> Self {
        let mut client = Self::with_cutoff(ticker, nodes, cutoff_s, false);
        if let Some(inner) = Arc::get_mut(&mut client.inner) {
            for (node, flag) in inner.nodes.iter_mut().zip(history) {
                node.history = *flag;
            }
        }
        client
    }

    /// Whether the client has a keyed TronGrid node, that is, whether an account listing can be asked at all.
    pub(crate) fn has_history_nodes(&self) -> bool { self.inner.nodes.iter().any(|node| node.history) }

    /// The host-only names of the nodes in the order the next request starts with.
    #[cfg(test)]
    pub(crate) fn order_labels(&self) -> Vec<String> {
        let inner = &self.inner;
        inner
            .current_order()
            .into_iter()
            .map(|i| inner.nodes[i].label.clone())
            .collect()
    }

    /// Execute an HTTP POST to a TRON endpoint, trying the nodes one at a time in the current order (R-N4). The
    /// node that answers becomes the first node of the order (R-N5); no lock is held while a node is asked (R-N7).
    async fn try_post<Req, Resp>(&self, path: &str, request: &Req) -> Result<Resp, TronApiError>
    where
        Req: Serialize,
        Resp: serde::de::DeserializeOwned + Send + 'static,
    {
        self.try_post_validated(path, request, |_| Ok(())).await
    }

    /// Like [`Self::try_post`], with a shape check of the decoded answer that runs inside each node attempt: a
    /// wrongly shaped 2xx answer is an [`TronApiError::InvalidResponse`] and moves on to the next node (R-N4 (d)).
    async fn try_post_validated<Req, Resp>(
        &self,
        path: &str,
        request: &Req,
        validate: fn(&Resp) -> Result<(), TronApiError>,
    ) -> Result<Resp, TronApiError>
    where
        Req: Serialize,
        Resp: serde::de::DeserializeOwned + Send + 'static,
    {
        let json_body = serde_json::to_string(request).map_err(|e| TronApiError::InvalidResponse(e.to_string()))?;
        self.walk(&Call::Post { path, json_body }, validate).await
    }

    /// GET the endpoint `path` with `query` from the keyed TronGrid nodes, one at a time in the current order, with
    /// the rules of [`Self::try_post_validated`]: the same failover, cutoff, shape check inside the attempt and
    /// scrubbed errors. No other node is ever asked, so the address listings and the key go to TronGrid only.
    ///
    /// # Errors
    ///
    /// [`TronApiError::AllNodesFailed`] without any request when the client has no keyed TronGrid node; the errors
    /// of a walk otherwise.
    async fn try_get_keyed_validated<Resp>(
        &self,
        path: &str,
        query: &str,
        validate: fn(&Resp) -> Result<(), TronApiError>,
    ) -> Result<Resp, TronApiError>
    where
        Resp: serde::de::DeserializeOwned + Send + 'static,
    {
        if !self.has_history_nodes() {
            return Err(TronApiError::AllNodesFailed(
                "no TronGrid node with an API key is configured".to_owned(),
            ));
        }
        self.walk(&Call::GetKeyed { path, query }, validate).await
    }

    /// The walk every request shares: the nodes the call is asked of are tried one at a time in the current order
    /// (R-N4), the node that answers becomes the first node of the order and the nodes that failed before it move
    /// to the tail in their relative order (R-N5); no lock is held while a node is asked (R-N7).
    async fn walk<Resp>(
        &self,
        call: &Call<'_>,
        validate: fn(&Resp) -> Result<(), TronApiError>,
    ) -> Result<Resp, TronApiError>
    where
        Resp: serde::de::DeserializeOwned + Send + 'static,
    {
        let inner = &self.inner;
        // The snapshot this request walks; concurrent requests work on their own snapshots.
        let order = inner.current_order();
        let mut failures: Vec<Failure> = Vec::new();

        for &index in order.iter() {
            let node = &inner.nodes[index];
            let (path, url, attempt) = match call {
                Call::Post { path, json_body } => {
                    let url = endpoint_url(&node.url, path);
                    let attempt =
                        post_json_checked::<Resp>(&url, json_body.clone(), &node.headers, inner.cutoff_s, validate)
                            .await;
                    (*path, url, attempt)
                },
                Call::GetKeyed { path, query } => {
                    if !node.history {
                        continue;
                    }
                    let url = endpoint_url_with_query(&node.url, path, query);
                    let attempt = get_json_checked::<Resp>(&url, &node.headers, inner.cutoff_s, validate).await;
                    (*path, url, attempt)
                },
            };
            match attempt {
                Ok(resp) => {
                    // The answering node first, the nodes that failed before it at the tail in their relative
                    // order. The last store wins and is always a complete permutation.
                    if !failures.is_empty() {
                        let failed = |i: &usize| failures.iter().any(|failure| failure.node == *i);
                        let mut next = Vec::with_capacity(order.len());
                        next.push(index);
                        next.extend(order.iter().copied().filter(|i| *i != index && !failed(i)));
                        next.extend(order.iter().copied().filter(|i| *i != index && failed(i)));
                        inner.set_order(next);
                    }
                    return Ok(resp);
                },
                Err(e) => {
                    let error = e.without_node_urls(&[&url, &node.url]);
                    debug!(
                        "{}",
                        node.slot
                            .line("TRON node ", &node.label, &format!(" failed {path}: {error}"))
                    );
                    if !is_retryable(&error) {
                        return Err(error);
                    }
                    failures.push(Failure { node: index, error });
                },
            }
        }
        Err(all_failed(inner, failures))
    }

    /// Get the current block (for TAPOS data and timestamp).
    pub async fn get_now_block(&self) -> Result<GetNowBlockResponse, TronApiError> {
        #[derive(Serialize)]
        struct Empty {}
        // The block id must be usable for TAPOS: a body that decodes but carries an unusable id is a bad answer.
        self.try_post_validated("/wallet/getnowblock", &Empty {}, |block: &GetNowBlockResponse| {
            if block.block_header.raw_data.timestamp <= 0 {
                return Err(TronApiError::InvalidResponse(
                    "getnowblock answer has a non-positive timestamp".to_owned(),
                ));
            }
            block.to_tapos().map(|_| ())
        })
        .await
    }

    /// Get account info. Returns `None` only when the node answers a 2xx empty JSON object `{}`, its documented
    /// form for an account that does not exist on chain.
    ///
    /// Any other answer is an existing account when it carries a `balance` or a `create_time`, and an
    /// [`TronApiError::InvalidResponse`] otherwise (a non-object, or a non-empty object with neither field).
    pub async fn get_account(&self, address: &TronAddress) -> Result<Option<AccountInfo>, TronApiError> {
        let req = GetAccountRequest {
            address: address.to_base58(),
            visible: true,
        };
        let answer: serde_json::Value = self
            .try_post_validated("/wallet/getaccount", &req, check_account_shape)
            .await?;
        // The shape was checked in the attempt: an object, empty or an existing account.
        if answer.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(None);
        }
        serde_json::from_value(answer)
            .map(Some)
            .map_err(|e| TronApiError::InvalidResponse(format!("node answer does not decode: {e}")))
    }

    /// Call a smart contract in read-only mode (no broadcast).
    pub async fn trigger_constant_contract(
        &self,
        owner_address: &TronAddress,
        contract_address: &TronAddress,
        function_selector: &str,
        parameter_hex: &str,
    ) -> Result<TriggerConstantContractResponse, TronApiError> {
        self.trigger_constant_contract_checked(
            owner_address,
            contract_address,
            function_selector,
            parameter_hex,
            check_trigger_answer,
        )
        .await
    }

    /// Like [`Self::trigger_constant_contract`] for the dry run of a call that is going to be sent (CRD ch.21
    /// R21.10B.13 and its finding).
    ///
    /// # Errors
    ///
    /// [`TronApiError::ContractRevert`] when the answer carries a revert indication (a revert message beside a true
    /// success flag, or a simulated execution result other than success), with the node's URL removed from the
    /// reason; no other node is asked then. The errors of the walk otherwise.
    pub async fn trigger_constant_dry_run(
        &self,
        owner_address: &TronAddress,
        contract_address: &TronAddress,
        function_selector: &str,
        parameter_hex: &str,
    ) -> Result<TriggerConstantContractResponse, TronApiError> {
        self.trigger_constant_contract_checked(
            owner_address,
            contract_address,
            function_selector,
            parameter_hex,
            check_dry_run_answer,
        )
        .await
    }

    /// Like [`Self::trigger_constant_contract`] for a read of a contract value (a balance, a decimals figure): a
    /// `constant_result` that is absent, empty or whose first entry is not hex is a bad answer and moves on to the next node (R-N4 (d)).
    pub async fn trigger_constant_read(
        &self,
        owner_address: &TronAddress,
        contract_address: &TronAddress,
        function_selector: &str,
        parameter_hex: &str,
    ) -> Result<TriggerConstantContractResponse, TronApiError> {
        self.trigger_constant_contract_checked(
            owner_address,
            contract_address,
            function_selector,
            parameter_hex,
            |answer| {
                // A read needs a present, non-empty `constant_result` whose first entry is non-empty hex.
                match answer.constant_result.as_ref().and_then(|results| results.first()) {
                    Some(first) if !first.is_empty() && hex::decode(first).is_ok() => Ok(()),
                    _ => Err(TronApiError::InvalidResponse(
                        "constant_result is absent, empty or not hex".to_owned(),
                    )),
                }
            },
        )
        .await
    }

    async fn trigger_constant_contract_checked(
        &self,
        owner_address: &TronAddress,
        contract_address: &TronAddress,
        function_selector: &str,
        parameter_hex: &str,
        validate: fn(&TriggerConstantContractResponse) -> Result<(), TronApiError>,
    ) -> Result<TriggerConstantContractResponse, TronApiError> {
        let req = TriggerConstantContractRequest {
            owner_address: owner_address.to_base58(),
            contract_address: contract_address.to_base58(),
            function_selector: function_selector.to_string(),
            parameter: parameter_hex.to_string(),
            visible: true,
        };
        self.try_post_validated("/wallet/triggerconstantcontract", &req, validate)
            .await
    }

    /// Broadcast a signed transaction (protobuf hex).
    pub async fn broadcast_hex(&self, tx_hex: &str) -> Result<BroadcastHexResponse, TronApiError> {
        let req = BroadcastHexRequest {
            transaction: tx_hex.to_string(),
        };
        // A body without a `result` flag is not a node's answer and fails over; a well-formed `result: false`
        // passes the check and ends the request below (R-N6).
        let resp: BroadcastHexResponse = self
            .try_post_validated("/wallet/broadcasthex", &req, |answer: &BroadcastHexResponse| {
                if answer.result.is_some() {
                    Ok(())
                } else {
                    Err(TronApiError::InvalidResponse(
                        "broadcasthex answer carries no result flag".to_owned(),
                    ))
                }
            })
            .await?;
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

    /// The receipt of a transaction the chain has confirmed, for the height and the fee of a history row (CRD ch.21
    /// §21.10C). A node that does not know the transaction answers an empty object: that is a bad answer here and
    /// moves on to the next node.
    pub(crate) async fn get_confirmed_transaction_info(
        &self,
        tx_hash: &str,
    ) -> Result<TransactionInfoResponse, TronApiError> {
        let req = TxByIdRequest {
            value: tx_hash.to_string(),
        };
        self.try_post_validated("/wallet/gettransactioninfobyid", &req, check_confirmed_info)
            .await
    }

    /// One page of a confirmed-only account listing from a keyed TronGrid node (CRD ch.21 §21.10C). `fingerprint`
    /// is the cursor of the previous page (`meta.fingerprint`); the link the provider sends beside it is never
    /// followed.
    ///
    /// # Errors
    ///
    /// [`TronApiError::InvalidResponse`] without any request for a cursor that is not made of letters, digits,
    /// `-` and `_` (it is put into the request URL as it is); the errors of the walk otherwise.
    pub(crate) async fn get_account_listing(
        &self,
        address: &TronAddress,
        listing: AccountListing<'_>,
        fingerprint: Option<&str>,
    ) -> Result<AccountListingPage, TronApiError> {
        let owner = address.to_base58();
        let mut query = format!("only_confirmed=true&limit={LISTING_PAGE_LIMIT}");
        let path = match listing {
            AccountListing::Transactions => format!("/v1/accounts/{owner}/transactions"),
            AccountListing::Trc20 { contract } => {
                if let Some(contract) = contract {
                    query.push_str("&contract_address=");
                    query.push_str(&contract.to_base58());
                }
                format!("/v1/accounts/{owner}/transactions/trc20")
            },
        };
        if let Some(fingerprint) = fingerprint {
            let plain = !fingerprint.is_empty()
                && fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
            if !plain {
                return Err(TronApiError::InvalidResponse(
                    "the cursor of an account listing has an unexpected form".to_owned(),
                ));
            }
            query.push_str("&fingerprint=");
            query.push_str(fingerprint);
        }
        self.try_get_keyed_validated(&path, &query, check_listing_page).await
    }

    /// Get chain parameters (fee prices).
    pub async fn get_chain_parameters(&self) -> Result<GetChainParametersResponse, TronApiError> {
        #[derive(Serialize)]
        struct Empty {}
        self.try_post_validated(
            "/wallet/getchainparameters",
            &Empty {},
            |answer: &GetChainParametersResponse| {
                // The two prices every fee estimate needs must be present and positive.
                let positive = |key: &str| {
                    answer
                        .chain_parameter
                        .iter()
                        .any(|parameter| parameter.key == key && parameter.value.is_some_and(|value| value > 0))
                };
                if positive("getTransactionFee") && positive("getEnergyFee") {
                    Ok(())
                } else {
                    Err(TronApiError::InvalidResponse(
                        "getchainparameters answer lacks a positive getTransactionFee or getEnergyFee".to_owned(),
                    ))
                }
            },
        )
        .await
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
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Inner {
    fn current_order(&self) -> Vec<usize> { self.order.lock().unwrap_or_else(PoisonError::into_inner).clone() }

    fn set_order(&self, order: Vec<usize>) { *self.order.lock().unwrap_or_else(PoisonError::into_inner) = order; }
}

/// The error of a request that every node failed (R-N8).
///
/// The class is that of the last failure that was not a cutoff: the node's own error form stays a
/// [`TronApiError::NodeError`] and an undecodable answer stays an [`TronApiError::InvalidResponse`], anything else
/// is [`TronApiError::AllNodesFailed`]. When every node hit the cutoff, the error is a [`TronApiError::Timeout`]
/// that says no node answered in time. The text lists each host tried once, host-only, with its failure; it carries
/// no URL part and no header value.
fn all_failed(inner: &Inner, failures: Vec<Failure>) -> TronApiError {
    if failures.is_empty() {
        return TronApiError::AllNodesFailed("no nodes configured".to_owned());
    }
    let Some(last) = failures
        .iter()
        .rev()
        .find(|f| !matches!(f.error, TronApiError::Timeout(_)))
    else {
        return TronApiError::Timeout(format!(
            "nodes unreachable: no node answered within {} s",
            inner.cutoff_s
        ));
    };
    let mut tried: Vec<(&str, String)> = Vec::new();
    for failure in &failures {
        let label = inner.nodes[failure.node].label.as_str();
        let text = match &failure.error {
            TronApiError::Timeout(_) => format!("no answer within {} s", inner.cutoff_s),
            other => other.to_string(),
        };
        match tried.iter_mut().find(|(seen, _)| *seen == label) {
            Some(entry) => entry.1 = text,
            None => tried.push((label, text)),
        }
    }
    let listing = tried
        .into_iter()
        .map(|(label, text)| format!("{label}: {text}"))
        .collect::<Vec<_>>()
        .join("; ");
    match last.error {
        TronApiError::NodeError(_) => TronApiError::NodeError(listing),
        TronApiError::InvalidResponse(_) => TronApiError::InvalidResponse(listing),
        _ => TronApiError::AllNodesFailed(listing),
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
    fn get_now_block_decodes_the_nodes_answer_form() {
        let id = "0000000003456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let zeros = "0".repeat(64);
        let json = format!(
            r#"{{"blockID":"{id}","block_header":{{"raw_data":{{"number":1,"txTrieRoot":"{zeros}","witness_address":"41ab","parentHash":"{zeros}","version":32,"timestamp":1}},"witness_signature":"ab"}},"transactions":[]}}"#
        );
        let resp: GetNowBlockResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.block_id, id);
        assert_eq!(resp.block_header.raw_data.number, 1);
        assert_eq!(resp.block_header.raw_data.timestamp, 1);
        // The camel-case spelling is not the node's form and must not decode.
        let wrong = format!(r#"{{"blockID":"{id}","blockHeader":{{"raw_data":{{"number":1,"timestamp":1}}}}}}"#);
        assert!(serde_json::from_str::<GetNowBlockResponse>(&wrong).is_err());
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

    /// The answer forms are those a public mainnet node gave for a USDT `transfer(address,uint256)` on 2026-11-20.
    #[test]
    fn a_dry_run_answer_is_read_for_its_revert_indication() {
        let reason_of = |json: &str| {
            let answer: TriggerConstantContractResponse = serde_json::from_str(json).unwrap();
            dry_run_revert(&answer)
        };
        let word = "0000000000000000000000000000000000000000000000000000000000000001";

        // A successful call: no message, an empty simulated result.
        let success = format!(
            r#"{{"result":{{"result":true}},"energy_used":64285,"energy_penalty":49635,"constant_result":["{word}"],"logs":[{{"address":"a614f803b6fd780986a42c78ec9c7f77e6ded13c","data":"{word}","topics":["{word}"]}}],"transaction":{{"ret":[{{}}],"visible":true,"txID":"{word}","raw_data":{{}},"raw_data_hex":"0a02"}}}}"#
        );
        assert_eq!(reason_of(&success), None);
        assert_eq!(
            reason_of(r#"{"result":{"result":true},"transaction":{"ret":[{"ret":"SUCCESS"}]}}"#),
            None
        );
        // An empty `constant_result` alone, an empty message, and a message beside a flag that is not true.
        assert_eq!(reason_of(r#"{"result":{"result":true},"constant_result":[""]}"#), None);
        assert_eq!(reason_of(r#"{"result":{"result":true,"message":" "}}"#), None);
        assert_eq!(reason_of(r#"{"result":{"code":"OTHER_ERROR","message":"no"}}"#), None);
        assert_eq!(reason_of(r#"{"constant_result":["00"],"energy_used":1}"#), None);

        // A reverted call carries both markers; the message is the reason.
        let reverted = r#"{"result":{"result":true,"message":"REVERT opcode executed"},"energy_used":8624,"energy_penalty":6640,"constant_result":[""],"transaction":{"ret":[{"ret":"FAILED"}],"visible":true}}"#;
        assert_eq!(reason_of(reverted), Some("REVERT opcode executed".to_owned()));
        // Either marker alone.
        assert_eq!(
            reason_of(r#"{"result":{"result":true,"message":"REVERT opcode executed"},"energy_used":8624}"#),
            Some("REVERT opcode executed".to_owned())
        );
        for (ret, name) in [
            (r#"{"ret":"FAILED"}"#, "FAILED"),
            (r#"{"contractRet":"REVERT"}"#, "REVERT"),
            (r#"{"ret":"SUCCESS","contractRet":"OUT_OF_ENERGY"}"#, "OUT_OF_ENERGY"),
            (r#"{"ret":1}"#, "1"),
        ] {
            let answer =
                format!(r#"{{"result":{{"result":true}},"energy_used":8624,"transaction":{{"ret":[{{}},{ret}]}}}}"#);
            assert_eq!(reason_of(&answer), Some(format!("simulated result {name}")), "{ret}");
        }
    }

    #[test]
    fn a_revert_reason_is_scrubbed_of_node_urls_and_ends_the_request() {
        let node = "https://user:secret@tron.example.org:8090/APIKEY";
        let url = format!("{node}/wallet/triggerconstantcontract");
        let reverted = TronApiError::ContractRevert(format!("refused, see {url} or {node}"));
        assert!(!is_retryable(&reverted));
        let text = reverted.without_node_urls(&[&url, node]).to_string();
        for secret in ["user", "secret", "APIKEY"] {
            assert!(!text.contains(secret), "a credential part is rendered");
        }
        assert!(text.starts_with("Contract call reverted: refused, see "), "{text}");
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
        let client = TronApiClient::new("TRX", vec![
            TronNode::plain("http://user:secret@127.0.0.1:1/APIKEY".to_owned()),
            TronNode::plain("http://127.0.0.1:1/OTHERKEY".to_owned()),
        ]);
        let err = common::block_on(client.get_now_block()).unwrap_err().to_string();
        for secret in ["user", "secret", "APIKEY", "OTHERKEY"] {
            assert!(!err.contains(secret), "a credential part is rendered");
        }
    }

    #[test]
    fn a_request_query_goes_before_the_query_of_a_node_url() {
        for (base, expected) in [
            ("https://h", "https://h/v1/p?a=1&b=2"),
            ("https://h/", "https://h/v1/p?a=1&b=2"),
            ("https://h/x", "https://h/x/v1/p?a=1&b=2"),
            ("https://h/x?key=1", "https://h/x/v1/p?a=1&b=2&key=1"),
            ("https://h/x?key=1&c=3#frag", "https://h/x/v1/p?a=1&b=2&key=1&c=3"),
        ] {
            assert_eq!(endpoint_url_with_query(base, "/v1/p", "a=1&b=2"), expected, "{base}");
        }
    }

    #[test]
    fn the_endpoint_path_goes_before_the_query_of_a_node_url() {
        for (base, expected) in [
            ("https://h/x?key=1", "https://h/x/wallet/getaccount?key=1"),
            ("https://h?key=1", "https://h/wallet/getaccount?key=1"),
            ("https://h/x?key=1&b=2", "https://h/x/wallet/getaccount?key=1&b=2"),
            ("https://h/x?key=1#frag", "https://h/x/wallet/getaccount?key=1"),
            ("https://h/x#frag", "https://h/x/wallet/getaccount"),
            ("https://h/x", "https://h/x/wallet/getaccount"),
            ("https://h", "https://h/wallet/getaccount"),
            ("https://h/", "https://h/wallet/getaccount"),
            ("https://h/?k=1", "https://h/wallet/getaccount?k=1"),
            ("https://h/x/", "https://h/x/wallet/getaccount"),
            ("https://h/x//?k=1", "https://h/x/wallet/getaccount?k=1"),
        ] {
            assert_eq!(endpoint_url(base, "/wallet/getaccount"), expected, "{base}");
        }
    }
}
