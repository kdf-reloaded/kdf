//! The typed payloads of the GasFree provider's REST API (CRD ch.63 §63.4.4 to §63.4.7, R5, R6, R12).
//!
//! The member names are the provider's. The dictated quirks are followed: an integer-valued member is read from
//! a JSON number or a decimal string (a negative, fractional or empty value is rejected); the transfer-fee
//! estimate is read under both of its spellings; a lifecycle value outside the published sets, a `version` other
//! than 1, and a hash or signature with a `0x` prefix or of the wrong length fail the decoding of the payload.
//! A member this module does not name is ignored.
//!
//! The record of a transfer ([`TransferRecord`]) is read with these allowances, which the provider's published API
//! and its answers on the Nile test network call for (§63.4.7, status note of 2026-11-22). They touch no member a
//! decision is taken from:
//!
//! - the three times are taken in any form (a number, any text, `null`, absent) and are not used;
//! - `maxFee`, `signature` and `updatedAt` may be absent or `null`, as in a trace answer;
//! - an empty string in an optional `txn*` or `estimatedTotal*` member means the same as its absence: not yet.

use super::super::TronAddress;
use super::authorization::{check_version, parse_decimal_u256, parse_decimal_u64, PermitTransfer, SignedPermit,
                           PERMIT_VERSION};
use ethereum_types::U256;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as Json;
use std::fmt;
use uuid::Uuid;

/// An integer-valued member as text: a non-negative JSON integer or a string, never anything else.
fn integer_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Json::deserialize(deserializer)? {
        Json::Number(number) => number
            .as_u64()
            .map(|value| value.to_string())
            .ok_or_else(|| D::Error::custom("an integer member is not a non-negative integer")),
        Json::String(text) => Ok(text),
        _ => Err(D::Error::custom("an integer member is neither a number nor a string")),
    }
}

/// A 256-bit integer member: a JSON number or a decimal string (§63.4.7).
fn integer_u256<'de, D: Deserializer<'de>>(deserializer: D) -> Result<U256, D::Error> {
    parse_decimal_u256(&integer_text(deserializer)?).map_err(D::Error::custom)
}

/// A 64-bit integer member: a JSON number or a decimal string (§63.4.7).
fn integer_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    parse_decimal_u64(&integer_text(deserializer)?).map_err(D::Error::custom)
}

/// An integer-valued member that may be absent or `null`, as text.
fn optional_integer_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match Json::deserialize(deserializer)? {
        Json::Null => Ok(None),
        Json::Number(number) => number
            .as_u64()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| D::Error::custom("an integer member is not a non-negative integer")),
        Json::String(text) => Ok(Some(text)),
        _ => Err(D::Error::custom("an integer member is neither a number nor a string")),
    }
}

/// Like [`integer_u256`] for a member that may be absent or `null`.
fn optional_u256<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<U256>, D::Error> {
    optional_integer_text(deserializer)?
        .map(|text| parse_decimal_u256(&text).map_err(D::Error::custom))
        .transpose()
}

/// Like [`integer_u64`] for a member that may be absent or `null`.
fn optional_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    optional_integer_text(deserializer)?
        .map(|text| parse_decimal_u64(&text).map_err(D::Error::custom))
        .transpose()
}

/// An optional member that the provider leaves "empty" until the transfer is on chain (§63.4.7): absent, `null`
/// and the empty string are all `None`; anything else is the text of a non-negative integer or of a string.
fn empty_or_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(optional_integer_text(deserializer)?.filter(|text| !text.is_empty()))
}

/// Like [`optional_u256`], with the empty string as an absent member.
fn empty_or_u256<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<U256>, D::Error> {
    empty_or_text(deserializer)?
        .map(|text| parse_decimal_u256(&text).map_err(D::Error::custom))
        .transpose()
}

/// Like [`optional_u64`], with the empty string as an absent member.
fn empty_or_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    empty_or_text(deserializer)?
        .map(|text| parse_decimal_u64(&text).map_err(D::Error::custom))
        .transpose()
}

/// A time of a transfer record as the provider writes it: a number (its decimal text), any string, or nothing.
/// The published API shows ISO 8601 text and the provider sends Unix milliseconds; the value is not used.
fn time_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match Json::deserialize(deserializer)? {
        Json::Null => Ok(None),
        Json::Number(number) => Ok(Some(number.to_string())),
        Json::String(text) => Ok(Some(text)),
        _ => Err(D::Error::custom("a time member is neither a number nor a string")),
    }
}

/// The `txnHash` of a transfer record: `None` when absent, `null` or empty, else a strict [`TransactionHash`].
fn empty_or_hash<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<TransactionHash>, D::Error> {
    match Option::<String>::deserialize(deserializer)? {
        Some(text) if !text.is_empty() => TransactionHash::from_text(&text).map(Some).map_err(D::Error::custom),
        _ => Ok(None),
    }
}

