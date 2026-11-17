//! WalletConnect v2 signing integration for Cosmos (`cosmos`) coins.
//!
//! Implements the chapter 22 §22.3 integration trait
//! ([`WcCoinSigner`](kdf_walletconnect::integration::WcCoinSigner)) for
//! [`TendermintCoin`]. All Cosmos-specific knowledge — the CAIP-2 chain
//! reference (§22.8.1.4 — `cosmos:<chain-registry name>`), the
//! `cosmos_signDirect` / `cosmos_signAmino` parameter shapes (§22.8.1.2), the
//! amino-vs-direct selection rule (§22.8.1.7) and the Keplr base64-vs-hex
//! binary-field encoding rule (§22.8.1.2) — lives here in the coin module, never
//! in the WalletConnect subsystem (binding R1/R2).
//!
//! Every request is carried inside the WC2 `wc_sessionRequest` envelope: this
//! module builds the inner `{ "chainId", "request": { "method", "params" } }`
//! payload (§22.8.1) and hands it to
//! [`WalletConnectCtx::send_session_request`], which wraps it as the outer
//! `wc_sessionRequest` `params`.
//!
//! Unlike the EVM family there is no wallet-broadcast Cosmos method: the wallet
//! only *signs*, returning the signature plus the (possibly normalised) `signed`
//! echo; the integration then assembles the broadcast-ready `TxRaw` and
//! broadcasts it through the coin's own node-RPC path.
//!
//! The activation of a coin with a WalletConnect session (CRD ch.36 R36.14.13 -- R36.14.15, D36.15, D36.16) lives
//! here too: [`read_wallet_connect_activation`] reads the account and key from the session, and
//! [`TendermintCoin::wc_sign_message`] signs one message through the wallet. Both reach the subsystem through
//! the narrow [`CosmosWallet`] trait, which [`WalletConnectCtx`] implements and a test can script.

use super::rpc::*;
use super::tendermint_helpers::{amino_bank_send_json, amino_sign_doc, TendermintCommons};
use super::tendermint_types::{account_id_from_public_key, parse_expected_sequence_number, FeeIntent,
                              TendermintCoinRpcError, ABCI_REQUEST_HEIGHT, ABCI_REQUEST_PROVE, ABCI_SIMULATE_TX_PATH,
                              ACCOUNT_SEQUENCE_ERR, FEE_GAS_MARGIN, GAS_LIMIT_DEFAULT};
use super::{TendermintCoin, TendermintPublicKey};
use crate::{DelegationError, WithdrawError, WithdrawFee};
use async_trait::async_trait;
use cosmrs::proto::cosmos::tx::v1beta1::{AuthInfo as AuthInfoProto, SimulateRequest, SimulateResponse, TxRaw};
use cosmrs::proto::prost::Message;
use cosmrs::tx::{self, Fee, ModeInfo, SignMode, SignerInfo};
use cosmrs::{AccountId, Any, Coin};
use derive_more::Display;
use kdf_walletconnect::chain::WcRequestMethods;
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::integration::WcCoinSigner;
use kdf_walletconnect::session::KeyInfo;
use kdf_walletconnect::{Topic, WalletConnectCtx};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The dictated WC2 app-metadata `name` that selects base64 binary-field
/// encoding; every other wallet uses hex (chapter 22 §22.8.1.2).
const KEPLR_WALLET_NAME: &str = "Keplr";

/// Formats a Cosmos chain-registry name as its CAIP-2 identifier
/// `cosmos:<chain-name>` (chapter 22 §22.8.1.4 — the reference is the chain
/// registry name, e.g. `cosmoshub-4`, **not** a numeric id).
fn cosmos_caip2_chain_id(chain_name: &str) -> String { format!("cosmos:{chain_name}") }

/// The encoding applied to Cosmos byte-valued fields (`authInfoBytes`,
/// `bodyBytes`, public-key and signature bytes), selected solely by the paired
/// wallet's WC2 app-metadata `name` (chapter 22 §22.8.1.2): `Keplr` ⇒ base64,
/// every other wallet ⇒ hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CosmosByteEncoding {
    Base64,
    Hex,
}

impl CosmosByteEncoding {
    /// Selects the encoding from the wallet's metadata `name`.
    fn for_wallet_name(name: &str) -> Self {
        if name == KEPLR_WALLET_NAME {
            CosmosByteEncoding::Base64
        } else {
            CosmosByteEncoding::Hex
        }
    }

    /// Encodes raw bytes for an outbound binary field.
    fn encode(self, bytes: &[u8]) -> String {
        match self {
            CosmosByteEncoding::Base64 => base64::encode(bytes),
            CosmosByteEncoding::Hex => hex::encode(bytes),
        }
    }

    /// Decodes an inbound binary field, mapping a malformed value to
    /// [`WalletConnectError::InvalidResponse`].
    fn decode(self, text: &str) -> Result<Vec<u8>, WalletConnectError> {
        match self {
            CosmosByteEncoding::Base64 => {
                base64::decode(text).map_err(|e| WalletConnectError::InvalidResponse(format!("invalid base64: {e}")))
            },
            CosmosByteEncoding::Hex => {
                hex::decode(text).map_err(|e| WalletConnectError::InvalidResponse(format!("invalid hex: {e}")))
            },
        }
    }
}

/// The associated [`UnsignedTx`](WcCoinSigner::UnsignedTx) parameter type for
/// Cosmos coins: a chain-specific description carrying both representations the
/// two sign modes need.
///
/// The direct path uses the protobuf `bodyBytes` / `authInfoBytes` (built with
/// SignMode `SIGN_MODE_DIRECT`); the amino path uses `fee` / `amino_msgs` /
/// `memo` to shape the `StdSignDoc` and reuses `body_bytes` for broadcast. The
/// internal field naming is informative (R36); only the emitted wire shapes are
/// binding.
#[derive(Clone, Debug)]
pub struct WcCosmosTxParams {
    /// Bech32 signer address (`signerAddress`).
    pub signer_address: String,
    /// Inner SignDoc chain id, the chain-registry name (e.g. `cosmoshub-4`).
    pub sign_doc_chain_id: String,
    /// On-chain account number.
    pub account_number: u64,
    /// Account sequence.
    pub sequence: u64,
    /// Serialized protobuf `TxBody` (direct path + amino broadcast body).
    pub body_bytes: Vec<u8>,
    /// Serialized protobuf `AuthInfo` (direct path, SignMode `SIGN_MODE_DIRECT`).
    pub auth_info_bytes: Vec<u8>,
    /// The transaction fee (amino fee JSON + amino-mode `AuthInfo` rebuild).
    pub fee: Fee,
    /// Amino-encoded message array for the `cosmos_signAmino` `StdSignDoc`.
    pub amino_msgs: Vec<Value>,
    /// Transaction memo (may be empty).
    pub memo: String,
}

