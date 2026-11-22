//! Sending a smart-contract call of a TRON coin, and its energy dry run (CRD ch.21 §21.6, §21.9, §21.11C).
//!
//! # Invariants
//!
//! - A coin without a local private key is refused before any node is asked.
//! - No fee policy lives here: the caller gives the `fee_limit` and reads the dry run's energy figure itself.
//! - Once a transaction is signed, every failure of its broadcast returns it ([`TronSwapError::Broadcast`]).

use super::tx::TronSwapTx;
use super::TronSwapError;
use crate::eth::tron::api::{TronApiClient, TronApiError};
use crate::eth::tron::proto::DEFAULT_EXPIRATION_SEC;
use crate::eth::tron::sign::sign_transaction_raw;
use crate::eth::tron::tx_builder::build_trigger_smart_contract;
use crate::eth::tron::TronAddress;
use crate::eth::EthCoin;
use common::now_ms;

/// The lifetime of a sent call: the default expiry of a TRON transaction, counted from the head block's time.
const LIFETIME_MS: i64 = DEFAULT_EXPIRATION_SEC as i64 * 1000;

/// The node client of a TRX or TRC20 coin.
fn tron_api(coin: &EthCoin) -> Result<TronApiClient, TronSwapError> {
    match (&coin.tron_api, coin.is_tron_family()) {
        (Some(api), true) => Ok(api.clone()),
        _ => Err(TronSwapError::NotTronCoin),
    }
}

/// Builds the call of `contract` with the call data `data` and the call value `call_value` (SUN) on the reference
/// block of the current head, signs it with the key of the coin's address and broadcasts it through the coin's
/// node pool. `fee_limit_sun` is the transaction's `fee_limit`, in SUN. The transaction expires 60 seconds after
/// the head block's time.
///
/// # Errors
///
/// Before any node is asked: [`TronSwapError::NoLocalKey`] for a coin a hardware or an external wallet signs
/// for, [`TronSwapError::NotTronCoin`], [`TronSwapError::NegativeCallValue`], and [`TronSwapError::FeeLimit`] for a
/// `fee_limit_sun` beyond the protocol's signed 64-bit field. Then [`TronSwapError::Node`] when the head block
/// cannot be read and [`TronSwapError::Signing`]. After signing, [`TronSwapError::Broadcast`] with the signed
/// transaction: no node took the request, a node refused the transaction (`result: false`), or the accepting
/// node named another transaction id.
pub async fn send_trigger(
    coin: &EthCoin,
    contract: &TronAddress,
    data: Vec<u8>,
    call_value: i64,
    fee_limit_sun: u64,
) -> Result<TronSwapTx, TronSwapError> {
    if coin.signer.local_secret().is_none() {
        return Err(TronSwapError::NoLocalKey);
    }
    let api = tron_api(coin)?;
    if call_value < 0 {
        return Err(TronSwapError::NegativeCallValue);
    }
    let fee_limit = i64::try_from(fee_limit_sun)
        .map_err(|_| TronSwapError::FeeLimit(format!("{fee_limit_sun} SUN is beyond the range of the field")))?;
    let owner = TronAddress::from_evm_address(coin.my_address);

    let block = api.get_now_block().await.map_err(TronSwapError::Node)?;
    let tapos = block.to_tapos().map_err(TronSwapError::Node)?;
    let block_ms = block.block_header.raw_data.timestamp;
    let expiration_ms = block_ms.checked_add(LIFETIME_MS).ok_or_else(|| {
        TronSwapError::Node(TronApiError::InvalidResponse(
            "the head block's time leaves no room for an expiry".to_owned(),
        ))
    })?;
    // The transaction's own timestamp is the local time of building, as on the withdraw path.
    let timestamp_ms = now_ms() as i64;

    let raw = build_trigger_smart_contract(
        &owner,
        contract,
        data,
        call_value,
        &tapos,
        timestamp_ms,
        expiration_ms,
        fee_limit,
    );
    let tx = {
        let secret = coin.signer.local_secret().ok_or(TronSwapError::NoLocalKey)?;
        let (_, signature) = sign_transaction_raw(secret, &raw).map_err(TronSwapError::Signing)?;
        TronSwapTx::new(raw, signature)
    };

    let answer = match api.broadcast_hex(&hex::encode(tx.to_bytes())).await {
        Ok(answer) => answer,
        Err(error) => {
            return Err(TronSwapError::Broadcast {
                tx: Box::new(tx),
                error,
            })
        },
    };
    // The id is known locally; an accepting node that names another one did not accept this transaction.
    if let Some(named) = answer.txid {
        let named = named.strip_prefix("0x").unwrap_or(&named);
        if hex::decode(named).ok().as_deref() != Some(tx.txid().as_bytes()) {
            return Err(TronSwapError::Broadcast {
                tx: Box::new(tx),
                error: TronApiError::InvalidResponse("the node named another transaction id".to_owned()),
            });
        }
    }
    Ok(tx)
}

/// The energy the call of `contract` with the call data `data` and the call value `call_value` (SUN) would use,
/// from the node's dry run (`triggerconstantcontract`) made as the coin's address.
///
/// # Errors
///
/// [`TronSwapError::NotTronCoin`] and [`TronSwapError::NegativeCallValue`] before any node is asked;
/// [`TronSwapError::Reverted`] with the node's reason when the dry run reverts, in which case no other node is
/// asked; [`TronSwapError::Node`] when the read fails or the answer has no positive `energy_used`. No figure
/// stands in for a missing one.
pub async fn dry_run_energy(
    coin: &EthCoin,
    contract: &TronAddress,
    data: &[u8],
    call_value: i64,
) -> Result<u64, TronSwapError> {
    let api = tron_api(coin)?;
    if call_value < 0 {
        return Err(TronSwapError::NegativeCallValue);
    }
    let owner = TronAddress::from_evm_address(coin.my_address);
    let answer = api
        .trigger_constant_dry_run_data(&owner, contract, data, call_value)
        .await
        .map_err(|e| match e {
            TronApiError::ContractRevert(result) => TronSwapError::Reverted { result },
            other => TronSwapError::Node(other),
        })?;
    match answer.energy_used {
        Some(energy) if energy > 0 => Ok(energy),
        _ => Err(TronSwapError::Node(TronApiError::InvalidResponse(
            "the dry run reports no positive energy_used".to_owned(),
        ))),
    }
}
