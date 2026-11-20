//! The GasFree `PermitTransfer` authorization: its TIP-712 typed data, its signer and the signed artifact the
//! withdraw returns (CRD ch.63 §63.6, §63.8 step 5, R3 to R7, R9).
//!
//! # Invariants
//!
//! - The `version` of an authorization is always 1 (R5): the type has no version field, and a document that
//!   carries another version does not read back.
//! - A [`SignedPermit`] is only built by [`sign_permit`] or read back from its own document, and its signature
//!   has `v` in 27/28 (R6).
//! - The raw signature is never rendered by `Debug` (R9).

use super::super::{Network, TronAddress};
use super::custody::derive_custody;
use super::failure::GasFreeError;
use super::networks::{gasfree_network, DOMAIN_NAME, DOMAIN_VERSION};
use super::{address_word, keccak};
use ethereum_types::{H256, U256};
use mm2_eth::keys::{sign, KeyPair, Secret};
use mm2_eth::typed_data::{hash_typed_data, Eip712, FieldKind, TypeDef};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::fmt;

/// The only authorization version (R5).
pub(crate) const PERMIT_VERSION: u64 = 1;

/// The name of the signed structure (§63.6).
const PRIMARY_TYPE: &str = "PermitTransfer";

/// The encoded type of the typed-data domain (§63.6).
const DOMAIN_TYPE: &str = "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";

/// The encoded type of `PermitTransfer` (§63.6): the member names and their order are dictated.
const PERMIT_TYPE: &str = "PermitTransfer(address token,address serviceProvider,address user,address receiver,\
                           uint256 value,uint256 maxFee,uint256 deadline,uint256 version,uint256 nonce)";

/// The values of one `PermitTransfer` (§63.6). Amounts are in the token's smallest unit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PermitTransfer {
    /// The TRC20 token contract.
    pub token: TronAddress,
    /// The fee-paying provider.
    pub service_provider: TronAddress,
    /// The account whose custody address is debited; the signer.
    pub user: TronAddress,
    pub receiver: TronAddress,
    pub value: U256,
    /// The highest fee the user accepts.
    pub max_fee: U256,
    /// Unix time in seconds after which the authorization is void.
    pub deadline: u64,
    /// The nonce the provider's account record reports.
    pub nonce: u64,
}

/// `value` as one 32-byte big-endian word.
fn u256_word(value: U256) -> [u8; 32] {
    let mut word = [0u8; 32];
    value.to_big_endian(&mut word);
    word
}

/// The domain separator of the GasFree controller of `network` (§63.6).
pub(crate) fn domain_separator(network: &Network) -> [u8; 32] {
    let constants = gasfree_network(network);
    let mut encoded = Vec::with_capacity(5 * 32);
    encoded.extend_from_slice(&keccak(DOMAIN_TYPE.as_bytes()));
    encoded.extend_from_slice(&keccak(DOMAIN_NAME.as_bytes()));
    encoded.extend_from_slice(&keccak(DOMAIN_VERSION.as_bytes()));
    encoded.extend_from_slice(&u256_word(U256::from(constants.chain_id)));
    encoded.extend_from_slice(&address_word(&constants.controller()));
    keccak(&encoded)
}

/// The struct hash of `permit` (§63.6).
pub(crate) fn struct_hash(permit: &PermitTransfer) -> [u8; 32] {
    let mut encoded = Vec::with_capacity(10 * 32);
    encoded.extend_from_slice(&keccak(PERMIT_TYPE.as_bytes()));
    for address in [&permit.token, &permit.service_provider, &permit.user, &permit.receiver] {
        encoded.extend_from_slice(&address_word(address));
    }
    for number in [
        permit.value,
        permit.max_fee,
        U256::from(permit.deadline),
        U256::from(PERMIT_VERSION),
        U256::from(permit.nonce),
    ] {
        encoded.extend_from_slice(&u256_word(number));
    }
    keccak(&encoded)
}

/// The domain values as the shared typed-data encoder reads them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainValues {
    name: &'static str,
    version: &'static str,
    chain_id: u64,
    verifying_contract: String,
}

/// The message values as the shared typed-data encoder reads them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageValues {
    token: String,
    service_provider: String,
    user: String,
    receiver: String,
    value: String,
    max_fee: String,
    deadline: String,
    version: String,
    nonce: String,
}

