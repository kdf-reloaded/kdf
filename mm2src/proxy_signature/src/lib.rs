//! Proxy authentication via libp2p message signing.
//!
//! Provides [`RawMessage`] and [`ProxySign`] for creating and validating
//! signed proxy requests. A node signs its outgoing proxy requests so the
//! receiving proxy can verify the sender's identity without shared secrets.

use chrono::Utc;
use http::Uri;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

/// Magic prefix for the signed payload, preventing cross-protocol replay.
const SIGN_PREFIX: &[u8] = b"Proxy Auth Payload\n";

// ---------------------------------------------------------------------------
// RawMessage — the data that gets signed
// ---------------------------------------------------------------------------

/// The payload that is serialized, signed, and later verified.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawMessage {
    /// The request URI being authenticated.
    pub uri: String,
    /// Size of the request body in bytes.
    pub body_size: usize,
    /// Protobuf-encoded public key of the signer.
    pub public_key_encoded: Vec<u8>,
    /// Unix timestamp (seconds) after which the signature expires.
    pub expires_at: i64,
}

impl RawMessage {
    /// Creates a signed proxy authentication token.
    ///
    /// The signature covers the URI, body size, public key, and expiration.
    /// Returns a [`ProxySign`] ready for transport.
    pub fn sign(
        keypair: &Keypair,
        uri: &Uri,
        body_size: usize,
        expires_in_seconds: i64,
    ) -> Result<ProxySign, libp2p::identity::SigningError> {
        let pubkey = keypair.public();
        let encoded_pk = pubkey.encode_protobuf();
        let peer_id = PeerId::from_public_key(&pubkey);

        let expires_at = Utc::now().timestamp() + expires_in_seconds;

        let msg = RawMessage {
            uri: uri.to_string(),
            body_size,
            public_key_encoded: encoded_pk,
            expires_at,
        };

        let payload = msg.to_signable_bytes();
        let sig = keypair.sign(&payload)?;

        Ok(ProxySign {
            signature_bytes: sig,
            address: peer_id.to_string(),
            raw_message: msg,
        })
    }

    /// Serializes the message into bytes suitable for signing / verification.
    fn to_signable_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SIGN_PREFIX.len() + self.public_key_encoded.len() + self.uri.len() + 16);
        buf.extend_from_slice(SIGN_PREFIX);
        buf.extend_from_slice(&self.public_key_encoded);
        buf.extend_from_slice(self.uri.as_bytes());
        buf.extend_from_slice(&self.body_size.to_ne_bytes());
        buf.extend_from_slice(&self.expires_at.to_ne_bytes());
        buf
    }
}

// ---------------------------------------------------------------------------
// ProxySign — signed payload ready for transport
// ---------------------------------------------------------------------------

/// A signed proxy authentication payload.
///
/// Contains the raw message, the ECDSA/Ed25519 signature, and the signer's
/// peer ID. The receiver calls [`is_valid_message`](ProxySign::is_valid_message)
/// to verify authenticity and freshness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxySign {
    /// The cryptographic signature over the raw message.
    pub signature_bytes: Vec<u8>,
    /// String representation of the signer's libp2p PeerId.
    pub address: String,
    /// The signed payload.
    pub raw_message: RawMessage,
}

impl ProxySign {
    /// Validates the signature, peer identity, and expiration.
    ///
    /// `max_exp_secs` is the maximum allowed lifetime in seconds; messages
    /// with a longer remaining lifetime are rejected to prevent replay.
    pub fn is_valid_message(&self, max_exp_secs: u64) -> bool {
        let now = Utc::now().timestamp();

        // Must not be expired.
        if self.raw_message.expires_at <= now {
            return false;
        }

        // Remaining lifetime must not exceed the server's policy.
        let remaining = (self.raw_message.expires_at - now) as u64;
        if remaining > max_exp_secs {
            return false;
        }

        // Decode the public key from the protobuf blob.
        let pubkey = match PublicKey::try_decode_protobuf(&self.raw_message.public_key_encoded) {
            Ok(k) => k,
            Err(_) => return false,
        };

        // Verify the claimed peer address matches the public key.
        let expected_peer = PeerId::from_public_key(&pubkey);
        if expected_peer.to_string() != self.address {
            return false;
        }

        // Verify the cryptographic signature.
        let payload = self.raw_message.to_signable_bytes();
        pubkey.verify(&payload, &self.signature_bytes)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use http::Uri;

    fn test_keypair() -> Keypair {
        // Deterministic Ed25519 keypair for reproducible tests.
        let mut seed: [u8; 32] = [42; 32];
        let secret = libp2p::identity::ed25519::SecretKey::try_from_bytes(&mut seed).unwrap();
        let ed_kp = libp2p::identity::ed25519::Keypair::from(secret);
        Keypair::from(ed_kp)
    }

    fn test_uri() -> Uri { "https://proxy.example.com/rpc".parse().unwrap() }

    #[test]
    fn sign_and_verify() {
        let kp = test_keypair();
        let uri = test_uri();
        let proxy = RawMessage::sign(&kp, &uri, 256, 300).unwrap();
        assert!(proxy.is_valid_message(600));
    }

    #[test]
    fn expired_signature_rejected() {
        let kp = test_keypair();
        let uri = test_uri();
        // Negative expiration → already expired.
        let proxy = RawMessage::sign(&kp, &uri, 0, -10).unwrap();
        assert!(!proxy.is_valid_message(600));
    }

    #[test]
    fn tampered_message_rejected() {
        let kp = test_keypair();
        let uri = test_uri();
        let mut proxy = RawMessage::sign(&kp, &uri, 100, 300).unwrap();

        // Tamper with URI.
        proxy.raw_message.uri = "https://evil.com/steal".into();
        assert!(!proxy.is_valid_message(600));
    }

    #[test]
    fn lifetime_overflow_rejected() {
        let kp = test_keypair();
        let uri = test_uri();
        let proxy = RawMessage::sign(&kp, &uri, 0, 3600).unwrap();
        // max_exp_secs=60 is less than the 3600s remaining → rejected.
        assert!(!proxy.is_valid_message(60));
    }

    #[test]
    fn peer_id_deterministic() {
        let kp = test_keypair();
        let peer = PeerId::from_public_key(&kp.public());
        // Same seed → same peer ID.
        let kp2 = test_keypair();
        let peer2 = PeerId::from_public_key(&kp2.public());
        assert_eq!(peer, peer2);
    }
}
