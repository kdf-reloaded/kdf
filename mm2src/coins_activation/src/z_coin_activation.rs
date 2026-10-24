use crate::context::CoinsActivationContext;
use crate::prelude::*;
use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinError,
                             InitStandaloneCoinInitialStatus, InitStandaloneCoinTaskHandle,
                             InitStandaloneCoinTaskManagerShared};
use async_trait::async_trait;
use coins::coin_balance::{EnableCoinBalance, IguanaWalletBalance};
use coins::utxo::rpc_clients::ElectrumRpcRequest;
use coins::utxo::{UtxoActivationParams, UtxoRpcMode};
use coins::z_coin::{z_coin_from_conf_and_params, ZCoin, ZCoinBuildError, ZcoinBackend, ZcoinProtocolInfo};
#[cfg(not(target_arch = "wasm32"))]
use coins::z_coin::{LightwalletdError, ShieldedFetchError};
use coins::{BalanceError, CoinProtocol, MarketCoinOps, PrivKeyActivationPolicy, RegisterCoinError};
use common::{executor::Timer, log, now_ms};
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use crypto::{CryptoCtx, CryptoCtxError, CryptoInitError};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::RpcTaskError;
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::future::Future;
use std::num::NonZeroU32;
use std::time::Duration;

/// Pause between two attempts of a shielded sync that failed after it had started (CRD ch.39
/// R39.10.14: "on the order of ten seconds").
#[cfg(not(target_arch = "wasm32"))]
const SHIELDED_SYNC_RETRY_PAUSE_SECS: f64 = 10.;

/// Recorded deviation (CRD ch.39 R39.10.1): `task::enable_z_coin` is routed on WASM as the chapter requires,
/// but the WebAssembly build has no shielded wallet database, so `ZCoin::my_balance` cannot report a real
/// balance there. An activation therefore ends with `CoinCreationError` before the coin is built or
/// registered, rather than reporting a made-up zero balance.
const SHIELDED_WALLET_UNAVAILABLE: bool = cfg!(target_arch = "wasm32");

fn shielded_unavailable_error(ticker: &str) -> ZcoinInitError {
    ZcoinInitError::CoinCreationError {
        ticker: ticker.to_owned(),
        error: "Shielded activation is not available in the WebAssembly build: it has no shielded wallet database"
            .to_owned(),
    }
}

/// The backend an activation request asks for. A Light-mode request with no lightwalletd endpoint is
/// a configuration error naming the coin, reported before any shielded work begins -- not a
/// transient backend failure (CRD ch.39 R39.11.5). `electrum_servers` and the Electrum connection
/// counts are accepted on the wire and have no effect here (R39.11.5, R39.11.7).
fn zcoin_backend(ticker: &str, mode: &ZcoinRpcMode) -> Result<ZcoinBackend, ZcoinInitError> {
    match mode {
        #[cfg(not(target_arch = "wasm32"))]
        ZcoinRpcMode::Native => Ok(ZcoinBackend::Native),
        ZcoinRpcMode::Light {
            light_wallet_d_servers, ..
        } => {
            if light_wallet_d_servers.is_empty() {
                return Err(ZcoinInitError::CoinCreationError {
                    ticker: ticker.to_owned(),
                    error: "`light_wallet_d_servers` must list at least one lightwalletd endpoint".to_owned(),
                });
            }
            Ok(ZcoinBackend::Light {
                light_wallet_d_servers: light_wallet_d_servers.clone(),
            })
        },
    }
}

pub type ZcoinTaskManagerShared = InitStandaloneCoinTaskManagerShared<ZCoin>;
pub type ZcoinRpcTaskHandle = InitStandaloneCoinTaskHandle<ZCoin>;
pub type ZcoinAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type ZcoinUserAction = HwRpcTaskUserAction;

#[derive(Clone, Serialize)]
pub struct ZcoinActivationResult {
    pub ticker: String,
    pub current_block: u64,
    pub wallet_balance: EnableCoinBalance,
    /// The resolved shielded sync start block (R39.8.0h). Emitted
    /// unconditionally to match the dictated activation result: when the caller
    /// supplied a `sync_params` it reflects that request, otherwise it reflects
    /// the height the shielded scan was actually anchored at.
    pub first_sync_block: FirstSyncBlock,
}

/// Details of the block from which the initial shielded sync was anchored,
/// reported back to the caller when a `sync_start` was supplied (R39.8.0h).
#[derive(Clone, Serialize)]
pub struct FirstSyncBlock {
    /// The start height the caller requested (a height directly, or the block
    /// resolved from a requested calendar date).
    pub requested: u64,
    /// Whether `requested` is below this coin's Sapling activation height.
    pub is_pre_sapling: bool,
    /// The height actually used to anchor the sync: `requested`, floored at the
    /// Sapling activation height.
    pub actual: u64,
}

impl CurrentBlock for ZcoinActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

// Docs vs reference (CRD ch.39 R39.8, R39.10.7): the public in-progress list omits
// `UpdatingBlocksCache`, `BuildingWalletDb` and `TemporaryError`, which the reference emits, and wrongly
// lists `FollowHwDeviceInstructions`, which a shielded-coin task never reaches. Reloaded follows the
// reference: it emits the first three and never emits `FollowHwDeviceInstructions`, so a client sees
// exactly the values the reference produces.
#[derive(Clone, Serialize)]
#[non_exhaustive]
pub enum ZcoinInProgressStatus {
    ActivatingCoin,
    /// Compact-block cache download phase (R39.3.1). Serializes as the dictated
    /// `{"UpdatingBlocksCache": {current_scanned_block, latest_block}}`.
    UpdatingBlocksCache {
        current_scanned_block: u64,
        latest_block: u64,
    },
    /// Wallet-database build/scan phase (R39.3.1). Serializes as the dictated
    /// `{"BuildingWalletDb": {current_scanned_block, latest_block}}`.
    BuildingWalletDb {
        current_scanned_block: u64,
        latest_block: u64,
    },
    /// A retriable shielded-backend or sync failure; the activation keeps retrying and the task stays
    /// alive (CRD ch.39 R39.10.7, R39.10.14). Serializes as `{"TemporaryError": "<text>"}`.
    TemporaryError(String),
    RequestingWalletBalance,
    Finishing,
    /// This status doesn't require the user to send `UserAction`,
    /// but it tells the user that he should confirm/decline an address on his device.
    WaitingForTrezorToConnect,
    WaitingForUserToConfirmPubkey,
}

impl InitStandaloneCoinInitialStatus for ZcoinInProgressStatus {
    fn initial_status() -> Self { ZcoinInProgressStatus::ActivatingCoin }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "rpc", content = "rpc_data")]
pub enum ZcoinRpcMode {
    /// Full-node backend. It exists only on native builds: on WASM the name is an unknown variant and
    /// the request fails to parse (CRD ch.39 R39.10.4).
    #[cfg(not(target_arch = "wasm32"))]
    Native,
    /// lightwalletd backend. `"Electrum"` is accepted as a synonym of `"Light"`, as the public
    /// `ZcoinRpcModeEnum` documents it (CRD ch.39 R39.10.4).
    ///
    /// lightwalletd is the **sole** backend of a Light-mode coin (R39.11.1, R39.11.7): no Electrum
    /// connection is opened, attempted, probed or reported on, and activation never fails for an
    /// Electrum-related reason.
    #[serde(alias = "Electrum")]
    Light {
        /// Accepted and ignored (R39.11.5): deployed KDF-family wallets send it unconditionally and a
        /// parse rejection would break every one of them. It may be absent or empty. `servers` is
        /// accepted as a synonym (CRD ch.39 R39.10.4).
        #[serde(default, alias = "servers")]
        electrum_servers: Vec<ElectrumRpcRequest>,
        /// Accepted and ignored, like `electrum_servers` (R39.11.5).
        #[serde(default)]
        min_connected: Option<usize>,
        /// Accepted and ignored, like `electrum_servers` (R39.11.5).
        #[serde(default)]
        max_connected: Option<usize>,
        /// Mandatory and non-empty (R39.11.5): the only backend of the coin.
        light_wallet_d_servers: Vec<String>,
        /// Optional shielded sync starting point (R39.6.2). Externally dictated
        /// wire location: nested inside the Light-mode `rpc_data` alongside the
        /// server lists, exactly as sent by KDF-family wallets.
        #[serde(default)]
        sync_params: Option<ZcoinSyncParams>,
        /// Optional resume flag (R39.6.2 `skip_sync_params`): when true, resume
        /// from existing local sync state and consult `sync_params` only when no
        /// prior synced state exists. Sibling of `sync_params` in `rpc_data`.
        #[serde(default)]
        skip_sync_params: Option<bool>,
    },
}