/// The `txnState` of a transfer record: `None` when absent, `null` or empty, else a value of the published set.
fn empty_or_transaction_state<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<TransactionState>, D::Error> {
    match Option::<String>::deserialize(deserializer)? {
        Some(text) if !text.is_empty() => TransactionState::from_wire(&text)
            .map(Some)
            .ok_or_else(|| D::Error::custom("a transaction state outside the published set")),
        _ => Ok(None),
    }
}

/// The `version` member of a provider payload, when it is present: 1 and nothing else (R5).
fn optional_version_one<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    optional_integer_text(deserializer)?
        .map(|text| check_version(&text).map(|()| PERMIT_VERSION).map_err(D::Error::custom))
        .transpose()
}

/// One token of an account record (§63.4.4). Amounts are in the token's smallest unit.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountAsset {
    pub token_address: TronAddress,
    pub token_symbol: String,
    /// The fee of activating the custody account, charged while the account is not active.
    #[serde(deserialize_with = "integer_u256")]
    pub activate_fee: U256,
    #[serde(deserialize_with = "integer_u256")]
    pub transfer_fee: U256,
    /// The token's decimals as the provider knows them.
    #[serde(deserialize_with = "integer_u64")]
    pub decimal: u64,
    /// The amount locked by transfers in flight.
    #[serde(deserialize_with = "integer_u256")]
    pub frozen: U256,
}

/// The provider's record of an account (§63.4.4), the `data` of `GET .../address/{account}`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccount {
    pub account_address: TronAddress,
    /// The provider's view of the custody address; checked against the local derivation before use (R2).
    pub gas_free_address: TronAddress,
    /// Whether the custody account is activated on chain.
    pub active: bool,
    /// The nonce the next authorization carries.
    #[serde(deserialize_with = "integer_u64")]
    pub nonce: u64,
    /// Clear while an earlier transfer of the account is unsettled.
    pub allow_submit: bool,
    pub assets: Vec<AccountAsset>,
}

/// The `data` of `GET .../config/token/all` (§63.4.5): an object that wraps the list as `tokens`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct SupportedTokenList {
    pub tokens: Vec<SupportedToken>,
}

/// One entry of the supported-token list (§63.4.5).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupportedToken {
    pub token_address: TronAddress,
    #[serde(deserialize_with = "integer_u256")]
    pub activate_fee: U256,
    #[serde(deserialize_with = "integer_u256")]
    pub transfer_fee: U256,
    pub symbol: String,
    #[serde(deserialize_with = "integer_u64")]
    pub decimal: u64,
    pub supported: bool,
}

/// The body of `POST .../gasfree/submit` (§63.4.6): the signed authorization under the provider's member names,
/// the integers as decimal strings, the addresses in base58 and the signature as 130 hex digits without `0x`.
///
/// It has no `Debug`: it holds the signature (R9).
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(try_from = "SubmitDocument", into = "SubmitDocument")]
pub(crate) struct SubmitRequest {
    request_id: Option<Uuid>,
    signed: SignedPermit,
}

impl SubmitRequest {
    /// The submit body of `signed`, with the optional caller-chosen `request_id`.
    ///
    /// # Errors
    ///
    /// A text when `request_id` is not a version-4 UUID.
    pub(crate) fn new(signed: SignedPermit, request_id: Option<&str>) -> Result<Self, String> {
        let request_id = request_id.map(parse_request_id).transpose()?;
        Ok(SubmitRequest { request_id, signed })
    }
}

/// A `requestId`: a UUID of version 4 (§63.4.6).
fn parse_request_id(text: &str) -> Result<Uuid, String> {
    let id = Uuid::parse_str(text).map_err(|_| "the request id is not a UUID".to_owned())?;
    if id.get_version_num() != 4 {
        return Err("the request id is not a version 4 UUID".to_owned());
    }
    Ok(id)
}

/// The JSON form of a [`SubmitRequest`].
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
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

impl From<SubmitRequest> for SubmitDocument {
    fn from(request: SubmitRequest) -> Self {
        let permit = request.signed.permit();
        SubmitDocument {
            request_id: request.request_id.map(|id| id.to_string()),
            token: permit.token,
            service_provider: permit.service_provider,
            user: permit.user,
            receiver: permit.receiver,
            value: permit.value.to_string(),
            max_fee: permit.max_fee.to_string(),
            deadline: permit.deadline.to_string(),
            version: PERMIT_VERSION.to_string(),
            nonce: permit.nonce.to_string(),
            sig: request.signed.sig_hex(),
        }
    }
}

impl TryFrom<SubmitDocument> for SubmitRequest {
    type Error = String;

