//! The typed failures of the daemon and network RPC cores (CRD ch.43 §43.1R; plan `mmrpc-v2-completion`, group E):
//! `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`,
//! `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `ban_pubkey`, `list_banned_pubkeys` and
//! `unban_pubkeys`.
//!
//! Each core fails with an error enum in the adjacently tagged form of the versioned methods (`error_type`,
//! `error_data`), a status (`HttpStatusCode`) and a `Display` text. The `Display` text of a variant is the text the
//! legacy handler produced for that failure, without its `file:line] ` prefix, so the legacy surface renders the same
//! answer from it (see `crate::mm2::rpc::legacy_adapter`).
//!
//! The tokens, payload kinds and statuses follow the nearest documented versioned sibling for the peer and network
//! queries, `peer_connection_healthcheck` (`Internal`, string, 500; CRD ch.43 R43.1C.8, D43.1C.4 option (ii)). The ban
//! methods have no sibling: a request the caller got wrong is `InvalidParam` (string, 400) and every other failure is
//! `Internal` (string, 500), the general convention.
//!
//! A request that cannot be decoded (a public key that is not 32 hex bytes, a missing member) never reaches a core: the
//! versioned dispatcher answers `InvalidRequest` (string, 400) for it.

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use std::fmt;

/// The failures of a core that cannot fail: `version`, `stop`, `get_directly_connected_peers`, `get_gossip_mesh`,
/// `get_gossip_peer_topics`, `get_gossip_topic_peers` and `get_relay_mesh`. The versioned envelope needs an error type,
/// so this is an enum without variants: no value of it exists, so these calls have no error wire form.
#[derive(Debug, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum InfallibleRpcError {}

impl fmt::Display for InfallibleRpcError {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result { match *self {} }
}

impl HttpStatusCode for InfallibleRpcError {
    fn status_code(&self) -> StatusCode { match *self {} }
}

/// The failures of `get_my_peer_id`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetMyPeerIdError {
    /// The node has no peer identifier yet. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl HttpStatusCode for GetMyPeerIdError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetMyPeerIdError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `metrics`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MetricsError {
    /// The metrics registry is not initialized or cannot be read. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl HttpStatusCode for MetricsError {
    fn status_code(&self) -> StatusCode {
        match self {
            MetricsError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `ban_pubkey`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum BanPubkeyError {
    /// The public key is banned already. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InvalidParam(String),
    /// The swap context or the ban list could not be read. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl HttpStatusCode for BanPubkeyError {
    fn status_code(&self) -> StatusCode {
        match self {
            BanPubkeyError::InvalidParam(_) => StatusCode::BAD_REQUEST,
            BanPubkeyError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `list_banned_pubkeys` and `unban_pubkeys`, which fail alike: the swap context or the ban list cannot
/// be read, or the ban list cannot be turned into its answer.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum BanListError {
    /// The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl HttpStatusCode for BanListError {
    fn status_code(&self) -> StatusCode {
        match self {
            BanListError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRD ch.43 R43.8.6: the serialized `error_type` token, the JSON type of `error_data`, the HTTP status and the
    /// `Display` text of an error.
    fn assert_wire<E>(error: &E, token: &str, status: u16, text: &str)
    where
        E: serde::Serialize + HttpStatusCode + fmt::Display,
    {
        // No assertion message below carries the error, its payload or its text: only the token.
        let wire = serde_json::to_value(error).unwrap();
        assert_eq!(wire["error_type"], token, "{token}: error_type");
        assert!(wire["error_data"].is_string(), "{token}: error_data kind");
        assert_eq!(error.status_code().as_u16(), status, "{token}: status");
        assert!(error.to_string() == text, "{token}: Display text");
    }

    #[test]
    fn daemon_error_wire_forms() {
        assert_wire(
            &GetMyPeerIdError::Internal("Peer ID is not initialized".into()),
            "Internal",
            500,
            "Peer ID is not initialized",
        );
        assert_wire(
            &MetricsError::Internal("metrics system is not initialized yet".into()),
            "Internal",
            500,
            "metrics system is not initialized yet",
        );
        assert_wire(
            &BanPubkeyError::InvalidParam("Pubkey is banned already".into()),
            "InvalidParam",
            400,
            "Pubkey is banned already",
        );
        assert_wire(
            &BanPubkeyError::Internal("poisoned".into()),
            "Internal",
            500,
            "poisoned",
        );
        assert_wire(&BanListError::Internal("poisoned".into()), "Internal", 500, "poisoned");
    }
}