/// Shielded sync starting point (R39.6.2). This is the externally *dictated*
/// wire shape used by KDF-family wallets for `mode.rpc_data.sync_params`:
///
/// - `{"height": <block-height>}` — start from an explicit block height;
/// - `{"date": <unix-timestamp>}` — start from the block matching a date;
/// - `"earliest"` — start from Sapling activation.
///
/// Externally tagged with lowercase variant names so the serde representation is
/// exactly the dictated JSON.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ZcoinSyncParams {
    Date(u64),
    Height(u64),
    Earliest,
}

/// The backend used when `mode` is absent: Light mode with empty server lists, which then ends the task
/// with `CoinCreationError` (CRD ch.39 R39.10.4, R39.10.13).
fn default_zcoin_rpc_mode() -> ZcoinRpcMode {
    ZcoinRpcMode::Light {
        electrum_servers: Vec::new(),
        min_connected: None,
        max_connected: None,
        light_wallet_d_servers: Vec::new(),
        sync_params: None,
        skip_sync_params: None,
    }
}

// Members the public `ActivationRpcData` table lists but that have no Z-coin meaning (`tx_history`,
// `get_balances`, `gap_limit`, `scan_policy`, `path_to_address`, `min_addresses_number`) and any other
// unknown member are deliberately ignored: this struct has no strict-field rejection (CRD ch.39 R39.10.5).
#[derive(Deserialize)]
pub struct ZcoinActivationParams {
    #[serde(default = "default_zcoin_rpc_mode")]
    pub mode: ZcoinRpcMode,
    pub required_confirmations: Option<u64>,
    pub requires_notarization: Option<bool>,
    /// Sync-throughput tuning: blocks processed per iteration (R39.6.2). The
    /// dictated wire name is `scan_blocks_per_iteration`; `blocks_per_iteration`
    /// is accepted as an alias. Defaults to 1000.
    /// A zero is a request-parse failure, not a silent clamp (CRD ch.39 R39.10.4).
    #[serde(default, alias = "scan_blocks_per_iteration")]
    pub blocks_per_iteration: Option<NonZeroU32>,
    /// Sync pacing interval (R39.6.2). The public-API dictated wire name is
    /// `scan_interval_ms`; the desktop wallet sends `scan_interval`. Both are
    /// accepted as aliases. Defaults to 0.
    #[serde(default, alias = "scan_interval_ms", alias = "scan_interval")]
    pub inter_iteration_interval_ms: Option<u64>,
    /// HD account index for the shielded key derivation (R39.6.4 §2). Used only
    /// under the HD (BIP39) key policy, where the shielded spending key is
    /// derived at `m/<z_derivation_path>/account'`. Ignored under the legacy
    /// Iguana policy. Defaults to `0`.
    #[serde(default)]
    pub account: Option<u32>,
    /// Optional Sapling parameter directory (R39.6.2 `zcash_params_path`). When
    /// present it overrides the platform-default parameter location. Native
    /// only; ignored where shielded proving is unavailable.
    #[serde(default)]
    pub zcash_params_path: Option<String>,
}

impl ZcoinActivationParams {
    /// The shielded sync starting point, extracted from the Light-mode
    /// `rpc_data` where the dictated wire places it. `None` in Native mode or
    /// when no sync point was supplied.
    fn sync_params(&self) -> Option<&ZcoinSyncParams> {
        match &self.mode {
            ZcoinRpcMode::Light { sync_params, .. } => sync_params.as_ref(),
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinRpcMode::Native => None,
        }
    }

    /// Whether the caller asked to resume from existing local sync state
    /// (R39.6.2 `skip_sync_params`). `false` in Native mode or when unset.
    fn skip_sync_params(&self) -> bool {
        match &self.mode {
            ZcoinRpcMode::Light { skip_sync_params, .. } => skip_sync_params.unwrap_or(false),
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinRpcMode::Native => false,
        }
    }
}

impl TxHistory for ZcoinActivationParams {
    fn tx_history(&self) -> bool { false }
}

/// Resolves a requested sync date to the last block at or before it (CRD ch.39 R39.8.0h, R39.10.11).
///
/// A date in the future is an error. "Future" means later than both the wall clock (`now`) and the
/// tip's own block time: a date after the tip but not after `now` is the ordinary case of a chain
/// that has not produced a block since that moment, and resolves to the tip; a date after `now` that
/// is covered by a tip block time ahead of the clock (block times may run ahead of it) resolves to a
/// real block. Only a date beyond both is rejected. A start given as a height is not handled here and
/// keeps clamping to the tip.
async fn resolve_requested_shielded_scan_start_height<F, Fut>(
    target_timestamp: u64,
    tip_height: u64,
    now: u64,
    mut get_block_timestamp: F,
) -> Result<u64, String>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<u64, String>>,
{
    let tip_timestamp = get_block_timestamp(tip_height).await?;
    if target_timestamp > now.max(tip_timestamp) {
        return Err(format!(
            "the sync start date {target_timestamp} is in the future (current time {now}, chain tip block time {tip_timestamp})"
        ));
    }

    let mut low = 0u64;
    let mut high = tip_height;
    let mut resolved_height = 0u64;

    while low <= high {
        let mid = low + (high - low) / 2;
        let block_timestamp = get_block_timestamp(mid).await?;
        if block_timestamp <= target_timestamp {
            resolved_height = mid;
            low = mid + 1;
        } else if mid == 0 {
            // A date before block 0: `saturating_sub` would leave the window at 0..=0 forever.
            break;
        } else {
            high = mid - 1;
        }
    }

    Ok(resolved_height)
}

