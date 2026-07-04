//! ethabi-compatible ABI facade backed by alloy (`alloy-json-abi` +
//! `alloy-dyn-abi`).
//!
//! KDF works with a handful of smart-contract ABIs known at compile time (the
//! v1/v2 atomic-swap contracts, ERC-20/721/1155, the QTUM delegation contract).
//! Historically this went through `ethabi`; this module provides the same small
//! API surface — `Token`, `Contract`, `Function`, `encode`, `AbiError` — on top
//! of alloy so the rest of the crate keeps its existing call sites.
//!
//! The alloy backend is proven byte-identical to the previous `ethabi` output by
//! `abi_golden_tests` (frozen calldata vectors) and, during the migration, by
//! `abi_alloy_differential_tests`.
//!
//! Primitives stay `ethereum_types` (`Address` = `H160`, `U256`) to match the
//! rest of the eth module; conversion to/from alloy primitives happens only at
//! the encode/decode boundary here.

use alloy_dyn_abi::{DynSolType, DynSolValue, FunctionExt, JsonAbiExt};
use alloy_json_abi::JsonAbi;
use ethereum_types::{Address, H256, U256};
use std::collections::HashMap;
use std::fmt;

/// ABI value type. Mirrors the subset of `ethabi::Token` KDF constructs and
/// pattern-matches on.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Address(Address),
    Uint(U256),
    Int(U256),
    Bool(bool),
    FixedBytes(Vec<u8>),
    Bytes(Vec<u8>),
    String(String),
    Tuple(Vec<Token>),
    Array(Vec<Token>),
    FixedArray(Vec<Token>),
}

/// ABI error surfaced by the facade. Replaces `ethabi::Error` at the crate's
/// `From` conversion boundaries.
#[derive(Clone, Debug, PartialEq)]
pub enum AbiError {
    /// The ABI JSON could not be parsed.
    Parse(String),
    /// Encoding the provided tokens failed.
    Encode(String),
    /// Decoding the provided bytes failed.
    Decode(String),
    /// No function with the requested name exists in the ABI.
    FunctionNotFound(String),
    /// Input did not match the expected shape (wrong arity/length/type).
    InvalidData,
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AbiError::Parse(e) => write!(f, "ABI parse error: {e}"),
            AbiError::Encode(e) => write!(f, "ABI encode error: {e}"),
            AbiError::Decode(e) => write!(f, "ABI decode error: {e}"),
            AbiError::FunctionNotFound(name) => write!(f, "Invalid function name: {name}"),
            AbiError::InvalidData => write!(f, "Invalid data"),
        }
    }
}

impl std::error::Error for AbiError {}

fn to_alloy_u256(v: &U256) -> alloy_primitives::U256 {
    let mut be = [0u8; 32];
    v.to_big_endian(&mut be);
    alloy_primitives::U256::from_be_slice(&be)
}

fn to_eth_u256(v: &alloy_primitives::U256) -> U256 { U256::from_big_endian(&v.to_be_bytes::<32>()) }

fn to_alloy_addr(a: &Address) -> alloy_primitives::Address { alloy_primitives::Address::from(a.0) }

fn to_eth_addr(a: &alloy_primitives::Address) -> Address { Address::from(a.into_array()) }

