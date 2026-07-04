//! Top-level WalletConnect v2 JSON-RPC handlers (chapter 22 §22.9A.2).
//!
//! These five unnamespaced methods drive the WalletConnect pairing and session
//! lifecycle from the public RPC dispatcher:
//!
//! - `wc_new_connection` — start a pairing, return the `wc:` URI + pairing topic
//! - `wc_get_sessions`   — enumerate every live session
//! - `wc_get_session`    — resolve a single session by topic
//! - `wc_delete_session` — tear a session down and forget it
//! - `wc_ping_session`   — ping a session and report success/failure
//!
//! The handlers are chain-agnostic: per-coin signing is layered separately
//! through the §22.3 integration trait and is out of scope here.

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::{SessionInfo, Topic, WalletConnectCtx};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::sync::Arc;

/// Typed error envelope for the WalletConnect RPC surface (chapter 22 §22.9A.2
/// RP7). The three variants are the three wire `error_type` tokens; their
/// human-readable messages are diagnostic and not part of the contract.
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
// Variants intentionally share the idiomatic `Error` suffix; renaming them is
// churny and hurts readability.
#[allow(clippy::enum_variant_names)]
pub enum WalletConnectRpcError {
    /// An initialisation or precondition failure (client error / 400): the
    /// subsystem could not be brought up, or the request referenced something
    /// that does not exist yet.
    #[display(fmt = "WalletConnect initialization failed: {}", _0)]
    InitializationError(String),
    /// A WC2 session request (ping / delete / propose) failed at the relay or
    /// transport (server error / 500).
    #[display(fmt = "WalletConnect session request failed: {}", _0)]
    SessionRequestError(String),
    /// A generic internal failure (server error / 500).
    #[display(fmt = "WalletConnect internal error: {}", _0)]
    InternalError(String),
}

impl HttpStatusCode for WalletConnectRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            WalletConnectRpcError::InitializationError(_) => StatusCode::BAD_REQUEST,
            WalletConnectRpcError::SessionRequestError(_) | WalletConnectRpcError::InternalError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

/// Convenience alias for the WalletConnect RPC handler results.
pub type WcRpcResult<T> = Result<T, MmError<WalletConnectRpcError>>;

/// `wc_new_connection` request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewConnectionRequest {
    /// Required CAIP namespace requirements (a JSON object).
    pub required_namespaces: Map<String, Json>,
    /// Optional CAIP namespace requirements (a JSON object).
    #[serde(default)]
    pub optional_namespaces: Option<Map<String, Json>>,
}

/// `wc_get_sessions` request: no fields. An empty object is accepted; a missing
/// parameter object (`null`) is accepted by typing the handler argument as
/// `Option<GetSessionsRequest>`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSessionsRequest {}

/// `wc_get_session` request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSessionRequest {
    /// Session topic (or pairing topic, when `with_pairing_topic` is set).
    pub topic: String,
    /// When `true`, also resolve a session whose pairing topic equals `topic`.
    #[serde(default)]
    pub with_pairing_topic: bool,
}

/// Shared request shape for `wc_delete_session` and `wc_ping_session`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTopicRequest {
    /// Session topic to act on.
    pub topic: String,
}

/// `wc_new_connection` response.
#[derive(Debug, Serialize)]
pub struct NewConnectionResponse {
    /// The `wc:` pairing URI, delivered verbatim.
    pub url: String,
    /// The pairing topic the connection was initiated on.
    pub pairing_topic: String,
}

/// `wc_get_sessions` response.
#[derive(Serialize)]
pub struct GetSessionsResponse {
    /// Every live session, in [`SessionInfo`] wire shape.
    pub sessions: Vec<SessionInfo>,
}

/// `wc_get_session` response.
#[derive(Serialize)]
pub struct GetSessionResponse {
    /// The resolved session, or `null` when no session matched.
    pub session: Option<SessionInfo>,
}

/// `wc_delete_session` response: an empty object.
#[derive(Debug, Serialize)]
pub struct EmptyResponse {}

/// `wc_ping_session` response.
#[derive(Debug, Serialize)]
pub struct PingSessionResponse {
    /// Human-readable status; the exact wording is not part of the contract.
    pub result: String,
}

