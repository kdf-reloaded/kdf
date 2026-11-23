//! The identifiers of a TRON V1 swap payment (CRD ch.21 §21.11B.2, §21.11C): the swap id the client derives, the
//! payment hash the contract stores, and the width rule of the secret hash.

use super::TronSwapError;
use crate::eth::tron::TronAddress;
use ethereum_types::{Address, U256};
use sha2::{Digest, Sha256};

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(input));
    out
}

/// The id of a swap payment: `SHA-256(little-endian uint32(lock_time) ‖ secret_hash)` over the 32-byte secret hash
/// (CRD ch.21 §21.11B.2, D21.B4). The same derivation as the EVM family's, on the value the swap negotiated.
pub fn tron_swap_id(lock_time: u32, secret_hash: &[u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 36];
    input[..4].copy_from_slice(&lock_time.to_le_bytes());
    input[4..].copy_from_slice(secret_hash);
    sha256(&input)
}

/// The payment hash the contract stores for a payment (CRD ch.21 §21.11B.2): SHA-256 over the packed
/// `(receiver, sender, secretHash, tokenAddress, amount)`. The addresses are their 20-byte payloads, `token` is
/// `None` for native TRX (the zero address), and the amount is a 32-byte big-endian word.
pub fn tron_payment_hash(
    receiver: &TronAddress,
    sender: &TronAddress,
    secret_hash: &[u8; 32],
    token: Option<&TronAddress>,
    amount: U256,
) -> [u8; 32] {
    // The zero address is the contract's own notation for TRX, not a stand-in for an unknown token.
    let token = token.map_or_else(Address::zero, TronAddress::to_evm_address);
    let mut packed = [0u8; 20 + 20 + 32 + 20 + 32];
    packed[..20].copy_from_slice(receiver.to_evm_address().as_bytes());
    packed[20..40].copy_from_slice(sender.to_evm_address().as_bytes());
    packed[40..72].copy_from_slice(secret_hash);
    packed[72..92].copy_from_slice(token.as_bytes());
    amount.to_big_endian(&mut packed[92..]);
    sha256(&packed)
}

/// The secret hash of a TRON leg as the contract takes it: the 32 bytes given, unchanged (CRD ch.21 R21.B6,
/// ch.51 R72A). A 32-byte value is never narrowed.
///
/// # Errors
///
/// [`TronSwapError::HashWidth`] for any other width, the 20 bytes of the default secret hash included.
pub fn require_32_byte_hash(secret_hash: &[u8]) -> Result<[u8; 32], TronSwapError> {
    <[u8; 32]>::try_from(secret_hash).map_err(|_| TronSwapError::HashWidth { got: secret_hash.len() })
}
