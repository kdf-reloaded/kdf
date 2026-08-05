//! WalletConnect v2 signing integration for UTXO (`bip122`) coins.
//!
//! Implements the chapter 22 §22.3 integration trait
//! ([`WcCoinSigner`](kdf_walletconnect::integration::WcCoinSigner)) for
//! [`UtxoStandardCoin`], plus the bip122 `signMessage` / `getAccountAddresses` /
//! `sendTransfer` flows. All UTXO-specific knowledge — the CAIP-2 chain
//! reference (§22.8.1.4 — `bip122:<first 16 bytes of the genesis hash>`), the
//! `signPsbt` / `sendTransfer` / `getAccountAddresses` / `signMessage` parameter
//! shapes (§22.8.1.3) — lives here in the coin module, never in the
//! WalletConnect subsystem (binding R1/R2).
//!
//! Every request is carried inside the WC2 `wc_sessionRequest` envelope: this
//! module builds the inner `{ "chainId", "request": { "method", "params" } }`
//! payload (§22.8.1) and hands it to
//! [`WalletConnectCtx::send_session_request`], which wraps it as the outer
//! `wc_sessionRequest` `params`.
//!
//! Upstream divergence (§22.8.1.3 informative note): the bip122 message-signing
//! wire method is `signMessage`, **not** `personal_sign` (the latter is the
//! `eip155` method); this module emits `signMessage`
//! ([`WcRequestMethods::UtxoPersonalSign`]).

use super::rpc_clients::UtxoRpcClientEnum;
use super::utxo_common;
use super::utxo_standard::UtxoStandardCoin;
use async_trait::async_trait;
use chain::BlockHeader;
use futures::compat::Future01CompatExt;
use kdf_walletconnect::chain::WcRequestMethods;
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::integration::WcCoinSigner;
use kdf_walletconnect::{Topic, WalletConnectCtx};
use primitives::hash::H256;
use rpc::v1::types::H256 as H256Json;
use serde_json::{json, Map, Value};
use serialization::deserialize;

/// The CAIP-2 `bip122` reference is the leading 16 bytes (32 hex characters) of
/// the genesis block hash in conventional big-endian display order; the
/// truncation length is fixed by CAIP-2 (§22.8.1.4), not implementation
/// discretion.
const BIP122_REFERENCE_BYTES: usize = 16;

/// Formats a genesis block hash (given in conventional big-endian **display**
/// order) as its CAIP-2 identifier `bip122:<first 32 hex chars>` (§22.8.1.4).
fn bip122_caip2_chain_id(genesis_display_hash: &H256) -> String {
    format!(
        "bip122:{}",
        hex::encode(&genesis_display_hash[..BIP122_REFERENCE_BYTES])
    )
}

/// A single entry of a bip122 `signPsbt` `signInputs` array (§22.8.1.3):
/// the input's address, its index in the PSBT, and optional sighash types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcUtxoSignInput {
    /// Address that owns the input to sign.
    pub address: String,
    /// Index of the input within the PSBT.
    pub index: u32,
    /// Optional sighash type flags for this input.
    pub sighash_types: Option<Vec<u32>>,
}

impl WcUtxoSignInput {
    /// Builds the `{ "address", "index", "sighashTypes"? }` object (§22.8.1.3).
    fn to_request_object(&self) -> Value {
        let mut object = Map::new();
        object.insert("address".to_string(), Value::String(self.address.clone()));
        object.insert("index".to_string(), Value::from(self.index));
        if let Some(types) = &self.sighash_types {
            let encoded: Vec<Value> = types.iter().map(|t| Value::from(*t)).collect();
            object.insert("sighashTypes".to_string(), Value::Array(encoded));
        }
        Value::Object(object)
    }
}

/// The associated [`UnsignedTx`](WcCoinSigner::UnsignedTx) parameter type for
/// UTXO coins: a base64 BIP-174 PSBT plus the optional account selector and
/// per-input signing hints the wallet's `signPsbt` method consumes (§22.8.1.3).
///
/// This slice carries a caller-built PSBT; the module does not construct the
/// PSBT from a raw transaction. The internal field naming is informative (R36);
/// only the emitted wire shape (built by [`sign_psbt_params`]) is binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcUtxoPsbtParams {
    /// Optional source-account selector (`account`).
    pub account: Option<String>,
    /// The PSBT, base64-encoded (BIP-174) (`psbt`, required).
    pub psbt: String,
    /// Inputs the wallet should sign (`signInputs`); empty means "all".
    pub sign_inputs: Vec<WcUtxoSignInput>,
}

