//! Clean-room behavioural contract for the WalletConnect v2 subsystem.
//!
//! Every assertion in this file pins **dictated** behaviour only — protocol
//! wire shapes (WalletConnect v2 / CAIP-2 / chain JSON-RPC method names),
//! serde wire formats, and the session-key cryptographic construction with
//! independent, RFC-anchored oracles. It deliberately avoids asserting any
//! discretionary internal expression (private helper names, control flow,
//! storage SQL, error strings) so that it can serve as the frozen oracle for
//! an independent re-implementation without re-leaking implementation detail.
//!
//! Oracles:
//!   * x25519 ECDH:        RFC 7748 §6.1 known-answer vector.
//!   * HKDF-SHA256:        RFC 5869 construction (salt = none, info = empty),
//!                         recomputed independently here.
//!   * topic derivation:   SHA-256 over the symmetric key (recomputed here).

use hkdf::Hkdf;
use kdf_walletconnect::chain::{WcChain, WcChainId, WcRequestMethods};
use kdf_walletconnect::session::{EncodingAlgo, KeyInfo, SessionProperties, SessionType};
use kdf_walletconnect::SessionKey;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use x25519_dalek::{PublicKey, StaticSecret};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn h32(s: &str) -> [u8; 32] {
    let v = hex::decode(s).expect("valid hex");
    v.try_into().expect("32 bytes")
}

