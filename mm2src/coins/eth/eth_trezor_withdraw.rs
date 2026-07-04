//! EVM Trezor hardware-wallet withdrawal: the device-driven signing half of the
//! `task::withdraw::init` path (CRD §50). This module is native, non-iOS only —
//! the Trezor signing policy exists only there. It reuses the shared unsigned-tx
//! builder ([`build_evm_withdraw_plan`]) and the shared completed-payload builder
//! ([`build_evm_withdraw_details`]) so a Trezor-signed withdrawal is byte-shape
//! identical to a software-signed one (R50.22 / R50.23), and drives the public
//! Trezor Ethereum signing exchange through the withdrawal task's status /
//! user-action vocabulary (R50.13 / R50.14).

use super::*;
use crate::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletOps};
use crate::rpc_command::init_withdraw::{WithdrawAwaitingStatus, WithdrawInProgressStatus, WithdrawTask,
                                        WithdrawTaskHandle};
use crate::{CoinWithDerivationMethod, DerivationMethod, WithdrawError, WithdrawRequest, WithdrawResult};
use crypto::hw_rpc_task::{HwConnectStatuses, TrezorRpcTaskConnectProcessor};
use crypto::trezor::client::TrezorClient;
use crypto::trezor::ethereum::TrezorEthTxInput;
use crypto::{Bip44DerivationPath, CryptoCtx, DerivationPath};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use std::str::FromStr;
use std::time::Duration;

/// Device connect / interaction time budget shared with the UTXO Trezor path (R50.16).
const TREZOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
const TREZOR_PIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Big-endian, minimally-trimmed byte encoding expected by the public Trezor
/// Ethereum field encoding (R50.6): an empty vector for zero, otherwise the
/// big-endian bytes with leading zero bytes dropped.
fn u256_to_trezor_be(v: U256) -> Vec<u8> {
    let mut buf = [0u8; 32];
    v.to_big_endian(&mut buf);
    let first = buf.iter().position(|&b| b != 0).unwrap_or(buf.len());
    buf[first..].to_vec()
}

/// Resolve the withdrawal sender address and the signing derivation path for a
/// Trezor EVM withdrawal, **without** deriving any local secret (R50.1). With
/// `from` omitted the enabled address / path is used (R50.4 / R49.7); with `from`
/// present the HD selector is validated and resolved per R49.2 (R50.8), failing
/// with the ch. 49 sender-selector discriminants (R50.19). This runs before any
/// device exchange, so a bad selector never triggers a device prompt.
async fn resolve_evm_trezor_withdraw_sender(
    coin: &EthCoin,
    req: &WithdrawRequest,
    signer: &EthTrezorSigner,
) -> MmResult<(Address, DerivationPath), WithdrawError> {
    let from = match req.from.clone() {
        Some(from) => from,
        None => return Ok((signer.address, signer.derivation_path.clone())),
    };

    let hd_wallet = match coin.derivation_method() {
        DerivationMethod::HDWallet(hd_wallet) => hd_wallet,
        DerivationMethod::Iguana(_) => {
            let error = "'from' is not supported if the EVM coin is initialized with a single private key";
            return MmError::err(WithdrawError::UnexpectedFromAddress(error.to_owned()));
        },
    };

    let crate::HDAddressId {
        account_id,
        chain,
        address_id,
    } = match from {
        crate::WithdrawFrom::AddressId(id) => id,
        crate::WithdrawFrom::DerivationPath { derivation_path } => {
            let derivation_path = Bip44DerivationPath::from_str(&derivation_path)
                .map_to_mm(|e| WithdrawError::UnexpectedFromAddress(format!("{:?}", e)))?;
            let coin_type = derivation_path.coin_type();
            let expected_coin_type = hd_wallet.coin_type();
            if coin_type != expected_coin_type {
                let error = format!(
                    "Derivation path '{}' must have '{}' coin type",
                    derivation_path, expected_coin_type
                );
                return MmError::err(WithdrawError::UnexpectedFromAddress(error));
            }
            crate::HDAddressId::from(derivation_path)
        },
    };

    let hd_account = hd_wallet
        .get_account(account_id)
        .await
        .or_mm_err(|| WithdrawError::UnknownAccount { account_id })?;
    let is_address_activated = hd_account
        .is_address_activated(chain, address_id)
        .mm_err(|e| WithdrawError::UnexpectedFromAddress(e.to_string()))?;
    let hd_address = coin
        .derive_address(&hd_account, chain, address_id)
        .mm_err(|e| WithdrawError::UnexpectedFromAddress(e.to_string()))?;
    if !is_address_activated {
        let error = format!("'{}' address is not activated", hd_address.address);
        return MmError::err(WithdrawError::UnexpectedFromAddress(error));
    }

    Ok((hd_address.address, hd_address.derivation_path))
}