/// A parsed bip122 `getAccountAddresses` entry (§22.8.1.3): the address is
/// required; the compressed public key (hex), the BIP-32 path and the intention
/// hint are optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcUtxoAddressEntry {
    /// The address (required).
    pub address: String,
    /// Compressed public key, hex (optional).
    pub public_key: Option<String>,
    /// BIP-32 derivation path (optional).
    pub path: Option<String>,
    /// Address purpose hint, e.g. `payment` (optional).
    pub intention: Option<String>,
}

/// A parsed bip122 `signMessage` result (§22.8.1.3): the base64-decoded
/// signature bytes and the signing address echoed back by the wallet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcUtxoMessageSignature {
    /// The signature bytes (decoded from the base64 wire field).
    pub signature: Vec<u8>,
    /// The address that produced the signature.
    pub address: String,
}

/// Builds the inner `wc_sessionRequest` payload (§22.8.1): the CAIP-2 `chainId`
/// plus a `request` carrying the method string and its params. This becomes the
/// outer `wc_sessionRequest` `params` once wrapped by
/// [`WalletConnectCtx::send_session_request`].
fn session_request_payload(chain_id: &str, method: WcRequestMethods, params: Value) -> Value {
    json!({
        "chainId": chain_id,
        "request": {
            "method": method.as_ref(),
            "params": params,
        }
    })
}

/// Builds the `signPsbt` `params` object (§22.8.1.3): the base64 `psbt`, the
/// optional `account`, the optional `signInputs` array, and the `broadcast`
/// flag (`false` ⇒ sign-only, `true` ⇒ sign-and-broadcast).
fn sign_psbt_params(tx: &WcUtxoPsbtParams, broadcast: bool) -> Value {
    let mut object = Map::new();
    if let Some(account) = &tx.account {
        object.insert("account".to_string(), Value::String(account.clone()));
    }
    object.insert("psbt".to_string(), Value::String(tx.psbt.clone()));
    if !tx.sign_inputs.is_empty() {
        let inputs: Vec<Value> = tx.sign_inputs.iter().map(WcUtxoSignInput::to_request_object).collect();
        object.insert("signInputs".to_string(), Value::Array(inputs));
    }
    object.insert("broadcast".to_string(), Value::Bool(broadcast));
    Value::Object(object)
}

/// Builds the `signMessage` `params` object (§22.8.1.3):
/// `{ "address": <addr>, "message": <message> }`.
fn sign_message_params(address: &str, message: &str) -> Value { json!({ "address": address, "message": message }) }

/// Builds the `getAccountAddresses` `params` object (§22.8.1.3): an
/// empty selector object.
fn get_account_addresses_params() -> Value { json!({}) }

/// Builds the `sendTransfer` `params` object (§22.8.1.3): the required
/// `recipientAddress` / `amount` (base-unit string) plus the optional
/// `account` / `changeAddress` / `memo`.
fn send_transfer_params(
    account: Option<&str>,
    recipient_address: &str,
    amount: &str,
    change_address: Option<&str>,
    memo: Option<&str>,
) -> Value {
    let mut object = Map::new();
    if let Some(account) = account {
        object.insert("account".to_string(), Value::String(account.to_string()));
    }
    object.insert(
        "recipientAddress".to_string(),
        Value::String(recipient_address.to_string()),
    );
    object.insert("amount".to_string(), Value::String(amount.to_string()));
    if let Some(change) = change_address {
        object.insert("changeAddress".to_string(), Value::String(change.to_string()));
    }
    if let Some(memo) = memo {
        object.insert("memo".to_string(), Value::String(memo.to_string()));
    }
    Value::Object(object)
}

