use crate::{NumConversError, SignatureError, SignatureResult, SolanaCoin, ValidateAddressResult, VerificationError,
            VerificationResult, WithdrawError};
use base58::{FromBase58, ToBase58};
use common::mm_number::BigDecimal;
use mm2_err_handle::prelude::*;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use std::str::FromStr;

pub fn lamports_to_sol(lamports: u64) -> BigDecimal { BigDecimal::from(lamports) / BigDecimal::from(LAMPORTS_PER_SOL) }

pub fn amount_to_ui_amount(amount: u64, decimals: u8) -> BigDecimal {
    BigDecimal::from(amount) / BigDecimal::from(10_u64.pow(decimals as u32))
}

/// The length in bytes of a Solana public key, and so of an address once base58-decoded.
const SOLANA_PUBKEY_LEN: usize = 32;

/// `validateaddress` (CRD ch.40 §40.6B.5, D40.6B.1 option (b)): valid iff the text is base58 that decodes to exactly
/// 32 bytes. The text is not required to be 44 characters or to lie on the curve: a 32-byte key encodes to 32..=44
/// characters, and a program-derived address is off the curve by construction. An invalid answer carries a reason.
pub fn validate_address(address: &str) -> ValidateAddressResult {
    match address.from_base58() {
        Ok(bytes) if bytes.len() == SOLANA_PUBKEY_LEN => ValidateAddressResult {
            is_valid: true,
            reason: None,
        },
        Ok(bytes) => ValidateAddressResult {
            is_valid: false,
            reason: Some(format!(
                "The address decodes to {} bytes, expected {}",
                bytes.len(),
                SOLANA_PUBKEY_LEN
            )),
        },
        Err(_) => ValidateAddressResult {
            is_valid: false,
            reason: Some("The address is not valid base58".to_string()),
        },
    }
}

/// One smallest unit of a coin with `decimals` decimals, 10^-decimals. Built from the exponent, so any `u8` is safe.
pub fn smallest_unit(decimals: u8) -> BigDecimal { BigDecimal::new(1.into(), i64::from(decimals)) }

/// The 64-byte keypair of the coin (32-byte secret, then the 32-byte public key) in base58, the form common Solana
/// wallets import (CRD ch.40 D40.6B.1 option (b), `show_priv_key`).
pub fn keypair_base58(coin: &SolanaCoin) -> String { coin.key_pair.to_bytes()[..].to_base58() }

pub fn sign_message(coin: &SolanaCoin, message: &str) -> SignatureResult<String> {
    let signature = coin
        .key_pair
        .try_sign_message(message.as_bytes())
        .map_err(|e| SignatureError::InternalError(e.to_string()))?;
    Ok(signature.to_string())
}

pub fn verify_message(
    coin: &SolanaCoin,
    signature: &str,
    message: &str,
    pubkey_bs58: &str,
) -> VerificationResult<bool> {
    let pubkey = pubkey_bs58.from_base58()?;
    let signature =
        Signature::from_str(signature).map_err(|e| VerificationError::SignatureDecodingError(e.to_string()))?;
    let is_valid = signature.verify(&pubkey, message.as_bytes());
    Ok(is_valid)
}

/// `amount` units of a coin with `decimals` decimals as a decimal number, 10^-decimals each, with no trailing zeros.
/// Built from the exponent, so any `u8` is safe.
pub fn units_to_amount(units: u64, decimals: u8) -> BigDecimal {
    BigDecimal::new(units.into(), i64::from(decimals)).normalized()
}

/// `amount` (in whole units of a coin with `decimals` decimals) as a count of its smallest units. Digits beyond
/// `decimals` are dropped, not rounded and not refused (CRD ch.40 R40.6B.9, R40.6B.10). A negative amount, or one
/// that is more than a `u64` of units, is an error: callers compare the amount with the balance first, so the
/// error is not reached for a valid withdraw.
pub fn amount_to_units(amount: &BigDecimal, decimals: u8) -> Result<u64, MmError<NumConversError>> {
    let (units, _) = amount.with_scale(i64::from(decimals)).as_bigint_and_exponent();
    bigdecimal::ToPrimitive::to_u64(&units)
        .or_mm_err(|| NumConversError("The amount does not fit the smallest units of the coin".to_string()))
}

/// `amount` with the digits beyond `decimals` dropped (CRD ch.40 R40.6B.9, R40.6B.10): the amount a withdraw moves.
pub fn truncate_amount(amount: &BigDecimal, decimals: u8) -> BigDecimal {
    amount.with_scale(i64::from(decimals)).normalized()
}

/// The recipient of a withdraw: any base58 text that decodes to exactly 32 bytes (CRD ch.40 R40.6B.9). Anything
/// else is `InvalidAddress` with a reason. The key is not tested for being on the curve or for existing.
pub fn parse_recipient(address: &str) -> Result<Pubkey, MmError<WithdrawError>> {
    let bytes = address.from_base58().map_err(|_| {
        MmError::new(WithdrawError::InvalidAddress(
            "The address is not valid base58".to_string(),
        ))
    })?;
    let key: [u8; SOLANA_PUBKEY_LEN] = bytes.as_slice().try_into().map_err(|_| {
        MmError::new(WithdrawError::InvalidAddress(format!(
            "The address decodes to {} bytes, expected {}",
            bytes.len(),
            SOLANA_PUBKEY_LEN
        )))
    })?;
    Ok(Pubkey::new_from_array(key))
}
