use crate::context::CoinsActivationContext;
use crate::prelude::*;
use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinError,
                             InitStandaloneCoinInitialStatus, InitStandaloneCoinTaskHandle,
                             InitStandaloneCoinTaskManagerShared};
use async_trait::async_trait;
use coins::coin_balance::{EnableCoinBalance, EnableCoinBalanceOps, EnableCoinScanPolicy, IguanaWalletBalance};
use coins::hd_pubkey::RpcTaskXPubExtractor;
use coins::siacoin::{SiaCoin, SiaCoinActivationRequest, SiaCoinNewError};
use coins::{BalanceError, CoinBalance, CoinProtocol, MarketCoinOps, PrivKeyBuildPolicy, RegisterCoinError};
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use crypto::{CryptoCtx, CryptoCtxError};
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
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CouldNotGetBalance(String),
    CouldNotGetBlockCount(String),
    Internal(String),
}

/// The `MM2.json` key of the compatibility switch for the shape of `wallet_balance` under an HD wallet.
const SIA_SINGLE_ADDRESS_BALANCE_KEY: &str = "sia_activation_single_address_balance";

/// Whether `wallet_balance` is always reported in the single-address `"Iguana"` form (CRD ch.46 R46.2.4).
///
/// Docs vs reference: the public docs do not document Sia activation at all. The reference always reports
/// the single-address `"Iguana"` form, also under an HD wallet. Reloaded deliberately reports the HD
/// per-account report under an HD wallet instead, so that a Sia coin shows its balance in the same shape
/// as the other coins of an HD wallet (maintainer decision 2026-10-11, R46.2.4). Both shapes carry the
/// same single address and balance, and a client tells them apart by `wallet_type`. Setting this switch to
/// `true` restores the reference behavior: `wallet_balance` is then always the single-address form, with
/// `address` being the single derived address under HD. Under the single-key policy the switch changes
/// nothing, because that form is the only one.
fn single_address_balance_requested(conf: &Json) -> bool {
    conf[SIA_SINGLE_ADDRESS_BALANCE_KEY].as_bool() == Some(true)
}

/// The single-address `wallet_balance` of R46.2.4.
fn single_address_wallet_balance(address: String, balance: CoinBalance) -> EnableCoinBalance {
    EnableCoinBalance::Iguana(IguanaWalletBalance { address, balance })
}

