#[macro_use] extern crate lazy_static;

mod adex_ping;
pub mod atomicdex_behaviour;
mod ip_helpers;
pub mod network_streamer;
pub mod peers_exchange;
mod redial_guard;
pub mod relay_address;
pub mod request_response;
#[cfg(not(target_arch = "wasm32"))] mod resolving_transport;
mod runtime;

use common::msgpack_bounds::check_peer_value;
use lazy_static::lazy_static;
use secp256k1::ecdsa::Signature;
use secp256k1::{Message as SecpMessage, PublicKey as Secp256k1Pubkey, Secp256k1, SecretKey, SignOnly, VerifyOnly};
use sha2::{Digest, Sha256};

pub use atomicdex_behaviour::{spawn_gossipsub, AdexBehaviourError, NodeType, WssCerts};
pub use libp2p::gossipsub::{Event as GossipsubEvent, Message as GossipsubMessage, MessageId};
pub use libp2p::identity::secp256k1::PublicKey as Libp2pSecpPublic;
pub use libp2p::identity::DecodingError;
pub use libp2p::identity::PublicKey as Libp2pPublic;
pub use libp2p::{Multiaddr, PeerId};
pub use peers_exchange::PeerAddresses;
pub use relay_address::{RelayAddress, RelayAddressError};
use serde::{de, Deserialize, Serialize, Serializer};

lazy_static! {
    static ref SECP_VERIFY: Secp256k1<VerifyOnly> = Secp256k1::verification_only();
    static ref SECP_SIGN: Secp256k1<SignOnly> = Secp256k1::signing_only();
}

#[derive(Clone, Copy)]
pub enum NetworkInfo {
    /// The in-memory network.
    InMemory,
    /// The distributed network (out of the app memory).
    Distributed { network_ports: NetworkPorts },
}

impl NetworkInfo {
    pub fn in_memory(&self) -> bool { matches!(self, NetworkInfo::InMemory) }
}

#[derive(Clone, Copy)]
pub struct NetworkPorts {
    pub tcp: u16,
    pub wss: u16,
}

pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec(message)
}

/// Decodes a MessagePack value received from a peer.
///
/// The value is first checked against the bounds of CRD ch.28 R16a: nesting at most
/// [`MAX_PEER_NESTING`](common::msgpack_bounds::MAX_PEER_NESTING) levels, and every declared
/// length and count within the remaining bytes. A value outside the bounds is not decoded: it is
/// counted and the call returns a decoding error.
///
/// # Errors
///
/// [`rmp_serde::decode::Error`] if the value is outside the bounds or does not decode as `T`.
pub fn decode_message<'de, T: de::Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, rmp_serde::decode::Error> {
    if check_peer_value(bytes, "decode_message").is_err() {
        return Err(rmp_serde::decode::Error::Syntax(
            "value outside the decoding bounds".into(),
        ));
    }
    rmp_serde::from_read_ref(bytes)
}

#[derive(Deserialize, Serialize)]
struct SignedMessageSerdeHelper<'a> {
    pubkey: PublicKey,
    #[serde(with = "serde_bytes")]
    signature: &'a [u8],
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
}

pub fn encode_and_sign<T: Serialize>(message: &T, secret: &[u8; 32]) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let secret = SecretKey::from_slice(secret).unwrap();
    let encoded = encode_message(message)?;
    let sig_hash = SecpMessage::from_digest_slice(&sha256(&encoded)).expect("Message::from_slice should never fail");
    let sig = SECP_SIGN.sign_ecdsa(&sig_hash, &secret);
    let serialized_sig = sig.serialize_compact();
    let pubkey = PublicKey::from(Secp256k1Pubkey::from_secret_key(&*SECP_SIGN, &secret));
    let msg = SignedMessageSerdeHelper {
        pubkey,
        signature: &serialized_sig,
        payload: &encoded,
    };
    encode_message(&msg)
}

pub fn decode_signed<'de, T: de::Deserialize<'de>>(
    encoded: &'de [u8],
) -> Result<(T, Signature, PublicKey), rmp_serde::decode::Error> {
    let helper: SignedMessageSerdeHelper = decode_message(encoded)?;
    let signature = Signature::from_compact(helper.signature)
        .map_err(|e| rmp_serde::decode::Error::Syntax(format!("Failed to parse signature {}", e)))?;
    let sig_hash =
        SecpMessage::from_digest_slice(&sha256(helper.payload)).expect("Message::from_slice should never fail");
    match &helper.pubkey {
        PublicKey::Secp256k1(serialized_pub) => {
            if SECP_VERIFY
                .verify_ecdsa(&sig_hash, &signature, &serialized_pub.0)
                .is_err()
            {
                return Err(rmp_serde::decode::Error::Syntax("Invalid message signature".into()));
            }
        },
    }

    let payload: T = decode_message(helper.payload)?;
    Ok((payload, signature, helper.pubkey))
}

