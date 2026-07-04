//! Error taxonomy for the WalletConnect subsystem.

use derive_more::Display;

/// Errors surfaced by the WalletConnect subsystem.
#[derive(Debug, Display)]
pub enum WalletConnectError {
    /// A CAIP-2 / chain identifier could not be understood.
    #[display(fmt = "unsupported chain: {}", _0)]
    UnsupportedChain(String),
    /// Payload encryption or decryption failed.
    #[display(fmt = "payload codec error: {}", _0)]
    Codec(String),
    /// A relay or transport level failure.
    #[display(fmt = "relay error: {}", _0)]
    Relay(String),
    /// Session-key derivation failed.
    #[display(fmt = "key derivation error: {}", _0)]
    KeyDerivation(String),
    /// Storage backend failure.
    #[display(fmt = "storage error: {}", _0)]
    Storage(String),
    /// Required configuration is missing or malformed.
    #[display(fmt = "configuration error: {}", _0)]
    Config(String),
    /// No live session is registered for the requested topic.
    #[display(fmt = "no session for topic: {}", _0)]
    SessionNotFound(String),
    /// A request timed out waiting for the wallet to respond.
    #[display(fmt = "request timed out")]
    Timeout,
    /// A wallet response was missing, malformed, or of an unexpected shape.
    #[display(fmt = "invalid wallet response: {}", _0)]
    InvalidResponse(String),
    /// Serialization / deserialization failure.
    #[display(fmt = "serde error: {}", _0)]
    Serde(String),
    /// An internal invariant was violated.
    #[display(fmt = "internal error: {}", _0)]
    Internal(String),
}

impl std::error::Error for WalletConnectError {}

impl From<serde_json::Error> for WalletConnectError {
    fn from(e: serde_json::Error) -> Self { WalletConnectError::Serde(e.to_string()) }
}

impl From<hkdf::InvalidLength> for WalletConnectError {
    fn from(e: hkdf::InvalidLength) -> Self { WalletConnectError::KeyDerivation(e.to_string()) }
}

impl From<crate::chain::UnknownChain> for WalletConnectError {
    fn from(e: crate::chain::UnknownChain) -> Self { WalletConnectError::UnsupportedChain(e.0) }
}

impl From<relay_client::error::ClientError> for WalletConnectError {
    fn from(e: relay_client::error::ClientError) -> Self { WalletConnectError::Relay(e.to_string()) }
}