/// The 20-byte payload of `address` in hex, the form of an `address` member of the typed data.
fn address_hex(address: &TronAddress) -> String { hex::encode(address.to_evm_address().as_bytes()) }

/// The full typed-data hash of `permit` on `network`, the digest that is signed (§63.6): the result of the
/// shared typed-data encoder, required to equal `keccak256(0x1901 ‖ domain separator ‖ struct hash)` as this
/// module computes them.
///
/// # Errors
///
/// [`GasFreeError::Authorization`] when the encoder refuses a value (it takes integers up to 128 bits) or the
/// two computations disagree.
pub(crate) fn typed_data_hash(network: &Network, permit: &PermitTransfer) -> Result<[u8; 32], GasFreeError> {
    let constants = gasfree_network(network);

    let mut domain_type = TypeDef::domain();
    domain_type
        .field("name", FieldKind::String)
        .field("version", FieldKind::String)
        .field("chainId", FieldKind::Uint256)
        .field("verifyingContract", FieldKind::Address);
    let mut permit_type = TypeDef::new(PRIMARY_TYPE);
    permit_type
        .field("token", FieldKind::Address)
        .field("serviceProvider", FieldKind::Address)
        .field("user", FieldKind::Address)
        .field("receiver", FieldKind::Address)
        .field("value", FieldKind::Uint256)
        .field("maxFee", FieldKind::Uint256)
        .field("deadline", FieldKind::Uint256)
        .field("version", FieldKind::Uint256)
        .field("nonce", FieldKind::Uint256);

    let typed_data = Eip712 {
        types: [
            (domain_type.name, domain_type.fields),
            (permit_type.name, permit_type.fields),
        ]
        .into_iter()
        .collect(),
        domain: DomainValues {
            name: DOMAIN_NAME,
            version: DOMAIN_VERSION,
            chain_id: constants.chain_id,
            verifying_contract: address_hex(&constants.controller()),
        },
        primary_type: PRIMARY_TYPE.to_owned(),
        message: MessageValues {
            token: address_hex(&permit.token),
            service_provider: address_hex(&permit.service_provider),
            user: address_hex(&permit.user),
            receiver: address_hex(&permit.receiver),
            value: permit.value.to_string(),
            max_fee: permit.max_fee.to_string(),
            deadline: permit.deadline.to_string(),
            version: PERMIT_VERSION.to_string(),
            nonce: permit.nonce.to_string(),
        },
    };
    let encoded = hash_typed_data(typed_data).map_err(|e| GasFreeError::Authorization(e.to_string()))?;

    let mut preimage = Vec::with_capacity(2 + 32 + 32);
    preimage.extend_from_slice(&[0x19, 0x01]);
    preimage.extend_from_slice(&domain_separator(network));
    preimage.extend_from_slice(&struct_hash(permit));
    if keccak(&preimage) != encoded {
        return Err(GasFreeError::Authorization(
            "the two computations of the typed-data hash disagree".to_owned(),
        ));
    }
    Ok(encoded)
}

/// A `PermitTransfer` with the user's signature over it.
///
/// Its JSON document is the signed-authorization object of §63.8 step 5: the snake_case members `token`,
/// `service_provider`, `user`, `receiver`, `value`, `max_fee`, `deadline`, `version`, `nonce` and `sig`, with the
/// addresses in base58, the integers as decimal strings and the signature as 130 hex digits. Reading it back
/// rejects an unknown member and a `version` other than 1.
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "SignedPermitDocument", into = "SignedPermitDocument")]
pub(crate) struct SignedPermit {
    permit: PermitTransfer,
    /// `r ‖ s ‖ v`, `v` in 27/28. A secret-like value: only the artifact and the submit payload carry it (R9).
    sig: [u8; 65],
}

impl SignedPermit {
    pub(crate) fn permit(&self) -> &PermitTransfer { &self.permit }

    /// The signature as the documents carry it: 130 hex digits without `0x` (R6).
    pub(crate) fn sig_hex(&self) -> String { hex::encode(self.sig) }