fn sha256(input: impl AsRef<[u8]>) -> [u8; 32] { Sha256::new().chain(input).finalize().into() }

#[derive(Debug, Eq, PartialEq)]
pub struct Secp256k1PubkeySerialize(Secp256k1Pubkey);

impl Serialize for Secp256k1PubkeySerialize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0.serialize())
    }
}

impl<'de> de::Deserialize<'de> for Secp256k1PubkeySerialize {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as de::Deserializer<'de>>::Error>
    where
        D: de::Deserializer<'de>,
    {
        let slice: &[u8] = de::Deserialize::deserialize(deserializer)?;
        let pubkey =
            Secp256k1Pubkey::from_slice(slice).map_err(|e| de::Error::custom(format!("Error {} parsing pubkey", e)))?;

        Ok(Secp256k1PubkeySerialize(pubkey))
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PublicKey {
    Secp256k1(Secp256k1PubkeySerialize),
}

impl PublicKey {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            PublicKey::Secp256k1(pubkey) => pubkey.0.serialize().to_vec(),
        }
    }

    pub fn to_hex(&self) -> String {
        match self {
            PublicKey::Secp256k1(pubkey) => hex::encode(pubkey.0.serialize().as_ref()),
        }
    }

    pub fn unprefixed(&self) -> [u8; 32] {
        let mut res = [0; 32];
        match self {
            PublicKey::Secp256k1(pubkey) => res.copy_from_slice(&pubkey.0.serialize()[1..33]),
        }
        res
    }
}

impl From<Secp256k1Pubkey> for PublicKey {
    fn from(pubkey: Secp256k1Pubkey) -> Self { PublicKey::Secp256k1(Secp256k1PubkeySerialize(pubkey)) }
}

pub type TopicPrefix = &'static str;
pub const TOPIC_SEPARATOR: char = '/';

pub fn pub_sub_topic(prefix: TopicPrefix, topic: &str) -> String {
    let mut res = prefix.to_owned();
    res.push(TOPIC_SEPARATOR);
    res.push_str(topic);
    res
}

#[test]
fn signed_message_serde() {
    let secret = [1u8; 32];
    let initial_msg = vec![0u8; 32];
    let signed_encoded = encode_and_sign(&initial_msg, &secret).unwrap();

    let (decoded, ..) = decode_signed::<Vec<u8>>(&signed_encoded).unwrap();
    assert_eq!(decoded, initial_msg);
}

#[cfg(test)]
mod t4a_bounded_reading_tests {
    use super::*;
    use common::msgpack_bounds::{check_msgpack_bounds, dropped_peer_values, max_nesting_depth, MAX_PEER_NESTING};

    const SECRET: [u8; 32] = [7u8; 32];

    /// The envelope of `encode_and_sign`, re-encoded as a map with one more key. The key's value is
    /// `levels` one-element arrays around nil, so the envelope's total nesting is `levels + 1`.
    fn envelope_with_extra_key(envelope: &[u8], levels: usize) -> Vec<u8> {
        let helper: SignedMessageSerdeHelper = decode_message(envelope).unwrap();
        let mut named = rmp_serde::to_vec_named(&helper).unwrap();
        assert_eq!(named[0], 0x83, "a map of three entries");
        named[0] = 0x84;
        named.extend_from_slice(&[0xa5, b'e', b'x', b't', b'r', b'a']);
        named.extend(vec![0x91; levels]);
        named.push(0xc0);
        named
    }

    #[test]
    fn t4a_d_envelope_beyond_the_bound_is_dropped_and_counted() {
        let envelope = encode_and_sign(&vec![1u8, 2, 3], &SECRET).unwrap();
        assert!(decode_signed::<Vec<u8>>(&envelope).is_ok());

        // Total nesting 64: inside the bound, the extra key is ignored by the decoder.
        let at_bound = envelope_with_extra_key(&envelope, MAX_PEER_NESTING - 1);
        assert!(decode_signed::<Vec<u8>>(&at_bound).is_ok());

        let before = dropped_peer_values();
        let beyond = envelope_with_extra_key(&envelope, MAX_PEER_NESTING);
        let err = decode_signed::<Vec<u8>>(&beyond).unwrap_err();
        assert!(err.to_string().contains("decoding bounds"), "{err}");
        assert!(dropped_peer_values() > before);
    }

