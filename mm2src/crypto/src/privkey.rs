/******************************************************************************
 * Copyright © 2014-2018 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/

use crate::global_hd_ctx::Bip39Seed;
use bip32::Error as Bip32Error;
use bip39::Error as Bip39Error;
use ed25519_dalek_bip32::{DerivationPath as Ed25519DerivationPath, Error as Ed25519Bip32Error};
use kdf_crypto::{sha256, ChecksumType};
use keys::{Error as KeysError, KeyPair, Private, Secret as Secp256k1Secret};
use mm2_err_handle::prelude::*;
use primitives::hash::H160;
use rustc_hex::FromHexError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub type PrivKeyResult<T> = Result<T, MmError<PrivKeyError>>;

/// Errors related to private key operations and HD key derivation.
#[derive(Debug, Error)]
pub enum PrivKeyError {
    #[error("bip39_seed_from_passphrase: Error parsing passphrase: {0}")]
    Bip39Parsing(#[from] Bip39Error),
    #[error("private_from_seed: Error parsing provided WIF: {0}")]
    WifSecp256k1Parsing(KeysError),
    #[error(
        "private_from_seed: Error parsing raw secp256k1 private key, expected 0x prefixed 32 byte hex string: {0}"
    )]
    RawSecp256k1Parsing(#[from] FromHexError),
    #[error("GlobalHDAccountCtx::new: Failed to calculate secp256k1 master xpriv from bip39 seed: {0}")]
    Secp256k1MasterKey(Bip32Error),
    #[error("GlobalHDAccountCtx::new: Failed to calculate ed25519 master xpriv from bip39 seed: {0}")]
    Ed25519MasterKey(Ed25519Bip32Error),
    #[error("GlobalHDAccountCtx::derive_ed25519_signing_key: Failed to derive key for path:{1} with error: {0}")]
    Ed25519DeriveKey(Ed25519Bip32Error, Ed25519DerivationPath),
    #[error("GlobalHDAccountCtx::new: Failed to derive internal secp256k1 private key: {0}")]
    Secp256k1InternalKey(Bip32Error),
    #[error("key_pair_from_secret: Failed to create KeyPair from byte array {0}")]
    KeyPairFromSecret(KeysError),
    #[error("key_pair_from_seed: Expected compressed public key, found uncompressed")]
    ExpectedCompressedKeys,
    #[error("key_pair_from_seed: Failed to create KeyPair from Private {0}")]
    PrivateIntoKeyPair(KeysError),
    #[error("shared_db_id_from_seed: passphrase must not be empty")]
    EmptyPassphrase,
}

fn private_from_seed(seed: &str) -> PrivKeyResult<Private> {
    // Attempt to parse the seed as a WIF
    match seed.parse() {
        Ok(private) => return Ok(private),
        Err(e) => {
            if let KeysError::InvalidChecksum = e {
                return MmError::err(PrivKeyError::WifSecp256k1Parsing(e));
            }
        }, // else ignore other errors, assume the passphrase is not WIF
    }

    // If the seed starts with 0x, treat it as hex-encoded secp256k1 private key
    match seed.strip_prefix("0x") {
        Some(stripped) => {
            let hash: Secp256k1Secret = stripped.parse()?;
            Ok(Private {
                prefix: 0,
                secret: hash,
                compressed: true,
                checksum_type: ChecksumType::DSHA256,
            })
        },
        None => Ok(private_from_seed_hash(seed)),
    }
}

/// Derives a secp256k1 private key from a passphrase by SHA-256 hashing it
/// and clamping bits for EC validity.
pub(crate) fn private_from_seed_hash(seed: &str) -> Private {
    let hash = sha256(seed.as_bytes());
    Private {
        prefix: 0,
        secret: secp_privkey_from_hash(hash),
        compressed: true,
        checksum_type: ChecksumType::DSHA256,
    }
}

/// Mutates an arbitrary hash to become a valid secp256k1 private key.
pub fn secp_privkey_from_hash(mut hash: Secp256k1Secret) -> Secp256k1Secret {
    hash[0] &= 248;
    hash[31] &= 127;
    hash[31] |= 64;
    hash
}

pub fn key_pair_from_seed(seed: &str) -> PrivKeyResult<KeyPair> {
    let private = private_from_seed(seed)?;
    if !private.compressed {
        return MmError::err(PrivKeyError::ExpectedCompressedKeys);
    }
    let pair = KeyPair::from_private(private).map_err(PrivKeyError::PrivateIntoKeyPair)?;
    // Sanity check: compressed public keys are always 33 bytes.
    assert_eq!(pair.public().len(), 33);
    Ok(pair)
}

/// Creates a KeyPair from a secret byte slice (must be exactly 32 bytes).
pub fn key_pair_from_secret(secret: &[u8]) -> PrivKeyResult<KeyPair> {
    let private = Private {
        prefix: 0,
        secret: secret.into(),
        compressed: true,
        checksum_type: ChecksumType::DSHA256,
    };
    Ok(KeyPair::from_private(private).map_err(PrivKeyError::KeyPairFromSecret)?)
}

/// Reloaded-chosen namespace salt for the process-level shared-database
/// identifier derivation (R18). This names only a process-local on-disk database
/// namespace; it is NOT part of any third-party interop contract and is
/// deliberately a fresh reloaded-branded constant (it does not reproduce any
/// upstream salt expression).
const SHARED_DB_ID_SALT: &str = "kdf-reloaded-shared-db-id-namespace-v1";

/// Derives the 20-byte process-level shared-database identifier from the active
/// seed passphrase (R18): strip a leading `0x` prefix, combine the passphrase
/// with [`SHARED_DB_ID_SALT`], hash the combined value into a secp256k1 private
/// key, form a key-pair, and take its `RIPEMD160(SHA256(public_key))` address
/// hash. Rejects an empty passphrase.
pub fn shared_db_id_from_seed(passphrase: &str) -> PrivKeyResult<H160> {
    if passphrase.is_empty() {
        return MmError::err(PrivKeyError::EmptyPassphrase);
    }
    let stripped = passphrase.strip_prefix("0x").unwrap_or(passphrase);
    let salted = format!("{SHARED_DB_ID_SALT}{stripped}");
    let secret = secp_privkey_from_hash(sha256(salted.as_bytes()));
    let key_pair = key_pair_from_secret(secret.as_slice())?;
    Ok(key_pair.public().address_hash())
}

/// Derives a 64-byte BIP39 seed from a mnemonic phrase.
pub fn bip39_seed_from_mnemonic(mnemonic_str: &str) -> PrivKeyResult<Bip39Seed> {
    let mnemonic = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, mnemonic_str)?;
    let seed = mnemonic.to_seed_normalized("");
    Ok(Bip39Seed(seed))
}

#[derive(Clone, Copy, Debug)]
pub struct SerializableSecp256k1Keypair {
    inner: KeyPair,
}

impl PartialEq for SerializableSecp256k1Keypair {
    fn eq(&self, other: &Self) -> bool { self.inner.public() == other.inner.public() }
}

impl Eq for SerializableSecp256k1Keypair {}

impl SerializableSecp256k1Keypair {
    pub fn new(key: [u8; 32]) -> PrivKeyResult<Self> {
        Ok(SerializableSecp256k1Keypair {
            inner: key_pair_from_secret(&key)?,
        })
    }

    pub fn key_pair(&self) -> &KeyPair { &self.inner }

    pub fn public_slice(&self) -> &[u8] { self.inner.public_slice() }

    pub fn priv_key(&self) -> [u8; 32] { self.inner.private().secret.take() }

    pub fn random() -> Self {
        SerializableSecp256k1Keypair {
            inner: KeyPair::random_compressed(),
        }
    }

    pub fn into_inner(self) -> KeyPair { self.inner }
}

impl From<KeyPair> for SerializableSecp256k1Keypair {
    fn from(inner: KeyPair) -> Self { SerializableSecp256k1Keypair { inner } }
}

impl Serialize for SerializableSecp256k1Keypair {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.priv_key().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SerializableSecp256k1Keypair {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let priv_key = <[u8; 32]>::deserialize(deserializer)?;
        SerializableSecp256k1Keypair::new(priv_key).map_err(serde::de::Error::custom)
    }
}

#[test]
fn serializable_secp256k1_keypair_test() {
    use serde_json::{self as json};

    let key_pair = KeyPair::random_compressed();
    let serializable = SerializableSecp256k1Keypair { inner: key_pair };
    let serialized = json::to_string(&serializable).unwrap();
    println!("{serialized}");
    let deserialized = json::from_str(&serialized).unwrap();
    assert_eq!(serializable, deserialized);

    let invalid_privkey: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xba, 0xae,
        0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
    ];
    let invalid_privkey_serialized = json::to_string(&invalid_privkey).unwrap();
    let err = json::from_str::<SerializableSecp256k1Keypair>(&invalid_privkey_serialized).unwrap_err();
    println!("{err}");
}

#[test]
fn shared_db_id_from_seed_rejects_empty_passphrase() {
    let err = shared_db_id_from_seed("").unwrap_err();
    assert!(matches!(err.into_inner(), PrivKeyError::EmptyPassphrase));
}

#[test]
fn shared_db_id_from_seed_is_deterministic_and_seed_specific() {
    let a = shared_db_id_from_seed("my secret seed phrase").unwrap();
    let b = shared_db_id_from_seed("my secret seed phrase").unwrap();
    assert_eq!(a, b);

    let other = shared_db_id_from_seed("a different seed phrase").unwrap();
    assert_ne!(a, other);

    // A leading `0x` prefix is stripped before derivation.
    let prefixed = shared_db_id_from_seed("0xdeadbeef").unwrap();
    let unprefixed = shared_db_id_from_seed("deadbeef").unwrap();
    assert_eq!(prefixed, unprefixed);
}
