//! # Purpose
//! Async JSON-RPC 2.0 client for the Solana HTTP API.
//!
//! # Public exports
//! - [`SolanaRpcClient`] — async, cloneable client over `mm2_net::native_http`.
//! - [`RpcError`], [`RpcErrorKind`] — `From`-convertible into `BalanceError`
//!   and `WithdrawError`.
//! - Response shapes for the seven endpoints we exercise:
//!   `getBalance`, `getLatestBlockhash`, `getFeeForMessage`,
//!   `getBlockHeight`, `sendTransaction`, `getTokenAccountsByOwner`,
//!   `getTokenAccountBalance`, `getAccountInfo`.
//! - [`UiTransactionEncoding`] / [`TokenAccountsFilter`] mirrors used by
//!   the legacy `solana-client` 1.x surface; these are KDF-original so
//!   downstream call sites can keep their existing import shape.
//!
//! # Invariants
//! - Every call sends a single JSON-RPC request and parses one JSON-RPC
//!   response; no batching, no retries, no commitment-aware caching.
//! - Errors from the HTTP layer surface as
//!   [`RpcErrorKind::Transport`]; structured `error` objects from the
//!   server surface as [`RpcErrorKind::Rpc`]; unparseable bodies as
//!   [`RpcErrorKind::Decode`].
//! - The client is `Clone`; cloning duplicates only the endpoint URL
//!   and commitment level (no socket state).
//!
//! # Non-goals
//! - Web-sockets / pub-sub. KDF only consumes RPC for balances,
//!   blockhashes, and broadcasts.
//! - Hardware-wallet (Ledger) integration. Removing the
//!   `solana-remote-wallet` dependency is the entire point of P14.

use crate::node_url::{host_only, scrub_node_url, scrub_urls_in_text};
use common::executor::Timer;
use derive_more::Display;
use futures::future::{select, Either};
use futures::FutureExt;
use mm2_err_handle::prelude::*;
use mm2_net::native_http::slurp_post_json;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const JSONRPC_VERSION: &str = "2.0";

/// Hard timeout for a single JSON-RPC round-trip. Matches the upstream
/// `solana-client` default for non-block-subscribe calls and the
/// 5-second cap used on the GLEEC-compatible network.
const REQUEST_TIMEOUT_SECS: f64 = 5.0;

/// Minimum compute-unit price at which sendTransaction is willing to
/// accept a transaction with default settings (used to seed the request
/// counter — value choice is cosmetic, only the monotonic property
/// matters).
const REQUEST_ID_BASE: u64 = 1;

/// Encoding hint for the `getTransaction` / `simulateTransaction`
/// family. Mirrors the minimal subset the legacy
/// `solana_transaction_status::UiTransactionEncoding` used to expose.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiTransactionEncoding {
    Base64,
    Base58,
    Json,
    JsonParsed,
}

/// Filter argument for `getTokenAccountsByOwner`. The legacy 1.x
/// surface had `Mint(Pubkey)` and `ProgramId(Pubkey)`; we only need
/// `Mint`, but keep the variant to preserve the import shape used at
/// call sites.
#[derive(Clone, Debug)]
pub enum TokenAccountsFilter {
    /// Filter by token-mint pubkey.
    Mint(Pubkey),
    /// Filter by SPL-token program-id (rarely used; kept for parity).
    ProgramId(Pubkey),
}

impl TokenAccountsFilter {
    fn into_json(self) -> Json {
        match self {
            Self::Mint(p) => json!({ "mint": p.to_string() }),
            Self::ProgramId(p) => json!({ "programId": p.to_string() }),
        }
    }
}

/// Async, cloneable JSON-RPC client.
#[derive(Clone, Debug)]
pub struct SolanaRpcClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    url: String,
    commitment: CommitmentConfig,
    next_id: AtomicU64,
}

