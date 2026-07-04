use crate::context::CoinsActivationContext;
use crate::prelude::*;
use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinError,
                             InitStandaloneCoinInitialStatus, InitStandaloneCoinTaskHandle,
                             InitStandaloneCoinTaskManagerShared};
use async_trait::async_trait;
use coins::coin_balance::{EnableCoinBalance, IguanaWalletBalance};
use coins::siacoin::{SiaCoin, SiaCoinActivationRequest, SiaCoinNewError};
use coins::{BalanceError, CoinProtocol, MarketCoinOps, PrivKeyBuildPolicy, RegisterCoinError};
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use crypto::CryptoCtxError;
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::RpcTaskError;
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::time::Duration;

pub type SiaTaskManagerShared = InitStandaloneCoinTaskManagerShared<SiaCoin>;
pub type SiaRpcTaskHandle = InitStandaloneCoinTaskHandle<SiaCoin>;
pub type SiaAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type SiaUserAction = HwRpcTaskUserAction;

/// The final `ok` activation result of `task::enable_sia::status` (R46.2.4).
#[derive(Clone, Serialize)]
pub struct SiaActivationResult {
    pub ticker: String,
    pub current_block: u64,
    pub wallet_balance: EnableCoinBalance,
}

impl CurrentBlock for SiaActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

/// The in-progress status enumeration reported by `status` (R46.2.3).
#[derive(Clone, Serialize)]
#[non_exhaustive]
pub enum SiaInProgressStatus {
    ActivatingCoin,
    RequestingWalletBalance,
    Finishing,
}

impl InitStandaloneCoinInitialStatus for SiaInProgressStatus {
    fn initial_status() -> Self { SiaInProgressStatus::ActivatingCoin }
}

/// The `activation_params` object of `task::enable_sia::init` (R46.1.2).
///
/// This newtype wraps the Sia coin's own activation-request type so the same
/// `{client_conf, tx_history, required_confirmations, gap_limit}` shape is
/// reused verbatim; the wrapper exists only to satisfy the local `TxHistory`
/// bound the standalone-coin task framework requires.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct SiaActivationParams(SiaCoinActivationRequest);

impl TxHistory for SiaActivationParams {
    fn tx_history(&self) -> bool { self.0.tx_history }
}

#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SiaInitError {
    #[display(fmt = "Error on coin {} creation: {}", ticker, error)]
    CoinCreationError {
        ticker: String,
        error: String,
    },
    CoinIsAlreadyActivated {
        ticker: String,
    },
    #[display(fmt = "Private key is not allowed: {}", _0)]
    PrivKeyNotAllowed(String),
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CouldNotGetBalance(String),
    CouldNotGetBlockCount(String),
    Internal(String),
}

impl SiaInitError {
    fn from_new_err(err: SiaCoinNewError, ticker: String) -> Self {
        match err {
            SiaCoinNewError::UnsupportedPrivKeyPolicy => SiaInitError::PrivKeyNotAllowed(
                "Sia supports only the single-key and single-address HD signing policies".to_string(),
            ),
            other => SiaInitError::CoinCreationError {
                ticker,
                error: other.to_string(),
            },
        }
    }
}

impl From<BalanceError> for SiaInitError {
    fn from(err: BalanceError) -> Self { SiaInitError::CouldNotGetBalance(err.to_string()) }
}

impl From<RegisterCoinError> for SiaInitError {
    fn from(reg_err: RegisterCoinError) -> SiaInitError {
        match reg_err {
            RegisterCoinError::CoinIsInitializedAlready { coin } => {
                SiaInitError::CoinIsAlreadyActivated { ticker: coin }
            },
            RegisterCoinError::Internal(internal) => SiaInitError::Internal(internal),
        }
    }
}

impl From<RpcTaskError> for SiaInitError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::Timeout(duration) => SiaInitError::TaskTimedOut { duration },
            internal_error => SiaInitError::Internal(internal_error.to_string()),
        }
    }
}

impl From<CryptoCtxError> for SiaInitError {
    fn from(err: CryptoCtxError) -> Self { SiaInitError::Internal(err.to_string()) }
}

impl From<SiaInitError> for InitStandaloneCoinError {
    fn from(err: SiaInitError) -> Self {
        match err {
            SiaInitError::CoinCreationError { ticker, error } => {
                InitStandaloneCoinError::CoinCreationError { ticker, error }
            },
            SiaInitError::CoinIsAlreadyActivated { ticker } => {
                InitStandaloneCoinError::CoinIsAlreadyActivated { ticker }
            },
            SiaInitError::PrivKeyNotAllowed(e) => InitStandaloneCoinError::PrivKeyNotAllowed(e),
            SiaInitError::TaskTimedOut { duration } => InitStandaloneCoinError::TaskTimedOut { duration },
            SiaInitError::CouldNotGetBalance(e) | SiaInitError::CouldNotGetBlockCount(e) => {
                InitStandaloneCoinError::Internal(e)
            },
            SiaInitError::Internal(e) => InitStandaloneCoinError::Internal(e),
        }
    }
}

pub struct SiaProtocolInfo;

impl TryFromCoinProtocol for SiaProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::SIA => Ok(SiaProtocolInfo),
            protocol => MmError::err(protocol),
        }
    }
}

#[async_trait]
impl InitStandaloneCoinActivationOps for SiaCoin {
    type ActivationRequest = SiaActivationParams;
    type StandaloneProtocol = SiaProtocolInfo;
    type ActivationResult = SiaActivationResult;
    type ActivationError = SiaInitError;
    type InProgressStatus = SiaInProgressStatus;
    type AwaitingStatus = SiaAwaitingStatus;
    type UserAction = SiaUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &SiaTaskManagerShared {
        &activation_ctx.init_sia_task_manager
    }

    async fn init_standalone_coin(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: &SiaActivationParams,
        _protocol_info: SiaProtocolInfo,
        _task_handle: &SiaRpcTaskHandle,
    ) -> MmResult<Self, SiaInitError> {
        let priv_key_policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).mm_err(SiaInitError::from)?;
        let coin = SiaCoin::new(&ctx, coin_conf, &activation_request.0, priv_key_policy)
            .await
            .mm_err(|e| SiaInitError::from_new_err(e, ticker))?;
        Ok(coin)
    }

    async fn get_activation_result(
        &self,
        _ctx: MmArc,
        task_handle: &SiaRpcTaskHandle,
        _activation_request: &Self::ActivationRequest,
    ) -> MmResult<Self::ActivationResult, SiaInitError> {
        task_handle
            .update_in_progress_status(SiaInProgressStatus::RequestingWalletBalance)
            .mm_err(Into::into)?;
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(SiaInitError::CouldNotGetBlockCount)?;

        let balance = self.my_balance().compat().await.mm_err(Into::into)?;
        let address = self.my_address().map_to_mm(SiaInitError::Internal)?;

        task_handle
            .update_in_progress_status(SiaInProgressStatus::Finishing)
            .mm_err(Into::into)?;

        Ok(SiaActivationResult {
            ticker: self.ticker().to_owned(),
            current_block,
            wallet_balance: EnableCoinBalance::Iguana(IguanaWalletBalance { address, balance }),
        })
    }
}
