use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinTaskHandle};
use crate::utxo_activation::init_utxo_standard_activation_error::InitUtxoStandardError;
use crate::utxo_activation::init_utxo_standard_statuses::{UtxoStandardAwaitingStatus, UtxoStandardInProgressStatus,
                                                          UtxoStandardUserAction};
use crate::utxo_activation::utxo_standard_activation_result::UtxoStandardActivationResult;
use coins::coin_balance::EnableCoinBalanceOps;
use coins::hd_pubkey::RpcTaskXPubExtractor;
use coins::utxo::UtxoActivationParams;
use coins::{MarketCoinOps, PrivKeyActivationPolicy, PrivKeyBuildPolicy};
use crypto::hw_rpc_task::HwConnectStatuses;
use crypto::{CryptoCtx, CryptoCtxError};
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

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
    let current_block =
        coin.current_block()
            .compat()
            .await
            .map_to_mm(|error| InitUtxoStandardError::CoinCreationError {
                ticker: coin.ticker().to_owned(),
                error,
            })?;

    // Construct an Xpub extractor without checking if the MarketMaker supports HD wallet ops.
    // [`EnableCoinBalanceOps::enable_coin_balance`] won't just use `xpub_extractor`
    // if the coin has been initialized with an Iguana priv key.
    let xpub_extractor = RpcTaskXPubExtractor::new_unchecked(ctx, task_handle, xpub_extractor_rpc_statuses());
    // Select the extended-public-key source by key-pair policy: the hardware device when a
    // hardware-wallet context is active, otherwise the in-context software derivation (`None`)
    // for a software global-HD account.
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|error| InitUtxoStandardError::CoinCreationError {
        ticker: coin.ticker().to_owned(),
        error: error.to_string(),
    })?;
    let xpub_extractor = if crypto_ctx.hw_ctx().is_some() {
        Some(&xpub_extractor)
    } else {
        None
    };
    task_handle
        .update_in_progress_status(UtxoStandardInProgressStatus::RequestingWalletBalance)
        .mm_err(Into::into)?;
    let wallet_balance = coin
        .enable_coin_balance(
            xpub_extractor,
            activation_params.scan_policy,
            activation_params.min_addresses_number.unwrap_or_default(),
        )
        .await
        .mm_err(|error| InitUtxoStandardError::CoinCreationError {
            ticker: coin.ticker().to_owned(),
            error: error.to_string(),
        })?;
    task_handle
        .update_in_progress_status(UtxoStandardInProgressStatus::ActivatingCoin)
        .mm_err(Into::into)?;

    let result = UtxoStandardActivationResult {
        current_block,
        wallet_balance,
    };
    Ok(result)
}

pub fn xpub_extractor_rpc_statuses() -> HwConnectStatuses<UtxoStandardInProgressStatus, UtxoStandardAwaitingStatus> {
    HwConnectStatuses {
        on_connect: UtxoStandardInProgressStatus::WaitingForTrezorToConnect,
        on_connected: UtxoStandardInProgressStatus::ActivatingCoin,
        on_connection_failed: UtxoStandardInProgressStatus::Finishing,
        on_button_request: UtxoStandardInProgressStatus::WaitingForUserToConfirmPubkey,
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
            PrivKeyBuildPolicy::detect_priv_key_policy(ctx).mm_err(|e: CryptoCtxError| {
                InitUtxoStandardError::CoinCreationError {
                    ticker: String::new(),
                    error: e.to_string(),
                }
            })
        },
        PrivKeyActivationPolicy::IguanaPrivKey => Ok(PrivKeyBuildPolicy::IguanaPrivKey(
            crypto_ctx.mm2_internal_privkey_secret(),
        )),
        PrivKeyActivationPolicy::Trezor => Ok(PrivKeyBuildPolicy::Trezor),
    }
}