/// The debug form names the node by its host-only form (CRD ch.40 R40.5A.1, ch.35 R35.1.17).
impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientInner")
            .field("node", &host_only(&self.url))
            .field("commitment", &self.commitment)
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl SolanaRpcClient {
    pub fn new(url: impl Into<String>) -> Self { Self::with_commitment(url, CommitmentConfig::default()) }

    pub fn with_commitment(url: impl Into<String>, commitment: CommitmentConfig) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                url: url.into(),
                commitment,
                next_id: AtomicU64::new(REQUEST_ID_BASE),
            }),
        }
    }

    pub fn url(&self) -> &str { &self.inner.url }

    pub fn commitment(&self) -> CommitmentConfig { self.inner.commitment }

    fn next_id(&self) -> u64 { self.inner.next_id.fetch_add(1, Ordering::Relaxed) }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Json) -> Result<T, RpcError> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();

        // Bound every round-trip with a hard timeout so a single dead
        // endpoint cannot wedge an entire coin operation. Using
        // `common::executor::Timer` keeps the wait portable across
        // native and WASM (we are native-only here, but the helper is
        // already part of the workspace's runtime abstraction).
        let req_fut = Box::pin(slurp_post_json(&self.inner.url, body).fuse());
        let timeout = Timer::sleep(REQUEST_TIMEOUT_SECS);
        let (status, _headers, bytes) = match select(req_fut, timeout).await {
            Either::Left((Ok(triple), _)) => triple,
            // The lower layer's text names the node by its full URL; a text names it host-only (R40.5A.1).
            Either::Left((Err(e), _)) => {
                return Err(RpcError::transport(
                    method,
                    scrub_urls_in_text(&scrub_node_url(&e.get_inner().to_string(), &self.inner.url)),
                ))
            },
            Either::Right(_) => {
                return Err(RpcError::transport(
                    method,
                    format!("{REQUEST_TIMEOUT_SECS}s timeout expired"),
                ))
            },
        };

        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes).into_owned();
            return Err(RpcError::transport(method, format!("HTTP {}: {}", status, body)));
        }

        let envelope: Envelope<T> =
            serde_json::from_slice(&bytes).map_err(|e| RpcError::decode(method, e.to_string(), &bytes))?;

        match envelope {
            Envelope::Ok { result, .. } => Ok(result),
            Envelope::Err { error, .. } => Err(RpcError::rpc(method, error)),
        }
    }

    /// `getHealth` — RPC liveness probe. Returns `Ok(())` when the
    /// node reports itself as healthy (`"ok"`); any other payload, a
    /// transport failure, or a timeout surfaces as a normal
    /// [`RpcError`].
    pub async fn get_health(&self) -> Result<(), RpcError> {
        let resp: String = self.call("getHealth", json!([])).await?;
        if resp == "ok" {
            Ok(())
        } else {
            Err(RpcError::rpc_simple(
                "getHealth",
                &format!("unexpected payload: {resp}"),
            ))
        }
    }

    /// `getMinimumBalanceForRentExemption` — minimum lamports an
    /// account of `data_len` bytes must hold to be exempt from rent.
    /// Used by the SPL withdraw path to size the rent reserve when an
    /// associated-token account has to be created on the fly.
    pub async fn get_minimum_balance_for_rent_exemption(&self, data_len: usize) -> Result<u64, RpcError> {
        self.call(
            "getMinimumBalanceForRentExemption",
            json!([data_len, self.commitment_param()]),
        )
        .await
    }

    /// `getBalance` — returns lamports.
    pub async fn get_balance(&self, address: &Pubkey) -> Result<u64, RpcError> {
        let resp: ContextResponse<u64> = self
            .call("getBalance", json!([address.to_string(), self.commitment_param()]))
            .await?;
        Ok(resp.value)
    }

    /// `getBlockHeight` — current block height.
    pub async fn get_block_height(&self) -> Result<u64, RpcError> {
        self.call("getBlockHeight", json!([self.commitment_param()])).await
    }

    /// `getLatestBlockhash` — recent blockhash for transaction signing.
    pub async fn get_latest_blockhash(&self) -> Result<Hash, RpcError> {
        let resp: ContextResponse<LatestBlockhashValue> = self
            .call("getLatestBlockhash", json!([self.commitment_param()]))
            .await?;
        Hash::from_str(&resp.value.blockhash).map_err(|e| RpcError::decode("getLatestBlockhash", e.to_string(), &[]))
    }

    /// `getFeeForMessage` — fee in lamports for a serialised message.
    /// Returns `None` if the network can no longer price the message
    /// (e.g. blockhash too old).
    pub async fn get_fee_for_message(&self, message: &Message) -> Result<u64, RpcError> {
        let serialised =
            bincode::serialize(message).map_err(|e| RpcError::decode("getFeeForMessage", e.to_string(), &[]))?;
        let encoded = base64::encode(&serialised);
        let resp: ContextResponse<Option<u64>> = self
            .call(
                "getFeeForMessage",
                json!([encoded, { "commitment": self.commitment_str(), "encoding": "base64" }]),
            )
            .await?;
        resp.value
            .ok_or_else(|| RpcError::rpc_simple("getFeeForMessage", "fee unavailable for blockhash"))
    }

    /// `sendTransaction` — broadcast a signed transaction; returns the
    /// transaction signature as a base58 string.
    pub async fn send_transaction(&self, tx: &Transaction) -> Result<Signature, RpcError> {
        let serialised = bincode::serialize(tx).map_err(|e| RpcError::decode("sendTransaction", e.to_string(), &[]))?;
        let encoded = base64::encode(&serialised);
        let raw: String = self
            .call(
                "sendTransaction",
                json!([encoded, { "encoding": "base64", "preflightCommitment": self.commitment_str() }]),
            )
            .await?;
        Signature::from_str(&raw).map_err(|e| RpcError::decode("sendTransaction", e.to_string(), raw.as_bytes()))
    }

    /// `getTokenAccountsByOwner` — list of `{ pubkey, account }` for
    /// every token account owned by `owner` matching `filter`.
    pub async fn get_token_accounts_by_owner(
        &self,
        owner: &Pubkey,
        filter: TokenAccountsFilter,
    ) -> Result<Vec<KeyedTokenAccount>, RpcError> {
        let resp: ContextResponse<Vec<KeyedTokenAccount>> = self
            .call(
                "getTokenAccountsByOwner",
                json!([
                    owner.to_string(),
                    filter.into_json(),
                    { "commitment": self.commitment_str(), "encoding": "jsonParsed" }
                ]),
            )
            .await?;
        Ok(resp.value)
    }

    /// `getTokenAccountBalance` — `(amount_string, decimals,
    /// ui_amount_string)` for a single SPL token account.
    pub async fn get_token_account_balance(&self, account: &Pubkey) -> Result<TokenAmount, RpcError> {
        let resp: ContextResponse<TokenAmount> = self
            .call(
                "getTokenAccountBalance",
                json!([account.to_string(), self.commitment_param()]),
            )
            .await?;
        Ok(resp.value)
    }

    /// `getAccountInfo` — minimal account-existence check used by SPL
    /// withdraw to decide whether the destination ATA must be created.
    pub async fn get_account_exists(&self, address: &Pubkey) -> Result<bool, RpcError> {
        let resp: ContextResponse<Option<Json>> = self
            .call(
                "getAccountInfo",
                json!([address.to_string(), { "commitment": self.commitment_str(), "encoding": "base64" }]),
            )
            .await?;
        Ok(resp.value.is_some())
    }

    fn commitment_param(&self) -> Json { json!({ "commitment": self.commitment_str() }) }

    fn commitment_str(&self) -> &'static str {
        // CommitmentConfig in the modular crate exposes `commitment` as
        // a `CommitmentLevel` enum; format it once as the static string
        // the JSON-RPC API expects.
        match format!("{:?}", self.inner.commitment.commitment)
            .to_lowercase()
            .as_str()
        {
            "finalized" => "finalized",
            "confirmed" => "confirmed",
            "processed" => "processed",
            _ => "confirmed",
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Wire shapes
// ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Envelope<T> {
    Ok {
        #[allow(dead_code)]
        jsonrpc: String,
        result: T,
        #[allow(dead_code)]
        id: u64,
    },
    Err {
        #[allow(dead_code)]
        jsonrpc: String,
        error: RpcErrorObject,
        #[allow(dead_code)]
        id: u64,
    },
}