/// Convert a facade `Token` into the alloy `DynSolValue` for a known target
/// type. `ty` is `None` for the free [`encode`] path, where the intrinsic type
/// of the token is used (uints default to 256-bit, fixed-bytes to their byte
/// length).
fn token_to_dyn(token: &Token, ty: Option<&DynSolType>) -> Result<DynSolValue, AbiError> {
    let uint_bits = |ty: Option<&DynSolType>| match ty {
        Some(DynSolType::Uint(bits)) => *bits,
        _ => 256,
    };
    let int_bits = |ty: Option<&DynSolType>| match ty {
        Some(DynSolType::Int(bits)) => *bits,
        _ => 256,
    };
    let value = match token {
        Token::Address(a) => DynSolValue::Address(to_alloy_addr(a)),
        Token::Uint(u) => DynSolValue::Uint(to_alloy_u256(u), uint_bits(ty)),
        Token::Int(u) => DynSolValue::Int(alloy_primitives::I256::from_raw(to_alloy_u256(u)), int_bits(ty)),
        Token::Bool(b) => DynSolValue::Bool(*b),
        Token::FixedBytes(bytes) => {
            let size = match ty {
                Some(DynSolType::FixedBytes(n)) => *n,
                _ => bytes.len(),
            };
            let mut word = [0u8; 32];
            let copy = bytes.len().min(32);
            word[..copy].copy_from_slice(&bytes[..copy]);
            DynSolValue::FixedBytes(alloy_primitives::B256::from(word), size)
        },
        Token::Bytes(bytes) => DynSolValue::Bytes(bytes.clone()),
        Token::String(s) => DynSolValue::String(s.clone()),
        Token::Tuple(items) => {
            let inner = match ty {
                Some(DynSolType::Tuple(tys)) => items
                    .iter()
                    .zip(tys)
                    .map(|(t, ty)| token_to_dyn(t, Some(ty)))
                    .collect::<Result<Vec<DynSolValue>, AbiError>>()?,
                _ => items
                    .iter()
                    .map(|t| token_to_dyn(t, None))
                    .collect::<Result<Vec<DynSolValue>, AbiError>>()?,
            };
            DynSolValue::Tuple(inner)
        },
        Token::Array(items) => {
            let elem_ty = match ty {
                Some(DynSolType::Array(inner)) => Some(inner.as_ref()),
                _ => None,
            };
            DynSolValue::Array(
                items
                    .iter()
                    .map(|t| token_to_dyn(t, elem_ty))
                    .collect::<Result<Vec<DynSolValue>, AbiError>>()?,
            )
        },
        Token::FixedArray(items) => {
            let elem_ty = match ty {
                Some(DynSolType::FixedArray(inner, _)) => Some(inner.as_ref()),
                _ => None,
            };
            DynSolValue::FixedArray(
                items
                    .iter()
                    .map(|t| token_to_dyn(t, elem_ty))
                    .collect::<Result<Vec<DynSolValue>, AbiError>>()?,
            )
        },
    };
    Ok(value)
}

/// Convert an alloy `DynSolValue` back into a facade `Token`.
fn dyn_to_token(value: &DynSolValue) -> Token {
    match value {
        DynSolValue::Address(a) => Token::Address(to_eth_addr(a)),
        DynSolValue::Uint(u, _) => Token::Uint(to_eth_u256(u)),
        DynSolValue::Int(i, _) => Token::Int(to_eth_u256(&i.into_raw())),
        DynSolValue::Bool(b) => Token::Bool(*b),
        DynSolValue::FixedBytes(word, size) => Token::FixedBytes(word.as_slice()[..(*size).min(32)].to_vec()),
        DynSolValue::Bytes(bytes) => Token::Bytes(bytes.clone()),
        DynSolValue::String(s) => Token::String(s.clone()),
        DynSolValue::Tuple(items) => Token::Tuple(items.iter().map(dyn_to_token).collect()),
        DynSolValue::Array(items) => Token::Array(items.iter().map(dyn_to_token).collect()),
        DynSolValue::FixedArray(items) => Token::FixedArray(items.iter().map(dyn_to_token).collect()),
        // Type KDF's ABIs never contain; represented as its 24-byte encoding for
        // completeness (never hit on any live path).
        DynSolValue::Function(f) => Token::FixedBytes(f.as_slice().to_vec()),
    }
}

/// A single ABI function, with its input/output solidity types pre-resolved.
pub struct Function {
    inner: alloy_json_abi::Function,
    input_types: Vec<DynSolType>,
    output_types: Vec<DynSolType>,
}

impl Function {
    /// 4-byte function selector.
    pub fn short_signature(&self) -> [u8; 4] { self.inner.selector().0 }

    /// The function's name.
    pub fn name(&self) -> &str { &self.inner.name }

    /// Encode `tokens` as calldata: 4-byte selector followed by the ABI-encoded
    /// arguments.
    pub fn encode_input(&self, tokens: &[Token]) -> Result<Vec<u8>, AbiError> {
        if tokens.len() != self.input_types.len() {
            return Err(AbiError::InvalidData);
        }
        let values = tokens
            .iter()
            .zip(&self.input_types)
            .map(|(token, ty)| token_to_dyn(token, Some(ty)))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner
            .abi_encode_input(&values)
            .map_err(|e| AbiError::Encode(e.to_string()))
    }