    /// A signed authorization read back from the members of a document: `version` and `sig` as the document
    /// carries them.
    ///
    /// # Errors
    ///
    /// A text, which never repeats a value, when `version` is not 1 (R5) or `sig` is not a well-formed
    /// signature (R6).
    pub(crate) fn from_document_members(permit: PermitTransfer, version: &str, sig: &str) -> Result<Self, String> {
        check_version(version)?;
        Ok(SignedPermit {
            permit,
            sig: parse_sig_hex(sig)?,
        })
    }
}

impl fmt::Debug for SignedPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedPermit")
            .field("permit", &self.permit)
            .field("sig", &"<redacted>")
            .finish()
    }
}

/// Signs `permit` for `network` with `secret` at the time `now_s` (Unix seconds).
///
/// # Errors
///
/// - [`GasFreeError::SignerNotUser`] when `secret` is not the key of `permit.user` (R4).
/// - [`GasFreeError::DeadlineNotInFuture`] when `permit.deadline` is not after `now_s` (R7).
/// - [`GasFreeError::Authorization`] when the typed data cannot be encoded or signed.
pub(crate) fn sign_permit(
    network: &Network,
    permit: PermitTransfer,
    secret: &Secret,
    now_s: u64,
) -> Result<SignedPermit, GasFreeError> {
    let key_pair = KeyPair::from_secret(secret.clone()).map_err(|e| GasFreeError::Authorization(e.to_string()))?;
    if key_pair.address() != permit.user.to_evm_address() {
        return Err(GasFreeError::SignerNotUser);
    }
    if permit.deadline <= now_s {
        return Err(GasFreeError::DeadlineNotInFuture {
            deadline: permit.deadline,
            now: now_s,
        });
    }
    let digest = typed_data_hash(network, &permit)?;
    let signature = sign(secret, &H256::from(digest)).map_err(|e| GasFreeError::Authorization(e.to_string()))?;
    let mut sig = signature.into_bytes();
    // The signer returns the recovery id 0 or 1; the EIP-712 convention is 27 or 28 (R6).
    sig[64] = match sig[64] {
        0 => 27,
        1 => 28,
        other => {
            return Err(GasFreeError::Authorization(format!(
                "the signer returned the recovery id {other}"
            )))
        },
    };
    Ok(SignedPermit { permit, sig })
}

/// A decimal string of digits only as a 256-bit integer. An empty string, a sign and any other character are
/// refused.
pub(crate) fn parse_decimal_u256(text: &str) -> Result<U256, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("an integer member is not a string of decimal digits".to_owned());
    }
    U256::from_dec_str(text).map_err(|_| "an integer member does not fit 256 bits".to_owned())
}

/// Like [`parse_decimal_u256`] for a 64-bit integer.
pub(crate) fn parse_decimal_u64(text: &str) -> Result<u64, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("an integer member is not a string of decimal digits".to_owned());
    }
    text.parse()
        .map_err(|_| "an integer member does not fit 64 bits".to_owned())
}

/// A signature as the documents carry it: exactly 130 hex digits, no `0x`, `v` in 27/28 (R6). The error text
/// never repeats the value.
pub(crate) fn parse_sig_hex(text: &str) -> Result<[u8; 65], String> {
    if text.len() != 130 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("the signature is not 130 hex digits".to_owned());
    }
    let bytes = hex::decode(text).map_err(|_| "the signature is not hex".to_owned())?;
    let sig = <[u8; 65]>::try_from(bytes.as_slice()).map_err(|_| "the signature is not 65 bytes".to_owned())?;
    if sig[64] != 27 && sig[64] != 28 {
        return Err("the signature's recovery byte is neither 27 nor 28".to_owned());
    }
    Ok(sig)
}

/// The `version` member of a document: the decimal string of [`PERMIT_VERSION`] and nothing else (R5).
pub(crate) fn check_version(text: &str) -> Result<(), String> {
    if parse_decimal_u64(text)? == PERMIT_VERSION {
        Ok(())
    } else {
        Err("the authorization version is not 1".to_owned())
    }
}

/// The JSON form of a [`SignedPermit`] (§63.8 step 5). It has no `Debug`: it holds the signature.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedPermitDocument {
    token: TronAddress,
    service_provider: TronAddress,
    user: TronAddress,
    receiver: TronAddress,
    value: String,
    max_fee: String,
    deadline: String,
    version: String,
    nonce: String,
    sig: String,
}

