use crate::standalone_coin::InitStandaloneCoinError;
use coins::coin_balance::EnableCoinBalanceError;
use coins::hd_wallet::NewAccountCreatingError;
use coins::utxo::utxo_builder::UtxoCoinBuildError;
use coins::{BalanceError, RegisterCoinError};
use crypto::{CryptoCtxError, CryptoInitError, HwError, HwRpcError};
use derive_more::Display;
use rpc_task::RpcTaskError;
use ser_error_derive::SerializeErrorType;
use serde_derive::Serialize;
use std::time::Duration;

/// The failure classes of the UTXO-family members, mapped onto the standalone-coin error set
/// (CRD ch. 55 R55.5.14).
#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum InitUtxoStandardError {
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CoinIsAlreadyActivated {
        ticker: String,
    },
    #[display(fmt = "Error on platform coin {} creation: {}", ticker, error)]
    CoinCreationError {
        ticker: String,
        error: String,
    },
    #[display(fmt = "Hardware wallet error: {}", _0)]
    HwError(HwRpcError),
    Transport(String),
    Internal(String),
}

impl From<RpcTaskError> for InitUtxoStandardError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::Timeout(duration) => InitUtxoStandardError::TaskTimedOut { duration },
            internal_error => InitUtxoStandardError::Internal(internal_error.to_string()),
        }
    }
}

impl From<CryptoInitError> for InitUtxoStandardError {
    /// `CryptoCtx` is expected to be initialized already.
    fn from(crypto_err: CryptoInitError) -> Self { InitUtxoStandardError::Internal(crypto_err.to_string()) }
}

impl From<CryptoCtxError> for InitUtxoStandardError {
    fn from(e: CryptoCtxError) -> Self { InitUtxoStandardError::Internal(e.to_string()) }
}

impl From<InitUtxoStandardError> for InitStandaloneCoinError {
    fn from(e: InitUtxoStandardError) -> Self {
        match e {
            InitUtxoStandardError::TaskTimedOut { duration } => InitStandaloneCoinError::TaskTimedOut { duration },
            InitUtxoStandardError::CoinIsAlreadyActivated { ticker } => {
                InitStandaloneCoinError::CoinIsAlreadyActivated { ticker }
            },
            InitUtxoStandardError::CoinCreationError { ticker, error } => {
                InitStandaloneCoinError::CoinCreationError { ticker, error }
            },
            InitUtxoStandardError::HwError(hw_error) => InitStandaloneCoinError::HwError(hw_error),
            InitUtxoStandardError::Transport(transport) => InitStandaloneCoinError::Transport(transport),
            InitUtxoStandardError::Internal(internal) => InitStandaloneCoinError::Internal(internal),
        }
    }
}

impl InitUtxoStandardError {
    /// Maps a coin build failure. No Electrum server being reachable is a backend failure (`Transport`),
    /// Electrum settings that cannot be used and internal failures are `Internal`, everything else is a
    /// coin creation failure. Building probes every listed server and fails when none connects (CRD ch.38
    /// R38.6.4k-R); a failed block-height read after the build is `Transport` too
    /// ([`Self::from_block_height_err`]).
    pub fn from_build_err(build_err: UtxoCoinBuildError, ticker: String) -> Self {
        match build_err {
            UtxoCoinBuildError::Internal(internal) => InitUtxoStandardError::Internal(internal),
            UtxoCoinBuildError::FailedToConnectToElectrums(failure) => InitUtxoStandardError::Transport(failure),
            build_err @ (UtxoCoinBuildError::NoUsableElectrumServers { .. }
            | UtxoCoinBuildError::HDWalletStorageError(_)) => InitUtxoStandardError::Internal(build_err.to_string()),
            build_err => InitUtxoStandardError::CoinCreationError {
                ticker,
                error: build_err.to_string(),
            },
        }
    }

    /// Maps a failure of the block-height query that follows the build.
    pub fn from_block_height_err(err: String) -> Self { InitUtxoStandardError::Transport(err) }

    /// Maps a failure of the balance query.
    pub fn from_balance_err(err: BalanceError, ticker: String) -> Self {
        match err {
            // A balance read sends no fee cap; the arm keeps the node's text so the conversion is total.
            BalanceError::Transport(_) | BalanceError::InvalidResponse(_) | BalanceError::FeeCapBelowBaseFee(_) => {
                InitUtxoStandardError::Transport(err.to_string())
            },
            BalanceError::Internal(_) | BalanceError::WalletStorageError(_) => {
                InitUtxoStandardError::Internal(err.to_string())
            },
            BalanceError::UnexpectedDerivationMethod(_) => InitUtxoStandardError::CoinCreationError {
                ticker,
                error: err.to_string(),
            },
        }
    }

    /// Maps a hardware-wallet failure raised while an HD account is created. A missing, duplicate or
    /// unexpected device is reported as such, a connection timeout is a task timeout, and any other
    /// failure the device reports is a coin creation failure.
    pub fn from_hw_err(err: HwError, ticker: String) -> Self {
        if let HwError::ConnectionTimedOut { timeout } = err {
            return InitUtxoStandardError::TaskTimedOut { duration: timeout };
        }
        match err.hw_rpc_error() {
            Some(
                hw_error @ (HwRpcError::NoTrezorDeviceAvailable
                | HwRpcError::FoundMultipleDevices
                | HwRpcError::FoundUnexpectedDevice),
            ) => InitUtxoStandardError::HwError(hw_error),
            _ => InitUtxoStandardError::CoinCreationError {
                ticker,
                error: err.to_string(),
            },
        }
    }

    /// Maps a failure of the wallet balance read, including the creation of the HD accounts.
    pub fn from_enable_balance_err(err: EnableCoinBalanceError, ticker: String) -> Self {
        match err {
            EnableCoinBalanceError::BalanceError(balance_err) => Self::from_balance_err(balance_err, ticker),
            EnableCoinBalanceError::NewAccountCreatingError(account_err) => match account_err {
                NewAccountCreatingError::RpcTaskError(RpcTaskError::Timeout(duration)) => {
                    InitUtxoStandardError::TaskTimedOut { duration }
                },
                NewAccountCreatingError::RpcTaskError(other) => InitUtxoStandardError::Internal(other.to_string()),
                NewAccountCreatingError::HardwareWalletError(hw_err) => Self::from_hw_err(hw_err, ticker),
                NewAccountCreatingError::ErrorSavingAccountToStorage(_) | NewAccountCreatingError::Internal(_) => {
                    InitUtxoStandardError::Internal(account_err.to_string())
                },
                NewAccountCreatingError::HwContextNotInitialized
                | NewAccountCreatingError::HDWalletUnavailable
                | NewAccountCreatingError::CoinDoesntSupportTrezor
                | NewAccountCreatingError::AccountLimitReached { .. } => InitUtxoStandardError::CoinCreationError {
                    ticker,
                    error: account_err.to_string(),
                },
            },
        }
    }
}

impl From<RegisterCoinError> for InitUtxoStandardError {
    fn from(reg_err: RegisterCoinError) -> InitUtxoStandardError {
        match reg_err {
            RegisterCoinError::CoinIsInitializedAlready { coin } => {
                InitUtxoStandardError::CoinIsAlreadyActivated { ticker: coin }
            },
            RegisterCoinError::Internal(internal) => InitUtxoStandardError::Internal(internal),
        }
    }
}