/// Connect to the Trezor device, surfacing device connect and PIN / passphrase
/// requests through the withdrawal task's status / awaiting-status vocabulary
/// (R50.13 / R50.14), bounded by the shared connect / PIN timeouts (R50.16).
///
/// Returns both the connected client and the request processor so the *same*
/// processor drives the subsequent signing exchange — a signing session is a
/// fresh device session, so a PIN / passphrase-protected device re-requests its
/// secret at signing time and it must be surfaced the same way.
fn trezor_connect_processor(task_handle: &WithdrawTaskHandle) -> TrezorRpcTaskConnectProcessor<'_, WithdrawTask> {
    TrezorRpcTaskConnectProcessor::new(task_handle, HwConnectStatuses {
        on_connect: WithdrawInProgressStatus::WaitingForTrezorToConnect,
        on_connected: WithdrawInProgressStatus::Preparing,
        on_connection_failed: WithdrawInProgressStatus::Finishing,
        on_button_request: WithdrawInProgressStatus::WaitingForUserToConfirmSigning,
        on_pin_request: WithdrawAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: WithdrawAwaitingStatus::EnterTrezorPassphrase,
        on_ready: WithdrawInProgressStatus::Preparing,
    })
    .with_connect_timeout(TREZOR_CONNECT_TIMEOUT)
    .with_pin_timeout(TREZOR_PIN_TIMEOUT)
}

async fn trezor_client(
    ctx: &MmArc,
    processor: &TrezorRpcTaskConnectProcessor<'_, WithdrawTask>,
) -> MmResult<TrezorClient, WithdrawError> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| WithdrawError::InternalError(e.to_string()))?;
    let hw_ctx = crypto_ctx
        .hw_ctx()
        .or_mm_err(|| WithdrawError::NoTrezorDeviceAvailable)?;

    hw_ctx.trezor(processor).await.mm_err(WithdrawError::from)
}

/// R50.20: TRON-family coins/tokens are unsupported under the Trezor signing
/// policy. Rejected with a structured error BEFORE any device exchange — no
/// connect, no PIN/passphrase prompt, no signing exchange. Extracted so the
/// rejection can be asserted without a device or task handle.
pub(crate) fn ensure_trezor_withdraw_supported(coin: &EthCoin) -> MmResult<(), WithdrawError> {
    if matches!(coin.coin_type, EthCoinType::Tron | EthCoinType::Trc20 { .. }) {
        return MmError::err(WithdrawError::UnsupportedUnderTrezor(
            "TRON withdraw is not supported under the Trezor signing policy".to_owned(),
        ));
    }
    Ok(())
}

