//! WalletConnect v2 signing integration for EVM (`eip155`) coins.
//!
//! Implements the chapter 22 §22.3 integration trait
//! ([`WcCoinSigner`](kdf_walletconnect::integration::WcCoinSigner)) for
//! [`EthCoin`], plus the EVM `personal_sign` flow. All EVM-specific knowledge —
//! the CAIP-2 chain reference (§22.8.1.4), the `eth_signTransaction` /
//! `eth_sendTransaction` transaction-object shape and the `personal_sign`
//! parameters (§22.8.1.1) — lives here in the coin module, never in the
//! WalletConnect subsystem (binding R1/R2).
//!
//! Every request is carried inside the WC2 `wc_sessionRequest` envelope: this
//! module builds the inner `{ "chainId", "request": { "method", "params" } }`
//! payload (§22.8.1) and hands it to
//! [`WalletConnectCtx::send_session_request`], which wraps it as the outer
//! `wc_sessionRequest` `params`.

use super::{Action, Address, EthCoin, UnSignedEthTx, H256, U256};
use async_trait::async_trait;
use kdf_walletconnect::chain::WcRequestMethods;
use kdf_walletconnect::error::WalletConnectError;
use kdf_walletconnect::integration::WcCoinSigner;
use kdf_walletconnect::{Topic, WalletConnectCtx};
use serde_json::{json, Value};

/// Formats an EVM chain id as its CAIP-2 identifier `eip155:<decimal>`
/// (chapter 22 §22.8.1.4 — the reference is the chain id in **decimal**).
fn eip155_chain_id(chain_id: u64) -> String { format!("eip155:{chain_id}") }

/// Formats a `U256` as an Ethereum JSON-RPC `0x`-prefixed quantity (minimal
/// hex, `0x0` for zero).
fn u256_to_0x_quantity(value: &U256) -> String { format!("0x{value:x}") }

/// Formats a 20-byte address as a `0x`-prefixed hex string.
fn address_to_0x(address: &Address) -> String { format!("{address:#x}") }

/// EIP-1559 vs legacy fee selection for a §22.8.1.1 EVM transaction object.
///
/// Modelling the two fee modes as a closed enum makes the legacy `gasPrice` and
/// the EIP-1559 `maxFeePerGas` / `maxPriorityFeePerGas` pair **mutually
/// exclusive by construction** (§22.8.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WcEvmFee {
    /// Legacy pricing: a single `gasPrice`.
    Legacy { gas_price: U256 },
    /// EIP-1559 pricing: a `maxFeePerGas` / `maxPriorityFeePerGas` pair.
    Eip1559 {
        max_fee_per_gas: U256,
        max_priority_fee_per_gas: U256,
    },
}

/// The associated [`UnsignedTx`](WcCoinSigner::UnsignedTx) parameter type for
/// EVM coins: a thin chain-specific description of an unsigned EVM transaction,
/// from which the §22.8.1.1 transaction object is built.
///
/// The internal field naming is informative (R36); only the emitted wire shape
/// (built by [`WcEvmTxParams::to_request_object`]) is binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcEvmTxParams {
    /// Sender address (`from`, required).
    pub from: Address,
    /// Recipient address (`to`); `None` for contract creation.
    pub to: Option<Address>,
    /// Call data / contract input (`data`).
    pub data: Vec<u8>,
    /// Wei amount (`value`).
    pub value: U256,
    /// Gas limit (`gas`).
    pub gas: U256,
    /// Fee selection (legacy `gasPrice` or the EIP-1559 pair).
    pub fee: WcEvmFee,
    /// Account nonce (`nonce`).
    pub nonce: Option<U256>,
    /// Target chain id (`chainId`).
    pub chain_id: Option<u64>,
}

impl WcEvmTxParams {
    /// Builds the WalletConnect parameters from the coin's existing
    /// [`UnSignedEthTx`] (a legacy transaction carrying `gas_price`), the sender
    /// address, and the activated chain id. Legacy transactions map to
    /// [`WcEvmFee::Legacy`].
    pub fn from_unsigned(tx: &UnSignedEthTx, from: Address, chain_id: Option<u64>) -> Self {
        let to = match tx.action {
            Action::Call(address) => Some(address),
            Action::Create => None,
        };
        WcEvmTxParams {
            from,
            to,
            data: tx.data.to_vec(),
            value: tx.value,
            gas: tx.gas,
            fee: WcEvmFee::Legacy {
                gas_price: tx.gas_price,
            },
            nonce: Some(tx.nonce),
            chain_id,
        }
    }