#[derive(Debug, Deserialize)]
struct ContextResponse<T> {
    #[allow(dead_code)]
    context: Option<Json>,
    value: T,
}

#[derive(Debug, Deserialize)]
struct LatestBlockhashValue {
    blockhash: String,
    #[allow(dead_code)]
    #[serde(rename = "lastValidBlockHeight")]
    last_valid_block_height: Option<u64>,
}

/// `{ pubkey: <base58>, account: <opaque jsonParsed value> }` —
/// downstream code only reads `.pubkey`, so `account` stays as opaque
/// JSON.
#[derive(Clone, Debug, Deserialize)]
pub struct KeyedTokenAccount {
    pub pubkey: String,
    #[allow(dead_code)]
    pub account: Json,
}

/// `getTokenAccountBalance` value shape.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenAmount {
    pub amount: String,
    pub decimals: u8,
    #[serde(default)]
    pub ui_amount: Option<f64>,
    pub ui_amount_string: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Json>,
}

// ────────────────────────────────────────────────────────────────────
// Errors
// ────────────────────────────────────────────────────────────────────

/// Top-level RPC error.
#[derive(Clone, Debug, Display)]
#[display(fmt = "{} on {}: {}", "kind.label()", method, kind)]
pub struct RpcError {
    /// JSON-RPC method name that produced the failure (debug-only).
    pub method: String,
    pub kind: RpcErrorKind,
}

