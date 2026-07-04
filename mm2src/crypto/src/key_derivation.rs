/// Key derivation for the canonical wallet encryption envelope (Chapter 07 §7.7).
///
/// Two key-derivation variants are bound:
/// - **Argon2** (tag `Argon2`): memory-hard password-based KDF used for user
///   wallet records. The AES key and the HMAC key are produced by **two
///   independent Argon2id evaluations** of the password — one under `salt_aes`,
///   one under `salt_hmac` — each emitting `output_len` (32) bytes (R16).
/// - **SLIP0021** (tag `SLIP0021`): deterministic symmetric-key derivation from
///   a master seed, used by the higher-level mnemonic-from-seed bootstrap of
///   Chapter 05. It is NOT accepted as the key-derivation method for a
///   user-facing wallet record.
use crate::slip21;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use derive_more::Display;
use zeroize::Zeroize;

/// Salt Base64 *decode* engine: standard alphabet, padding-INDIFFERENT.
///
/// Interop: the canonical wallet record (and every record written by the
/// reference build) stores each Argon2 salt as an **unpadded** 16-byte
/// `SaltString` (22 chars, e.g. `Zy+pgtDwUkpJ0EZuedpFBQ`). A strict
/// padding-required decoder rejects that form, which previously made such
/// records undecryptable (surfacing as a spurious "invalid password"). This
/// engine accepts a salt whether it carries canonical padding or none, so both
/// canonical unpadded salts and any legacy padded salt are read back.
const SALT_B64_DECODE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Bound Argon2id parameters (R15). The whole object travels with every
/// persisted record so a future parameter raise stays backward-compatible — an
/// old record decrypts under its own embedded parameters.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Argon2Params {
    /// Argon2 algorithm name; bound value `argon2id`.
    pub algorithm: String,
    /// Argon2 version; bound value `19` (`0x13`).
    pub version: u32,
    /// Memory cost in KiB; bound value `65536` (64 MiB).
    pub m_cost: u32,
    /// Iteration (time) cost; bound value `2`.
    pub t_cost: u32,
    /// Degree of parallelism; bound value `1`.
    pub p_cost: u32,
    /// Derived-key length in bytes; bound value `32`.
    pub output_len: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Argon2Params {
            algorithm: "argon2id".to_string(),
            version: 19,
            m_cost: 65536,
            t_cost: 2,
            p_cost: 1,
            output_len: 32,
        }
    }
}

/// Specifies how the symmetric keys were derived (R15). The enum is
/// **externally tagged** — the variant name (`Argon2` / `SLIP0021`) is the JSON
/// key wrapping the variant's fields — which is the interop contract: records
/// written by the original build are read back, and vice-versa.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum KeyDerivationDetails {
    /// Password-derived. Carries the Argon2 parameters plus two independent,
    /// Base64-encoded salts (`salt_aes`, `salt_hmac`).
    Argon2 {
        params: Argon2Params,
        salt_aes: String,
        salt_hmac: String,
    },
    /// Seed-derived (SLIP-0021). Used by the Chapter 05 bootstrap only; it is
    /// explicitly NOT accepted as the KDF for a user mnemonic record.
    #[serde(rename = "SLIP0021")]
    SLIP0021 {
        encryption_path: String,
        authentication_path: String,
    },
}

/// Errors during key derivation.
#[derive(Debug, Display)]
pub enum KeyDerivationError {
    #[display(fmt = "Argon2 key derivation failed: {}", _0)]
    Argon2Error(String),
    #[display(fmt = "SLIP-0021 key derivation failed: {}", _0)]
    Slip0021Error(String),
    #[display(fmt = "Invalid Base64 in key-derivation details: {}", _0)]
    InvalidBase64(String),
    #[display(fmt = "Unsupported key-derivation parameter: {}", _0)]
    UnsupportedParam(String),
}

/// A pair of derived keys: one for encryption and one for HMAC authentication.
/// Both keys are zeroized on drop.
pub struct DerivedKeys {
    pub encryption_key: [u8; 32],
    pub hmac_key: [u8; 32],
}

