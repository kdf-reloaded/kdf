pub const HARDENED_PATH: u32 = 2147483648;

pub use bip32::{ChildNumber, DerivationPath, Error as Bip32Error, ExtendedPrivateKey, ExtendedPublicKey};

pub type Secp256k1ExtendedPublicKey = ExtendedPublicKey<secp256k1_33::PublicKey>;
pub type Secp256k1ExtendedPrivateKey = ExtendedPrivateKey<secp256k1_33::SecretKey>;
pub type XPub = String;

#[derive(Clone, Copy)]
pub enum EcdsaCurve {
    Secp256k1,
}
