//! Session data types: the negotiated session role, the per-account key
//! material wallets advertise, the session-properties envelope, and the
//! payload-encoding negotiated at settlement.

use crate::error::WalletConnectError;
use crate::storage::StoredSession;
use parking_lot::Mutex;
use relay_rpc::domain::{SubscriptionId, Topic};
use relay_rpc::rpc::params::session::{ProposeNamespaces, SettleNamespaces};
use relay_rpc::rpc::params::{Metadata, Relay};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use wc_common::SymKey;

pub mod key;
pub mod rpc;

pub use key::SessionKey;

/// Which side controls a session. The two spellings are dictated by the
/// WalletConnect session-settlement payload, so the serde representation is the
/// variant name verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionType {
    /// The settling wallet (controller).
    Controller,
    /// The proposing dApp (this codebase).
    Proposer,
}

/// Per-account key information a wallet reports inside `sessionProperties`.
///
/// Field spellings on the wire are camelCase, matching the wallet
/// (Keplr-style) payloads this is decoded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub chain_id: String,
    pub name: String,
    pub algo: String,
    pub pub_key: String,
    pub address: String,
    pub bech32_address: String,
    pub ethereum_hex_address: String,
    pub is_nano_ledger: bool,
    pub is_keystone: bool,
}

/// The `sessionProperties` object. The only field of interest is `keys`, which
/// some wallets deliver as a JSON array and others (notably Keplr) deliver as a
/// JSON-encoded string. Both shapes decode to the same value; an absent field
/// decodes to `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProperties {
    #[serde(default, deserialize_with = "deserialize_keys")]
    pub keys: Option<Vec<KeyInfo>>,
}

/// Accepts `keys` as either a JSON-encoded string or a JSON array.
fn deserialize_keys<'de, D>(deserializer: D) -> Result<Option<Vec<KeyInfo>>, D::Error>
where
    D: Deserializer<'de>,
{
    /// Either spelling the wire may carry for `keys`.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum KeysWire {
        Encoded(String),
        Decoded(Vec<KeyInfo>),
    }

    match Option::<KeysWire>::deserialize(deserializer)? {
        None => Ok(None),
        Some(KeysWire::Decoded(keys)) => Ok(Some(keys)),
        Some(KeysWire::Encoded(blob)) => serde_json::from_str(&blob).map(Some).map_err(serde::de::Error::custom),
    }
}

/// The transport encoding negotiated for a session's payloads: hex for most
/// wallets, base64 for the ones that require it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EncodingAlgo {
    #[default]
    Hex,
    Base64,
}

impl EncodingAlgo {
    /// Encodes the given bytes with the negotiated scheme.
    pub fn encode<T: AsRef<[u8]>>(&self, data: T) -> String {
        match self {
            EncodingAlgo::Hex => hex::encode(data),
            EncodingAlgo::Base64 => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(data)
            },
        }
    }
}

/// A live, settled session held in memory.
pub struct Session {
    /// The session topic the peers exchange messages on.
    pub topic: Topic,
    /// The pairing topic the session was negotiated through.
    pub pairing_topic: Topic,
    /// Symmetric key material for this session.
    pub session_key: SessionKey,
    /// Which side controls the session (always the wallet once settled).
    pub controller: SessionType,
    /// The wallet's advertised metadata (the `controller` descriptor on disk).
    pub metadata: Metadata,
    /// Absolute expiry timestamp (unix seconds).
    pub expiry: u64,
    /// The negotiated payload encoding for this session.
    pub encoding: EncodingAlgo,
    /// Optional session properties (per-account key material).
    pub properties: Option<SessionProperties>,
    /// Relay subscription id assigned once subscribed.
    pub subscription_id: Option<SubscriptionId>,
    /// This dApp's advertised metadata (the `proposer` descriptor on disk).
    pub proposer: Metadata,
    /// The relay descriptor negotiated for this session.
    pub relay: Relay,
    /// The agreed namespaces (settle-time).
    pub namespaces: SettleNamespaces,
    /// The proposed namespaces (propose-time).
    pub propose_namespaces: ProposeNamespaces,
    /// Optional active CAIP-2 chain id.
    pub active_chain_id: Option<String>,
}

