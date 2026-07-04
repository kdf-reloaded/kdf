// SegWit P2WPKH / P2WSH addresses per BIP-141 + BIP-173 (Bech32).
//
// `Bech32m` (BIP-350) is intentionally not used here — KDF only emits
// witness v0 outputs. A future v1 (Taproot) integration would pick the
// variant based on `version`.

use crate::AddressHashEnum;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSegwitAddressFormat,
    Bech32(bech32::Error),
    EmptyBech32Payload,
    InvalidWitnessVersion(u8),
    UnsupportedWitnessVersion(u8),
    InvalidWitnessProgramLength(usize),
    InvalidSegwitV0ProgramLength(usize),
    UncompressedPubkey,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSegwitAddressFormat => f.write_str("Invalid segwit address format"),
            Error::Bech32(e) => write!(f, "bech32: {}", e),
            Error::EmptyBech32Payload => f.write_str("the bech32 payload was empty"),
            Error::InvalidWitnessVersion(v) => write!(f, "invalid witness script version: {}", v),
            Error::UnsupportedWitnessVersion(v) => write!(
                f,
                "unsupported witness version {} (only segwit v0 is supported; witness v1 / Taproot bech32m addresses are not supported)",
                v
            ),
            Error::InvalidWitnessProgramLength(l) => {
                write!(
                    f,
                    "the witness program must be between 2 and 40 bytes in length: length={}",
                    l
                )
            },
            Error::InvalidSegwitV0ProgramLength(l) => {
                write!(
                    f,
                    "a v0 witness program must be either of length 20 or 32 bytes: length={}",
                    l
                )
            },
            Error::UncompressedPubkey => f.write_str("an uncompressed pubkey was used where it is not allowed"),
        }
    }
}

impl From<bech32::Error> for Error {
    fn from(e: bech32::Error) -> Self { Error::Bech32(e) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddressType {
    P2wpkh,
    P2wsh,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegwitAddress {
    pub hrp: String,
    version: bech32::u5,
    pub program: Vec<u8>,
}

impl SegwitAddress {
    pub fn new(hash: &AddressHashEnum, hrp: String) -> SegwitAddress {
        SegwitAddress {
            hrp,
            version: bech32::u5::try_from_u8(0).expect("witness v0 < 32"),
            program: hash.to_vec(),
        }
    }

    pub fn address_type(&self) -> Option<AddressType> {
        match self.version.to_u8() {
            0 => match self.program.len() {
                20 => Some(AddressType::P2wpkh),
                32 => Some(AddressType::P2wsh),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_standard(&self) -> bool { self.address_type().is_some() }
}

/// Wraps a `fmt::Write` and forwards every char in uppercase. Used for
/// QR-code rendering where uppercase bech32 yields denser QR codes
/// (alphanumeric mode).
struct UpperWriter<W: fmt::Write>(W);

impl<W: fmt::Write> fmt::Write for UpperWriter<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.0.write_char(c.to_ascii_uppercase())?;
        }
        Ok(())
    }
}

impl fmt::Display for SegwitAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut upper;
        let writer: &mut dyn fmt::Write = if f.alternate() {
            upper = UpperWriter(f);
            &mut upper
        } else {
            f
        };
        let mut bw = bech32::Bech32Writer::new(&self.hrp, bech32::Variant::Bech32, writer)?;
        bech32::WriteBase32::write_u5(&mut bw, self.version)?;
        bech32::ToBase32::write_base32(&self.program, &mut bw)
    }
}

impl FromStr for SegwitAddress {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hrp_end = s.rfind('1').ok_or(Error::InvalidSegwitAddressFormat)?;
        let hrp = s[..hrp_end].to_string();

        let (_, payload, _) = bech32::decode(s)?;
        if payload.is_empty() {
            return Err(Error::EmptyBech32Payload);
        }

        let (v_slice, rest) = payload.split_at(1);
        let version = v_slice[0];
        let program: Vec<u8> = bech32::FromBase32::from_base32(rest)?;

        if version.to_u8() > 16 {
            return Err(Error::InvalidWitnessVersion(version.to_u8()));
        }
        // KDF only supports spending segwit v0 outputs. Witness v1 (Taproot,
        // bech32m, `bc1p...`) and any other non-zero version must be rejected
        // here rather than silently mis-encoded as a v0 P2WSH downstream.
        if version.to_u8() != 0 {
            return Err(Error::UnsupportedWitnessVersion(version.to_u8()));
        }
        if program.len() < 2 || program.len() > 40 {
            return Err(Error::InvalidWitnessProgramLength(program.len()));
        }
        if program.len() != 20 && program.len() != 32 {
            return Err(Error::InvalidSegwitV0ProgramLength(program.len()));
        }

        Ok(SegwitAddress { hrp, version, program })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Public;
    use crypto::sha256;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn p2wpkh_bitcoin_mainnet() {
        // tx b3c8c2b6cfc335abbcb2c7823a8453f55d64b2b5125a9a61e8737230cdb8ce20
        let bytes = unhex("033bc8c83c52df5712229a2f72206d90192366c36428cb0c12b6af98324d97bfbc");
        let pk = Public::from_slice(&bytes).unwrap();
        let addr = SegwitAddress::new(&AddressHashEnum::AddressHash(pk.address_hash()), "bc".into());
        assert_eq!(addr.to_string(), "bc1qvzvkjn4q3nszqxrv3nraga2r822xjty3ykvkuw");
        assert_eq!(addr.address_type(), Some(AddressType::P2wpkh));
    }

    #[test]
    fn p2wsh_bitcoin_mainnet() {
        let script = unhex("210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ac");
        let addr = SegwitAddress::new(&AddressHashEnum::WitnessScriptHash(sha256(&script)), "bc".into());
        assert_eq!(
            addr.to_string(),
            "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3"
        );
        assert_eq!(addr.address_type(), Some(AddressType::P2wsh));
    }

    #[test]
    fn taproot_v1_address_is_rejected() {
        // BIP-86 account 0, first receiving address (a real published mainnet
        // P2TR / Taproot test vector). It is a bech32m witness-v1 address with a
        // 32-byte program. Parsing must FAIL rather than silently produce a
        // (mis-encoded) v0 P2WSH address.
        let taproot = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";

        // Sanity-check the raw decode: witness version 1, 32-byte program.
        let (_, payload, _) = bech32::decode(taproot).unwrap();
        let (v_slice, rest) = payload.split_at(1);
        assert_eq!(v_slice[0].to_u8(), 1, "expected witness version 1 (Taproot)");
        let program: Vec<u8> = bech32::FromBase32::from_base32(rest).unwrap();
        assert_eq!(program.len(), 32, "expected a 32-byte witness program");

        let parsed = SegwitAddress::from_str(taproot);
        assert!(
            matches!(parsed, Err(Error::UnsupportedWitnessVersion(1))),
            "expected UnsupportedWitnessVersion(1), got Ok or wrong error"
        );
    }
}