// Docs vs reference (CRD ch.39 R39.10.6 "start-point resolution", R39.8.0g): the reference resolves a
// `{"date": T}` start with a block-time estimate from the coin config's `avg_blocktime`, and without
// `sync_params` it starts roughly one day before the tip. Reloaded resolves a date against the backend's
// block timestamps instead, because the estimate drifts from the real chain. Only the value of
// `first_sync_block.requested` differs; the wire shapes are the same.
async fn requested_shielded_scan_start_height(
    coin: &ZCoin,
    ticker: &str,
    sync_params: Option<&ZcoinSyncParams>,
) -> Result<Option<u64>, ZcoinInitError> {
    match sync_params {
        None => Ok(None),
        Some(ZcoinSyncParams::Height(height)) => Ok(Some(*height)),
        Some(ZcoinSyncParams::Earliest) => Ok(Some(coin.sapling_activation_height())),
        Some(ZcoinSyncParams::Date(target_timestamp)) => {
            // R39.10.11: a date in the future ends activation with `CoinCreationError`.
            let tip_height = coin
                .current_block()
                .compat()
                .await
                .map_err(ZcoinInitError::CouldNotGetBlockCount)?;
            let resolved_height = resolve_requested_shielded_scan_start_height(
                *target_timestamp,
                tip_height,
                now_ms() / 1_000,
                |height| async move {
                    coin.rpc_client()
                        .get_block_timestamp(height)
                        .await
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .map_err(|error| ZcoinInitError::CouldNotResolveSyncStartDate {
                ticker: ticker.to_owned(),
                sync_start: target_timestamp.to_string(),
                error,
            })?;
            Ok(Some(resolved_height))
        },
    }
}

/// Builds the [`FirstSyncBlock`] report from a resolved start height and the
/// coin's Sapling activation height (R39.8.0h). `actual` floors `requested` at
/// Sapling activation, the lowest height at which shielded outputs can exist.
fn first_sync_block(requested: u64, sapling_activation_height: u64) -> FirstSyncBlock {
    FirstSyncBlock {
        requested,
        is_pre_sapling: requested < sapling_activation_height,
        actual: requested.max(sapling_activation_height),
    }
}

#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ZcoinInitError {
    #[display(fmt = "Error on coin {} creation: {}", ticker, error)]
    CoinCreationError {
        ticker: String,
        error: String,
    },
    CoinIsAlreadyActivated {
        ticker: String,
    },
    #[display(fmt = "Hardware wallets are not supported yet for {}", ticker)]
    HardwareWalletsAreNotSupportedYet {
        ticker: String,
    },
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CouldNotGetBalance(String),
    CouldNotGetBlockCount(String),
    #[display(fmt = "Could not resolve sync start date {}: {}", sync_start, error)]
    CouldNotResolveSyncStartDate {
        ticker: String,
        sync_start: String,
        error: String,
    },
    #[display(
        fmt = "Shielded wallet DB scanner did not complete for {} through activation tip {}: {}",
        ticker,
        activation_tip,
        error
    )]
    ShieldedWalletDbScanIncomplete {
        ticker: String,
        activation_tip: u64,
        error: String,
    },
    /// Every configured lightwalletd endpoint failed the server-information read (R39.11.6).
    #[display(fmt = "No lightwalletd endpoint of {} is reachable: {}", ticker, error)]
    LightwalletdUnreachable {
        ticker: String,
        error: String,
    },
    /// Endpoints answered, and admission excluded every one of them as serving another chain or
    /// branch (R39.11.6, R39.11.7a). Distinct from [`ZcoinInitError::LightwalletdUnreachable`].
    #[display(
        fmt = "No lightwalletd endpoint of {} passed admission (wrong chain or branch, or stale): {}",
        ticker,
        error
    )]
    LightwalletdExcluded {
        ticker: String,
        error: String,
    },
    Internal(String),
}

impl ZcoinInitError {
    #[cfg(not(target_arch = "wasm32"))]
    fn from_lightwalletd(ticker: &str, error: LightwalletdError) -> Self {
        let ticker = ticker.to_owned();
        match error {
            LightwalletdError::AllUnreachable(error) => ZcoinInitError::LightwalletdUnreachable { ticker, error },
            LightwalletdError::AllExcluded(error) => ZcoinInitError::LightwalletdExcluded { ticker, error },
            // The whole message: the bare payload (`37a5165b x1`) reads as a chain verdict, not as
            // "this branch is reported by one endpoint only".
            no_majority @ LightwalletdError::NoBranchMajority(_) => ZcoinInitError::LightwalletdExcluded {
                ticker,
                error: no_majority.to_string(),
            },
            LightwalletdError::NoEndpoints => ZcoinInitError::CoinCreationError {
                ticker,
                error: "`light_wallet_d_servers` must list at least one lightwalletd endpoint".to_owned(),
            },
            other => ZcoinInitError::Internal(other.to_string()),
        }
    }

    /// Maps a coin-build failure to the task error.
    ///
    /// Docs vs reference (CRD ch.39 R39.10.12): the public page binds the content of the error object
    /// for missing Sapling parameters, `error_data.error == "ZCashParamsNotFound"`, and shows that error
    /// once as a bare object and once inside a `status` `Error`. The reference only ever emits it inside
    /// a `status` `Error`. Reloaded follows the reference for the placement (it is a task-level
    /// `CoinCreationError`) and the docs for the content, so clients that match on the token keep
    /// working. The descriptive text with the path goes to the log.
    pub fn from_build_err(build_err: ZCoinBuildError, ticker: String) -> Self {
        let error = match build_err {
            ZCoinBuildError::ZCashParamsNotFound | ZCoinBuildError::ZCashParamsDirNotFound { .. } => {
                log::warn!("Cannot activate {}: {}", ticker, build_err);
                "ZCashParamsNotFound".to_owned()
            },
            other => other.to_string(),
        };
        ZcoinInitError::CoinCreationError { ticker, error }
    }
}

impl From<BalanceError> for ZcoinInitError {
    fn from(err: BalanceError) -> Self { ZcoinInitError::CouldNotGetBalance(err.to_string()) }
}

impl From<RegisterCoinError> for ZcoinInitError {
    fn from(reg_err: RegisterCoinError) -> ZcoinInitError {
        match reg_err {
            RegisterCoinError::CoinIsInitializedAlready { coin } => {
                ZcoinInitError::CoinIsAlreadyActivated { ticker: coin }
            },
            RegisterCoinError::Internal(internal) => ZcoinInitError::Internal(internal),
        }
    }
}

impl From<RpcTaskError> for ZcoinInitError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::Timeout(duration) => ZcoinInitError::TaskTimedOut { duration },
            internal_error => ZcoinInitError::Internal(internal_error.to_string()),
        }
    }
}

impl From<CryptoInitError> for ZcoinInitError {
    fn from(err: CryptoInitError) -> Self { ZcoinInitError::Internal(err.to_string()) }
}

impl From<CryptoCtxError> for ZcoinInitError {
    fn from(err: CryptoCtxError) -> Self { ZcoinInitError::Internal(err.to_string()) }
}

impl From<ZcoinInitError> for InitStandaloneCoinError {
    fn from(err: ZcoinInitError) -> Self {
        match err.clone() {
            ZcoinInitError::CoinCreationError { ticker, error } => {
                InitStandaloneCoinError::CoinCreationError { ticker, error }
            },
            ZcoinInitError::CoinIsAlreadyActivated { ticker } => {
                InitStandaloneCoinError::CoinIsAlreadyActivated { ticker }
            },
            ZcoinInitError::HardwareWalletsAreNotSupportedYet { ticker } => {
                InitStandaloneCoinError::CoinCreationError {
                    ticker,
                    error: "Hardware wallets are not supported yet".into(),
                }
            },
            ZcoinInitError::TaskTimedOut { duration } => InitStandaloneCoinError::TaskTimedOut { duration },
            ZcoinInitError::LightwalletdUnreachable { .. } => InitStandaloneCoinError::Transport(err.to_string()),
            ZcoinInitError::LightwalletdExcluded { ticker, error } => InitStandaloneCoinError::CoinCreationError {
                ticker,
                error: format!(
                    "No lightwalletd endpoint passed admission (wrong chain or branch, or stale): {}",
                    error
                ),
            },
            // The coin was built and the shielded sync finished, but the transparent backend could not
            // deliver the height or the balance (CRD ch.39 R39.10.11).
            ZcoinInitError::CouldNotGetBalance(e) | ZcoinInitError::CouldNotGetBlockCount(e) => {
                InitStandaloneCoinError::Transport(e)
            },
            ZcoinInitError::CouldNotResolveSyncStartDate {
                ticker,
                sync_start,
                error,
            } => InitStandaloneCoinError::CoinCreationError {
                ticker,
                error: format!("Could not resolve sync start date {}: {}", sync_start, error),
            },
            // Docs vs reference (CRD ch.39 R39.10.13): the public error catalogue lists
            // `HardwareWalletsAreNotSupportedYet`, `CouldNotGetBalance` and `CouldNotGetBlockCount` as
            // Z-coin errors. The reference never puts those names on the wire, and neither does
            // Reloaded: they are translated into the task-level discriminants here.
            ZcoinInitError::ShieldedWalletDbScanIncomplete { error: e, .. } | ZcoinInitError::Internal(e) => {
                InitStandaloneCoinError::Internal(e)
            },
        }
    }
}

