/// BIP39 mnemonic generation, encryption, and decryption — canonical wallet
/// encryption envelope (Chapter 07 §7.7).
///
/// The persisted record ([`EncryptedMnemonicData`]) is a single JSON object with
/// six top-level fields: `version`, `encryption_algorithm`,
/// `key_derivation_details`, and the Base64-encoded `iv`, `ciphertext` and
/// `tag`. Encryption is AES-256-CBC (PKCS-7) under a 32-byte key; authentication
/// is HMAC-SHA-256 under a *separate* 32-byte key, computed over the byte
/// concatenation `ciphertext || iv` in encrypt-then-MAC order. Decryption
/// verifies the tag in constant time *before* any cipher operation.
use crate::key_derivation::{derive_keys_for_mnemonic, Argon2Params, KeyDerivationDetails, KeyDerivationError};
use aes::Aes256;
use base64::engine::general_purpose::{STANDARD as BASE64, STANDARD_NO_PAD};
use base64::Engine;
use bip39::Mnemonic;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use derive_more::Display;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;
type HmacSha256 = Hmac<Sha256>;

/// The bound format-version tag for newly written records (R12).
pub const ENVELOPE_VERSION: u32 = 1;

/// Errors related to mnemonic operations.
#[derive(Debug, Display)]
pub enum MnemonicError {
    #[display(fmt = "Failed to generate mnemonic: {}", _0)]
    GenerationError(String),
    #[display(fmt = "Invalid mnemonic: {}", _0)]
    InvalidMnemonic(String),
    #[display(fmt = "Key derivation failed: {}", _0)]
    KeyDerivationFailed(KeyDerivationError),
    #[display(fmt = "Encryption failed: {}", _0)]
    EncryptionError(String),
    #[display(fmt = "Decryption failed: {}", _0)]
    DecryptionError(String),
}

impl From<KeyDerivationError> for MnemonicError {
    fn from(e: KeyDerivationError) -> Self { MnemonicError::KeyDerivationFailed(e) }
}

/// The symmetric cipher used to protect the mnemonic at rest (R12). Serializes
/// as a plain string token; the bound value is `AES256CBC`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum EncryptionAlgorithm {
    #[serde(rename = "AES256CBC")]
    Aes256Cbc,
}

/// The canonical, interoperable encrypted-mnemonic record (R12). Six top-level
/// fields; `iv`, `ciphertext` and `tag` are Base64-encoded strings (standard
/// alphabet), NOT nested objects and NOT raw byte arrays.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EncryptedMnemonicData {
    /// Format-version tag; current value `1`.
    pub version: u32,
    /// Symmetric cipher token; bound value `AES256CBC`.
    pub encryption_algorithm: EncryptionAlgorithm,
    /// How the symmetric keys were derived.
    pub key_derivation_details: KeyDerivationDetails,
    /// AES initialization vector, Base64-encoded.
    pub iv: String,
    /// AES ciphertext, Base64-encoded.
    pub ciphertext: String,
    /// HMAC-SHA-256 authentication tag, Base64-encoded.
    pub tag: String,
}

/// Generates a new BIP39 mnemonic phrase.
///
/// # Arguments
/// * `word_count` - Number of words (12, 15, 18, 21, or 24). Must correspond to
///   valid BIP39 entropy sizes (128, 160, 192, 224, or 256 bits).
pub fn generate_mnemonic(word_count: usize) -> Result<Mnemonic, MnemonicError> {
    let entropy_bits = word_count_to_entropy_bits(word_count)?;
    let mut entropy = vec![0u8; entropy_bits / 8];
    common::os_rng(&mut entropy).map_err(|e| MnemonicError::GenerationError(format!("RNG error: {e}")))?;
    Mnemonic::from_entropy(&entropy).map_err(|e| MnemonicError::GenerationError(e.to_string()))
}