    /// Builds the §22.8.1.1 EVM transaction object. Present quantity fields are
    /// emitted as `0x`-prefixed hex; the canonical `data` spelling is always
    /// used (never the `input` alias); the fee mode selects either `gasPrice`
    /// or the `maxFeePerGas` / `maxPriorityFeePerGas` pair, never both.
    pub fn to_request_object(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("from".to_string(), Value::String(address_to_0x(&self.from)));
        if let Some(to) = &self.to {
            object.insert("to".to_string(), Value::String(address_to_0x(to)));
        }
        if !self.data.is_empty() {
            object.insert(
                "data".to_string(),
                Value::String(format!("0x{}", hex::encode(&self.data))),
            );
        }
        object.insert("value".to_string(), Value::String(u256_to_0x_quantity(&self.value)));
        object.insert("gas".to_string(), Value::String(u256_to_0x_quantity(&self.gas)));
        match &self.fee {
            WcEvmFee::Legacy { gas_price } => {
                object.insert("gasPrice".to_string(), Value::String(u256_to_0x_quantity(gas_price)));
            },
            WcEvmFee::Eip1559 {
                max_fee_per_gas,
                max_priority_fee_per_gas,
            } => {
                object.insert(
                    "maxFeePerGas".to_string(),
                    Value::String(u256_to_0x_quantity(max_fee_per_gas)),
                );
                object.insert(
                    "maxPriorityFeePerGas".to_string(),
                    Value::String(u256_to_0x_quantity(max_priority_fee_per_gas)),
                );
            },
        }
        if let Some(nonce) = &self.nonce {
            object.insert("nonce".to_string(), Value::String(u256_to_0x_quantity(nonce)));
        }
        if let Some(chain_id) = self.chain_id {
            object.insert("chainId".to_string(), Value::String(format!("0x{chain_id:x}")));
        }
        Value::Object(object)
    }
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

/// Builds the `personal_sign` (EIP-191) parameter array `[challenge, address]`
/// (§22.8.1.1): the message hex-encoded with a `0x` prefix, then the signer's
/// `0x` address.
fn personal_sign_params(message: &[u8], address: &Address) -> Value {
    json!([format!("0x{}", hex::encode(message)), address_to_0x(address)])
}

/// Decodes a `0x`-prefixed hex string out of a JSON-RPC `result` value.
fn decode_0x_hex(value: &Value) -> Result<Vec<u8>, WalletConnectError> {
    let text = value
        .as_str()
        .ok_or_else(|| WalletConnectError::InvalidResponse(format!("expected a 0x-hex string, got `{value}`")))?;
    let body = text
        .strip_prefix("0x")
        .ok_or_else(|| WalletConnectError::InvalidResponse(format!("response `{text}` is missing the 0x prefix")))?;
    hex::decode(body)
        .map_err(|e| WalletConnectError::InvalidResponse(format!("response `{text}` is not valid hex: {e}")))
}

/// Parses an `eth_signTransaction` result: the signed raw transaction bytes
/// (RLP), which must be non-empty.
fn parse_signed_tx_result(value: &Value) -> Result<Vec<u8>, WalletConnectError> {
    let bytes = decode_0x_hex(value)?;
    if bytes.is_empty() {
        return Err(WalletConnectError::InvalidResponse(
            "signed transaction result is empty".to_string(),
        ));
    }
    Ok(bytes)
}

/// Parses an `eth_sendTransaction` result: a 32-byte transaction hash.
fn parse_tx_hash_result(value: &Value) -> Result<H256, WalletConnectError> {
    let bytes = decode_0x_hex(value)?;
    if bytes.len() != 32 {
        return Err(WalletConnectError::InvalidResponse(format!(
            "transaction hash must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(H256::from_slice(&bytes))
}

/// Parses a `personal_sign` result: a 65-byte `r ‖ s ‖ v` signature.
fn parse_signature_result(value: &Value) -> Result<[u8; 65], WalletConnectError> {
    let bytes = decode_0x_hex(value)?;
    if bytes.len() != 65 {
        return Err(WalletConnectError::InvalidResponse(format!(
            "signature must be 65 bytes, got {}",
            bytes.len()
        )));
    }
    let mut signature = [0u8; 65];
    signature.copy_from_slice(&bytes);
    Ok(signature)
}

impl EthCoin {
    /// This coin's CAIP-2 chain id (`eip155:<decimal>`), or an error if the coin
    /// was activated without a chain id.
    fn wc_eip155_chain_id(&self) -> Result<String, WalletConnectError> {
        let chain_id = self
            .chain_id()
            .ok_or_else(|| WalletConnectError::UnsupportedChain(format!("{} has no EVM chain id", self.ticker)))?;
        Ok(eip155_chain_id(chain_id))
    }

    /// Resolves the settled session for this coin's chain, sends `payload` as a
    /// `wc_sessionRequest`, and returns the JSON-RPC `result`.
    async fn wc_round_trip(&self, wc: &WalletConnectCtx, payload: Value) -> Result<Value, WalletConnectError> {
        let chain_id = self.wc_eip155_chain_id()?;
        let topic = wc
            .sessions()
            .session_topic_for_chain(&chain_id)
            .ok_or_else(|| WalletConnectError::SessionNotFound(chain_id.clone()))?;
        let (sym_key, _encoding) = wc
            .sessions()
            .transport_for(&topic)
            .ok_or_else(|| WalletConnectError::SessionNotFound(topic.to_string()))?;
        wc.send_session_request(&topic, &sym_key, payload).await
    }

    /// Requests an EIP-191 `personal_sign` over WalletConnect, returning the
    /// 65-byte `r ‖ s ‖ v` signature (§22.8.1.1).
    pub async fn wc_personal_sign(
        &self,
        wc: &WalletConnectCtx,
        message: &[u8],
    ) -> Result<[u8; 65], WalletConnectError> {
        let chain_id = self.wc_eip155_chain_id()?;
        let params = personal_sign_params(message, &self.my_address);
        let payload = session_request_payload(&chain_id, WcRequestMethods::EthPersonalSign, params);
        let result = self.wc_round_trip(wc, payload).await?;
        parse_signature_result(&result)
    }
}

#[async_trait]
impl WcCoinSigner for EthCoin {
    type UnsignedTx = WcEvmTxParams;
    type SignedTx = Vec<u8>;
    type TxHash = H256;

    async fn wc_chain_id(&self, _wc: &WalletConnectCtx) -> Result<String, WalletConnectError> {
        self.wc_eip155_chain_id()
    }

    async fn wc_sign_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::SignedTx, WalletConnectError> {
        let chain_id = self.wc_eip155_chain_id()?;
        let params = json!([tx.to_request_object()]);
        let payload = session_request_payload(&chain_id, WcRequestMethods::EthSignTransaction, params);
        let result = self.wc_round_trip(wc, payload).await?;
        parse_signed_tx_result(&result)
    }

    async fn wc_sign_and_send_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::TxHash, WalletConnectError> {
        let chain_id = self.wc_eip155_chain_id()?;
        let params = json!([tx.to_request_object()]);
        let payload = session_request_payload(&chain_id, WcRequestMethods::EthSendTransaction, params);
        let result = self.wc_round_trip(wc, payload).await?;
        parse_tx_hash_result(&result)
    }

    async fn wc_session_topic(&self, wc: &WalletConnectCtx) -> Result<Topic, WalletConnectError> {
        let chain_id = self.wc_eip155_chain_id()?;
        wc.sessions()
            .session_topic_for_chain(&chain_id)
            .ok_or_else(|| WalletConnectError::SessionNotFound(chain_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address { Address::from([byte; 20]) }

    fn legacy_params() -> WcEvmTxParams {
        WcEvmTxParams {
            from: addr(0x11),
            to: Some(addr(0x22)),
            data: vec![0xde, 0xad, 0xbe, 0xef],
            value: U256::from(1_000_000_000u64),
            gas: U256::from(21_000u64),
            fee: WcEvmFee::Legacy {
                gas_price: U256::from(20_000_000_000u64),
            },
            nonce: Some(U256::from(7u64)),
            chain_id: Some(1),
        }
    }

    // (a) CAIP-2 formatting: eip155:<decimal>.
    #[test]
    fn caip2_chain_id_is_decimal() {
        assert_eq!(eip155_chain_id(1), "eip155:1");
        assert_eq!(eip155_chain_id(137), "eip155:137");
        assert_eq!(eip155_chain_id(56), "eip155:56");
    }

    // (b) EVM transaction-object construction: field spellings, 0x-hex, `data`
    // (not `input`), legacy gasPrice present and the 1559 pair absent.
    #[test]
    fn legacy_tx_object_field_spellings() {
        let object = legacy_params().to_request_object();
        assert_eq!(object["from"], "0x1111111111111111111111111111111111111111");
        assert_eq!(object["to"], "0x2222222222222222222222222222222222222222");
        assert_eq!(object["data"], "0xdeadbeef");
        assert!(object.get("input").is_none(), "must emit `data`, never `input`");
        assert_eq!(object["value"], "0x3b9aca00");
        assert_eq!(object["gas"], "0x5208");
        assert_eq!(object["gasPrice"], "0x4a817c800");
        assert_eq!(object["nonce"], "0x7");
        assert_eq!(object["chainId"], "0x1");
        // Legacy and 1559 fee fields are mutually exclusive.
        assert!(object.get("maxFeePerGas").is_none());
        assert!(object.get("maxPriorityFeePerGas").is_none());
    }

    #[test]
    fn eip1559_tx_object_excludes_legacy_gas_price() {
        let mut params = legacy_params();
        params.fee = WcEvmFee::Eip1559 {
            max_fee_per_gas: U256::from(30_000_000_000u64),
            max_priority_fee_per_gas: U256::from(2_000_000_000u64),
        };
        let object = params.to_request_object();
        assert_eq!(object["maxFeePerGas"], "0x6fc23ac00");
        assert_eq!(object["maxPriorityFeePerGas"], "0x77359400");
        assert!(object.get("gasPrice").is_none(), "1559 tx must not carry gasPrice");
    }

    #[test]
    fn contract_creation_omits_to() {
        let mut params = legacy_params();
        params.to = None;
        let object = params.to_request_object();
        assert!(object.get("to").is_none());
    }

    #[test]
    fn from_unsigned_maps_legacy_fields() {
        let unsigned = UnSignedEthTx {
            nonce: U256::from(3u64),
            gas_price: U256::from(15u64),
            gas: U256::from(21_000u64),
            action: Action::Call(addr(0x44)),
            value: U256::from(99u64),
            data: vec![0x01, 0x02].into(),
        };
        let params = WcEvmTxParams::from_unsigned(&unsigned, addr(0x33), Some(137));
        assert_eq!(params.from, addr(0x33));
        assert_eq!(params.to, Some(addr(0x44)));
        assert_eq!(params.nonce, Some(U256::from(3u64)));
        assert_eq!(params.chain_id, Some(137));
        assert_eq!(params.fee, WcEvmFee::Legacy {
            gas_price: U256::from(15u64)
        });
        let object = params.to_request_object();
        assert_eq!(object["data"], "0x0102");
        assert_eq!(object["chainId"], "0x89");
    }

    // (c) Full wc_sessionRequest envelope for both eth methods.
    #[test]
    fn sign_transaction_envelope_shape() {
        let params = json!([legacy_params().to_request_object()]);
        let payload = session_request_payload("eip155:1", WcRequestMethods::EthSignTransaction, params);
        assert_eq!(payload["chainId"], "eip155:1");
        assert_eq!(payload["request"]["method"], "eth_signTransaction");
        assert!(payload["request"]["params"].is_array());
        assert_eq!(payload["request"]["params"].as_array().unwrap().len(), 1);
        assert_eq!(
            payload["request"]["params"][0]["from"],
            "0x1111111111111111111111111111111111111111"
        );
    }

    #[test]
    fn send_transaction_envelope_shape() {
        let params = json!([legacy_params().to_request_object()]);
        let payload = session_request_payload("eip155:137", WcRequestMethods::EthSendTransaction, params);
        assert_eq!(payload["chainId"], "eip155:137");
        assert_eq!(payload["request"]["method"], "eth_sendTransaction");
        assert!(payload["request"]["params"].is_array());
    }

    // (d) personal_sign param ordering [challenge, address] + signature parsing.
    #[test]
    fn personal_sign_param_ordering() {
        let params = personal_sign_params(b"hello", &addr(0xaa));
        let array = params.as_array().expect("params is an array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0], "0x68656c6c6f"); // "hello" hex-encoded
        assert_eq!(array[1], "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn parse_signature_result_ok_and_malformed() {
        let good = Value::String(format!("0x{}", hex::encode([0x5au8; 65])));
        let signature = parse_signature_result(&good).expect("valid 65-byte signature");
        assert_eq!(signature, [0x5au8; 65]);

        // Wrong length.
        let short = Value::String(format!("0x{}", hex::encode([0x5au8; 64])));
        assert!(matches!(
            parse_signature_result(&short),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        // Missing 0x prefix.
        let no_prefix = Value::String(hex::encode([0x5au8; 65]));
        assert!(matches!(
            parse_signature_result(&no_prefix),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }

    // (e) result parsing for signed tx + tx hash, good and malformed.
    #[test]
    fn parse_signed_tx_result_ok_and_malformed() {
        let good = Value::String("0xabcdef".to_string());
        assert_eq!(parse_signed_tx_result(&good).unwrap(), vec![0xab, 0xcd, 0xef]);

        let empty = Value::String("0x".to_string());
        assert!(matches!(
            parse_signed_tx_result(&empty),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        let not_string = json!(42);
        assert!(matches!(
            parse_signed_tx_result(&not_string),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }

    #[test]
    fn parse_tx_hash_result_ok_and_malformed() {
        let hash_bytes = [0x9bu8; 32];
        let good = Value::String(format!("0x{}", hex::encode(hash_bytes)));
        assert_eq!(parse_tx_hash_result(&good).unwrap(), H256::from(hash_bytes));

        let wrong_len = Value::String(format!("0x{}", hex::encode([0x9bu8; 16])));
        assert!(matches!(
            parse_tx_hash_result(&wrong_len),
            Err(WalletConnectError::InvalidResponse(_))
        ));
        let bad_hex = Value::String("0xzz".to_string());
        assert!(matches!(
            parse_tx_hash_result(&bad_hex),
            Err(WalletConnectError::InvalidResponse(_))
        ));
    }
}
