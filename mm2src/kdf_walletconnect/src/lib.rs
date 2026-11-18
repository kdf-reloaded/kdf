//! In-tree WalletConnect v2 dApp client.
//!
//! The subsystem acts purely as a relay-client / dApp: it generates pairings,
//! proposes and maintains sessions with external wallets, dispatches signing
//! requests over the encrypted JSON-RPC channel, and persists sessions across
//! restarts. It never fills the wallet role.
//!
//! Layering:
//! - [`chain`] — CAIP-2 chain taxonomy and request-method names.
//! - [`session`] — session data types, key derivation and per-method RPC.
//! - [`pairing`] — pairing material and `wc:` URI formatting.
//! - [`connection_handler`] / [`inbound_message`] — relay event loop plumbing.
//! - [`storage`] — session persistence (SQLite native, IndexedDB wasm).

use connection_handler::{RelayEvent, WcConnectionHandler};
use error::WalletConnectError;
use inbound_message::PendingRequests;
use mm2_core::mm_ctx::MmArc;
use pairing::Pairing;
use parking_lot::Mutex;
use relay_client::websocket::{Client, PublishedMessage};
use relay_client::{ConnectionOptions, MessageIdGenerator};
use relay_rpc::auth::ed25519_dalek::SigningKey;
use relay_rpc::auth::AuthToken;
use relay_rpc::domain::MessageId;
/// The relay topic type is re-exported so consumers of the public handle can
/// name it without depending on `relay_rpc` directly.
pub use relay_rpc::domain::Topic;
use relay_rpc::rpc::params::session::{Namespace, ProposeNamespaces, SettleNamespaces};
use relay_rpc::rpc::params::{Metadata, Relay};
use session::rpc::{propose, settle};
use session::{EncodingAlgo, Session, SessionManager};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use wc_common::{EnvelopeType, SymKey};
use x25519_dalek::{PublicKey, StaticSecret};

pub mod chain;
mod client_key;
pub mod connection_handler;
pub mod error;
pub mod inbound_message;
pub mod integration;
pub mod metadata;
pub mod pairing;
pub mod session;
pub mod storage;

pub use session::key::SessionKey;
pub use session::SessionInfo;

/// Default time-to-live for a pairing before it expires (seconds).
pub const PAIRING_TTL_SECS: u64 = 5 * 60;

/// Default time-to-live for awaiting a wallet response to a session request.
pub const REQUEST_RESPONSE_TTL: Duration = Duration::from_secs(5 * 60);

/// Lifetime of the relay authentication token (seconds).
const RELAY_AUTH_TTL_SECS: u64 = 8 * 60 * 60;

/// JSON-RPC `method` for a signing request carried over a settled session.
const SESSION_REQUEST_METHOD: &str = "wc_sessionRequest";
/// IRN relay tag for an outbound `wc_sessionRequest`.
const SESSION_REQUEST_TAG: u32 = 1108;

/// First wait before a relay reconnect attempt; it doubles after every failed attempt.
const RECONNECT_BACKOFF_START_SECS: u64 = 1;
/// Longest wait between two relay reconnect attempts.
const RECONNECT_BACKOFF_MAX_SECS: u64 = 60;