/// The JSON payload stored in the `data` column / object-store field of a
/// persisted session row.
///
/// **Externally constrained (Interop / wire-format reuse, chapter 22 \u00a722.5.3).**
/// The key names below are byte-interchangeable with GLEEC KDF in both
/// directions and MUST be emitted and consumed exactly as named. New keys may
/// be added additively but existing ones must not be renamed or dropped.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Session topic; mirrors the row's primary key.
    pub topic: String,
    /// Relay subscription id.
    #[serde(default)]
    pub subscription_id: Option<SubscriptionId>,
    /// Session key material (`sym_key` + `public_key`).
    pub session_key: SessionKey,
    /// Controlling party (wallet) descriptor.
    pub controller: Metadata,
    /// Proposing party (this dApp) descriptor.
    pub proposer: Metadata,
    /// Relay descriptor.
    pub relay: Relay,
    /// Agreed namespaces.
    pub namespaces: SettleNamespaces,
    /// Proposed namespaces.
    pub propose_namespaces: ProposeNamespaces,
    /// Expiry, Unix epoch seconds (mirrors the column).
    pub expiry: u64,
    /// Pairing topic.
    pub pairing_topic: String,
    /// Controller / Proposer role.
    pub session_type: SessionType,
    /// Optional wallet-reported session properties.
    #[serde(default)]
    pub session_properties: Option<SessionProperties>,
    /// Optional active CAIP-2 chain id.
    #[serde(default)]
    pub active_chain_id: Option<String>,
    /// Negotiated transport encoding (hex / base64).
    #[serde(default)]
    pub encoding_algo: EncodingAlgo,
}

impl From<&Session> for SessionRecord {
    fn from(session: &Session) -> Self {
        SessionRecord {
            topic: session.topic.to_string(),
            subscription_id: session.subscription_id.clone(),
            session_key: session.session_key.clone(),
            controller: session.metadata.clone(),
            proposer: session.proposer.clone(),
            relay: session.relay.clone(),
            namespaces: session.namespaces.clone(),
            propose_namespaces: session.propose_namespaces.clone(),
            expiry: session.expiry,
            pairing_topic: session.pairing_topic.to_string(),
            session_type: session.controller,
            session_properties: session.properties.clone(),
            active_chain_id: session.active_chain_id.clone(),
            encoding_algo: session.encoding,
        }
    }
}

impl From<SessionRecord> for Session {
    fn from(record: SessionRecord) -> Self {
        Session {
            topic: Topic::from(record.topic),
            pairing_topic: Topic::from(record.pairing_topic),
            session_key: record.session_key,
            controller: record.session_type,
            metadata: record.controller,
            expiry: record.expiry,
            encoding: record.encoding_algo,
            properties: record.session_properties,
            subscription_id: record.subscription_id,
            proposer: record.proposer,
            relay: record.relay,
            namespaces: record.namespaces,
            propose_namespaces: record.propose_namespaces,
            active_chain_id: record.active_chain_id,
        }
    }
}

impl Session {
    /// Serializes the session into a persistable storage row (the `open`
    /// plaintext format of chapter 22 \u00a722.5.3).
    ///
    /// # Errors
    /// Returns [`WalletConnectError::Serde`] if the record cannot be encoded.
    pub fn to_stored(&self) -> Result<StoredSession, WalletConnectError> {
        let record = SessionRecord::from(self);
        let data = serde_json::to_string(&record)?;
        Ok(StoredSession {
            topic: self.topic.to_string(),
            data,
            expiry: self.expiry as i64,
        })
    }

    /// Reconstructs an in-memory session from a persisted storage row,
    /// rebuilding the symmetric key so the restored session can decrypt.
    ///
    /// Loading is unconditional and format-autodetecting (chapter 22 \u00a722.5.2).
    /// Today only the `open` plaintext format exists.
    ///
    /// # Errors
    /// Returns [`WalletConnectError::Serde`] if the stored `data` cannot be
    /// decoded.
    pub fn from_stored(stored: &StoredSession) -> Result<Session, WalletConnectError> {
        // TODO(ch22-D8): when the encrypted-at-rest format ships, auto-detect it
        // here from a distinct on-disk discriminator (a renamed field/column or
        // an explicit format/version marker) and dispatch to the matching
        // decoder. Until then only the `open` plaintext record exists, so the
        // payload is always decoded as a plaintext `SessionRecord`.
        let record: SessionRecord = serde_json::from_str(&stored.data)?;
        Ok(Session::from(record))
    }
}