    fn try_from(document: SubmitDocument) -> Result<Self, Self::Error> {
        let request_id = document.request_id.as_deref().map(parse_request_id).transpose()?;
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
        let signed = SignedPermit::from_document_members(permit, &document.version, &document.sig)?;
        Ok(SubmitRequest { request_id, signed })
    }
}

/// The lifecycle state of a transfer at the provider (§63.4.7). A value outside the set fails the decoding (R12).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) enum TransferState {
    #[serde(rename = "WAITING")]
    Waiting,
    #[serde(rename = "INPROGRESS")]
    InProgress,
    #[serde(rename = "CONFIRMING")]
    Confirming,
    #[serde(rename = "SUCCEED")]
    Succeed,
    #[serde(rename = "FAILED")]
    Failed,
}

impl TransferState {
    /// The state as the provider writes it (§63.4.7).
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            TransferState::Waiting => "WAITING",
            TransferState::InProgress => "INPROGRESS",
            TransferState::Confirming => "CONFIRMING",
            TransferState::Succeed => "SUCCEED",
            TransferState::Failed => "FAILED",
        }
    }
}

/// The on-chain state of a transfer's transaction (§63.4.7). A value outside the set fails the decoding (R12).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) enum TransactionState {
    #[serde(rename = "INIT")]
    Init,
    #[serde(rename = "NOT_ON_CHAIN")]
    NotOnChain,
    #[serde(rename = "ON_CHAIN")]
    OnChain,
    #[serde(rename = "SOLIDITY")]
    Solidity,
    #[serde(rename = "ON_CHAIN_FAILED")]
    OnChainFailed,
}

/// A transaction hash as the provider writes it: 64 hex digits without `0x` (§63.4.7).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransactionHash(pub [u8; 32]);

impl TransactionHash {
    /// The hash as 64 lower-case hex digits without `0x`: the form the `tx_hash` of a native TRON withdraw has.
    pub(crate) fn to_hex(self) -> String { hex::encode(self.0) }
}

impl fmt::Debug for TransactionHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "TransactionHash({})", hex::encode(self.0)) }
}

impl TransactionHash {
    /// The hash `text` holds: exactly 64 hex digits, no `0x`.
    fn from_text(text: &str) -> Result<Self, &'static str> {
        if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("a transaction hash is not 64 hex digits");
        }
        let bytes = hex::decode(text).map_err(|_| "a transaction hash is not hex")?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map(TransactionHash)
            .map_err(|_| "a transaction hash is not 32 bytes")
    }
}

impl<'de> Deserialize<'de> for TransactionHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_text(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl TransactionState {
    /// The state the provider writes as `text`, when it is one of the published set.
    fn from_wire(text: &str) -> Option<Self> {
        match text {
            "INIT" => Some(TransactionState::Init),
            "NOT_ON_CHAIN" => Some(TransactionState::NotOnChain),
            "ON_CHAIN" => Some(TransactionState::OnChain),
            "SOLIDITY" => Some(TransactionState::Solidity),
            "ON_CHAIN_FAILED" => Some(TransactionState::OnChainFailed),
            _ => None,
        }
    }
}

/// A signature member of a provider payload, checked and then dropped: only its form matters here (130 hex
/// digits without `0x`, §63.4.7), and the value is not kept (R9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WellFormedSignature;

impl<'de> Deserialize<'de> for WellFormedSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 130 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(D::Error::custom("a signature is not 130 hex digits"));
        }
        Ok(WellFormedSignature)
    }
}