/// Waits for the commitment-tree sync loop to report that it has caught up.
///
/// While the loop cannot read from its backend (the full-node daemon in Native mode, the Electrum
/// connection in Light mode) the failure is shown as a retriable `TemporaryError` and the wait goes on
/// (CRD ch.39 R39.10.14). A loop that ended without ever reporting completion is a dead sync process and
/// ends the task with `Internal`.
async fn wait_for_sapling_state_sync(coin: &ZCoin, task_handle: &ZcoinRpcTaskHandle) -> MmResult<(), ZcoinInitError> {
    let mut reported_error: Option<String> = None;
    loop {
        // The loop ends right after it reports completion, so "stopped" must be read before "synced":
        // a stopped loop that is not synced on the second read really never completed.
        let stopped = coin.sapling_loop_stopped();
        if coin.is_sapling_state_synced() {
            return Ok(());
        }
        if stopped {
            return MmError::err(ZcoinInitError::Internal(
                "The shielded sync process stopped before it reported completion".to_owned(),
            ));
        }

        let current_error = coin.sapling_loop_error();
        if current_error != reported_error {
            let status = match &current_error {
                Some(message) => ZcoinInProgressStatus::TemporaryError(message.clone()),
                None => ZcoinInProgressStatus::ActivatingCoin,
            };
            task_handle.update_in_progress_status(status).mm_err(Into::into)?;
            reported_error = current_error;
        }
        Timer::sleep(1.).await;
    }
}

/// Runs the Light-mode shielded sync until it succeeds.
///
/// A failure before the sync started (no lightwalletd endpoint can be connected, the endpoint list is
/// empty, the initial chain-state query fails, the local storage cannot be read) ends the task with
/// `CoinCreationError`. A failure after it started is reported as a `TemporaryError` in-progress value
/// and the sync is retried after a pause, without any cap: the task ends only on success or on cancel
/// (CRD ch.39 R39.10.14, maintainer decision: follow the reference). Once a failure was reported the
/// sync counts as started, so later failures never turn terminal.
#[cfg(not(target_arch = "wasm32"))]
async fn run_shielded_sync_with_retry<Attempt, AttemptFut, Report, Pause, PauseFut>(
    ticker: &str,
    mut attempt: Attempt,
    mut report_temporary_error: Report,
    mut pause: Pause,
) -> MmResult<(), ZcoinInitError>
where
    Attempt: FnMut() -> AttemptFut,
    AttemptFut: Future<Output = Result<(), ShieldedFetchError>>,
    Report: FnMut(String) -> MmResult<(), ZcoinInitError>,
    Pause: FnMut() -> PauseFut,
    PauseFut: Future<Output = ()>,
{
    let mut sync_started = false;
    loop {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(ShieldedFetchError::BeforeSync(error)) if !sync_started => {
                return MmError::err(ZcoinInitError::CoinCreationError {
                    ticker: ticker.to_owned(),
                    error,
                });
            },
            Err(error) => {
                sync_started = true;
                log::warn!("ZCoin shielded sync for {} failed, retrying: {}", ticker, error);
                report_temporary_error(error.to_string())?;
                pause().await;
            },
        }
    }
}

impl TryFromCoinProtocol for ZcoinProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::ZHTLC(info) => Ok(info),
            protocol => MmError::err(protocol),
        }
    }
}

#[async_trait]
impl InitStandaloneCoinActivationOps for ZCoin {
    type ActivationRequest = ZcoinActivationParams;
    type StandaloneProtocol = ZcoinProtocolInfo;
    type ActivationResult = ZcoinActivationResult;
    type ActivationError = ZcoinInitError;
    type InProgressStatus = ZcoinInProgressStatus;
    type AwaitingStatus = ZcoinAwaitingStatus;
    type UserAction = ZcoinUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &ZcoinTaskManagerShared {
        &activation_ctx.init_z_coin_task_manager
    }

    async fn acquire_activation_guard(
        ctx: MmArc,
        ticker: &str,
    ) -> Result<Option<Box<dyn Send>>, MmError<Self::ActivationError>> {
        let activation_ctx = CoinsActivationContext::from_ctx(&ctx).map_to_mm(ZcoinInitError::Internal)?;
        let lock = activation_ctx
            .z_coin_activation_lock(ticker)
            .map_to_mm(ZcoinInitError::Internal)?;
        Ok(Some(Box::new(lock.lock_owned().await)))
    }