/// Selects the request method from the dictated `isNanoLedger` signal (chapter
/// 22 §22.8.1.7): a Ledger account must sign with Amino-JSON, otherwise direct.
fn select_cosmos_method(is_nano_ledger: bool) -> WcRequestMethods {
    if is_nano_ledger {
        WcRequestMethods::CosmosSignAmino
    } else {
        WcRequestMethods::CosmosSignDirect
    }
}

/// Builds the `cosmos_signDirect` `params` object (§22.8.1.2): camelCase
/// `chainId` / `accountNumber`, with `authInfoBytes` / `bodyBytes` encoded per
/// the wallet's binary-field rule.
fn cosmos_sign_direct_params(tx: &WcCosmosTxParams, encoding: CosmosByteEncoding) -> Value {
    json!({
        "signerAddress": tx.signer_address,
        "signDoc": {
            "chainId": tx.sign_doc_chain_id,
            "accountNumber": tx.account_number.to_string(),
            "authInfoBytes": encoding.encode(&tx.auth_info_bytes),
            "bodyBytes": encoding.encode(&tx.body_bytes),
        }
    })
}

/// Builds the `cosmos_signAmino` `params` object (§22.8.1.2): the Amino `StdSignDoc` with snake_case `chain_id` /
/// `account_number` / `sequence`, the `{ amount, gas }` fee, the message array and the memo. The document is the one
/// [`amino_sign_doc`] builds for the unsigned result of a watch-only coin too (CRD ch.36 R36.14.31): every number a
/// decimal string, the fee coin also at zero, the memo also when empty, and no `timeout_height`.
fn cosmos_sign_amino_params(tx: &WcCosmosTxParams) -> Value {
    json!({
        "signerAddress": tx.signer_address,
        "signDoc": amino_sign_doc(
            &tx.sign_doc_chain_id,
            tx.account_number,
            tx.sequence,
            &tx.fee,
            &tx.memo,
            tx.amino_msgs.clone(),
        ),
    })
}

/// Builds the inner `wc_sessionRequest` payload (§22.8.1): the CAIP-2 `chainId`
/// plus a `request` carrying the method string and its params.
fn session_request_payload(chain_id: &str, method: WcRequestMethods, params: Value) -> Value {
    json!({
        "chainId": chain_id,
        "request": {
            "method": method.as_ref(),
            "params": params,
        }
    })
}

/// Parses the `signature.signature` field out of a Cosmos sign result, decoding
/// per the wallet's binary-field rule.
fn parse_cosmos_signature(result: &Value, encoding: CosmosByteEncoding) -> Result<Vec<u8>, WalletConnectError> {
    let text = result
        .get("signature")
        .and_then(|signature| signature.get("signature"))
        .and_then(Value::as_str)
        .ok_or_else(|| WalletConnectError::InvalidResponse("missing `signature.signature` field".to_string()))?;
    encoding.decode(text)
}

/// Parses the `signed` echo of a `cosmos_signDirect` result into the
/// `(bodyBytes, authInfoBytes)` the broadcast tx must be assembled from
/// (§22.8.1.2 — the echo may be wallet-normalised and MUST be used).
fn parse_signed_direct(result: &Value, encoding: CosmosByteEncoding) -> Result<(Vec<u8>, Vec<u8>), WalletConnectError> {
    let signed = result
        .get("signed")
        .ok_or_else(|| WalletConnectError::InvalidResponse("missing `signed` echo".to_string()))?;
    let body = signed
        .get("bodyBytes")
        .and_then(Value::as_str)
        .ok_or_else(|| WalletConnectError::InvalidResponse("missing `signed.bodyBytes`".to_string()))?;
    let auth_info = signed
        .get("authInfoBytes")
        .and_then(Value::as_str)
        .ok_or_else(|| WalletConnectError::InvalidResponse("missing `signed.authInfoBytes`".to_string()))?;
    Ok((encoding.decode(body)?, encoding.decode(auth_info)?))
}

/// Assembles a broadcast-ready `TxRaw` from its body, auth-info, and the single
/// wallet signature.
fn assemble_tx_raw(body_bytes: Vec<u8>, auth_info_bytes: Vec<u8>, signature: Vec<u8>) -> Vec<u8> {
    TxRaw {
        body_bytes,
        auth_info_bytes,
        signatures: vec![signature],
    }
    .encode_to_vec()
}

// ————————————————————————————————————————————————————————————————
// The wallet seen by the Cosmos coin
// ————————————————————————————————————————————————————————————————

/// A namespace of a session, reduced to what the Cosmos coin reads: the CAIP-2 chains it lists and its CAIP-10
/// accounts.
#[derive(Clone)]
pub(super) struct WcNamespace {
    pub(super) chains: Vec<String>,
    pub(super) accounts: Vec<String>,
}

/// A live session, reduced to what the Cosmos coin reads (CRD ch.22 §22.8.1.5, §22.8.1.6): the wallet's metadata
/// `name`, the agreed namespaces and the per-account key records of the session properties.
#[derive(Clone)]
pub(super) struct WcSessionView {
    pub(super) wallet_name: String,
    pub(super) namespaces: BTreeMap<String, WcNamespace>,
    pub(super) keys: Vec<KeyInfo>,
}

/// What the Cosmos coin needs of the WalletConnect subsystem: the live session of a topic and one signing or
/// enumeration request. [`WalletConnectCtx`] implements it; a test scripts it.
#[async_trait]
pub(super) trait CosmosWallet: Send + Sync {
    /// The live session registered for `topic`, `None` when there is none.
    fn session_view(&self, topic: &Topic) -> Option<WcSessionView>;

    /// Sends one `wc_sessionRequest` carrying `payload` on the session `topic` and answers the wallet's answer; a
    /// relay failure, a vanished session or an elapsed time-to-live (five minutes) are errors.
    async fn send_request(&self, topic: &Topic, payload: Value) -> Result<Value, WalletConnectError>;
}

