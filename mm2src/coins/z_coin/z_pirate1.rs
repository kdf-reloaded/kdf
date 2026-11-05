//! Pirate1 (Ironwood) payment destinations of a withdrawal (CRD ch.39 R39.6.5a--R39.6.5f).
//!
//! The wallet stays Sapling-only. This module holds the pieces a Sapling wallet needs to *pay* an
//! Ironwood address: the address form, the withdraw-only payment type and the fixed fee. The anchor
//! and flags of the Ironwood bundle are set where the transaction is built
//! (`z_coin_ops::build_from_wallet_db`, R39.6.5d--R39.6.5e). Nothing here lets a swap path express an
//! Ironwood recipient: [`IronwoodPayment`] is not a [`ZOutput`](super::ZOutput) and no function of
//! the swap paths accepts it.

use super::{configured_or_builtin_ironwood_time, ZcoinConsensusParams};
use bech32::primitives::decode::UncheckedHrpstring;
use bech32::{Bech32, Bech32m, Hrp};
use derive_more::Display;
use zcash_keys::encoding::decode_payment_address;
use zcash_protocol::consensus::NetworkType;
use zcash_protocol::value::Zatoshis as Amount;

/// Pirate1 prefix of the main network (R39.6.5a).
pub(crate) const PIRATE1_HRP_MAIN: &str = "pirate";
/// Pirate1 prefix of the test network (R39.6.5a).
pub(crate) const PIRATE1_HRP_TEST: &str = "pirate-test";
/// Pirate1 prefix of the regtest network (R39.6.5a).
pub(crate) const PIRATE1_HRP_REGTEST: &str = "pirate-regtest";

/// The fixed fee of a transaction that carries an Ironwood output, in base units (R39.6.5f).
pub(crate) const IRONWOOD_PAYMENT_FEE_SAT: u64 = 10_000;

/// The data part of a Pirate1 address: an 11-byte diversifier and a 32-byte transmission key.
const PIRATE1_DATA_LEN: usize = 43;

/// The Ironwood payment of a withdrawal: a recipient that is not a Sapling address, and the amount.
/// Only the withdrawal path builds one (R39.6.5d).
#[derive(Clone, Debug)]
pub(crate) struct IronwoodPayment {
    pub(crate) recipient: orchard::Address,
    pub(crate) amount: Amount,
}

/// The fee a transaction pays: the fixed Ironwood fee when it carries an Ironwood output, whatever the
/// coin's configured fee says, else the configured one (R39.6.5f).
pub(crate) fn fee_sat(carries_ironwood_output: bool, configured_sat: u64) -> u64 {
    if carries_ironwood_output {
        IRONWOOD_PAYMENT_FEE_SAT
    } else {
        configured_sat
    }
}

/// Whether the coin has an Ironwood pool: an Ironwood activation time or height is known for it,
/// configured, built-in or discovered. `ironwood_gate_disabled` does not change that (R39.6.5a).
pub(crate) fn has_ironwood_pool(params: &ZcoinConsensusParams, ticker: &str) -> bool {
    params.effective_ironwood_activation_height().is_some()
        || configured_or_builtin_ironwood_time(params, ticker).is_some()
}

/// The built-in Pirate1 prefix of the network the coin's Sapling prefixes select.
fn builtin_pirate1_hrp(params: &ZcoinConsensusParams) -> &'static str {
    match params.network_type_hint() {
        NetworkType::Main => PIRATE1_HRP_MAIN,
        NetworkType::Test => PIRATE1_HRP_TEST,
        NetworkType::Regtest => PIRATE1_HRP_REGTEST,
    }
}

/// The Pirate1 prefix of the coin: the configured `hrp_ironwood_payment_address`, else the built-in one.
pub(crate) fn pirate1_hrp(params: &ZcoinConsensusParams) -> String {
    params
        .hrp_ironwood_payment_address()
        .map(str::to_owned)
        .unwrap_or_else(|| builtin_pirate1_hrp(params).to_owned())
}