    async fn init_standalone_coin(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: &ZcoinActivationParams,
        mut protocol_info: ZcoinProtocolInfo,
        task_handle: &ZcoinRpcTaskHandle,
    ) -> MmResult<Self, ZcoinInitError> {
        if SHIELDED_WALLET_UNAVAILABLE {
            return MmError::err(shielded_unavailable_error(&ticker));
        }
        let backend = zcoin_backend(&ticker, &activation_request.mode)?;
        let utxo_params = UtxoActivationParams {
            // Not consulted: the RPC client of a shielded coin is chosen by `backend`, so no Electrum
            // connection can be built from this value (R39.11.1, R39.11.5).
            mode: UtxoRpcMode::Native,
            utxo_merge_params: None,
            tx_history: false,
            required_confirmations: activation_request.required_confirmations,
            requires_notarization: activation_request.requires_notarization,
            address_format: None,
            gap_limit: None,
            min_addresses_number: None,
            scan_policy: Default::default(),
            priv_key_policy: PrivKeyActivationPolicy::IguanaPrivKey,
            check_utxo_maturity: None,
            path_to_address: None,
        };

        // Wire sync parameters from RPC request into protocol_info (R39.6.2)
        if let Some(blocks_per_iter) = activation_request.blocks_per_iteration {
            protocol_info.blocks_per_iteration = blocks_per_iter.get();
        }
        if let Some(inter_iter_ms) = activation_request.inter_iteration_interval_ms {
            protocol_info.inter_iteration_interval_ms = inter_iter_ms;
        }
        let scan_blocks_per_iteration = protocol_info.blocks_per_iteration.max(1);
        let scan_interval_ms = protocol_info.inter_iteration_interval_ms;
        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(Into::into)?;
        let priv_key = crypto_ctx.mm2_internal_privkey_secret();
        let coin = z_coin_from_conf_and_params(
            &ctx,
            &ticker,
            &coin_conf,
            &utxo_params,
            priv_key.as_slice(),
            activation_request.account.unwrap_or(0),
            // The Sapling parameter directory is a native-build setting; it is ignored on WASM
            // (CRD ch.39 R39.10.4).
            #[cfg(not(target_arch = "wasm32"))]
            activation_request
                .zcash_params_path
                .as_ref()
                .map(std::path::PathBuf::from),
            protocol_info,
            backend,
        )
        .await
        .mm_err(|e| ZcoinInitError::from_build_err(e, ticker.clone()))?;

        // Admission before anything reads the chain (R39.11.6, R39.11.7a): an unreachable set and an
        // excluded set are different failures and neither reports a placeholder tip.
        #[cfg(not(target_arch = "wasm32"))]
        if coin.is_light() {
            coin.admitted_lightwalletd_servers()
                .await
                .map_to_mm(|e| ZcoinInitError::from_lightwalletd(&ticker, e))?;
        }

        let requested_start_height =
            requested_shielded_scan_start_height(&coin, &ticker, activation_request.sync_params()).await?;
        // Docs vs reference (CRD ch.39 R39.10.6, code-quality finding): the documented intent of
        // `skip_sync_params` is "resume from the last synced block; use `sync_params` only when nothing
        // was synced yet". The reference still rewinds and rescans from `sync_params` when the flag is
        // set and a local block cache exists, so it honours the flag only for an empty cache. Reloaded
        // implements the documented intent (R39.6.2) and does not copy that behavior.
        let skip_sync_params = activation_request.skip_sync_params();
        log::info!(
            "ZCoin shielded sync settings for {}: sync_params={:?}, resolved_start_height={:?}, skip_sync_params={}, scan_blocks_per_iteration={}, scan_interval_ms={}",
            ticker,
            activation_request.sync_params(),
            requested_start_height,
            skip_sync_params,
            scan_blocks_per_iteration,
            scan_interval_ms
        );

        wait_for_sapling_state_sync(&coin, task_handle).await?;
        // The shielded wallet scan below needs the tip; WASM has no wallet scan, so there it only proves
        // that the transparent backend answers.
        #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
        let activation_tip = coin
            .current_block()
            .compat()
            .await
            .map_to_mm(ZcoinInitError::CouldNotGetBlockCount)?;

        // Native mode scans the wallet database once, up to the activation tip. A scan failure there is a
        // local one, not a lightwalletd outage, so it ends the task.
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(activation_request.mode, ZcoinRpcMode::Native) {
            let build_progress = |current_scanned_block: u64, latest_block: u64| {
                let _ = task_handle.update_in_progress_status(ZcoinInProgressStatus::BuildingWalletDb {
                    current_scanned_block,
                    latest_block,
                });
            };
            coin.scan_shielded_wallet_db_to_height(activation_tip, build_progress)
                .map_err(|error| ZcoinInitError::ShieldedWalletDbScanIncomplete {
                    ticker: ticker.clone(),
                    activation_tip,
                    error,
                })?;
        }

        // Light mode: download compact blocks from lightwalletd, then scan them into the wallet
        // database. The shielded wallet database exists on native builds only, so on WASM the
        // commitment-tree sync above is the whole shielded sync.
        #[cfg(not(target_arch = "wasm32"))]
        if coin.is_light() {
            run_shielded_sync_with_retry(
                &ticker,
                || async {
                    // Phase 1 - `UpdatingBlocksCache`: download compact blocks from lightwalletd,
                    // reporting incremental progress (R39.3.1).
                    let cache_progress = |current_scanned_block: u64, latest_block: u64| {
                        let _ = task_handle.update_in_progress_status(ZcoinInProgressStatus::UpdatingBlocksCache {
                            current_scanned_block,
                            latest_block,
                        });
                    };
                    coin.fetch_lightwalletd_compact_blocks_to_height(
                        activation_tip,
                        requested_start_height,
                        skip_sync_params,
                        &cache_progress,
                    )
                    .await?;

                    // Phase 2 - `BuildingWalletDb`: scan cached blocks into the wallet DB, reporting
                    // incremental progress (R39.3.1).
                    let build_progress = |current_scanned_block: u64, latest_block: u64| {
                        let _ = task_handle.update_in_progress_status(ZcoinInProgressStatus::BuildingWalletDb {
                            current_scanned_block,
                            latest_block,
                        });
                    };
                    coin.scan_shielded_wallet_db_to_height(activation_tip, build_progress)
                        .map(|_| ())
                        .map_err(ShieldedFetchError::DuringSync)
                },
                |message| {
                    task_handle
                        .update_in_progress_status(ZcoinInProgressStatus::TemporaryError(message))
                        .mm_err(Into::into)
                },
                || Timer::sleep(SHIELDED_SYNC_RETRY_PAUSE_SECS),
            )
            .await?;

            // The scan above is a one-shot up to the activation tip. Light mode has no other component
            // advancing the wallet DB afterwards, so without this the balance would stay frozen at the
            // activation height until the coin is activated again.
            coin.spawn_post_activation_shielded_sync();
        }

        Ok(coin)
    }