    #[test]
    fn t4a_d_envelope_beyond_the_bound_is_dropped_before_its_signature_is_examined() {
        // The signature bytes of this envelope are not a signature at all; the error is still the bounds one.
        let helper = SignedMessageSerdeHelper {
            pubkey: PublicKey::from(Secp256k1Pubkey::from_secret_key(
                &*SECP_SIGN,
                &SecretKey::from_slice(&SECRET).unwrap(),
            )),
            signature: &[0u8; 3],
            payload: &[0xc0],
        };
        let mut named = rmp_serde::to_vec_named(&helper).unwrap();
        named[0] = 0x84;
        named.extend_from_slice(&[0xa5, b'e', b'x', b't', b'r', b'a']);
        named.extend(vec![0x91; 10_000]);
        named.push(0xc0);

        let before = dropped_peer_values();
        let err = decode_signed::<()>(&named).unwrap_err();
        assert!(err.to_string().contains("decoding bounds"), "{err}");
        assert!(dropped_peer_values() > before);
    }

    #[test]
    fn t4a_d_payload_beyond_the_bound_is_dropped_after_a_valid_signature() {
        // A correctly signed payload that nests 65 levels: the envelope passes, the payload does not.
        let mut payload = vec![0x91; MAX_PEER_NESTING + 1];
        payload.push(0xc0);
        let secret = SecretKey::from_slice(&SECRET).unwrap();
        let sig_hash = SecpMessage::from_digest_slice(&sha256(&payload)).unwrap();
        let signature = SECP_SIGN.sign_ecdsa(&sig_hash, &secret).serialize_compact();
        let helper = SignedMessageSerdeHelper {
            pubkey: PublicKey::from(Secp256k1Pubkey::from_secret_key(&*SECP_SIGN, &secret)),
            signature: &signature,
            payload: &payload,
        };
        let envelope = encode_message(&helper).unwrap();
        assert_eq!(check_msgpack_bounds(&envelope), Ok(envelope.len()));

        let before = dropped_peer_values();
        let err = decode_signed::<()>(&envelope).unwrap_err();
        assert!(err.to_string().contains("decoding bounds"), "{err}");
        assert!(dropped_peer_values() > before);

        // At the bound the same payload is read (and then does not fit the type asked for).
        let mut payload = vec![0x91; MAX_PEER_NESTING];
        payload.push(0xc0);
        let sig_hash = SecpMessage::from_digest_slice(&sha256(&payload)).unwrap();
        let signature = SECP_SIGN.sign_ecdsa(&sig_hash, &secret).serialize_compact();
        let helper = SignedMessageSerdeHelper {
            pubkey: PublicKey::from(Secp256k1Pubkey::from_secret_key(&*SECP_SIGN, &secret)),
            signature: &signature,
            payload: &payload,
        };
        let envelope = encode_message(&helper).unwrap();
        let err = decode_signed::<()>(&envelope).unwrap_err();
        assert!(!err.to_string().contains("decoding bounds"), "{err}");
    }

    #[test]
    fn t4a_d_peer_addresses_of_the_peers_topic_still_decode() {
        let mut addresses = PeerAddresses::new();
        addresses.insert("/ip4/127.0.0.1/tcp/42845".parse().unwrap());
        addresses.insert("/dns4/example.org/tcp/42845".parse().unwrap());
        let encoded = rmp_serde::to_vec(&addresses).unwrap();
        let decoded: PeerAddresses = decode_message(&encoded).unwrap();
        assert_eq!(decoded, addresses);
    }

    #[test]
    fn t4a_a_envelope_and_peer_addresses_pass_the_check_within_half_the_bound() {
        let mut addresses = PeerAddresses::new();
        addresses.insert("/ip4/127.0.0.1/tcp/42845".parse().unwrap());
        let signed = encode_and_sign(&(String::from("payload"), vec![1u8, 2, 3], Some(5u64)), &SECRET).unwrap();
        for encoded in [signed, rmp_serde::to_vec(&addresses).unwrap()] {
            assert_eq!(check_msgpack_bounds(&encoded), Ok(encoded.len()));
            assert!(max_nesting_depth(&encoded) <= MAX_PEER_NESTING / 2);
        }
    }
}
