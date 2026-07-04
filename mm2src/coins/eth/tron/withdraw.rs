//! TRON withdraw pipeline.
//!
//! The `withdraw` RPC produces a signed serialized transaction that the caller
//! can later submit via `send_raw_transaction`. For TRON the flow is:
//!
//! 1. Parse the destination (Base58Check or hex) to a [`TronAddress`].
//! 2. Query `/wallet/getnowblock` for the TAPOS reference data required by
//!    every TRON transaction.
//! 3. Build the unsigned [`proto::TransactionRaw`] with [`tx_builder`].
//! 4. SHA-256 hash the canonical protobuf serialization and sign with
//!    secp256k1 to produce a 65-byte R||S||V signature.
//! 5. Wrap raw + signature into a [`proto::Transaction`] and emit the
//!    hex-encoded protobuf as `tx_hex`. The txid is the hash.
//! 6. Build a [`TronTxFeeDetails`] breakdown for the response. Resource
//!    usage comes from `/wallet/getaccountresource`; energy estimates for
//!    TRC20 come from `triggerconstantcontract`, with a conservative
//!    fallback if the node does not return one.

use super::api::{TronApiClient, TronApiError};
use super::fee::{estimate_trc20_transfer_fee, estimate_trx_transfer_fee, TronAccountResources, TronChainPrices,
                 TronTxFeeDetails};
use super::proto;
use super::sign::sign_transaction_raw;
use super::tx_builder::{abi_encode_trc20_transfer, build_trc20_transfer, build_trx_transfer};
use super::{TronAddress, TRX_DECIMALS};

use crate::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType};
use crate::{BytesJson, TransactionDetails, WithdrawError, WithdrawRequest, WithdrawResult};

use bigdecimal::BigDecimal;
use common::now_ms;
use ethereum_types::U256;
use mm2_err_handle::prelude::*;
use prost::Message as _;

/// TRC20 transfer energy estimate used when the node does not return one.
///
/// USDT-style TRC20 contracts consume roughly 28k–32k energy for a plain
/// balance-to-balance transfer; we pad slightly so the user does not see
/// a failed broadcast because of an optimistic cap.
const TRC20_TRANSFER_ENERGY_DEFAULT: i64 = 35_000;

/// Fee limit (in SUN) used when building TRC20 transactions.
///
/// 100 TRX is the conventional wallet cap and is more than enough for a
/// plain TRC20 `transfer`.
const TRC20_DEFAULT_FEE_LIMIT_SUN: i64 = 100_000_000;

/// Entry point for TRX / TRC20 withdrawals.
pub async fn withdraw_tron(coin: EthCoin, req: WithdrawRequest) -> WithdrawResult {
    let api = coin
        .tron_api
        .clone()
        .ok_or_else(|| WithdrawError::InternalError("TRON API client missing on this coin".to_owned()))?;

    let sender = TronAddress::from_evm_address(coin.my_address);
    let receiver = parse_tron_address(&req.to)?;

    if receiver.to_evm_address().is_zero() {
        return MmError::err(WithdrawError::InvalidAddress(
            "Receiver cannot be zero address".to_owned(),
        ));
    }

    match &coin.coin_type {
        EthCoinType::Tron => withdraw_native_trx(coin.clone(), api, sender, receiver, req).await,
        EthCoinType::Trc20 { token_addr, .. } => {
            let contract = TronAddress::from_evm_address(*token_addr);
            withdraw_trc20_token(coin.clone(), api, sender, receiver, contract, req).await
        },
        _ => MmError::err(WithdrawError::InternalError(
            "withdraw_tron invoked on non-TRON coin".to_owned(),
        )),
    }
}

async fn withdraw_native_trx(
    coin: EthCoin,
    api: TronApiClient,
    sender: TronAddress,
    receiver: TronAddress,
    req: WithdrawRequest,
) -> WithdrawResult {
    let (balance_sun, balance_dec) = fetch_trx_balance(&api, &sender).await?;

    let (amount_sun_u256, amount_dec) = if req.max {
        (balance_sun, balance_dec.clone())
    } else {
        let amount = wei_from_big_decimal(&req.amount, TRX_DECIMALS).mm_err(Into::into)?;
        (amount, req.amount.clone())
    };

    if amount_sun_u256 > balance_sun {
        return MmError::err(WithdrawError::NotSufficientBalance {
            coin: coin.ticker.clone(),
            available: balance_dec,
            required: amount_dec,
        });
    }

    let amount_sun = u256_to_i64(amount_sun_u256)?;
    let tapos = api
        .get_now_block()
        .await
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?
        .to_tapos()
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?;
    let now_ms_i = now_ms() as i64;
    let raw = build_trx_transfer(&sender, &receiver, amount_sun, &tapos, now_ms_i);

    let secret = coin
        .signer
        .local_secret()
        .ok_or_else(|| WithdrawError::InternalError("TRON requires a local private key".to_string()))?;
    let (hash, sig) = sign_transaction_raw(secret, &raw)
        .map_err(|e| WithdrawError::InternalError(format!("TRON signing failed: {e}")))?;
    let tx = proto::Transaction {
        raw_data: Some(raw),
        signature: vec![sig],
    };
    let tx_bytes = tx.encode_to_vec();

    let fee_details = build_fee_details_trx(&api, &sender, &receiver).await;

    let my_address_str = sender.to_base58();
    let receiver_str = receiver.to_base58();
    let received_by_me: BigDecimal = if receiver_str == my_address_str {
        amount_dec.clone()
    } else {
        0.into()
    };
    let spent_by_me = amount_dec.clone();

    Ok(TransactionDetails {
        tx_hex: BytesJson::from(tx_bytes),
        tx_hash: format!("{:x}", hash),
        from: vec![my_address_str],
        to: vec![receiver_str],
        total_amount: amount_dec,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: fee_details.map(Into::into),
        coin: coin.ticker.clone(),
        internal_id: vec![].into(),
        kmd_rewards: None,
        transaction_type: Default::default(),
    })
}