    async fn get_activation_result(
        &self,
        _ctx: MmArc,
        task_handle: &ZcoinRpcTaskHandle,
        activation_request: &Self::ActivationRequest,
    ) -> MmResult<Self::ActivationResult, ZcoinInitError> {
        // Unreachable on WASM (the coin is never built there); kept so that no zero balance can be reported.
        if SHIELDED_WALLET_UNAVAILABLE {
            return MmError::err(shielded_unavailable_error(self.ticker()));
        }
        task_handle
            .update_in_progress_status(ZcoinInProgressStatus::RequestingWalletBalance)
            .mm_err(Into::into)?;
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(ZcoinInitError::CouldNotGetBlockCount)?;

        // Expose the resolved shielded sync start point (R39.8.0h), emitted
        // unconditionally to match the dictated result. When the caller supplied
        // a `sync_params` it is reported directly (height requests resolve
        // without RPC; date requests resolve deterministically against the same
        // backend used during scan); otherwise it falls back to the height the
        // shielded scan was actually anchored at, then to Sapling activation.
        let requested_start_height =
            requested_shielded_scan_start_height(self, self.ticker(), activation_request.sync_params()).await?;
        let effective_start_height = requested_start_height
            .or_else(|| self.shielded_wallet_sync_start_height())
            .unwrap_or_else(|| self.sapling_activation_height());
        let first_sync_block = first_sync_block(effective_start_height, self.sapling_activation_height());

        let balance = self.my_balance().compat().await.mm_err(Into::into)?;
        // Docs vs reference (CRD ch.39 R39.10.9): the public `get_balances` remark says the structure of the
        // result may vary. The reference ignores `get_balances` for this coin and always reports the
        // single-address form with `"wallet_type": "Iguana"`, also when the shielded key is HD-derived.
        // Reloaded does the same: there is no per-account HD report and no ticker-keyed balance map.
        Ok(ZcoinActivationResult {
            ticker: self.ticker().to_owned(),
            current_block,
            wallet_balance: EnableCoinBalance::Iguana(IguanaWalletBalance {
                address: self.my_z_address_encoded(),
                balance,
            }),
            first_sync_block,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coins::CoinBalance;
    use serde_json::json;

    #[test]
    fn zcoin_sync_params_match_the_dictated_wire_shapes() {
        // `{"height": N}` / `{"date": N}` / `"earliest"`, exactly as sent by
        // KDF-family wallets in `mode.rpc_data.sync_params`.
        assert_eq!(
            serde_json::from_value::<ZcoinSyncParams>(json!({ "height": 3_785_000 })).unwrap(),
            ZcoinSyncParams::Height(3_785_000)
        );
        assert_eq!(
            serde_json::from_value::<ZcoinSyncParams>(json!({ "date": 1_775_037_600u64 })).unwrap(),
            ZcoinSyncParams::Date(1_775_037_600)
        );
        assert_eq!(
            serde_json::from_value::<ZcoinSyncParams>(json!("earliest")).unwrap(),
            ZcoinSyncParams::Earliest
        );
    }

    #[test]
    fn activation_params_parse_the_desktop_wallet_wire_format() {
        // Mirrors the exact request the desktop wallet emits: sync_params nested
        // in the Light rpc_data, plus scan_blocks_per_iteration / scan_interval
        // at the activation-params level.
        let params: ZcoinActivationParams = serde_json::from_value(json!({
            "mode": {
                "rpc": "Light",
                "rpc_data": {
                    "electrum_servers": [{ "url": "electrum1.example:10008" }],
                    "light_wallet_d_servers": ["https://lightd1.example:443"],
                    "sync_params": { "height": 3_785_000 }
                }
            },
            "scan_blocks_per_iteration": 5000,
            "scan_interval": 0
        }))
        .unwrap();

        assert_eq!(params.blocks_per_iteration.map(NonZeroU32::get), Some(5000));
        assert_eq!(params.inter_iteration_interval_ms, Some(0));
        assert_eq!(params.sync_params(), Some(&ZcoinSyncParams::Height(3_785_000)));
    }

    #[test]
    fn activation_params_without_sync_params_yield_none() {
        let params: ZcoinActivationParams = serde_json::from_value(json!({
            "mode": {
                "rpc": "Light",
                "rpc_data": {
                    "electrum_servers": [{ "url": "electrum1.example:10008" }],
                    "light_wallet_d_servers": ["https://lightd1.example:443"]
                }
            }
        }))
        .unwrap();

        assert_eq!(params.sync_params(), None);
        assert_eq!(params.blocks_per_iteration, None);
    }

    #[test]
    fn resolve_requested_shielded_scan_start_height_finds_the_last_block_before_the_target_timestamp() {
        let resolved = futures::executor::block_on(resolve_requested_shielded_scan_start_height(
            350,
            3,
            1_000,
            |height| async move {
                let timestamps = [100u64, 200u64, 300u64, 400u64];
                Ok::<u64, String>(timestamps[height as usize])
            },
        ))
        .unwrap();

        assert_eq!(resolved, 2);
    }

    fn resolve_date(target: u64, now: u64) -> Result<u64, String> {
        futures::executor::block_on(resolve_requested_shielded_scan_start_height(
            target,
            3,
            now,
            |height| async move {
                let timestamps = [100u64, 200u64, 300u64, 400u64];
                Ok::<u64, String>(timestamps[height as usize])
            },
        ))
    }

    #[test]
    fn sync_date_beyond_the_clock_and_the_tip_is_rejected_not_clamped() {
        // R39.10.11: tip time 400, clock 1_000, date 1_001.
        let error = resolve_date(1_001, 1_000).unwrap_err();
        assert!(error.contains("in the future"), "{error}");
    }

    #[test]
    fn sync_date_between_the_tip_time_and_the_clock_resolves_to_the_tip() {
        // Deliberate window: later than the tip block (400) but not later than now (1_000).
        assert_eq!(resolve_date(700, 1_000), Ok(3));
        assert_eq!(resolve_date(1_000, 1_000), Ok(3));
    }

    #[test]
    fn sync_date_ahead_of_a_lagging_clock_but_covered_by_the_tip_block_resolves() {
        // Clock 300 is behind the tip block time 400; a date of 350 is not in the future.
        assert_eq!(resolve_date(350, 300), Ok(2));
        assert_eq!(resolve_date(400, 300), Ok(3));
        assert!(resolve_date(401, 300).is_err());
    }

    #[test]
    fn sync_date_at_the_tip_time_and_before_genesis_still_resolve() {
        assert_eq!(resolve_date(400, 1_000), Ok(3));
        assert_eq!(resolve_date(50, 1_000), Ok(0));
    }

    #[test]
    fn first_sync_block_reports_requested_at_or_above_sapling() {
        let block = first_sync_block(5_000, 1_000);
        assert_eq!(block.requested, 5_000);
        assert!(!block.is_pre_sapling);
        assert_eq!(block.actual, 5_000);
    }

    #[test]
    fn first_sync_block_floors_pre_sapling_request_at_activation() {
        let block = first_sync_block(500, 1_000);
        assert_eq!(block.requested, 500);
        assert!(block.is_pre_sapling);
        assert_eq!(block.actual, 1_000);
    }

    #[test]
    fn zcoin_scan_progress_status_wire_shape_is_stable() {
        assert_eq!(
            serde_json::to_value(ZcoinInProgressStatus::UpdatingBlocksCache {
                current_scanned_block: 1_234,
                latest_block: 5_678,
            })
            .unwrap(),
            json!({
                "UpdatingBlocksCache": {
                    "current_scanned_block": 1_234,
                    "latest_block": 5_678,
                }
            })
        );
        assert_eq!(
            serde_json::to_value(ZcoinInProgressStatus::BuildingWalletDb {
                current_scanned_block: 2_345,
                latest_block: 5_678,
            })
            .unwrap(),
            json!({
                "BuildingWalletDb": {
                    "current_scanned_block": 2_345,
                    "latest_block": 5_678,
                }
            })
        );
    }

    fn lightd_light_mode_json() -> serde_json::Value {
        json!({
            "rpc": "Light",
            "rpc_data": {
                "electrum_servers": [{ "url": "electrum1.example:10008" }],
                "light_wallet_d_servers": ["https://lightd1.example:443"]
            }
        })
    }

    #[test]
    fn activation_params_without_mode_are_light_with_empty_server_lists() {
        // CRD ch.39 R39.10.4 / T39.10.2 (a): a request without `mode` parses and ends later with
        // `CoinCreationError`.
        let params: ZcoinActivationParams = serde_json::from_value(json!({})).unwrap();
        match params.mode {
            ZcoinRpcMode::Light {
                electrum_servers,
                light_wallet_d_servers,
                min_connected,
                max_connected,
                ..
            } => {
                assert!(electrum_servers.is_empty());
                assert!(light_wallet_d_servers.is_empty());
                assert_eq!((min_connected, max_connected), (None, None));
            },
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinRpcMode::Native => panic!("a missing mode must behave as Light mode"),
        }
    }

    #[test]
    fn electrum_is_a_synonym_of_light_and_servers_of_electrum_servers() {
        // T39.10.2 (b), (c) and (d).
        let params: ZcoinActivationParams = serde_json::from_value(json!({
            "mode": {
                "rpc": "Electrum",
                "rpc_data": {
                    "servers": [{ "url": "electrum1.example:10008" }, { "url": "electrum2.example:10008" }],
                    "min_connected": 2,
                    "max_connected": 5,
                    "light_wallet_d_servers": ["https://lightd1.example:443"]
                }
            }
        }))
        .unwrap();
        match params.mode {
            ZcoinRpcMode::Light {
                electrum_servers,
                min_connected,
                max_connected,
                ..
            } => {
                assert_eq!(electrum_servers.len(), 2);
                assert_eq!(min_connected, Some(2));
                assert_eq!(max_connected, Some(5));
            },
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinRpcMode::Native => panic!("`Electrum` must parse as Light mode"),
        }
    }

    /// T39.11.b / R39.11.5: `light_wallet_d_servers` is mandatory and non-empty; `electrum_servers`
    /// is neither. The retired form of this test required both lists.
    #[test]
    fn light_mode_requires_only_the_lightwalletd_list() {
        // Absent: a parse failure, as before.
        let missing_lightd = json!({ "mode": { "rpc": "Light", "rpc_data": {
            "electrum_servers": [{ "url": "electrum1.example:10008" }]
        } } });
        assert!(serde_json::from_value::<ZcoinActivationParams>(missing_lightd).is_err());

        // Empty: a configuration error that names the coin.
        let empty_lightd = json!({ "mode": { "rpc": "Light", "rpc_data": { "light_wallet_d_servers": [] } } });
        let params: ZcoinActivationParams = serde_json::from_value(empty_lightd).unwrap();
        match zcoin_backend("ARRR", &params.mode) {
            Err(ZcoinInitError::CoinCreationError { ticker, error }) => {
                assert_eq!(ticker, "ARRR");
                assert!(error.contains("light_wallet_d_servers"), "{error}");
            },
            _ => panic!("an empty lightwalletd list must be a configuration error"),
        }
    }

    /// T39.11.b / R39.11.5, R39.11.7: absent, empty and populated (unreachable) `electrum_servers`
    /// all activate identically, because the field is accepted and ignored.
    #[test]
    fn electrum_servers_are_accepted_and_have_no_effect() {
        let lightd = json!(["https://lightd1.example:443", "https://lightd2.example:443"]);
        let variants = [
            json!({ "light_wallet_d_servers": lightd }),
            json!({ "electrum_servers": [], "light_wallet_d_servers": lightd }),
            json!({
                "electrum_servers": [{ "url": "unreachable.invalid:10008" }, { "url": "also-unreachable.invalid:10008" }],
                "min_connected": 3,
                "max_connected": 9,
                "light_wallet_d_servers": lightd
            }),
        ];
        let mut backends = Vec::new();
        for rpc_data in variants {
            let params: ZcoinActivationParams =
                serde_json::from_value(json!({ "mode": { "rpc": "Light", "rpc_data": rpc_data } })).unwrap();
            match zcoin_backend("ARRR", &params.mode).ok().unwrap() {
                ZcoinBackend::Light { light_wallet_d_servers } => backends.push(light_wallet_d_servers),
                #[cfg(not(target_arch = "wasm32"))]
                ZcoinBackend::Native => panic!("Light mode must select the lightwalletd backend"),
            }
        }
        assert_eq!(backends[0], vec![
            "https://lightd1.example:443",
            "https://lightd2.example:443"
        ]);
        assert!(backends.iter().all(|servers| servers == &backends[0]));
    }

    /// T39.11.b / R39.11.6: an unreachable endpoint set and an excluded one are different failures.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unreachable_and_excluded_endpoint_sets_are_distinguishable_activation_errors() {
        let unreachable =
            ZcoinInitError::from_lightwalletd("ARRR", LightwalletdError::AllUnreachable("a: refused".to_owned()));
        let excluded =
            ZcoinInitError::from_lightwalletd("ARRR", LightwalletdError::AllExcluded("a: other branch".to_owned()));
        let no_majority =
            ZcoinInitError::from_lightwalletd("ARRR", LightwalletdError::NoBranchMajority("37a5165b x1".to_owned()));
        assert!(matches!(unreachable, ZcoinInitError::LightwalletdUnreachable { .. }));
        assert!(matches!(excluded, ZcoinInitError::LightwalletdExcluded { .. }));
        match &no_majority {
            ZcoinInitError::LightwalletdExcluded { error, .. } => {
                // The branch is the healthy one; the endpoint is merely alone, and the message must
                // not say it is off the coin's chain.
                assert!(
                    error.contains("37a5165b x1") && error.contains("fewer than two"),
                    "{error}"
                );
            },
            _ => panic!("expected LightwalletdExcluded"),
        }
        assert!(!no_majority.to_string().contains("on this coin's chain"));
        // Neither carries a tip.
        assert!(matches!(
            InitStandaloneCoinError::from(unreachable),
            InitStandaloneCoinError::Transport(_)
        ));
        assert!(matches!(
            InitStandaloneCoinError::from(excluded),
            InitStandaloneCoinError::CoinCreationError { .. }
        ));
    }

    #[test]
    fn zero_scan_blocks_per_iteration_is_a_parse_failure() {
        // T39.10.2 (e): the value 0 is not silently clamped.
        for name in ["scan_blocks_per_iteration", "blocks_per_iteration"] {
            let request = json!({ "mode": lightd_light_mode_json(), name: 0 });
            assert!(
                serde_json::from_value::<ZcoinActivationParams>(request).is_err(),
                "{name}: 0 must not parse"
            );
        }
        let params: ZcoinActivationParams =
            serde_json::from_value(json!({ "mode": lightd_light_mode_json(), "scan_blocks_per_iteration": 1 }))
                .unwrap();
        assert_eq!(params.blocks_per_iteration.map(NonZeroU32::get), Some(1));
    }

    #[test]
    fn shielded_wallet_availability_matches_the_build_target() {
        // Recorded deviation (CRD ch.39 R39.10.1): WASM activation ends with CoinCreationError, native is
        // unaffected.
        assert_eq!(SHIELDED_WALLET_UNAVAILABLE, cfg!(target_arch = "wasm32"));
        let (wire, _) = task_error(shielded_unavailable_error("ARRR"));
        assert_eq!(wire["error_type"], "CoinCreationError");
        assert_eq!(wire["error_data"]["ticker"], "ARRR");
        assert!(wire["error_data"]["error"]
            .as_str()
            .unwrap()
            .contains("not available in the WebAssembly build"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_mode_parses_on_native_builds() {
        let params: ZcoinActivationParams = serde_json::from_value(json!({ "mode": { "rpc": "Native" } })).unwrap();
        assert!(matches!(params.mode, ZcoinRpcMode::Native));
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn native_mode_is_an_unknown_variant_on_wasm() {
        // T39.10.2 (f).
        assert!(serde_json::from_value::<ZcoinActivationParams>(json!({ "mode": { "rpc": "Native" } })).is_err());
    }

    #[test]
    fn members_without_zcoin_meaning_are_ignored() {
        // T39.10.2 (g): `get_balances`, `tx_history`, an Electrum `ws_url` and unknown members.
        let params: ZcoinActivationParams = serde_json::from_value(json!({
            "mode": { "rpc": "Light", "rpc_data": {
                "electrum_servers": [{ "url": "electrum1.example:10008", "ws_url": "wss://electrum1.example:30008" }],
                "light_wallet_d_servers": ["https://lightd1.example:443"]
            } },
            "get_balances": false,
            "tx_history": true,
            "gap_limit": 20,
            "scan_policy": "scan_if_new_wallet",
            "path_to_address": { "account_id": 0 },
            "min_addresses_number": 3,
            "something_else": [1, 2]
        }))
        .unwrap();
        assert!(!params.tx_history());
    }

    #[test]
    fn temporary_error_in_progress_status_wire_shape() {
        // T39.10.3.
        assert_eq!(
            serde_json::to_value(ZcoinInProgressStatus::TemporaryError("lightwalletd is down".to_owned())).unwrap(),
            json!({ "TemporaryError": "lightwalletd is down" })
        );
        assert_eq!(
            serde_json::to_value(ZcoinInProgressStatus::ActivatingCoin).unwrap(),
            json!("ActivatingCoin")
        );
        assert_eq!(
            serde_json::to_value(ZcoinInProgressStatus::RequestingWalletBalance).unwrap(),
            json!("RequestingWalletBalance")
        );
    }

    fn task_error(err: ZcoinInitError) -> (serde_json::Value, common::StatusCode) {
        use common::HttpStatusCode;
        let err: InitStandaloneCoinError = err.into();
        (serde_json::to_value(&err).unwrap(), err.status_code())
    }

    #[test]
    fn height_and_balance_failures_are_transport_errors_with_a_string() {
        // T39.10.4 (a).
        for err in [
            ZcoinInitError::CouldNotGetBlockCount("every lightwalletd endpoint is unreachable".to_owned()),
            ZcoinInitError::CouldNotGetBalance("balance read failed".to_owned()),
        ] {
            let (wire, status) = task_error(err);
            assert_eq!(wire["error_type"], "Transport");
            assert!(wire["error_data"].is_string());
            assert_eq!(status, common::StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    #[test]
    fn unresolvable_sync_date_is_a_coin_creation_error_with_the_requested_ticker() {
        // T39.10.4 (b).
        let (wire, status) = task_error(ZcoinInitError::CouldNotResolveSyncStartDate {
            ticker: "ARRR".to_owned(),
            sync_start: "1775037600".to_owned(),
            error: "no block timestamp".to_owned(),
        });
        assert_eq!(wire["error_type"], "CoinCreationError");
        assert_eq!(wire["error_data"]["ticker"], "ARRR");
        assert!(wire["error_data"]["error"].is_string());
        assert_eq!(wire["error_data"].as_object().map(|object| object.len()), Some(2));
        assert_eq!(status, common::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn missing_sapling_parameters_report_the_bare_token() {
        // T39.10.4 (d), R39.10.12.
        for build_err in [
            ZCoinBuildError::ZCashParamsNotFound,
            ZCoinBuildError::ZCashParamsDirNotFound {
                path: "/nonexistent/ZcashParams".to_owned(),
            },
        ] {
            let (wire, _) = task_error(ZcoinInitError::from_build_err(build_err, "ARRR".to_owned()));
            assert_eq!(wire["error_type"], "CoinCreationError");
            assert_eq!(wire["error_data"]["ticker"], "ARRR");
            assert_eq!(wire["error_data"]["error"], "ZCashParamsNotFound");
        }
        // Other build failures keep their human-readable text.
        let (wire, _) = task_error(ZcoinInitError::from_build_err(
            ZCoinBuildError::HdDerivationError("z_derivation_path is missing".to_owned()),
            "ARRR".to_owned(),
        ));
        assert_ne!(wire["error_data"]["error"], "ZCashParamsNotFound");
    }

    #[test]
    fn unsupported_key_policy_is_a_coin_creation_error_and_never_a_priv_key_error() {
        // T39.10.4 (e) and (f), R39.10.13.
        let (wire, _) = task_error(ZcoinInitError::HardwareWalletsAreNotSupportedYet {
            ticker: "ARRR".to_owned(),
        });
        assert_eq!(wire["error_type"], "CoinCreationError");
        assert_eq!(wire["error_data"]["ticker"], "ARRR");

        // The residual hardware-wallet class is an `Internal` string; neither it nor any other
        // activation error uses a name that R39.10.13 forbids.
        let forbidden = ["HwError", "PrivKeyNotAllowed", "UnexpectedDerivationMethod"];
        let errors = [
            ZcoinInitError::CoinCreationError {
                ticker: "ARRR".to_owned(),
                error: "e".to_owned(),
            },
            ZcoinInitError::CoinIsAlreadyActivated {
                ticker: "ARRR".to_owned(),
            },
            ZcoinInitError::HardwareWalletsAreNotSupportedYet {
                ticker: "ARRR".to_owned(),
            },
            ZcoinInitError::TaskTimedOut {
                duration: Duration::from_secs(1),
            },
            ZcoinInitError::CouldNotGetBalance("e".to_owned()),
            ZcoinInitError::CouldNotGetBlockCount("e".to_owned()),
            ZcoinInitError::CouldNotResolveSyncStartDate {
                ticker: "ARRR".to_owned(),
                sync_start: "1".to_owned(),
                error: "e".to_owned(),
            },
            ZcoinInitError::ShieldedWalletDbScanIncomplete {
                ticker: "ARRR".to_owned(),
                activation_tip: 1,
                error: "e".to_owned(),
            },
            ZcoinInitError::Internal("the wallet key context is not initialised".to_owned()),
        ];
        for err in errors {
            let (wire, _) = task_error(err);
            let name = wire["error_type"].as_str().unwrap().to_owned();
            assert!(!forbidden.contains(&name.as_str()), "{name} must not be observable");
            if name == "Internal" {
                assert!(wire["error_data"].is_string());
            }
        }
    }

    #[test]
    fn registration_race_and_timeout_keep_their_shapes() {
        let (wire, status) = task_error(ZcoinInitError::CoinIsAlreadyActivated {
            ticker: "ARRR".to_owned(),
        });
        assert_eq!(
            wire,
            json!({ "error_type": "CoinIsAlreadyActivated", "error_data": { "ticker": "ARRR" } })
        );
        assert_eq!(status, common::StatusCode::BAD_REQUEST);

        let (wire, status) = task_error(ZcoinInitError::TaskTimedOut {
            duration: Duration::from_secs(3),
        });
        assert_eq!(wire["error_type"], "TaskTimedOut");
        assert_eq!(status, common::StatusCode::REQUEST_TIMEOUT);
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod sync_retry {
        use super::*;
        use std::cell::{Cell, RefCell};

        /// Runs the retry loop against a scripted sequence of attempt outcomes, and returns the
        /// result with the reported temporary errors and the number of pauses taken.
        fn run(
            outcomes: Vec<Result<(), ShieldedFetchError>>,
        ) -> (MmResult<(), ZcoinInitError>, Vec<String>, usize, usize) {
            let outcomes = RefCell::new(outcomes.into_iter());
            let attempts = Cell::new(0usize);
            let reported = RefCell::new(Vec::new());
            let pauses = Cell::new(0usize);
            let result = futures::executor::block_on(run_shielded_sync_with_retry(
                "ARRR",
                || {
                    attempts.set(attempts.get() + 1);
                    let outcome = outcomes.borrow_mut().next().expect("the script ran out of outcomes");
                    async move { outcome }
                },
                |message| {
                    reported.borrow_mut().push(message);
                    Ok(())
                },
                || {
                    pauses.set(pauses.get() + 1);
                    async {}
                },
            ));
            (result, reported.into_inner(), pauses.get(), attempts.get())
        }

        #[test]
        fn an_outage_at_the_start_is_a_coin_creation_error() {
            // T39.10.4 (c).
            let (result, reported, pauses, attempts) = run(vec![Err(ShieldedFetchError::BeforeSync(
                "All lightwalletd servers failed: connection refused".to_owned(),
            ))]);
            match result.map_err(|e| e.into_inner()) {
                Err(ZcoinInitError::CoinCreationError { ticker, error }) => {
                    assert_eq!(ticker, "ARRR");
                    assert!(error.contains("connection refused"));
                },
                Err(other) => panic!("expected CoinCreationError, got {}", other),
                Ok(()) => panic!("expected CoinCreationError, got success"),
            }
            assert!(reported.is_empty());
            assert_eq!((pauses, attempts), (0, 1));
        }

        #[test]
        fn a_mid_sync_failure_is_reported_and_retried_until_the_backend_recovers() {
            // T39.10.5.
            let (result, reported, pauses, attempts) = run(vec![
                Err(ShieldedFetchError::DuringSync("batch 2 failed".to_owned())),
                Err(ShieldedFetchError::DuringSync("batch 2 failed again".to_owned())),
                Ok(()),
            ]);
            assert!(result.is_ok());
            assert_eq!(reported, vec![
                "batch 2 failed".to_owned(),
                "batch 2 failed again".to_owned()
            ]);
            assert_eq!((pauses, attempts), (2, 3));
        }

        #[test]
        fn once_the_sync_started_even_an_unreachable_backend_is_only_temporary() {
            let (result, reported, _, attempts) = run(vec![
                Err(ShieldedFetchError::DuringSync("scan failed".to_owned())),
                Err(ShieldedFetchError::BeforeSync("connection refused".to_owned())),
                Ok(()),
            ]);
            assert!(result.is_ok());
            assert_eq!(reported.len(), 2);
            assert_eq!(attempts, 3);
        }

        #[test]
        fn retries_have_no_cap() {
            // Maintainer decision: the sync never turns terminal until it recovers or is cancelled.
            let mut outcomes: Vec<_> = (0..500)
                .map(|n| Err(ShieldedFetchError::DuringSync(format!("failure {n}"))))
                .collect();
            outcomes.push(Ok(()));
            let (result, reported, pauses, attempts) = run(outcomes);
            assert!(result.is_ok());
            assert_eq!((reported.len(), pauses, attempts), (500, 500, 501));
        }
    }

    #[test]
    fn zcoin_activation_result_wire_shape_is_stable() {
        let result = ZcoinActivationResult {
            ticker: "ARRR".to_owned(),
            current_block: 5_678,
            wallet_balance: EnableCoinBalance::Iguana(IguanaWalletBalance {
                address: "zs-test-address".to_owned(),
                balance: CoinBalance::default(),
            }),
            first_sync_block: FirstSyncBlock {
                requested: 1_234,
                is_pre_sapling: false,
                actual: 1_234,
            },
        };

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({
                "ticker": "ARRR",
                "current_block": 5_678,
                "wallet_balance": {
                    "wallet_type": "Iguana",
                    "address": "zs-test-address",
                    "balance": {
                        "spendable": "0",
                        "unspendable": "0",
                    }
                },
                "first_sync_block": {
                    "requested": 1_234,
                    "is_pre_sapling": false,
                    "actual": 1_234,
                }
            })
        );
    }
}