/// The provider's view of a transfer (§63.4.7): the `data` of the submit response and of the trace response.
/// The members only one of the two responses carries are optional. Amounts are in the token's smallest unit; the
/// provider's times are kept as text and not used.
///
/// The members the flow reads (`id`, the five addresses, `amount`, `nonce`, `state`, `txnState`, `txnHash` and a
/// present `version`) are read strictly.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferRecord {
    /// The trace id.
    pub id: String,
    /// Not parsed: ISO 8601 text or the decimal text of Unix milliseconds.
    #[serde(default, deserialize_with = "time_text")]
    pub created_at: Option<String>,
    /// Like `created_at`. A trace response may lack it.
    #[serde(default, deserialize_with = "time_text")]
    pub updated_at: Option<String>,
    /// Like `created_at`.
    #[serde(default, deserialize_with = "time_text")]
    pub expired_at: Option<String>,
    pub account_address: TronAddress,
    pub gas_free_address: TronAddress,
    pub provider_address: TronAddress,
    pub target_address: TronAddress,
    pub token_address: TronAddress,
    #[serde(deserialize_with = "integer_u256")]
    pub amount: U256,
    /// A trace response may lack it.
    #[serde(default, deserialize_with = "optional_u256")]
    pub max_fee: Option<U256>,
    /// Checked for its form, when it is present, and not kept. A trace response may lack it.
    #[serde(default)]
    pub signature: Option<WellFormedSignature>,
    #[serde(deserialize_with = "integer_u64")]
    pub nonce: u64,
    pub state: TransferState,
    #[serde(deserialize_with = "integer_u256")]
    pub estimated_activate_fee: U256,
    /// The provider spells this member with and without the `d` of "estimated" (§63.4.7).
    #[serde(alias = "estimateTransferFee", deserialize_with = "integer_u256")]
    pub estimated_transfer_fee: U256,
    /// 1 when the member is present: another value fails the decoding (R5).
    #[serde(default, deserialize_with = "optional_version_one")]
    pub version: Option<u64>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub estimated_total_fee: Option<U256>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub estimated_total_cost: Option<U256>,
    /// The transaction of the transfer, once it is on chain. Absent, `null` or empty before that.
    #[serde(default, deserialize_with = "empty_or_hash")]
    pub txn_hash: Option<TransactionHash>,
    #[serde(default, deserialize_with = "empty_or_u64")]
    pub txn_block_num: Option<u64>,
    /// Unix milliseconds.
    #[serde(default, deserialize_with = "empty_or_u64")]
    pub txn_block_timestamp: Option<u64>,
    /// Absent, `null` or empty before the transfer has a transaction.
    #[serde(default, deserialize_with = "empty_or_transaction_state")]
    pub txn_state: Option<TransactionState>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub txn_activate_fee: Option<U256>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub txn_transfer_fee: Option<U256>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub txn_total_fee: Option<U256>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub txn_amount: Option<U256>,
    #[serde(default, deserialize_with = "empty_or_u256")]
    pub txn_total_cost: Option<U256>,
}

#[cfg(test)]
mod tests {
    use super::super::super::Network;
    use super::super::authorization::sign_permit;
    use super::super::test_vectors::{address, MESSAGES, MESSAGE_MAX_FEE, MESSAGE_SERVICE_PROVIDER};
    use super::*;
    use mm2_eth::keys::Secret;
    use serde_json::json;

    const REQUEST_ID_V4: &str = "3f2b8c1e-5d4a-4f6b-9c7d-8e9f0a1b2c3d";
    const REQUEST_ID_V1: &str = "3f2b8c1e-5d4a-1f6b-9c7d-8e9f0a1b2c3d";
    const TXN_HASH: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn signed_first_message() -> SignedPermit {
        let message = &MESSAGES[0];
        let permit = PermitTransfer {
            token: address(message.token),
            service_provider: address(MESSAGE_SERVICE_PROVIDER),
            user: address(message.user),
            receiver: address(message.receiver),
            value: U256::from(message.value),
            max_fee: U256::from(MESSAGE_MAX_FEE),
            deadline: message.deadline,
            nonce: message.nonce,
        };
        let mut key = [0u8; 32];
        key[31] = 1;
        let secret = Secret::from_slice(&key).expect("a valid test key");
        sign_permit(&Network::Nile, permit, &secret, message.deadline - 1).expect("the user signs")
    }

    fn account_json() -> Json {
        json!({
            "accountAddress": MESSAGES[0].user,
            "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "active": false,
            "nonce": 2,
            "allowSubmit": true,
            "assets": [{
                "tokenAddress": MESSAGES[0].token,
                "tokenSymbol": "USDT",
                "activateFee": 1000000,
                "transferFee": "2000000",
                "decimal": 6,
                "frozen": "0"
            }],
            "aMemberThisBuildDoesNotKnow": true
        })
    }

    fn transfer_json() -> Json {
        json!({
            "id": "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0",
            "createdAt": "2024-09-13T06:07:12.000Z",
            "updatedAt": "2024-09-13T06:07:13.000Z",
            "expiredAt": "2024-09-13T06:12:12.000Z",
            "maxFee": 2000,
            "signature": signed_first_message().sig_hex(),
            "accountAddress": MESSAGES[0].user,
            "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "providerAddress": MESSAGE_SERVICE_PROVIDER,
            "targetAddress": MESSAGES[0].receiver,
            "tokenAddress": MESSAGES[0].token,
            "amount": "10000",
            "estimatedActivateFee": 1000000,
            "estimatedTransferFee": "2000000",
            "nonce": "2",
            "state": "WAITING"
        })
    }

    /// `document` with `member` set to `value` (or removed, for `None`), decoded as `T`.
    fn decodes<T: serde::de::DeserializeOwned>(document: &Json, member: &str, value: Option<Json>) -> bool {
        let mut changed = document.clone();
        match value {
            Some(value) => changed[member] = value,
            None => {
                if let Some(object) = changed.as_object_mut() {
                    object.remove(member);
                }
            },
        }
        serde_json::from_value::<T>(changed).is_ok()
    }

