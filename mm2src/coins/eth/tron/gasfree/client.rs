//! GasFree provider REST API client (§49.4, dictated interop).
//!
//! Models the provider's public REST surface: the `{code, reason, message,
//! data}` response envelope (§49.4.3), the HMAC-SHA256 request-auth headers
//! (§49.4.2), the account-info / supported-tokens payloads (§49.4.4/§49.4.5),
//! the submit request payload (§49.4.6), and the submit/trace response payloads
//! with strict lifecycle-state parsing (§49.4.7 / R12).
//!
//! Today the GET reads (account info, supported tokens, trace) use the
//! cross-platform header-bearing GET capability ([`slurp_url_with_headers`]);
//! the POST submit is **deferred** (D-submit) and returns not-implemented.

use super::error::{sanitize_provider_message, GasFreeProviderError};
use crate::eth::tron::address::TronAddress;
use crate::eth::tron::Network;
use ethereum_types::U256;
use http::StatusCode;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Request authentication (§49.4.2)
// ---------------------------------------------------------------------------

/// HMAC-SHA256 over `msg` keyed by `key` (manual construction; the `hmac` crate
/// is not a dependency of this crate).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = {
        let mut h = Sha256::new();
        h.update(ipad);
        h.update(msg);
        h.finalize()
    };
    let outer = {
        let mut h = Sha256::new();
        h.update(opad);
        h.update(inner);
        h.finalize()
    };
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

/// Build the two request-auth headers (§49.4.2): `Timestamp` and
/// `Authorization: ApiKey {api_key}:{base64(hmac_sha256(secret, method‖path‖ts))}`.
pub fn build_auth_headers(
    method: &str,
    path: &str,
    api_key: &str,
    api_secret: &str,
    timestamp_secs: u64,
) -> Vec<(String, String)> {
    let ts = timestamp_secs.to_string();
    let mut msg = String::with_capacity(method.len() + path.len() + ts.len());
    msg.push_str(method);
    msg.push_str(path);
    msg.push_str(&ts);
    let mac = hmac_sha256(api_secret.as_bytes(), msg.as_bytes());
    let signature = base64::encode(&mac);
    vec![
        ("Timestamp".to_string(), ts),
        ("Authorization".to_string(), format!("ApiKey {api_key}:{signature}")),
    ]
}

// ---------------------------------------------------------------------------
// Endpoint paths (§49.4.1)
// ---------------------------------------------------------------------------

const API_PREFIX: &str = "api/v1";

fn segment(network: &Network) -> &'static str { super::config::network_path_segment(network) }

pub fn path_supported_tokens(network: &Network) -> String {
    format!("/{}/{}/config/token/all", segment(network), API_PREFIX)
}

pub fn path_account(network: &Network, account_address: &str) -> String {
    format!("/{}/{}/address/{}", segment(network), API_PREFIX, account_address)
}

pub fn path_submit(network: &Network) -> String { format!("/{}/{}/gasfree/submit", segment(network), API_PREFIX) }

pub fn path_trace(network: &Network, trace_id: &str) -> String {
    format!("/{}/{}/gasfree/{}", segment(network), API_PREFIX, trace_id)
}

// ---------------------------------------------------------------------------
// Response envelope (§49.4.3)
// ---------------------------------------------------------------------------

/// Business-success code in the provider envelope (§49.4.3).
const ENVELOPE_OK: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub code: i64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default = "none")]
    pub data: Option<T>,
}

fn none<T>() -> Option<T> { None }

