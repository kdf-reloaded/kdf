//! TRON blockchain support for KDF-RELOADED.
//!
//! TRON is an EVM-compatible blockchain with its own address format (Base58Check
//! with 0x41 prefix), protobuf-based transaction serialization, and a dual
//! bandwidth+energy fee model. TRC20 tokens reuse ERC20 ABI encoding.

pub mod activation;
pub mod address;
pub mod api;
pub mod fee;
pub mod gasfree;
pub mod proto;
pub mod sign;
pub mod tx_builder;
pub mod withdraw;

pub use activation::tron_coin_from_conf_and_request;
pub use address::TronAddress;

use serde::{Deserialize, Serialize};

/// Derive a Base58Check-encoded TRON address string from a hex-encoded
/// secp256k1 public key. Mirrors `eth::addr_from_pubkey_str` so callers
/// that only know the protocol family can produce a display-ready address.
pub fn addr_from_pubkey_str(pubkey: &str) -> Result<String, String> {
    let evm = crate::eth::addr_from_pubkey_str(pubkey)?;
    let evm = evm.strip_prefix("0x").unwrap_or(&evm);
    let bytes = hex::decode(evm).map_err(|e| e.to_string())?;
    if bytes.len() != 20 {
        return Err(format!("expected 20-byte EVM address, got {}", bytes.len()));
    }
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&bytes);
    Ok(TronAddress::from_evm_address(arr.into()).to_base58())
}

/// TRX uses 6 decimal places (1 TRX = 1,000,000 SUN).
pub const TRX_DECIMALS: u8 = 6;

/// HTTP request timeout for TRON API calls.
pub const TRON_API_TIMEOUT_SEC: u64 = 10;

/// TRON network variants.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum Network {
    /// TRON Mainnet
    Mainnet,
    /// TRON Shasta testnet
    Shasta,
    /// TRON Nile testnet
    Nile,
}

impl Default for Network {
    fn default() -> Self { Network::Mainnet }
}