    #[test]
    fn an_account_record_reads_numbers_and_strings() {
        let account: ProviderAccount = serde_json::from_value(account_json()).expect("an account record");
        assert_eq!(account.account_address, address(MESSAGES[0].user));
        assert_eq!(
            account.gas_free_address.to_base58(),
            "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye"
        );
        assert!(!account.active);
        assert!(account.allow_submit);
        assert_eq!(account.nonce, 2);
        let asset = &account.assets[0];
        assert_eq!(asset.token_address, address(MESSAGES[0].token));
        assert_eq!(asset.token_symbol, "USDT");
        assert_eq!(asset.activate_fee, U256::from(1_000_000u64));
        assert_eq!(asset.transfer_fee, U256::from(2_000_000u64));
        assert_eq!(asset.decimal, 6);
        assert_eq!(asset.frozen, U256::zero());

        let document = account_json();
        assert!(decodes::<ProviderAccount>(&document, "nonce", Some(json!("7"))));
        for bad in [
            json!(-1),
            json!(""),
            json!("-1"),
            json!(1.5),
            json!("1.5"),
            json!(null),
            json!(true),
        ] {
            assert!(
                !decodes::<ProviderAccount>(&document, "nonce", Some(bad)),
                "a malformed integer is accepted"
            );
        }
        assert!(!decodes::<ProviderAccount>(
            &document,
            "gasFreeAddress",
            Some(json!("not an address"))
        ));
        assert!(!decodes::<ProviderAccount>(&document, "allowSubmit", None));
        assert!(!decodes::<ProviderAccount>(&document, "assets", None));
    }