impl Drop for DerivedKeys {
    fn drop(&mut self) {
        self.encryption_key.zeroize();
        self.hmac_key.zeroize();
    }
}

/// Derives the AES encryption key and the HMAC authentication key for the
/// canonical envelope from `password_or_seed` and the supplied derivation
/// details.
///
/// For the `Argon2` variant the AES key and the HMAC key are produced by two
/// independent Argon2id evaluations (one per salt); a single salt or derivation
/// output is NEVER reused for both keys (R16).
pub fn derive_keys_for_mnemonic(
    password_or_seed: &[u8],
    details: &KeyDerivationDetails,
) -> Result<DerivedKeys, KeyDerivationError> {
    match details {
        KeyDerivationDetails::Argon2 {
            params,
            salt_aes,
            salt_hmac,
        } => {
            let salt_aes = SALT_B64_DECODE
                .decode(salt_aes)
                .map_err(|e| KeyDerivationError::InvalidBase64(format!("salt_aes: {e}")))?;
            let salt_hmac = SALT_B64_DECODE
                .decode(salt_hmac)
                .map_err(|e| KeyDerivationError::InvalidBase64(format!("salt_hmac: {e}")))?;
            let encryption_key = derive_argon2_key(password_or_seed, params, &salt_aes)?;
            let hmac_key = derive_argon2_key(password_or_seed, params, &salt_hmac)?;
            Ok(DerivedKeys {
                encryption_key,
                hmac_key,
            })
        },
        KeyDerivationDetails::SLIP0021 { .. } => derive_keys_slip0021(password_or_seed),
    }
}

