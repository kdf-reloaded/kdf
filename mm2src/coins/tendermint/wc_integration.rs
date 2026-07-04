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

use super::rpc::*;
use super::tendermint_helpers::TendermintCommons;
use super::TendermintCoin;
use async_trait::async_trait;
use cosmrs::proto::cosmos::tx::v1beta1::TxRaw;
use cosmrs::proto::prost::Message;
use cosmrs::tx::{self, Fee, ModeInfo, SignMode, SignerInfo};
use cosmrs::Any;
use kdf_walletconnect::chain::WcRequestMethods;
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::integration::WcCoinSigner;
use kdf_walletconnect::{Topic, WalletConnectCtx};
use serde_json::{json, Value};

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

/// Builds the `cosmos_signAmino` `params` object (§22.8.1.2): the Amino
/// `StdSignDoc` with snake_case `chain_id` / `account_number` / `sequence`,
/// the `{ amount, gas }` fee, the message array and the memo.
fn cosmos_sign_amino_params(tx: &WcCosmosTxParams) -> Value {
    json!({
        "signerAddress": tx.signer_address,
        "signDoc": {
            "chain_id": tx.sign_doc_chain_id,
            "account_number": tx.account_number.to_string(),
            "sequence": tx.sequence.to_string(),
            "fee": cosmos_amino_fee_json(&tx.fee),
            "msgs": tx.amino_msgs,
            "memo": tx.memo,
        }
    })
}

/// Builds the Amino fee object `{ "amount": [{ denom, amount }], "gas": <str> }`
/// (§22.8.1.2) from the cosmrs [`Fee`]; amounts and gas are decimal strings.
fn cosmos_amino_fee_json(fee: &Fee) -> Value {
    let amount: Vec<Value> = fee
        .amount
        .iter()
        .map(|coin| json!({ "denom": coin.denom.to_string(), "amount": coin.amount.to_string() }))
        .collect();
    json!({ "amount": amount, "gas": fee.gas_limit.to_string() })
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

    /// Resolves the session, selects the sign mode from the matching
    /// `sessionProperties.keys` `isNanoLedger` flag, shapes and sends the
    /// `wc_sessionRequest`, and assembles the broadcast-ready signed `TxRaw`.
    async fn wc_request_cosmos_signature(
        &self,
        wc: &WalletConnectCtx,
        tx: &WcCosmosTxParams,
    ) -> Result<Vec<u8>, WalletConnectError> {
        let chain_id = self.wc_cosmos_chain_id();
        let topic = wc
            .sessions()
            .session_topic_for_chain(&chain_id)
            .ok_or_else(|| WalletConnectError::SessionNotFound(chain_id.clone()))?;
        let (wallet_name, key_entry) = wc
            .sessions()
            .signing_account_details(&topic, &tx.signer_address)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        let encoding = CosmosByteEncoding::for_wallet_name(&wallet_name);
        let is_nano_ledger = key_entry.map(|key| key.is_nano_ledger).unwrap_or(false);
        let method = select_cosmos_method(is_nano_ledger);

        let params = if is_nano_ledger {
            cosmos_sign_amino_params(tx)
        } else {
            cosmos_sign_direct_params(tx, encoding)
        };
        let payload = session_request_payload(&chain_id, method, params);

        let (sym_key, _encoding) = wc
            .sessions()
            .transport_for(&topic)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        let result = wc.send_session_request(&topic, &sym_key, payload).await?;

        let signature = parse_cosmos_signature(&result, encoding)?;
        if is_nano_ledger {
            self.assemble_amino_tx(tx, signature)
        } else {
            let (body_bytes, auth_info_bytes) = parse_signed_direct(&result, encoding)?;
            Ok(assemble_tx_raw(body_bytes, auth_info_bytes, signature))
        }
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
        self.wc_request_cosmos_signature(wc, &tx).await
    }

    async fn wc_sign_and_send_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::TxHash, WalletConnectError> {
        let tx_bytes = self.wc_request_cosmos_signature(wc, &tx).await?;
        self.wc_broadcast_cosmos_tx(tx_bytes).await
    }

    async fn wc_session_topic(&self, wc: &WalletConnectCtx) -> Result<Topic, WalletConnectError> {
        let chain_id = self.wc_cosmos_chain_id();
        wc.sessions()
            .session_topic_for_chain(&chain_id)
            .ok_or_else(|| WalletConnectError::SessionNotFound(chain_id))
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