/// Interpret a provider HTTP response (status + raw body) into either the typed
/// `data` payload or a classified provider error (§49.4.3 / §49.9 / R11).
///
/// A business-success requires HTTP 2xx, envelope `code == 200`, and a present
/// `data`; any other combination is an error even at HTTP 200.
pub fn interpret_response<T>(status: StatusCode, body: &[u8]) -> Result<T, GasFreeProviderError>
where
    T: serde::de::DeserializeOwned,
{
    // Non-2xx HTTP statuses map by class (§49.9), folding any provider message.
    if !status.is_success() {
        let msg = extract_provider_message(body);
        return Err(map_status_class(status, msg));
    }

    let envelope: ApiEnvelope<T> =
        serde_json::from_slice(body).map_err(|e| GasFreeProviderError::InvalidResponse(e.to_string()))?;

    if envelope.code == ENVELOPE_OK {
        return envelope
            .data
            .ok_or_else(|| GasFreeProviderError::InvalidResponse("success envelope missing 'data'".to_owned()));
    }

    // 200 HTTP but a non-200 envelope code → fold the envelope code by class.
    let msg = sanitize_provider_message(&combine_msg(&envelope.message, envelope.reason.as_deref()));
    Err(map_envelope_code(envelope.code, msg))
}

fn combine_msg(message: &str, reason: Option<&str>) -> String {
    match reason {
        Some(r) if !r.is_empty() => format!("{message} ({r})"),
        _ => message.to_string(),
    }
}

/// Best-effort extraction of a provider message from a (possibly error) body.
fn extract_provider_message(body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(default)]
        message: String,
        #[serde(default)]
        reason: Option<String>,
    }
    match serde_json::from_slice::<Partial>(body) {
        Ok(p) => sanitize_provider_message(&combine_msg(&p.message, p.reason.as_deref())),
        Err(_) => String::new(),
    }
}

fn map_status_class(status: StatusCode, msg: String) -> GasFreeProviderError {
    match status.as_u16() {
        400 => GasFreeProviderError::ProviderBadRequest(msg),
        401 => GasFreeProviderError::Unauthorized,
        403 => GasFreeProviderError::Forbidden,
        429 => GasFreeProviderError::RateLimited,
        s if (400..500).contains(&s) => GasFreeProviderError::ProviderBadRequest(msg),
        _ => GasFreeProviderError::Upstream(msg),
    }
}

fn map_envelope_code(code: i64, msg: String) -> GasFreeProviderError {
    match code {
        400 => GasFreeProviderError::ProviderBadRequest(msg),
        401 => GasFreeProviderError::Unauthorized,
        403 => GasFreeProviderError::Forbidden,
        429 => GasFreeProviderError::RateLimited,
        c if (400..500).contains(&c) => GasFreeProviderError::ProviderBadRequest(msg),
        c if (500..600).contains(&c) => GasFreeProviderError::Upstream(msg),
        _ => GasFreeProviderError::InvalidResponse(format!("unexpected envelope code {code}: {msg}")),
    }
}

// ---------------------------------------------------------------------------
// Flexible numeric deserialization (provider sends ints as strings or numbers)
// ---------------------------------------------------------------------------

fn de_u256<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = U256;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a u256 as a decimal string or integer")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<U256, E> { Ok(U256::from(v)) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<U256, E> {
            u64::try_from(v).map(U256::from).map_err(|_| E::custom("negative u256"))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<U256, E> {
            U256::from_dec_str(v.trim()).map_err(|_| E::custom(format!("invalid u256 '{v}'")))
        }
    }
    d.deserialize_any(V)
}

fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a u64 as a decimal string or integer")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom("negative u64"))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            v.trim().parse().map_err(|_| E::custom(format!("invalid u64 '{v}'")))
        }
    }
    d.deserialize_any(V)
}