/// What went wrong.
#[derive(Clone, Debug, Display)]
pub enum RpcErrorKind {
    /// HTTP transport / network failure.
    #[display(fmt = "transport: {}", _0)]
    Transport(String),
    /// JSON envelope, body or hash/signature decoding failed.
    #[display(fmt = "decode: {}", _0)]
    Decode(String),
    /// Server returned a structured `error` object.
    #[display(fmt = "server error {}: {}", "_0.code", "_0.message")]
    Rpc(RpcErrorObject),
}

impl RpcErrorKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport",
            Self::Decode(_) => "decode",
            Self::Rpc(_) => "rpc",
        }
    }
}

impl RpcError {
    pub(crate) fn transport(method: &str, msg: impl Into<String>) -> Self {
        Self {
            method: method.to_owned(),
            kind: RpcErrorKind::Transport(msg.into()),
        }
    }

    fn decode(method: &str, msg: impl Into<String>, _bytes: &[u8]) -> Self {
        Self {
            method: method.to_owned(),
            kind: RpcErrorKind::Decode(msg.into()),
        }
    }

    fn rpc(method: &str, error: RpcErrorObject) -> Self {
        Self {
            method: method.to_owned(),
            kind: RpcErrorKind::Rpc(error),
        }
    }

    fn rpc_simple(method: &str, msg: &str) -> Self {
        Self {
            method: method.to_owned(),
            kind: RpcErrorKind::Rpc(RpcErrorObject {
                code: 0,
                message: msg.to_owned(),
                data: None,
            }),
        }
    }
}

impl std::error::Error for RpcError {}