#[async_trait]
impl CosmosWallet for WalletConnectCtx {
    fn session_view(&self, topic: &Topic) -> Option<WcSessionView> {
        self.sessions().with_session(topic, |session| WcSessionView {
            wallet_name: session.metadata.name.clone(),
            namespaces: session
                .namespaces
                .iter()
                .map(|(name, namespace)| {
                    let namespace = WcNamespace {
                        chains: namespace.chains.iter().flatten().cloned().collect(),
                        accounts: namespace.accounts.iter().flatten().cloned().collect(),
                    };
                    (name.clone(), namespace)
                })
                .collect(),
            keys: session
                .properties
                .as_ref()
                .and_then(|properties| properties.keys.clone())
                .unwrap_or_default(),
        })
    }

    async fn send_request(&self, topic: &Topic, payload: Value) -> Result<Value, WalletConnectError> {
        let (sym_key, _encoding) = self
            .sessions()
            .transport_for(topic)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        self.send_session_request(topic, &sym_key, payload).await
    }
}

/// A test scripts the wallet by caching this slot on the context, where [`wc_backend`] looks first.
#[cfg(test)]
pub(super) struct TestWalletSlot(pub(super) Arc<dyn CosmosWallet>);

/// The wallet subsystem of the node: the handle cached on the context, initialised on first use.
async fn wc_backend(ctx: &MmArc) -> Result<Arc<dyn CosmosWallet>, WalletConnectError> {
    #[cfg(test)]
    {
        let cached = ctx.wallet_connect.lock().unwrap().clone();
        if let Some(slot) = cached.and_then(|any| any.downcast::<TestWalletSlot>().ok()) {
            return Ok(slot.0.clone());
        }
    }
    let handle = WalletConnectCtx::get_or_init(ctx).await?;
    Ok(handle)
}

/// Why a signature through the wallet could not be had.
#[derive(Debug, Display)]
pub(super) enum WcSignError {
    /// CRD ch.36 D36.15 (b): the external wallet did not sign. It answers a typed signing error, never an internal
    /// one: a refusal, no answer within the request's time-to-live, a session that is gone or no longer grants the
    /// chain, a relay that could not carry the request, an answer without a usable signature, and (for
    /// `cosmos_signDirect`) an echoed body that differs from the requested one.
    #[display(fmt = "{}", _0)]
    NotSigned(String),
    /// A failure of this node's own, before or after the wallet's part (the account query, the encoding of the
    /// document, a message the Amino form does not cover). It is not a signing error.
    #[display(fmt = "{}", _0)]
    Local(String),
}

impl From<WcSignError> for WithdrawError {
    fn from(e: WcSignError) -> Self {
        match e {
            WcSignError::NotSigned(reason) => WithdrawError::SigningError(reason),
            WcSignError::Local(reason) => WithdrawError::InternalError(reason),
        }
    }
}

impl From<WcSignError> for DelegationError {
    fn from(e: WcSignError) -> Self {
        match e {
            WcSignError::NotSigned(reason) => DelegationError::SigningError(reason),
            WcSignError::Local(reason) => DelegationError::InternalError(reason),
        }
    }
}

// ————————————————————————————————————————————————————————————————
// Activation: what is read from the session (CRD ch.36 R36.14.14, D36.16 (b))
// ————————————————————————————————————————————————————————————————

/// What the activation reads from the session.
pub(super) struct WcActivationKey {
    pub(super) account_id: AccountId,
    pub(super) public_key: TendermintPublicKey,
    /// The Ledger flag: the matching key record's `isNanoLedger`, `false` when the key came from the enumeration.
    pub(super) is_ledger_connection: bool,
}

/// Whether a key record's `chainId` names the coin's chain, as the bare chain id or as its CAIP-2 identifier.
fn key_record_is_of_chain(record_chain: &str, chain_id: &str) -> bool {
    record_chain == chain_id || record_chain == cosmos_caip2_chain_id(chain_id)
}

/// CRD ch.36 R36.14.14 item 2: whether the session grants the chain `cosmos:<chain_id>`. Any of three ways: the
/// `cosmos` namespace lists it among its chains; the `cosmos` namespace lists no chains and a key record of the
/// session has that chain id; or the session has a namespace keyed by the full chain identifier.
fn cosmos_chain_granted(view: &WcSessionView, chain_id: &str) -> bool {
    let caip2 = cosmos_caip2_chain_id(chain_id);
    if view.namespaces.contains_key(&caip2) {
        return true;
    }
    let Some(cosmos) = view.namespaces.get("cosmos") else {
        return false;
    };
    if cosmos.chains.contains(&caip2) {
        return true;
    }
    cosmos.chains.is_empty()
        && view
            .keys
            .iter()
            .any(|key| key_record_is_of_chain(&key.chain_id, chain_id))
}

/// CRD ch.36 R36.14.14 item 3: the addresses of the CAIP-10 accounts of the session for the chain, from the
/// `cosmos` namespace and from a namespace keyed by the full chain identifier.
fn session_accounts_of_chain(view: &WcSessionView, chain_id: &str) -> Vec<String> {
    let caip2 = cosmos_caip2_chain_id(chain_id);
    let prefix = format!("{caip2}:");
    ["cosmos", caip2.as_str()]
        .iter()
        .filter_map(|name| view.namespaces.get(*name))
        .flat_map(|namespace| namespace.accounts.iter())
        .filter_map(|account| account.strip_prefix(&prefix))
        .map(str::to_owned)
        .collect()
}

/// CRD ch.36 R36.14.14 item 5: key bytes given as text are read as hex when every character is a hex digit and the
/// length is even, and as base64 otherwise.
fn decode_key_text(text: &str) -> Result<Vec<u8>, String> {
    let is_hex = !text.is_empty() && text.len() % 2 == 0 && text.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex {
        hex::decode(text).map_err(|e| format!("the public key is not valid hex: {e}"))
    } else {
        base64::decode(text).map_err(|e| format!("the public key is neither hex nor base64: {e}"))
    }
}

/// The key bytes of an enumeration entry (CRD ch.36 R36.14.14 item 5): text, or an array of byte values.
fn key_bytes_of_json(value: &Value) -> Result<Vec<u8>, String> {
    match value {
        Value::String(text) => decode_key_text(text),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .ok_or_else(|| "the public key is not an array of byte values".to_owned())
            })
            .collect(),
        _ => Err("the account entry has no public key".to_owned()),
    }
}