// ---------------------------------------------------------------------------
// Account info payload (§49.4.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
pub struct AccountInfo {
    #[serde(rename = "accountAddress")]
    pub account_address: String,
    #[serde(rename = "gasFreeAddress")]
    pub gas_free_address: String,
    pub active: bool,
    #[serde(deserialize_with = "de_u64")]
    pub nonce: u64,
    #[serde(rename = "allowSubmit", default)]
    pub allow_submit: bool,
    #[serde(default)]
    pub assets: Vec<AccountAsset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccountAsset {
    #[serde(rename = "tokenAddress")]
    pub token_address: String,
    #[serde(rename = "tokenSymbol", default)]
    pub token_symbol: String,
    #[serde(rename = "activateFee", deserialize_with = "de_u256")]
    pub activate_fee: U256,
    #[serde(rename = "transferFee", deserialize_with = "de_u256")]
    pub transfer_fee: U256,
    pub decimal: u8,
    #[serde(default, deserialize_with = "de_u256")]
    pub frozen: U256,
}

// ---------------------------------------------------------------------------
// Supported-tokens payload (§49.4.5)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
pub struct SupportedToken {
    #[serde(rename = "tokenAddress")]
    pub token_address: String,
    #[serde(rename = "activateFee", deserialize_with = "de_u256")]
    pub activate_fee: U256,
    #[serde(rename = "transferFee", deserialize_with = "de_u256")]
    pub transfer_fee: U256,
    #[serde(default)]
    pub symbol: String,
    pub decimal: u8,
    #[serde(default)]
    pub supported: bool,
}

// ---------------------------------------------------------------------------
// Submit request payload (§49.4.6)
// ---------------------------------------------------------------------------

/// The signed transfer authorization, serialized for `POST .../gasfree/submit`.
/// Integer-valued fields are decimal strings, addresses are base58, and the
/// signature is 130 hex chars without `0x` (§49.4.6 / R5 / R6).
#[derive(Clone, Debug, Serialize)]
pub struct SubmitRequest {
    #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub token: String,
    #[serde(rename = "serviceProvider")]
    pub service_provider: String,
    pub user: String,
    pub receiver: String,
    pub value: String,
    #[serde(rename = "maxFee")]
    pub max_fee: String,
    pub deadline: String,
    pub version: String,
    pub nonce: String,
    pub sig: String,
}

#[derive(Clone, Debug)]
pub struct SubmitFields {
    pub request_id: Option<uuid::Uuid>,
    pub token: TronAddress,
    pub service_provider: TronAddress,
    pub user: TronAddress,
    pub receiver: TronAddress,
    pub value: U256,
    pub max_fee: U256,
    pub deadline: u64,
    pub version: u64,
    pub nonce: u64,
    /// 65-byte signature serialized as 130 hex chars, no `0x` (R6).
    pub sig_hex: String,
}

impl SubmitRequest {
    /// Build and validate the submit payload (T5): the `requestId` (if any)
    /// must be a UUIDv4, `version` must be 1 (R5), and `sig_hex` must be exactly
    /// 130 lowercase hex chars without `0x` (R6).
    pub fn new(fields: SubmitFields) -> Result<Self, GasFreeProviderError> {
        if let Some(id) = &fields.request_id {
            if id.get_version_num() != 4 {
                return Err(GasFreeProviderError::InvalidRequest(
                    "requestId must be a UUIDv4".to_owned(),
                ));
            }
        }
        if fields.version != super::permit::PERMIT_VERSION {
            return Err(GasFreeProviderError::InvalidRequest(format!(
                "version must be {}",
                super::permit::PERMIT_VERSION
            )));
        }
        let sig = fields.sig_hex.strip_prefix("0x").unwrap_or(&fields.sig_hex);
        if sig.len() != 130 || !sig.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(GasFreeProviderError::InvalidRequest(
                "signature must be 130 hex chars without 0x".to_owned(),
            ));
        }