/// The EVM Trezor withdrawal task path (R49.6). Builds the unsigned legacy
/// EIP-155 transaction with the shared builder, has the device sign it over the
/// public Trezor Ethereum protocol, and assembles the standard transaction
/// details payload.
pub(crate) async fn withdraw_trezor_impl(
    ctx: MmArc,
    coin: EthCoin,
    req: WithdrawRequest,
    task_handle: &WithdrawTaskHandle,
) -> WithdrawResult {
    // R50.20: reject TRON before any device exchange.
    ensure_trezor_withdraw_supported(&coin)?;

    validate_evm_withdraw_request(&coin, &req)?;

    let signer = match &coin.signer {
        EthSigner::Trezor(signer) => signer.clone(),
        _ => {
            return MmError::err(WithdrawError::InternalError(
                "withdraw_trezor_impl invoked for a non-Trezor signer".to_owned(),
            ))
        },
    };

    // R50.4 / R50.8 / R50.19: resolve the sender address + signing path (and
    // validate any explicit `from` selector) BEFORE any device exchange.
    let (sender_address, signing_path) = resolve_evm_trezor_withdraw_sender(&coin, &req, &signer).await?;

    task_handle
        .update_in_progress_status(WithdrawInProgressStatus::GeneratingTransaction)
        .mm_err(WithdrawError::from)?;

    // Shared unsigned-tx construction (balance check, amount/max, fee/gas, nonce,
    // unsigned EIP-155 tx). The nonce lock is held through signing.
    let (plan, _nonce_lock) = build_evm_withdraw_plan(&ctx, &coin, &req, sender_address).await?;

    // R50.7: the EIP-155 chain id must be present for an EVM signing request.
    let chain_id = coin
        .chain_id
        .or_mm_err(|| WithdrawError::InternalError("EVM Trezor withdraw requires a chain_id".to_owned()))?;

    // R50.6: assemble the device signing input from the shared unsigned tx.
    let input = TrezorEthTxInput {
        address_n: signing_path.iter().map(|index| index.0).collect(),
        nonce: u256_to_trezor_be(plan.unsigned.nonce),
        gas_price: u256_to_trezor_be(plan.unsigned.gas_price),
        gas_limit: u256_to_trezor_be(plan.unsigned.gas),
        to: format!("{:#x}", plan.call_addr),
        value: u256_to_trezor_be(plan.unsigned.value),
        data: plan.unsigned.data.clone(),
        chain_id,
    };

    // R50.13: connect to the device (connect / PIN / passphrase surfaced as task
    // statuses / user actions). The processor is reused for the signing exchange
    // so a PIN / passphrase re-requested in the fresh signing session is surfaced
    // the same way.
    let processor = trezor_connect_processor(task_handle);
    let trezor_client = trezor_client(&ctx, &processor).await?;

    task_handle
        .update_in_progress_status(WithdrawInProgressStatus::WaitingForUserToConfirmSigning)
        .mm_err(WithdrawError::from)?;

    // R50.10 / R50.11: drive the public Trezor Ethereum signing exchange
    // (streaming the payload in chunks if needed) and read `(v, r, s)`.
    let signature = {
        let mut session = trezor_client.session().await.mm_err(WithdrawError::from)?;
        session
            .sign_eth_tx_with_processor(input, &processor)
            .await
            .mm_err(WithdrawError::from)?
    };

    task_handle
        .update_in_progress_status(WithdrawInProgressStatus::Finishing)
        .mm_err(WithdrawError::from)?;

    // R50.7 / R50.12 / R50.18: normalize the recovery value, re-apply EIP-155
    // replay protection, and assemble the byte-identical signed transaction. An
    // invalid recovery value / unrecoverable signature fails as a device-internal
    // error rather than emitting a malformed transaction.
    let signed = super::legacy_tx::signed_eth_tx_from_rsv(
        plan.unsigned.clone(),
        signature.v as u32,
        &signature.r,
        &signature.s,
        Some(chain_id),
    )
    .map_to_mm(|e| WithdrawError::HardwareWalletInternal(format!("Invalid device signature: {}", e)))?;

    let bytes = rlp::encode(&signed);
    let from_checksum = checksum_address(&format!("{:#02x}", sender_address));
    build_evm_withdraw_details(
        &coin,
        &plan,
        from_checksum,
        sender_address,
        bytes.into(),
        format!("{:02x}", signed.tx_hash()),
    )
}
