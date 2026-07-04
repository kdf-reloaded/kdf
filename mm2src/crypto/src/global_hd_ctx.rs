/// Global HD Account context — the core HD wallet support for the crypto layer.
///
/// `GlobalHDAccountCtx` holds a BIP39 seed and the master keys derived from it:
/// - secp256k1 extended private key (for Bitcoin-like coins, via BIP32)
/// - ed25519 extended signing key (for Cosmos/Solana-like coins, via SLIP-0010)
///
/// This context is stored in `CryptoCtx` when the user initializes with a BIP39 mnemonic
/// (as opposed to a legacy Iguana passphrase).
use crate::privkey::{bip39_seed_from_mnemonic, key_pair_from_secret, PrivKeyError};
use bip32::ExtendedPrivateKey;
use common::drop_mutability;
use ed25519_dalek_bip32::{DerivationPath as Ed25519DerivationPath, ExtendedSigningKey};
use hw_common::primitives::{Bip32Error, DerivationPath, Secp256k1ExtendedPublicKey};
use keys::{KeyPair, Secret as Secp256k1Secret};
use mm2_err_handle::prelude::*;
use std::ops::Deref;
use std::sync::Arc;
use zeroize::Zeroize;

pub type Mm2InternalKeyPair = KeyPair;

/// A 64-byte BIP39 seed, zeroized on drop for security.
pub struct Bip39Seed(pub [u8; 64]);

impl Drop for Bip39Seed {
    fn drop(&mut self) { self.0.zeroize(); }
}

/// Thread-safe reference-counted handle to [`GlobalHDAccountCtx`].
#[derive(Clone)]
pub struct GlobalHDAccountArc(Arc<GlobalHDAccountCtx>);

impl Deref for GlobalHDAccountArc {
    type Target = GlobalHDAccountCtx;
    fn deref(&self) -> &Self::Target { &self.0 }
}

/// Global HD account context holding the master BIP39 seed and derived master keys.
///
/// # Key Derivation
/// - **secp256k1**: Uses BIP32 derivation from the BIP39 seed.
///   Coins use `derive_secp256k1_secret()` to get their per-coin keys.
/// - **ed25519**: Uses SLIP-0010 derivation from the BIP39 seed.
///   Coins use `derive_ed25519_signing_key()` for their per-coin keys.
///
/// # Security
/// The BIP39 seed is the root of all derived keys. It MUST NOT be logged or exposed.
/// It is zeroized on drop via the [`Bip39Seed`] wrapper.
pub struct GlobalHDAccountCtx {
    /// The root BIP39 seed (64 bytes), derived from the mnemonic.
    bip39_seed: Bip39Seed,
    /// BIP32 secp256k1 master extended private key, derived from `bip39_seed`.
    bip39_secp_priv_key: ExtendedPrivateKey<secp256k1::SecretKey>,
    /// SLIP-0010 ed25519 master extended signing key, derived from `bip39_seed`.
    ed25519_master_priv_key: ExtendedSigningKey,
}

impl GlobalHDAccountCtx {
    /// Creates a new `GlobalHDAccountCtx` from a BIP39 mnemonic string.
    ///
    /// Returns the mm2-internal key pair (derived at `mm2_internal_der_path()`) and the
    /// global HD context holding the master keys.
    ///
    /// # Key Pair
    /// The internal key pair is derived at `m/44'/141'/2147483647/0/0` (KMD coin type,
    /// max account index) and is used for P2P identity and internal signing.
    pub fn new(mnemonic_str: &str) -> Result<(Mm2InternalKeyPair, GlobalHDAccountCtx), MmError<PrivKeyError>> {
        let bip39_seed = bip39_seed_from_mnemonic(mnemonic_str)?;
        let bip39_secp_priv_key: ExtendedPrivateKey<secp256k1::SecretKey> =
            ExtendedPrivateKey::new(bip39_seed.0).map_to_mm(PrivKeyError::Secp256k1MasterKey)?;

        let ed25519_master_priv_key =
            ExtendedSigningKey::from_seed(&bip39_seed.0).map_to_mm(PrivKeyError::Ed25519MasterKey)?;

        // Derive the mm2 internal key pair for P2P identity.
        // Path: m/44'/141'/2147483647/0/0
        let derivation_path = crate::mm2_internal_der_path();
        let mut internal_priv_key = bip39_secp_priv_key.clone();
        for child in derivation_path {
            internal_priv_key = internal_priv_key
                .derive_child(child)
                .map_to_mm(PrivKeyError::Secp256k1InternalKey)?;
        }
        drop_mutability!(internal_priv_key);

        let mm2_internal_key_pair = key_pair_from_secret(internal_priv_key.private_key().as_ref())?;

        let global_hd_ctx = GlobalHDAccountCtx {
            bip39_seed,
            bip39_secp_priv_key,
            ed25519_master_priv_key,
        };
        Ok((mm2_internal_key_pair, global_hd_ctx))
    }