/// Reads a required string field out of a JSON object, mapping an absent or
/// non-string value to [`WalletConnectError::InvalidResponse`].
fn required_str(result: &Value, field: &str) -> Result<String, WalletConnectError> {
    result
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| WalletConnectError::InvalidResponse(format!("missing `{field}` field")))
}

/// Parses a `signPsbt` result `{ "psbt", "txid"? }` (§22.8.1.3). The signed PSBT
/// is always required; when `broadcast` was requested the `txid` must be present
/// (a missing `txid` after a broadcast request is rejected with
/// [`WalletConnectError::InvalidResponse`]).
fn parse_sign_psbt_result(result: &Value, broadcast: bool) -> Result<(String, Option<String>), WalletConnectError> {
    let psbt = required_str(result, "psbt")?;
    let txid = match result.get("txid").and_then(Value::as_str) {
        Some(txid) => Some(txid.to_string()),
        None => {
            if broadcast {
                return Err(WalletConnectError::InvalidResponse(
                    "broadcast was requested but the result carries no `txid`".to_string(),
                ));
            }
            None
        },
    };
    Ok((psbt, txid))
}

/// Parses a `sendTransfer` result `{ "txid" }` (§22.8.1.3).
fn parse_send_transfer_result(result: &Value) -> Result<String, WalletConnectError> { required_str(result, "txid") }

/// Parses a `signMessage` result `{ "signature", "address" }` (§22.8.1.3),
/// base64-decoding the signature field.
fn parse_sign_message_result(result: &Value) -> Result<WcUtxoMessageSignature, WalletConnectError> {
    let signature_b64 = required_str(result, "signature")?;
    let signature = base64::decode(&signature_b64)
        .map_err(|e| WalletConnectError::InvalidResponse(format!("invalid base64 signature: {e}")))?;
    let address = required_str(result, "address")?;
    Ok(WcUtxoMessageSignature { signature, address })
}