/// Reads the account, its public key and the Ledger flag of a coin's chain from the session `session_topic`, in the
/// order of CRD ch.36 R36.14.14; the first failure ends it. The wallet is contacted only when the session carries no
/// key record for the chain, with exactly one parameterless `cosmos_getAccounts` (D36.16 (b); ch.22 §22.8.1.2).
///
/// # Errors
///
/// The step that failed, in words: the activation answers it as `PlatformCoinCreationError` and registers no coin.
pub(super) async fn read_wallet_connect_activation(
    ctx: &MmArc,
    session_topic: &str,
    chain_id: &str,
    account_prefix: &str,
) -> Result<WcActivationKey, String> {
    let wallet = wc_backend(ctx)
        .await
        .map_err(|e| format!("the WalletConnect subsystem is not available: {e}"))?;
    read_session_key(&*wallet, session_topic, chain_id, account_prefix).await
}

async fn read_session_key(
    wallet: &dyn CosmosWallet,
    session_topic: &str,
    chain_id: &str,
    account_prefix: &str,
) -> Result<WcActivationKey, String> {
    // 1. Session.
    let topic = Topic::from(session_topic.to_owned());
    let view = wallet
        .session_view(&topic)
        .ok_or_else(|| "no live session has that topic".to_owned())?;
    // 2. Chain.
    if !cosmos_chain_granted(&view, chain_id) {
        return Err(format!(
            "the session does not grant the chain {}",
            cosmos_caip2_chain_id(chain_id)
        ));
    }
    // 3. Account.
    let accounts = session_accounts_of_chain(&view, chain_id);
    if accounts.is_empty() {
        return Err("the session holds no account for the chain".to_owned());
    }
    // 4. Public key: the key record of the coin's chain (not simply the first record), else one enumeration.
    let (algo, key_bytes, is_ledger_connection) = match view
        .keys
        .iter()
        .find(|key| key_record_is_of_chain(&key.chain_id, chain_id))
    {
        Some(record) => (
            record.algo.clone(),
            decode_key_text(&record.pub_key)?,
            record.is_nano_ledger,
        ),
        None => {
            let payload = session_request_payload(
                &cosmos_caip2_chain_id(chain_id),
                WcRequestMethods::CosmosGetAccounts,
                json!({}),
            );
            let answer = wallet
                .send_request(&topic, payload)
                .await
                .map_err(|e| format!("the wallet's account enumeration failed: {e}"))?;
            if let Some(error) = answer.get("error") {
                return Err(format!("the wallet refused the account enumeration: {error}"));
            }
            let entries = answer
                .as_array()
                .ok_or_else(|| "the wallet's account enumeration is not a list".to_owned())?;
            // The entry of the session's account for the chain; else the first one, whose key then has to give
            // that account (item 6).
            let entry = entries
                .iter()
                .find(|entry| {
                    entry["address"]
                        .as_str()
                        .is_some_and(|address| accounts.iter().any(|account| account == address))
                })
                .or_else(|| entries.first())
                .ok_or_else(|| "the wallet returned no account".to_owned())?;
            let algo = entry["algo"]
                .as_str()
                .ok_or_else(|| "the account entry has no `algo`".to_owned())?
                .to_owned();
            (algo, key_bytes_of_json(&entry["pubkey"])?, false)
        },
    };
    // 5. Key form.
    if algo != "secp256k1" && algo != "tendermint/PubKeySecp256k1" {
        return Err(format!("the key algorithm {algo} is not supported, only secp256k1"));
    }
    let public_key = TendermintPublicKey::from_raw_secp256k1(&key_bytes)
        .ok_or_else(|| "the public key is not a valid secp256k1 key".to_owned())?;
    // 6. Address: under the coin's own prefix, and it must be the session's account for the chain (D36.16 (b)).
    let account_id = account_id_from_public_key(&public_key, account_prefix)
        .map_err(|e| format!("the account cannot be derived from the public key: {e}"))?;
    if !accounts.iter().any(|account| *account == account_id.to_string()) {
        return Err("the address of the wallet's key differs from the session's account for the chain".to_owned());
    }
    // 7. The Ledger flag is the matching record's, set above.
    Ok(WcActivationKey {
        account_id,
        public_key,
        is_ledger_connection,
    })
}

/// The fee the signed transaction carries: the amount of the coin `denom` in the fee of the `AuthInfo` of the `TxRaw`
/// (zero when the fee holds no coin of that denomination) and the gas limit. The wallet may sign an `AuthInfo` that
/// differs from the one requested, so the fee a WalletConnect result reports is read from the transaction that is
/// answered (CRD ch.36 R36.14.15, as-built note of 2026-11-17). For the Amino path the `AuthInfo` is the one this node
/// assembled from the requested fee; both paths decode the same way.
///
/// # Errors
///
/// A description when the bytes are not a `TxRaw`, the `AuthInfo` is not decodable, it carries no fee, or an amount is
/// not a decimal number that fits 64 bits.
pub(super) fn signed_fee_of(tx_bytes: &[u8], denom: &str) -> Result<(u64, u64), String> {
    let raw = TxRaw::decode(tx_bytes).map_err(|e| format!("the signed transaction does not decode: {e}"))?;
    let auth_info = AuthInfoProto::decode(raw.auth_info_bytes.as_slice())
        .map_err(|e| format!("the signed auth info does not decode: {e}"))?;
    let fee = auth_info
        .fee
        .ok_or_else(|| "the signed auth info carries no fee".to_owned())?;
    let amount = match fee.amount.iter().find(|coin| coin.denom == denom) {
        Some(coin) => coin
            .amount
            .parse::<u64>()
            .map_err(|e| format!("the signed fee amount is not a number: {e}"))?,
        None => 0,
    };
    Ok((amount, fee.gas_limit))
}

/// The gas limit and the fee amount (in the smallest unit) of a request the wallet signs, from the simulated gas
/// (see `TendermintCoin::wc_request_fee`). With `explicit_gas_limit` the limit is kept and the amount is the
/// simulated gas times the margin times the price; without it the limit is the simulated gas times the margin and the
/// amount is that limit times the price. Both round up.
fn wc_fee_figures(gas_used: u64, gas_price: f64, explicit_gas_limit: Option<u64>) -> (u64, u64) {
    let margin_gas = gas_used as f64 * FEE_GAS_MARGIN;
    match explicit_gas_limit {
        Some(limit) => (limit, (margin_gas * gas_price).ceil() as u64),
        None => {
            let limit = margin_gas.ceil();
            (limit as u64, (limit * gas_price).ceil() as u64)
        },
    }
}

impl TendermintCoin {
    /// This coin's CAIP-2 chain id (`cosmos:<chain-registry name>`).
    fn wc_cosmos_chain_id(&self) -> String { cosmos_caip2_chain_id(self.protocol_info.chain_id.as_ref()) }

