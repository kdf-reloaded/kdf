//! Fixed-size byte-array newtypes used as hash and tag values.
//!
//! Each [`define_hash!`] invocation produces an opaque newtype around a
//! `[u8; N]` array with the standard suite of trait impls (Default, Clone,
//! Copy, PartialEq, Eq, Ord, Hash, From<[u8; N]>, From<&[u8]>,
//! From<&'static str>, FromStr, Display, Debug, Deref, DerefMut).
//!
//! KDF-original. The macro layout below is structured around explicit
//! per-impl blocks rather than a single mega-expansion to keep diagnostics
//! readable when consumers push through the type system.

use rustc_hex::{FromHex, FromHexError, ToHex};
use std::hash::{Hash, Hasher};
use std::{cmp, fmt, ops, str};

macro_rules! define_hash {
    ($name:ident, $size:expr) => {
        #[doc = concat!("Fixed ", stringify!($size), "-byte payload (`[u8; ", stringify!($size), "]`).")]
        #[derive(Copy)]
        #[repr(transparent)]
        pub struct $name([u8; $size]);

        impl $name {
            /// Consume the wrapper and return the inner array.
            #[inline]
            pub fn take(self) -> [u8; $size] { self.0 }

            /// View the value as a byte slice.
            #[inline]
            pub fn as_slice(&self) -> &[u8] { &self.0 }

            /// Reverse-byte copy (Bitcoin-style "txid string" order).
            #[inline]
            pub fn reversed(&self) -> Self {
                let mut out = *self;
                out.0.reverse();
                out
            }

            /// Width in bytes.
            #[inline]
            pub fn size() -> usize { $size }

            /// Whether every byte is zero.
            #[inline]
            pub fn is_zero(&self) -> bool { self.0.iter().all(|b| *b == 0) }
        }

        impl Default for $name {
            #[inline]
            fn default() -> Self { $name([0u8; $size]) }
        }

        impl Clone for $name {
            #[inline]
            fn clone(&self) -> Self { *self }
        }

        impl AsRef<$name> for $name {
            #[inline]
            fn as_ref(&self) -> &$name { self }
        }

        impl From<[u8; $size]> for $name {
            #[inline]
            fn from(arr: [u8; $size]) -> Self { $name(arr) }
        }

        impl From<$name> for [u8; $size] {
            #[inline]
            fn from(h: $name) -> Self { h.0 }
        }

        impl<'a> From<&'a [u8]> for $name {
            #[inline]
            fn from(slc: &[u8]) -> Self {
                let mut inner = [0u8; $size];
                inner.copy_from_slice(&slc[0..$size]);
                $name(inner)
            }
        }

        impl From<&'static str> for $name {
            #[inline]
            fn from(s: &'static str) -> Self { s.parse().expect("static hex literal must parse") }
        }

        impl From<u8> for $name {
            #[inline]
            fn from(v: u8) -> Self {
                let mut out = Self::default();
                out.0[0] = v;
                out
            }
        }

        impl str::FromStr for $name {
            type Err = FromHexError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let raw: Vec<u8> = s.from_hex()?;
                if raw.len() != $size {
                    return Err(FromHexError::InvalidHexLength);
                }
                let mut inner = [0u8; $size];
                inner.copy_from_slice(&raw);
                Ok($name(inner))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str(&self.0.to_hex::<String>()) }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str(&self.0.to_hex::<String>()) }
        }

        impl ops::Deref for $name {
            type Target = [u8; $size];

            #[inline]
            fn deref(&self) -> &Self::Target { &self.0 }
        }

        impl ops::DerefMut for $name {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
        }

        impl AsRef<[u8]> for $name {
            #[inline]
            fn as_ref(&self) -> &[u8] { &self.0 }
        }

        impl AsMut<[u8]> for $name {
            #[inline]
            fn as_mut(&mut self) -> &mut [u8] { &mut self.0 }
        }

        impl cmp::PartialEq for $name {
            fn eq(&self, other: &Self) -> bool { self.0[..] == other.0[..] }
        }

        impl cmp::PartialEq<&$name> for $name {
            fn eq(&self, other: &&Self) -> bool { self.0[..] == other.0[..] }
        }

        impl cmp::Eq for $name {}

        impl cmp::PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> { Some(self.cmp(other)) }
        }

        impl cmp::Ord for $name {
            fn cmp(&self, other: &Self) -> cmp::Ordering { self.0[..].cmp(&other.0[..]) }
        }

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) { state.write(&self.0); }
        }
    };
}

// Standard sizes used by the UTXO codec, address derivation, and p2p.
define_hash!(H32, 4);
define_hash!(H48, 6);
define_hash!(H64, 8);
define_hash!(H96, 12);
define_hash!(H128, 16);
define_hash!(H160, 20);
define_hash!(H256, 32);
define_hash!(H264, 33);
define_hash!(H512, 64);
define_hash!(H520, 65);

// Zcash sapling / sprout payload sizes. Lengths come from the Zcash
// protocol specification (ZIP-0203 / NU3) — they are byte counts of the
// associated cryptographic objects, not parity-derived constants.
define_hash!(OutCipherText, 80);
define_hash!(ZkProofSapling, 192);
define_hash!(ZkProof, 296);
define_hash!(EncCipherText, 580);
define_hash!(CipherText, 601);
define_hash!(EquihashSolution, 1344);

impl H256 {
    /// Parse a hex literal as a Bitcoin-style "txid string" — bytes are
    /// taken in reverse order, matching the way explorers print txids.
    #[inline]
    pub fn from_reversed_str(s: &'static str) -> Self { H256::from(s).reversed() }

    /// Render as a Bitcoin-style "txid string" (reverse byte order).
    #[inline]
    pub fn to_reversed_str(self) -> String { self.reversed().to_string() }
}