    /// Wraps this context in a thread-safe `Arc`.
    #[inline]
    pub fn into_arc(self) -> GlobalHDAccountArc { GlobalHDAccountArc(Arc::new(self)) }

    /// Returns a reference to the root BIP39 seed.
    pub fn root_seed(&self) -> &Bip39Seed { &self.bip39_seed }

    /// Returns the root BIP39 seed as a byte slice.
    pub fn root_seed_bytes(&self) -> &[u8] { &self.bip39_seed.0 }

    /// Returns the root BIP32 secp256k1 extended private key.
    pub fn root_priv_key(&self) -> &ExtendedPrivateKey<secp256k1::SecretKey> { &self.bip39_secp_priv_key }

    /// Derives a secp256k1 secret key at the given BIP32 derivation path.
    ///
    /// The derivation path should be a full path from the master key
    /// (e.g., `m/44'/141'/0'/0/0`). Each path component is applied
    /// sequentially from the root.
    pub fn derive_secp256k1_secret(
        &self,
        derivation_path: &DerivationPath,
    ) -> MmResult<Secp256k1Secret, hw_common::primitives::Bip32Error> {
        derive_secp256k1_secret(self.bip39_secp_priv_key.clone(), derivation_path)
    }

    /// Derives the account-level secp256k1 extended **public** key at the given BIP-32
    /// account derivation path (`purpose'/coin_type'/account'`).
    ///
    /// Walks the path from the in-memory BIP-32 master extended private key (`m`) and
    /// returns the extended public key at that node. Callers serialise it to the canonical
    /// `xpub` form via `.to_string(bip32::Prefix::XPUB)`. This is the software
    /// (non-hardware) source of account extended public keys for global-HD wallets.
    pub fn derive_account_extended_pubkey(
        &self,
        account_derivation_path: &DerivationPath,
    ) -> MmResult<Secp256k1ExtendedPublicKey, Bip32Error> {
        derive_secp256k1_extended_pubkey(self.bip39_secp_priv_key.clone(), account_derivation_path)
    }

    /// Derives an ed25519 signing key at the given SLIP-0010 derivation path.
    pub fn derive_ed25519_signing_key(
        &self,
        derivation_path: &Ed25519DerivationPath,
    ) -> MmResult<ExtendedSigningKey, PrivKeyError> {
        self.ed25519_master_priv_key
            .derive(derivation_path)
            .map_to_mm(|e| PrivKeyError::Ed25519DeriveKey(e, derivation_path.clone()))
    }
}

/// Derives a secp256k1 secret key by walking a BIP32 derivation path from a given extended private key.
pub fn derive_secp256k1_secret(
    bip39_secp_priv_key: ExtendedPrivateKey<secp256k1::SecretKey>,
    derivation_path: &DerivationPath,
) -> MmResult<Secp256k1Secret, hw_common::primitives::Bip32Error> {
    let mut priv_key = bip39_secp_priv_key;
    for child in derivation_path.iter() {
        priv_key = priv_key.derive_child(child)?;
    }
    drop_mutability!(priv_key);

    let secret = *priv_key.private_key().as_ref();
    Ok(Secp256k1Secret::from(secret))
}