    /// Decode the ABI-encoded argument bytes (WITHOUT the 4-byte selector).
    pub fn decode_input(&self, data: &[u8]) -> Result<Vec<Token>, AbiError> {
        let values = self
            .inner
            .abi_decode_input(data)
            .map_err(|e| AbiError::Decode(e.to_string()))?;
        Ok(values.iter().map(dyn_to_token).collect())
    }

    /// Decode ABI-encoded return data (as produced by an `eth_call`).
    pub fn decode_output(&self, data: &[u8]) -> Result<Vec<Token>, AbiError> {
        let _ = &self.output_types;
        let values = self
            .inner
            .abi_decode_output(data)
            .map_err(|e| AbiError::Decode(e.to_string()))?;
        Ok(values.iter().map(dyn_to_token).collect())
    }
}

/// A parsed contract ABI. Functions are resolved once at load time and looked
/// up by name.
pub struct Contract {
    functions: HashMap<String, Function>,
    events: HashMap<String, Event>,
}

/// Resolve an ABI parameter's Solidity type string (e.g. `uint256`, `bytes20`,
/// `address`) to an alloy [`DynSolType`]. KDF's ABIs use only flat parameter
/// types (no nested `tuple` components), so parsing `param.ty` is sufficient.
fn parse_param_type(param: &alloy_json_abi::Param) -> Result<DynSolType, AbiError> {
    DynSolType::parse(&param.ty).map_err(|e| AbiError::Parse(format!("{}: {e}", param.ty)))
}

impl Contract {
    /// Parse a JSON ABI document.
    pub fn load(bytes: &[u8]) -> Result<Contract, AbiError> {
        let abi: JsonAbi = serde_json::from_slice(bytes).map_err(|e| AbiError::Parse(e.to_string()))?;
        let mut functions = HashMap::new();
        for (name, overloads) in &abi.functions {
            let Some(f) = overloads.first() else { continue };
            let input_types = f.inputs.iter().map(parse_param_type).collect::<Result<Vec<_>, _>>()?;
            let output_types = f.outputs.iter().map(parse_param_type).collect::<Result<Vec<_>, _>>()?;
            functions.insert(name.clone(), Function {
                inner: f.clone(),
                input_types,
                output_types,
            });
        }
        let mut events = HashMap::new();
        for (name, overloads) in &abi.events {
            let Some(e) = overloads.first() else { continue };
            events.insert(name.clone(), Event { inner: e.clone() });
        }
        Ok(Contract { functions, events })
    }

    /// Look up a function by name.
    pub fn function(&self, name: &str) -> Result<&Function, AbiError> {
        self.functions
            .get(name)
            .ok_or_else(|| AbiError::FunctionNotFound(name.to_string()))
    }

    /// Look up an event by name.
    pub fn event(&self, name: &str) -> Result<&Event, AbiError> {
        self.events
            .get(name)
            .ok_or_else(|| AbiError::FunctionNotFound(name.to_string()))
    }
}

/// A contract event. Only the topic-0 signature hash is exposed — all KDF uses
/// events for is building `eth_getLogs` topic filters.
pub struct Event {
    inner: alloy_json_abi::Event,
}

impl Event {
    /// Topic-0: `keccak256` of the canonical event signature.
    pub fn signature(&self) -> H256 { H256::from_slice(self.inner.selector().as_slice()) }
}

/// Free-standing ABI encoding of a token sequence (no function selector), the
/// equivalent of `ethabi::encode`. Token intrinsic types are used (uints are
/// 256-bit, fixed-bytes take their byte length).
pub fn encode(tokens: &[Token]) -> Vec<u8> {
    let values: Vec<DynSolValue> = tokens
        .iter()
        .map(|t| token_to_dyn(t, None).expect("intrinsic token conversion is infallible"))
        .collect();
    DynSolValue::Tuple(values).abi_encode_params()
}

#[cfg(test)]
mod tests {
    //! Validate the alloy-backed facade against the same calldata bytes frozen
    //! in `abi_golden_tests`. This proves the `Token` -> `DynSolValue` coercion
    //! and encode/decode paths produce ethabi-identical output.
    use super::*;
    use crate::eth::eth_types::{ERC20_ABI, SWAP_CONTRACT_ABI};

    const QTUM_ABI: &str = r#"[{"constant":false,"inputs":[{"internalType":"address","name":"_staker","type":"address"},{"internalType":"uint8","name":"_fee","type":"uint8"},{"internalType":"bytes","name":"_PoD","type":"bytes"}],"name":"addDelegation","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[],"name":"removeDelegation","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"}]"#;