    #[test]
    fn a_supported_token_list_reads_from_the_tokens_member() {
        let data = json!({ "tokens": [
            { "tokenAddress": MESSAGES[0].token, "createdAt": "2024-09-13T06:07:12.000+00:00",
              "updatedAt": "2024-09-13T06:07:12.000+00:00", "activateFee": 1000000, "transferFee": 2000000,
              "supported": true, "symbol": "USDT", "decimal": 6 },
            { "tokenAddress": MESSAGES[1].token, "activateFee": "0", "transferFee": "0", "symbol": "X", "decimal": "18",
              "supported": false }
        ]});
        let tokens = serde_json::from_value::<SupportedTokenList>(data.clone())
            .expect("a token list")
            .tokens;
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_address, address(MESSAGES[0].token));
        assert_eq!(tokens[0].activate_fee, U256::from(1_000_000u64));
        assert_eq!(tokens[0].transfer_fee, U256::from(2_000_000u64));
        assert_eq!(tokens[0].decimal, 6);
        assert!(tokens[0].supported);
        assert_eq!(tokens[1].decimal, 18);
        assert!(!tokens[1].supported);
        // The bare array is not the provider's form.
        assert!(serde_json::from_value::<SupportedTokenList>(data["tokens"].clone()).is_err());
        assert!(!decodes::<SupportedTokenList>(&data, "tokens", None));
    }

    /// ch.63 T5: the submit payload contract.
    #[test]
    fn the_submit_payload_follows_the_contract() {
        let signed = signed_first_message();
        let request = SubmitRequest::new(signed.clone(), Some(REQUEST_ID_V4)).expect("a version 4 request id");
        let document = serde_json::to_value(&request).expect("a document");
        let expected = json!({
            "requestId": REQUEST_ID_V4,
            "token": MESSAGES[0].token,
            "serviceProvider": MESSAGE_SERVICE_PROVIDER,
            "user": MESSAGES[0].user,
            "receiver": MESSAGES[0].receiver,
            "value": "10000",
            "maxFee": "2000",
            "deadline": "1726207632",
            "version": "1",
            "nonce": "2",
            "sig": signed.sig_hex(),
        });
        assert!(document == expected, "the document differs from the expected one");
        let sig = signed.sig_hex();
        assert!(
            sig.len() == 130 && !sig.starts_with("0x"),
            "the signature is not 130 bare hex digits"
        );
        let back: SubmitRequest = serde_json::from_value(document.clone()).expect("the document reads back");
        assert!(back == request, "the document does not read back as it was written");

        // The request id is optional and omitted when absent.
        let without = SubmitRequest::new(signed.clone(), None).expect("no request id");
        let without = serde_json::to_value(&without).expect("a document");
        assert!(without.get("requestId").is_none(), "an absent request id is written");

        assert!(SubmitRequest::new(signed.clone(), Some(REQUEST_ID_V1)).is_err());
        assert!(SubmitRequest::new(signed, Some("not-a-uuid")).is_err());
        assert!(!decodes::<SubmitRequest>(
            &document,
            "requestId",
            Some(json!(REQUEST_ID_V1))
        ));
        assert!(!decodes::<SubmitRequest>(&document, "version", Some(json!("2"))));
        assert!(!decodes::<SubmitRequest>(&document, "version", Some(json!("0"))));
        assert!(!decodes::<SubmitRequest>(
            &document,
            "sig",
            Some(json!(format!("0x{sig}")))
        ));
        assert!(!decodes::<SubmitRequest>(&document, "sig", Some(json!(&sig[..128]))));
        assert!(!decodes::<SubmitRequest>(
            &document,
            "sig",
            Some(json!("zz".repeat(65)))
        ));
        assert!(!decodes::<SubmitRequest>(&document, "value", Some(json!("-5"))));
        assert!(!decodes::<SubmitRequest>(&document, "value", Some(json!(""))));
    }

    /// ch.63 T6: provider-payload drift.
    #[test]
    fn a_transfer_record_is_read_strictly() {
        let document = transfer_json();
        let record: TransferRecord = serde_json::from_value(document.clone()).expect("a submit response");
        assert_eq!(record.id, "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0");
        assert_eq!(record.amount, U256::from(10_000u64));
        assert_eq!(record.estimated_activate_fee, U256::from(1_000_000u64));
        assert_eq!(record.estimated_transfer_fee, U256::from(2_000_000u64));
        assert_eq!(record.nonce, 2);
        assert_eq!(record.state, TransferState::Waiting);
        assert_eq!(record.max_fee, Some(U256::from(2_000u64)));
        assert_eq!(record.created_at.as_deref(), Some("2024-09-13T06:07:12.000Z"));
        assert_eq!(record.expired_at.as_deref(), Some("2024-09-13T06:12:12.000Z"));
        assert_eq!(record.version, None);
        assert_eq!(record.txn_hash, None);
        assert_eq!(record.txn_state, None);
        assert_eq!(record.txn_total_cost, None);
        // The members the flow reads are required.
        for member in [
            "id",
            "accountAddress",
            "gasFreeAddress",
            "providerAddress",
            "targetAddress",
            "tokenAddress",
            "nonce",
            "state",
            "amount",
        ] {
            assert!(!decodes::<TransferRecord>(&document, member, None), "{member}");
            assert!(
                !decodes::<TransferRecord>(&document, member, Some(json!(null))),
                "{member}"
            );
        }
        // An empty string is not an address and not an integer.
        for member in ["accountAddress", "targetAddress", "tokenAddress", "nonce", "amount"] {
            assert!(
                !decodes::<TransferRecord>(&document, member, Some(json!(""))),
                "{member}"
            );
        }
        // A present `maxFee` keeps the integer rules.
        for bad in [json!(-1), json!(""), json!("1.5")] {
            assert!(!decodes::<TransferRecord>(&document, "maxFee", Some(bad)));
        }

        // The members only a trace response carries.
        let mut trace = document.clone();
        for (member, value) in [
            ("estimatedTotalFee", json!(3000000)),
            ("estimatedTotalCost", json!("3010000")),
            ("txnBlockNum", json!(52537691)),
            ("txnBlockTimestamp", json!(1726207635000u64)),
            ("txnActivateFee", json!(1000000)),
            ("txnTransferFee", json!(2000000)),
            ("txnTotalFee", json!(3000000)),
            ("txnAmount", json!(10000)),
            ("txnTotalCost", json!(3010000)),
            ("version", json!(1)),
        ] {
            trace[member] = value;
        }
        let record: TransferRecord = serde_json::from_value(trace).expect("a trace response");
        assert_eq!(record.estimated_total_fee, Some(U256::from(3_000_000u64)));
        assert_eq!(record.estimated_total_cost, Some(U256::from(3_010_000u64)));
        assert_eq!(record.txn_block_num, Some(52_537_691));
        assert_eq!(record.txn_block_timestamp, Some(1_726_207_635_000));
        assert_eq!(record.txn_activate_fee, Some(U256::from(1_000_000u64)));
        assert_eq!(record.txn_transfer_fee, Some(U256::from(2_000_000u64)));
        assert_eq!(record.txn_total_fee, Some(U256::from(3_000_000u64)));
        assert_eq!(record.txn_amount, Some(U256::from(10_000u64)));
        assert_eq!(record.txn_total_cost, Some(U256::from(3_010_000u64)));
        assert_eq!(record.version, Some(1));
        assert!(!decodes::<TransferRecord>(&document, "txnTotalCost", Some(json!(-1))));
        assert!(!decodes::<TransferRecord>(&document, "txnBlockNum", Some(json!("x"))));

        // Both spellings of the transfer-fee estimate.
        let mut other_spelling = document.clone();
        let object = other_spelling.as_object_mut().expect("an object");
        let fee = object.remove("estimatedTransferFee").expect("the member");
        object.insert("estimateTransferFee".to_owned(), fee);
        let record: TransferRecord = serde_json::from_value(other_spelling).expect("the other spelling");
        assert_eq!(record.estimated_transfer_fee, U256::from(2_000_000u64));
        assert!(!decodes::<TransferRecord>(&document, "estimatedTransferFee", None));

        // Every lifecycle value of the two sets, and nothing else.
        for (text, state) in [
            ("WAITING", TransferState::Waiting),
            ("INPROGRESS", TransferState::InProgress),
            ("CONFIRMING", TransferState::Confirming),
            ("SUCCEED", TransferState::Succeed),
            ("FAILED", TransferState::Failed),
        ] {
            let mut changed = document.clone();
            changed["state"] = json!(text);
            let record: TransferRecord = serde_json::from_value(changed).expect("a known state");
            assert_eq!(record.state, state);
        }
        for (text, state) in [
            ("INIT", TransactionState::Init),
            ("NOT_ON_CHAIN", TransactionState::NotOnChain),
            ("ON_CHAIN", TransactionState::OnChain),
            ("SOLIDITY", TransactionState::Solidity),
            ("ON_CHAIN_FAILED", TransactionState::OnChainFailed),
        ] {
            let mut changed = document.clone();
            changed["txnState"] = json!(text);
            let record: TransferRecord = serde_json::from_value(changed).expect("a known state");
            assert_eq!(record.txn_state, Some(state));
        }
        for unknown in ["DONE", "waiting", ""] {
            assert!(!decodes::<TransferRecord>(&document, "state", Some(json!(unknown))));
        }
        for unknown in ["DONE", "on_chain", " "] {
            assert!(!decodes::<TransferRecord>(&document, "txnState", Some(json!(unknown))));
        }
        assert!(!decodes::<TransferRecord>(&document, "txnState", Some(json!(1))));

        // The transaction hash: 64 bare hex digits.
        let mut on_chain = document.clone();
        on_chain["txnHash"] = json!(TXN_HASH);
        let record: TransferRecord = serde_json::from_value(on_chain).expect("a trace response");
        assert_eq!(
            record.txn_hash.map(|hash| hex::encode(hash.0)).as_deref(),
            Some(TXN_HASH)
        );
        for bad in [
            format!("0x{TXN_HASH}"),
            format!("0x{}", &TXN_HASH[2..]),
            TXN_HASH[2..].to_owned(),
            "zz".repeat(32),
        ] {
            assert!(!decodes::<TransferRecord>(&document, "txnHash", Some(json!(bad))));
        }

        // A signature member: 130 bare hex digits.
        let sig = signed_first_message().sig_hex();
        assert!(decodes::<TransferRecord>(&document, "signature", Some(json!(sig))));
        assert!(!decodes::<TransferRecord>(
            &document,
            "signature",
            Some(json!(format!("0x{sig}")))
        ));
        assert!(!decodes::<TransferRecord>(
            &document,
            "signature",
            Some(json!(&sig[..128]))
        ));

        // The version: 1 as a number or a string, and nothing else.
        assert!(decodes::<TransferRecord>(&document, "version", Some(json!("1"))));
        for bad in [json!(2), json!("2"), json!(0), json!("")] {
            assert!(!decodes::<TransferRecord>(&document, "version", Some(bad)));
        }
        // An absent version is accepted.
        assert!(decodes::<TransferRecord>(&document, "version", None));

        // Integers: negative or empty values are rejected.
        for bad in [json!(-1), json!(""), json!("-1")] {
            assert!(!decodes::<TransferRecord>(&document, "amount", Some(bad)));
        }
    }

    /// A submit answer as the provider's published API shows it: ISO times, numbers, the fee estimate without the
    /// `d`, `signature`, and no `version`.
    fn documented_submit_answer() -> Json {
        json!({
            "id": "6c3ff67e-0bf4-4c09-91ca-0c7c254b01a0",
            "createdAt": "2024-09-10T08:11:50.822+00:00",
            "updatedAt": "2024-09-10T08:11:50.822+00:00",
            "accountAddress": MESSAGES[0].user,
            "gasFreeAddress": "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "providerAddress": MESSAGE_SERVICE_PROVIDER,
            "targetAddress": MESSAGES[0].receiver,
            "tokenAddress": MESSAGES[0].token,
            "amount": 10000,
            "maxFee": 2000,
            "signature": signed_first_message().sig_hex(),
            "nonce": 2,
            "expiredAt": "2024-09-10T08:16:50.000+00:00",
            "state": "WAITING",
            "estimatedActivateFee": 1000000,
            "estimateTransferFee": 2000000
        })
    }

    /// §63.4.7, status note of 2026-11-22: the submit answer of the published API decodes, and so does the same
    /// answer with the times as Unix milliseconds, as the provider sends them.
    #[test]
    fn a_submit_answer_decodes_with_iso_times_and_with_millisecond_times() {
        let documented = documented_submit_answer();
        let record: TransferRecord = serde_json::from_value(documented.clone()).expect("the documented shape");
        assert_eq!(record.created_at.as_deref(), Some("2024-09-10T08:11:50.822+00:00"));
        assert_eq!(record.amount, U256::from(10_000u64));
        assert_eq!(record.max_fee, Some(U256::from(2_000u64)));
        assert_eq!(record.estimated_transfer_fee, U256::from(2_000_000u64));
        assert_eq!(record.nonce, 2);
        assert_eq!(record.version, None);
        assert!(record.signature.is_some());

        let mut live = documented;
        live["createdAt"] = json!(1_795_354_473_207u64);
        live["updatedAt"] = json!(1_795_354_473_207u64);
        live["expiredAt"] = json!(1_795_354_773_000u64);
        let record: TransferRecord = serde_json::from_value(live.clone()).expect("the live shape");
        assert_eq!(record.created_at.as_deref(), Some("1795354473207"));
        assert_eq!(record.expired_at.as_deref(), Some("1795354773000"));
        assert_eq!(record.state, TransferState::Waiting);

        // A time is taken in any form of a number or a string, and as nothing; never as another JSON type.
        for time in [json!(null), json!(""), json!("yesterday"), json!(1.5), json!(-1)] {
            assert!(decodes::<TransferRecord>(&live, "createdAt", Some(time)));
        }
        for member in ["createdAt", "updatedAt", "expiredAt"] {
            assert!(decodes::<TransferRecord>(&live, member, None));
            assert!(!decodes::<TransferRecord>(&live, member, Some(json!({ "at": 1 }))));
            assert!(!decodes::<TransferRecord>(&live, member, Some(json!([1]))));
        }
    }

    /// §63.4.7, status note of 2026-11-22: a trace answer without `signature`, `maxFee`, `updatedAt` and `version`
    /// decodes, with the transaction members empty, `null` or absent before the transfer is on chain.
    #[test]
    fn a_trace_answer_decodes_without_the_submit_only_members_and_with_empty_transaction_members() {
        const NOT_YET: [&str; 11] = [
            "estimatedTotalFee",
            "estimatedTotalCost",
            "txnHash",
            "txnBlockNum",
            "txnBlockTimestamp",
            "txnState",
            "txnActivateFee",
            "txnTransferFee",
            "txnTotalFee",
            "txnAmount",
            "txnTotalCost",
        ];
        let mut trace = documented_submit_answer();
        let object = trace.as_object_mut().expect("an object");
        for member in ["signature", "maxFee", "updatedAt"] {
            object.remove(member);
        }
        object.insert("estimatedTransferFee".to_owned(), json!(2000000));
        object.remove("estimateTransferFee");
        object.insert("state".to_owned(), json!("INPROGRESS"));

        for empty in [Some(json!("")), Some(json!(null)), None] {
            let mut document = trace.clone();
            if let Some(empty) = &empty {
                for member in NOT_YET {
                    document[member] = empty.clone();
                }
            }
            let record: TransferRecord = serde_json::from_value(document).expect("a trace answer");
            assert_eq!(record.state, TransferState::InProgress);
            assert_eq!(record.max_fee, None);
            assert_eq!(record.signature, None);
            assert_eq!(record.updated_at, None);
            assert_eq!(record.version, None);
            assert_eq!(record.txn_hash, None);
            assert_eq!(record.txn_state, None);
            assert_eq!(record.txn_block_num, None);
            assert_eq!(record.txn_block_timestamp, None);
            assert_eq!(record.estimated_total_fee, None);
            assert_eq!(record.txn_total_cost, None);
        }
        // `null` for the two members a trace answer may lack.
        assert!(decodes::<TransferRecord>(&trace, "maxFee", Some(json!(null))));
        assert!(decodes::<TransferRecord>(&trace, "signature", Some(json!(null))));
        // An empty string is "not yet" for the optional members only.
        assert!(!decodes::<TransferRecord>(&trace, "signature", Some(json!(""))));
        assert!(!decodes::<TransferRecord>(&trace, "nonce", Some(json!(""))));
        assert!(!decodes::<TransferRecord>(
            &trace,
            "estimatedActivateFee",
            Some(json!(""))
        ));

        // On chain: the members are read strictly again.
        let mut settled = trace.clone();
        settled["state"] = json!("SUCCEED");
        settled["txnHash"] = json!(TXN_HASH);
        settled["txnState"] = json!("ON_CHAIN");
        settled["txnBlockNum"] = json!(52537691);
        settled["txnBlockTimestamp"] = json!(1_795_354_476_000u64);
        settled["txnTotalCost"] = json!("3010000");
        let record: TransferRecord = serde_json::from_value(settled).expect("a settled trace answer");
        assert_eq!(record.txn_state, Some(TransactionState::OnChain));
        assert_eq!(record.txn_hash.map(TransactionHash::to_hex).as_deref(), Some(TXN_HASH));
        assert_eq!(record.txn_block_timestamp, Some(1_795_354_476_000));
        assert_eq!(record.txn_total_cost, Some(U256::from(3_010_000u64)));
    }
}