/// Derives a secp256k1 extended **public** key by walking a BIP32 derivation path from a given
/// extended private key, and returning the extended public key at the resulting node.
///
/// This is the software (non-hardware) source of account-level extended public keys for
/// global-HD wallets: callers pass the in-memory BIP-32 master `m` and an account derivation
/// path (`purpose'/coin_type'/account'`), then serialise the result to the canonical `xpub`
/// form via `.to_string(bip32::Prefix::XPUB)`.
pub fn derive_secp256k1_extended_pubkey(
    bip39_secp_priv_key: ExtendedPrivateKey<secp256k1::SecretKey>,
    derivation_path: &DerivationPath,
) -> MmResult<Secp256k1ExtendedPublicKey, Bip32Error> {
    let mut priv_key = bip39_secp_priv_key;
    for child in derivation_path.iter() {
        priv_key = priv_key.derive_child(child)?;
    }
    drop_mutability!(priv_key);
    Ok(priv_key.public_key())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard BIP39 test mnemonic (zero entropy). DO NOT use in production.
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_global_hd_ctx_creation() {
        let (key_pair, ctx) =
            GlobalHDAccountCtx::new(TEST_MNEMONIC).expect("should create HD context from valid mnemonic");

        // Key pair should be compressed (33 bytes)
        assert_eq!(key_pair.public().len(), 33);
        // Seed should be 64 bytes
        assert_eq!(ctx.root_seed_bytes().len(), 64);
    }

    #[test]
    fn test_global_hd_ctx_deterministic() {
        let (kp1, ctx1) = GlobalHDAccountCtx::new(TEST_MNEMONIC).unwrap();
        let (kp2, ctx2) = GlobalHDAccountCtx::new(TEST_MNEMONIC).unwrap();

        assert_eq!(kp1.public(), kp2.public());
        assert_eq!(ctx1.root_seed_bytes(), ctx2.root_seed_bytes());
    }

    #[test]
    fn test_global_hd_ctx_invalid_mnemonic() {
        let result = GlobalHDAccountCtx::new("not a valid mnemonic");
        assert!(result.is_err());
    }

    #[test]
    fn test_derive_secp256k1_secret() {
        let (_kp, ctx) = GlobalHDAccountCtx::new(TEST_MNEMONIC).unwrap();
        let path = DerivationPath::from_str("m/44'/141'/0'/0/0").expect("valid derivation path");
        let secret = ctx.derive_secp256k1_secret(&path).expect("derivation should succeed");
        // Secret should be 32 bytes
        assert_eq!(secret.as_slice().len(), 32);
    }

    #[test]
    fn test_derive_secp256k1_deterministic() {
        let (_kp, ctx) = GlobalHDAccountCtx::new(TEST_MNEMONIC).unwrap();
        let path = DerivationPath::from_str("m/44'/141'/0'/0/0").unwrap();
        let s1 = ctx.derive_secp256k1_secret(&path).unwrap();
        let s2 = ctx.derive_secp256k1_secret(&path).unwrap();
        assert_eq!(s1.as_slice(), s2.as_slice());
    }

    #[test]
    fn test_derive_account_extended_pubkey_canonical_xpub() {
        let (_kp, ctx) = GlobalHDAccountCtx::new(TEST_MNEMONIC).unwrap();

        // BIP-84 and BIP-44 account paths both serialise to the canonical `xpub` prefix.
        let segwit_path = DerivationPath::from_str("m/84'/141'/0'").unwrap();
        let legacy_path = DerivationPath::from_str("m/44'/141'/0'").unwrap();
        let segwit_xpub = ctx
            .derive_account_extended_pubkey(&segwit_path)
            .unwrap()
            .to_string(bip32::Prefix::XPUB);
        let legacy_xpub = ctx
            .derive_account_extended_pubkey(&legacy_path)
            .unwrap()
            .to_string(bip32::Prefix::XPUB);

        assert!(segwit_xpub.starts_with("xpub"));
        assert!(legacy_xpub.starts_with("xpub"));
        assert_ne!(segwit_xpub, legacy_xpub);

        // Deterministic across re-derivation and matches the free helper.
        let again = ctx
            .derive_account_extended_pubkey(&segwit_path)
            .unwrap()
            .to_string(bip32::Prefix::XPUB);
        assert_eq!(segwit_xpub, again);
        let helper = derive_secp256k1_extended_pubkey(ctx.root_priv_key().clone(), &segwit_path)
            .unwrap()
            .to_string(bip32::Prefix::XPUB);
        assert_eq!(segwit_xpub, helper);
    }

    use std::str::FromStr;
}
