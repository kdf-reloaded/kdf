//! Legacy wallet-record format — **read-only backward compatibility**.
//!
//! An earlier reloaded build persisted wallet files as `<dbdir>/wallets/<name>.wallet`
//! using a *two-field* envelope: an `encrypted_data` object holding three raw
//! byte vectors (`encrypted`, `iv`, `hmac`) plus a `key_derivation` object with
//! a single Argon2 salt. Key derivation used one Argon2id evaluation emitting
//! 64 bytes split into an AES key (first 32) and an HMAC key (last 32), and the
//! HMAC covered `iv || ciphertext`.
//!
//! This module retains exactly that scheme so the current node can still **read**
//! (list and decrypt) wallets written by the previous build. New wallet records
//! are ALWAYS written in the canonical §7.7 format (see [`crate::mnemonic`]);
//! nothing here is used for new writes except the test-only encrypt helper that
//! reconstructs a legacy record for the legacy-read regression tests.
use crate::decrypt::decrypt_data;
use crate::encrypt::{encrypt_data, EncryptedData};
use crate::mnemonic::MnemonicError;
use bip39::Mnemonic;
use zeroize::Zeroize;

/// Legacy Argon2 parameters (single derivation, 64-byte output split).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyArgon2Params {
    pub memory_cost_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

/// Legacy key-derivation details (single salt, tagged `type`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum LegacyKeyDerivationDetails {
    #[serde(rename = "argon2")]
    Argon2 { params: LegacyArgon2Params, salt: Vec<u8> },
    #[serde(rename = "slip0021")]
    SLIP0021,
}

/// Legacy two-field encrypted-mnemonic record.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyEncryptedMnemonicData {
    pub encrypted_data: EncryptedData,
    pub key_derivation: LegacyKeyDerivationDetails,
}

/// Derives the legacy AES + HMAC keys: one Argon2id evaluation emitting 64
/// bytes, split into the encryption key (first 32) and HMAC key (last 32).
fn legacy_derive_keys(
    password: &[u8],
    params: &LegacyArgon2Params,
    salt: &[u8],
) -> Result<([u8; 32], [u8; 32]), MnemonicError> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let argon2_params = Params::new(params.memory_cost_kib, params.iterations, params.parallelism, Some(64))
        .map_err(|e| MnemonicError::DecryptionError(format!("legacy argon2: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    let mut output = [0u8; 64];
    argon2
        .hash_password_into(password, salt, &mut output)
        .map_err(|e| MnemonicError::DecryptionError(format!("legacy argon2: {e}")))?;

    let mut encryption_key = [0u8; 32];
    let mut hmac_key = [0u8; 32];
    encryption_key.copy_from_slice(&output[..32]);
    hmac_key.copy_from_slice(&output[32..]);
    output.zeroize();
    Ok((encryption_key, hmac_key))
}

/// Decrypts a legacy `.wallet` record. Read-only backward compatibility.
pub fn legacy_decrypt_mnemonic(
    encrypted: &LegacyEncryptedMnemonicData,
    password: &str,
) -> Result<String, MnemonicError> {
    let (encryption_key, hmac_key) = match &encrypted.key_derivation {
        LegacyKeyDerivationDetails::Argon2 { params, salt } => legacy_derive_keys(password.as_bytes(), params, salt)?,
        LegacyKeyDerivationDetails::SLIP0021 => {
            return Err(MnemonicError::DecryptionError(
                "legacy SLIP0021 records are not supported".to_string(),
            ));
        },
    };

    let decrypted = decrypt_data(&encrypted.encrypted_data, &encryption_key, &hmac_key)
        .map_err(|e| MnemonicError::DecryptionError(e.to_string()))?;

    String::from_utf8(decrypted)
        .map_err(|e| MnemonicError::DecryptionError(format!("Invalid UTF-8 in decrypted mnemonic: {e}")))
}

/// Encrypts a mnemonic into the legacy two-field record. Retained only so the
/// legacy-read regression tests can reconstruct a record the previous build
/// would have written. New code MUST write the canonical §7.7 envelope instead.
pub fn legacy_encrypt_mnemonic(
    mnemonic_str: &str,
    password: &str,
) -> Result<LegacyEncryptedMnemonicData, MnemonicError> {
    let _ = Mnemonic::parse_in_normalized(bip39::Language::English, mnemonic_str)
        .map_err(|e| MnemonicError::InvalidMnemonic(e.to_string()))?;

    let mut salt = [0u8; 32];
    common::os_rng(&mut salt).map_err(|e| MnemonicError::EncryptionError(format!("RNG error: {e}")))?;

    let params = LegacyArgon2Params {
        memory_cost_kib: 65536,
        iterations: 3,
        parallelism: 1,
    };
    let (encryption_key, hmac_key) = legacy_derive_keys(password.as_bytes(), &params, &salt)
        .map_err(|e| MnemonicError::EncryptionError(e.to_string()))?;

    let mut iv = [0u8; 16];
    common::os_rng(&mut iv).map_err(|e| MnemonicError::EncryptionError(format!("RNG error: {e}")))?;

    let encrypted_data = encrypt_data(mnemonic_str.as_bytes(), &encryption_key, &iv, &hmac_key);

    Ok(LegacyEncryptedMnemonicData {
        encrypted_data,
        key_derivation: LegacyKeyDerivationDetails::Argon2 {
            params,
            salt: salt.to_vec(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_legacy_roundtrip() {
        let encrypted = legacy_encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        let decrypted = legacy_decrypt_mnemonic(&encrypted, "pw").unwrap();
        assert_eq!(decrypted, TEST_MNEMONIC);
    }

    #[test]
    fn test_legacy_wrong_password_fails() {
        let encrypted = legacy_encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        assert!(legacy_decrypt_mnemonic(&encrypted, "nope").is_err());
    }

    #[test]
    fn test_legacy_json_shape_is_two_field() {
        let encrypted = legacy_encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        let value = serde_json::to_value(&encrypted).unwrap();
        let obj = value.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["encrypted_data", "key_derivation"]);
        // encrypted_data holds raw byte arrays, not Base64 strings.
        assert!(obj["encrypted_data"]["encrypted"].is_array());
        assert!(obj["encrypted_data"]["iv"].is_array());
        assert!(obj["encrypted_data"]["hmac"].is_array());
    }
}
