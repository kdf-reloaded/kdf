use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinTaskHandle};
use crate::utxo_activation::init_utxo_standard_activation_error::InitUtxoStandardError;
use crate::utxo_activation::init_utxo_standard_statuses::{UtxoStandardAwaitingStatus, UtxoStandardInProgressStatus,
                                                          UtxoStandardUserAction};
use crate::utxo_activation::utxo_standard_activation_result::{UtxoStandardActivationResult, UtxoWalletBalance};
use coins::coin_balance::EnableCoinBalanceOps;
use coins::hd_pubkey::RpcTaskXPubExtractor;
use coins::hd_wallet::HDAddressId;
use coins::utxo::UtxoActivationParams;
use coins::{MarketCoinOps, PrivKeyActivationPolicy, PrivKeyBuildPolicy};
use crypto::hw_rpc_task::HwConnectStatuses;
use crypto::{CryptoCtx, HwRpcError};
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

/// Rejects Electrum connection settings that cannot be used (CRD ch. 55 R55.5.14): no server,
/// `min_connected` or `max_connected` below 1, or `min_connected` above `max_connected`.
pub fn validate_electrum_connection_settings(
    activation_params: &UtxoActivationParams,
) -> MmResult<(), InitUtxoStandardError> {
    match activation_params.mode.electrum_connection_settings_error() {
        Some(reason) => MmError::err(InitUtxoStandardError::Internal(format!(
            "Unusable Electrum connection settings: {}",
            reason
        ))),
        None => Ok(()),
    }
}

/// Whether the extended public keys of the activated coin come from the hardware device: only a coin of the Trezor
/// policy reads them from there, and it needs the hardware context it was built for. Every other coin derives them
/// in software, whether or not a hardware context exists (CRD ch.05 R35c, R35a).
///
/// # Errors
///
/// A coin of the Trezor policy whose hardware context is gone: the reader of its extended keys is missing.
pub(crate) fn reads_keys_from_device(
    priv_key_policy: &PrivKeyActivationPolicy,
    hw_context_present: bool,
) -> MmResult<bool, InitUtxoStandardError> {
    match priv_key_policy {
        PrivKeyActivationPolicy::Trezor if hw_context_present => Ok(true),
        PrivKeyActivationPolicy::Trezor => MmError::err(InitUtxoStandardError::HwError(HwRpcError::NotInitialized)),
        PrivKeyActivationPolicy::ContextPrivKey | PrivKeyActivationPolicy::IguanaPrivKey => Ok(false),
    }
}

pub async fn get_activation_result<Coin>(
    ctx: &MmArc,
    coin: &Coin,
    task_handle: &InitStandaloneCoinTaskHandle<Coin>,
    activation_params: &UtxoActivationParams,
) -> MmResult<UtxoStandardActivationResult, InitUtxoStandardError>
where
    Coin: InitStandaloneCoinActivationOps<
            ActivationError = InitUtxoStandardError,
            InProgressStatus = UtxoStandardInProgressStatus,
            AwaitingStatus = UtxoStandardAwaitingStatus,
            UserAction = UtxoStandardUserAction,
        > + EnableCoinBalanceOps
        + MarketCoinOps,
{
    let ticker = coin.ticker().to_owned();
    let current_block = coin
        .current_block()
        .compat()
        .await
        .map_to_mm(InitUtxoStandardError::from_block_height_err)?;

    // Construct an Xpub extractor without checking if the MarketMaker supports HD wallet ops.
    // [`EnableCoinBalanceOps::enable_coin_balance`] won't just use `xpub_extractor`
    // if the coin has been initialized with an Iguana priv key.
    let xpub_extractor = RpcTaskXPubExtractor::new_unchecked(ctx, task_handle, xpub_extractor_rpc_statuses());
    // Select the extended-public-key source by the coin's own key policy (CRD ch.05 R35c, R35a): the hardware
    // device for a coin of the Trezor policy, the in-context software derivation (`None`) for a software
    // global-HD account -- also when a hardware context exists beside a software login.
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(InitUtxoStandardError::from)?;
    let xpub_extractor = if reads_keys_from_device(&activation_params.priv_key_policy, crypto_ctx.hw_ctx().is_some())? {
        Some(&xpub_extractor)
    } else {
        None
    };
    task_handle
        .update_in_progress_status(UtxoStandardInProgressStatus::RequestingWalletBalance)
        .mm_err(Into::into)?;
    // The activation lists the address at `path_to_address`, and creates its account when the wallet holds none
    // (CRD ch.05 R35a). The Trezor policy does not honor `path_to_address` (CRD ch.55 §55.5), so its activation
    // handles the first account as before.
    let path_to_address = if matches!(activation_params.priv_key_policy, PrivKeyActivationPolicy::Trezor) {
        None
    } else {
        Some(HDAddressId::from(
            &activation_params.path_to_address.clone().unwrap_or_default(),
        ))
    };
    let wallet_balance = coin
        .enable_coin_balance(
            xpub_extractor,
            activation_params.scan_policy,
            activation_params.min_addresses_number.unwrap_or_default(),
            path_to_address,
        )
        .await
        .mm_err(|error| InitUtxoStandardError::from_enable_balance_err(error, ticker.clone()))?;
    task_handle
        .update_in_progress_status(UtxoStandardInProgressStatus::ActivatingCoin)
        .mm_err(Into::into)?;

    let result = UtxoStandardActivationResult {
        wallet_balance: UtxoWalletBalance::from_enable_coin_balance(&ticker, wallet_balance),
        ticker,
        current_block,
    };
    Ok(result)
}

pub fn xpub_extractor_rpc_statuses() -> HwConnectStatuses<UtxoStandardInProgressStatus, UtxoStandardAwaitingStatus> {
    HwConnectStatuses {
        on_connect: UtxoStandardInProgressStatus::WaitingForTrezorToConnect,
        on_connected: UtxoStandardInProgressStatus::ActivatingCoin,
        on_connection_failed: UtxoStandardInProgressStatus::Finishing,
        on_button_request: UtxoStandardInProgressStatus::FollowHwDeviceInstructions,
        on_pin_request: UtxoStandardAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: UtxoStandardAwaitingStatus::EnterTrezorPassphrase,
        on_ready: UtxoStandardInProgressStatus::ActivatingCoin,
    }
}

pub fn priv_key_build_policy(
    ctx: &MmArc,
    crypto_ctx: &CryptoCtx,
    activation_policy: PrivKeyActivationPolicy,
) -> MmResult<PrivKeyBuildPolicy, InitUtxoStandardError> {
    match activation_policy {
        PrivKeyActivationPolicy::ContextPrivKey => {
            PrivKeyBuildPolicy::detect_priv_key_policy(ctx).mm_err(|e| InitUtxoStandardError::Internal(e.to_string()))
        },
        PrivKeyActivationPolicy::IguanaPrivKey => Ok(PrivKeyBuildPolicy::IguanaPrivKey(
            crypto_ctx.mm2_internal_privkey_secret(),
        )),
        PrivKeyActivationPolicy::Trezor => Ok(PrivKeyBuildPolicy::Trezor),
    }
}