/// The public `session-info` wire record (chapter 22 §22.8.1.5 / §22.9A.2 RP6).
///
/// Returned by the `wc_get_session` / `wc_get_sessions` RPC handlers. The five
/// top-level field spellings are dictated by §22.8.1.5 and are exhaustive: the
/// per-account `sessionProperties.keys` detail (§22.8.1.6) is delivered at
/// session-settle and consumed internally by the signing integrations, not
/// emitted in this record.
#[derive(Clone, Serialize)]
pub struct SessionInfo {
    /// Session topic.
    pub topic: String,
    /// Wallet-reported WC2 app metadata.
    pub metadata: Metadata,
    /// Originating pairing topic.
    pub pairing_topic: String,
    /// Map: agreed CAIP namespace → WC2 namespace record.
    pub namespaces: SettleNamespaces,
    /// Session expiry, Unix epoch seconds.
    pub expiry: u64,
}

impl From<&Session> for SessionInfo {
    fn from(session: &Session) -> Self {
        SessionInfo {
            topic: session.topic.to_string(),
            metadata: session.metadata.clone(),
            pairing_topic: session.pairing_topic.to_string(),
            namespaces: session.namespaces.clone(),
            expiry: session.expiry,
        }
    }
}

/// In-memory index of live sessions, keyed by topic.
#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<Topic, Session>>,
}
impl SessionManager {
    pub fn new() -> Self { Self::default() }

    /// Inserts or replaces a session.
    pub fn insert(&self, session: Session) { self.sessions.lock().insert(session.topic.clone(), session); }

    /// Removes the session with the given topic.
    pub fn remove(&self, topic: &Topic) -> bool { self.sessions.lock().remove(topic).is_some() }

    /// Returns the number of live sessions.
    pub fn len(&self) -> usize { self.sessions.lock().len() }

    /// Whether there are no live sessions.
    pub fn is_empty(&self) -> bool { self.sessions.lock().is_empty() }

    /// The topics of all live sessions.
    pub fn topics(&self) -> Vec<Topic> { self.sessions.lock().keys().cloned().collect() }

    /// Builds the [`SessionInfo`] wire record for a single session, looked up by
    /// its topic. When `include_pairing` is set, a session is also matched if
    /// `topic` equals its pairing topic (chapter 22 §22.9A.2 AC3).
    pub fn session_info(&self, topic: &Topic, include_pairing: bool) -> Option<SessionInfo> {
        let guard = self.sessions.lock();
        let session = guard.get(topic).or_else(|| {
            if include_pairing {
                guard.values().find(|session| &session.pairing_topic == topic)
            } else {
                None
            }
        })?;
        Some(SessionInfo::from(session))
    }

    /// Builds the [`SessionInfo`] wire records for every live session.
    pub fn all_session_info(&self) -> Vec<SessionInfo> {
        self.sessions.lock().values().map(SessionInfo::from).collect()
    }

    /// The transport material needed to encrypt/decrypt traffic on a session:
    /// its symmetric key and the negotiated payload encoding. `None` when no
    /// session is registered for the topic.
    pub fn transport_for(&self, topic: &Topic) -> Option<(SymKey, EncodingAlgo)> {
        self.sessions
            .lock()
            .get(topic)
            .map(|session| (session.session_key.symmetric_key(), session.encoding))
    }

    /// Overwrites the absolute expiry of a live session (used when the wallet
    /// extends it). Returns whether a matching session was present.
    pub fn set_expiry(&self, topic: &Topic, expiry: u64) -> bool {
        match self.sessions.lock().get_mut(topic) {
            Some(session) => {
                session.expiry = expiry;
                true
            },
            None => false,
        }
    }