async fn withdraw_trc20_token(
    coin: EthCoin,
    api: TronApiClient,
    sender: TronAddress,
    receiver: TronAddress,
    contract: TronAddress,
    req: WithdrawRequest,
) -> WithdrawResult {
    let decimals = coin.decimals;

    let (balance_units, balance_dec) = fetch_trc20_balance(&api, &contract, &sender, decimals).await?;

    let (amount_units_u256, amount_dec) = if req.max {
        (balance_units, balance_dec.clone())
    } else {
        let amount = wei_from_big_decimal(&req.amount, decimals).mm_err(Into::into)?;
        (amount, req.amount.clone())
    };

    if amount_units_u256 > balance_units {
        return MmError::err(WithdrawError::NotSufficientBalance {
            coin: coin.ticker.clone(),
            available: balance_dec,
            required: amount_dec,
        });
    }

    let amount_units = u256_to_u64(amount_units_u256)?;
    let tapos = api
        .get_now_block()
        .await
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?
        .to_tapos()
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?;
    let now_ms_i = now_ms() as i64;
    let raw = build_trc20_transfer(
        &sender,
        &contract,
        &receiver,
        amount_units,
        &tapos,
        now_ms_i,
        TRC20_DEFAULT_FEE_LIMIT_SUN,
    );

    let secret = coin
        .signer
        .local_secret()
        .ok_or_else(|| WithdrawError::InternalError("TRON requires a local private key".to_string()))?;
    let (hash, sig) = sign_transaction_raw(secret, &raw)
        .map_err(|e| WithdrawError::InternalError(format!("TRON signing failed: {e}")))?;
    let tx = proto::Transaction {
        raw_data: Some(raw),
        signature: vec![sig],
    };
    let tx_bytes = tx.encode_to_vec();

    let fee_details = build_fee_details_trc20(&api, &sender, &contract, &receiver, amount_units).await;

    let my_address_str = sender.to_base58();
    let receiver_str = receiver.to_base58();
    let received_by_me: BigDecimal = if receiver_str == my_address_str {
        amount_dec.clone()
    } else {
        0.into()
    };
    let spent_by_me = amount_dec.clone();

    Ok(TransactionDetails {
        tx_hex: BytesJson::from(tx_bytes),
        tx_hash: format!("{:x}", hash),
        from: vec![my_address_str],
        to: vec![receiver_str],
        total_amount: amount_dec,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: fee_details.map(Into::into),
        coin: coin.ticker.clone(),
        internal_id: vec![].into(),
        kmd_rewards: None,
        transaction_type: Default::default(),
    })
}

fn parse_tron_address(s: &str) -> MmResult<TronAddress, WithdrawError> {
    if s.starts_with("0x") || s.starts_with("41") {
        TronAddress::from_hex(s).map_to_mm(|e| WithdrawError::InvalidAddress(e.to_string()))
    } else {
        TronAddress::from_base58(s).map_to_mm(|e| WithdrawError::InvalidAddress(e.to_string()))
    }
}

async fn fetch_trx_balance(api: &TronApiClient, address: &TronAddress) -> MmResult<(U256, BigDecimal), WithdrawError> {
    let account = api
        .get_account(address)
        .await
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?;
    let sun = account.map(|a| a.balance).unwrap_or(0);
    if sun < 0 {
        return MmError::err(WithdrawError::InternalError(
            "TRON account reported negative balance".to_owned(),
        ));
    }
    let u256 = U256::from(sun as u64);
    let dec = u256_to_big_decimal(u256, TRX_DECIMALS).mm_err(Into::into)?;
    Ok((u256, dec))
}