/// Lazily initialises (and caches on the [`MmArc`]) the per-context
/// [`WalletConnectCtx`] handle (chapter 22 §22.2). The handle connects to the
/// relay on first construction; subsequent calls reuse the cached instance.
async fn wc_ctx(ctx: &MmArc) -> WcRpcResult<Arc<WalletConnectCtx>> {
    if let Some(existing) = ctx.wallet_connect.lock().unwrap().clone() {
        return downcast_wc_ctx(existing);
    }

    let handle = WalletConnectCtx::init(ctx)
        .await
        .map_to_mm(|e| WalletConnectRpcError::InitializationError(e.to_string()))?;

    let mut guard = ctx.wallet_connect.lock().unwrap();
    // Another caller may have raced us to initialise while we were connecting;
    // if so, keep the already-stored handle and drop ours.
    if let Some(existing) = guard.clone() {
        return downcast_wc_ctx(existing);
    }
    let any: Arc<dyn std::any::Any + Send + Sync> = handle.clone();
    *guard = Some(any);
    Ok(handle)
}

/// Downcasts the type-erased cached handle back to a [`WalletConnectCtx`].
fn downcast_wc_ctx(any: Arc<dyn std::any::Any + Send + Sync>) -> WcRpcResult<Arc<WalletConnectCtx>> {
    any.downcast::<WalletConnectCtx>().map_err(|_| {
        MmError::new(WalletConnectRpcError::InternalError(
            "unexpected WalletConnect context type".to_string(),
        ))
    })
}

/// Maps a subsystem [`WalletConnectError`] onto the public RP7 error envelope.
fn map_session_err(e: WalletConnectError) -> MmError<WalletConnectRpcError> {
    let token = match &e {
        WalletConnectError::Config(_) | WalletConnectError::SessionNotFound(_) => {
            WalletConnectRpcError::InitializationError(e.to_string())
        },
        WalletConnectError::Relay(_) | WalletConnectError::Timeout | WalletConnectError::Codec(_) => {
            WalletConnectRpcError::SessionRequestError(e.to_string())
        },
        _ => WalletConnectRpcError::InternalError(e.to_string()),
    };
    MmError::new(token)
}

/// `wc_new_connection`: initiate a new pairing and return its `wc:` URI and
/// pairing topic (chapter 22 §22.9A.2 RP5).
pub async fn wc_new_connection(ctx: MmArc, req: NewConnectionRequest) -> WcRpcResult<NewConnectionResponse> {
    let wc = wc_ctx(&ctx).await?;
    let (pairing_topic, url) = wc
        .new_connection(
            Json::Object(req.required_namespaces),
            req.optional_namespaces.map(Json::Object),
        )
        .await
        .map_err(map_session_err)?;
    Ok(NewConnectionResponse {
        url,
        pairing_topic: pairing_topic.to_string(),
    })
}

/// `wc_get_sessions`: enumerate every live session (chapter 22 §22.9A.2 AC3).
pub async fn wc_get_sessions(ctx: MmArc, _req: Option<GetSessionsRequest>) -> WcRpcResult<GetSessionsResponse> {
    let wc = wc_ctx(&ctx).await?;
    Ok(GetSessionsResponse {
        sessions: wc.sessions().all_session_info(),
    })
}

/// `wc_get_session`: resolve a single session by topic (and, when
/// `with_pairing_topic` is set, by pairing topic) (chapter 22 §22.9A.2 AC3).
pub async fn wc_get_session(ctx: MmArc, req: GetSessionRequest) -> WcRpcResult<GetSessionResponse> {
    let wc = wc_ctx(&ctx).await?;
    let topic = Topic::from(req.topic);
    Ok(GetSessionResponse {
        session: wc.sessions().session_info(&topic, req.with_pairing_topic),
    })
}

/// `wc_delete_session`: perform the WC2 session delete, drop the persisted
/// record (subject to `wc_session_persistence`) and unsubscribe (chapter 22
/// §22.9A.2 RP5 / AC4).
pub async fn wc_delete_session(ctx: MmArc, req: SessionTopicRequest) -> WcRpcResult<EmptyResponse> {
    let wc = wc_ctx(&ctx).await?;
    wc.drop_session(&Topic::from(req.topic))
        .await
        .map_err(map_session_err)?;
    Ok(EmptyResponse {})
}

