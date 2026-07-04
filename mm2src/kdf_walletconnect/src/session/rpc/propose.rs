//! `wc_sessionPropose` payloads.

use super::IrnTag;
use relay_rpc::rpc::params::{Metadata, Relay};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// JSON-RPC `method` for a `wc_sessionPropose` request.
pub const METHOD: &str = "wc_sessionPropose";

/// IRN relay tags for `wc_sessionPropose`.
pub const TAG: IrnTag = IrnTag {
    request: 1100,
    response: 1101,
};

/// A single proposed namespace entry (chains, methods, events).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposeNamespace {
    #[serde(default)]
    pub chains: BTreeSet<String>,
    #[serde(default)]
    pub methods: BTreeSet<String>,
    #[serde(default)]
    pub events: BTreeSet<String>,
}

/// The proposing peer's identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposer {
    pub public_key: String,
    pub metadata: Metadata,
}

/// `wc_sessionPropose` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeRequest {
    pub relays: Vec<Relay>,
    pub proposer: Proposer,
    pub required_namespaces: std::collections::BTreeMap<String, ProposeNamespace>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub optional_namespaces: Option<std::collections::BTreeMap<String, ProposeNamespace>>,
}

/// `wc_sessionPropose` response, carrying the responder's public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeResponse {
    pub relay: Relay,
    pub responder_public_key: String,
}