impl From<SignedPermit> for SignedPermitDocument {
    fn from(signed: SignedPermit) -> Self {
        let sig = signed.sig_hex();
        let permit = signed.permit;
        SignedPermitDocument {
            token: permit.token,
            service_provider: permit.service_provider,
            user: permit.user,
            receiver: permit.receiver,
            value: permit.value.to_string(),
            max_fee: permit.max_fee.to_string(),
            deadline: permit.deadline.to_string(),
            version: PERMIT_VERSION.to_string(),
            nonce: permit.nonce.to_string(),
            sig,
        }
    }
}

impl TryFrom<SignedPermitDocument> for SignedPermit {
    type Error = String;

    fn try_from(document: SignedPermitDocument) -> Result<Self, Self::Error> {
        let permit = PermitTransfer {
            token: document.token,
            service_provider: document.service_provider,
            user: document.user,
            receiver: document.receiver,
            value: parse_decimal_u256(&document.value)?,
            max_fee: parse_decimal_u256(&document.max_fee)?,
            deadline: parse_decimal_u64(&document.deadline)?,
            nonce: parse_decimal_u64(&document.nonce)?,
        };
        SignedPermit::from_document_members(permit, &document.version, &document.sig)
    }
}

/// The relay-type discriminator of the off-chain payload (§63.8 step 5).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) enum RelayType {
    #[serde(rename = "tron_gasfree")]
    TronGasFree,
}

/// The off-chain payload a sign-only gasless withdraw returns (§63.8 step 5): the signed authorization with the
/// values a relay needs beside it. It is not a transaction and is not broadcast.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GasFreeRelayPayload {
    pub relay_type: RelayType,
    /// The network's EIP-712 chain id, as a decimal string.
    #[serde(with = "decimal_u64")]
    pub chain_id: u64,
    /// The coin's ticker.
    pub coin: String,
    /// The HD selector of the sending address as the withdraw request gave it, if it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Json>,
    pub from_address: TronAddress,
    /// The derived custody address of `from_address` (§63.5).
    pub gasfree_address: TronAddress,
    /// The controller contract of the network.
    pub verifying_contract: TronAddress,
    /// When the payload was created: Unix seconds, written as RFC 3339 in UTC with whole seconds.
    #[serde(with = "rfc3339_seconds")]
    pub created_at: u64,
    pub signed_authorization: SignedPermit,
}

impl GasFreeRelayPayload {
    /// The payload for `signed` on `network`, created at `created_at` (Unix seconds).
    pub(crate) fn new(
        network: &Network,
        coin: &str,
        from: Option<Json>,
        signed: SignedPermit,
        created_at: u64,
    ) -> Self {
        let constants = gasfree_network(network);
        let user = signed.permit.user;
        GasFreeRelayPayload {
            relay_type: RelayType::TronGasFree,
            chain_id: constants.chain_id,
            coin: coin.to_owned(),
            from,
            from_address: user,
            gasfree_address: derive_custody(network, &user).address,
            verifying_contract: constants.controller(),
            created_at,
            signed_authorization: signed,
        }
    }
}

/// A `u64` as a decimal string.
mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_decimal_u64(&text).map_err(serde::de::Error::custom)
    }
}

/// Unix seconds as an RFC 3339 time in UTC with whole seconds, `YYYY-MM-DDTHH:MM:SSZ`.
mod rfc3339_seconds {
    use serde::{Deserialize, Deserializer, Serializer};

    const SECONDS_PER_DAY: u64 = 86_400;
    /// Days from 0000-03-01 to 1970-01-01 in the proleptic Gregorian calendar.
    const DAYS_TO_UNIX_EPOCH: u64 = 719_468;
    /// Days of a 400-year cycle.
    const DAYS_PER_ERA: u64 = 146_097;

    /// The calendar date (year, month, day) of the day `days` after 1970-01-01. The year is counted from March,
    /// so that the leap day is the last day of a year.
    fn civil_from_days(days: u64) -> (u64, u64, u64) {
        let shifted = days + DAYS_TO_UNIX_EPOCH;
        let era = shifted / DAYS_PER_ERA;
        let day_of_era = shifted % DAYS_PER_ERA;
        let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + u64::from(month <= 2);
        (year, month, day)
    }