/// The wait before reconnect attempt number `attempt` (counted from zero): 1 s, 2 s, 4 s, ... capped at
/// [`RECONNECT_BACKOFF_MAX_SECS`].
fn reconnect_backoff(attempt: u32) -> Duration {
    let secs = RECONNECT_BACKOFF_START_SECS
        .checked_shl(attempt)
        .unwrap_or(RECONNECT_BACKOFF_MAX_SECS)
        .min(RECONNECT_BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

/// What is needed to open the relay connection again after it was lost.
struct RelayConnectParams {
    project_id: String,
    relay_address: String,
    /// The one relay client signing key of this node's WalletConnect context (secret; never logged or returned).
    signing_key: SigningKey,
    /// Whether `signing_key` was loaded from the storage or created at start.
    key_source: client_key::ClientKeySource,
}

impl RelayConnectParams {
    /// Connection options with a freshly signed auth token (the token of an earlier connection may have expired).
    /// `attempt` is the number of this connect attempt (1 for the first connect), used only for the debug log.
    fn options(&self, attempt: u32) -> Result<ConnectionOptions, WalletConnectError> {
        let auth = relay_auth_token(&self.relay_address, &self.signing_key)?;
        let opts = ConnectionOptions::new(self.project_id.clone(), auth).with_address(self.relay_address.as_str());
        let has_token =
            matches!(&opts.auth, relay_client::Authorization::Query(token) if !token.to_string().is_empty());
        common::log::debug!(
            "walletconnect: relay connect attempt={attempt} host={} project_id_present={} auth_token_present={} \
             auth_token_fresh=true client_key={} client_key_source={}",
            relay_host(&opts.address),
            !self.project_id.is_empty(),
            has_token,
            client_key::fingerprint(&self.signing_key),
            self.key_source.as_str()
        );
        Ok(opts)
    }
}

/// The host of a relay address, without scheme, port, path or query string.
fn relay_host(address: &str) -> &str {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    authority.split(':').next().unwrap_or("")
}

/// Configuration the subsystem pulls from `MmCtx::conf`.
///
/// Expected JSON shape:
/// ```jsonc
/// {
///   "walletconnect": { "project_id": "<id>", "relay_address": "wss://...", "app_name": "<name>" },
///   "wc_session_persistence": "open"
/// }
/// ```
/// `project_id` is required (issued by WalletConnect Cloud); `relay_address`
/// is optional and defaults to [`metadata::RELAY_ADDRESS`]; `app_name` is
/// optional: when present and not empty it is the application name wallets
/// list for this node, in place of `KDF Reloaded <client key fingerprint>`; the top-level
/// `wc_session_persistence` toggle is optional and defaults to
/// [`WcSessionPersistence::Open`].
struct WalletConnectConfig {
    project_id: String,
    relay_address: String,
    /// The operator's own application name (`walletconnect.app_name`), when set and not empty.
    app_name: Option<String>,
    persistence: WcSessionPersistence,
}

impl WalletConnectConfig {
    fn from_ctx(ctx: &MmArc) -> Result<Self, WalletConnectError> {
        let section = &ctx.conf["walletconnect"];
        let project_id = section["project_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| WalletConnectError::Config("`walletconnect.project_id` is required".to_string()))?
            .to_string();
        let relay_address = section["relay_address"]
            .as_str()
            .filter(|addr| !addr.is_empty())
            .unwrap_or(metadata::RELAY_ADDRESS)
            .to_string();
        let app_name = section["app_name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let persistence = WcSessionPersistence::from_conf(&ctx.conf)?;
        Ok(WalletConnectConfig {
            project_id,
            relay_address,
            app_name,
            persistence,
        })
    }
}

/// Whether and how WalletConnect sessions are *written* to durable storage.
///
/// Read from the top-level `wc_session_persistence` string in `MM2.json`. This
/// setting governs **saving only**; loading is unconditional (chapter 22
/// \u00a722.5.1 / R7 / R8). The default is [`WcSessionPersistence::Open`], the
/// GLEEC-compatible plaintext format \u2014 a documented
/// Security-versus-compatibility departure (CODING_STANDARDS \u00a75.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WcSessionPersistence {
    /// Write the GLEEC-compatible plaintext record (chapter 22 \u00a722.5.3),
    /// including the session symmetric key, in plaintext at rest. *(default)*
    #[default]
    Open,
    /// Never write, update, or insert session rows. Existing rows are still
    /// read and used at startup; they are simply never rewritten.
    None,
}

impl WcSessionPersistence {
    /// The `MM2.json` key this setting is read from.
    const SETTING_KEY: &'static str = "wc_session_persistence";

    /// Parses the setting from the node configuration.
    ///
    /// An absent (or `null`) value defaults to [`WcSessionPersistence::Open`].
    /// The reserved `encrypted` value, any non-string value, and any
    /// unrecognized string each yield an explanatory error that names the
    /// setting, the offending value, and the allowed values. The node stops
    /// its start-up on that error through [`validate_session_persistence`].
    ///
    /// # Errors
    /// Returns [`WalletConnectError::Config`] for an invalid or not-yet-supported
    /// value.
    fn from_conf(conf: &serde_json::Value) -> Result<Self, WalletConnectError> {
        match conf.get(Self::SETTING_KEY) {
            None | Some(serde_json::Value::Null) => Ok(WcSessionPersistence::Open),
            Some(serde_json::Value::String(value)) => match value.as_str() {
                "open" => Ok(WcSessionPersistence::Open),
                "none" => Ok(WcSessionPersistence::None),
                // TODO(ch22-D9): once the `encrypted` format ships and a
                // stronger-security value can be offered, selecting the
                // less-secure `open` value should emit a prominent runtime
                // warning and may be acknowledgement-gated. Nothing better is
                // offered yet, so no warning is emitted today.
                "encrypted" => Err(WalletConnectError::Config(format!(
                    "`{key}` value `encrypted` is reserved for a future encrypted-at-rest format \
                     and is not yet implemented; allowed values are `open`, `none`, `encrypted`",
                    key = Self::SETTING_KEY,
                ))),
                other => Err(WalletConnectError::Config(format!(
                    "`{key}` has unrecognised value `{other}`; allowed values are `open`, `none`, `encrypted`",
                    key = Self::SETTING_KEY,
                ))),
            },
            Some(_) => Err(WalletConnectError::Config(format!(
                "`{key}` must be a string; allowed values are `open`, `none`, `encrypted`",
                key = Self::SETTING_KEY,
            ))),
        }
    }

    /// Whether session rows may be written under this setting.
    fn should_write(self) -> bool { matches!(self, WcSessionPersistence::Open) }
}

/// Checks the `wc_session_persistence` setting of the node configuration without using WalletConnect.
///
/// The node calls this at start-up so that an invalid value (the reserved `encrypted`, an unrecognized string
/// or a non-string value) stops the launch, instead of failing the first WalletConnect RPC. It applies the same
/// parsing as the first use of the subsystem; an absent or `null` value is valid.
///
/// # Errors
/// Returns [`WalletConnectError::Config`], naming the setting, for an invalid or not-yet-supported value.
pub fn validate_session_persistence(conf: &serde_json::Value) -> Result<(), WalletConnectError> {
    WcSessionPersistence::from_conf(conf).map(|_| ())
}

/// Writes `session` to `storage` only when `persistence` permits it.
///
/// Centralises the save-gating decision (chapter 22 R7): under
/// [`WcSessionPersistence::Open`] the GLEEC-compatible plaintext record is
/// written; under [`WcSessionPersistence::None`] the call is a no-op. Loading
/// is unaffected.
///
/// # Errors
/// Returns [`WalletConnectError`] if serialization or the storage write fails.
async fn save_session_if_enabled(
    persistence: WcSessionPersistence,
    storage: &dyn storage::WcStorageOps,
    session: &Session,
) -> Result<(), WalletConnectError> {
    if persistence.should_write() {
        storage.save_session(session.to_stored()?).await?;
    }
    Ok(())
}

/// The subsystem's per-context handle.
///
/// Holds the live relay client, the message-id generator, the pending-request
/// registry, the live session index, the in-flight pairing material and the
/// session-persistence backend.
pub struct WalletConnectCtx {
    client: Client,
    message_ids: MessageIdGenerator,
    pending: PendingRequests,
    sessions: SessionManager,
    pairings: Mutex<HashMap<Topic, Pairing>>,
    /// Proposals awaiting the responder's public key, keyed by pairing topic.
    /// Each retains our ephemeral x25519 secret until the `wc_sessionPropose`
    /// response arrives and the session key can be derived (chapter 22 §22.6).
    proposals: Mutex<HashMap<Topic, PendingProposal>>,
    /// Establishments awaiting `wc_sessionSettle`, keyed by the derived session
    /// topic. Each holds the derived session key so settle-topic traffic
    /// decrypts before the settled [`Session`] is built.
    establishing: Mutex<HashMap<Topic, PendingSettle>>,
    storage: Arc<dyn storage::WcStorageOps>,
    persistence: WcSessionPersistence,
    /// Used to reopen the relay connection after the relay closed it.
    connect_params: RelayConnectParams,
    /// The app metadata advertised to wallets; built once the relay client key is known.
    app_metadata: Metadata,
}

/// In-progress proposal state retained between publishing a `wc_sessionPropose`
/// and receiving the responder's reply (chapter 22 §22.4 / §22.6). Keyed by the
/// pairing topic the proposal was published on.
struct PendingProposal {
    /// Our ephemeral x25519 secret, retained until the responder public key
    /// arrives so the session key can be derived against it.
    secret: StaticSecret,
    /// The pairing topic this proposal was published on.
    pairing_topic: Topic,
    /// The namespace requirements advertised in the proposal, threaded into the
    /// eventual settled [`Session`].
    propose_namespaces: ProposeNamespaces,
}

/// In-progress establishment state retained between deriving the session key
/// and receiving `wc_sessionSettle` (chapter 22 §22.4). Keyed by the derived
/// session topic.
struct PendingSettle {
    /// The derived session symmetric key (and our advertised public key).
    session_key: SessionKey,
    /// The originating pairing topic.
    pairing_topic: Topic,
    /// The namespace requirements carried forward from the proposal.
    propose_namespaces: ProposeNamespaces,
}

impl WalletConnectCtx {
    /// Builds the handle, connects to the relay and starts the inbound loop.
    ///
    /// Reads `walletconnect.project_id` / `walletconnect.relay_address` from
    /// the context configuration, opens the persistence backend, restores any
    /// non-expired sessions (re-subscribing to their topics) and spawns the
    /// background task that decrypts and routes inbound relay traffic.
    pub async fn init(ctx: &MmArc) -> Result<Arc<Self>, WalletConnectError> {
        let config = WalletConnectConfig::from_ctx(ctx)?;
        let storage = open_storage(ctx).await?;
        storage.init().await?;

        let (signing_key, key_source) = client_key::load_or_create(storage.as_ref()).await?;
        let app_metadata =
            metadata::generate_metadata(config.app_name.as_deref(), &client_key::fingerprint(&signing_key));
        let connect_params = RelayConnectParams {
            project_id: config.project_id,
            relay_address: config.relay_address,
            signing_key,
            key_source,
        };
        let opts = connect_params.options(1)?;

        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(WcConnectionHandler::new(inbound_tx));
        client.connect(&opts).await?;

        let handle = Arc::new(WalletConnectCtx {
            client,
            message_ids: MessageIdGenerator::new(),
            pending: PendingRequests::new(),
            sessions: SessionManager::new(),
            pairings: Mutex::new(HashMap::new()),
            proposals: Mutex::new(HashMap::new()),
            establishing: Mutex::new(HashMap::new()),
            storage,
            persistence: config.persistence,
            connect_params,
            app_metadata,
        });

        handle.restore_sessions().await?;

        common::executor::spawn(run_inbound_loop(Arc::downgrade(&handle), inbound_rx));

        Ok(handle)
    }

    /// The per-context handle: the one cached on the context when there is one, else a freshly initialised one
    /// that is cached for later callers (chapter 22 §22.2). A coin activation that needs a wallet session uses this,
    /// so that it does not depend on a `wc_*` request having initialised the subsystem first.
    ///
    /// # Errors
    ///
    /// The failures of [`WalletConnectCtx::init`], and [`WalletConnectError::Internal`] when the value cached on the
    /// context is not a [`WalletConnectCtx`].
    pub async fn get_or_init(ctx: &MmArc) -> Result<Arc<Self>, WalletConnectError> {
        fn downcast(any: Arc<dyn std::any::Any + Send + Sync>) -> Result<Arc<WalletConnectCtx>, WalletConnectError> {
            any.downcast::<WalletConnectCtx>()
                .map_err(|_| WalletConnectError::Internal("unexpected WalletConnect context type".to_string()))
        }

        if let Some(existing) = ctx.wallet_connect.lock().unwrap().clone() {
            return downcast(existing);
        }
        let handle = Self::init(ctx).await?;
        let mut guard = ctx.wallet_connect.lock().unwrap();
        // Another caller may have initialised the handle while this one was connecting: keep the stored one.
        if let Some(existing) = guard.clone() {
            return downcast(existing);
        }
        let any: Arc<dyn std::any::Any + Send + Sync> = handle.clone();
        *guard = Some(any);
        Ok(handle)
    }

    /// The live session index.
    pub fn sessions(&self) -> &SessionManager { &self.sessions }

    /// The pending-request registry.
    pub fn pending(&self) -> &PendingRequests { &self.pending }

    /// Allocates the next outbound JSON-RPC message id.
    pub fn next_message_id(&self) -> MessageId { self.message_ids.next() }

    /// Generates a new pairing, retains its symmetric material and returns its
    /// topic and the `wc:` URI to show to the user.
    pub fn new_pairing(&self) -> (Topic, String) {
        let pairing = Pairing::generate(PAIRING_TTL_SECS);
        let topic = pairing.topic.clone();
        let uri = pairing.uri();
        self.pairings.lock().insert(topic.clone(), pairing);
        (topic, uri)
    }

    /// Initiates a new connection (chapter 22 §22.9A.2 RP5): generates a fresh
    /// pairing carrying the caller-supplied namespace requirements, publishes a
    /// `wc_sessionPropose` on the pairing topic, and returns the pairing topic
    /// together with the `wc:` URI to present to a wallet.
    ///
    /// The proposal advertises a freshly generated ephemeral x25519 public key;
    /// the matching secret is retained in [`Self::proposals`] until the
    /// responder's reply arrives (chapter 22 §22.4 / §22.6). The returned `wc:`
    /// URI is delivered verbatim regardless of the proposal publish (AC2).
    ///
    /// # Errors
    /// Returns [`WalletConnectError`] if subscribing to or publishing on the
    /// pairing topic fails, or if the proposal payload cannot be encoded.
    pub async fn new_connection(
        &self,
        required_namespaces: serde_json::Value,
        optional_namespaces: Option<serde_json::Value>,
    ) -> Result<(Topic, String), WalletConnectError> {
        let mut pairing = Pairing::generate(PAIRING_TTL_SECS);
        pairing.required_namespaces = required_namespaces;
        pairing.optional_namespaces = optional_namespaces;
        let topic = pairing.topic.clone();
        let uri = pairing.uri();
        let sym_key = pairing.sym_key;

        // Generate our ephemeral keypair and build the proposal payload.
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let our_public = PublicKey::from(&secret);
        let request = build_propose_request(
            &our_public,
            &pairing.required_namespaces,
            pairing.optional_namespaces.as_ref(),
            &self.app_metadata,
        );
        let propose_namespaces = propose_namespaces_from_value(&pairing.required_namespaces);

        // Retain the pairing so inbound pairing-topic traffic can be decrypted.
        self.pairings.lock().insert(topic.clone(), pairing);

        // Subscribe before publishing so the responder's reply is delivered.
        let subscribed = self.client.subscribe(topic.clone()).await;
        log_subscribe("subscribe to a pairing topic", &topic, &subscribed);
        subscribed.map_err(WalletConnectError::from)?;

        let id = self.next_message_id();
        let payload = serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "method": propose::METHOD,
            "params": request,
        });
        let encoded = self.encode_payload(&sym_key, &payload)?;
        self.publish_frame(&topic, encoded, propose::METHOD, id, propose::TAG.request, true)
            .await?;

        // Retain our ephemeral secret + requirements until the response arrives.
        self.proposals.lock().insert(topic.clone(), PendingProposal {
            secret,
            pairing_topic: topic.clone(),
            propose_namespaces,
        });

        Ok((topic, uri))
    }

    /// Encrypts and encodes a JSON-RPC payload into a WalletConnect Type 0
    /// envelope using the session symmetric key.
    pub fn encode_payload(&self, sym_key: &SymKey, payload: &serde_json::Value) -> Result<String, WalletConnectError> {
        let bytes = serde_json::to_vec(payload)?;
        wc_common::encrypt_and_encode(EnvelopeType::Type0, bytes, sym_key)
            .map_err(|e| WalletConnectError::Codec(e.to_string()))
    }

    /// Decodes and decrypts a Type 0 envelope back into a JSON-RPC payload.
    pub fn decode_payload(&self, sym_key: &SymKey, message: &str) -> Result<serde_json::Value, WalletConnectError> {
        let json = wc_common::decode_and_decrypt_type0(message.as_bytes(), sym_key)
            .map_err(|e| WalletConnectError::Codec(e.to_string()))?;
        serde_json::from_str(&json).map_err(WalletConnectError::from)
    }

    /// Applies the negotiated transport encoding to already-enveloped bytes.
    pub fn apply_transport_encoding(&self, algo: EncodingAlgo, envelope: &[u8]) -> String { algo.encode(envelope) }

    /// Sends a `wc_sessionRequest` over the relay and awaits the wallet's
    /// response.
    ///
    /// The supplied `payload` becomes the request's `params`. The JSON-RPC
    /// envelope is encrypted with `sym_key`, published on `topic`, and its id
    /// is registered so the inbound loop can wake this caller when the matching
    /// response arrives. A relay failure, a closed channel or an elapsed
    /// [`REQUEST_RESPONSE_TTL`] each map to a distinct error.
    pub async fn send_session_request(
        &self,
        topic: &Topic,
        sym_key: &SymKey,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, WalletConnectError> {
        let id = self.next_message_id();
        let request = serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "method": SESSION_REQUEST_METHOD,
            "params": payload,
        });
        let encoded = self.encode_payload(sym_key, &request)?;

        let response_rx = self.pending.register(id);

        self.publish_frame(topic, encoded, SESSION_REQUEST_METHOD, id, SESSION_REQUEST_TAG, true)
            .await
            .inspect_err(|_| {
                self.pending.cancel(id);
            })?;

        let deadline = common::executor::Timer::sleep(REQUEST_RESPONSE_TTL.as_secs_f64());
        match futures::future::select(response_rx, deadline).await {
            futures::future::Either::Left((Ok(value), _)) => Ok(value),
            futures::future::Either::Left((Err(_), _)) => {
                Err(WalletConnectError::Internal("response channel closed".to_string()))
            },
            futures::future::Either::Right(((), _)) => {
                self.pending.cancel(id);
                Err(WalletConnectError::Timeout)
            },
        }
    }

    /// Issues a `wc_sessionPing` over the relay and awaits the wallet's reply
    /// (chapter 22 §22.9A.2 RP5). Mirrors [`send_session_request`] but carries
    /// no signing payload: a successful reply maps to `Ok(())`, and a relay
    /// failure, a closed channel or an elapsed [`REQUEST_RESPONSE_TTL`] each map
    /// to a distinct error.
    pub async fn ping_session(&self, topic: &Topic) -> Result<(), WalletConnectError> {
        let (sym_key, _encoding) = self
            .sessions
            .transport_for(topic)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        let id = self.next_message_id();
        let request = serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "method": session::rpc::ping::METHOD,
            "params": serde_json::json!({}),
        });
        let encoded = self.encode_payload(&sym_key, &request)?;

        let response_rx = self.pending.register(id);

        self.publish_frame(
            topic,
            encoded,
            session::rpc::ping::METHOD,
            id,
            session::rpc::ping::TAG.request,
            true,
        )
        .await
        .inspect_err(|_| {
            self.pending.cancel(id);
        })?;

        let deadline = common::executor::Timer::sleep(REQUEST_RESPONSE_TTL.as_secs_f64());
        match futures::future::select(response_rx, deadline).await {
            futures::future::Either::Left((Ok(_), _)) => Ok(()),
            futures::future::Either::Left((Err(_), _)) => {
                Err(WalletConnectError::Internal("response channel closed".to_string()))
            },
            futures::future::Either::Right(((), _)) => {
                self.pending.cancel(id);
                Err(WalletConnectError::Timeout)
            },
        }
    }

    /// Drops a session: best-effort sends the `wc_sessionDelete` RPC, then
    /// unsubscribes from the topic and removes the persisted row. The call is
    /// idempotent — if the session is already gone the delete RPC is skipped
    /// and the storage/subscription cleanup still runs.
    pub async fn drop_session(&self, topic: &Topic) -> Result<(), WalletConnectError> {
        if let Some((sym_key, _encoding)) = self.sessions.transport_for(topic) {
            let id = self.next_message_id();
            let request = serde_json::json!({
                "id": id,
                "jsonrpc": "2.0",
                "method": session::rpc::delete::METHOD,
                "params": session::rpc::delete::DeleteRequest::default(),
            });
            if let Ok(encoded) = self.encode_payload(&sym_key, &request) {
                let _ = self
                    .publish_frame(
                        topic,
                        encoded,
                        session::rpc::delete::METHOD,
                        id,
                        session::rpc::delete::TAG.request,
                        false,
                    )
                    .await;
            }
        }

        self.forget_session(topic).await
    }

    /// Every topic the node must be subscribed to: settled sessions, unexpired pairings and in-flight establishments.
    fn topics_to_subscribe(&self) -> Vec<Topic> {
        let establishing: Vec<Topic> = self.establishing.lock().keys().cloned().collect();
        collect_resubscribe_topics(self.sessions.topics(), &self.pairings.lock(), establishing, unix_now())
    }

    /// Subscribes to every topic of [`Self::topics_to_subscribe`] again. The relay forgets subscriptions together
    /// with the connection that held them. A failure of one topic is logged and does not stop the others.
    async fn resubscribe_all(&self) {
        for topic in self.topics_to_subscribe() {
            let result = self.client.subscribe(topic.clone()).await;
            log_subscribe("re-subscribe after reconnect", &topic, &result);
        }
    }

    /// Publishes an already encrypted message on the relay and logs the frame at debug level: the JSON-RPC method,
    /// the first 8 characters of the topic, the relay tag and the request id. The payload, keys and the full topic
    /// are never logged.
    async fn publish_frame(
        &self,
        topic: &Topic,
        encoded: String,
        method: &str,
        request_id: impl std::fmt::Display,
        tag: u32,
        prompt: bool,
    ) -> Result<(), WalletConnectError> {
        log_relay_frame("publish", topic, tag, method, &request_id);
        let result = self
            .client
            .publish(
                topic.clone(),
                encoded,
                no_attestation(),
                tag,
                REQUEST_RESPONSE_TTL,
                prompt,
            )
            .await;
        match &result {
            Ok(()) => common::log::debug!(
                "walletconnect: relay publish accepted method={method} topic={}.. id={request_id}",
                topic_prefix(topic)
            ),
            Err(e) => common::log::warn!(
                "walletconnect: relay publish failed method={method} topic={}.. tag={tag} id={request_id}: {}",
                topic_prefix(topic),
                error::relay_call_error_kind(e)
            ),
        }
        result.map_err(WalletConnectError::from)
    }

    /// Loads persisted sessions on connect (chapter 22 \u00a722.5.2). Loading is
    /// **unconditional** \u2014 every stored row is read regardless of the
    /// `wc_session_persistence` setting. Expired rows are deleted; for live
    /// rows the in-memory session is fully reconstructed (including the
    /// symmetric key, so the session can decrypt subsequent traffic) and the
    /// topic is re-subscribed so messages buffered while offline are delivered.
    async fn restore_sessions(&self) -> Result<(), WalletConnectError> {
        let now = unix_now();
        for row in self.storage.get_all_sessions().await? {
            if (row.expiry as u64) <= now {
                self.storage.delete_session(&row.topic).await?;
                continue;
            }
            let session = match Session::from_stored(&row) {
                Ok(session) => session,
                Err(e) => {
                    common::log::error!("walletconnect: failed to decode persisted session: {e}");
                    continue;
                },
            };
            let topic = session.topic.clone();
            self.sessions.insert(session);
            // A failed subscription does not stop the start-up: the loop that reopens a lost relay connection
            // subscribes to every session topic again.
            let result = self.client.subscribe(topic.clone()).await;
            log_subscribe("re-subscribe of a restored session", &topic, &result);
        }
        Ok(())
    }

    /// Writes a session record to storage subject to `wc_session_persistence`
    /// (chapter 22 \u00a722.5.2 step 5): under `open` the GLEEC-compatible plaintext
    /// record is written; under `none` the call is a no-op. Loading is never
    /// affected.
    ///
    /// # Errors
    /// Returns [`WalletConnectError`] if serialization or the storage write
    /// fails.
    pub async fn persist_session(&self, session: &Session) -> Result<(), WalletConnectError> {
        save_session_if_enabled(self.persistence, self.storage.as_ref(), session).await
    }

    /// Removes a session from the relay subscription, the persistence backend
    /// and the in-memory index. Best-effort on the transport side so it stays
    /// idempotent.
    async fn forget_session(&self, topic: &Topic) -> Result<(), WalletConnectError> {
        let _ = self.client.unsubscribe(topic.clone()).await;
        self.storage.delete_session(topic.as_ref()).await?;
        self.sessions.remove(topic);
        self.pairings.lock().remove(topic);
        Ok(())
    }

    /// Decrypts a single inbound relay message and routes it by JSON-RPC shape.
    async fn dispatch_inbound(&self, message: PublishedMessage) {
        let topic = message.topic.clone();
        let Some((sym_key, kind)) = self.inbound_transport(&topic) else {
            // Traffic on a topic with no settled session, pairing or in-flight
            // establishment; nothing to route.
            log_relay_frame("receive (unknown topic)", &topic, message.tag, "-", &"-");
            common::log::debug!("walletconnect: dropping inbound message on unknown topic");
            return;
        };

        let payload = match self.decode_payload(&sym_key, &message.message) {
            Ok(payload) => payload,
            Err(e) => {
                log_relay_frame("receive (undecryptable)", &topic, message.tag, "-", &"-");
                common::log::error!("walletconnect: failed to decrypt inbound message: {e}");
                return;
            },
        };

        let id = payload.get("id").and_then(serde_json::Value::as_u64);
        log_relay_frame(
            "receive",
            &topic,
            message.tag,
            payload
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("response"),
            &id.map_or_else(|| "-".to_string(), |id| id.to_string()),
        );

        // A response carries `result`/`error` and no `method`; correlate it. On
        // a pairing topic the only response we expect is the `wc_sessionPropose`
        // reply carrying the responder public key (chapter 22 §22.4).
        if payload.get("method").is_none() {
            match kind {
                TopicKind::Pairing => self.handle_propose_response(&topic, &payload).await,
                TopicKind::Session | TopicKind::Establishing => {
                    if let Some(id) = id {
                        let result = payload.get("result").cloned().unwrap_or_else(|| payload.clone());
                        self.pending.resolve(MessageId::new(id), result);
                    }
                },
            }
            return;
        }

        let method = payload
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match method {
            "wc_sessionPing" => {
                if let Some(id) = id {
                    self.reply_success(&topic, &sym_key, id, session::rpc::ping::TAG.response)
                        .await;
                }
            },
            "wc_sessionDelete" => {
                let _ = self.forget_session(&topic).await;
            },
            "wc_sessionExtend" => {
                if let Some(expiry) = payload
                    .get("params")
                    .and_then(|p| p.get("expiry"))
                    .and_then(serde_json::Value::as_u64)
                {
                    self.sessions.set_expiry(&topic, expiry);
                    self.persist_expiry(topic.as_ref(), expiry).await;
                }
                if let Some(id) = id {
                    self.reply_success(&topic, &sym_key, id, session::rpc::extend::TAG.response)
                        .await;
                }
            },
            "wc_sessionUpdate" => {
                common::log::debug!("walletconnect: received session update");
                if let Some(id) = id {
                    self.reply_success(&topic, &sym_key, id, session::rpc::update::TAG.response)
                        .await;
                }
            },
            "wc_sessionEvent" => {
                common::log::debug!("walletconnect: received session event");
            },
            "wc_sessionSettle" => {
                self.handle_settle(&topic, &sym_key, id, &payload).await;
            },
            "wc_sessionPropose" => {
                // As a dApp we never receive a propose *request*; the propose
                // *response* (no `method`) is handled above.
                common::log::debug!("walletconnect: ignoring inbound session propose request");
            },
            other => {
                common::log::debug!("walletconnect: ignoring unhandled inbound method `{other}`");
            },
        }
    }

    /// Publishes a `{ "result": true }` JSON-RPC response for an inbound
    /// request that expects acknowledgement.
    async fn reply_success(&self, topic: &Topic, sym_key: &SymKey, id: u64, tag: u32) {
        let response = serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "result": true,
        });
        if let Ok(encoded) = self.encode_payload(sym_key, &response) {
            let _ = self.publish_frame(topic, encoded, "response", id, tag, false).await;
        }
    }

    /// Updates the persisted expiry of a session if a row already exists.
    /// Skipped entirely under [`WcSessionPersistence::None`] (save-gating).
    async fn persist_expiry(&self, topic: &str, expiry: u64) {
        if !self.persistence.should_write() {
            return;
        }
        if let Ok(Some(mut row)) = self.storage.get_session(topic).await {
            row.expiry = expiry as i64;
            let _ = self.storage.save_session(row).await;
        }
    }

    /// Resolves the symmetric key for an inbound topic and how to interpret its
    /// traffic: a settled session, a pairing (propose-response stage), or an
    /// in-flight establishment (settle stage). `None` when the topic is unknown.
    fn inbound_transport(&self, topic: &Topic) -> Option<(SymKey, TopicKind)> {
        if let Some((sym_key, _encoding)) = self.sessions.transport_for(topic) {
            return Some((sym_key, TopicKind::Session));
        }
        if let Some(sym_key) = self.pairings.lock().get(topic).map(|pairing| pairing.sym_key) {
            return Some((sym_key, TopicKind::Pairing));
        }
        if let Some(sym_key) = self
            .establishing
            .lock()
            .get(topic)
            .map(|pending| pending.session_key.symmetric_key())
        {
            return Some((sym_key, TopicKind::Establishing));
        }
        None
    }

    /// Handles a `wc_sessionPropose` response on a pairing topic (chapter 22
    /// §22.4 / §22.6): derives the session key from the responder public key,
    /// subscribes to the resulting session topic and records the establishment
    /// state so the subsequent `wc_sessionSettle` decrypts.
    async fn handle_propose_response(&self, pairing_topic: &Topic, payload: &serde_json::Value) {
        let responder_hex = payload
            .get("result")
            .and_then(|result| result.get("responderPublicKey"))
            .and_then(serde_json::Value::as_str);
        let Some(responder_hex) = responder_hex else {
            common::log::error!("walletconnect: propose response missing responderPublicKey");
            return;
        };

        let Some(proposal) = self.proposals.lock().remove(pairing_topic) else {
            common::log::debug!("walletconnect: propose response for unknown pairing");
            return;
        };

        let peer_public = match decode_peer_public(responder_hex) {
            Ok(peer_public) => peer_public,
            Err(e) => {
                common::log::error!("walletconnect: invalid responder public key: {e}");
                return;
            },
        };

        let mut session_key = SessionKey::new(PublicKey::from(&proposal.secret));
        if let Err(e) = session_key.generate_symmetric_key(&proposal.secret, &peer_public) {
            common::log::error!("walletconnect: session key derivation failed: {e}");
            return;
        }
        let session_topic = Topic::from(session_key.generate_topic());

        let subscribed = self.client.subscribe(session_topic.clone()).await;
        log_subscribe("subscribe to a session topic", &session_topic, &subscribed);
        if subscribed.is_err() {
            return;
        }

        self.establishing.lock().insert(session_topic, PendingSettle {
            session_key,
            pairing_topic: proposal.pairing_topic,
            propose_namespaces: proposal.propose_namespaces,
        });
    }

    /// Handles an inbound `wc_sessionSettle` request (chapter 22 §22.4): builds
    /// the settled [`Session`] from the establishment state and the settle
    /// payload, registers and persists it, acknowledges the settle, and clears
    /// the in-flight establishment record.
    async fn handle_settle(
        &self,
        session_topic: &Topic,
        sym_key: &SymKey,
        id: Option<u64>,
        payload: &serde_json::Value,
    ) {
        let Some(pending) = self.establishing.lock().remove(session_topic) else {
            // No establishment state (e.g. a re-settle on a live session); ack
            // politely but do not rebuild.
            common::log::debug!("walletconnect: settle for topic without establishment state");
            if let Some(id) = id {
                self.reply_success(session_topic, sym_key, id, settle::TAG.response)
                    .await;
            }
            return;
        };

        let settle: settle::SettleRequest = match payload.get("params").cloned() {
            Some(params) => match serde_json::from_value(params) {
                Ok(settle) => settle,
                Err(e) => {
                    common::log::error!("walletconnect: failed to parse session settle: {e}");
                    return;
                },
            },
            None => {
                common::log::error!("walletconnect: session settle missing params");
                return;
            },
        };

        let session = build_session(session_topic.clone(), pending, settle, &self.app_metadata);
        if let Err(e) = self.persist_session(&session).await {
            common::log::error!("walletconnect: failed to persist settled session: {e}");
        }
        self.sessions.insert(session);

        if let Some(id) = id {
            self.reply_success(session_topic, sym_key, id, settle::TAG.response)
                .await;
        }
    }
}