    /// Builds the [`WcCosmosTxParams`] for a Cosmos transaction by reusing the
    /// coin's existing SignDoc construction: it queries the on-chain account
    /// number / sequence, builds the protobuf `TxBody` and the SignMode-direct
    /// `AuthInfo` (carrying the activated public key the external wallet will
    /// sign for), and serialises both. `amino_msgs` is the Amino-JSON message
    /// array used only when the wallet selects the Ledger amino path.
    pub async fn build_wc_cosmos_tx_params(
        &self,
        messages: Vec<Any>,
        fee: Fee,
        memo: String,
        timeout_height: u64,
        amino_msgs: Vec<Value>,
    ) -> Result<WcCosmosTxParams, WalletConnectError> {
        let account_info = self
            .account_info(&self.account_id)
            .await
            .map_err(|e| WalletConnectError::Internal(format!("account info query failed: {e}")))?;
        let public_key = cosmrs::crypto::PublicKey::from(
            self.activation_policy
                .public_key()
                .map_err(|e| WalletConnectError::Internal(format!("public key unavailable: {e}")))?,
        );
        let tx_body = tx::Body::new(messages, memo.clone(), timeout_height as u32);
        let auth_info = SignerInfo::single_direct(Some(public_key), account_info.sequence).auth_info(fee.clone());
        let body_bytes = tx_body
            .into_bytes()
            .map_err(|e| WalletConnectError::Internal(format!("encode tx body: {e}")))?;
        let auth_info_bytes = auth_info
            .into_bytes()
            .map_err(|e| WalletConnectError::Internal(format!("encode auth info: {e}")))?;
        Ok(WcCosmosTxParams {
            signer_address: self.account_id.to_string(),
            sign_doc_chain_id: self.protocol_info.chain_id.as_ref().to_string(),
            account_number: account_info.account_number,
            sequence: account_info.sequence,
            body_bytes,
            auth_info_bytes,
            fee,
            amino_msgs,
            memo,
        })
    }

    /// The gas the node's simulation reports for one message, with the activated public key in the signer info and an
    /// empty signature (the simulation path does not verify signatures; the transaction still carries one signature
    /// slot). The wallet's key is not held here, so this is how a WalletConnect coin gets a gas figure at all (CRD
    /// ch.36 R36.14.15, as-built note of 2026-11-17).
    ///
    /// # Errors
    ///
    /// A [`TendermintCoinRpcError`] when the account or the node cannot be queried, the node refuses the simulation
    /// or its answer carries no gas information.
    async fn wc_simulate_gas_used(
        &self,
        msg: &Any,
        timeout_height: u64,
        memo: &str,
    ) -> MmResult<u64, TendermintCoinRpcError> {
        let public_key = cosmrs::crypto::PublicKey::from(
            self.activation_policy
                .public_key()
                .map_err(|e| TendermintCoinRpcError::InternalError(format!("public key unavailable: {e}")))?,
        );
        let mut account_info = self.account_info(&self.account_id).await?;

        let (response, raw_response) = loop {
            let body_bytes = tx::Body::new(vec![msg.clone()], memo, timeout_height as u32)
                .into_bytes()
                .map_to_mm(|e| TendermintCoinRpcError::InternalError(format!("encode tx body: {e}")))?;
            let fee = Fee::from_amount_and_gas(
                Coin {
                    denom: self.protocol_info.denom.clone(),
                    amount: 0_u64.into(),
                },
                GAS_LIMIT_DEFAULT,
            );
            let auth_info_bytes = SignerInfo::single_direct(Some(public_key.clone()), account_info.sequence)
                .auth_info(fee)
                .into_bytes()
                .map_to_mm(|e| TendermintCoinRpcError::InternalError(format!("encode auth info: {e}")))?;
            let tx_bytes = assemble_tx_raw(body_bytes, auth_info_bytes, Vec::new());

            let request = AbciRequest::new(
                Some(ABCI_SIMULATE_TX_PATH.to_string()),
                SimulateRequest {
                    tx_bytes,
                    ..Default::default()
                }
                .encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            );
            let raw_response = self.rpc_client().await?.perform(request).await?;
            let log = raw_response.response.log.to_string();

            if log.contains(ACCOUNT_SEQUENCE_ERR) {
                account_info.sequence = parse_expected_sequence_number(&log)?;
                continue;
            }
            match raw_response.response.code {
                cosmrs::tendermint::abci::Code::Ok => {},
                cosmrs::tendermint::abci::Code::Err(code) => {
                    return MmError::err(TendermintCoinRpcError::InvalidResponse(format!(
                        "The gas simulation failed. Error code: {} Message: {}",
                        code, raw_response.response.log
                    )));
                },
            }
            break (
                SimulateResponse::decode(raw_response.response.value.as_slice())?,
                raw_response,
            );
        };

        response.gas_info.as_ref().map(|gas| gas.gas_used).ok_or_else(|| {
            MmError::new(TendermintCoinRpcError::InvalidResponse(format!(
                "Could not read gas_info. Response: {raw_response:?}"
            )))
        })
    }

    /// The fee of a transaction the wallet signs for a request (withdraw, delegation, undelegation, reward claim),
    /// from the node's gas simulation instead of a fixed assumed gas (CRD ch.36 R36.14.15, as-built note of
    /// 2026-11-17; a Reloaded divergence, since the fixed limit ran out of gas for a real transaction and the fee
    /// was lost). The margin is the one the local-key fee computation applies, [`FEE_GAS_MARGIN`].
    ///
    /// - A request with an explicit gas limit keeps it, as it does for a local key; the fee amount is the simulated
    ///   gas times the margin times the gas price (the local-key formula).
    /// - Without one, the gas limit is the simulated gas times the margin (rounded up) and the fee amount is that
    ///   limit times the gas price (rounded up), so the amount always covers the limit at the selected price.
    ///
    /// When the simulation fails the call fails; no guessed limit is used.
    ///
    /// # Errors
    ///
    /// The errors of the simulation (see `wc_simulate_gas_used`).
    pub(super) async fn wc_request_fee(
        &self,
        msg: &Any,
        timeout_height: u64,
        memo: &str,
        intent: &FeeIntent,
    ) -> MmResult<Fee, TendermintCoinRpcError> {
        let gas_used = self.wc_simulate_gas_used(msg, timeout_height, memo).await?;
        let (gas_price, selected_gas_limit) = self.selected_gas_price_and_limit(intent);
        let explicit_limit = matches!(intent, FeeIntent::Request {
            fee: Some(WithdrawFee::CosmosGas { .. } | WithdrawFee::CosmosGasPriority { .. }),
            ..
        });
        let (gas_limit, amount) = wc_fee_figures(gas_used, gas_price, explicit_limit.then_some(selected_gas_limit));
        Ok(Fee::from_amount_and_gas(
            Coin {
                denom: self.platform_denom().clone(),
                amount: amount.into(),
            },
            gas_limit,
        ))
    }