/// Independent HKDF-SHA256 oracle with the WalletConnect-dictated parameters:
/// `salt = None`, `info = empty`, output length 32.
fn hkdf_sha256_dictated(ikm: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut out = [0u8; 32];
    hk.expand(&[], &mut out).expect("32 is a valid HKDF length");
    out
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// crypto: session-key derivation (x25519 ECDH + HKDF-SHA256)
// ---------------------------------------------------------------------------

/// RFC 7748 §6.1 known-answer test. Anchors the x25519 leg to the standard and
/// the HKDF leg to an independent recomputation, so the full derivation
/// (ECDH -> HKDF-SHA256(salt=none, info=empty) -> 32-byte symmetric key) is
/// pinned to published vectors rather than to the implementation.
#[test]
fn session_key_matches_rfc7748_plus_hkdf_known_answer() {
    // RFC 7748 §6.1
    let alice_priv = h32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let bob_pub = h32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    let expected_shared = h32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");

    let alice_secret = StaticSecret::from(alice_priv);

    // Anchor the ECDH leg to RFC 7748.
    let shared = alice_secret.diffie_hellman(&PublicKey::from(bob_pub));
    assert_eq!(
        shared.as_bytes(),
        &expected_shared,
        "x25519 ECDH must match RFC 7748 §6.1"
    );

    // The implementation under test.
    let mut sk = SessionKey::new(PublicKey::from(&alice_secret));
    sk.generate_symmetric_key(&alice_secret, &bob_pub)
        .expect("symmetric key derivation");

    // Independent oracle: HKDF-SHA256(salt=none, info=empty) over the shared secret.
    let expected_sym = hkdf_sha256_dictated(&expected_shared);
    assert_eq!(
        sk.symmetric_key(),
        expected_sym,
        "derived symmetric key must equal HKDF-SHA256(shared, salt=none, info=empty)"
    );
}

/// Both peers performing the exchange must converge on the same symmetric key,
/// while exposing distinct ephemeral public keys.
#[test]
fn session_key_ecdh_converges_for_both_peers() {
    let a = StaticSecret::from([0x11u8; 32]);
    let a_pub = PublicKey::from(&a);
    let b = StaticSecret::from([0x22u8; 32]);
    let b_pub = PublicKey::from(&b);

    let mut a_key = SessionKey::new(a_pub);
    a_key.generate_symmetric_key(&a, &b_pub.to_bytes()).expect("a derives");
    let mut b_key = SessionKey::new(b_pub);
    b_key.generate_symmetric_key(&b, &a_pub.to_bytes()).expect("b derives");

    assert_eq!(
        a_key.symmetric_key(),
        b_key.symmetric_key(),
        "both peers must derive an identical symmetric key"
    );
    assert_ne!(
        a_key.diffie_public_key(),
        b_key.diffie_public_key(),
        "ephemeral public keys must differ"
    );
}

/// `from_osrng` generates a fresh ephemeral keypair; the counterparty must be
/// able to reproduce the same symmetric key from the published public key.
#[test]
fn session_key_from_osrng_is_reproducible_by_counterparty() {
    let bob = StaticSecret::from([0x07u8; 32]);
    let bob_pub = PublicKey::from(&bob);

    let alice_key = SessionKey::from_osrng(&bob_pub.to_bytes()).expect("osrng derivation");

    // Counterparty recomputes using Alice's advertised ephemeral public key.
    let bob_shared = bob.diffie_hellman(&PublicKey::from(alice_key.diffie_public_key()));
    let expected = hkdf_sha256_dictated(bob_shared.as_bytes());

    assert_eq!(alice_key.symmetric_key(), expected);
}

/// The relay topic is the hex-encoded SHA-256 of the symmetric key (64 hex chars).
#[test]
fn session_key_topic_is_sha256_of_symmetric_key() {
    let secret = StaticSecret::from([0x33u8; 32]);
    let peer = PublicKey::from(&StaticSecret::from([0x44u8; 32]));
    let mut sk = SessionKey::new(PublicKey::from(&secret));
    sk.generate_symmetric_key(&secret, &peer.to_bytes()).expect("derive");

    let topic = sk.generate_topic();
    assert_eq!(topic.len(), 64, "topic is a 32-byte hex digest");
    assert_eq!(topic, sha256_hex(&sk.symmetric_key()));
}

/// A freshly constructed key has a zeroed symmetric key, and the Debug
/// representation must never expose symmetric-key material.
#[test]
fn session_key_new_is_zeroed_and_debug_is_masked() {
    let sk = SessionKey::new(PublicKey::from(&StaticSecret::from([0x09u8; 32])));
    assert_eq!(sk.symmetric_key(), [0u8; 32]);

    // Derive a non-zero key, then assert Debug does not leak it.
    let secret = StaticSecret::from([0x55u8; 32]);
    let peer = PublicKey::from(&StaticSecret::from([0x66u8; 32]));
    let mut live = SessionKey::new(PublicKey::from(&secret));
    live.generate_symmetric_key(&secret, &peer.to_bytes()).expect("derive");
    let rendered = format!("{live:?}");
    assert!(
        !rendered.contains(&hex::encode(live.symmetric_key())),
        "Debug must not render raw symmetric-key bytes"
    );
}

// ---------------------------------------------------------------------------
// chain identifiers (CAIP-2) and request methods
// ---------------------------------------------------------------------------

#[test]
fn caip2_parses_supported_namespaces() {
    let eip = WcChainId::try_from_str("eip155:1").expect("eip155");
    assert_eq!(eip.chain, WcChain::Eip155);
    assert_eq!(eip.id, "1");
    assert_eq!(eip.to_string(), "eip155:1");

    let cosmos = WcChainId::try_from_str("cosmos:cosmoshub-4").expect("cosmos");
    assert_eq!(cosmos.chain, WcChain::Cosmos);
    assert_eq!(cosmos.id, "cosmoshub-4");
    assert_eq!(cosmos.to_string(), "cosmos:cosmoshub-4");

    let btc = WcChainId::try_from_str("bip122:000000000019d6689c085ae165831e93").expect("bip122");
    assert_eq!(btc.chain, WcChain::Bip122);
    assert_eq!(btc.id, "000000000019d6689c085ae165831e93");
}

#[test]
fn caip2_rejects_malformed_identifiers() {
    assert!(WcChainId::try_from_str("eip155").is_err(), "missing reference");
    assert!(WcChainId::try_from_str("eip155:1:2").is_err(), "too many segments");
    assert!(
        WcChainId::try_from_str("solana:mainnet").is_err(),
        "unsupported namespace"
    );
    assert!(WcChain::from_str("solana").is_err(), "unsupported namespace");
}

#[test]
fn chain_constructors_and_as_ref_round_trip() {
    assert_eq!(WcChainId::new_eip155("137".into()).to_string(), "eip155:137");
    assert_eq!(
        WcChainId::new_cosmos("osmosis-1".into()).to_string(),
        "cosmos:osmosis-1"
    );
    for raw in ["eip155", "cosmos", "bip122"] {
        let chain = WcChain::from_str(raw).expect("supported");
        assert_eq!(chain.as_ref(), raw);
    }
}

/// Wire method names are dictated by the chain / WalletConnect specifications.
#[test]
fn request_method_wire_strings_are_spec_exact() {
    let cases = [
        (WcRequestMethods::CosmosSignDirect, "cosmos_signDirect"),
        (WcRequestMethods::CosmosSignAmino, "cosmos_signAmino"),
        (WcRequestMethods::CosmosGetAccounts, "cosmos_getAccounts"),
        (WcRequestMethods::EthSignTransaction, "eth_signTransaction"),
        (WcRequestMethods::EthSendTransaction, "eth_sendTransaction"),
        (WcRequestMethods::EthPersonalSign, "personal_sign"),
        (WcRequestMethods::UtxoGetAccountAddresses, "getAccountAddresses"),
        (WcRequestMethods::UtxoSendTransfer, "sendTransfer"),
        (WcRequestMethods::UtxoSignPsbt, "signPsbt"),
        (WcRequestMethods::UtxoPersonalSign, "signMessage"),
    ];
    for (method, expected) in cases {
        assert_eq!(method.as_ref(), expected);
    }
}

// ---------------------------------------------------------------------------
// serde wire formats
// ---------------------------------------------------------------------------

fn sample_key_info() -> KeyInfo {
    KeyInfo {
        chain_id: "cosmoshub-4".to_string(),
        name: "Account 1".to_string(),
        algo: "secp256k1".to_string(),
        pub_key: "0123456789ABCDEF".to_string(),
        address: "cosmos1abc".to_string(),
        bech32_address: "cosmos1abc".to_string(),
        ethereum_hex_address: "0xabc".to_string(),
        is_nano_ledger: false,
        is_keystone: false,
    }
}

/// Some wallets (e.g. Keplr) deliver `keys` as a JSON-encoded string rather
/// than an array; both encodings must deserialize to the same value.
#[test]
fn session_properties_keys_accepts_string_or_array() {
    let key = sample_key_info();

    let inner = serde_json::to_string(&vec![key.clone()]).unwrap();
    let as_string = format!(r#"{{"keys": {}}}"#, serde_json::to_string(&inner).unwrap());
    let from_string: SessionProperties = serde_json::from_str(&as_string).unwrap();
    assert_eq!(from_string.keys, Some(vec![key.clone()]));

    let as_array = format!(r#"{{"keys": [{}]}}"#, serde_json::to_string(&key).unwrap());
    let from_array: SessionProperties = serde_json::from_str(&as_array).unwrap();
    assert_eq!(from_array.keys, Some(vec![key]));
}

#[test]
fn session_properties_handles_empty_and_absent_keys() {
    let empty: SessionProperties = serde_json::from_str(r#"{"keys": []}"#).unwrap();
    assert_eq!(empty.keys, Some(vec![]));

    let absent: SessionProperties = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(absent.keys, None);
}

#[test]
fn key_info_uses_camel_case_wire_names() {
    let json = r#"{
        "chainId": "cosmoshub-4",
        "name": "Account 1",
        "algo": "secp256k1",
        "pubKey": "0123456789ABCDEF",
        "address": "cosmos1abc",
        "bech32Address": "cosmos1abc",
        "ethereumHexAddress": "0xabc",
        "isNanoLedger": true,
        "isKeystone": false
    }"#;
    let parsed: KeyInfo = serde_json::from_str(json).unwrap();
    assert!(parsed.is_nano_ledger);
    assert_eq!(parsed.chain_id, "cosmoshub-4");
    assert_eq!(parsed.pub_key, "0123456789ABCDEF");

    // round-trip must reproduce camelCase wire names
    let reserialized = serde_json::to_string(&parsed).unwrap();
    assert!(reserialized.contains("\"chainId\""));
    assert!(reserialized.contains("\"isNanoLedger\""));
    assert!(reserialized.contains("\"ethereumHexAddress\""));
}

#[test]
fn session_type_serde_round_trip() {
    assert_eq!(
        serde_json::to_string(&SessionType::Controller).unwrap(),
        "\"Controller\""
    );
    assert_eq!(serde_json::to_string(&SessionType::Proposer).unwrap(), "\"Proposer\"");
    let parsed: SessionType = serde_json::from_str("\"Controller\"").unwrap();
    assert_eq!(parsed, SessionType::Controller);
}

/// Payload encoding negotiated at settlement: hex for most wallets, base64 for Keplr.
#[test]
fn encoding_algo_encodes_hex_and_base64() {
    let data = [0x01u8, 0x02, 0x03, 0xff];
    assert_eq!(EncodingAlgo::Hex.encode(data), "010203ff");
    assert_eq!(EncodingAlgo::Base64.encode(data), "AQID/w==");
    assert_eq!(EncodingAlgo::default(), EncodingAlgo::Hex);
    assert_eq!(serde_json::to_string(&EncodingAlgo::Hex).unwrap(), "\"Hex\"");
    assert_eq!(serde_json::to_string(&EncodingAlgo::Base64).unwrap(), "\"Base64\"");
    assert!(serde_json::from_str::<EncodingAlgo>("\"Binary\"").is_err());
}