/// How an inbound topic's traffic should be interpreted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TopicKind {
    /// A settled session.
    Session,
    /// A pairing awaiting the `wc_sessionPropose` response.
    Pairing,
    /// An in-flight establishment awaiting `wc_sessionSettle`.
    Establishing,
}

/// Decodes a hex-encoded 32-byte x25519 public key from a propose response.
fn decode_peer_public(hex_str: &str) -> Result<SymKey, WalletConnectError> {
    let bytes = hex::decode(hex_str).map_err(|e| WalletConnectError::Internal(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| WalletConnectError::Internal("responder public key must be 32 bytes".to_string()))
}

/// Builds a `wc_sessionPropose` request advertising our ephemeral public key and
/// the caller's namespace requirements (chapter 22 §22.4).
fn build_propose_request(
    our_public: &PublicKey,
    required_namespaces: &serde_json::Value,
    optional_namespaces: Option<&serde_json::Value>,
    app_metadata: &Metadata,
) -> propose::ProposeRequest {
    let required = serde_json::from_value(required_namespaces.clone()).unwrap_or_default();
    let optional = optional_namespaces.and_then(|value| serde_json::from_value(value.clone()).ok());
    propose::ProposeRequest {
        relays: vec![Relay {
            protocol: metadata::SUPPORTED_RELAY_PROTOCOL.to_string(),
            data: None,
        }],
        proposer: propose::Proposer {
            public_key: hex::encode(our_public.to_bytes()),
            metadata: app_metadata.clone(),
        },
        required_namespaces: required,
        optional_namespaces: optional,
    }
}

/// Decodes the proposal namespace requirements into the relay-SDK type retained
/// for the eventual settled session. Malformed/absent requirements decode to an
/// empty set.
fn propose_namespaces_from_value(value: &serde_json::Value) -> ProposeNamespaces {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Builds the settled [`Session`] from the in-flight establishment state and the
/// parsed `wc_sessionSettle` payload (chapter 22 §22.8.1.5).
fn build_session(
    session_topic: Topic,
    pending: PendingSettle,
    settle: settle::SettleRequest,
    app_metadata: &Metadata,
) -> Session {
    let encoding = session_encoding_for_wallet_name(&settle.controller.metadata.name);
    Session {
        topic: session_topic,
        pairing_topic: pending.pairing_topic,
        session_key: pending.session_key,
        controller: settle::SettleRequest::PEER_ROLE,
        metadata: settle.controller.metadata,
        expiry: settle.expiry,
        encoding,
        properties: settle.session_properties,
        subscription_id: None,
        proposer: app_metadata.clone(),
        relay: settle.relay,
        namespaces: settle_to_relay_namespaces(settle.namespaces),
        propose_namespaces: pending.propose_namespaces,
        active_chain_id: None,
    }
}

/// Selects the session-level byte-string encoder from the settled wallet
/// metadata. This is intentionally not keyed on CAIP namespace or request
/// method; Cosmos field-level byte encoding is handled by the coin module.
fn session_encoding_for_wallet_name(wallet_name: &str) -> EncodingAlgo {
    match wallet_name {
        "Keplr" => EncodingAlgo::Base64,
        _ => EncodingAlgo::Hex,
    }
}

/// Converts the locally-parsed settle namespaces into the relay-SDK
/// [`SettleNamespaces`] held on a [`Session`].
fn settle_to_relay_namespaces(namespaces: BTreeMap<String, settle::SettleNamespace>) -> SettleNamespaces {
    SettleNamespaces(
        namespaces
            .into_iter()
            .map(|(name, entry)| {
                (name, Namespace {
                    chains: entry.chains,
                    accounts: Some(entry.accounts),
                    methods: entry.methods,
                    events: entry.events,
                })
            })
            .collect(),
    )
}

/// The `attestation` argument the relay client expects; the dApp role never
/// attaches one.
fn no_attestation() -> Option<std::sync::Arc<str>> { None }

/// Builds the short-lived ed25519 JWT the relay requires for the connection, freshly signed (new issue and expiry
/// time) with the node's stable client key.
fn relay_auth_token(
    relay_address: &str,
    signing_key: &SigningKey,
) -> Result<relay_rpc::auth::SerializedAuthToken, WalletConnectError> {
    AuthToken::new(metadata::APP_URL)
        .aud(relay_address)
        .ttl(Duration::from_secs(RELAY_AUTH_TTL_SECS))
        .as_jwt(signing_key)
        .map_err(|e| WalletConnectError::Config(e.to_string()))
}

/// Opens the platform session-persistence backend bound to `ctx`.
#[cfg(not(target_arch = "wasm32"))]
async fn open_storage(ctx: &MmArc) -> Result<Arc<dyn storage::WcStorageOps>, WalletConnectError> {
    use db_common::async_sql_conn::AsyncConnection;
    let path = ctx.dbdir().join("KOMODEFI-WALLETCONNECT.db");
    let conn = AsyncConnection::open(path)
        .await
        .map_err(|e| WalletConnectError::Storage(e.to_string()))?;
    Ok(Arc::new(storage::sqlite::SqliteSessionStorage::new(conn)))
}

/// Opens the platform session-persistence backend bound to `ctx`.
#[cfg(target_arch = "wasm32")]
async fn open_storage(ctx: &MmArc) -> Result<Arc<dyn storage::WcStorageOps>, WalletConnectError> {
    Ok(Arc::new(storage::indexed_db::IndexedDbSessionStorage::new(ctx)))
}

/// Drains the inbound relay channel, dispatching each message until the handle
/// is dropped or the relay closes the channel.
async fn run_inbound_loop(handle: Weak<WalletConnectCtx>, mut inbound_rx: UnboundedReceiver<RelayEvent>) {
    // Messages that arrived while a reconnect was running; they are routed before the channel is read again.
    let mut backlog: VecDeque<PublishedMessage> = VecDeque::new();
    loop {
        if let Some(message) = backlog.pop_front() {
            match handle.upgrade() {
                Some(ctx) => ctx.dispatch_inbound(message).await,
                None => break,
            }
            continue;
        }
        match inbound_rx.recv().await {
            Some(RelayEvent::Message(message)) => match handle.upgrade() {
                Some(ctx) => ctx.dispatch_inbound(message).await,
                None => break,
            },
            Some(RelayEvent::Lost(reason)) => {
                common::log::warn!("walletconnect: relay connection lost ({reason}), reconnecting");
                if !reconnect_relay(&handle).await {
                    break;
                }
                // Loss events raised by the connection that was just replaced are stale; keep only the messages.
                while let Ok(event) = inbound_rx.try_recv() {
                    if let RelayEvent::Message(message) = event {
                        backlog.push_back(message);
                    }
                }
            },
            None => break,
        }
    }
}

/// Opens the relay connection again, retrying with a growing wait (see [`reconnect_backoff`]) until it succeeds, and
/// subscribes to the topics of the pairings and sessions again. Returns `false` when the handle was dropped meanwhile.
/// The handle is held only for the duration of one attempt, so dropping it stops the retries.
async fn reconnect_relay(handle: &Weak<WalletConnectCtx>) -> bool {
    let mut attempt: u32 = 0;
    loop {
        common::executor::Timer::sleep(reconnect_backoff(attempt).as_secs_f64()).await;
        let Some(ctx) = handle.upgrade() else {
            return false;
        };
        let outcome = match ctx.connect_params.options(attempt + 1) {
            Ok(opts) => ctx.client.connect(&opts).await.map_err(WalletConnectError::from),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => {
                ctx.resubscribe_all().await;
                common::log::info!("walletconnect: relay connection restored (attempt {})", attempt + 1);
                return true;
            },
            Err(e) => {
                common::log::warn!("walletconnect: relay reconnect attempt {} failed: {e}", attempt + 1);
                attempt = attempt.saturating_add(1);
            },
        }
    }
}

/// The topics to subscribe to after a reconnect: the given session topics and in-flight establishment topics, plus
/// every pairing that has not expired at `now`. Each topic appears once.
fn collect_resubscribe_topics(
    session_topics: Vec<Topic>,
    pairings: &HashMap<Topic, Pairing>,
    establishing_topics: Vec<Topic>,
    now: u64,
) -> Vec<Topic> {
    let mut seen: HashSet<Topic> = HashSet::new();
    let mut topics = Vec::new();
    let live_pairings = pairings
        .values()
        .filter(|pairing| pairing.expiry > now)
        .map(|pairing| pairing.topic.clone());
    for topic in session_topics
        .into_iter()
        .chain(establishing_topics)
        .chain(live_pairings)
    {
        if seen.insert(topic.clone()) {
            topics.push(topic);
        }
    }
    topics
}

/// Debug log of one relay frame. Only the direction, the JSON-RPC method, the first 8 characters of the topic, the
/// relay tag and the request id are written; never a payload, a key or a full topic.
fn topic_prefix(topic: &Topic) -> String { topic.as_ref().chars().take(8).collect() }

/// Logs the outcome of a relay subscribe: the topic prefix and "ok" at debug level, or the error kind at warn level.
fn log_subscribe<R, T>(what: &str, topic: &Topic, result: &Result<R, relay_client::error::Error<T>>) {
    match result {
        Ok(_) => common::log::debug!("walletconnect: {what} topic={}.. ok", topic_prefix(topic)),
        Err(e) => common::log::warn!(
            "walletconnect: {what} topic={}.. failed: {}",
            topic_prefix(topic),
            error::relay_call_error_kind(e)
        ),
    }
}

fn log_relay_frame(direction: &str, topic: &Topic, tag: u32, method: &str, request_id: &dyn std::fmt::Display) {
    let topic: &str = topic.as_ref();
    let prefix: String = topic.chars().take(8).collect();
    common::log::debug!("walletconnect: relay {direction} method={method} topic={prefix}.. tag={tag} id={request_id}");
}

/// Current unix timestamp in seconds.
fn unix_now() -> u64 { chrono::Utc::now().timestamp().max(0) as u64 }

#[cfg(all(test, not(target_arch = "wasm32")))]
mod persistence_tests {
    use super::*;
    use common::block_on;
    use db_common::async_sql_conn::AsyncConnection;
    use relay_rpc::domain::Topic;
    use relay_rpc::rpc::params::session::{ProposeNamespaces, SettleNamespaces};
    use relay_rpc::rpc::params::{Metadata, Relay};
    use session::{SessionKey, SessionType};
    use storage::sqlite::SqliteSessionStorage;
    use storage::WcStorageOps;
    use x25519_dalek::{PublicKey, StaticSecret};

    /// The §22.5.3 `data` JSON keys, asserted present in serialized records.
    const RECORD_KEYS: [&str; 14] = [
        "topic",
        "subscription_id",
        "session_key",
        "controller",
        "proposer",
        "relay",
        "namespaces",
        "propose_namespaces",
        "expiry",
        "pairing_topic",
        "session_type",
        "session_properties",
        "active_chain_id",
        "encoding_algo",
    ];

    fn session_key_from(seed: u8) -> SessionKey {
        let secret = StaticSecret::from([seed; 32]);
        let peer = PublicKey::from(&StaticSecret::from([seed ^ 0xFF; 32]));
        let mut sk = SessionKey::new(PublicKey::from(&secret));
        sk.generate_symmetric_key(&secret, &peer.to_bytes()).expect("derive");
        sk
    }

    fn sample_session(topic: &str, seed: u8) -> Session {
        Session {
            topic: Topic::from(topic.to_string()),
            pairing_topic: Topic::from(format!("pairing-{topic}")),
            session_key: session_key_from(seed),
            controller: SessionType::Controller,
            metadata: Metadata::default(),
            // Far-future expiry so restore never treats it as expired.
            expiry: 32_503_680_000,
            encoding: EncodingAlgo::Hex,
            properties: None,
            subscription_id: None,
            proposer: Metadata::default(),
            relay: Relay::default(),
            namespaces: SettleNamespaces::default(),
            propose_namespaces: ProposeNamespaces::default(),
            active_chain_id: None,
        }
    }

    #[test]
    fn wc_session_persistence_parses_every_value_and_default() {
        use serde_json::json;

        assert_eq!(
            WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": "open" })).unwrap(),
            WcSessionPersistence::Open,
        );
        assert_eq!(
            WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": "none" })).unwrap(),
            WcSessionPersistence::None,
        );
        // Absent and explicit null both default to `open`.
        assert_eq!(
            WcSessionPersistence::from_conf(&json!({})).unwrap(),
            WcSessionPersistence::Open,
        );
        assert_eq!(
            WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": null })).unwrap(),
            WcSessionPersistence::Open,
        );

        // `encrypted` is reserved and must stop startup with an explanatory error.
        let encrypted = WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": "encrypted" }))
            .expect_err("encrypted must error");
        let msg = encrypted.to_string();
        assert!(msg.contains("wc_session_persistence"), "names the setting: {msg}");
        assert!(msg.contains("encrypted"), "names the offending value: {msg}");
        assert!(
            msg.contains("open") && msg.contains("none"),
            "lists allowed values: {msg}"
        );

        // An unrecognised string is rejected.
        let invalid = WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": "bogus" }))
            .expect_err("invalid must error");
        assert!(invalid.to_string().contains("bogus"));

        // A non-string value is rejected.
        let non_string = WcSessionPersistence::from_conf(&json!({ "wc_session_persistence": 7 }))
            .expect_err("non-string must error");
        assert!(non_string.to_string().contains("must be a string"));
    }

    #[test]
    fn wc_session_persistence_validation_applies_the_same_rules_as_first_use() {
        use serde_json::json;

        // Valid: absent, null, `open`, `none`.
        for conf in [
            json!({}),
            json!({ "wc_session_persistence": null }),
            json!({ "wc_session_persistence": "open" }),
            json!({ "wc_session_persistence": "none" }),
        ] {
            assert!(validate_session_persistence(&conf).is_ok(), "{conf}");
        }

        // Invalid: the reserved value, an unknown string, a non-string. The error names the setting.
        for conf in [
            json!({ "wc_session_persistence": "encrypted" }),
            json!({ "wc_session_persistence": "bogus" }),
            json!({ "wc_session_persistence": 7 }),
        ] {
            let err = validate_session_persistence(&conf).expect_err("must be refused");
            assert!(matches!(err, WalletConnectError::Config(_)), "{conf}");
            assert!(err.to_string().contains("wc_session_persistence"), "{conf}: {err}");
        }
    }

    #[test]
    fn persisted_record_carries_every_chapter_22_5_3_key_and_round_trips_the_key() {
        let session = sample_session("topicC", 0x33);
        let original_key = session.session_key.symmetric_key();
        assert_ne!(original_key, [0u8; 32], "test key must be non-zero");

        let stored = session.to_stored().expect("serialize");
        assert_eq!(stored.topic, "topicC");
        assert_eq!(stored.expiry, session.expiry as i64);
        for key in RECORD_KEYS {
            assert!(stored.data.contains(&format!("\"{key}\"")), "data missing key `{key}`");
        }
        // The session-key object carries the externally-required sub-keys.
        assert!(stored.data.contains("\"sym_key\""));
        assert!(stored.data.contains("\"public_key\""));

        let restored = Session::from_stored(&stored).expect("deserialize");
        assert_eq!(
            restored.session_key.symmetric_key(),
            original_key,
            "the symmetric key must round-trip exactly",
        );

        // A restored session must be able to DECRYPT, not merely re-subscribe:
        // encrypt with the original key, decrypt with the reconstructed one.
        let plaintext = br#"{"jsonrpc":"2.0","id":1}"#.to_vec();
        let envelope =
            wc_common::encrypt_and_encode(EnvelopeType::Type0, plaintext.clone(), &original_key).expect("encrypt");
        let decrypted = wc_common::decode_and_decrypt_type0(envelope.as_bytes(), &restored.session_key.symmetric_key())
            .expect("decrypt with restored key");
        assert_eq!(decrypted.as_bytes(), plaintext.as_slice());
    }

    #[test]
    fn restored_record_without_encoding_algo_defaults_to_hex() {
        let session = sample_session("topicEncodingDefault", 0x34);
        let mut stored = session.to_stored().expect("serialize");
        let mut data: serde_json::Value = serde_json::from_str(&stored.data).expect("stored JSON");
        data.as_object_mut()
            .expect("stored record is an object")
            .remove("encoding_algo")
            .expect("fixture includes encoding_algo");
        stored.data = serde_json::to_string(&data).expect("serialize edited JSON");

        let restored = Session::from_stored(&stored).expect("deserialize without encoding_algo");
        let bytes = [0x01u8, 0x02, 0x03, 0xff];
        assert_eq!(restored.encoding, EncodingAlgo::Hex);
        assert_eq!(restored.encoding.encode(bytes), "010203ff");
    }

    #[test]
    fn restored_record_rejects_unknown_encoding_algo() {
        let session = sample_session("topicEncodingInvalid", 0x35);
        let mut stored = session.to_stored().expect("serialize");
        let mut data: serde_json::Value = serde_json::from_str(&stored.data).expect("stored JSON");
        data.as_object_mut().expect("stored record is an object").insert(
            "encoding_algo".to_string(),
            serde_json::Value::String("Binary".to_string()),
        );
        stored.data = serde_json::to_string(&data).expect("serialize edited JSON");

        assert!(
            Session::from_stored(&stored).is_err(),
            "unknown encoding_algo values must be invalid"
        );
    }

    #[test]
    fn type0_envelope_codec_does_not_use_session_encoding_algo() {
        let mut session = sample_session("topicType0", 0x36);
        session.encoding = EncodingAlgo::Base64;
        let sym_key = session.session_key.symmetric_key();
        let bytes = [0x01u8, 0x02, 0x03, 0xff];
        assert_eq!(session.encoding.encode(bytes), "AQID/w==");

        let plaintext = br#"{"jsonrpc":"2.0","id":1}"#.to_vec();
        let envelope =
            wc_common::encrypt_and_encode(EnvelopeType::Type0, plaintext.clone(), &sym_key).expect("type0 encrypt");
        let decrypted = wc_common::decode_and_decrypt_type0(envelope.as_bytes(), &sym_key).expect("type0 decrypt");
        assert_eq!(decrypted.as_bytes(), plaintext.as_slice());
    }

    #[test]
    fn save_gating_writes_under_open_and_skips_under_none() {
        block_on(async {
            let conn = AsyncConnection::open_in_memory().await.expect("open in-memory db");
            let storage = SqliteSessionStorage::new(conn);
            storage.init().await.expect("init schema");
            let session = sample_session("topicB", 0x55);

            // `none`: nothing is written.
            save_session_if_enabled(WcSessionPersistence::None, &storage, &session)
                .await
                .expect("none is a no-op");
            assert!(
                storage.get_all_sessions().await.unwrap().is_empty(),
                "`none` must not write any row",
            );

            // `open`: exactly one row is written, keyed by topic.
            save_session_if_enabled(WcSessionPersistence::Open, &storage, &session)
                .await
                .expect("open writes");
            let rows = storage.get_all_sessions().await.unwrap();
            assert_eq!(rows.len(), 1, "`open` must write one row");
            assert_eq!(rows[0].topic, "topicB");

            // The persisted row reconstructs a decrypt-capable session.
            let restored = Session::from_stored(&rows[0]).expect("restore from written row");
            assert_eq!(
                restored.session_key.symmetric_key(),
                session.session_key.symmetric_key(),
            );
        });
    }

    #[test]
    fn session_info_serialises_rp6_field_spellings() {
        use session::{KeyInfo, SessionInfo, SessionProperties};

        let mut session = sample_session("topicRP6", 0x44);
        session.properties = Some(SessionProperties {
            keys: Some(vec![KeyInfo {
                chain_id: "cosmos:cosmoshub-4".to_string(),
                name: "account-0".to_string(),
                algo: "secp256k1".to_string(),
                pub_key: "02abcdef".to_string(),
                address: "ABCDEF".to_string(),
                bech32_address: "cosmos1examplexyz".to_string(),
                ethereum_hex_address: "0x0123".to_string(),
                is_nano_ledger: true,
                is_keystone: false,
            }]),
        });

        let info = SessionInfo::from(&session);
        let value = serde_json::to_value(&info).expect("serialize session-info");
        let object = value.as_object().expect("session-info is a JSON object");

        // §22.8.1.5: the `session-info` record serialises with exactly these
        // five field spellings — no more.
        let mut fields: Vec<&str> = object.keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(fields, ["expiry", "metadata", "namespaces", "pairing_topic", "topic"]);
        assert_eq!(object["topic"], "topicRP6");
        assert_eq!(object["pairing_topic"], "pairing-topicRP6");
        assert!(object["expiry"].is_number(), "expiry must be a number");

        // §22.8.1.6 per-account detail is delivered at session-settle and
        // consumed internally — it is NOT emitted in `session-info`.
        assert!(
            !object.contains_key("session_properties"),
            "session-info must not carry per-account detail (§22.8.1.5)"
        );

        // The §22.8.1.6 `sessionProperties.keys` record itself still serialises
        // with the dictated camelCase spellings (used by the signing slices).
        let props = serde_json::to_value(session.properties.as_ref().unwrap()).expect("serialize session properties");
        let entry = props["keys"][0].as_object().expect("key entry is an object");
        for field in ["chainId", "algo", "pubKey", "address", "isNanoLedger"] {
            assert!(entry.contains_key(field), "key entry missing `{field}`");
        }
        assert_eq!(entry["isNanoLedger"], true);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod establishment_tests {
    use super::*;
    use x25519_dalek::{PublicKey, StaticSecret};

    fn session_key_from_seed(seed: u8) -> SessionKey {
        let secret = StaticSecret::from([seed; 32]);
        let peer = PublicKey::from(&StaticSecret::from([seed ^ 0xFF; 32]));
        let mut session_key = SessionKey::new(PublicKey::from(&secret));
        session_key
            .generate_symmetric_key(&secret, &peer.to_bytes())
            .expect("derive session key");
        session_key
    }

    fn cosmos_propose_namespaces() -> ProposeNamespaces {
        serde_json::from_value(serde_json::json!({
            "cosmos": {
                "chains": ["cosmos:cosmoshub-4"],
                "methods": ["cosmos_signDirect"],
                "events": []
            }
        }))
        .expect("parse propose namespaces")
    }

    /// The WC2 ECDH (chapter 22 §22.6): the proposer side (which retains its
    /// ephemeral secret) and the responder side must converge on the same
    /// 32-byte symmetric key and therefore the same session topic.
    fn test_app_metadata() -> Metadata { metadata::generate_metadata(None, "00000000") }

    #[test]
    fn ecdh_round_trip_derives_matching_key_and_topic() {
        let our_secret = StaticSecret::from([7u8; 32]);
        let our_public = PublicKey::from(&our_secret);
        let peer_secret = StaticSecret::from([19u8; 32]);
        let peer_public = PublicKey::from(&peer_secret);

        // Our (proposer) side: advertise our public, derive against the peer's.
        let mut ours = SessionKey::new(our_public);
        ours.generate_symmetric_key(&our_secret, &peer_public.to_bytes())
            .expect("derive ours");

        // Responder side: advertise their public, derive against ours.
        let mut theirs = SessionKey::new(peer_public);
        theirs
            .generate_symmetric_key(&peer_secret, &our_public.to_bytes())
            .expect("derive theirs");

        assert_ne!(ours.symmetric_key(), [0u8; 32], "key must be derived");
        assert_eq!(
            ours.symmetric_key(),
            theirs.symmetric_key(),
            "ECDH + HKDF must converge on the same symmetric key"
        );
        assert_eq!(
            ours.generate_topic(),
            theirs.generate_topic(),
            "both sides must derive the same session topic"
        );
    }

    /// `wc_sessionPropose` construction (chapter 22 §22.4): the request carries
    /// the spec method/tag, advertises our ephemeral public key, names the IRN
    /// relay, and threads the caller's namespace requirements through.
    #[test]
    fn propose_request_advertises_our_key_and_threads_namespaces() {
        let secret = StaticSecret::from([3u8; 32]);
        let public = PublicKey::from(&secret);
        let required = serde_json::json!({
            "eip155": {
                "chains": ["eip155:1"],
                "methods": ["personal_sign", "eth_sendTransaction"],
                "events": ["accountsChanged"]
            }
        });

        let request = build_propose_request(&public, &required, None, &test_app_metadata());

        assert_eq!(propose::METHOD, "wc_sessionPropose");
        assert_eq!(propose::TAG.request, 1100);
        assert_eq!(propose::TAG.response, 1101);
        assert_eq!(
            request.proposer.public_key,
            hex::encode(public.to_bytes()),
            "proposer advertises our ephemeral public key"
        );
        assert_eq!(request.relays.len(), 1);
        assert_eq!(request.relays[0].protocol, "irn");

        let ns = request
            .required_namespaces
            .get("eip155")
            .expect("eip155 requirement threaded through");
        assert!(ns.chains.contains("eip155:1"));
        assert!(ns.methods.contains("personal_sign"));
        assert!(ns.methods.contains("eth_sendTransaction"));
        assert!(ns.events.contains("accountsChanged"));
        assert!(request.optional_namespaces.is_none());
    }

    /// Settle parsing → `Session` build (chapter 22 §22.8.1.5): a representative
    /// `wc_sessionSettle` payload yields a `Session` carrying the right topic,
    /// pairing topic, namespaces, metadata, expiry, and per-account properties.
    #[test]
    fn settle_payload_builds_session_with_expected_fields() {
        let session_topic = Topic::from("session-topic".to_string());
        let pairing_topic = Topic::from("pairing-topic".to_string());

        let pending = PendingSettle {
            session_key: session_key_from_seed(5),
            pairing_topic: pairing_topic.clone(),
            propose_namespaces: cosmos_propose_namespaces(),
        };

        let params = serde_json::json!({
            "relay": { "protocol": "irn" },
            "controller": {
                "publicKey": "a3ad5e26070ddb2809200c6f56e739333512015bceeadbb8ea1731c4c7ddb207",
                "metadata": {
                    "description": "Keplr",
                    "url": "https://keplr.app",
                    "icons": [],
                    "name": "Keplr"
                }
            },
            "namespaces": {
                "cosmos": {
                    "accounts": ["cosmos:cosmoshub-4:cosmos1examplexyz"],
                    "methods": ["cosmos_signDirect", "cosmos_getAccounts"],
                    "events": []
                }
            },
            "expiry": 32_503_680_000u64,
            "sessionProperties": {
                "keys": [{
                    "chainId": "cosmos:cosmoshub-4",
                    "name": "account-0",
                    "algo": "secp256k1",
                    "pubKey": "02abcdef",
                    "address": "ABCDEF",
                    "bech32Address": "cosmos1examplexyz",
                    "ethereumHexAddress": "0x0123",
                    "isNanoLedger": true,
                    "isKeystone": false
                }]
            }
        });

        let settle: settle::SettleRequest = serde_json::from_value(params).expect("parse settle request");
        let session = build_session(session_topic.clone(), pending, settle, &test_app_metadata());

        assert_eq!(session.topic, session_topic);
        assert_eq!(session.pairing_topic, pairing_topic);
        assert_eq!(session.metadata.name, "Keplr");
        assert_eq!(session.expiry, 32_503_680_000);
        assert_eq!(session.encoding, EncodingAlgo::Base64);
        assert_eq!(session.encoding.encode([0x01u8, 0x02, 0x03, 0xff]), "AQID/w==");
        assert!(
            session
                .to_stored()
                .expect("serialize session")
                .data
                .contains("\"encoding_algo\":\"Base64\""),
            "Keplr sessions must persist Base64 encoding_algo"
        );

        let cosmos = session.namespaces.get("cosmos").expect("cosmos namespace");
        assert!(cosmos
            .accounts
            .as_ref()
            .expect("accounts")
            .contains("cosmos:cosmoshub-4:cosmos1examplexyz"));
        assert!(cosmos.methods.contains("cosmos_signDirect"));
        assert!(
            session.propose_namespaces.get("cosmos").is_some(),
            "propose namespaces carried forward"
        );

        let keys = session
            .properties
            .expect("session properties")
            .keys
            .expect("per-account keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].chain_id, "cosmos:cosmoshub-4");
        assert!(keys[0].is_nano_ledger, "advisory hardware-wallet flag preserved");
    }

    #[test]
    fn non_keplr_cosmos_settle_selects_hex_session_encoding() {
        let session_topic = Topic::from("session-topic-leap".to_string());
        let pairing_topic = Topic::from("pairing-topic-leap".to_string());
        let pending = PendingSettle {
            session_key: session_key_from_seed(6),
            pairing_topic: pairing_topic.clone(),
            propose_namespaces: cosmos_propose_namespaces(),
        };
        let params = serde_json::json!({
            "relay": { "protocol": "irn" },
            "controller": {
                "publicKey": "a3ad5e26070ddb2809200c6f56e739333512015bceeadbb8ea1731c4c7ddb207",
                "metadata": {
                    "description": "Leap",
                    "url": "https://leapwallet.io",
                    "icons": [],
                    "name": "Leap"
                }
            },
            "namespaces": {
                "cosmos": {
                    "accounts": ["cosmos:cosmoshub-4:cosmos1examplexyz"],
                    "methods": ["cosmos_signDirect", "cosmos_getAccounts"],
                    "events": []
                }
            },
            "expiry": 32_503_680_000u64
        });

        let settle: settle::SettleRequest = serde_json::from_value(params).expect("parse settle request");
        let session = build_session(session_topic, pending, settle, &test_app_metadata());
        let bytes = [0x01u8, 0x02, 0x03, 0xff];

        assert_eq!(session.pairing_topic, pairing_topic);
        assert!(
            session.namespaces.get("cosmos").is_some(),
            "fixture uses the cosmos namespace"
        );
        assert_eq!(session.metadata.name, "Leap");
        assert_eq!(session.encoding, EncodingAlgo::Hex);
        assert_eq!(session.encoding.encode(bytes), "010203ff");
        assert!(
            session
                .to_stored()
                .expect("serialize session")
                .data
                .contains("\"encoding_algo\":\"Hex\""),
            "non-Keplr cosmos sessions must persist Hex encoding_algo"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod reconnect_tests {
    use super::storage::sqlite::SqliteSessionStorage;
    use super::storage::WcStorageOps;
    use common::block_on;
    use db_common::async_sql_conn::AsyncConnection;

    #[test]
    fn the_relay_host_has_no_port_path_or_query() {
        assert_eq!(
            super::relay_host("wss://relay.example.com/?projectId=P&auth=J"),
            "relay.example.com"
        );
        assert_eq!(super::relay_host("wss://relay.example.com:443/x"), "relay.example.com");
        assert_eq!(super::relay_host("relay.example.com"), "relay.example.com");
    }

    use super::*;

    #[test]
    fn reconnect_backoff_doubles_and_is_capped() {
        let secs: Vec<u64> = (0..9).map(|attempt| reconnect_backoff(attempt).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(reconnect_backoff(200).as_secs(), RECONNECT_BACKOFF_MAX_SECS);
        assert_eq!(reconnect_backoff(u32::MAX).as_secs(), RECONNECT_BACKOFF_MAX_SECS);
    }

    #[test]
    fn resubscribe_covers_sessions_establishments_and_live_pairings_once() {
        let now = 1_000_000;
        let mut live = Pairing::generate(300);
        live.expiry = now + 300;
        let mut expired = Pairing::generate(300);
        expired.expiry = now - 1;
        let pairings: HashMap<Topic, Pairing> = [live.clone(), expired.clone()]
            .into_iter()
            .map(|pairing| (pairing.topic.clone(), pairing))
            .collect();

        let session = Topic::generate();
        let establishing = Topic::generate();
        let topics = collect_resubscribe_topics(
            // The same session topic twice and a live pairing that is also listed as a session topic.
            vec![session.clone(), session.clone(), live.topic.clone()],
            &pairings,
            vec![establishing.clone()],
            now,
        );

        assert_eq!(topics.len(), 3, "each topic once");
        assert!(topics.contains(&session));
        assert!(topics.contains(&establishing));
        assert!(topics.contains(&live.topic));
        assert!(
            !topics.contains(&expired.topic),
            "an expired pairing is not subscribed again"
        );
    }

    #[test]
    fn relay_frame_log_helper_does_not_panic_on_short_topics() {
        log_relay_frame(
            "publish",
            &Topic::from("abc".to_string()),
            1108,
            "wc_sessionRequest",
            &7,
        );
    }

    /// The `iss` (`did:key`) claim of a relay auth token.
    fn token_issuer(opts: &ConnectionOptions) -> String {
        use base64::Engine;
        let relay_client::Authorization::Query(token) = &opts.auth else {
            panic!("the relay auth is a query token");
        };
        let token = token.to_string();
        let payload = token.split('.').nth(1).expect("JWT payload segment");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .expect("base64url payload");
        let claims: serde_json::Value = serde_json::from_slice(&payload).expect("JSON claims");
        claims["iss"].as_str().expect("iss claim").to_string()
    }

    #[test]
    fn app_name_is_the_project_and_the_fingerprint_of_the_key_in_use() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let fp = client_key::fingerprint(&key);
        assert_eq!(
            metadata::generate_metadata(None, &fp).name,
            format!("KDF Reloaded {fp}")
        );
        assert_eq!(metadata::generate_metadata(Some("My Node"), &fp).name, "My Node");
        assert_eq!(
            metadata::generate_metadata(Some(""), &fp).name,
            format!("KDF Reloaded {fp}")
        );
    }

    fn params_with(signing_key: SigningKey) -> RelayConnectParams {
        RelayConnectParams {
            project_id: "test-project".to_string(),
            relay_address: metadata::RELAY_ADDRESS.to_string(),
            signing_key,
            key_source: client_key::ClientKeySource::Created,
        }
    }

    #[test]
    fn relay_client_key_is_created_once_and_reused_across_connects() {
        block_on(async {
            let conn = AsyncConnection::open_in_memory().await.expect("open in-memory db");
            let storage = SqliteSessionStorage::new(conn);
            storage.init().await.expect("init schema");
            assert_eq!(storage.load_client_key().await.unwrap(), None);

            let (key, source) = client_key::load_or_create(&storage).await.expect("create key");
            assert_eq!(source, client_key::ClientKeySource::Created);
            let params = params_with(key);
            let first = token_issuer(&params.options(1).expect("first connect"));
            let second = token_issuer(&params.options(2).expect("reconnect"));
            assert_eq!(first, second, "every connect is signed with the same client key");

            // Another key is another client identity.
            let other = params_with(SigningKey::generate(&mut rand::thread_rng()));
            assert_ne!(first, token_issuer(&other.options(1).expect("other connect")));
        });
    }

    #[test]
    fn relay_client_key_survives_a_storage_round_trip() {
        block_on(async {
            let path = std::env::temp_dir().join(format!("kdf-wc-client-key-test-{}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let created = {
                let conn = AsyncConnection::open(path.clone()).await.expect("open db");
                let storage = SqliteSessionStorage::new(conn);
                storage.init().await.expect("init schema");
                let (key, source) = client_key::load_or_create(&storage).await.expect("create key");
                assert_eq!(source, client_key::ClientKeySource::Created);
                key
            };
            // A new connection to the same file is the restarted node.
            let conn = AsyncConnection::open(path.clone()).await.expect("reopen db");
            let storage = SqliteSessionStorage::new(conn);
            storage.init().await.expect("init schema again");
            let (loaded, source) = client_key::load_or_create(&storage).await.expect("load key");
            let _ = std::fs::remove_file(&path);
            assert_eq!(source, client_key::ClientKeySource::Loaded);
            assert_eq!(loaded.to_bytes(), created.to_bytes());
            assert_eq!(
                token_issuer(&params_with(loaded).options(1).unwrap()),
                token_issuer(&params_with(created).options(1).unwrap()),
            );
        });
    }

    #[test]
    fn corrupt_stored_relay_client_key_is_replaced_by_a_new_one() {
        block_on(async {
            for corrupt in ["not hex at all", "abcd", ""] {
                let conn = AsyncConnection::open_in_memory().await.expect("open in-memory db");
                let storage = SqliteSessionStorage::new(conn);
                storage.init().await.expect("init schema");
                storage.save_client_key(corrupt.to_string()).await.unwrap();

                let (key, source) = client_key::load_or_create(&storage).await.expect("replace key");
                assert_eq!(
                    source,
                    client_key::ClientKeySource::Created,
                    "corrupt value {corrupt:?}"
                );
                let (again, source) = client_key::load_or_create(&storage).await.expect("load new key");
                assert_eq!(source, client_key::ClientKeySource::Loaded);
                assert_eq!(again.to_bytes(), key.to_bytes());
            }
        });
    }

    #[test]
    fn relay_client_key_fingerprint_is_the_public_key_prefix_only() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let fp = client_key::fingerprint(&key);
        assert_eq!(fp.len(), 8);
        assert!(hex::encode(key.verifying_key().as_bytes()).starts_with(&fp));
    }
}
