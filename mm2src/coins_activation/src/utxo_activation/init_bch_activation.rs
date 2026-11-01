//! `task::enable_bch` (CRD ch. 55 §55.8): the standalone-coin task that activates the BCH coin only.
//! SLP tokens are activated afterwards with `enable_slp`.

use crate::bch_with_tokens_activation::BchProtocolInfo;
use crate::context::CoinsActivationContext;
use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinTaskHandle,
                             InitStandaloneCoinTaskManagerShared};
use crate::utxo_activation::common_impl::validate_electrum_connection_settings;
use crate::utxo_activation::init_utxo_standard_activation_error::InitUtxoStandardError;
use crate::utxo_activation::init_utxo_standard_statuses::{UtxoStandardAwaitingStatus, UtxoStandardInProgressStatus,
                                                          UtxoStandardUserAction};
use crate::utxo_activation::utxo_standard_activation_result::{UtxoStandardActivationResult, UtxoWalletBalance};
use async_trait::async_trait;
use coins::utxo::bch::{bch_coin_from_conf_params_and_policy, BchActivationRequest, BchCoin, CashAddrPrefix};
use coins::{MarketCoinOps, PrivKeyActivationPolicy, PrivKeyBuildPolicy};
use crypto::CryptoCtx;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;
use std::str::FromStr;

pub type BchTaskManagerShared = InitStandaloneCoinTaskManagerShared<BchCoin>;
pub type BchRpcTaskHandle = InitStandaloneCoinTaskHandle<BchCoin>;

/// Why a BCH activation cannot use the requested key policy.
fn unsupported_policy_error(ticker: &str) -> InitUtxoStandardError {
    InitUtxoStandardError::CoinCreationError {
        ticker: ticker.to_owned(),
        error: "BCH supports only the single-address key policy; a hardware wallet cannot be used".to_owned(),
    }
}

#[async_trait]
impl InitStandaloneCoinActivationOps for BchCoin {
    type ActivationRequest = BchActivationRequest;
    type StandaloneProtocol = BchProtocolInfo;
    type ActivationResult = UtxoStandardActivationResult;
    type ActivationError = InitUtxoStandardError;
    type InProgressStatus = UtxoStandardInProgressStatus;
    type AwaitingStatus = UtxoStandardAwaitingStatus;
    type UserAction = UtxoStandardUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &BchTaskManagerShared {
        &activation_ctx.init_bch_task_manager
    }

    async fn init_standalone_coin(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: &Self::ActivationRequest,
        protocol_info: Self::StandaloneProtocol,
        _task_handle: &BchRpcTaskHandle,
    ) -> MmResult<Self, InitUtxoStandardError> {
        if activation_request.has_unsafe_slp_configuration() {
            return MmError::err(InitUtxoStandardError::CoinCreationError {
                ticker,
                error: "Using empty bchd_urls is unsafe for SLP users!".to_owned(),
            });
        }
        let slp_prefix = CashAddrPrefix::from_str(protocol_info.slp_prefix()).map_to_mm(|error| {
            InitUtxoStandardError::CoinCreationError {
                ticker: ticker.clone(),
                error: format!(
                    "Invalid slp prefix {} configured for {}: {}",
                    protocol_info.slp_prefix(),
                    ticker,
                    error
                ),
            }
        })?;
        validate_electrum_connection_settings(&activation_request.utxo_params)?;

        // The BCH coin and its SLP tokens operate on a single address (like `enable_bch_with_tokens`),
        // so the wallet's internal key serves every software policy and a hardware wallet is refused.
        let priv_key_policy = match activation_request.utxo_params.priv_key_policy {
            PrivKeyActivationPolicy::ContextPrivKey | PrivKeyActivationPolicy::IguanaPrivKey => {
                let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(Into::into)?;
                PrivKeyBuildPolicy::IguanaPrivKey(crypto_ctx.mm2_internal_privkey_secret())
            },
            PrivKeyActivationPolicy::Trezor => return MmError::err(unsupported_policy_error(&ticker)),
        };

        bch_coin_from_conf_params_and_policy(
            &ctx,
            &ticker,
            &coin_conf,
            activation_request,
            slp_prefix,
            priv_key_policy,
        )
        .await
        .mm_err(|e| InitUtxoStandardError::from_build_err(e, ticker))
    }

    async fn get_activation_result(
        &self,
        _ctx: MmArc,
        task_handle: &BchRpcTaskHandle,
        _activation_request: &Self::ActivationRequest,
    ) -> MmResult<Self::ActivationResult, InitUtxoStandardError> {
        let ticker = self.ticker().to_owned();
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(InitUtxoStandardError::from_block_height_err)?;
        let address = self
            .my_address()
            .map_to_mm(|error| InitUtxoStandardError::CoinCreationError {
                ticker: ticker.clone(),
                error,
            })?;

        task_handle
            .update_in_progress_status(UtxoStandardInProgressStatus::RequestingWalletBalance)
            .mm_err(Into::into)?;
        let balance = self
            .my_balance()
            .compat()
            .await
            .mm_err(|error| InitUtxoStandardError::from_balance_err(error, ticker.clone()))?;
        task_handle
            .update_in_progress_status(UtxoStandardInProgressStatus::ActivatingCoin)
            .mm_err(Into::into)?;

        Ok(UtxoStandardActivationResult {
            wallet_balance: UtxoWalletBalance::iguana(&ticker, address, balance),
            ticker,
            current_block,
        })
    }
}