/// Why a string is not an acceptable Pirate1 address (R39.6.5a, R39.6.5b).
#[derive(Debug, Display, PartialEq)]
pub(crate) enum Pirate1AddressError {
    #[display(fmt = "{} has no Ironwood pool, so it takes no Pirate1 address", coin)]
    NoIronwoodPool { coin: String },
    #[display(
        fmt = "the prefix '{}' belongs to another network; {} takes the Pirate1 prefix '{}'",
        found,
        coin,
        expected
    )]
    OtherNetwork {
        found: String,
        coin: String,
        expected: String,
    },
    #[display(fmt = "not a Bech32 string: {}", _0)]
    Malformed(String),
    #[display(fmt = "the checksum is Bech32m; a Pirate1 address carries the classic Bech32 (BIP 173) checksum")]
    Bech32m,
    #[display(fmt = "the Bech32 checksum is wrong")]
    BadChecksum,
    #[display(fmt = "the data part is {} bytes; a Pirate1 address carries exactly 43", _0)]
    DataLength(usize),
    #[display(fmt = "the data part is not the canonical Bech32 encoding of its 43 bytes (non-zero padding)")]
    NonCanonical,
    #[display(fmt = "the 43 bytes are not a valid Orchard-type payment address")]
    NotAnOrchardAddress,
}

/// What a string that is not a Sapling address of the coin turns out to be.
#[derive(Debug, PartialEq)]
pub(crate) enum Pirate1Verdict {
    /// Its prefix is not a Pirate1 prefix at all: the Sapling answer stands.
    NotPirate1,
    /// A Pirate1 address of the coin's network (R39.6.5a).
    Address(orchard::Address),
    /// A Pirate1-looking string that is refused, with the cause (R39.6.5b).
    Refused(Pirate1AddressError),
}

/// The human-readable part of `s` in lowercase: everything before the last `1`, which is the separator
/// of a Bech32 string. Read from the text alone, so a Pirate1 string with a defect elsewhere (mixed
/// case, a bad character) is still recognised as one and refused with its cause.
fn hrp_of(s: &str) -> Option<String> { s.rfind('1').map(|separator| s[..separator].to_ascii_lowercase()) }

/// Decodes the data part of a Pirate1 address whose prefix is `expected_hrp` (R39.6.5a).
fn decode_pirate1(expected_hrp: &str, s: &str) -> Result<orchard::Address, Pirate1AddressError> {
    let unchecked = UncheckedHrpstring::new(s).map_err(|e| Pirate1AddressError::Malformed(e.to_string()))?;
    let hrp = unchecked.hrp();
    if !hrp.as_str().eq_ignore_ascii_case(expected_hrp) {
        return Err(Pirate1AddressError::Malformed(format!(
            "the prefix is '{}', not '{}'",
            hrp.as_str(),
            expected_hrp
        )));
    }
    if !unchecked.has_valid_checksum::<Bech32>() {
        return Err(if unchecked.has_valid_checksum::<Bech32m>() {
            Pirate1AddressError::Bech32m
        } else {
            Pirate1AddressError::BadChecksum
        });
    }
    let bytes: Vec<u8> = unchecked.remove_checksum::<Bech32>().byte_iter().collect();
    let raw = <[u8; PIRATE1_DATA_LEN]>::try_from(bytes.as_slice())
        .map_err(|_| Pirate1AddressError::DataLength(bytes.len()))?;
    // The padding bits of the last character must be zero: the string is the canonical encoding.
    let canonical =
        bech32::encode_lower::<Bech32>(hrp, &bytes).map_err(|e| Pirate1AddressError::Malformed(e.to_string()))?;
    if canonical != s.to_ascii_lowercase() {
        return Err(Pirate1AddressError::NonCanonical);
    }
    Option::from(orchard::Address::from_raw_address_bytes(&raw)).ok_or(Pirate1AddressError::NotAnOrchardAddress)
}