        Ok(SubmitRequest {
            request_id: fields.request_id.map(|u| u.to_string()),
            token: fields.token.to_base58(),
            service_provider: fields.service_provider.to_base58(),
            user: fields.user.to_base58(),
            receiver: fields.receiver.to_base58(),
            value: fields.value.to_string(),
            max_fee: fields.max_fee.to_string(),
            deadline: fields.deadline.to_string(),
            version: fields.version.to_string(),
            nonce: fields.nonce.to_string(),
            sig: sig.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Submit/trace response payloads (§49.4.7)
// ---------------------------------------------------------------------------

/// Transfer lifecycle state (§49.4.7). Unknown values are rejected (R12).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum TransferState {
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

/// On-chain transaction state (trace, §49.4.7). Unknown values rejected (R12).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum TxnState {
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

/// Provider's view of a submitted transfer (submit + trace, §49.4.7). `version`
/// must be 1 (R5).
#[derive(Clone, Debug, Deserialize)]
pub struct TransferResponse {
    pub id: String,
    #[serde(rename = "txnHash", default)]
    pub txn_hash: Option<String>,
    #[serde(rename = "accountAddress")]
    pub account_address: String,
    #[serde(rename = "gasFreeAddress")]
    pub gas_free_address: String,
    #[serde(rename = "providerAddress")]
    pub provider_address: String,
    #[serde(rename = "targetAddress")]
    pub target_address: String,
    #[serde(rename = "tokenAddress")]
    pub token_address: String,
    #[serde(deserialize_with = "de_u256")]
    pub amount: U256,
    #[serde(deserialize_with = "de_u64")]
    pub nonce: u64,
    #[serde(deserialize_with = "de_u64")]
    pub version: u64,
    pub state: TransferState,
    #[serde(rename = "txnState", default)]
    pub txn_state: Option<TxnState>,
}

// ---------------------------------------------------------------------------
// REST client
// ---------------------------------------------------------------------------

/// A GasFree provider REST client bound to a host base URL and a network.
///
/// Credentials are held for request signing and never logged (R9): `Debug` is
/// intentionally not derived.
pub struct GasFreeRestClient {
    host_base_url: String,
    network: Network,
    api_key: String,
    api_secret: String,
}

impl GasFreeRestClient {
    pub fn new(host_base_url: String, network: Network, api_key: String, api_secret: String) -> Self {
        GasFreeRestClient {
            host_base_url,
            network,
            api_key,
            api_secret,
        }
    }

    fn url_for(&self, path: &str) -> String { format!("{}{}", self.host_base_url.trim_end_matches('/'), path) }

    #[cfg(not(target_arch = "wasm32"))]
    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    #[cfg(target_arch = "wasm32")]
    fn now_secs() -> u64 { (common::now_ms() / 1000) as u64 }

    /// Authenticated GET, returning the typed `data` payload (§49.4.3).
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, GasFreeProviderError> {
        let headers = build_auth_headers("GET", path, &self.api_key, &self.api_secret, Self::now_secs());
        let header_refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let url = self.url_for(path);
        let (status, _hdrs, body) = mm2_net::transport::slurp_url_with_headers(&url, header_refs)
            .await
            .map_err(map_slurp_err)?;
        interpret_response::<T>(status, &body)
    }

    /// Fetch account info for `account_address` (§49.4.4).
    pub async fn account_info(&self, account_address: &str) -> Result<AccountInfo, GasFreeProviderError> {
        self.get(&path_account(&self.network, account_address)).await
    }

    /// Fetch the supported-token set (§49.4.5).
    pub async fn supported_tokens(&self) -> Result<Vec<SupportedToken>, GasFreeProviderError> {
        self.get(&path_supported_tokens(&self.network)).await
    }

    /// Trace a submitted transfer (§49.4.7).
    pub async fn trace(&self, trace_id: &str) -> Result<TransferResponse, GasFreeProviderError> {
        self.get(&path_trace(&self.network, trace_id)).await
    }

    /// Submit a signed authorization (§49.4.6). **Deferred** (D-submit): the
    /// authenticated-POST transport is follow-on work; today this is
    /// not-implemented so the withdraw stays sign-only (§49.8).
    pub async fn submit(&self, _req: &SubmitRequest) -> Result<TransferResponse, GasFreeProviderError> {
        Err(GasFreeProviderError::NotImplemented(
            "GasFree submit is deferred (sign-only today)".to_owned(),
        ))
    }
}

fn map_slurp_err(e: mm2_err_handle::prelude::MmError<mm2_net::transport::SlurpError>) -> GasFreeProviderError {
    use mm2_net::transport::SlurpError;
    match e.into_inner() {
        SlurpError::Timeout { error, .. } => {
            let _ = error;
            GasFreeProviderError::Timeout
        },
        SlurpError::Transport { error, .. } => GasFreeProviderError::Transport(error),
        SlurpError::InvalidRequest(error) => GasFreeProviderError::InvalidRequest(error),
        SlurpError::ErrorDeserializing { error, .. } => GasFreeProviderError::InvalidResponse(error),
        SlurpError::Internal(error) => GasFreeProviderError::Internal(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eth_from_u64(n: u64) -> ethereum_types::Address {
        let mut b = [0u8; 20];
        b[12..].copy_from_slice(&n.to_be_bytes());
        ethereum_types::Address::from(b)
    }

    #[test]
    fn hmac_sha256_rfc4231_test_case_2() {
        // RFC 4231 TC2: key="Jefe", data="what do ya want for nothing?".
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn auth_header_format() {
        let headers = build_auth_headers("GET", "/tron/api/v1/config/token/all", "mykey", "mysecret", 1700000000);
        assert_eq!(headers[0].0, "Timestamp");
        assert_eq!(headers[0].1, "1700000000");
        assert_eq!(headers[1].0, "Authorization");
        assert!(headers[1].1.starts_with("ApiKey mykey:"));
        // Signature is base64 of a 32-byte HMAC → 44 chars (with padding).
        let sig = headers[1].1.strip_prefix("ApiKey mykey:").unwrap();
        assert_eq!(sig.len(), 44);
    }

    #[test]
    fn auth_signature_matches_manual_construction() {
        let headers = build_auth_headers("GET", "/p", "k", "s", 42);
        let want = base64::encode(&hmac_sha256(b"s", b"GET/p42"));
        assert_eq!(headers[1].1, format!("ApiKey k:{want}"));
    }

    #[test]
    fn path_resolution_per_network() {
        assert_eq!(
            path_supported_tokens(&Network::Mainnet),
            "/tron/api/v1/config/token/all"
        );
        assert_eq!(path_account(&Network::Nile, "TXyz"), "/nile/api/v1/address/TXyz");
        assert_eq!(path_submit(&Network::Shasta), "/shasta/api/v1/gasfree/submit");
        assert_eq!(path_trace(&Network::Mainnet, "abc"), "/tron/api/v1/gasfree/abc");
    }

    #[test]
    fn envelope_success_returns_data() {
        let body = br#"{"code":200,"message":"ok","data":{"supported":true,"tokenAddress":"T","activateFee":"1","transferFee":"2","decimal":6,"symbol":"USDT"}}"#;
        let token: SupportedToken = interpret_response(StatusCode::OK, body).unwrap();
        assert_eq!(token.decimal, 6);
        assert_eq!(token.activate_fee, U256::from(1u64));
    }

    #[test]
    fn http_200_with_non_200_envelope_is_rejected() {
        let body = br#"{"code":400,"message":"bad token","data":null}"#;
        let err = interpret_response::<SupportedToken>(StatusCode::OK, body).unwrap_err();
        assert!(matches!(err, GasFreeProviderError::ProviderBadRequest(_)));
    }

    #[test]
    fn http_200_success_without_data_is_rejected() {
        let body = br#"{"code":200,"message":"ok"}"#;
        let err = interpret_response::<SupportedToken>(StatusCode::OK, body).unwrap_err();
        assert!(matches!(err, GasFreeProviderError::InvalidResponse(_)));
    }

    #[test]
    fn http_status_classes_map_to_categories() {
        let b = br#"{"code":401,"message":"nope"}"#;
        assert!(matches!(
            interpret_response::<SupportedToken>(StatusCode::UNAUTHORIZED, b).unwrap_err(),
            GasFreeProviderError::Unauthorized
        ));
        assert!(matches!(
            interpret_response::<SupportedToken>(StatusCode::FORBIDDEN, b).unwrap_err(),
            GasFreeProviderError::Forbidden
        ));
        assert!(matches!(
            interpret_response::<SupportedToken>(StatusCode::TOO_MANY_REQUESTS, b).unwrap_err(),
            GasFreeProviderError::RateLimited
        ));
        assert!(matches!(
            interpret_response::<SupportedToken>(StatusCode::BAD_GATEWAY, b).unwrap_err(),
            GasFreeProviderError::Upstream(_)
        ));
        assert!(matches!(
            interpret_response::<SupportedToken>(StatusCode::BAD_REQUEST, b).unwrap_err(),
            GasFreeProviderError::ProviderBadRequest(_)
        ));
    }

    #[test]
    fn account_info_parses_assets() {
        let body = br#"{"code":200,"message":"ok","data":{"accountAddress":"TUser","gasFreeAddress":"TCustody","active":true,"nonce":"5","allowSubmit":true,"assets":[{"tokenAddress":"TToken","tokenSymbol":"USDT","activateFee":"1000000","transferFee":"2000000","decimal":6,"frozen":"0"}]}}"#;
        let info: AccountInfo = interpret_response(StatusCode::OK, body).unwrap();
        assert_eq!(info.nonce, 5);
        assert!(info.active);
        assert_eq!(info.assets.len(), 1);
        assert_eq!(info.assets[0].transfer_fee, U256::from(2_000_000u64));
    }

    #[test]
    fn transfer_state_unknown_rejected() {
        let json = r#""ZOMBIE""#;
        assert!(serde_json::from_str::<TransferState>(json).is_err());
        assert_eq!(
            serde_json::from_str::<TransferState>(r#""SUCCEED""#).unwrap(),
            TransferState::Succeed
        );
    }

    #[test]
    fn txn_state_unknown_rejected() {
        assert!(serde_json::from_str::<TxnState>(r#""WAT""#).is_err());
        assert_eq!(
            serde_json::from_str::<TxnState>(r#""ON_CHAIN""#).unwrap(),
            TxnState::OnChain
        );
    }

    fn tron(n: u64) -> TronAddress { TronAddress::from_evm_address(eth_from_u64(n)) }

    fn submit_fields() -> SubmitFields {
        SubmitFields {
            request_id: Some(uuid::Uuid::new_v4()),
            token: tron(1),
            service_provider: tron(2),
            user: tron(3),
            receiver: tron(4),
            value: U256::from(1000u64),
            max_fee: U256::from(20u64),
            deadline: 1_900_000_000,
            version: 1,
            nonce: 9,
            sig_hex: "ab".repeat(65),
        }
    }

    #[test]
    fn submit_serializes_integers_as_strings() {
        let req = SubmitRequest::new(submit_fields()).unwrap();
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["value"], "1000");
        assert_eq!(json["maxFee"], "20");
        assert_eq!(json["nonce"], "9");
        assert_eq!(json["version"], "1");
        // sig is 130 chars, no 0x.
        assert_eq!(json["sig"].as_str().unwrap().len(), 130);
        assert!(!json["sig"].as_str().unwrap().starts_with("0x"));
    }

    #[test]
    fn submit_rejects_non_v4_request_id() {
        let mut f = submit_fields();
        // A v1 UUID (timestamp-based) — version nibble != 4.
        f.request_id = Some(uuid::Uuid::parse_str("a8098c1a-f86e-11da-bd1a-00112444be1e").unwrap());
        assert!(SubmitRequest::new(f).is_err());
    }

    #[test]
    fn submit_rejects_bad_version() {
        let mut f = submit_fields();
        f.version = 2;
        assert!(SubmitRequest::new(f).is_err());
    }

    #[test]
    fn submit_rejects_malformed_signature() {
        let mut f = submit_fields();
        f.sig_hex = "deadbeef".to_owned(); // too short
        assert!(SubmitRequest::new(f).is_err());
        let mut f2 = submit_fields();
        f2.sig_hex = "zz".repeat(65); // non-hex
        assert!(SubmitRequest::new(f2).is_err());
    }

    #[test]
    fn submit_strips_0x_prefix_on_signature() {
        let mut f = submit_fields();
        f.sig_hex = format!("0x{}", "cd".repeat(65));
        let req = SubmitRequest::new(f).unwrap();
        assert_eq!(req.sig.len(), 130);
        assert!(!req.sig.starts_with("0x"));
    }
}