/// Runs a single Argon2id evaluation emitting a 32-byte key under `salt`.
fn derive_argon2_key(password: &[u8], params: &Argon2Params, salt: &[u8]) -> Result<[u8; 32], KeyDerivationError> {
    use argon2::{Algorithm, Argon2, Params, Version};

    if !params.algorithm.eq_ignore_ascii_case("argon2id") {
        return Err(KeyDerivationError::UnsupportedParam(format!(
            "algorithm {}",
            params.algorithm
        )));
    }
    if params.output_len != 32 {
        return Err(KeyDerivationError::UnsupportedParam(format!(
            "output_len {} (only 32 is supported)",
            params.output_len
        )));
    }
    let version = match params.version {
        0x10 => Version::V0x10,
        0x13 => Version::V0x13,
        other => {
            return Err(KeyDerivationError::UnsupportedParam(format!("version {other}")));
        },
    };

    let argon2_params = Params::new(
        params.m_cost,
        params.t_cost,
        params.p_cost,
        Some(params.output_len as usize),
    )
    .map_err(|e| KeyDerivationError::Argon2Error(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, version, argon2_params);

    let mut output = [0u8; 32];
    argon2
        .hash_password_into(password, salt, &mut output)
        .map_err(|e| KeyDerivationError::Argon2Error(e.to_string()))?;
    Ok(output)
}

/// Derives keys using SLIP-0021 deterministic symmetric-key derivation.
fn derive_keys_slip0021(seed: &[u8]) -> Result<DerivedKeys, KeyDerivationError> {
    let encryption_key = slip21::derive_key_from_path(seed, &slip21::ENCRYPTION_KEY_PATH)
        .map_err(|e| KeyDerivationError::Slip0021Error(e.to_string()))?;
    let hmac_key = slip21::derive_key_from_path(seed, &slip21::AUTHENTICATION_KEY_PATH)
        .map_err(|e| KeyDerivationError::Slip0021Error(e.to_string()))?;

    Ok(DerivedKeys {
        encryption_key,
        hmac_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::{STANDARD as BASE64, STANDARD_NO_PAD};

    fn test_params() -> Argon2Params {
        Argon2Params {
            algorithm: "argon2id".to_string(),
            version: 19,
            m_cost: 1024, // Low for tests
            t_cost: 1,
            p_cost: 1,
            output_len: 32,
        }
    }

    #[test]
    fn test_argon2_two_salts_produce_distinct_keys() {
        let password = b"test password";
        let details = KeyDerivationDetails::Argon2 {
            params: test_params(),
            salt_aes: BASE64.encode(b"salt_aes_value_0"),
            salt_hmac: BASE64.encode(b"salt_hmac_value0"),
        };

        let keys = derive_keys_for_mnemonic(password, &details).expect("Argon2 derivation should succeed");

        assert_ne!(keys.encryption_key, keys.hmac_key);
        assert_ne!(keys.encryption_key, [0u8; 32]);
        assert_ne!(keys.hmac_key, [0u8; 32]);
    }

    #[test]
    fn test_argon2_deterministic() {
        let password = b"test password";
        let details = KeyDerivationDetails::Argon2 {
            params: test_params(),
            salt_aes: BASE64.encode(b"saltsalt1"),
            salt_hmac: BASE64.encode(b"saltsalt2"),
        };

        let keys1 = derive_keys_for_mnemonic(password, &details).unwrap();
        let keys2 = derive_keys_for_mnemonic(password, &details).unwrap();

        assert_eq!(keys1.encryption_key, keys2.encryption_key);
        assert_eq!(keys1.hmac_key, keys2.hmac_key);
    }

    #[test]
    fn test_argon2_same_salt_yields_equal_keys() {
        // Sanity: deriving with the same salt twice yields the same key, which is
        // exactly why two independent salts are required for key separation (R16).
        let password = b"test password";
        let same = BASE64.encode(b"identical_salt00");
        let details = KeyDerivationDetails::Argon2 {
            params: test_params(),
            salt_aes: same.clone(),
            salt_hmac: same,
        };
        let keys = derive_keys_for_mnemonic(password, &details).unwrap();
        assert_eq!(keys.encryption_key, keys.hmac_key);
    }

    #[test]
    fn test_argon2_salt_padding_indifferent() {
        // Interop regression: the canonical record stores each salt as an
        // UNPADDED 16-byte Base64 string (`SaltString`, e.g. 22 chars). A padded
        // form of the same bytes must derive the identical key, proving the read
        // path no longer rejects canonical unpadded salts (which previously
        // surfaced as a spurious "invalid password").
        let password = b"test password";
        let salt_bytes = b"sixteen_byte_salt"; // 17 bytes; any length is fine for Argon2
        let unpadded = STANDARD_NO_PAD.encode(salt_bytes);
        let padded = BASE64.encode(salt_bytes);
        assert!(!unpadded.contains('='), "unpadded salt must carry no padding");

        let details_unpadded = KeyDerivationDetails::Argon2 {
            params: test_params(),
            salt_aes: unpadded.clone(),
            salt_hmac: unpadded,
        };
        let details_padded = KeyDerivationDetails::Argon2 {
            params: test_params(),
            salt_aes: padded.clone(),
            salt_hmac: padded,
        };

        let keys_unpadded =
            derive_keys_for_mnemonic(password, &details_unpadded).expect("unpadded (canonical) salt must derive");
        let keys_padded = derive_keys_for_mnemonic(password, &details_padded).expect("padded salt must derive");

        assert_eq!(keys_unpadded.encryption_key, keys_padded.encryption_key);
        assert_eq!(keys_unpadded.hmac_key, keys_padded.hmac_key);
    }

    #[test]
    fn test_slip0021_key_derivation() {
        let seed = [0xABu8; 64];
        let details = KeyDerivationDetails::SLIP0021 {
            encryption_path: "SLIP-0021/Encryption key".to_string(),
            authentication_path: "SLIP-0021/Authentication key".to_string(),
        };

        let keys = derive_keys_for_mnemonic(&seed, &details).expect("SLIP-0021 derivation should succeed");

        assert_ne!(keys.encryption_key, keys.hmac_key);
        assert_ne!(keys.encryption_key, [0u8; 32]);
    }
}
