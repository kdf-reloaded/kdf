//! `wc_sessionSettle` payloads.

use super::IrnTag;
use crate::session::SessionType;
use relay_rpc::rpc::params::{Metadata, Relay};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// IRN relay tags for `wc_sessionSettle`.
pub const TAG: IrnTag = IrnTag {
    request: 1102,
    response: 1103,
};

/// A settled namespace entry: accounts plus the granted methods and events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleNamespace {
    #[serde(default)]
    pub accounts: BTreeSet<String>,
    #[serde(default)]
    pub methods: BTreeSet<String>,
    #[serde(default)]
    pub events: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chains: Option<BTreeSet<String>>,
}

/// The controlling peer's identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Controller {
    pub public_key: String,
    pub metadata: Metadata,
}

/// `wc_sessionSettle` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleRequest {
    pub relay: Relay,
    pub controller: Controller,
    pub namespaces: BTreeMap<String, SettleNamespace>,
    pub expiry: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_properties: Option<crate::session::SessionProperties>,
}

impl SettleRequest {
    /// The negotiated peer role for a settled session is always the controller.
    pub const PEER_ROLE: SessionType = SessionType::Controller;
}