/// Encrypts a mnemonic phrase using a password with Argon2id key derivation,
/// producing the canonical §7.7 envelope.
///
/// # Security
/// Uses two independent Argon2id evaluations (one per salt) to derive the AES
/// key and the HMAC key; both salts and the IV are drawn fresh per encryption.
pub fn encrypt_mnemonic(mnemonic_str: &str, password: &str) -> Result<EncryptedMnemonicData, MnemonicError> {
    // Validate the mnemonic first (R17).
    let _ = Mnemonic::parse_in_normalized(bip39::Language::English, mnemonic_str)
        .map_err(|e| MnemonicError::InvalidMnemonic(e.to_string()))?;

    // Two independent, fresh 16-byte salts (R16) and the bound default Argon2
    // params (R15). Salts are stored as UNPADDED Base64 (the canonical
    // `SaltString` form, 22 chars) so records written here are byte-compatible
    // with the reference build and decrypt cleanly on read.
    let mut salt_aes = [0u8; 16];
    let mut salt_hmac = [0u8; 16];
    common::os_rng(&mut salt_aes).map_err(|e| MnemonicError::EncryptionError(format!("RNG error: {e}")))?;
    common::os_rng(&mut salt_hmac).map_err(|e| MnemonicError::EncryptionError(format!("RNG error: {e}")))?;

    let details = KeyDerivationDetails::Argon2 {
        params: Argon2Params::default(),
        salt_aes: STANDARD_NO_PAD.encode(salt_aes),
        salt_hmac: STANDARD_NO_PAD.encode(salt_hmac),
    };

    let keys = derive_keys_for_mnemonic(password.as_bytes(), &details)?;

    // Fresh random 16-byte IV (R13).
    let mut iv = [0u8; 16];
    common::os_rng(&mut iv).map_err(|e| MnemonicError::EncryptionError(format!("RNG error: {e}")))?;

    // AES-256-CBC + PKCS-7.
    let ciphertext = Aes256CbcEnc::new(&keys.encryption_key.into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(mnemonic_str.as_bytes());

    // Encrypt-then-MAC over `ciphertext || iv` (R14).
    let tag = hmac_tag(&keys.hmac_key, &ciphertext, &iv);

    Ok(EncryptedMnemonicData {
        version: ENVELOPE_VERSION,
        encryption_algorithm: EncryptionAlgorithm::Aes256Cbc,
        key_derivation_details: details,
        iv: BASE64.encode(iv),
        ciphertext: BASE64.encode(&ciphertext),
        tag: BASE64.encode(tag),
    })
}

/// Decrypts a mnemonic phrase that was encrypted with [`encrypt_mnemonic`].
///
/// Verifies the HMAC tag in constant time over `ciphertext || iv` *before* any
/// cipher operation (R14). A tag mismatch (wrong password or tampered record)
/// surfaces as a clean decryption failure, never a usable-but-corrupt plaintext.
pub fn decrypt_mnemonic(encrypted: &EncryptedMnemonicData, password: &str) -> Result<String, MnemonicError> {
    let iv = BASE64
        .decode(&encrypted.iv)
        .map_err(|e| MnemonicError::DecryptionError(format!("invalid Base64 iv: {e}")))?;
    let ciphertext = BASE64
        .decode(&encrypted.ciphertext)
        .map_err(|e| MnemonicError::DecryptionError(format!("invalid Base64 ciphertext: {e}")))?;
    let tag = BASE64
        .decode(&encrypted.tag)
        .map_err(|e| MnemonicError::DecryptionError(format!("invalid Base64 tag: {e}")))?;

    let keys = derive_keys_for_mnemonic(password.as_bytes(), &encrypted.key_derivation_details)?;

    // Step 1: verify the tag in constant time BEFORE touching the cipher (R14).
    let mut mac =
        HmacSha256::new_from_slice(&keys.hmac_key).expect("HMAC-SHA256 accepts any key length, 32 bytes is valid");
    mac.update(&ciphertext);
    mac.update(&iv);
    mac.verify_slice(&tag)
        .map_err(|_| MnemonicError::DecryptionError("HMAC verification failed".to_string()))?;

    // Step 2: extract the 16-byte IV and decrypt.
    let iv: [u8; 16] = iv
        .as_slice()
        .try_into()
        .map_err(|_| MnemonicError::DecryptionError(format!("invalid IV length: {}", iv.len())))?;
    let decrypted = Aes256CbcDec::new(&keys.encryption_key.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&ciphertext)
        .map_err(|_| MnemonicError::DecryptionError("invalid padding or corrupted ciphertext".to_string()))?;

    String::from_utf8(decrypted)
        .map_err(|e| MnemonicError::DecryptionError(format!("Invalid UTF-8 in decrypted mnemonic: {e}")))
}

/// Computes the HMAC-SHA-256 tag over `ciphertext || iv`.
fn hmac_tag(hmac_key: &[u8; 32], ciphertext: &[u8], iv: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(hmac_key).expect("HMAC-SHA256 accepts any key length, 32 bytes is always valid");
    mac.update(ciphertext);
    mac.update(iv);
    mac.finalize().into_bytes().to_vec()
}

/// Converts BIP39 word count to required entropy bit count.
fn word_count_to_entropy_bits(word_count: usize) -> Result<usize, MnemonicError> {
    match word_count {
        12 => Ok(128),
        15 => Ok(160),
        18 => Ok(192),
        21 => Ok(224),
        24 => Ok(256),
        _ => Err(MnemonicError::GenerationError(format!(
            "Invalid word count: {word_count}. Must be 12, 15, 18, 21, or 24"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_generate_mnemonic_12_words() {
        let mnemonic = generate_mnemonic(12).expect("should generate 12-word mnemonic");
        assert_eq!(mnemonic.word_count(), 12);
    }

    #[test]
    fn test_generate_mnemonic_24_words() {
        let mnemonic = generate_mnemonic(24).expect("should generate 24-word mnemonic");
        assert_eq!(mnemonic.word_count(), 24);
    }

    #[test]
    fn test_generate_mnemonic_invalid_count() {
        assert!(generate_mnemonic(13).is_err());
    }

    #[test]
    fn test_encrypt_decrypt_mnemonic_roundtrip() {
        let password = "test_password_123";
        let encrypted = encrypt_mnemonic(TEST_MNEMONIC, password).expect("encryption should succeed");
        let decrypted = decrypt_mnemonic(&encrypted, password).expect("decryption should succeed");
        assert_eq!(decrypted, TEST_MNEMONIC);
    }

    /// Interop regression: fresh records store each salt as the canonical
    /// UNPADDED 16-byte Base64 string (22 chars, no `=`), and re-padding those
    /// salts must still decrypt — proving canonical write + padding-indifferent
    /// read on the real mnemonic path (the cause of the prior spurious
    /// "invalid password" when opening reference-build wallets).
    #[test]
    fn test_salts_are_canonical_unpadded_and_decrypt_when_repadded() {
        use crate::key_derivation::KeyDerivationDetails;

        let encrypted = encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        let (salt_aes, salt_hmac) = match &encrypted.key_derivation_details {
            KeyDerivationDetails::Argon2 {
                salt_aes, salt_hmac, ..
            } => (salt_aes.clone(), salt_hmac.clone()),
            _ => panic!("expected Argon2 key derivation"),
        };

        // Canonical form: 16-byte salt -> 22 chars, no padding.
        assert_eq!(salt_aes.len(), 22, "16-byte salt encodes to 22 unpadded chars");
        assert_eq!(salt_hmac.len(), 22);
        assert!(!salt_aes.contains('='), "salt must be unpadded");
        assert!(!salt_hmac.contains('='), "salt must be unpadded");
        assert_eq!(STANDARD_NO_PAD.decode(&salt_aes).unwrap().len(), 16);

        // Re-pad the salts to the legacy padded form; decryption must still work.
        let mut repadded = encrypted.clone();
        repadded.key_derivation_details = KeyDerivationDetails::Argon2 {
            params: Argon2Params::default(),
            salt_aes: BASE64.encode(STANDARD_NO_PAD.decode(&salt_aes).unwrap()),
            salt_hmac: BASE64.encode(STANDARD_NO_PAD.decode(&salt_hmac).unwrap()),
        };
        let decrypted = decrypt_mnemonic(&repadded, "pw").expect("padded salts must still decrypt");
        assert_eq!(decrypted, TEST_MNEMONIC);
    }

    /// The serialized JSON MUST be the six-field, top-level, Base64-string
    /// layout of R12 with the bound tokens and Argon2 params of R15.
    #[test]
    fn test_envelope_shape_is_bound_six_field_layout() {
        let encrypted = encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        let value = serde_json::to_value(&encrypted).unwrap();
        let obj = value.as_object().unwrap();

        // Exactly the six bound top-level keys.
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec![
            "ciphertext",
            "encryption_algorithm",
            "iv",
            "key_derivation_details",
            "tag",
            "version"
        ]);

        assert_eq!(obj["version"], serde_json::json!(1));
        assert_eq!(obj["encryption_algorithm"], serde_json::json!("AES256CBC"));
        // iv/ciphertext/tag are Base64 strings, not arrays/objects.
        assert!(obj["iv"].is_string());
        assert!(obj["ciphertext"].is_string());
        assert!(obj["tag"].is_string());
        assert!(BASE64.decode(obj["iv"].as_str().unwrap()).is_ok());
        assert!(BASE64.decode(obj["ciphertext"].as_str().unwrap()).is_ok());
        assert!(BASE64.decode(obj["tag"].as_str().unwrap()).is_ok());

        // `key_derivation_details` is externally tagged: the variant name `Argon2`
        // is the JSON key wrapping the variant fields (interop contract, R15).
        let kdd = &obj["key_derivation_details"];
        assert!(kdd.get("type").is_none());
        let argon2 = &kdd["Argon2"];
        assert!(argon2.is_object());
        let params = &argon2["params"];
        assert_eq!(params["algorithm"], serde_json::json!("argon2id"));
        assert_eq!(params["version"], serde_json::json!(19));
        assert_eq!(params["m_cost"], serde_json::json!(65536));
        assert_eq!(params["t_cost"], serde_json::json!(2));
        assert_eq!(params["p_cost"], serde_json::json!(1));
        assert_eq!(params["output_len"], serde_json::json!(32));
        // Two distinct salts.
        let salt_aes = argon2["salt_aes"].as_str().unwrap();
        let salt_hmac = argon2["salt_hmac"].as_str().unwrap();
        assert!(!salt_aes.is_empty());
        assert!(!salt_hmac.is_empty());
        assert_ne!(salt_aes, salt_hmac);
    }

    #[test]
    fn test_decrypt_with_wrong_password_fails() {
        let encrypted = encrypt_mnemonic(TEST_MNEMONIC, "correct_password").expect("encryption should succeed");
        assert!(decrypt_mnemonic(&encrypted, "wrong_password").is_err());
    }

    #[test]
    fn test_encrypt_invalid_mnemonic_fails() {
        assert!(encrypt_mnemonic("not a valid mnemonic phrase at all", "password").is_err());
    }

    #[test]
    fn test_tamper_ciphertext_iv_tag_each_fails() {
        // Flipping a byte in ciphertext, iv, or tag MUST each fail authentication
        // cleanly (never a UTF-8 error, never a panic).
        let flip_first_b64_byte = |s: &str| -> String {
            let mut bytes = BASE64.decode(s).unwrap();
            bytes[0] ^= 0xff;
            BASE64.encode(bytes)
        };

        let base = encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();

        let mut tampered_ct = base.clone();
        tampered_ct.ciphertext = flip_first_b64_byte(&base.ciphertext);
        assert!(decrypt_mnemonic(&tampered_ct, "pw").is_err());

        let mut tampered_iv = base.clone();
        tampered_iv.iv = flip_first_b64_byte(&base.iv);
        assert!(decrypt_mnemonic(&tampered_iv, "pw").is_err());

        let mut tampered_tag = base.clone();
        tampered_tag.tag = flip_first_b64_byte(&base.tag);
        assert!(decrypt_mnemonic(&tampered_tag, "pw").is_err());
    }

    /// An old record decrypts under its own embedded parameters (R15): a record
    /// serialized to JSON deserializes and decrypts back to the original.
    #[test]
    fn test_envelope_json_roundtrip() {
        let encrypted = encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let parsed: EncryptedMnemonicData = serde_json::from_str(&json).unwrap();
        let decrypted = decrypt_mnemonic(&parsed, "pw").unwrap();
        assert_eq!(decrypted, TEST_MNEMONIC);
    }
}