    /// The inverse of [`civil_from_days`]; `None` for a date before 1970.
    fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
        let year = if month <= 2 { year.checked_sub(1)? } else { year };
        let era = year / 400;
        let year_of_era = year % 400;
        let shifted_month = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + day.checked_sub(1)?;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        (era * DAYS_PER_ERA + day_of_era).checked_sub(DAYS_TO_UNIX_EPOCH)
    }

    pub(super) fn format(unix_seconds: u64) -> String {
        let (year, month, day) = civil_from_days(unix_seconds / SECONDS_PER_DAY);
        let second_of_day = unix_seconds % SECONDS_PER_DAY;
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            second_of_day / 3_600,
            second_of_day % 3_600 / 60,
            second_of_day % 60
        )
    }

    /// Reads exactly the form [`format`] writes; anything else is `None`.
    pub(super) fn parse(text: &str) -> Option<u64> {
        let bytes = text.as_bytes();
        let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'Z')];
        if bytes.len() != 20 || separators.iter().any(|(at, separator)| bytes[*at] != *separator) {
            return None;
        }
        let number = |from: usize, to: usize| -> Option<u64> {
            let digits = text.get(from..to)?;
            if digits.bytes().all(|byte| byte.is_ascii_digit()) {
                digits.parse().ok()
            } else {
                None
            }
        };
        let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
        let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
        if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let days = days_from_civil(year, month, day)?;
        let unix_seconds = days * SECONDS_PER_DAY + hour * 3_600 + minute * 60 + second;
        // A day the month does not have reads as a later date: only a text that writes back as itself is taken.
        (format(unix_seconds) == text).then_some(unix_seconds)
    }

    pub(super) fn serialize<S: Serializer>(unix_seconds: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format(*unix_seconds))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse(&text)
            .ok_or_else(|| serde::de::Error::custom("the creation time is not RFC 3339 in UTC with whole seconds"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_vectors::{address, hex32, MessageVector, DOMAIN_VECTORS, MESSAGES, MESSAGE_MAX_FEE,
                                     MESSAGE_SERVICE_PROVIDER, USERS};
    use super::*;
    use mm2_eth::keys::{verify_address, Signature};
    use serde_json::json;
    use std::str::FromStr;

    fn permit_of(message: &MessageVector) -> PermitTransfer {
        PermitTransfer {
            token: address(message.token),
            service_provider: address(MESSAGE_SERVICE_PROVIDER),
            user: address(message.user),
            receiver: address(message.receiver),
            value: U256::from(message.value),
            max_fee: U256::from(MESSAGE_MAX_FEE),
            deadline: message.deadline,
            nonce: message.nonce,
        }
    }

    /// A test key: the secret key 1, whose address is the first fixture user.
    fn secret(last_byte: u8) -> Secret {
        let mut bytes = [0u8; 32];
        bytes[31] = last_byte;
        Secret::from_slice(&bytes).expect("a valid test key")
    }

    /// The first message, signed by its user one second before its deadline.
    fn signed_first_message() -> SignedPermit {
        let permit = permit_of(&MESSAGES[0]);
        let now_s = permit.deadline - 1;
        sign_permit(&Network::Nile, permit, &secret(1), now_s).expect("the user signs")
    }

    /// ch.63 T2: the domain separator of each network.
    #[test]
    fn the_domain_separators_are_the_pinned_ones() {
        for (network, separator, _) in DOMAIN_VECTORS {
            assert_eq!(domain_separator(&network), hex32(separator));
        }
    }

    /// ch.63 T2: the five published Nile messages.
    #[test]
    fn the_nile_message_vectors_are_reproduced() {
        for message in &MESSAGES {
            let permit = permit_of(message);
            assert_eq!(struct_hash(&permit), hex32(message.nile_struct_hash));
            let full = typed_data_hash(&Network::Nile, &permit).expect("a typed-data hash");
            assert_eq!(full, hex32(message.nile_full_hash));
        }
    }

    /// ch.63 T2: the first message on each network.
    #[test]
    fn the_first_message_has_the_pinned_hash_on_each_network() {
        let permit = permit_of(&MESSAGES[0]);
        for (network, _, full_hash) in DOMAIN_VECTORS {
            let full = typed_data_hash(&network, &permit).expect("a typed-data hash");
            assert_eq!(full, hex32(full_hash));
        }
    }

    #[test]
    fn a_value_the_encoder_cannot_take_is_refused() {
        let mut permit = permit_of(&MESSAGES[0]);
        permit.value = U256::MAX;
        let error = typed_data_hash(&Network::Nile, &permit).expect_err("an oversized value");
        assert!(matches!(error, GasFreeError::Authorization(_)));
    }

    #[test]
    fn the_signature_recovers_to_the_user_with_v_27_or_28() {
        assert_eq!(USERS[0].address, MESSAGES[0].user);
        let signed = signed_first_message();
        let sig = parse_sig_hex(&signed.sig_hex()).expect("a well-formed signature");
        assert!(sig[64] == 27 || sig[64] == 28, "the recovery byte is not normalized");

        let digest = typed_data_hash(&Network::Nile, signed.permit()).expect("a typed-data hash");
        let signature = Signature::from_str(&signed.sig_hex()).expect("65 bytes");
        let user = signed.permit().user.to_evm_address();
        assert!(verify_address(&user, &signature, &H256::from(digest)).expect("a recoverable signature"));
        // The same signature does not verify for another network's digest.
        let other = typed_data_hash(&Network::Mainnet, signed.permit()).expect("a typed-data hash");
        assert!(!matches!(
            verify_address(&user, &signature, &H256::from(other)),
            Ok(true)
        ));
    }

    #[test]
    fn a_key_that_is_not_the_user_is_refused() {
        let permit = permit_of(&MESSAGES[0]);
        let now_s = permit.deadline - 1;
        let error = sign_permit(&Network::Nile, permit, &secret(2), now_s).expect_err("a foreign key");
        assert!(matches!(error, GasFreeError::SignerNotUser));
    }

    #[test]
    fn a_deadline_that_is_not_in_the_future_is_refused() {
        let permit = permit_of(&MESSAGES[0]);
        for now_s in [permit.deadline, permit.deadline + 1] {
            let error =
                sign_permit(&Network::Nile, permit.clone(), &secret(1), now_s).expect_err("an expired deadline");
            assert!(matches!(error, GasFreeError::DeadlineNotInFuture { .. }));
        }
    }

    #[test]
    fn debug_hides_the_signature() {
        let signed = signed_first_message();
        let payload = GasFreeRelayPayload::new(&Network::Nile, "USDT-TRC20", None, signed.clone(), 1_726_207_000);
        let sig_hex = signed.sig_hex();
        for rendered in [format!("{signed:?}"), format!("{payload:?}")] {
            let rendered = rendered.to_ascii_lowercase();
            assert!(
                rendered.contains("<redacted>"),
                "the signature is not marked as redacted"
            );
            assert!(!rendered.contains(&sig_hex), "the signature is rendered");
            assert!(
                !rendered.contains(&sig_hex[..64]),
                "a part of the signature is rendered"
            );
            assert!(
                !rendered.contains(&sig_hex[64..128]),
                "a part of the signature is rendered"
            );
        }
    }

    /// The artifact of §63.8 step 5: member names, decimal strings, and the round trip.
    #[test]
    fn the_signed_authorization_round_trips() {
        let signed = signed_first_message();
        let document = serde_json::to_value(&signed).expect("a document");
        let expected = json!({
            "token": MESSAGES[0].token,
            "service_provider": MESSAGE_SERVICE_PROVIDER,
            "user": MESSAGES[0].user,
            "receiver": MESSAGES[0].receiver,
            "value": "10000",
            "max_fee": "2000",
            "deadline": "1726207632",
            "version": "1",
            "nonce": "2",
            "sig": signed.sig_hex(),
        });
        assert!(document == expected, "the document differs from the expected one");
        let back: SignedPermit = serde_json::from_value(document).expect("the document reads back");
        assert!(back == signed, "the document does not read back as it was written");
    }

    #[test]
    fn a_changed_signed_authorization_does_not_read_back() {
        let document = serde_json::to_value(signed_first_message()).expect("a document");
        let sig = document["sig"].as_str().expect("a signature member").to_owned();
        let with = |member: &str, value: Json| {
            let mut changed = document.clone();
            changed[member] = value;
            serde_json::from_value::<SignedPermit>(changed).is_ok()
        };
        assert!(with("value", json!("10001")), "a well-formed document is refused");
        assert!(!with("unknown_member", json!("x")), "an unknown member is accepted");
        assert!(!with("version", json!("2")), "another version is accepted");
        assert!(!with("version", json!("")), "an empty version is accepted");
        assert!(!with("version", json!(1)), "a version that is not a string is accepted");
        assert!(!with("value", json!("-1")), "a negative integer is accepted");
        assert!(!with("value", json!("")), "an empty integer is accepted");
        assert!(
            !with("nonce", json!("1e3")),
            "an integer that is not decimal is accepted"
        );
        assert!(
            !with("sig", json!(format!("0x{sig}"))),
            "a prefixed signature is accepted"
        );
        assert!(
            !with("sig", json!(format!("0x{}", &sig[2..]))),
            "a prefixed signature is accepted"
        );
        assert!(!with("sig", json!(&sig[2..])), "a short signature is accepted");
        assert!(!with("sig", json!(format!("{sig}00"))), "a long signature is accepted");
        // The recovery byte 0 or 1 is not the normalized form.
        assert!(
            !with("sig", json!(format!("{}00", &sig[..128]))),
            "a recovery byte 0 is accepted"
        );
    }

    #[test]
    fn the_relay_payload_carries_the_wrapper_members_and_round_trips() {
        let signed = signed_first_message();
        let from = json!({ "derivation_path": "m/44'/195'/0'/0/0" });
        let payload = GasFreeRelayPayload::new(
            &Network::Nile,
            "USDT-TRC20",
            Some(from.clone()),
            signed.clone(),
            1_726_207_632,
        );
        let document = serde_json::to_value(&payload).expect("a document");
        let expected = json!({
            "relay_type": "tron_gasfree",
            "chain_id": "3448148188",
            "coin": "USDT-TRC20",
            "from": from,
            "from_address": MESSAGES[0].user,
            "gasfree_address": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "verifying_contract": "THQGuFzL87ZqhxkgqYEryRAd7gqFqL5rdc",
            "created_at": "2024-09-13T06:07:12Z",
            "signed_authorization": serde_json::to_value(&signed).expect("a document"),
        });
        assert!(document == expected, "the document differs from the expected one");
        let back: GasFreeRelayPayload = serde_json::from_value(document.clone()).expect("the document reads back");
        assert!(back == payload, "the document does not read back as it was written");

        // Without a selector the member is omitted, never null.
        let without = GasFreeRelayPayload::new(&Network::Nile, "USDT-TRC20", None, signed, 1_726_207_632);
        let without = serde_json::to_value(&without).expect("a document");
        assert!(without.get("from").is_none(), "an absent selector is written");

        let with = |member: &str, value: Json| {
            let mut changed = document.clone();
            changed[member] = value;
            serde_json::from_value::<GasFreeRelayPayload>(changed).is_ok()
        };
        assert!(
            !with("relay_type", json!("tron_other")),
            "another relay type is accepted"
        );
        assert!(
            !with("chain_id", json!(3_448_148_188u64)),
            "a chain id that is not a string is accepted"
        );
        assert!(
            !with("created_at", json!("2024-09-13 06:07:12")),
            "another time form is accepted"
        );
        assert!(!with("unknown_member", json!(1)), "an unknown member is accepted");
    }

    #[test]
    fn the_creation_time_is_written_and_read_as_rfc_3339_whole_seconds() {
        let cases = [
            (0u64, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_726_207_632, "2024-09-13T06:07:12Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ];
        for (unix_seconds, text) in cases {
            assert_eq!(rfc3339_seconds::format(unix_seconds), text);
            assert_eq!(rfc3339_seconds::parse(text), Some(unix_seconds));
        }
        for malformed in [
            "",
            "2024-09-13T06:07:12",
            "2024-09-13T06:07:12.000Z",
            "2024-09-13t06:07:12Z",
            "2024-09-13T06:07:12+00:00",
            "2023-02-29T00:00:00Z",
            "2024-13-01T00:00:00Z",
            "2024-00-10T00:00:00Z",
            "2024-09-00T00:00:00Z",
            "2024-09-13T24:00:00Z",
            "2024-09-13T06:60:00Z",
            "1969-12-31T23:59:59Z",
            "+024-09-13T06:07:12Z",
        ] {
            assert_eq!(rfc3339_seconds::parse(malformed), None, "{malformed}");
        }
    }
}