// ────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_get_balance_envelope() {
        let body = br#"{"jsonrpc":"2.0","result":{"context":{"slot":123},"value":1234567890},"id":1}"#;
        let env: Envelope<ContextResponse<u64>> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Ok { result, .. } => assert_eq!(result.value, 1_234_567_890),
            Envelope::Err { .. } => panic!("expected Ok"),
        }
    }

    #[test]
    fn should_parse_error_envelope() {
        let body = br#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"invalid"},"id":1}"#;
        let env: Envelope<ContextResponse<u64>> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Err { error, .. } => assert_eq!(error.code, -32602),
            Envelope::Ok { .. } => panic!("expected Err"),
        }
    }

    #[test]
    fn should_parse_latest_blockhash() {
        let body = br#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"blockhash":"EkSnNWid2cvwEVnVx9aBqawnmiCNiDgp3gUdkDPTKN1N","lastValidBlockHeight":12}},"id":1}"#;
        let env: Envelope<ContextResponse<LatestBlockhashValue>> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Ok { result, .. } => {
                assert_eq!(result.value.blockhash, "EkSnNWid2cvwEVnVx9aBqawnmiCNiDgp3gUdkDPTKN1N");
                let parsed = Hash::from_str(&result.value.blockhash).unwrap();
                assert_eq!(parsed.to_string(), result.value.blockhash);
            },
            _ => panic!(),
        }
    }

    #[test]
    fn should_parse_token_account_balance() {
        let body = br#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"amount":"100","decimals":6,"uiAmount":0.0001,"uiAmountString":"0.0001"}},"id":1}"#;
        let env: Envelope<ContextResponse<TokenAmount>> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Ok { result, .. } => {
                assert_eq!(result.value.amount, "100");
                assert_eq!(result.value.decimals, 6);
                assert_eq!(result.value.ui_amount_string, "0.0001");
            },
            _ => panic!(),
        }
    }

    #[test]
    fn should_parse_token_accounts_by_owner() {
        let body = br#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":[{"pubkey":"So11111111111111111111111111111111111111112","account":{"data":""}}]},"id":1}"#;
        let env: Envelope<ContextResponse<Vec<KeyedTokenAccount>>> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Ok { result, .. } => {
                assert_eq!(result.value.len(), 1);
                assert_eq!(result.value[0].pubkey, "So11111111111111111111111111111111111111112");
            },
            _ => panic!(),
        }
    }

    #[test]
    fn should_parse_send_transaction() {
        let body = br#"{"jsonrpc":"2.0","result":"5h3kS6vr8b8X9ksuY3jLZQbHWfqaJsy2DqERkdMzfiNJjQAQ4qAW9z3GjvPvjyDgyy3yL5fNwbpxgvCQEwTtq8R","id":1}"#;
        let env: Envelope<String> = serde_json::from_slice(body).unwrap();
        match env {
            Envelope::Ok { result, .. } => assert_eq!(result.len(), 87),
            _ => panic!(),
        }
    }

    #[test]
    fn should_serialize_jsonrpc_envelope_with_monotonic_ids() {
        let client = SolanaRpcClient::new("http://localhost:8899");
        let id1 = client.next_id();
        let id2 = client.next_id();
        assert!(id2 > id1);
    }

    #[test]
    fn should_render_token_accounts_filter_as_mint() {
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let v = TokenAccountsFilter::Mint(mint).into_json();
        assert_eq!(v["mint"], "So11111111111111111111111111111111111111112");
    }

    /// T40.10 (R40.5A.1): a request to a closed port and the debug forms name the node host-only.
    #[test]
    fn t40_10_a_node_is_named_host_only_in_errors_and_debug_forms() {
        let url = "http://user:secret@127.0.0.1:1/v1/APIKEY?token=X";
        let client = SolanaRpcClient::new(url);
        let pool = crate::solana::rpc_pool::SolanaRpcPool::from_urls([url]).unwrap();
        let error = common::block_on(client.get_health()).unwrap_err().to_string();
        let pool_error = common::block_on(pool.get_health()).unwrap_err().to_string();
        for text in [error, pool_error, format!("{client:?}"), format!("{pool:?}")] {
            assert!(text.contains("http://127.0.0.1:1"), "{text}");
            for secret in ["user", "secret", "/v1/", "APIKEY", "token=X"] {
                assert!(!text.contains(secret), "a credential part reaches a text");
            }
        }
    }
}
