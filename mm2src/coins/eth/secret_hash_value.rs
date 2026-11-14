//! The value an EVM legacy swap contract call carries for the shared swap secret hash (CRD ch.51 R72A).
//!
//! The legacy swap contract stores a `bytes20` hash and, at spend time, checks the revealed secret with
//! `ripemd160(sha256(secret))`. The swap negotiates one `secret_hash` for both legs (ch.51 R71/R72): 20 bytes
//! by default, 32 bytes (`sha256(secret)`) when a coin with a native 32-byte hash lock is on either side. The
//! EVM coin layer therefore derives the contract value from the shared one (R72A, R72B).

use super::Token;
use kdf_crypto::ripemd160;

/// The `bytes20` value the legacy swap contract call embeds and compares for the shared `secret_hash`
/// (ch.51 R72A, second case).
///
/// - 20 bytes: returned unchanged. The 20-byte path is byte-for-byte what the EVM layer always sent.
/// - 32 bytes (`sha256(secret)`): `ripemd160` of those 32 bytes, applied once. This equals the
///   `ripemd160(sha256(secret))` the contract computes from the real secret at spend time.
/// - any other width: an error, so a payment is never built or accepted with a wrongly sized value.
///
/// The swap id (`etomic_swap_id`) is not derived through this function: both sides compute it over the shared
/// value as negotiated.
pub(crate) fn contract_secret_hash(secret_hash: &[u8]) -> Result<[u8; 20], String> {
    let mut out = [0u8; 20];
    match secret_hash.len() {
        20 => out.copy_from_slice(secret_hash),
        32 => out.copy_from_slice(ripemd160(secret_hash).as_slice()),
        other => {
            return Err(format!(
                "secret_hash must be 20 or 32 bytes (ch.51 R65/R71/R72), got {}",
                other
            ))
        },
    }
    Ok(out)
}

/// Checks the `secret_hash` argument decoded from a payment transaction against the value derived from the
/// shared `secret_hash` (ch.51 R72A). The error names the derived value as the expected one.
pub(crate) fn check_payment_secret_hash_arg(decoded_arg: &Token, secret_hash: &[u8]) -> Result<(), String> {
    let expected = Token::FixedBytes(contract_secret_hash(secret_hash)?.to_vec());
    if *decoded_arg != expected {
        return Err(format!(
            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
            decoded_arg, expected
        ));
    }
    Ok(())
}
