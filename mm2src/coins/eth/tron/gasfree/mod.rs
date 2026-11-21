//! TRON GasFree fee delegation: the building blocks of the gasless TRC20 withdraw rail (CRD ch.63).
//!
//! The group holds, per CRD ch.63:
//!
//! - [`networks`]: the per-network constants and the proxy creation bytecode (§63.4.1, §63.5, §63.6);
//! - [`custody`]: the local derivation of a user's custody address (§63.5), its check against the controller
//!   contract (R29) and the on-chain balance read of the custody address (§63.7);
//! - [`authorization`]: the `PermitTransfer` typed data, its signer and the signed artifact (§63.6, §63.8 step 5);
//! - [`provider`] and [`provider_payloads`]: the provider's REST client and its typed payloads (§63.4);
//! - [`preflight`]: the preflight decision (§63.7);
//! - [`binding`]: the provider binding of a platform coin and the `gasless` block of a token coin as an activation
//!   gives them (§63.3.1, §63.3.2), and the custody address a report shows (§63.3.3, R18);
//! - [`failure`]: the error type of the group and its collapse onto the withdraw RPC's arms (§63.9);
//! - [`withdraw`]: the gasless rail of a TRC20 withdraw, sign-only (§63.8 steps 2 to 5, §63.8A).
//!
//! # Invariants
//!
//! - The controller, the beacon and the creation bytecode of a network are fixed here; no caller input changes
//!   them (R2).
//! - The provider credentials and the raw bytes of a signature never reach a log line, an error text or a `Debug`
//!   rendering (R9).

// The withdraw rail is sign-only: the submit and trace payloads and the supported-token call have no caller until
// the submission flow exists (D63.6).
#![allow(dead_code)]

pub(crate) mod authorization;
pub(crate) mod binding;
pub(crate) mod custody;
pub(crate) mod failure;
pub(crate) mod networks;
pub(crate) mod preflight;
pub(crate) mod provider;
pub(crate) mod provider_payloads;
pub(crate) mod withdraw;

#[cfg(all(test, not(target_arch = "wasm32")))] mod live_tests;
#[cfg(test)] mod test_vectors;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod withdraw_tests;

/// Keccak-256 of `data`, as plain bytes.
fn keccak(data: &[u8]) -> [u8; 32] { crate::eth::keccak256(data).take() }

/// `address` as one 32-byte ABI word: the 20-byte payload, left-padded with zeros.
fn address_word(address: &super::TronAddress) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address.to_evm_address().as_ref());
    word
}