/// Parses a `getAccountAddresses` result: a JSON array of address entries
/// (§22.8.1.3 — `address` required, `publicKey` / `path` / `intention`
/// optional).
fn parse_account_addresses_result(result: &Value) -> Result<Vec<WcUtxoAddressEntry>, WalletConnectError> {
    let entries = result
        .as_array()
        .ok_or_else(|| WalletConnectError::InvalidResponse("expected an array of address entries".to_string()))?;
    entries
        .iter()
        .map(|entry| {
            let address = required_str(entry, "address")?;
            Ok(WcUtxoAddressEntry {
                address,
                public_key: entry.get("publicKey").and_then(Value::as_str).map(str::to_string),
                path: entry.get("path").and_then(Value::as_str).map(str::to_string),
                intention: entry.get("intention").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect()
}

impl UtxoStandardCoin {
    /// Fetches the genesis block hash in conventional big-endian **display**
    /// order from the active RPC backend.
    ///
    /// The genesis hash is not stored in the coin's configuration, so it is
    /// derived at call time: the native backend returns the hash directly
    /// (already in display order), while the Electrum backend returns the raw
    /// genesis header whose double-SHA256 is the internal-order hash, reversed
    /// for display.
    async fn wc_genesis_display_hash(&self) -> Result<H256, WalletConnectError> {
        match self.as_ref().rpc_client {
            UtxoRpcClientEnum::Native(ref native) => {
                let hash: H256Json = native
                    .get_block_hash(0)
                    .compat()
                    .await
                    .map_err(|e| WalletConnectError::Relay(format!("genesis block hash query failed: {e}")))?;
                // The JSON-RPC hash bytes are already in display order.
                Ok(H256::from(hash.0))
            },
            UtxoRpcClientEnum::Electrum(ref electrum) => {
                let header_bytes = electrum
                    .blockchain_block_header(0)
                    .compat()
                    .await
                    .map_err(|e| WalletConnectError::Relay(format!("genesis header query failed: {e:?}")))?;
                let header: BlockHeader = deserialize(header_bytes.0.as_slice())
                    .map_err(|e| WalletConnectError::InvalidResponse(format!("genesis header decode failed: {e:?}")))?;
                // `hash()` is internal order; reverse it for conventional display.
                Ok(header.hash().reversed())
            },
        }
    }

    /// This coin's CAIP-2 chain id `bip122:<first 32 hex chars of the genesis
    /// hash>` (§22.8.1.4).
    async fn wc_bip122_chain_id(&self) -> Result<String, WalletConnectError> {
        let genesis = self.wc_genesis_display_hash().await?;
        Ok(bip122_caip2_chain_id(&genesis))
    }

    /// Resolves the settled session for `chain_id`, sends `payload` as a
    /// `wc_sessionRequest`, and returns the JSON-RPC `result`.
    async fn wc_send_request(
        &self,
        wc: &WalletConnectCtx,
        chain_id: &str,
        payload: Value,
    ) -> Result<Value, WalletConnectError> {
        let topic = wc
            .sessions()
            .session_topic_for_chain(chain_id)
            .ok_or_else(|| WalletConnectError::SessionNotFound(chain_id.to_string()))?;
        let (sym_key, _encoding) = wc
            .sessions()
            .transport_for(&topic)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        wc.send_session_request(&topic, &sym_key, payload).await
    }

    /// Requests a bip122 `signMessage` over WalletConnect (§22.8.1.3),
    /// returning the wallet's signature and the echoed address. Emits the
    /// `signMessage` wire method, never `personal_sign`.
    pub async fn wc_sign_message(
        &self,
        wc: &WalletConnectCtx,
        message: &str,
    ) -> Result<WcUtxoMessageSignature, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        let address = utxo_common::my_address(self)
            .map_err(|e| WalletConnectError::Internal(format!("no signing address: {e}")))?;
        let params = sign_message_params(&address, message);
        let payload = session_request_payload(&chain_id, WcRequestMethods::UtxoPersonalSign, params);
        let result = self.wc_send_request(wc, &chain_id, payload).await?;
        parse_sign_message_result(&result)
    }

    /// Requests the bip122 `getAccountAddresses` enumeration over WalletConnect
    /// (§22.8.1.3), returning the parsed address entries.
    pub async fn wc_get_account_addresses(
        &self,
        wc: &WalletConnectCtx,
    ) -> Result<Vec<WcUtxoAddressEntry>, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        let params = get_account_addresses_params();
        let payload = session_request_payload(&chain_id, WcRequestMethods::UtxoGetAccountAddresses, params);
        let result = self.wc_send_request(wc, &chain_id, payload).await?;
        parse_account_addresses_result(&result)
    }

    /// Requests a bip122 `sendTransfer` over WalletConnect (§22.8.1.3): the
    /// wallet builds, signs and broadcasts the transfer, returning its txid.
    pub async fn wc_send_transfer(
        &self,
        wc: &WalletConnectCtx,
        account: Option<&str>,
        recipient_address: &str,
        amount: &str,
        change_address: Option<&str>,
        memo: Option<&str>,
    ) -> Result<String, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        let params = send_transfer_params(account, recipient_address, amount, change_address, memo);
        let payload = session_request_payload(&chain_id, WcRequestMethods::UtxoSendTransfer, params);
        let result = self.wc_send_request(wc, &chain_id, payload).await?;
        parse_send_transfer_result(&result)
    }
}

#[async_trait]
impl WcCoinSigner for UtxoStandardCoin {
    type UnsignedTx = WcUtxoPsbtParams;
    type SignedTx = String;
    type TxHash = String;

    async fn wc_chain_id(&self, _wc: &WalletConnectCtx) -> Result<String, WalletConnectError> {
        self.wc_bip122_chain_id().await
    }

    async fn wc_sign_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::SignedTx, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        let params = sign_psbt_params(&tx, false);
        let payload = session_request_payload(&chain_id, WcRequestMethods::UtxoSignPsbt, params);
        let result = self.wc_send_request(wc, &chain_id, payload).await?;
        let (signed_psbt, _txid) = parse_sign_psbt_result(&result, false)?;
        Ok(signed_psbt)
    }

    async fn wc_sign_and_send_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::TxHash, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        let params = sign_psbt_params(&tx, true);
        let payload = session_request_payload(&chain_id, WcRequestMethods::UtxoSignPsbt, params);
        let result = self.wc_send_request(wc, &chain_id, payload).await?;
        let (_signed_psbt, txid) = parse_sign_psbt_result(&result, true)?;
        txid.ok_or_else(|| {
            WalletConnectError::InvalidResponse("broadcast was requested but the result carries no `txid`".to_string())
        })
    }

    async fn wc_session_topic(&self, wc: &WalletConnectCtx) -> Result<Topic, WalletConnectError> {
        let chain_id = self.wc_bip122_chain_id().await?;
        wc.sessions()
            .session_topic_for_chain(&chain_id)
            .ok_or(WalletConnectError::SessionNotFound(chain_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bitcoin mainnet genesis block hash in conventional big-endian display
    /// order (the value block explorers print).
    const BTC_GENESIS_DISPLAY: &str = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";

    fn psbt_params() -> WcUtxoPsbtParams {
        WcUtxoPsbtParams {
            account: Some("acct-0".to_string()),
            psbt: "cHNidP8BAA==".to_string(),
            sign_inputs: vec![WcUtxoSignInput {
                address: "bc1qexampleaddr".to_string(),
                index: 0,
                sighash_types: Some(vec![1]),
            }],
        }
    }

    // (a) CAIP-2 bip122:<...> formatting + the 32-hex-char / 16-byte big-endian
    // truncation from a known genesis hash.
    #[test]
    fn caip2_bip122_truncates_to_16_bytes() {
        let genesis = H256::from(BTC_GENESIS_DISPLAY);
        let chain_id = bip122_caip2_chain_id(&genesis);
        assert_eq!(chain_id, "bip122:000000000019d6689c085ae165831e93");
        // 16 bytes ⇒ 32 hex chars after the `bip122:` prefix.
        assert_eq!(chain_id.strip_prefix("bip122:").unwrap().len(), 32);
    }

    // (b) signPsbt params shape (psbt base64, optional signInputs, broadcast
    // bool) inside the wc_sessionRequest envelope.
    #[test]
    fn sign_psbt_params_and_envelope_shape() {
        let params = sign_psbt_params(&psbt_params(), false);
        assert_eq!(params["psbt"], "cHNidP8BAA==");
        assert_eq!(params["account"], "acct-0");
        assert_eq!(params["broadcast"], false);
        let inputs = params["signInputs"].as_array().expect("signInputs array");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["address"], "bc1qexampleaddr");
        assert_eq!(inputs[0]["index"], 0);
        assert_eq!(inputs[0]["sighashTypes"], json!([1]));

        let payload = session_request_payload(
            "bip122:000000000019d6689c085ae165831e93",
            WcRequestMethods::UtxoSignPsbt,
            params,
        );
        assert_eq!(payload["chainId"], "bip122:000000000019d6689c085ae165831e93");
        assert_eq!(payload["request"]["method"], "signPsbt");
        assert_eq!(payload["request"]["params"]["psbt"], "cHNidP8BAA==");
    }

    #[test]
    fn sign_psbt_params_omit_optional_fields() {
        let params = sign_psbt_params(
            &WcUtxoPsbtParams {
                account: None,
                psbt: "cHNidP8BAA==".to_string(),
                sign_inputs: vec![],
            },
            false,
        );
        assert!(params.get("account").is_none());
        assert!(params.get("signInputs").is_none());
        assert_eq!(params["broadcast"], false);
    }

    // (c) wc_sign_transaction emits broadcast=false; wc_sign_and_send_transaction
    // emits broadcast=true. The trait methods are thin wrappers over
    // `sign_psbt_params`, so we assert the flag the builder embeds for each.
    #[test]
    fn broadcast_flag_matches_flow() {
        let sign_only = sign_psbt_params(&psbt_params(), false);
        assert_eq!(sign_only["broadcast"], false);
        let sign_and_send = sign_psbt_params(&psbt_params(), true);
        assert_eq!(sign_and_send["broadcast"], true);
        // The wire method is `signPsbt` for both flows.
        let payload = session_request_payload("bip122:abc", WcRequestMethods::UtxoSignPsbt, sign_and_send);
        assert_eq!(payload["request"]["method"], "signPsbt");
    }

    // (d) signMessage params {address, message} and the result parse
    // {signature(base64), address}; the emitted method string is `signMessage`,
    // NOT `personal_sign`.
    #[test]
    fn sign_message_params_and_method_string() {
        let params = sign_message_params("bc1qexampleaddr", "hello");
        assert_eq!(params["address"], "bc1qexampleaddr");
        assert_eq!(params["message"], "hello");

        let payload = session_request_payload("bip122:abc", WcRequestMethods::UtxoPersonalSign, params);
        assert_eq!(payload["request"]["method"], "signMessage");
        assert_ne!(payload["request"]["method"], "personal_sign");
    }

    #[test]
    fn sign_message_result_parse() {
        let signature_b64 = base64::encode(&[0xab_u8, 0xcd, 0xef]);
        let result = json!({ "signature": signature_b64, "address": "bc1qexampleaddr" });
        let parsed = parse_sign_message_result(&result).expect("valid result");
        assert_eq!(parsed.signature, vec![0xab, 0xcd, 0xef]);
        assert_eq!(parsed.address, "bc1qexampleaddr");
    }

    // (e) getAccountAddresses result parse into the typed entry vec (address
    // required, publicKey/path/intention optional).
    #[test]
    fn account_addresses_result_parse() {
        let result = json!([
            {
                "address": "bc1qfull",
                "publicKey": "02aabb",
                "path": "m/84'/0'/0'/0/0",
                "intention": "payment"
            },
            { "address": "bc1qminimal" }
        ]);
        let entries = parse_account_addresses_result(&result).expect("valid array");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].address, "bc1qfull");
        assert_eq!(entries[0].public_key.as_deref(), Some("02aabb"));
        assert_eq!(entries[0].path.as_deref(), Some("m/84'/0'/0'/0/0"));
        assert_eq!(entries[0].intention.as_deref(), Some("payment"));
        assert_eq!(entries[1].address, "bc1qminimal");
        assert!(entries[1].public_key.is_none());
        assert!(entries[1].path.is_none());
        assert!(entries[1].intention.is_none());
    }

    #[test]
    fn account_addresses_rejects_entry_without_address() {
        let result = json!([{ "publicKey": "02aabb" }]);
        assert!(matches!(
            parse_account_addresses_result(&result),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }

    // (f) result parsing rejects a malformed / `txid`-missing-after-broadcast
    // result with InvalidResponse.
    #[test]
    fn sign_psbt_result_parse_and_rejection() {
        // Sign-only: txid optional, psbt required.
        let signed_only = json!({ "psbt": "c2lnbmVk" });
        let (psbt, txid) = parse_sign_psbt_result(&signed_only, false).expect("valid sign-only");
        assert_eq!(psbt, "c2lnbmVk");
        assert!(txid.is_none());

        // Broadcast: txid present is accepted.
        let broadcast_ok = json!({ "psbt": "c2lnbmVk", "txid": "deadbeef" });
        let (_psbt, txid) = parse_sign_psbt_result(&broadcast_ok, true).expect("valid broadcast");
        assert_eq!(txid.as_deref(), Some("deadbeef"));

        // Broadcast requested but no txid ⇒ InvalidResponse.
        let broadcast_missing_txid = json!({ "psbt": "c2lnbmVk" });
        assert!(matches!(
            parse_sign_psbt_result(&broadcast_missing_txid, true),
            Err(WalletConnectError::InvalidResponse(_))
        ));

        // Missing psbt ⇒ InvalidResponse.
        let missing_psbt = json!({ "txid": "deadbeef" });
        assert!(matches!(
            parse_sign_psbt_result(&missing_psbt, false),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }

    #[test]
    fn send_transfer_params_and_result() {
        let params = send_transfer_params(None, "bc1qdest", "100000", None, Some("hi"));
        assert_eq!(params["recipientAddress"], "bc1qdest");
        assert_eq!(params["amount"], "100000");
        assert_eq!(params["memo"], "hi");
        assert!(params.get("account").is_none());
        assert!(params.get("changeAddress").is_none());

        let result = json!({ "txid": "abc123" });
        assert_eq!(parse_send_transfer_result(&result).unwrap(), "abc123");

        let malformed = json!({});
        assert!(matches!(
            parse_send_transfer_result(&malformed),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }
}