    /// Re-builds the broadcast `AuthInfo` with SignMode `SIGN_MODE_LEGACY_AMINO_JSON`
    /// (the wallet signed the Amino doc) and assembles the broadcast `TxRaw`,
    /// reusing the protobuf `body_bytes` from the params.
    fn assemble_amino_tx(&self, tx: &WcCosmosTxParams, signature: Vec<u8>) -> Result<Vec<u8>, WalletConnectError> {
        let public_key = cosmrs::crypto::PublicKey::from(
            self.activation_policy
                .public_key()
                .map_err(|e| WalletConnectError::Internal(format!("public key unavailable: {e}")))?,
        );
        let signer_info = SignerInfo {
            public_key: Some(public_key.into()),
            mode_info: ModeInfo::single(SignMode::LegacyAminoJson),
            sequence: tx.sequence,
        };
        let auth_info_bytes = signer_info
            .auth_info(tx.fee.clone())
            .into_bytes()
            .map_err(|e| WalletConnectError::Internal(format!("encode amino auth info: {e}")))?;
        Ok(assemble_tx_raw(tx.body_bytes.clone(), auth_info_bytes, signature))
    }

    /// Signs one message through the paired wallet and answers the broadcast-ready transaction (CRD ch.36
    /// R36.14.15). The wallet method is chosen by the Ledger flag of the activation alone: `cosmos_signAmino` with
    /// the flag, `cosmos_signDirect` without. With the flag the transaction is the bank send's Amino form: any other
    /// message fails here, before the account is queried and before any request is sent (R36.14.18 item 3), and the
    /// transaction body carries no timeout height, as the document the wallet signs has none (R36.14.31 item 4).
    /// Nothing is broadcast.
    ///
    /// # Errors
    ///
    /// [`WcSignError::NotSigned`] when the wallet does not sign (see [`WcSignError`]); [`WcSignError::Local`] for
    /// every failure of this node's own.
    pub(super) async fn wc_sign_message(
        &self,
        msg: Any,
        fee: Fee,
        timeout_height: u64,
        memo: &str,
    ) -> Result<Vec<u8>, WcSignError> {
        let (_, is_ledger) = self
            .activation_policy
            .wallet_connect_session()
            .ok_or_else(|| WcSignError::Local("The coin is not signed by a WalletConnect wallet".to_owned()))?;
        let ctx =
            MmArc::from_weak(&self.ctx).ok_or_else(|| WcSignError::Local("The node is shutting down".to_owned()))?;
        let (amino_msgs, timeout_height) = if is_ledger {
            let amino_msg = amino_bank_send_json(&msg).map_err(|e| WcSignError::Local(e.to_string()))?;
            (vec![amino_msg], 0)
        } else {
            (Vec::new(), timeout_height)
        };
        let params = self
            .build_wc_cosmos_tx_params(vec![msg], fee, memo.to_owned(), timeout_height, amino_msgs)
            .await
            .map_err(|e| WcSignError::Local(e.to_string()))?;
        let wallet = wc_backend(&ctx)
            .await
            .map_err(|e| WcSignError::NotSigned(e.to_string()))?;
        self.wc_signed_tx_bytes(&*wallet, &params).await
    }

    /// Checks the session again, sends the one signing request and assembles the broadcast-ready `TxRaw` from the
    /// wallet's answer (CRD ch.36 R36.14.15; ch.22 §22.8.1.2 for both payloads and the assembly).
    ///
    /// Before the request the session of the activation must still exist and grant the coin's chain. For
    /// `cosmos_signDirect` the broadcast body must be the body that was asked to be signed: the wallet's `signed`
    /// echo is used for the transaction, but a body that differs from the requested one fails the request (the
    /// auth-info bytes may legitimately differ).
    async fn wc_signed_tx_bytes(
        &self,
        wallet: &dyn CosmosWallet,
        tx: &WcCosmosTxParams,
    ) -> Result<Vec<u8>, WcSignError> {
        let not_signed = WcSignError::NotSigned;
        let (topic, is_ledger) = self
            .activation_policy
            .wallet_connect_session()
            .ok_or_else(|| WcSignError::Local("The coin is not signed by a WalletConnect wallet".to_owned()))?;
        let view = wallet
            .session_view(topic)
            .ok_or_else(|| not_signed("the WalletConnect session no longer exists".to_owned()))?;
        if !cosmos_chain_granted(&view, self.protocol_info.chain_id.as_ref()) {
            return Err(not_signed(
                "the WalletConnect session no longer grants the chain".to_owned(),
            ));
        }
        let encoding = CosmosByteEncoding::for_wallet_name(&view.wallet_name);

        let params = if is_ledger {
            cosmos_sign_amino_params(tx)
        } else {
            cosmos_sign_direct_params(tx, encoding)
        };
        let payload = session_request_payload(&self.wc_cosmos_chain_id(), select_cosmos_method(is_ledger), params);
        let result = wallet
            .send_request(topic, payload)
            .await
            .map_err(|e| not_signed(e.to_string()))?;
        // A wallet's refusal arrives as an answer that carries an `error` member and no result.
        if let Some(error) = result.get("error") {
            return Err(not_signed(format!("the wallet refused to sign: {error}")));
        }

        let signature = parse_cosmos_signature(&result, encoding).map_err(|e| not_signed(e.to_string()))?;
        if is_ledger {
            return self
                .assemble_amino_tx(tx, signature)
                .map_err(|e| WcSignError::Local(e.to_string()));
        }
        let (body_bytes, auth_info_bytes) =
            parse_signed_direct(&result, encoding).map_err(|e| not_signed(e.to_string()))?;
        if body_bytes != tx.body_bytes {
            return Err(not_signed(
                "the transaction body the wallet signed differs from the one that was requested".to_owned(),
            ));
        }
        Ok(assemble_tx_raw(body_bytes, auth_info_bytes, signature))
    }