    /// Resolves the topic of a settled session whose negotiated namespaces grant
    /// the given CAIP-2 chain id (e.g. `eip155:1`).
    ///
    /// This is the lookup behind the §22.3 integration trait's session-pointer
    /// method: a coin support module knows only its own CAIP-2 chain id and asks
    /// the subsystem which settled session may carry signing requests for it. A
    /// session grants a chain when any of its agreed namespace entries lists the
    /// chain in its `chains` set or carries a CAIP-10 account under that chain
    /// (§22.8.1.4). The first matching session (by topic order) is returned.
    pub fn session_topic_for_chain(&self, chain_id: &str) -> Option<Topic> {
        self.sessions
            .lock()
            .values()
            .find(|session| session_grants_chain(&session.namespaces, chain_id))
            .map(|session| session.topic.clone())
    }

    /// Resolves, for the session on `topic`, the wallet's WC2 app-metadata `name`
    /// and the `sessionProperties.keys` entry matching the signing account
    /// `address` (matched against the entry's `bech32Address` or raw `address`).
    ///
    /// Both outputs are dictated wire signals consumed by a coin integration: the
    /// metadata `name` drives the Cosmos binary-field encoding rule (chapter 22
    /// §22.8.1.2 — `Keplr` ⇒ base64, otherwise hex) and the matched entry's
    /// `isNanoLedger` flag drives the amino-vs-direct selection (§22.8.1.7). The
    /// return is intentionally the generic WC types ([`KeyInfo`]), never a
    /// chain-family type, so this stays coin-agnostic.
    ///
    /// Returns `None` only when no session is registered for `topic`; a session
    /// without a matching keys entry yields `(name, None)`.
    pub fn signing_account_details(&self, topic: &Topic, address: &str) -> Option<(String, Option<KeyInfo>)> {
        let guard = self.sessions.lock();
        let session = guard.get(topic)?;
        let name = session.metadata.name.clone();
        let entry = session
            .properties
            .as_ref()
            .and_then(|props| props.keys.as_ref())
            .and_then(|keys| {
                keys.iter()
                    .find(|key| key.bech32_address == address || key.address == address)
                    .cloned()
            });
        Some((name, entry))
    }
}

/// Whether a session's agreed namespaces grant the given CAIP-2 chain id.
///
/// A chain is granted when any namespace entry lists it in `chains`, or carries
/// a CAIP-10 account (`<namespace>:<reference>:<address>`) under it.
fn session_grants_chain(namespaces: &SettleNamespaces, chain_id: &str) -> bool {
    let account_prefix = format!("{chain_id}:");
    namespaces.values().any(|namespace| {
        let in_chains = namespace
            .chains
            .as_ref()
            .map_or(false, |chains| chains.contains(chain_id));
        let in_accounts = namespace.accounts.as_ref().map_or(false, |accounts| {
            accounts.iter().any(|account| account.starts_with(&account_prefix))
        });
        in_chains || in_accounts
    })
}

#[cfg(test)]
mod tests {
    use super::session_grants_chain;
    use relay_rpc::rpc::params::session::{Namespace, SettleNamespaces};
    use std::collections::{BTreeMap, BTreeSet};

    fn namespaces_with(chains: &[&str], accounts: &[&str]) -> SettleNamespaces {
        let namespace = Namespace {
            chains: Some(chains.iter().map(|c| c.to_string()).collect::<BTreeSet<_>>()),
            accounts: Some(accounts.iter().map(|a| a.to_string()).collect::<BTreeSet<_>>()),
            methods: BTreeSet::new(),
            events: BTreeSet::new(),
        };
        let mut map = BTreeMap::new();
        map.insert("eip155".to_string(), namespace);
        SettleNamespaces(map)
    }

    #[test]
    fn grants_chain_via_chains_set() {
        let namespaces = namespaces_with(&["eip155:1", "eip155:137"], &[]);
        assert!(session_grants_chain(&namespaces, "eip155:1"));
        assert!(session_grants_chain(&namespaces, "eip155:137"));
        assert!(!session_grants_chain(&namespaces, "eip155:56"));
        assert!(!session_grants_chain(&namespaces, "cosmos:cosmoshub-4"));
    }

    #[test]
    fn grants_chain_via_caip10_account() {
        let namespaces = namespaces_with(&[], &["eip155:1:0x1111111111111111111111111111111111111111"]);
        assert!(session_grants_chain(&namespaces, "eip155:1"));
        // A prefix that is not a CAIP-10 chain boundary must not match.
        assert!(!session_grants_chain(&namespaces, "eip155:11"));
    }
}