async fn fetch_trc20_balance(
    api: &TronApiClient,
    contract: &TronAddress,
    owner: &TronAddress,
    decimals: u8,
) -> MmResult<(U256, BigDecimal), WithdrawError> {
    // ABI-encode balanceOf(address) parameter: 32-byte slot with the address
    // left-padded to 12 zero bytes.
    let mut parameter = [0u8; 32];
    parameter[12..].copy_from_slice(owner.to_evm_address().as_ref());
    let param_hex = hex::encode(parameter);

    let resp = api
        .trigger_constant_contract(owner, contract, "balanceOf(address)", &param_hex)
        .await
        .map_to_mm(|e| WithdrawError::Transport(e.to_string()))?;

    let raw = resp
        .constant_result
        .and_then(|mut v: Vec<String>| v.pop())
        .ok_or_else(|| WithdrawError::Transport("balanceOf returned no result".to_owned()))?;
    let bytes = hex::decode(raw).map_to_mm(|e| WithdrawError::Transport(format!("bad hex: {e}")))?;
    if bytes.len() < 32 {
        return MmError::err(WithdrawError::Transport(
            "balanceOf result shorter than 32 bytes".to_owned(),
        ));
    }
    let u256 = U256::from_big_endian(&bytes[..32]);
    let dec = u256_to_big_decimal(u256, decimals).mm_err(Into::into)?;
    Ok((u256, dec))
}

async fn build_fee_details_trx(
    api: &TronApiClient,
    sender: &TronAddress,
    receiver: &TronAddress,
) -> Option<TronTxFeeDetails> {
    let prices = fetch_chain_prices(api).await.ok()?;
    let resources = fetch_account_resources(api, sender).await;
    let recipient_exists = api.get_account(receiver).await.ok().flatten().is_some();
    Some(estimate_trx_transfer_fee(&prices, &resources, recipient_exists))
}

async fn build_fee_details_trc20(
    api: &TronApiClient,
    sender: &TronAddress,
    contract: &TronAddress,
    receiver: &TronAddress,
    amount_units: u64,
) -> Option<TronTxFeeDetails> {
    let prices = fetch_chain_prices(api).await.ok()?;
    let resources = fetch_account_resources(api, sender).await;
    let recipient_exists = api.get_account(receiver).await.ok().flatten().is_some();

    let energy = estimate_trc20_energy(api, sender, contract, receiver, amount_units)
        .await
        .unwrap_or(TRC20_TRANSFER_ENERGY_DEFAULT);

    Some(estimate_trc20_transfer_fee(
        &prices,
        &resources,
        energy,
        recipient_exists,
    ))
}

async fn estimate_trc20_energy(
    api: &TronApiClient,
    sender: &TronAddress,
    contract: &TronAddress,
    receiver: &TronAddress,
    amount_units: u64,
) -> Option<i64> {
    let data = abi_encode_trc20_transfer(receiver, amount_units);
    // Strip the 4-byte selector: triggerconstantcontract wants parameters only.
    let param_hex = hex::encode(&data[4..]);
    let resp = api
        .trigger_constant_contract(sender, contract, "transfer(address,uint256)", &param_hex)
        .await
        .ok()?;
    resp.energy_used.and_then(|e| i64::try_from(e).ok())
}

async fn fetch_chain_prices(api: &TronApiClient) -> Result<TronChainPrices, TronApiError> {
    let params = api.get_chain_parameters().await?;
    Ok(TronChainPrices::from_chain_parameters(&params))
}

async fn fetch_account_resources(api: &TronApiClient, address: &TronAddress) -> TronAccountResources {
    // If the node or the account do not return usable resource data we fall
    // back to an empty quota, which makes the fee estimate conservative
    // (i.e. the preview shows the highest fee the user could pay).
    let value = match api.get_account_resource(address).await {
        Ok(v) => v,
        Err(_) => return TronAccountResources::default(),
    };
    serde_json::from_value(value).unwrap_or_default()
}

fn u256_to_i64(v: U256) -> MmResult<i64, WithdrawError> {
    if v > U256::from(i64::MAX as u64) {
        return MmError::err(WithdrawError::InternalError(
            "Amount exceeds TRON i64 protocol limit".to_owned(),
        ));
    }
    Ok(v.as_u64() as i64)
}

fn u256_to_u64(v: U256) -> MmResult<u64, WithdrawError> {
    if v > U256::from(u64::MAX) {
        return MmError::err(WithdrawError::InternalError(
            "Amount exceeds TRC20 u64 representation limit".to_owned(),
        ));
    }
    Ok(v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_base58() {
        let s = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";
        assert!(parse_tron_address(s).is_ok());
    }

    #[test]
    fn parse_accepts_0x41_hex() {
        let raw = format!("0x41{}", "0".repeat(40));
        assert!(parse_tron_address(&raw).is_ok());
    }

    #[test]
    fn parse_rejects_short_hex() {
        let bad = "0x4128ba"; // way too short
        assert!(parse_tron_address(bad).is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_tron_address("not-an-address").is_err());
    }

    #[test]
    fn u256_to_i64_rejects_overflow() {
        let huge = U256::from(i64::MAX as u64) + 1;
        assert!(u256_to_i64(huge).is_err());
    }
}