    /// Broadcasts an assembled Cosmos `TxRaw` through the coin's node RPC,
    /// returning the broadcast transaction hash.
    async fn wc_broadcast_cosmos_tx(&self, tx_bytes: Vec<u8>) -> Result<String, WalletConnectError> {
        let client = self
            .rpc_client()
            .await
            .map_err(|e| WalletConnectError::Relay(format!("no usable RPC node: {e}")))?;
        let broadcast_res = client
            .broadcast_tx_commit(tx_bytes)
            .await
            .map_err(|e| WalletConnectError::Relay(format!("broadcast failed: {e}")))?;
        if !broadcast_res.check_tx.code.is_ok() {
            return Err(WalletConnectError::InvalidResponse(format!(
                "tx check failed: {}",
                broadcast_res.check_tx.log
            )));
        }
        if !broadcast_res.tx_result.code.is_ok() {
            return Err(WalletConnectError::InvalidResponse(format!(
                "tx delivery failed: {}",
                broadcast_res.tx_result.log
            )));
        }
        Ok(broadcast_res.hash.to_string())
    }
}

#[async_trait]
impl WcCoinSigner for TendermintCoin {
    type UnsignedTx = WcCosmosTxParams;
    type SignedTx = Vec<u8>;
    type TxHash = String;

    async fn wc_chain_id(&self, _wc: &WalletConnectCtx) -> Result<String, WalletConnectError> {
        Ok(self.wc_cosmos_chain_id())
    }

    async fn wc_sign_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::SignedTx, WalletConnectError> {
        self.wc_signed_tx_bytes(wc, &tx)
            .await
            .map_err(|e| WalletConnectError::Internal(e.to_string()))
    }

    async fn wc_sign_and_send_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::TxHash, WalletConnectError> {
        let tx_bytes = self.wc_sign_transaction(wc, tx).await?;
        self.wc_broadcast_cosmos_tx(tx_bytes).await
    }