    fn addr(byte: u8) -> Address { Address::from([byte; 20]) }

    #[test]
    fn facade_encode_matches_frozen_erc20_transfer() {
        let c = Contract::load(ERC20_ABI.as_bytes()).unwrap();
        let cd = c
            .function("transfer")
            .unwrap()
            .encode_input(&[Token::Address(addr(0x11)), Token::Uint(U256::from(123_456_789u64))])
            .unwrap();
        assert_eq!(
            hex::encode(cd),
            "a9059cbb000000000000000000000000111111111111111111111111111111111111111100000000000000000000000000000000000000000000000000000000075bcd15"
        );
    }

    #[test]
    fn facade_encode_matches_frozen_v1_erc20_payment() {
        let c = Contract::load(SWAP_CONTRACT_ABI.as_bytes()).unwrap();
        let cd = c
            .function("erc20Payment")
            .unwrap()
            .encode_input(&[
                Token::FixedBytes(vec![0x33; 32]),
                Token::Uint(U256::from(123_456_789u64)),
                Token::Address(addr(0x11)),
                Token::Address(addr(0x22)),
                Token::FixedBytes(vec![0x66; 20]),
                Token::Uint(U256::from(1_700_000_000u64)),
            ])
            .unwrap();
        assert_eq!(
            hex::encode(cd),
            "9b415b2a333333333333333333333333333333333333333333333333333333333333333300000000000000000000000000000000000000000000000000000000075bcd15000000000000000000000000111111111111111111111111111111111111111100000000000000000000000022222222222222222222222222222222222222226666666666666666666666666666666666666666000000000000000000000000000000000000000000000000000000000000000000000000000000006553f100"
        );
    }

    #[test]
    fn facade_encode_matches_frozen_qtum_dynamic_bytes() {
        // Dynamic `bytes` tail + uint8 — the head/tail offset path.
        let c = Contract::load(QTUM_ABI.as_bytes()).unwrap();
        let cd = c
            .function("addDelegation")
            .unwrap()
            .encode_input(&[
                Token::Address(addr(0x11)),
                Token::Uint(U256::from(7u64)),
                Token::Bytes(vec![0x77; 40]),
            ])
            .unwrap();
        assert_eq!(
            hex::encode(cd),
            "4c0e968c000000000000000000000000111111111111111111111111111111111111111100000000000000000000000000000000000000000000000000000000000000070000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000002877777777777777777777777777777777777777777777777777777777777777777777777777777777000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn facade_decode_input_roundtrip_and_output() {
        let c = Contract::load(SWAP_CONTRACT_ABI.as_bytes()).unwrap();
        let f = c.function("receiverSpend").unwrap();
        let tokens = vec![
            Token::FixedBytes(vec![0x33; 32]),
            Token::Uint(U256::from(123_456_789u64)),
            Token::FixedBytes(vec![0x44; 32]),
            Token::Address(addr(0x11)),
            Token::Address(addr(0x22)),
        ];
        let cd = f.encode_input(&tokens).unwrap();
        assert_eq!(f.decode_input(&cd[4..]).unwrap(), tokens, "decode_input round-trip");

        // payments(bytes32) -> (bytes20 paymentHash, uint64 lockTime, uint8 state)
        let out = vec![
            Token::FixedBytes(vec![0xAB; 20]),
            Token::Uint(U256::from(1_700_000_000u64)),
            Token::Uint(U256::from(2u8)),
        ];
        let blob = encode(&out);
        assert_eq!(
            c.function("payments").unwrap().decode_output(&blob).unwrap(),
            out,
            "decode_output"
        );
    }

    #[test]
    fn facade_rejects_bad_input() {
        let c = Contract::load(SWAP_CONTRACT_ABI.as_bytes()).unwrap();
        assert!(matches!(c.function("nonexistent"), Err(AbiError::FunctionNotFound(_))));
        let f = c.function("erc20Payment").unwrap();
        assert_eq!(f.encode_input(&[]), Err(AbiError::InvalidData), "wrong arity rejected");
        assert!(f.decode_input(&[0u8; 10]).is_err(), "truncated body rejected");
        assert!(Contract::load(b"not json").is_err(), "garbage ABI rejected");
    }
}