/// Classifies a string that is not a Sapling address of the coin (R39.6.5b, R39.6.5c).
pub(crate) fn classify(params: &ZcoinConsensusParams, ticker: &str, s: &str) -> Pirate1Verdict {
    let Some(found) = hrp_of(s) else {
        return Pirate1Verdict::NotPirate1;
    };
    let expected = pirate1_hrp(params);
    let is_builtin = [PIRATE1_HRP_MAIN, PIRATE1_HRP_TEST, PIRATE1_HRP_REGTEST].contains(&found.as_str());
    if found != expected.to_ascii_lowercase() && !is_builtin {
        return Pirate1Verdict::NotPirate1;
    }
    if !has_ironwood_pool(params, ticker) {
        return Pirate1Verdict::Refused(Pirate1AddressError::NoIronwoodPool {
            coin: ticker.to_owned(),
        });
    }
    if found != expected.to_ascii_lowercase() {
        return Pirate1Verdict::Refused(Pirate1AddressError::OtherNetwork {
            found,
            coin: ticker.to_owned(),
            expected,
        });
    }
    match decode_pirate1(&expected, s) {
        Ok(address) => Pirate1Verdict::Address(address),
        Err(e) => Pirate1Verdict::Refused(e),
    }
}

/// A withdrawal's destination: a Sapling address of the coin, or a Pirate1 (Ironwood) address (R39.6.5c).
#[derive(Debug)]
pub(crate) enum WithdrawDestination {
    Sapling(sapling::PaymentAddress),
    Ironwood(orchard::Address),
}

/// Why a string is neither a Sapling address of the coin nor an acceptable Pirate1 address.
#[derive(Debug, PartialEq)]
pub(crate) struct DestinationRefusal {
    ticker: String,
    sapling_error: String,
    /// Set when the string is shaped like a Pirate1 address and refused as one.
    pirate1: Option<Pirate1AddressError>,
    pirate1_hrp: String,
    has_ironwood_pool: bool,
}

impl DestinationRefusal {
    /// The `reason` of `validateaddress` (R39.6.5b): the cause of the Pirate1 refusal when the string
    /// is shaped like one, else the Sapling decoding failure, as before.
    pub(crate) fn validate_reason(&self) -> String {
        match &self.pirate1 {
            Some(cause) => format!("Error on decode of a Pirate1 (Ironwood) address: {}", cause),
            None => format!("Error {} on decode_payment_address", self.sapling_error),
        }
    }

    /// The reason of `InvalidAddress` on a withdrawal (R39.6.5c): it names both accepted forms.
    pub(crate) fn withdraw_reason(&self) -> String {
        if !self.has_ironwood_pool && self.pirate1.is_none() {
            return self.sapling_error.clone();
        }
        let pirate1 = match &self.pirate1 {
            Some(cause) => format!("not an acceptable Pirate1 (Ironwood) address: {}", cause),
            None => format!(
                "not a Pirate1 (Ironwood) address with the prefix '{}'",
                self.pirate1_hrp
            ),
        };
        format!(
            "not a Sapling address of {} ({}) and {}",
            self.ticker, self.sapling_error, pirate1
        )
    }
}

/// Decodes a withdrawal destination: a Sapling address under the coin's configured prefix, else a
/// Pirate1 address of the coin's network (R39.6.5b, R39.6.5c).
pub(crate) fn decode_destination(
    params: &ZcoinConsensusParams,
    ticker: &str,
    to: &str,
) -> Result<WithdrawDestination, DestinationRefusal> {
    let sapling_error = match decode_payment_address(params.hrp_sapling_payment_address(), to) {
        Ok(address) => return Ok(WithdrawDestination::Sapling(address)),
        Err(e) => e.to_string(),
    };
    let pirate1 = match classify(params, ticker, to) {
        Pirate1Verdict::Address(address) => return Ok(WithdrawDestination::Ironwood(address)),
        Pirate1Verdict::NotPirate1 => None,
        Pirate1Verdict::Refused(cause) => Some(cause),
    };
    Err(DestinationRefusal {
        ticker: ticker.to_owned(),
        sapling_error,
        pirate1,
        pirate1_hrp: pirate1_hrp(params),
        has_ironwood_pool: has_ironwood_pool(params, ticker),
    })
}