    async fn wc_session_topic(&self, _wc: &WalletConnectCtx) -> Result<Topic, WalletConnectError> {
        // The session is the one the coin was activated with, whatever other session may also grant the chain.
        match self.activation_policy.wallet_connect_session() {
            Some((topic, _)) => Ok(topic.clone()),
            None => Err(WalletConnectError::SessionNotFound(self.wc_cosmos_chain_id())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmrs::{Coin, Denom};
    use std::str::FromStr;

    fn sample_fee() -> Fee {
        Fee::from_amount_and_gas(
            Coin {
                denom: Denom::from_str("uatom").unwrap(),
                amount: 2000u128,
            },
            200_000u64,
        )
    }

    fn sample_params() -> WcCosmosTxParams {
        WcCosmosTxParams {
            signer_address: "cosmos1abcdef".to_string(),
            sign_doc_chain_id: "cosmoshub-4".to_string(),
            account_number: 42,
            sequence: 7,
            body_bytes: vec![0x0a, 0x0b, 0x0c],
            auth_info_bytes: vec![0x01, 0x02],
            fee: sample_fee(),
            amino_msgs: vec![json!({ "type": "cosmos-sdk/MsgSend", "value": {} })],
            memo: String::new(),
        }
    }

    // (a) CAIP-2 formatting: cosmos:<chain-name>, not a numeric id.
    #[test]
    fn caip2_chain_id_is_registry_name() {
        assert_eq!(cosmos_caip2_chain_id("cosmoshub-4"), "cosmos:cosmoshub-4");
        assert_eq!(cosmos_caip2_chain_id("osmosis-1"), "cosmos:osmosis-1");
        assert_eq!(cosmos_caip2_chain_id("iris-1"), "cosmos:iris-1");
    }

    // (b) amino-vs-direct method selection from isNanoLedger.
    #[test]
    fn amino_vs_direct_selection() {
        assert_eq!(select_cosmos_method(true), WcRequestMethods::CosmosSignAmino);
        assert_eq!(select_cosmos_method(false), WcRequestMethods::CosmosSignDirect);
        // And the emitted wire strings.
        assert_eq!(select_cosmos_method(true).as_ref(), "cosmos_signAmino");
        assert_eq!(select_cosmos_method(false).as_ref(), "cosmos_signDirect");
    }

    // (c) binary-field encoding rule: name == "Keplr" => base64, otherwise hex.
    #[test]
    fn binary_field_encoding_keyed_on_wallet_name() {
        assert_eq!(CosmosByteEncoding::for_wallet_name("Keplr"), CosmosByteEncoding::Base64);
        assert_eq!(CosmosByteEncoding::for_wallet_name("Leap"), CosmosByteEncoding::Hex);
        assert_eq!(
            CosmosByteEncoding::for_wallet_name("Cosmostation"),
            CosmosByteEncoding::Hex
        );
        assert_eq!(CosmosByteEncoding::for_wallet_name(""), CosmosByteEncoding::Hex);

        // The byte fields encode accordingly.
        let bytes = [0x0a, 0x0b, 0x0c];
        assert_eq!(CosmosByteEncoding::Base64.encode(&bytes), base64::encode(&bytes));
        assert_eq!(CosmosByteEncoding::Hex.encode(&bytes), "0a0b0c");
    }

    #[test]
    fn direct_params_encode_bytes_per_wallet_rule() {
        // Keplr => base64 authInfoBytes/bodyBytes.
        let keplr = cosmos_sign_direct_params(&sample_params(), CosmosByteEncoding::Base64);
        assert_eq!(keplr["signDoc"]["authInfoBytes"], base64::encode(&[0x01, 0x02]));
        assert_eq!(keplr["signDoc"]["bodyBytes"], base64::encode(&[0x0a, 0x0b, 0x0c]));
        // Other wallets => hex.
        let other = cosmos_sign_direct_params(&sample_params(), CosmosByteEncoding::Hex);
        assert_eq!(other["signDoc"]["authInfoBytes"], "0102");
        assert_eq!(other["signDoc"]["bodyBytes"], "0a0b0c");
    }

    // (d) cosmos_signDirect params shape: camelCase, signerAddress.
    #[test]
    fn sign_direct_params_shape() {
        let params = cosmos_sign_direct_params(&sample_params(), CosmosByteEncoding::Hex);
        assert_eq!(params["signerAddress"], "cosmos1abcdef");
        assert_eq!(params["signDoc"]["chainId"], "cosmoshub-4");
        assert_eq!(params["signDoc"]["accountNumber"], "42");
        assert_eq!(params["signDoc"]["authInfoBytes"], "0102");
        assert_eq!(params["signDoc"]["bodyBytes"], "0a0b0c");
        // No snake_case spellings leak into the direct doc.
        assert!(params["signDoc"].get("chain_id").is_none());
        assert!(params["signDoc"].get("account_number").is_none());
    }

    // (d) cosmos_signAmino params shape: snake_case + fee/msgs/memo.
    #[test]
    fn sign_amino_params_shape() {
        let params = cosmos_sign_amino_params(&sample_params());
        assert_eq!(params["signerAddress"], "cosmos1abcdef");
        assert_eq!(params["signDoc"]["chain_id"], "cosmoshub-4");
        assert_eq!(params["signDoc"]["account_number"], "42");
        assert_eq!(params["signDoc"]["sequence"], "7");
        assert_eq!(params["signDoc"]["fee"]["gas"], "200000");
        assert_eq!(params["signDoc"]["fee"]["amount"][0]["denom"], "uatom");
        assert_eq!(params["signDoc"]["fee"]["amount"][0]["amount"], "2000");
        assert!(params["signDoc"]["msgs"].is_array());
        assert_eq!(params["signDoc"]["memo"], "");
        // No camelCase spellings leak into the amino doc.
        assert!(params["signDoc"].get("chainId").is_none());
        assert!(params["signDoc"].get("accountNumber").is_none());
    }

    // (e) full wc_sessionRequest envelope for both cosmos methods.
    #[test]
    fn sign_direct_envelope_shape() {
        let params = cosmos_sign_direct_params(&sample_params(), CosmosByteEncoding::Hex);
        let payload = session_request_payload("cosmos:cosmoshub-4", WcRequestMethods::CosmosSignDirect, params);
        assert_eq!(payload["chainId"], "cosmos:cosmoshub-4");
        assert_eq!(payload["request"]["method"], "cosmos_signDirect");
        assert_eq!(payload["request"]["params"]["signerAddress"], "cosmos1abcdef");
        assert_eq!(payload["request"]["params"]["signDoc"]["chainId"], "cosmoshub-4");
    }

    #[test]
    fn sign_amino_envelope_shape() {
        let params = cosmos_sign_amino_params(&sample_params());
        let payload = session_request_payload("cosmos:cosmoshub-4", WcRequestMethods::CosmosSignAmino, params);
        assert_eq!(payload["chainId"], "cosmos:cosmoshub-4");
        assert_eq!(payload["request"]["method"], "cosmos_signAmino");
        assert_eq!(payload["request"]["params"]["signDoc"]["chain_id"], "cosmoshub-4");
    }

    // (f) result parsing: signature + signed echo, with malformed rejection.
    #[test]
    fn parse_signature_and_signed_echo_base64() {
        let signature_bytes = [0x5au8; 64];
        let result = json!({
            "signature": {
                "pub_key": { "type": "tendermint/PubKeySecp256k1", "value": base64::encode(&[0xaau8; 33]) },
                "signature": base64::encode(&signature_bytes),
            },
            "signed": {
                "chainId": "cosmoshub-4",
                "accountNumber": "42",
                "authInfoBytes": base64::encode(&[0x01, 0x02]),
                "bodyBytes": base64::encode(&[0x0a, 0x0b, 0x0c]),
            }
        });
        let signature = parse_cosmos_signature(&result, CosmosByteEncoding::Base64).unwrap();
        assert_eq!(signature, signature_bytes.to_vec());
        let (body, auth_info) = parse_signed_direct(&result, CosmosByteEncoding::Base64).unwrap();
        assert_eq!(body, vec![0x0a, 0x0b, 0x0c]);
        assert_eq!(auth_info, vec![0x01, 0x02]);
    }

    #[test]
    fn parse_signature_hex_for_non_keplr() {
        let result = json!({
            "signature": { "signature": hex::encode([0xbbu8; 64]) },
        });
        let signature = parse_cosmos_signature(&result, CosmosByteEncoding::Hex).unwrap();
        assert_eq!(signature, vec![0xbbu8; 64]);
    }

    #[test]
    fn parse_rejects_malformed_results() {
        // Missing signature field.
        let no_signature = json!({ "signed": { "bodyBytes": "00", "authInfoBytes": "00" } });
        assert!(matches!(
            parse_cosmos_signature(&no_signature, CosmosByteEncoding::Base64),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        // Missing signed echo.
        let no_signed = json!({ "signature": { "signature": base64::encode(&[0u8; 64]) } });
        assert!(matches!(
            parse_signed_direct(&no_signed, CosmosByteEncoding::Base64),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        // Present echo but missing a byte field.
        let partial_signed = json!({ "signed": { "bodyBytes": base64::encode(&[0u8; 2]) } });
        assert!(matches!(
            parse_signed_direct(&partial_signed, CosmosByteEncoding::Base64),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        // Invalid base64 in the signature.
        let bad_base64 = json!({ "signature": { "signature": "not base64 @@@" } });
        assert!(matches!(
            parse_cosmos_signature(&bad_base64, CosmosByteEncoding::Base64),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }
}

#[cfg(test)]
mod fee_tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn the_gas_limit_covers_the_simulated_gas_with_the_margin() {
        // The real defect: a send that used 125,865 gas ran out of gas under the fixed limit 125,000.
        let (limit, amount) = wc_fee_figures(125_865, 0.006, None);
        assert_eq!(limit, 188_798, "ceil(125,865 * 1.5)");
        assert!(limit > 125_865);
        assert_eq!(amount, 1_133, "ceil(188,798 * 0.006)");
        // The amount covers the limit at the price.
        assert!(amount as f64 >= limit as f64 * 0.006);
    }

    #[test]
    fn an_explicit_limit_is_kept_and_the_amount_is_the_local_key_formula() {
        let (limit, amount) = wc_fee_figures(100_000, 0.5, Some(200_000));
        assert_eq!((limit, amount), (200_000, 75_000), "100,000 * 1.5 * 0.5");
    }

    #[test]
    fn the_fee_of_the_signed_auth_info_is_read_for_the_direct_and_the_amino_assembly() {
        let denom = cosmrs::Denom::from_str("uatom").unwrap();
        let fee = Fee::from_amount_and_gas(
            Coin {
                denom,
                amount: 900_u64.into(),
            },
            150_000u64,
        );
        let auth_info_bytes = SignerInfo::single_direct(None, 3).auth_info(fee).into_bytes().unwrap();
        // The direct path answers the echoed auth info bytes, the Amino path an auth info this node assembled: both
        // reach the same decoder.
        let tx = assemble_tx_raw(vec![1, 2], auth_info_bytes, vec![7; 64]);
        assert_eq!(signed_fee_of(&tx, "uatom"), Ok((900, 150_000)));
        assert_eq!(
            signed_fee_of(&tx, "other"),
            Ok((0, 150_000)),
            "no coin of that denomination"
        );
        assert!(signed_fee_of(&[0xff, 0xff], "uatom").is_err());
    }
}
