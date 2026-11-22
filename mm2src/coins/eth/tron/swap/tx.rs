//! [`TronSwapTx`]: the signed TRON transaction a swap record keeps for a TRON leg (CRD ch.21 §21.6, §21.11C).
//!
//! Its bytes are the protobuf `Transaction` (the raw body and one signature), the form `broadcasthex` takes and
//! peers exchange. Its id is the SHA-256 of the raw body's bytes.

use super::contract_abi::{decode_call, TronSwapCall};
use super::TronSwapError;
use crate::eth::tron::proto::{ContractType, Transaction, TransactionRaw, TriggerSmartContract,
                              TRIGGER_SMART_CONTRACT_TYPE_URL};
use crate::eth::tron::sign::hash_transaction_raw;
use crate::eth::tron::TronAddress;
use ethereum_types::H256;
use prost::Message;
use sha2::{Digest, Sha256};

/// The length of a TRON transaction signature: `r`, `s` and the recovery byte.
const SIGNATURE_LEN: usize = 65;

/// The outer transaction with its raw body left as the bytes that were signed, so that the id of a received
/// transaction is computed over exactly those bytes. A message field and a bytes field have the same wire form.
#[derive(Clone, PartialEq, Message)]
struct TransactionEnvelope {
    #[prost(bytes = "vec", optional, tag = "1")]
    raw_data: Option<Vec<u8>>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    signature: Vec<Vec<u8>>,
}

/// A signed TRON transaction: its raw body, its one signature and its id.
#[derive(Clone, Debug, PartialEq)]
pub struct TronSwapTx {
    raw: TransactionRaw,
    signature: Vec<u8>,
    txid: H256,
}

/// The smart-contract call a transaction makes: who calls which contract with what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TronTrigger {
    /// The caller: the owner of the transaction.
    pub owner: TronAddress,
    /// The contract that is called.
    pub contract: TronAddress,
    /// The TRX the call carries, in SUN.
    pub call_value: i64,
    /// The call data, selector first.
    pub data: Vec<u8>,
}

impl TronSwapTx {
    /// The transaction of the raw body `raw` signed with `signature`. Its id is the hash of `raw`.
    pub fn new(raw: TransactionRaw, signature: Vec<u8>) -> Self {
        let txid = hash_transaction_raw(&raw);
        TronSwapTx { raw, signature, txid }
    }

    /// Decodes the protobuf bytes of a signed transaction.
    ///
    /// # Errors
    ///
    /// [`TronSwapError::Transaction`] when `bytes` are not a protobuf transaction with a raw body and exactly one
    /// 65-byte signature, or when the raw body does not re-encode to the bytes received (it carries something this
    /// build does not model, so its id could not be kept).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TronSwapError> {
        let malformed = |reason: &str| TronSwapError::Transaction(reason.to_owned());
        let envelope =
            TransactionEnvelope::decode(bytes).map_err(|_| malformed("the bytes are not a protobuf transaction"))?;
        let raw_bytes = envelope
            .raw_data
            .ok_or_else(|| malformed("the transaction has no raw body"))?;
        let raw = TransactionRaw::decode(raw_bytes.as_slice())
            .map_err(|_| malformed("the raw body is not a protobuf transaction body"))?;
        if raw.encode_to_vec() != raw_bytes {
            return Err(malformed("the raw body does not re-encode to the bytes received"));
        }
        let [signature] = <[Vec<u8>; 1]>::try_from(envelope.signature)
            .map_err(|_| malformed("the transaction does not carry exactly one signature"))?;
        if signature.len() != SIGNATURE_LEN {
            return Err(malformed("the signature is not 65 bytes"));
        }
        let txid = H256::from_slice(&Sha256::digest(&raw_bytes));
        Ok(TronSwapTx { raw, signature, txid })
    }

    /// The protobuf bytes of the signed transaction, the form `broadcasthex` takes.
    pub fn to_bytes(&self) -> Vec<u8> {
        Transaction {
            raw_data: Some(self.raw.clone()),
            signature: vec![self.signature.clone()],
        }
        .encode_to_vec()
    }

    /// The transaction id: the SHA-256 of the raw body's bytes.
    pub fn txid(&self) -> H256 { self.txid }

    /// The raw body.
    pub fn raw(&self) -> &TransactionRaw { &self.raw }

    /// The signature.
    pub fn signature(&self) -> &[u8] { &self.signature }

    /// The smart-contract call the transaction makes.
    ///
    /// # Errors
    ///
    /// [`TronSwapError::NotAContractCall`] when the transaction is not exactly one `TriggerSmartContract` (a TRX
    /// transfer, for example); [`TronSwapError::Transaction`] when the call's own encoding or one of its addresses
    /// is not valid.
    pub fn trigger(&self) -> Result<TronTrigger, TronSwapError> {
        let [contract] = self.raw.contract.as_slice() else {
            return Err(TronSwapError::NotAContractCall);
        };
        if contract.r#type != ContractType::TriggerSmartContract as i32 {
            return Err(TronSwapError::NotAContractCall);
        }
        let parameter = contract.parameter.as_ref().ok_or(TronSwapError::NotAContractCall)?;
        if parameter.type_url != TRIGGER_SMART_CONTRACT_TYPE_URL {
            return Err(TronSwapError::NotAContractCall);
        }
        let malformed = |reason: &str| TronSwapError::Transaction(reason.to_owned());
        let trigger = TriggerSmartContract::decode(parameter.value.as_slice())
            .map_err(|_| malformed("the contract call does not decode"))?;
        Ok(TronTrigger {
            owner: TronAddress::from_bytes(&trigger.owner_address)
                .map_err(|_| malformed("the caller is not a TRON address"))?,
            contract: TronAddress::from_bytes(&trigger.contract_address)
                .map_err(|_| malformed("the called contract is not a TRON address"))?,
            call_value: trigger.call_value,
            data: trigger.data,
        })
    }
}

/// The swap-contract call `tx` makes: the contract it calls, the call value in SUN, and the decoded call. The
/// caller compares the contract with the swap contract it expects.
///
/// # Errors
///
/// The errors of [`TronSwapTx::trigger`] and of [`decode_call`].
pub fn call_of(tx: &TronSwapTx) -> Result<(TronAddress, i64, TronSwapCall), TronSwapError> {
    let trigger = tx.trigger()?;
    let call = decode_call(&trigger.data)?;
    Ok((trigger.contract, trigger.call_value, call))
}

impl crate::Transaction for TronSwapTx {
    fn tx_hex(&self) -> Vec<u8> { self.to_bytes() }

    fn tx_hash(&self) -> crate::BytesJson { self.txid.as_bytes().to_vec().into() }
}
