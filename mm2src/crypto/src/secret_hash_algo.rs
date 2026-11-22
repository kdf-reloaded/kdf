/// Secret hashing algorithm selection for HTLC (Hash Time Locked Contract) swaps.
///
/// Atomic swaps use hash locks where the secret must be hashed to create the lock.
/// Different chains may require different hash algorithms for their scripts.
use kdf_crypto::{dhash160, sha256};

/// Available hash algorithms for creating HTLC secret hashes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum SecretHashAlgo {
    /// RIPEMD160(SHA256(secret)) — traditional Bitcoin-style double hash.
    /// Produces a 20-byte hash. Used by most UTXO coins.
    DHASH160,
    /// SHA256(secret) — single SHA-256 hash.
    /// Produces a 32-byte hash. Used by some newer protocols and EVM chains.
    SHA256,
}

impl SecretHashAlgo {
    /// Hashes the given secret using the selected algorithm.
    pub fn hash_secret(&self, secret: &[u8]) -> Vec<u8> {
        match self {
            SecretHashAlgo::DHASH160 => dhash160(secret).take().to_vec(),
            SecretHashAlgo::SHA256 => sha256(secret).take().to_vec(),
        }
    }
}

/// The length of a swap secret in bytes.
pub const SWAP_SECRET_LEN: usize = 32;

/// Whether `secret` is the secret of `secret_hash` (CRD ch.51 §51.6E).
///
/// The secret must be 32 bytes. A 20-byte hash is compared with `dhash160(secret)` and a 32-byte hash with
/// `sha256(secret)`.
///
/// # Errors
///
/// A hash of any other width is an error (CRD ch.51 R65, R71).
pub fn secret_matches_hash(secret: &[u8], secret_hash: &[u8]) -> Result<bool, String> {
    let algo = match secret_hash.len() {
        20 => SecretHashAlgo::DHASH160,
        32 => SecretHashAlgo::SHA256,
        other => {
            return Err(format!(
                "Unsupported secret hash width: {} bytes, expected 20 or 32",
                other
            ))
        },
    };
    Ok(secret.len() == SWAP_SECRET_LEN && algo.hash_secret(secret) == secret_hash)
}

/// [`secret_matches_hash`] as a check on an extracted value: the value is returned when it is the secret of
/// `secret_hash`.
///
/// # Errors
///
/// A value that is not the secret of the hash, or a hash of an unsupported width.
pub fn ensure_secret_matches_hash(secret: Vec<u8>, secret_hash: &[u8]) -> Result<Vec<u8>, String> {
    if secret_matches_hash(&secret, secret_hash)? {
        Ok(secret)
    } else {
        Err(format!(
            "The extracted value of {} bytes is not the secret of the secret hash {}",
            secret.len(),
            hex::encode(secret_hash)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_matches_a_20_byte_hash() {
        let secret = [7u8; 32];
        let hash = SecretHashAlgo::DHASH160.hash_secret(&secret);
        assert_eq!(secret_matches_hash(&secret, &hash), Ok(true));
        assert_eq!(secret_matches_hash(&[8u8; 32], &hash), Ok(false));
    }

    #[test]
    fn secret_matches_a_32_byte_hash() {
        let secret = [7u8; 32];
        let hash = SecretHashAlgo::SHA256.hash_secret(&secret);
        assert_eq!(secret_matches_hash(&secret, &hash), Ok(true));
        assert_eq!(secret_matches_hash(&[8u8; 32], &hash), Ok(false));
    }

    #[test]
    fn secret_of_another_length_never_matches() {
        let secret = [7u8; 31];
        let hash = SecretHashAlgo::SHA256.hash_secret(&secret);
        assert_eq!(secret_matches_hash(&secret, &hash), Ok(false));
        let secret = [7u8; 33];
        let hash = SecretHashAlgo::DHASH160.hash_secret(&secret);
        assert_eq!(secret_matches_hash(&secret, &hash), Ok(false));
    }

    #[test]
    fn hash_of_another_width_is_an_error() {
        assert!(secret_matches_hash(&[7u8; 32], &[0u8; 19]).is_err());
        assert!(secret_matches_hash(&[7u8; 32], &[0u8; 33]).is_err());
        assert!(ensure_secret_matches_hash(vec![7u8; 32], &[]).is_err());
    }

    #[test]
    fn ensure_returns_the_value_only_when_it_matches() {
        let secret = vec![7u8; 32];
        let hash = SecretHashAlgo::SHA256.hash_secret(&secret);
        assert_eq!(ensure_secret_matches_hash(secret.clone(), &hash), Ok(secret));
        assert!(ensure_secret_matches_hash(vec![8u8; 32], &hash).is_err());
    }

    #[test]
    fn test_dhash160_output_length() {
        let hash = SecretHashAlgo::DHASH160.hash_secret(b"test secret");
        assert_eq!(hash.len(), 20);
    }

    #[test]
    fn test_sha256_output_length() {
        let hash = SecretHashAlgo::SHA256.hash_secret(b"test secret");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_hash_deterministic() {
        let secret = b"atomic swap secret";
        let h1 = SecretHashAlgo::SHA256.hash_secret(secret);
        let h2 = SecretHashAlgo::SHA256.hash_secret(secret);
        assert_eq!(h1, h2);
    }
}
