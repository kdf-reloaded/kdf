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
    fn from(e: relay_client::error::ClientError) -> Self { WalletConnectError::Relay(relay_error_kind(&e).to_string()) }
}

impl<T> From<relay_client::error::Error<T>> for WalletConnectError {
    fn from(e: relay_client::error::Error<T>) -> Self {
        WalletConnectError::Relay(relay_call_error_kind(&e).to_string())
    }
}

/// A fixed category for a relay client error. The relay connection URL carries the auth token and the project id as
/// query parameters, and the `Display` of a client or transport error can contain that URL, so no log line or RPC
/// answer may format such an error; they use this category instead.
pub fn relay_error_kind(e: &relay_client::error::ClientError) -> &'static str {
    use relay_client::error::ClientError;
    match e {
        ClientError::RequestBuilder(_) => "failed to build the connection request",
        ClientError::WebsocketClient(_) => "websocket client error",
        ClientError::ChannelClosed => "internal channel closed",
        ClientError::DuplicateRequestId => "duplicate request id",
        ClientError::InvalidResponseId => "invalid response id",
        // The library raises this when the relay answered with an error whose code or tag is outside its table.
        ClientError::InvalidErrorResponse => "relay error answer the client library cannot classify",
        ClientError::Serialization(_) => "serialization failed",
        ClientError::Deserialization(_) => "deserialization failed",
        ClientError::Rpc { .. } => "relay rpc error",
        ClientError::InvalidRequestType => "invalid request type",
        #[allow(unreachable_patterns)]
        _ => "relay client error",
    }
}

/// Like [`relay_error_kind`], for the error of a relay request (a client failure or an answer of the relay).
pub fn relay_call_error_kind<T>(e: &relay_client::error::Error<T>) -> &'static str {
    match e {
        relay_client::error::Error::Client(c) => relay_error_kind(c),
        relay_client::error::Error::Response(e) => match e {
            relay_rpc::rpc::Error::Auth(_) => "relay rejected the request: auth",
            relay_rpc::rpc::Error::Payload(_) => "relay rejected the request: payload",
            relay_rpc::rpc::Error::Handler(_) => "relay rejected the request: handler",
            relay_rpc::rpc::Error::Internal(_) => "relay rejected the request: internal",
            relay_rpc::rpc::Error::TooManyRequests => "relay rejected the request: too many requests",
        },
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod relay_error_kind_tests {
    use super::*;
    use relay_client::error::ClientError;

    fn assert_clean(text: &str) {
        for needle in ["wss://", "projectId", "auth"] {
            assert!(!text.contains(needle), "{text:?} contains {needle:?}");
        }
    }

    #[test]
    fn the_kinds_never_carry_a_url_or_credentials() {
        let leaky = "wss://relay.example/?projectId=P&auth=JWT";
        let errors = vec![
            ClientError::RequestBuilder(relay_client::error::RequestBuildError::Url(leaky.to_string())),
            ClientError::WebsocketClient(relay_client::websocket::WebsocketClientError::IntoClientError(
                leaky.to_string(),
            )),
            ClientError::WebsocketClient(relay_client::websocket::WebsocketClientError::NotConnected),
            ClientError::ChannelClosed,
            ClientError::DuplicateRequestId,
            ClientError::InvalidResponseId,
            ClientError::InvalidErrorResponse,
            ClientError::Rpc {
                code: 1,
                message: leaky.to_string(),
                data: Some(leaky.to_string()),
            },
            ClientError::InvalidRequestType,
        ];
        for e in &errors {
            assert_clean(relay_error_kind(e));
            let wc = WalletConnectError::from(ClientError::Rpc {
                code: 1,
                message: leaky.to_string(),
                data: None,
            });
            assert_clean(&wc.to_string());
        }
    }

    #[test]
    fn a_failed_request_names_what_the_relay_answered() {
        use relay_client::error::Error;
        type E = Error<()>;
        let unclassified: E = Error::Client(ClientError::InvalidErrorResponse);
        assert_eq!(
            relay_call_error_kind(&unclassified),
            "relay error answer the client library cannot classify"
        );
        let too_many: E = Error::Response(relay_rpc::rpc::Error::TooManyRequests);
        assert_eq!(
            relay_call_error_kind(&too_many),
            "relay rejected the request: too many requests"
        );
        let handler: E = Error::Response(relay_rpc::rpc::Error::Handler(()));
        assert_clean(relay_call_error_kind(&handler));
    }
}