impl SiaInitError {
    fn from_new_err(err: SiaCoinNewError, ticker: String) -> Self {
        match err {
            // A signing policy other than the single-key and the HD one is a task-level
            // `CoinCreationError`, not a dedicated key-policy discriminant (CRD ch.46 R46.1.3, R46.5.2).
            SiaCoinNewError::UnsupportedPrivKeyPolicy => SiaInitError::CoinCreationError {
                ticker,
                error: "Sia supports only the single-key and single-address HD signing policies".to_string(),
            },
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
            SiaInitError::TaskTimedOut { duration } => InitStandaloneCoinError::TaskTimedOut { duration },
            // The coin was built (walletd answered the probe) but the height or the balance could not be
            // read afterwards (CRD ch.46 R46.5.2). The public catalogue's `CouldNotGetBalance` and
            // `CouldNotGetBlockCount` names never reach the wire (R46.5.3).
            SiaInitError::CouldNotGetBalance(e) | SiaInitError::CouldNotGetBlockCount(e) => {
                InitStandaloneCoinError::Transport(e)
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
        ctx: MmArc,
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

        // `wallet_balance`'s shape is dictated by ch.46 R46.2.4: the single-address
        // ("Iguana") `{address, balance}` report or, for an HD wallet, the generic
        // per-account/per-address balance report -- the same `EnableCoinBalanceOps`
        // blanket impl UTXO/ETH's own activation results use, which branches on
        // `self.derivation_method()` (CRD ch.20 D1). `xpub_extractor` is
        // constructed the same way UTXO/ETH's own `get_activation_result` construct
        // theirs, but for Sia it is *always* `None` in practice: a Trezor priv-key
        // policy is rejected long before this point (R-T3, ch.46 R46.1.3), so
        // `crypto_ctx.hw_ctx()` can never be `Some` for an activated `SiaCoin`.
        let xpub_extractor = RpcTaskXPubExtractor::new_unchecked(&ctx, task_handle, sia_xpub_extractor_rpc_statuses());
        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|error| SiaInitError::Internal(error.to_string()))?;
        let xpub_extractor = if crypto_ctx.hw_ctx().is_some() {
            Some(&xpub_extractor)
        } else {
            None
        };

        // Activation itself never grows the HD wallet past account 0 (ch.46
        // R46.1.3: only a single-address HD-account state reaches a successfully
        // activated Sia coin) -- `EnableCoinScanPolicy::default()`
        // (`ScanIfNewWallet`) only scans a *newly created* account's addresses up
        // to its gap limit; it never creates a second account, and `SiaHDWallet`
        // has no persistent account storage to have grown one from a prior run
        // (`sia_hd_wallet.rs` module doc comment), so the "new wallet" branch is
        // the only one ever taken here.
        let wallet_balance = if single_address_balance_requested(&ctx.conf) {
            // Compatibility switch `sia_activation_single_address_balance`: the reference's
            // single-address report, also under HD (see `single_address_balance_requested`).
            let address = self.my_address().map_to_mm(SiaInitError::Internal)?;
            let balance = self
                .my_balance()
                .compat()
                .await
                .mm_err(|error| SiaInitError::CouldNotGetBalance(error.to_string()))?;
            single_address_wallet_balance(address, balance)
        } else {
            self.enable_coin_balance(xpub_extractor, EnableCoinScanPolicy::default(), 0)
                .await
                .mm_err(|error| SiaInitError::CouldNotGetBalance(error.to_string()))?
        };

        task_handle
            .update_in_progress_status(SiaInProgressStatus::Finishing)
            .mm_err(Into::into)?;

        Ok(SiaActivationResult {
            ticker: self.ticker().to_owned(),
            current_block,
            wallet_balance,
        })
    }
}

/// `HwConnectStatuses` mandates values even for the Trezor-connect states Sia can
/// never reach in practice (see `get_activation_result`'s doc comment) --
/// `SiaInProgressStatus` deliberately gains no new variants for them (unlike
/// `UtxoStandardInProgressStatus`'s `WaitingForTrezorToConnect`/
/// `WaitingForUserToConfirmPubkey`); the existing states are reused since these
/// slots are structurally required but never actually surfaced for Sia.
fn sia_xpub_extractor_rpc_statuses() -> HwConnectStatuses<SiaInProgressStatus, SiaAwaitingStatus> {
    HwConnectStatuses {
        on_connect: SiaInProgressStatus::ActivatingCoin,
        on_connected: SiaInProgressStatus::ActivatingCoin,
        on_connection_failed: SiaInProgressStatus::Finishing,
        on_button_request: SiaInProgressStatus::ActivatingCoin,
        on_pin_request: SiaAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: SiaAwaitingStatus::EnterTrezorPassphrase,
        on_ready: SiaInProgressStatus::ActivatingCoin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{HttpStatusCode, StatusCode};
    use serde_json::json;

    fn task_error(err: SiaInitError) -> (Json, StatusCode) {
        let err: InitStandaloneCoinError = err.into();
        (serde_json::to_value(&err).unwrap(), err.status_code())
    }

    #[test]
    fn height_and_balance_failures_are_transport_errors_with_a_string() {
        // T46.8.2 (b)
        for err in [
            SiaInitError::CouldNotGetBlockCount("walletd consensus tip is unavailable".to_owned()),
            SiaInitError::CouldNotGetBalance("walletd balance is unavailable".to_owned()),
        ] {
            let (wire, status) = task_error(err);
            assert_eq!(wire["error_type"], "Transport");
            assert!(wire["error_data"].is_string());
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    #[test]
    fn unsupported_key_policy_is_a_coin_creation_error() {
        // T46.8.2 (c)
        let err = SiaInitError::from_new_err(SiaCoinNewError::UnsupportedPrivKeyPolicy, "SC".to_owned());
        let (wire, status) = task_error(err);
        assert_eq!(wire["error_type"], "CoinCreationError");
        assert_eq!(wire["error_data"]["ticker"], "SC");
        assert!(wire["error_data"]["error"].is_string());
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn no_sia_error_exposes_a_forbidden_discriminant() {
        // R46.5.3
        let forbidden = [
            "PrivKeyNotAllowed",
            "UnexpectedDerivationMethod",
            "HwError",
            "HardwareWalletsAreNotSupportedYet",
            "CouldNotGetBalance",
            "CouldNotGetBlockCount",
        ];
        for err in [
            SiaInitError::CoinCreationError {
                ticker: "SC".to_owned(),
                error: "e".to_owned(),
            },
            SiaInitError::CoinIsAlreadyActivated {
                ticker: "SC".to_owned(),
            },
            SiaInitError::TaskTimedOut {
                duration: Duration::from_secs(1),
            },
            SiaInitError::CouldNotGetBalance("e".to_owned()),
            SiaInitError::CouldNotGetBlockCount("e".to_owned()),
            SiaInitError::Internal("e".to_owned()),
        ] {
            let (wire, _) = task_error(err);
            let name = wire["error_type"].as_str().unwrap().to_owned();
            assert!(!forbidden.contains(&name.as_str()), "{name} must not be observable");
        }
    }

    #[test]
    fn single_address_balance_switch_defaults_to_false_and_needs_a_boolean_true() {
        // T46.8.3: the two values of `sia_activation_single_address_balance`.
        assert!(!single_address_balance_requested(&json!({})));
        assert!(!single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": false })
        ));
        assert!(!single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": "true" })
        ));
        assert!(single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": true })
        ));
    }

    #[test]
    fn single_address_wallet_balance_has_the_iguana_wire_shape() {
        // R46.2.4: what the switch selects under HD is the single-address "Iguana" form.
        let balance = single_address_wallet_balance("addr:abc".to_owned(), CoinBalance::default());
        assert_eq!(
            serde_json::to_value(balance).unwrap(),
            json!({
                "wallet_type": "Iguana",
                "address": "addr:abc",
                "balance": { "spendable": "0", "unspendable": "0" }
            })
        );
    }
}
