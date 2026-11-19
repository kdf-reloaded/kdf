//! The persistent relay client signing key.
//!
//! The relay identifies a client by the ed25519 key that signs its auth token (the `did:key` in the `iss` claim).
//! The node keeps one such key for its WalletConnect context, so a relay reconnect or a node restart does not
//! change the client identity under which the sessions were created. The key is secret: it is never logged,
//! returned or put into an error text.

use crate::error::WalletConnectError;
use crate::storage::WcStorageOps;
use relay_rpc::auth::ed25519_dalek::SigningKey;

/// Where the signing key of this run came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientKeySource {
    /// Read from the WalletConnect storage.
    Loaded,
    /// Generated now (nothing stored, or the stored value was unusable) and written to the storage.
    Created,
}

impl ClientKeySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ClientKeySource::Loaded => "loaded",
            ClientKeySource::Created => "created",
        }
    }
}

/// Decodes a stored key (hex of the 32-byte seed). `None` when the value is not exactly such a seed.
fn decode_key(stored: &str) -> Option<SigningKey> {
    let bytes = hex::decode(stored.trim()).ok()?;
    let seed: [u8; 32] = bytes.try_into().ok()?;
    Some(SigningKey::from_bytes(&seed))
}

/// Loads the stored relay client key, or generates a new one and stores it when none is stored or the stored value
/// is corrupt.
///
/// # Errors
///
/// Returns [`WalletConnectError::Storage`] if the storage cannot be read or written.
pub(crate) async fn load_or_create(
    storage: &dyn WcStorageOps,
) -> Result<(SigningKey, ClientKeySource), WalletConnectError> {
    let stored = storage.load_client_key().await?;
    if let Some(key) = stored.as_deref().and_then(decode_key) {
        return Ok((key, ClientKeySource::Loaded));
    }
    if stored.is_some() {
        common::log::warn!(
            "walletconnect: stored relay client key is unusable; a new client identity was created, and existing \
             WalletConnect sessions must be paired again"
        );
    }
    let key = SigningKey::generate(&mut rand::thread_rng());
    storage.save_client_key(hex::encode(key.to_bytes())).await?;
    if stored.is_none() {
        common::log::debug!("walletconnect: a new relay client identity was created");
    }
    Ok((key, ClientKeySource::Created))
}

/// A short, non-secret fingerprint of the client identity: the first 8 hex digits of the public key.
pub(crate) fn fingerprint(key: &SigningKey) -> String { hex::encode(&key.verifying_key().as_bytes()[..4]) }