/// `wc_ping_session`: issue a WC2 session ping and report success/failure
/// (chapter 22 §22.9A.2 RP5).
pub async fn wc_ping_session(ctx: MmArc, req: SessionTopicRequest) -> WcRpcResult<PingSessionResponse> {
    let wc = wc_ctx(&ctx).await?;
    wc.ping_session(&Topic::from(req.topic))
        .await
        .map_err(map_session_err)?;
    Ok(PingSessionResponse {
        result: "session ping succeeded".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_token(e: MmError<WalletConnectRpcError>) -> String {
        serde_json::to_value(e.into_inner()).unwrap()["error_type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn new_connection_request_is_strict() {
        let ok: NewConnectionRequest = serde_json::from_value(json!({
            "required_namespaces": { "eip155": {} },
            "optional_namespaces": { "cosmos": {} },
        }))
        .expect("valid request");
        assert!(ok.required_namespaces.contains_key("eip155"));
        assert!(ok.optional_namespaces.is_some());

        // optional_namespaces may be omitted.
        let minimal: NewConnectionRequest =
            serde_json::from_value(json!({ "required_namespaces": {} })).expect("minimal request");
        assert!(minimal.optional_namespaces.is_none());

        // required_namespaces is mandatory.
        assert!(serde_json::from_value::<NewConnectionRequest>(json!({})).is_err());
        // unknown fields are rejected.
        assert!(serde_json::from_value::<NewConnectionRequest>(json!({
            "required_namespaces": {},
            "surprise": 1,
        }))
        .is_err());
        // required_namespaces must be an object.
        assert!(serde_json::from_value::<NewConnectionRequest>(json!({ "required_namespaces": "nope" })).is_err());
    }

    #[test]
    fn get_session_request_defaults_with_pairing_topic_false() {
        let req: GetSessionRequest = serde_json::from_value(json!({ "topic": "abc" })).expect("valid request");
        assert_eq!(req.topic, "abc");
        assert!(!req.with_pairing_topic);

        let req: GetSessionRequest =
            serde_json::from_value(json!({ "topic": "abc", "with_pairing_topic": true })).expect("valid request");
        assert!(req.with_pairing_topic);

        // topic is mandatory; unknown fields rejected.
        assert!(serde_json::from_value::<GetSessionRequest>(json!({})).is_err());
        assert!(serde_json::from_value::<GetSessionRequest>(json!({ "topic": "a", "x": 1 })).is_err());
    }

    #[test]
    fn session_topic_request_is_strict() {
        let req: SessionTopicRequest = serde_json::from_value(json!({ "topic": "t" })).expect("valid request");
        assert_eq!(req.topic, "t");
        assert!(serde_json::from_value::<SessionTopicRequest>(json!({})).is_err());
        assert!(serde_json::from_value::<SessionTopicRequest>(json!({ "topic": "t", "extra": true })).is_err());
    }

    #[test]
    fn get_sessions_request_accepts_empty_and_absent_params() {
        // Empty object → Some(default).
        let some: Option<GetSessionsRequest> = serde_json::from_value(json!({})).expect("empty object accepted");
        assert!(some.is_some());
        // Absent params (null) → None.
        let none: Option<GetSessionsRequest> = serde_json::from_value(json!(null)).expect("null accepted");
        assert!(none.is_none());
        // Unknown fields are still rejected.
        assert!(serde_json::from_value::<Option<GetSessionsRequest>>(json!({ "x": 1 })).is_err());
    }

    #[test]
    fn error_envelope_status_codes() {
        assert_eq!(
            WalletConnectRpcError::InitializationError("x".to_string()).status_code(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            WalletConnectRpcError::SessionRequestError("x".to_string()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert_eq!(
            WalletConnectRpcError::InternalError("x".to_string()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn error_envelope_serialises_tag_and_content() {
        let value = serde_json::to_value(WalletConnectRpcError::InitializationError("boom".to_string())).unwrap();
        assert_eq!(value["error_type"], "InitializationError");
        assert_eq!(value["error_data"], "boom");
    }

    #[test]
    fn map_session_err_buckets_into_three_tokens() {
        // init / precondition (400)
        assert_eq!(
            error_token(map_session_err(WalletConnectError::Config("c".to_string()))),
            "InitializationError",
        );
        assert_eq!(
            error_token(map_session_err(WalletConnectError::SessionNotFound("t".to_string()))),
            "InitializationError",
        );
        // session-request (500)
        assert_eq!(
            error_token(map_session_err(WalletConnectError::Relay("r".to_string()))),
            "SessionRequestError",
        );
        assert_eq!(
            error_token(map_session_err(WalletConnectError::Timeout)),
            "SessionRequestError",
        );
        // generic internal (500)
        assert_eq!(
            error_token(map_session_err(WalletConnectError::Storage("s".to_string()))),
            "InternalError",
        );
    }
}
