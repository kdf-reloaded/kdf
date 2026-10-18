use super::z_rpc::z_coin_grpc;
use super::{CheckPointBlockInfo, ShieldedFetchError, ZCoinBuildError, ZcoinConsensusParams, ZcoinDecryptionParams};
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use common::mm_number::BigDecimal;
use common::{calc_total_pages, log, PagingOptionsEnum};
use db_common::sqlite::rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use futures::StreamExt;
use lazy_static::lazy_static;
use mm2_err_handle::prelude::*;
use parking_lot::Mutex;
use prost_14::Message as ProstMessage;
use rand::rngs::OsRng;
use sapling::zip32::ExtendedFullViewingKey;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use zcash_client_backend::data_api::{chain::{error::Error as ChainScanError, scan_cached_blocks, BlockSource,
                                             ChainState},
                                     wallet::ConfirmationsPolicy,
                                     AccountBirthday, AccountPurpose, WalletCommitmentTrees, WalletRead, WalletWrite};
use zcash_client_backend::proto::compact_formats as zcash_compact;
use zcash_client_sqlite::{chain::init::init_cache_database, error::SqliteClientError, util::SystemClock,
                          wallet::init::init_wallet_db, BlockDb, WalletDb};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_primitives::block::BlockHash;
use zcash_primitives::merkle_tree::read_commitment_tree;
use zcash_protocol::consensus::{BlockHeight, NetworkUpgrade, Parameters};

const DEFAULT_LIGHT_WALLETD_RECENT_SCAN_BLOCKS: u64 = 2_880;
const LIGHTWALLETD_BLOCK_BATCH_SIZE: u64 = 500;
const LIGHTWALLETD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LIGHTWALLETD_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const LIGHTWALLETD_GRPC_SERVICE: &str = "pirate.wallet.sdk.rpc.CompactTxStreamer";
/// How far back the shielded wallet rewinds when it finds it has scanned a block
/// that is no longer on the chain.
///
/// A wallet that syncs to the chain tip records the tip block, and an ordinary
/// one-block reorg then leaves it holding a block the network has dropped.
///
/// The depth is one less than `PRUNING_DEPTH` (100) of `zcash_client_sqlite`, the
/// number of commitment-tree checkpoints the wallet database retains. Checkpoints
/// sit at distinct block heights at or below the scan tip, so a wallet holding all
/// 100 has its oldest at or below `tip - 99`: a rewind of this depth from the tip
/// always has a retained checkpoint at or below it to land on. That matters because
/// below the retained window the backend truncates to the block just before the
/// wallet's earliest note -- for a long-lived wallet, a rescan of its whole note
/// history -- or refuses; see [`ZCoinShieldedHistory::truncate_wallet_to_height`].
/// A depth of 100 lands exactly one block below the window of a wallet the
/// background sync keeps current (one checkpoint per block), so every routine
/// one-block reorg would pay that rescan.
///
/// Reorgs observed on ARRR were one block deep and Pirate is notarised, so its
/// reorgs are shallow; a rewind of this depth rescans at most 99 compact blocks,
/// which costs well under a second. A divergence the wallet cannot rewind past
/// goes to the rebuild path. The tests pin the 100-checkpoint window against the
/// real backend, so a dependency bump that changes it fails them.
const ZCOIN_REORG_REWIND_DEPTH: u64 = 99;

/// How many times the wallet rewinds before giving up and rebuilding at its sync
/// start.
///
/// Each attempt rewinds at most [`ZCOIN_REORG_REWIND_DEPTH`] blocks, and because
/// that spans the retained checkpoint window a divergence still present after this
/// many attempts is not a reorg any more; rebuilding is then both correct and
/// cheaper than walking back block by block. The loop is bounded by this count and
/// ends at once when a rewind cannot move below the divergence. Nothing is lost
/// either way -- every shielded note is recoverable from the chain with the
/// wallet's own viewing key.
const ZCOIN_MAX_REORG_REWINDS: u32 = 3;

/// Length of a Sapling commitment root, which a lightwalletd `TreeState` must never
/// carry in place of a serialized commitment tree. See
/// [`parse_checkpoint_commitment_tree`].
const SAPLING_COMMITMENT_ROOT_LEN: usize = 32;

pub(crate) type LightwalletdClient = z_coin_grpc::compact_tx_streamer_client::CompactTxStreamerClient<Channel>;
type ReloadedWalletDb = WalletDb<Connection, ZcoinConsensusParams, SystemClock, OsRng>;

/// The chain's view of the Sapling tree state at a given height.
///
/// [`ZCoinShieldedHistory::ensure_lightwalletd_chain_state`] compares the wallet's
/// anchor against it. Production asks lightwalletd; tests substitute a fixture so
/// the recovery steps can be driven without a gRPC server.
trait TreeStateSource {
    async fn tree_state(&mut self, height: u64) -> Result<z_coin_grpc::TreeState, String>;
}

impl TreeStateSource for LightwalletdClient {
    async fn tree_state(&mut self, height: u64) -> Result<z_coin_grpc::TreeState, String> {
        let request = z_coin_grpc::BlockId {
            height,
            hash: Vec::new(),
        };
        Ok(
            tokio::time::timeout(LIGHTWALLETD_REQUEST_TIMEOUT, self.get_tree_state(request))
                .await
                .map_err(|_| format!("GetTreeState timed out at height {}", height))?
                .map_err(|e| lightwalletd_error_with_sources(&e))?
                .into_inner(),
        )
    }
}

// Serializes wallet-database mutation per database file, across coin instances.
//
// Disabling a shielded coin drops it from the registry, but a background sync
// pass that is already running holds its own strong reference and keeps going
// for the rest of that pass. A re-activation started in that window rebuilds the
// same files — `reset_wallet_scan_state` renames the wallet and compact-block
// databases out from under the in-flight scan. SQLite's own locking prevents
// corruption from concurrent *statements*, but not from a rename, so the scan
// and rebuild entry points take this lock instead.
//
// Keyed by wallet-database path rather than held in `ZCoinShieldedHistory`,
// because the two racing parties are different instances pointed at the same
// files. Only synchronous sections are guarded, so it is never held across an
// await.
lazy_static! {
    static ref WALLET_DB_MUTATION_LOCKS: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(HashMap::new());
}

fn wallet_db_mutation_lock(wallet_db_path: &Path) -> Arc<Mutex<()>> {
    WALLET_DB_MUTATION_LOCKS
        .lock()
        .entry(wallet_db_path.to_path_buf())
        .or_default()
        .clone()
}

#[derive(Clone, Debug)]
pub(crate) struct ZCoinShieldedHistory {
    compact_blocks_path: PathBuf,
    wallet_db_path: PathBuf,
    consensus_params: ZcoinConsensusParams,
    extfvk: ExtendedFullViewingKey,
    initial_chain_state: Arc<Mutex<Option<ChainState>>>,
    /// The anchor this wallet is *born* at: the coin configuration's checkpoint
    /// height, or Sapling activation when the coin declares no checkpoint.
    ///
    /// Distinguishes a wallet that has never been anchored anywhere else from one
    /// deliberately re-anchored by a caller-supplied sync start. The two are
    /// otherwise indistinguishable until a scan finds its first transaction.
    birth_anchor_height: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ZCoinTxHistoryDetails {
    pub(crate) tx_hash: String,
    pub(crate) from: Vec<String>,
    pub(crate) to: Vec<String>,
    pub(crate) spent_by_me: BigDecimal,
    pub(crate) received_by_me: BigDecimal,
    pub(crate) my_balance_change: BigDecimal,
    pub(crate) block_height: u64,
    pub(crate) confirmations: u64,
    pub(crate) timestamp: u64,
    pub(crate) transaction_fee: BigDecimal,
    pub(crate) coin: String,
    pub(crate) internal_id: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct ZCoinTxHistoryPage {
    pub(crate) transactions: Vec<ZCoinTxHistoryDetails>,
    pub(crate) skipped: usize,
    pub(crate) total: usize,
    pub(crate) total_pages: usize,
}

#[derive(Debug)]
struct ZCoinStoredHistoryRow {
    internal_id: i64,
    tx_hash: String,
    block_height: u64,
    timestamp: u64,
    received_by_me: u64,
    spent_by_me: u64,
    received_addresses: Vec<String>,
    sent_addresses: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LightwalletdFetchPlan {
    start_height: u64,
    reset_stale_empty_checkpoint: bool,
    reset_scan_state: bool,
}

impl ZCoinShieldedHistory {
    pub(crate) fn open_or_create(
        ticker: &str,
        db_dir_path: PathBuf,
        consensus_params: ZcoinConsensusParams,
        extfvk: &ExtendedFullViewingKey,
        check_point_block: Option<&CheckPointBlockInfo>,
    ) -> MmResult<Self, ZCoinBuildError> {
        let paths = ZCoinShieldedHistoryPaths::new(ticker, db_dir_path);
        if let Some(parent) = paths.wallet_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        paths.log_legacy_databases_left_untouched();

        match classify_compact_db(&paths.compact_blocks_path) {
            CompactDbGeneration::AbsentOrEmpty | CompactDbGeneration::Recognized => {},
            CompactDbGeneration::Unknown => {
                return MmError::err(ZCoinBuildError::ShieldedDbSchema {
                    path: paths.compact_blocks_path.display().to_string(),
                    reason: "unrecognized compact-block cache schema; file was left unchanged".to_owned(),
                });
            },
            CompactDbGeneration::Corrupt => {
                return MmError::err(ZCoinBuildError::ShieldedDbSchema {
                    path: paths.compact_blocks_path.display().to_string(),
                    reason: "compact-block cache is corrupt or unreadable; file was left unchanged".to_owned(),
                });
            },
        }

        match classify_wallet_db(&paths.wallet_db_path, consensus_params.clone()) {
            WalletDbGeneration::AbsentOrEmpty | WalletDbGeneration::SelectedCurrent => {},
            WalletDbGeneration::UpgradeablePreviousReloaded => {
                let mutation_lock = wallet_db_mutation_lock(&paths.wallet_db_path);
                let _guard = mutation_lock.lock();
                upgrade_previous_reloaded_wallet(&paths.wallet_db_path, consensus_params.clone()).map_err(
                    |reason| {
                        MmError::new(ZCoinBuildError::ShieldedDbSchema {
                            path: paths.wallet_db_path.display().to_string(),
                            reason,
                        })
                    },
                )?;
            },
            WalletDbGeneration::ReferenceLegacy => {
                // Renaming the wallet database aside is a rebuild path (R39.8.0ad).
                // This runs during activation, so it is exactly the party that can
                // race a background scan still finishing on a previous instance.
                let mutation_lock = wallet_db_mutation_lock(&paths.wallet_db_path);
                let _guard = mutation_lock.lock();
                let backup_path = preserve_database_for_rebuild(&paths.wallet_db_path, "legacy-v2")
                    .map_err(|reason| {
                        MmError::new(ZCoinBuildError::ShieldedDbSchema {
                            path: paths.wallet_db_path.display().to_string(),
                            reason,
                        })
                    })?
                    .ok_or_else(|| {
                        MmError::new(ZCoinBuildError::ShieldedDbSchema {
                            path: paths.wallet_db_path.display().to_string(),
                            reason: "recognized legacy wallet disappeared before it could be preserved".to_owned(),
                        })
                    })?;
                log::info!(
                    "Preserved legacy shielded wallet database {} as {}; a fresh Reloaded wallet will be rebuilt and rescanned",
                    paths.wallet_db_path.display(),
                    backup_path.display()
                );
            },
            WalletDbGeneration::Unknown => {
                return MmError::err(ZCoinBuildError::ShieldedDbSchema {
                    path: paths.wallet_db_path.display().to_string(),
                    reason: "unrecognized shielded wallet schema; file was left unchanged".to_owned(),
                });
            },
            WalletDbGeneration::Corrupt => {
                return MmError::err(ZCoinBuildError::ShieldedDbSchema {
                    path: paths.wallet_db_path.display().to_string(),
                    reason: "shielded wallet database is corrupt or unreadable; file was left unchanged".to_owned(),
                });
            },
        }

        let compact_db = BlockDb::for_path(&paths.compact_blocks_path)
            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?;
        init_cache_database(&compact_db)
            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?;

        let mut wallet_db = open_wallet_db(&paths.wallet_db_path, consensus_params.clone())
            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?;
        init_wallet_db(&mut wallet_db, None)
            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?;

        let ufvk = sapling_ufvk(extfvk).map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?;
        let account_ids = wallet_db
            .get_account_ids()
            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?;
        let mut initial_chain_state = None;
        if account_ids.is_empty() {
            if let Some(check_point) = check_point_block {
                let chain_state = chain_state_from_checkpoint(check_point)
                    .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?;
                import_wallet_account(&mut wallet_db, &ufvk, chain_state.clone())
                    .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?;
                initial_chain_state = Some(chain_state);
            }
        } else {
            let matching_account = wallet_db
                .get_account_for_ufvk(&ufvk)
                .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?;
            if matching_account.is_none() {
                return MmError::err(ZCoinBuildError::SaplingCacheError(
                    "Reloaded shielded wallet database belongs to a different viewing key".to_owned(),
                ));
            }
            if wallet_db
                .block_max_scanned()
                .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))?
                .is_none()
            {
                if let Some(check_point) = check_point_block {
                    initial_chain_state = Some(
                        chain_state_from_checkpoint(check_point)
                            .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?,
                    );
                }
            }
        }
        drop(wallet_db);

        let birth_anchor_height = check_point_block
            .map(|check_point| u64::from(check_point.height))
            .unwrap_or_else(|| {
                consensus_params
                    .activation_height(NetworkUpgrade::Sapling)
                    .map(|height| u64::from(u32::from(height)))
                    .unwrap_or(0)
            });

        Ok(ZCoinShieldedHistory {
            compact_blocks_path: paths.compact_blocks_path,
            wallet_db_path: paths.wallet_db_path,
            consensus_params,
            extfvk: extfvk.clone(),
            initial_chain_state: Arc::new(Mutex::new(initial_chain_state)),
            birth_anchor_height,
        })
    }

    pub(crate) fn wallet_db_path(&self) -> &Path { &self.wallet_db_path }

    pub(crate) fn compact_blocks_path(&self) -> &Path { &self.compact_blocks_path }

    pub(crate) async fn fetch_compact_blocks_from_lightwalletd(
        &self,
        consensus_params: &ZcoinConsensusParams,
        servers: &[String],
        target_height: u64,
        requested_start_height: Option<u64>,
        skip_sync_params: bool,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<u64, ShieldedFetchError> {
        // Local wallet storage that cannot be read means the sync cannot start.
        let Some(fetch_plan) = self
            .lightwalletd_fetch_plan(
                consensus_params,
                target_height,
                requested_start_height,
                skip_sync_params,
            )
            .map_err(ShieldedFetchError::BeforeSync)?
        else {
            log::info!(
                "ZCoin shielded wallet DB already scanned through requested lightwalletd target height {}",
                target_height
            );
            return Ok(self
                .scanned_height()
                .map_err(ShieldedFetchError::BeforeSync)?
                .unwrap_or(0));
        };

        log::info!(
            "ZCoin lightwalletd fetch plan: service={}, start_height={}, target_height={}, requested_start_height={:?}, reset_stale_empty_checkpoint={}, reset_scan_state={}, servers={}",
            LIGHTWALLETD_GRPC_SERVICE,
            fetch_plan.start_height,
            target_height,
            requested_start_height,
            fetch_plan.reset_stale_empty_checkpoint,
            fetch_plan.reset_scan_state,
            servers.len()
        );

        let mut errors = Vec::new();
        // The sync counts as started as soon as any server got past its initial connection and
        // chain-state query; only then is an overall failure a retriable mid-sync failure.
        let mut sync_started = false;
        for server in servers {
            match self
                .fetch_compact_blocks_from_server(consensus_params.clone(), server, fetch_plan, target_height, progress)
                .await
            {
                Ok(fetched_height) => return Ok(fetched_height),
                Err(e) => {
                    log::warn!("ZCoin lightwalletd server {} failed: {}", server, e);
                    sync_started |= matches!(e, ShieldedFetchError::DuringSync(_));
                    errors.push(format!("{}: {}", server, e));
                },
            }
        }

        let error = if errors.is_empty() {
            "No lightwalletd servers configured".to_owned()
        } else {
            format!("All lightwalletd servers failed: {}", errors.join("; "))
        };
        log::warn!("ZCoin lightwalletd fetch failed: {}", error);
        Err(if sync_started {
            ShieldedFetchError::DuringSync(error)
        } else {
            ShieldedFetchError::BeforeSync(error)
        })
    }

    fn lightwalletd_fetch_plan(
        &self,
        consensus_params: &ZcoinConsensusParams,
        target_height: u64,
        requested_start_height: Option<u64>,
        skip_sync_params: bool,
    ) -> Result<Option<LightwalletdFetchPlan>, String> {
        // Sapling is the earliest height at which any shielded output can exist,
        // so it is the hard floor for every sync start point (R39.8.0g).
        let sapling_floor = consensus_params
            .activation_height(NetworkUpgrade::Sapling)
            .map(|h| u32::from(h) as u64)
            .unwrap_or(1)
            .max(1);
        let default_recent_start = target_height
            .saturating_sub(DEFAULT_LIGHT_WALLETD_RECENT_SCAN_BLOCKS)
            .max(sapling_floor);
        // A caller-supplied start is floored at Sapling activation and clamped to
        // the current tip: a request beyond the tip has no earlier history to
        // fetch and must never trigger a destructive reset (R39.8.0g/h).
        let explicit_requested_start =
            requested_start_height.map(|height| height.max(sapling_floor).min(target_height));

        if let Some(scanned_height) = self.scanned_height()? {
            // `skip_sync_params` asks to resume from existing local sync state and
            // ignore any supplied `sync_params` when prior synced state exists
            // (R39.6.2). With no prior state (the `else` branch below), the
            // requested start is still consulted.
            let explicit_requested_start = if skip_sync_params {
                None
            } else {
                explicit_requested_start
            };
            if let Some(requested_start) = explicit_requested_start {
                // The wallet's current sync-start anchor is one block above its
                // earliest stored block (its seed): the height the current scan was
                // actually started from. When the caller's requested start differs
                // from it — in *either* direction — the wallet is anchored on a
                // different point than requested, so activation must rewind/recreate
                // the compact-block cache and wallet database and rescan from the
                // requested start (R39.8.0h). This is checked before the
                // already-scanned short-circuit below so that a changed sync
                // start/date is honored even when the wallet is fully scanned.
                let wallet_sync_start = self.wallet_anchor_height()?.map(|anchor| anchor + 1);
                if Some(requested_start) != wallet_sync_start {
                    return Ok(Some(LightwalletdFetchPlan {
                        start_height: requested_start,
                        reset_stale_empty_checkpoint: false,
                        reset_scan_state: true,
                    }));
                }
                // Requested start matches the current anchor: reuse local state and
                // continue from the tip (no rescan on unchanged re-activations).
                if scanned_height >= target_height {
                    return Ok(None);
                }
                return Ok(Some(LightwalletdFetchPlan {
                    start_height: scanned_height + 1,
                    reset_stale_empty_checkpoint: false,
                    reset_scan_state: false,
                }));
            }

            // No explicit start requested: continue from existing local state.
            if scanned_height >= target_height {
                return Ok(None);
            }
            let resumed_start = scanned_height + 1;
            // If the wallet is still empty and its seed checkpoint predates the
            // default recent window, jump forward to that window instead of
            // replaying long-dead history.
            //
            // Only for a wallet still sitting at the anchor it was born with. One
            // deliberately re-anchored -- a caller-supplied sync start, which is how
            // a restored seed reaches its older funds -- looks *identical* to a
            // fresh wallet right up until the rescan reaches its first transaction,
            // so without this check the very next activation that carries no
            // `sync_params` silently discards the rescan and skips forward past the
            // funds it was asked to find (R39.8.0an).
            let anchored_at_birth = self
                .wallet_sync_start_height()?
                .is_some_and(|start| start <= self.birth_anchor_height.saturating_add(1));
            let should_reseed_empty_checkpoint =
                anchored_at_birth && resumed_start < default_recent_start && self.wallet_scan_state_is_empty()?;
            return Ok(Some(LightwalletdFetchPlan {
                start_height: if should_reseed_empty_checkpoint {
                    default_recent_start
                } else {
                    resumed_start
                },
                reset_stale_empty_checkpoint: should_reseed_empty_checkpoint,
                reset_scan_state: false,
            }));
        }

        let start_height = explicit_requested_start.unwrap_or(default_recent_start);
        Ok((start_height <= target_height).then_some(LightwalletdFetchPlan {
            start_height,
            reset_stale_empty_checkpoint: false,
            reset_scan_state: false,
        }))
    }

    /// The wallet's sync anchor: the earliest block height stored in the shielded
    /// wallet database (the seed checkpoint), or `None` when no block is stored.
    fn wallet_anchor_height(&self) -> Result<Option<u64>, String> {
        let wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        wallet_db
            .get_wallet_birthday()
            .map(|height| height.map(|height| u64::from(u32::from(height).saturating_sub(1))))
            .map_err(|e| e.to_string())
    }

    /// The height the current shielded scan was anchored at: one block above the
    /// earliest block stored in the wallet DB (the seed checkpoint). `None` when
    /// the wallet has no stored blocks. Lets activation report `first_sync_block`
    /// even when the caller supplied no explicit `sync_params` (R39.8.0h).
    pub(crate) fn wallet_sync_start_height(&self) -> Result<Option<u64>, String> {
        let wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        wallet_db
            .get_wallet_birthday()
            .map(|height| height.map(|height| u64::from(u32::from(height))))
            .map_err(|e| e.to_string())
    }

    async fn fetch_compact_blocks_from_server(
        &self,
        consensus_params: ZcoinConsensusParams,
        server: &str,
        fetch_plan: LightwalletdFetchPlan,
        target_height: u64,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<u64, ShieldedFetchError> {
        let started = Instant::now();
        let start_height = fetch_plan.start_height;
        log::info!(
            "ZCoin lightwalletd server {} scan started: compact block range {}..={}",
            server,
            start_height,
            target_height
        );

        // Connecting, the local resets and the initial chain-state query below happen before any block
        // is downloaded: a failure there means the sync could not start (CRD ch.39 R39.10.14).
        let mut client = Self::connect_lightwalletd(server)
            .await
            .map_err(ShieldedFetchError::BeforeSync)?;

        if fetch_plan.reset_stale_empty_checkpoint {
            log::info!(
                "ZCoin shielded wallet DB has stale empty checkpoint; resetting before fetching from height {}",
                start_height
            );
            self.reset_empty_wallet_scan_state()
                .map_err(ShieldedFetchError::BeforeSync)?;
        } else if fetch_plan.reset_scan_state {
            log::info!(
                "ZCoin shielded wallet DB scan state reset requested before fetching from height {}",
                start_height
            );
            self.reset_wallet_scan_state().map_err(ShieldedFetchError::BeforeSync)?;
        }

        // A rewind past a chain divergence lowers the anchor, so the fetch has to
        // start from where the wallet actually ends up rather than from the plan's
        // height -- resuming above it would leave the rewound blocks unscanned.
        let start_height = if start_height > 0 {
            let anchor = self
                .ensure_lightwalletd_chain_state(&mut client, consensus_params.clone(), start_height - 1)
                .await
                .map_err(ShieldedFetchError::BeforeSync)?;
            if anchor + 1 != start_height {
                log::info!(
                    "ZCoin lightwalletd fetch start moved from {} to {} after re-anchoring the wallet",
                    start_height,
                    anchor + 1
                );
            }
            anchor + 1
        } else {
            start_height
        };

        let mut fetched_height = self
            .compact_cache_resume_height(start_height, target_height)
            .map_err(ShieldedFetchError::DuringSync)?;
        progress(fetched_height, target_height);
        let mut batch_start = fetched_height
            .checked_add(1)
            .ok_or_else(|| ShieldedFetchError::DuringSync("Compact block height overflow".to_owned()))?;
        while batch_start <= target_height {
            let batch_end = std::cmp::min(target_height, batch_start + LIGHTWALLETD_BLOCK_BATCH_SIZE - 1);
            fetched_height = self
                .fetch_compact_block_batch_from_server(&mut client, batch_start, batch_end)
                .await
                .map_err(ShieldedFetchError::DuringSync)?;
            if fetched_height < batch_end {
                return Err(ShieldedFetchError::DuringSync(format!(
                    "lightwalletd returned compact blocks through {}, below requested batch end {}",
                    fetched_height, batch_end
                )));
            }
            progress(fetched_height, target_height);
            batch_start = batch_end + 1;
        }

        log::info!(
            "ZCoin lightwalletd server {} scan fetched compact blocks through {} in {:?}",
            server,
            fetched_height,
            started.elapsed()
        );
        Ok(fetched_height)
    }

    /// The height compact fetching resumes after: the top of a cached segment that
    /// still validates against the wallet's anchor, or `start_height - 1` when
    /// nothing cached is reusable. A cache that fails validation is discarded and
    /// recreated empty.
    fn compact_cache_resume_height(&self, start_height: u64, target_height: u64) -> Result<u64, String> {
        match self.validated_cached_resume_height(start_height, target_height) {
            Ok(Some(cached_height)) => {
                log::info!(
                    "ZCoin lightwalletd reusing validated compact cache through height {}",
                    cached_height
                );
                Ok(cached_height)
            },
            Ok(None) => Ok(start_height.saturating_sub(1)),
            Err(error) => {
                log::warn!(
                    "ZCoin compact cache cannot resume from height {}: {}; discarding it and refetching",
                    start_height,
                    error
                );
                // Replacing the compact cache is a rebuild path (R39.8.0ad): a
                // background scan on another instance may be reading it. Held for
                // these two calls only, because the caller's reset paths take the
                // same non-reentrant lock.
                //
                // The cache is discarded rather than kept as a backup: it holds only
                // blocks the backend can serve again, and a failure that repeats on
                // every pass would otherwise leave one more copy behind each time
                // (R39.8.0n).
                let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
                let _guard = mutation_lock.lock();
                discard_compact_cache(&self.compact_blocks_path)?;
                initialize_compact_db(&self.compact_blocks_path)?;
                Ok(start_height.saturating_sub(1))
            },
        }
    }

    fn wallet_scan_state_is_empty(&self) -> Result<bool, String> {
        let conn = Connection::open(&self.wallet_db_path).map_err(|e| e.to_string())?;
        let has_transactions: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM transactions LIMIT 1)", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        Ok(!has_transactions)
    }

    fn reset_empty_wallet_scan_state(&self) -> Result<(), String> {
        if !self.wallet_scan_state_is_empty()? {
            return Err("Refusing to reset shielded wallet DB because it contains wallet scan activity".to_owned());
        }

        self.reset_wallet_scan_state()
    }

    fn reset_wallet_scan_state(&self) -> Result<(), String> {
        let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
        let _guard = mutation_lock.lock();
        *self.initial_chain_state.lock() = None;
        preserve_database_for_rebuild(&self.wallet_db_path, "rescan")?;
        preserve_database_for_rebuild(&self.compact_blocks_path, "rescan")?;
        initialize_compact_db(&self.compact_blocks_path)?;
        initialize_wallet_db(&self.wallet_db_path, self.consensus_params.clone())
    }

    /// Rewinds the shielded wallet database to `height`, dropping everything
    /// scanned above it, and reports the height actually reached.
    ///
    /// The note commitment tree constrains where a truncation can land. The backend
    /// (`zcash_client_sqlite`) truncates to the newest scanned block at or below the
    /// request that its commitment trees can be cut at: a block with a retained
    /// checkpoint, or, below the retained checkpoints, a block before the wallet's
    /// earliest note with a recorded tree position. It therefore stops *lower* than
    /// asked, never higher, and for a wallet holding notes that can be far lower. The
    /// caller must check the returned height rather than assume the request was
    /// honoured.
    ///
    /// When no block qualifies the backend refuses and reports the oldest
    /// checkpoint it retains as the safe rewind height. Such a request is
    /// redirected to that checkpoint instead of failing, because it is as far back
    /// as the wallet can rewind locally.
    ///
    /// How many blocks the retained checkpoints span depends on how the wallet was
    /// scanned: a block gets one when it carries Sapling outputs, and every scan
    /// batch adds one at its start. After an activation scans a long range in one
    /// batch they are sparse and span far more than [`ZCOIN_REORG_REWIND_DEPTH`]
    /// blocks. A wallet the background sync keeps current scans each new block as
    /// its own batch, so nearly every block has one; the window then spans about
    /// 100 blocks. The recovery loop's first rewind is sized to land inside the
    /// window either way (see [`ZCOIN_REORG_REWIND_DEPTH`]); the refusal and the
    /// fall-back below the window apply to later attempts, and to a young wallet
    /// whose window has not filled yet.
    ///
    /// Compact blocks cached above the height reached are deleted in the same step.
    /// They were fetched for the chain being abandoned, and the cache would
    /// otherwise feed the orphaned block back to the scanner. The server sends
    /// them again, so nothing is lost.
    fn truncate_wallet_to_height(&self, height: u64) -> Result<u64, String> {
        let height_u32 = u32::try_from(height)
            .map_err(|_| format!("Shielded wallet rewind height {} does not fit into u32", height))?;
        let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
        let _guard = mutation_lock.lock();
        // The cached anchor describes the pre-rewind chain, so drop it before the
        // truncation rather than after: a failure in between must not leave a
        // stale anchor pointing above the wallet's new tip.
        *self.initial_chain_state.lock() = None;
        let mut wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        let requested = BlockHeight::from_u32(height_u32);
        let truncated = match wallet_db.truncate_to_height(requested) {
            Ok(truncated) => truncated,
            Err(SqliteClientError::RequestedRewindInvalid {
                safe_rewind_height: Some(safe_height),
                ..
            }) if safe_height > requested => {
                log::info!(
                    "ZCoin shielded wallet keeps no checkpoint at or below height {}; rewinding to its oldest checkpoint {} instead",
                    height,
                    safe_height
                );
                wallet_db
                    .truncate_to_height(safe_height)
                    .map_err(|e| format!("Shielded wallet rewind to height {} failed: {}", safe_height, e))?
            },
            Err(e) => return Err(format!("Shielded wallet rewind to height {} failed: {}", height, e)),
        };
        drop(wallet_db);
        self.delete_cached_blocks_above(truncated)?;
        Ok(u64::from(u32::from(truncated)))
    }

    /// Deletes the compact blocks cached at heights above `height`. The caller holds
    /// the wallet-database mutation lock.
    fn delete_cached_blocks_above(&self, height: BlockHeight) -> Result<(), String> {
        let conn = Connection::open(&self.compact_blocks_path).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM compactblocks WHERE height > ?1", params![u32::from(
            height
        )])
        .map_err(|e| format!("Failed to clear cached compact blocks above height {}: {}", height, e))?;
        Ok(())
    }

    /// Replaces the shielded wallet database with a fresh one holding a single
    /// account anchored at `anchor`, and clears the compact-block cache.
    ///
    /// Used when the wallet diverges from the chain by more than it can rewind
    /// locally. The previous wallet database is kept as exactly one backup (it may
    /// hold data the chain cannot restore, such as sent-note memos); the compact
    /// cache is deleted, not backed up, because it holds only blocks the server can
    /// send again. The account is created in the same locked step as the reset, so
    /// no pass ever sees an empty database without an account.
    fn rebuild_wallet_at_anchor(&self, anchor: ChainState) -> Result<(), String> {
        let ufvk = sapling_ufvk(&self.extfvk)?;
        let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
        let _guard = mutation_lock.lock();
        *self.initial_chain_state.lock() = None;
        preserve_database_for_rebuild(&self.wallet_db_path, "chain-divergence")?;
        discard_compact_cache(&self.compact_blocks_path)?;
        initialize_compact_db(&self.compact_blocks_path)?;
        initialize_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        let mut wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        import_wallet_account(&mut wallet_db, &ufvk, anchor.clone())?;
        *self.initial_chain_state.lock() = Some(anchor);
        Ok(())
    }

    fn reset_unscanned_wallet_db(&self) -> Result<(), String> {
        if !self.wallet_scan_state_is_empty()? {
            return Err("Refusing to rebuild an unscanned shielded wallet DB that contains transactions".to_owned());
        }
        let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
        let _guard = mutation_lock.lock();
        *self.initial_chain_state.lock() = None;
        preserve_database_for_rebuild(&self.wallet_db_path, "checkpoint")?;
        initialize_wallet_db(&self.wallet_db_path, self.consensus_params.clone())
    }

    pub(crate) async fn connect_lightwalletd(server: &str) -> Result<LightwalletdClient, String> {
        let endpoint_url = lightwalletd_endpoint(server);
        let endpoint = Endpoint::from_shared(endpoint_url.clone())
            .map_err(|e| lightwalletd_error_with_sources(&e))?
            .connect_timeout(LIGHTWALLETD_CONNECT_TIMEOUT)
            .timeout(LIGHTWALLETD_REQUEST_TIMEOUT);
        let endpoint = if endpoint_url.starts_with("https://") {
            endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|e| lightwalletd_error_with_sources(&e))?
        } else {
            endpoint
        };
        let endpoint = endpoint
            .http2_keep_alive_interval(Duration::from_secs(20))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true)
            .connect()
            .await
            .map_err(|e| lightwalletd_error_with_sources(&e))?;
        Ok(z_coin_grpc::compact_tx_streamer_client::CompactTxStreamerClient::new(
            endpoint,
        ))
    }

    /// Establishes the wallet's sync anchor from lightwalletd, rewinding past a
    /// chain divergence if the wallet has scanned a block the chain has dropped.
    ///
    /// Returns the height actually anchored at, which is **lower** than
    /// `checkpoint_height` when a rewind or rebuild was needed. Callers must resume
    /// fetching from the height after the returned one, not from the one they asked
    /// for. The returned height is always the height of a real anchor with a tree
    /// state behind it; it is never a sentinel.
    ///
    /// The comparison with the chain runs on every call, even when the in-memory
    /// chain state is already at `checkpoint_height`. That state is what the last
    /// scan derived from cached blocks, not what the chain says now, so a reorg
    /// after the scan leaves it stale. Trusting it skipped the divergence check for
    /// the rest of the session, and every background pass then failed against the
    /// stale anchor until a restart (R39.8.0al).
    ///
    /// A divergence is first repaired by rewinding the wallet (see
    /// [`ZCOIN_REORG_REWIND_DEPTH`] and [`ZCOIN_MAX_REORG_REWINDS`]). One the wallet
    /// cannot rewind past is repaired by rebuilding the wallet at its sync start.
    ///
    /// # Errors
    ///
    /// Fails when the chain cannot be queried or answers inconsistently, when a
    /// wallet that needs rebuilding has no recorded sync start, and when local
    /// storage cannot be read or written.
    async fn ensure_lightwalletd_chain_state<S: TreeStateSource>(
        &self,
        tree_states: &mut S,
        consensus_params: ZcoinConsensusParams,
        checkpoint_height: u64,
    ) -> Result<u64, String> {
        let scanned_height = self.scanned_block_height()?;
        if scanned_height.is_some_and(|height| height != checkpoint_height) {
            return Err(format!(
                "Shielded wallet is scanned through height {:?}, but compact fetching requires chain state at {}",
                scanned_height, checkpoint_height
            ));
        }

        let wallet_db = open_wallet_db(&self.wallet_db_path, consensus_params.clone())?;
        let has_account = !wallet_db.get_account_ids().map_err(|e| e.to_string())?.is_empty();
        drop(wallet_db);
        if scanned_height.is_none() && has_account {
            // The prior process stopped after importing the account birthday but
            // before scanning its first block. The birthday stores only the tree
            // size, not the frontier, so rebuild the still-empty wallet and fetch
            // the checkpoint again rather than guessing state.
            self.reset_unscanned_wallet_db()?;
        }

        // A wallet that scanned to the chain tip may hold a block a later reorg
        // dropped. That makes the stored anchor stale, not corrupt, so rewind past
        // the divergence and re-anchor rather than refusing to sync -- otherwise
        // every subsequent activation repeats the same comparison and the wallet
        // never recovers (R39.8.0al).
        let mut anchor_height = checkpoint_height;
        for attempt in 0..=ZCOIN_MAX_REORG_REWINDS {
            log::info!(
                "ZCoin lightwalletd requesting wallet checkpoint tree state at height {}",
                anchor_height
            );
            let tree_state = tree_states.tree_state(anchor_height).await?;
            if tree_state.height != anchor_height {
                return Err(format!(
                    "lightwalletd returned tree state at height {}, expected {}",
                    tree_state.height, anchor_height
                ));
            }

            match self.init_wallet_checkpoint_from_tree_state(consensus_params.clone(), tree_state)? {
                CheckpointOutcome::Accepted => return Ok(anchor_height),
                CheckpointOutcome::Diverged if attempt == ZCOIN_MAX_REORG_REWINDS => break,
                CheckpointOutcome::Diverged => {},
            }

            // `saturating_sub` keeps the request inside the chain; a wallet whose
            // anchor is already at or below the rewind depth has nothing useful to
            // rewind to and is rebuilt below instead.
            let requested = anchor_height.saturating_sub(ZCOIN_REORG_REWIND_DEPTH);
            if requested == 0 {
                break;
            }
            // A rewind the backend refuses even after the redirect to its oldest
            // checkpoint is not fatal -- it only means this wallet cannot be
            // rewound, so fall through to the rebuild rather than replacing one
            // permanent failure with another.
            let truncated = match self.truncate_wallet_to_height(requested) {
                Ok(truncated) => truncated,
                Err(error) => {
                    log::warn!(
                        "ZCoin shielded wallet cannot rewind to height {}: {}; rebuilding",
                        requested,
                        error
                    );
                    break;
                },
            };
            if truncated >= anchor_height {
                // The commitment tree would not let us rewind past the divergence,
                // so walking back further cannot help.
                log::warn!(
                    "ZCoin shielded wallet could not rewind below height {} (stopped at {}); rebuilding",
                    anchor_height,
                    truncated
                );
                break;
            }
            log::info!(
                "ZCoin shielded wallet rewound from height {} to {} after a chain divergence",
                anchor_height,
                truncated
            );
            anchor_height = truncated;
        }

        // Rewinding did not reach agreement with the chain. Rebuild at the wallet's
        // sync start and rescan from there: correct regardless of how the wallet got
        // here, and lossless within the range the wallet was asked to cover, because
        // every shielded note in it is recoverable from the chain with the viewing
        // key.
        log::warn!("ZCoin shielded wallet still diverges from the chain after rewinding; rebuilding at its sync start");
        self.rebuild_at_sync_start(tree_states, consensus_params).await
    }

    /// Rebuilds the wallet at the height it originally began scanning from and
    /// returns that anchor height (the sync start minus one).
    ///
    /// The sync start is the wallet account's birthday, which survives every
    /// rewind and which this rebuild recreates, so it is read before anything is
    /// destroyed. The tree state at the anchor is fetched and validated first too:
    /// a chain that cannot answer leaves the wallet untouched instead of replacing
    /// it with an empty one. Notes below the sync start stay out of scope, as for
    /// any wallet anchored there.
    ///
    /// # Errors
    ///
    /// Fails, without touching the wallet, when no sync start is recorded or it
    /// leaves no block below it to anchor on; the rebuild never falls back to
    /// anchoring at height 0.
    async fn rebuild_at_sync_start<S: TreeStateSource>(
        &self,
        tree_states: &mut S,
        consensus_params: ZcoinConsensusParams,
    ) -> Result<u64, String> {
        let sync_start = self.wallet_sync_start_height()?.ok_or_else(|| {
            "Shielded wallet diverges from the chain and has no recorded sync start to rebuild at; refusing to rebuild without it"
                .to_owned()
        })?;
        let anchor_height = sync_start
            .checked_sub(1)
            .filter(|height| *height > 0)
            .ok_or_else(|| {
                format!(
                    "Shielded wallet diverges from the chain and its recorded sync start {} leaves no block to anchor a rebuild on",
                    sync_start
                )
            })?;

        log::info!(
            "ZCoin lightwalletd requesting rebuild anchor tree state at height {}",
            anchor_height
        );
        let tree_state = tree_states.tree_state(anchor_height).await?;
        if tree_state.height != anchor_height {
            return Err(format!(
                "lightwalletd returned tree state at height {}, expected {}",
                tree_state.height, anchor_height
            ));
        }
        let chain_state = chain_state_from_tree_state(&consensus_params, &tree_state)?;
        self.rebuild_wallet_at_anchor(chain_state)?;
        log::info!(
            "ZCoin shielded wallet rebuilt at its sync start: anchored at height {}, rescanning from {}",
            anchor_height,
            sync_start
        );
        Ok(anchor_height)
    }

    fn init_wallet_checkpoint_from_tree_state(
        &self,
        consensus_params: ZcoinConsensusParams,
        tree_state: z_coin_grpc::TreeState,
    ) -> Result<CheckpointOutcome, String> {
        let chain_state = chain_state_from_tree_state(&consensus_params, &tree_state)?;
        let checkpoint_height = chain_state.block_height();
        let mut wallet_db = open_wallet_db(&self.wallet_db_path, consensus_params)?;
        if wallet_db.get_account_ids().map_err(|e| e.to_string())?.is_empty() {
            let ufvk = sapling_ufvk(&self.extfvk)?;
            import_wallet_account(&mut wallet_db, &ufvk, chain_state.clone())?;
        } else {
            let block_metadata = wallet_db
                .block_metadata(checkpoint_height)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    format!(
                        "Shielded wallet DB has an account but no scanned block metadata at refreshed chain-state height {}",
                        checkpoint_height
                    )
                })?;
            if block_metadata.block_hash() != chain_state.block_hash() {
                // The wallet scanned a block that is no longer on the chain --
                // almost always a block it scanned while that block was the tip,
                // which a later reorg replaced. Report it for the caller to rewind
                // past rather than failing: the stored state is stale, not corrupt.
                log::warn!(
                    "ZCoin shielded wallet diverges from the chain at height {}: wallet has block {}, \
                     lightwalletd reports {}",
                    checkpoint_height,
                    display_block_hash(&block_metadata.block_hash()),
                    display_block_hash(&chain_state.block_hash()),
                );
                return Ok(CheckpointOutcome::Diverged);
            }
            let sapling_tree_size = u32::try_from(chain_state.final_sapling_tree().tree_size()).map_err(|_| {
                format!(
                    "lightwalletd Sapling tree size at height {} does not fit into u32",
                    checkpoint_height
                )
            })?;
            if block_metadata.sapling_tree_size() != Some(sapling_tree_size) {
                return Err(format!(
                    "lightwalletd Sapling tree size {} at height {} does not match wallet DB metadata {:?}",
                    sapling_tree_size,
                    checkpoint_height,
                    block_metadata.sapling_tree_size()
                ));
            }
        }
        let sapling_tree_size = chain_state.final_sapling_tree().tree_size();
        *self.initial_chain_state.lock() = Some(chain_state);
        log::debug!(
            "ZCoin shielded wallet accepted chain state at height {} with Sapling tree size {}",
            checkpoint_height,
            sapling_tree_size
        );
        Ok(CheckpointOutcome::Accepted)
    }

    async fn fetch_compact_block_batch_from_server(
        &self,
        client: &mut LightwalletdClient,
        start_height: u64,
        target_height: u64,
    ) -> Result<u64, String> {
        let started = Instant::now();
        log::debug!(
            "ZCoin lightwalletd requesting compact block batch {}..={}",
            start_height,
            target_height
        );
        let request = z_coin_grpc::BlockRange {
            start: Some(z_coin_grpc::BlockId {
                height: start_height,
                hash: Vec::new(),
            }),
            end: Some(z_coin_grpc::BlockId {
                height: target_height,
                hash: Vec::new(),
            }),
        };
        let response = tokio::time::timeout(LIGHTWALLETD_REQUEST_TIMEOUT, client.get_block_range(request))
            .await
            .map_err(|_| {
                format!(
                    "GetBlockRange timed out for compact block batch {}..{}",
                    start_height, target_height
                )
            })?
            .map_err(|e| lightwalletd_error_with_sources(&e))?
            .into_inner();
        let mut stream = response;

        let mut blocks = Vec::with_capacity(LIGHTWALLETD_BLOCK_BATCH_SIZE as usize);
        let mut expected_height = start_height;
        loop {
            let Some(block) = tokio::time::timeout(LIGHTWALLETD_REQUEST_TIMEOUT, stream.next())
                .await
                .map_err(|_| {
                    format!(
                        "lightwalletd compact block stream timed out for batch {}..{}",
                        start_height, target_height
                    )
                })?
            else {
                break;
            };
            let block = block.map_err(|e| lightwalletd_error_with_sources(&e))?;
            if block.height != expected_height {
                return Err(format!(
                    "lightwalletd returned compact block height {}, expected {} in batch {}..{}",
                    block.height, expected_height, start_height, target_height
                ));
            }
            if block.height > target_height {
                return Err(format!(
                    "lightwalletd returned compact block height {} beyond requested batch end {}",
                    block.height, target_height
                ));
            }
            blocks.push(convert_compact_block(block)?);
            expected_height = expected_height
                .checked_add(1)
                .ok_or_else(|| "Compact block height overflow".to_owned())?;
        }

        let last_height = expected_height.saturating_sub(1);
        if last_height < target_height {
            Err(format!(
                "lightwalletd returned compact blocks through {}, below requested {}",
                last_height, target_height
            ))
        } else {
            self.insert_compact_blocks(&blocks)?;
            log::debug!(
                "ZCoin lightwalletd fetched compact block batch {}..={} in {:?}",
                start_height,
                target_height,
                started.elapsed()
            );
            Ok(last_height)
        }
    }

    fn insert_compact_block(&self, block: zcash_compact::CompactBlock) -> Result<(), String> {
        self.insert_compact_blocks(std::slice::from_ref(&block))
    }

    fn insert_compact_blocks(&self, blocks: &[zcash_compact::CompactBlock]) -> Result<(), String> {
        let mut conn = Connection::open(&self.compact_blocks_path).map_err(|e| e.to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut statement = transaction
                .prepare_cached("INSERT OR REPLACE INTO compactblocks (height, data) VALUES (?1, ?2)")
                .map_err(|e| e.to_string())?;
            for block in blocks {
                statement
                    .execute(params![u32::from(block.height()), block.encode_to_vec()])
                    .map_err(|e| e.to_string())?;
            }
        }
        transaction.commit().map_err(|e| e.to_string())
    }

    /// Returns the highest cached block that can be reused for this fetch after
    /// validating the complete cached segment against the wallet/checkpoint
    /// chain state. This is especially important when a process stops after the
    /// cache-download phase but before the wallet scan begins.
    fn validated_cached_resume_height(&self, start_height: u64, target_height: u64) -> Result<Option<u64>, String> {
        let start_height_u32 = u32::try_from(start_height)
            .map_err(|_| format!("Compact cache start height {} does not fit into u32", start_height))?;
        let target_height_u32 = u32::try_from(target_height)
            .map_err(|_| format!("Compact cache target height {} does not fit into u32", target_height))?;
        let conn = Connection::open(&self.compact_blocks_path).map_err(|e| e.to_string())?;
        let cached_height = conn
            .query_row(
                "SELECT MAX(height) FROM compactblocks WHERE height BETWEEN ?1 AND ?2",
                params![start_height_u32, target_height_u32],
                |row| row.get::<_, Option<u32>>(0),
            )
            .map_err(|e| e.to_string())?
            .map(u64::from);
        drop(conn);
        let Some(cached_height) = cached_height else {
            return Ok(None);
        };

        let from_height = BlockHeight::from_u32(start_height_u32);
        let from_state = self
            .initial_chain_state
            .lock()
            .as_ref()
            .filter(|state| state.block_height() + 1 == from_height)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| {
                let mut wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
                wallet_chain_state_before(&mut wallet_db, from_height)
            })?;
        let count = usize::try_from(cached_height - start_height + 1)
            .map_err(|_| "Compact cache resume range does not fit into usize".to_owned())?;
        let block_db = BlockDb::for_path(&self.compact_blocks_path).map_err(|e| e.to_string())?;
        if validate_cached_block_chain(&block_db, from_height, &from_state, count)? {
            Ok(Some(cached_height))
        } else {
            Err(format!(
                "compact cache reports height {} but contains no reusable block at or after {}",
                cached_height, start_height
            ))
        }
    }

    pub(crate) fn scanned_height(&self) -> Result<Option<u64>, String> {
        if let Some(height) = self.scanned_block_height()? {
            return Ok(Some(height));
        }
        let wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        wallet_db
            .get_wallet_birthday()
            .map(|height| height.map(|height| u64::from(u32::from(height).saturating_sub(1))))
            .map_err(|e| e.to_string())
    }

    /// Whether the wallet database holds scanned blocks (as opposed to only an imported account
    /// birthday). A wallet that does is bound to the chain those blocks belong to (R39.11.7a(e)).
    pub(crate) fn has_scanned_blocks(&self) -> Result<bool, String> {
        self.scanned_block_height().map(|height| height.is_some())
    }

    fn scanned_block_height(&self) -> Result<Option<u64>, String> {
        let wallet_db = open_wallet_db(&self.wallet_db_path, self.consensus_params.clone())?;
        wallet_db
            .block_max_scanned()
            .map(|block| block.map(|block| u64::from(u32::from(block.block_height()))))
            .map_err(|e| e.to_string())
    }

    pub(crate) fn balance(&self, consensus_params: ZcoinConsensusParams) -> Result<u64, String> {
        let wallet_db = open_wallet_db(&self.wallet_db_path, consensus_params)?;
        let summary = wallet_db
            .get_wallet_summary(ConfirmationsPolicy::MIN)
            .map_err(|e| e.to_string())?;
        let mut total = 0u64;
        if let Some(summary) = summary {
            for balance in summary.account_balances().values() {
                total = total
                    .checked_add(balance.sapling_balance().total().into_u64())
                    .ok_or_else(|| "Shielded wallet balance overflow".to_owned())?;
            }
        }
        Ok(total)
    }

    pub(crate) fn scan_cached_blocks_to_height<F: FnMut(u64, u64)>(
        &self,
        consensus_params: ZcoinConsensusParams,
        target_height: u64,
        blocks_per_iteration: u32,
        inter_iteration_interval_ms: u64,
        progress: F,
    ) -> Result<u64, String> {
        // Held for the whole scan so a concurrent rebuild cannot rename the
        // database files out from under it. See `WALLET_DB_MUTATION_LOCKS`.
        let mutation_lock = wallet_db_mutation_lock(&self.wallet_db_path);
        let _guard = mutation_lock.lock();
        self.scan_cached_blocks_to_height_with_sleeper(
            consensus_params,
            target_height,
            blocks_per_iteration,
            inter_iteration_interval_ms,
            progress,
            std::thread::sleep,
        )
    }

    fn scan_cached_blocks_to_height_with_sleeper<F, S>(
        &self,
        consensus_params: ZcoinConsensusParams,
        target_height: u64,
        blocks_per_iteration: u32,
        inter_iteration_interval_ms: u64,
        mut progress: F,
        mut sleep: S,
    ) -> Result<u64, String>
    where
        F: FnMut(u64, u64),
        S: FnMut(Duration),
    {
        let scan_batch_size = usize::try_from(blocks_per_iteration.max(1))
            .map_err(|_| "Shielded wallet scan iteration size does not fit into usize".to_owned())?;
        let initial_scanned_height = self.scanned_height()?.unwrap_or(0);
        if initial_scanned_height >= target_height {
            log::info!(
                "ZCoin shielded wallet DB scan skipped: scanned_height={}, target_height={}",
                initial_scanned_height,
                target_height
            );
            return Ok(initial_scanned_height);
        }

        let started = Instant::now();
        log::info!(
            "ZCoin shielded wallet DB scan started: scanned_height={}, target_height={}, blocks_per_iteration={}, inter_iteration_interval_ms={}, compact_blocks_path={}, wallet_db_path={}",
            initial_scanned_height,
            target_height,
            scan_batch_size,
            inter_iteration_interval_ms,
            self.compact_blocks_path.display(),
            self.wallet_db_path.display()
        );

        let block_db = BlockDb::for_path(&self.compact_blocks_path).map_err(|e| e.to_string())?;
        let mut wallet_db = open_wallet_db(&self.wallet_db_path, consensus_params.clone())?;
        let target_height_u32 = u32::try_from(target_height)
            .map_err(|_| format!("Shielded wallet scan target {} does not fit into u32", target_height))?;
        wallet_db
            .update_chain_tip(BlockHeight::from_u32(target_height_u32))
            .map_err(|e| e.to_string())?;

        // Scan in bounded batches so activation can report incremental
        // `BuildingWalletDb` progress (R39.3.1). The modern scanner requires
        // the exact chain state immediately preceding each batch.
        let mut scanned_height = initial_scanned_height;
        let mut next_chain_state = self.initial_chain_state.lock().clone();
        progress(scanned_height, target_height);
        while scanned_height < target_height {
            let from_height_u32 = u32::try_from(scanned_height + 1).map_err(|_| {
                format!(
                    "Shielded wallet scan height {} does not fit into u32",
                    scanned_height + 1
                )
            })?;
            let from_height = BlockHeight::from_u32(from_height_u32);
            let from_state = next_chain_state
                .as_ref()
                .filter(|state| state.block_height() + 1 == from_height)
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| wallet_chain_state_before(&mut wallet_db, from_height))?;
            let remaining = usize::try_from(target_height - scanned_height)
                .map_err(|_| "Shielded wallet scan range does not fit into usize".to_owned())?;
            let scan_limit = remaining.min(scan_batch_size);
            let batch_target = scanned_height
                .checked_add(
                    u64::try_from(scan_limit)
                        .map_err(|_| "Shielded wallet scan iteration size does not fit into u64".to_owned())?,
                )
                .ok_or_else(|| "Shielded wallet scan batch height overflow".to_owned())?;
            let batch_started = Instant::now();
            log::debug!(
                "ZCoin shielded wallet DB scan batch started: range {}..={}, preceding_sapling_tree_size={}",
                from_height,
                batch_target,
                from_state.final_sapling_tree().tree_size()
            );
            let Some(validated_to_state) =
                validated_cached_block_chain_state(&block_db, from_height, &from_state, scan_limit)?
            else {
                break;
            };
            let sapling_tree_size_before =
                u32::try_from(from_state.final_sapling_tree().tree_size()).map_err(|_| {
                    format!(
                        "Sapling tree size before compact block {} does not fit into u32",
                        from_height
                    )
                })?;
            let block_source = PirateChainMetadataBlockSource {
                inner: &block_db,
                first_height: from_height,
                sapling_tree_size_before,
            };
            // Decryption-only parameters: Pirate accepts both note plaintext
            // versions at every height, which upstream can only express inside the
            // ZIP-212 grace period (R39.8.0am). Never used to build a transaction.
            let decryption_params = ZcoinDecryptionParams::new(consensus_params.clone());
            let summary = match scan_cached_blocks(
                &decryption_params,
                &block_source,
                &mut wallet_db,
                from_height,
                &from_state,
                scan_limit,
            ) {
                Ok(summary) => summary,
                Err(ChainScanError::BlockSource(SqliteClientError::CacheMiss(_))) => break,
                Err(e) => return Err(e.to_string()),
            };
            let scanned_range = summary.scanned_range();
            let new_scanned_height = u32::from(scanned_range.end).saturating_sub(1) as u64;
            if new_scanned_height <= scanned_height {
                // No further cached blocks were scanned this iteration.
                break;
            }
            let validated_height = u64::from(u32::from(validated_to_state.block_height()));
            if new_scanned_height != validated_height {
                return Err(format!(
                    "Shielded wallet scanner reached height {}, but validated compact frontier reached {}",
                    new_scanned_height, validated_height
                ));
            }
            next_chain_state = Some(validated_to_state.clone());
            *self.initial_chain_state.lock() = Some(validated_to_state);
            scanned_height = new_scanned_height;
            log::debug!(
                "ZCoin shielded wallet DB scan batch finished: range {}..={} in {:?}, final_sapling_tree_size={}",
                from_height,
                scanned_height,
                batch_started.elapsed(),
                next_chain_state
                    .as_ref()
                    .map(|state| state.final_sapling_tree().tree_size())
                    .unwrap_or_default()
            );
            progress(scanned_height, target_height);
            if scanned_height >= target_height {
                break;
            }
            if inter_iteration_interval_ms > 0 {
                log::debug!(
                    "ZCoin shielded wallet DB scan pausing {} ms after height {}",
                    inter_iteration_interval_ms,
                    scanned_height
                );
                sleep(Duration::from_millis(inter_iteration_interval_ms));
            }
        }

        let scanned_height = wallet_db
            .block_max_scanned()
            .map_err(|e| e.to_string())?
            .map(|block| u64::from(u32::from(block.block_height())))
            .unwrap_or(scanned_height);
        if scanned_height >= target_height {
            log::info!(
                "ZCoin shielded wallet DB scan finished through height {} in {:?}",
                scanned_height,
                started.elapsed()
            );
            Ok(scanned_height)
        } else {
            Err(format!(
                "Shielded wallet DB scanned only through height {}, below activation tip {}. \
                 Compact-block scanner/source is unavailable or has not cached enough blocks.",
                scanned_height, target_height
            ))
        }
    }

    pub(crate) fn load_page(
        &self,
        ticker: &str,
        wallet_z_address: &str,
        decimals: u8,
        current_block: u64,
        paging_options: &PagingOptionsEnum<i64>,
        limit: usize,
    ) -> Result<ZCoinTxHistoryPage, String> {
        let rows = self.load_rows()?;
        let total = rows.len();
        let skipped = skipped_by_paging(&rows, paging_options, limit)?;
        let transactions = rows
            .into_iter()
            .skip(skipped)
            .take(limit)
            .map(|row| row.into_details(ticker, wallet_z_address, decimals, current_block))
            .collect();

        Ok(ZCoinTxHistoryPage {
            transactions,
            skipped,
            total,
            total_pages: calc_total_pages(total, limit),
        })
    }

    fn load_rows(&self) -> Result<Vec<ZCoinStoredHistoryRow>, String> {
        let conn = Connection::open(&self.wallet_db_path).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    t.id_tx,
                    vt.txid,
                    COALESCE(vt.mined_height, 0) AS block_height,
                    COALESCE(vt.block_time, 0) AS timestamp,
                    COALESCE(SUM(vt.total_received), 0) AS received_by_me,
                    COALESCE(SUM(vt.total_spent), 0) AS spent_by_me,
                    COALESCE((
                        SELECT GROUP_CONCAT(DISTINCT outputs.to_address)
                        FROM v_tx_outputs outputs
                        WHERE outputs.transaction_id = t.id_tx
                          AND outputs.to_account_uuid IS NOT NULL
                          AND outputs.to_address IS NOT NULL
                    ), '') AS received_addresses,
                    COALESCE((
                        SELECT GROUP_CONCAT(DISTINCT outputs.to_address)
                        FROM v_tx_outputs outputs
                        WHERE outputs.transaction_id = t.id_tx
                          AND outputs.from_account_uuid IS NOT NULL
                          AND outputs.to_account_uuid IS NULL
                          AND outputs.to_address IS NOT NULL
                    ), '') AS sent_addresses
                FROM v_transactions vt
                INNER JOIN transactions t ON t.txid = vt.txid
                GROUP BY t.id_tx, vt.txid, vt.mined_height, vt.block_time
                ORDER BY COALESCE(vt.mined_height, -1) DESC, t.id_tx DESC",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                let txid: Vec<u8> = row.get(1)?;
                let received: i64 = row.get(4)?;
                let spent: i64 = row.get(5)?;
                Ok(ZCoinStoredHistoryRow {
                    internal_id: row.get(0)?,
                    tx_hash: encode_display_txid(txid),
                    block_height: row.get::<_, u32>(2)? as u64,
                    timestamp: row.get::<_, u32>(3)? as u64,
                    received_by_me: non_negative_amount(received, 4)?,
                    spent_by_me: non_negative_amount(spent, 5)?,
                    received_addresses: split_group_concat(row.get::<_, String>(6)?),
                    sent_addresses: split_group_concat(row.get::<_, String>(7)?),
                })
            })
            .map_err(|e| e.to_string())?;

        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

fn lightwalletd_error_with_sources(error: &(dyn StdError + 'static)) -> String {
    let mut details = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        details.push_str(": ");
        details.push_str(&cause.to_string());
        source = cause.source();
    }
    details
}

impl ZCoinStoredHistoryRow {
    fn into_details(
        self,
        ticker: &str,
        wallet_z_address: &str,
        decimals: u8,
        current_block: u64,
    ) -> ZCoinTxHistoryDetails {
        let received_by_me = big_decimal_from_sat_unsigned(self.received_by_me, decimals);
        let spent_by_me = big_decimal_from_sat_unsigned(self.spent_by_me, decimals);
        let mut from = Vec::new();
        if self.spent_by_me > 0 {
            from.push(wallet_z_address.to_owned());
        }
        from.sort();
        from.dedup();

        let mut to = self.received_addresses;
        to.extend(self.sent_addresses);
        if self.received_by_me > 0 {
            to.push(wallet_z_address.to_owned());
        }
        to.sort();
        to.dedup();

        ZCoinTxHistoryDetails {
            tx_hash: self.tx_hash,
            from,
            to,
            spent_by_me: spent_by_me.clone(),
            received_by_me: received_by_me.clone(),
            my_balance_change: received_by_me - spent_by_me,
            block_height: self.block_height,
            confirmations: confirmations(current_block, self.block_height),
            timestamp: self.timestamp,
            transaction_fee: BigDecimal::from(0),
            coin: ticker.to_owned(),
            internal_id: self.internal_id,
        }
    }
}

fn lightwalletd_endpoint(server: &str) -> String {
    if server.starts_with("http://") || server.starts_with("https://") {
        server.to_owned()
    } else {
        format!("https://{}", server)
    }
}

fn decode_hex_field(name: &str, value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| format!("Invalid {} hex: {}", name, e))
}

fn decode_32_byte_hex(name: &str, value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex_field(name, value)?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("Invalid {} length: expected 32 bytes, got {}", name, bytes.len()))
}

/// Whether a tree-state's `tree` field is the substituted bare commitment root.
///
/// The discriminator is `saplingFrontier` (field 6, which Pirate added "to
/// cross-check the value in field 5"): it is empty exactly when the server
/// substituted `finalRoot` for `finalState`, and populated whenever the tree is
/// genuine. Length alone is not the test -- a 3-byte tree with a 3-byte frontier
/// is a real empty tree, a 32-byte tree with an empty frontier is the
/// substitution.
fn is_substituted_commitment_root(sapling_tree: &[u8], sapling_frontier_is_empty: bool) -> bool {
    sapling_tree.len() == SAPLING_COMMITMENT_ROOT_LEN && sapling_frontier_is_empty
}

fn substituted_root_error(checkpoint_height: BlockHeight, sapling_activation: BlockHeight) -> String {
    format!(
        "lightwalletd tree-state sapling tree at height {} is a bare {}-byte commitment root, not a \
         serialized commitment tree; the server substituted finalRoot for finalState and the wallet \
         must not anchor on it. Use sync_params \"earliest\" (Sapling activation height {}), a height \
         at or above it, or a different lightwalletd server",
        checkpoint_height, SAPLING_COMMITMENT_ROOT_LEN, sapling_activation
    )
}

/// Parses the Sapling commitment tree a lightwalletd `TreeState` carries, refusing
/// the shapes that would otherwise be accepted as a silently wrong anchor.
///
/// Pirate's lightwalletd fills this field through `preferredTreeState(finalState,
/// finalRoot)`, which substitutes the **32-byte commitment root** whenever the node
/// cannot supply a frontier — in `GetTreeState` and `GetBridgeTreeState` alike, so
/// there is no alternative RPC to ask instead. Three rejections are needed, because
/// `read_commitment_tree` alone catches none of them reliably:
///
/// 1. A bare 32-byte root. It is not a serialized tree at all.
/// 2. Trailing bytes. `read_commitment_tree` reads `left`, `right` and `parents` and
///    returns without checking that the buffer is exhausted, so a root beginning
///    `00 00 00` parses **successfully** as an *empty* tree while 29 bytes are
///    silently discarded — anchoring the wallet on a tree of size 0.
/// 3. An empty field above Sapling activation. Upstream treats the empty string as
///    the empty tree, which is only legitimate at or below the activation height;
///    the fetch plan floors every request at that height.
fn parse_checkpoint_commitment_tree(
    sapling_tree: &[u8],
    sapling_frontier_is_empty: bool,
    checkpoint_height: BlockHeight,
    sapling_activation: BlockHeight,
) -> Result<sapling::CommitmentTree, String> {
    if sapling_tree.is_empty() {
        if checkpoint_height <= sapling_activation {
            return Ok(sapling::CommitmentTree::empty());
        }
        return Err(format!(
            "lightwalletd tree-state sapling tree at height {} is empty, but Sapling activated at {}",
            checkpoint_height, sapling_activation
        ));
    }

    if is_substituted_commitment_root(sapling_tree, sapling_frontier_is_empty) {
        // Below Sapling activation no Sapling commitment exists, so the tree is
        // empty by consensus rule whatever the server put in the field. The
        // substituted root carries no information we need: only the block hash
        // beside it is used, and that is unaffected. This is the anchor
        // `sync_params: "earliest"` needs, one block under the activation height.
        if checkpoint_height < sapling_activation {
            log::info!(
                "lightwalletd tree-state at height {} (below Sapling activation {}) carries a substituted \
                 commitment root; using the empty tree that holds there by definition",
                checkpoint_height,
                sapling_activation
            );
            return Ok(sapling::CommitmentTree::empty());
        }
        return Err(substituted_root_error(checkpoint_height, sapling_activation));
    }

    if sapling_tree.len() == SAPLING_COMMITMENT_ROOT_LEN {
        // 32 bytes beside a populated frontier: not the known substitution shape,
        // and still not a serialized tree.
        return Err(substituted_root_error(checkpoint_height, sapling_activation));
    }

    let mut cursor = std::io::Cursor::new(sapling_tree);
    let tree: sapling::CommitmentTree = read_commitment_tree(&mut cursor).map_err(|e| {
        format!(
            "lightwalletd tree-state sapling tree at height {} is not a serialized commitment tree: {}",
            checkpoint_height, e
        )
    })?;
    let consumed = cursor.position();
    if consumed != sapling_tree.len() as u64 {
        return Err(format!(
            "lightwalletd tree-state sapling tree at height {} has {} trailing byte(s) after a {}-byte \
             commitment tree; refusing a partially parsed anchor",
            checkpoint_height,
            sapling_tree.len() as u64 - consumed,
            consumed
        ));
    }
    Ok(tree)
}

/// Validates a lightwalletd `TreeState` and converts it into the scan anchor it
/// describes. Touches no wallet or in-memory state (R39.8.0ak).
fn chain_state_from_tree_state(
    consensus_params: &ZcoinConsensusParams,
    tree_state: &z_coin_grpc::TreeState,
) -> Result<ChainState, String> {
    let hash = decode_display_block_hash("lightwalletd tree-state block hash", &tree_state.hash)?;
    let sapling_tree = decode_hex_field("lightwalletd tree-state sapling tree", &tree_state.tree)?;
    let checkpoint_height = BlockHeight::from_u32(tree_state.height.try_into().map_err(|_| {
        format!(
            "lightwalletd tree-state height {} does not fit into u32",
            tree_state.height
        )
    })?);
    // The frontier field, when the server sends one, must carry the same bytes
    // as the tree field. A divergence is a server-side defect we want visible;
    // field 5 stays authoritative because every supported server populates it.
    if !tree_state.sapling_frontier.is_empty() && tree_state.sapling_frontier != tree_state.tree {
        log::warn!(
            "lightwalletd tree-state at height {} disagrees with itself: sapling tree is {} hex chars, \
             sapling frontier is {} hex chars; using the tree field",
            checkpoint_height,
            tree_state.tree.len(),
            tree_state.sapling_frontier.len()
        );
    }
    let sapling_activation = consensus_params
        .activation_height(NetworkUpgrade::Sapling)
        .unwrap_or(BlockHeight::from_u32(0));
    let commitment_tree: sapling::CommitmentTree = parse_checkpoint_commitment_tree(
        &sapling_tree,
        tree_state.sapling_frontier.is_empty(),
        checkpoint_height,
        sapling_activation,
    )?;
    Ok(ChainState::new(
        checkpoint_height,
        BlockHash(hash),
        commitment_tree.to_frontier(),
    ))
}

/// Whether a lightwalletd tree state could be adopted as the wallet's sync anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointOutcome {
    /// The tree state agrees with the wallet, and is now the anchor.
    Accepted,
    /// The wallet has scanned a block the chain no longer carries. The stored
    /// state is stale rather than corrupt, so the caller rewinds past the
    /// divergence instead of failing.
    Diverged,
}

/// Renders a block hash in the big-endian display order block explorers use.
///
/// Block hashes are held internally in little-endian byte order, so a hash logged
/// verbatim cannot be looked up anywhere; a divergence report is only actionable
/// if both sides can be pasted into an explorer.
fn display_block_hash(hash: &BlockHash) -> String {
    let mut bytes = hash.0;
    bytes.reverse();
    hex::encode(bytes)
}

/// Renders a transaction ID in the big-endian *display* order every block
/// explorer, every other KDF coin's history, and this coin's own `withdraw`
/// response use.
///
/// `zcash_client_sqlite` stores `transactions.txid` in the internal
/// little-endian byte order, so the raw column must be reversed before it is
/// shown. Skipping the reversal produced an `z_coin_tx_history` `tx_hash` that
/// no explorer could resolve and that disagreed with the value
/// `send_raw_transaction` returned for the very same transaction (R39.8.7).
fn encode_display_txid(mut txid: Vec<u8>) -> String {
    txid.reverse();
    hex::encode(txid)
}

/// Converts the display-order block ID returned by `z_gettreestate` (and thus
/// lightwalletd `TreeState`) into the canonical little-endian representation
/// used by compact-block hash byte fields.
fn decode_display_block_hash(name: &str, value: &str) -> Result<[u8; 32], String> {
    let mut hash = decode_32_byte_hex(name, value)?;
    hash.reverse();
    Ok(hash)
}

fn convert_compact_block(block: z_coin_grpc::CompactBlock) -> Result<zcash_compact::CompactBlock, String> {
    Ok(zcash_compact::CompactBlock {
        height: block.height,
        hash: block.hash,
        prev_hash: block.prev_hash,
        time: block.time,
        header: block.header,
        vtx: block.vtx.into_iter().map(convert_compact_tx).collect(),
        chain_metadata: None,
    })
}

fn convert_compact_tx(tx: z_coin_grpc::CompactTx) -> zcash_compact::CompactTx {
    zcash_compact::CompactTx {
        index: tx.index,
        txid: tx.hash,
        fee: tx.fee,
        spends: tx.spends.into_iter().map(convert_compact_spend).collect(),
        outputs: tx.outputs.into_iter().map(convert_compact_output).collect(),
        actions: Vec::new(),
        ironwood_actions: Vec::new(),
        vin: Vec::new(),
        vout: Vec::new(),
    }
}

fn convert_compact_spend(spend: z_coin_grpc::CompactSpend) -> zcash_compact::CompactSaplingSpend {
    zcash_compact::CompactSaplingSpend { nf: spend.nf }
}

fn convert_compact_output(output: z_coin_grpc::CompactOutput) -> zcash_compact::CompactSaplingOutput {
    zcash_compact::CompactSaplingOutput {
        cmu: output.cmu,
        ephemeral_key: output.epk,
        ciphertext: output.ciphertext,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletDbGeneration {
    AbsentOrEmpty,
    ReferenceLegacy,
    /// A wallet created by the previous Reloaded build (`zcash_client_sqlite 0.21.1`,
    /// 48 migrations). The current migrator can bring it forward (R39.8.0ba).
    UpgradeablePreviousReloaded,
    SelectedCurrent,
    Unknown,
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompactDbGeneration {
    AbsentOrEmpty,
    Recognized,
    Unknown,
    Corrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SchemaFingerprint {
    user_version: i64,
    records: Vec<String>,
}

const REFERENCE_LEGACY_WALLET_SCHEMA: &str = r#"
CREATE TABLE accounts (
    account INTEGER PRIMARY KEY,
    extfvk TEXT NOT NULL,
    address TEXT NOT NULL
);
CREATE TABLE blocks (
    height INTEGER PRIMARY KEY,
    hash BLOB NOT NULL,
    time INTEGER NOT NULL,
    sapling_tree BLOB NOT NULL
);
CREATE TABLE transactions (
    id_tx INTEGER PRIMARY KEY,
    txid BLOB NOT NULL UNIQUE,
    created TEXT,
    block INTEGER,
    tx_index INTEGER,
    expiry_height INTEGER,
    raw BLOB,
    FOREIGN KEY (block) REFERENCES blocks(height)
);
CREATE TABLE received_notes (
    id_note INTEGER PRIMARY KEY,
    tx INTEGER NOT NULL,
    output_index INTEGER NOT NULL,
    account INTEGER NOT NULL,
    diversifier BLOB NOT NULL,
    value INTEGER NOT NULL,
    rcm BLOB NOT NULL,
    nf BLOB NOT NULL UNIQUE,
    is_change INTEGER NOT NULL,
    memo BLOB,
    spent INTEGER,
    UNIQUE (tx, output_index),
    FOREIGN KEY (tx) REFERENCES transactions(id_tx),
    FOREIGN KEY (account) REFERENCES accounts(account),
    FOREIGN KEY (spent) REFERENCES transactions(id_tx)
);
CREATE TABLE sapling_witnesses (
    id_witness INTEGER PRIMARY KEY,
    note INTEGER NOT NULL,
    block INTEGER NOT NULL,
    witness BLOB NOT NULL,
    UNIQUE (note, block),
    FOREIGN KEY (note) REFERENCES received_notes(id_note),
    FOREIGN KEY (block) REFERENCES blocks(height)
);
CREATE TABLE sent_notes (
    id_note INTEGER PRIMARY KEY,
    tx INTEGER NOT NULL,
    output_index INTEGER NOT NULL,
    from_account INTEGER NOT NULL,
    address TEXT NOT NULL,
    value INTEGER NOT NULL,
    memo BLOB,
    UNIQUE (tx, output_index),
    FOREIGN KEY (tx) REFERENCES transactions(id_tx),
    FOREIGN KEY (from_account) REFERENCES accounts(account)
);
"#;

/// Identity of the wallet schema produced by the previous Reloaded build
/// (`zcash_client_sqlite 0.21.1`): `user_version = 8`, 467 canonical records, pinned by
/// the digest of [`schema_fingerprint_digest`]. It cannot be rebuilt from DDL because the
/// upstream migrator keys off its own migration table, so it is pinned by digest only.
const PREVIOUS_RELOADED_WALLET_USER_VERSION: i64 = 8;
const PREVIOUS_RELOADED_WALLET_RECORD_COUNT: usize = 467;
const PREVIOUS_RELOADED_WALLET_DIGEST: &str = "dfba9135b6d5ae446e88d83d162e9458730b5b098051d8565fdd91fe6be5bea8";

const RECOGNIZED_COMPACT_CACHE_SCHEMA: &str = r#"
CREATE TABLE compactblocks (
    height INTEGER PRIMARY KEY,
    data BLOB NOT NULL
);
"#;

fn classify_wallet_db(path: &Path, consensus_params: ZcoinConsensusParams) -> WalletDbGeneration {
    if !path.exists() || path.metadata().is_ok_and(|metadata| metadata.len() == 0) {
        return WalletDbGeneration::AbsentOrEmpty;
    }

    let actual = match read_only_schema_fingerprint(path) {
        Ok(actual) => actual,
        Err(_) => return WalletDbGeneration::Corrupt,
    };
    if schema_is_empty(&actual) {
        return WalletDbGeneration::AbsentOrEmpty;
    }

    match reference_legacy_wallet_fingerprint() {
        Ok(reference) if actual == reference => return WalletDbGeneration::ReferenceLegacy,
        Err(_) => return WalletDbGeneration::Corrupt,
        _ => {},
    }
    match selected_current_wallet_fingerprint(consensus_params) {
        Ok(reference) if actual == reference => WalletDbGeneration::SelectedCurrent,
        Ok(_) if is_previous_reloaded_wallet_fingerprint(&actual) => WalletDbGeneration::UpgradeablePreviousReloaded,
        Ok(_) => WalletDbGeneration::Unknown,
        Err(_) => WalletDbGeneration::Corrupt,
    }
}

/// SHA-256 over `user_version:<n>\n` followed by every canonical record and `\n`.
fn schema_fingerprint_digest(fingerprint: &SchemaFingerprint) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(format!("user_version:{}\n", fingerprint.user_version));
    for record in &fingerprint.records {
        hasher.update(record.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn is_previous_reloaded_wallet_fingerprint(actual: &SchemaFingerprint) -> bool {
    actual.user_version == PREVIOUS_RELOADED_WALLET_USER_VERSION
        && actual.records.len() == PREVIOUS_RELOADED_WALLET_RECORD_COUNT
        && schema_fingerprint_digest(actual) == PREVIOUS_RELOADED_WALLET_DIGEST
}

/// R39.8.0ba: back up, migrate a working copy, verify, fall back to preserve-and-rebuild.
///
/// The caller holds the wallet-database mutation lock. The original file is never
/// modified by the migration: it runs on a private working copy that replaces the
/// original only after it classifies as [`WalletDbGeneration::SelectedCurrent`]. On any
/// failure the original is moved aside (the rebuild path of R39.8.0x) and activation
/// proceeds with a fresh wallet. Only a failure to create the backup is returned as an
/// error, with the original untouched.
fn upgrade_previous_reloaded_wallet(path: &Path, consensus_params: ZcoinConsensusParams) -> Result<(), String> {
    let backup_path = copy_database_as_backup(path, "pre-upgrade")?;
    log::info!(
        "Backed up shielded wallet database {} as {} before upgrading its schema",
        path.display(),
        backup_path.display()
    );

    let work_path = path_with_suffix(path, ".upgrade-work");
    match migrate_wallet_copy(path, &work_path, consensus_params) {
        Ok(()) => {
            // The original was checkpointed when it was backed up; drop any sidecar so it
            // cannot be replayed into the upgraded file.
            for suffix in DATABASE_SIDECAR_SUFFIXES {
                let _ = std::fs::remove_file(path_with_suffix(path, suffix));
            }
            match std::fs::rename(&work_path, path) {
                Ok(()) => {
                    log::info!(
                        "Upgraded shielded wallet database {} to the current schema; backup kept at {}",
                        path.display(),
                        backup_path.display()
                    );
                    return Ok(());
                },
                Err(e) => log::warn!(
                    "Upgraded shielded wallet copy could not replace {}: {}; rebuilding instead",
                    path.display(),
                    e
                ),
            }
        },
        Err(reason) => log::warn!(
            "Upgrading shielded wallet database {} failed: {}; rebuilding instead",
            path.display(),
            reason
        ),
    }

    remove_database_files(&work_path);
    let rebuilt_from = preserve_database_for_rebuild(path, "upgrade-failed")?
        .ok_or_else(|| "recognized previous wallet disappeared before it could be preserved".to_owned())?;
    log::info!(
        "Preserved shielded wallet database {} as {} (backup also at {}); a fresh wallet will be rebuilt and rescanned",
        path.display(),
        rebuilt_from.display(),
        backup_path.display()
    );
    Ok(())
}

fn migrate_wallet_copy(source: &Path, work_path: &Path, consensus_params: ZcoinConsensusParams) -> Result<(), String> {
    remove_database_files(work_path);
    std::fs::copy(source, work_path).map_err(|e| format!("failed to create working copy: {}", e))?;
    {
        let mut wallet_db = open_wallet_db(work_path, consensus_params.clone())?;
        init_wallet_db(&mut wallet_db, None).map_err(|e| format!("schema migration failed: {}", e))?;
    }
    // Fold the WAL into the main file so the file alone carries the upgraded database.
    Connection::open(work_path)
        .and_then(|conn| conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);"))
        .map_err(|e| format!("failed to checkpoint upgraded copy: {}", e))?;
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let _ = std::fs::remove_file(path_with_suffix(work_path, suffix));
    }
    match classify_wallet_db(work_path, consensus_params) {
        WalletDbGeneration::SelectedCurrent => Ok(()),
        other => Err(format!(
            "upgraded schema classified as {:?}, not the current schema",
            other
        )),
    }
}

fn remove_database_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let _ = std::fs::remove_file(path_with_suffix(path, suffix));
    }
}

/// Copies `path` (after folding its WAL into the main file) to a fresh `.bak` name
/// without disturbing the original.
fn copy_database_as_backup(path: &Path, reason: &str) -> Result<PathBuf, String> {
    let conn = Connection::open(path)
        .map_err(|e| format!("Failed to open shielded database {} for backup: {}", path.display(), e))?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| format!("Failed to checkpoint shielded database {}: {}", path.display(), e))?;
    drop(conn);

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid shielded database path {}", path.display()))?;
    let timestamp = common::now_ms();
    for sequence in 0..1_000u32 {
        let name = if sequence == 0 {
            format!("{}.{}.{}.bak", file_name, reason, timestamp)
        } else {
            format!("{}.{}.{}.{}.bak", file_name, reason, timestamp, sequence)
        };
        let candidate = path.with_file_name(name);
        if candidate.exists() {
            continue;
        }
        std::fs::copy(path, &candidate).map_err(|e| {
            format!(
                "Failed to back up shielded database {} as {}: {}",
                path.display(),
                candidate.display(),
                e
            )
        })?;
        return Ok(candidate);
    }
    Err("Too many shielded database backup-name collisions".to_owned())
}

fn classify_compact_db(path: &Path) -> CompactDbGeneration {
    if !path.exists() || path.metadata().is_ok_and(|metadata| metadata.len() == 0) {
        return CompactDbGeneration::AbsentOrEmpty;
    }

    let actual = match read_only_schema_fingerprint(path) {
        Ok(actual) => actual,
        Err(_) => return CompactDbGeneration::Corrupt,
    };
    if schema_is_empty(&actual) {
        return CompactDbGeneration::AbsentOrEmpty;
    }
    match recognized_compact_cache_fingerprint() {
        Ok(reference) if actual == reference => CompactDbGeneration::Recognized,
        Ok(_) => CompactDbGeneration::Unknown,
        Err(_) => CompactDbGeneration::Corrupt,
    }
}

fn schema_is_empty(fingerprint: &SchemaFingerprint) -> bool {
    fingerprint.user_version == 0 && fingerprint.records.is_empty()
}

fn reference_legacy_wallet_fingerprint() -> Result<SchemaFingerprint, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(REFERENCE_LEGACY_WALLET_SCHEMA)
        .map_err(|e| e.to_string())?;
    schema_fingerprint(&conn)
}

fn recognized_compact_cache_fingerprint() -> Result<SchemaFingerprint, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    conn.execute_batch(RECOGNIZED_COMPACT_CACHE_SCHEMA)
        .map_err(|e| e.to_string())?;
    schema_fingerprint(&conn)
}

fn selected_current_wallet_fingerprint(consensus_params: ZcoinConsensusParams) -> Result<SchemaFingerprint, String> {
    let mut conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    db_common::sqlite::rusqlite::vtab::array::load_module(&conn).map_err(|e| e.to_string())?;
    {
        let mut wallet_db = WalletDb::from_connection(&mut conn, consensus_params, SystemClock, OsRng);
        init_wallet_db(&mut wallet_db, None).map_err(|e| e.to_string())?;
    }
    schema_fingerprint(&conn)
}

fn read_only_schema_fingerprint(path: &Path) -> Result<SchemaFingerprint, String> {
    match read_only_schema_fingerprint_once(path) {
        Ok(fingerprint) => return Ok(fingerprint),
        Err(direct_error)
            if !DATABASE_SIDECAR_SUFFIXES
                .iter()
                .any(|suffix| path_with_suffix(path, suffix).exists()) =>
        {
            return Err(direct_error);
        },
        Err(direct_error) => {
            return schema_fingerprint_from_recovery_copy(path).map_err(|recovery_error| {
                format!(
                    "Read-only schema inspection failed: {}; sidecar recovery probe failed: {}",
                    direct_error, recovery_error
                )
            });
        },
    }
}

fn read_only_schema_fingerprint_once(path: &Path) -> Result<SchemaFingerprint, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    schema_fingerprint(&conn)
}

/// Probes a database with pending SQLite sidecars without mutating the source.
///
/// A read-only SQLite connection cannot roll back a hot `-journal`, so a clean
/// database left by an interrupted activation otherwise looks corrupt. Copying
/// the database and its sidecars to a private temporary directory lets SQLite
/// recover the copy before fingerprinting it; the source remains byte-for-byte
/// unchanged until normal wallet opening performs the real recovery.
fn schema_fingerprint_from_recovery_copy(path: &Path) -> Result<SchemaFingerprint, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Invalid shielded database path {}", path.display()))?;
    let probe_dir = create_schema_probe_dir()?;
    let _cleanup = SchemaProbeDirGuard(probe_dir.clone());
    let probe_path = probe_dir.join(file_name);
    std::fs::copy(path, &probe_path).map_err(|e| {
        format!(
            "Failed to copy shielded database {} to recovery probe: {}",
            path.display(),
            e
        )
    })?;
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let source_sidecar = path_with_suffix(path, suffix);
        if source_sidecar.exists() {
            std::fs::copy(&source_sidecar, path_with_suffix(&probe_path, suffix)).map_err(|e| {
                format!(
                    "Failed to copy shielded database sidecar {} to recovery probe: {}",
                    source_sidecar.display(),
                    e
                )
            })?;
        }
    }

    let conn = Connection::open(&probe_path).map_err(|e| e.to_string())?;
    schema_fingerprint(&conn)
}

fn create_schema_probe_dir() -> Result<PathBuf, String> {
    let root = std::env::temp_dir();
    let timestamp = common::now_ms();
    for sequence in 0..1_000u32 {
        let candidate = root.join(format!(
            "kdf-zcoin-schema-probe-{}-{}-{}",
            std::process::id(),
            timestamp,
            sequence
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    if let Err(error) = std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)) {
                        let _ = std::fs::remove_dir(&candidate);
                        return Err(format!(
                            "Failed to restrict shielded database recovery probe directory {}: {}",
                            candidate.display(),
                            error
                        ));
                    }
                }
                return Ok(candidate);
            },
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create shielded database recovery probe directory {}: {}",
                    candidate.display(),
                    error
                ));
            },
        }
    }
    Err("Too many shielded database recovery probe-name collisions".to_owned())
}

struct SchemaProbeDirGuard(PathBuf);

impl Drop for SchemaProbeDirGuard {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn schema_fingerprint(conn: &Connection) -> Result<SchemaFingerprint, String> {
    let integrity: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if integrity != "ok" {
        return Err(format!("SQLite integrity check failed: {}", integrity));
    }

    let user_version = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let objects = schema_objects(conn)?;
    let mut records = objects
        .iter()
        .map(|(object_type, name)| format!("object|{}|{}", object_type, hex::encode(name.as_bytes())))
        .collect::<Vec<_>>();

    for (object_type, name) in &objects {
        if object_type == "table" || object_type == "view" {
            records.extend(prefixed_pragma_records(conn, "columns", name, "table_xinfo")?);
        }
        if object_type == "table" {
            records.extend(prefixed_pragma_records(conn, "foreign_keys", name, "foreign_key_list")?);
            records.extend(index_fingerprint_records(conn, name)?);
        }
    }

    if objects
        .iter()
        .any(|(object_type, name)| object_type == "table" && name == "schemer_migrations")
    {
        records.extend(canonical_query_records(
            conn,
            "SELECT hex(id) FROM schemer_migrations ORDER BY id",
            "migration",
        )?);
    }
    records.sort();
    Ok(SchemaFingerprint { user_version, records })
}

fn schema_objects(conn: &Connection) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name
             FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn prefixed_pragma_records(
    conn: &Connection,
    category: &str,
    object_name: &str,
    pragma: &str,
) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA {}({})", pragma, quote_sql_identifier(object_name));
    canonical_query_records(
        conn,
        &sql,
        &format!("{}|{}", category, hex::encode(object_name.as_bytes())),
    )
}

fn index_fingerprint_records(conn: &Connection, table_name: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA index_list({})", quote_sql_identifier(table_name));
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let indexes = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut records = Vec::with_capacity(indexes.len());
    for (index_name, unique, origin, partial) in indexes {
        let index_sql = format!("PRAGMA index_xinfo({})", quote_sql_identifier(&index_name));
        let columns = canonical_query_records(conn, &index_sql, "column")?;
        records.push(format!(
            "index|{}|{}|{}|{}|{}",
            hex::encode(table_name.as_bytes()),
            unique,
            origin,
            partial,
            columns.join(";")
        ));
    }
    records.sort();
    Ok(records)
}

fn canonical_query_records(conn: &Connection, sql: &str, prefix: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let column_count = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            let mut values = Vec::with_capacity(column_count);
            for column in 0..column_count {
                values.push(canonical_sql_value(row.get_ref(column)?));
            }
            Ok(format!("{}|{}", prefix, values.join("|")))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn canonical_sql_value(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "null".to_owned(),
        ValueRef::Integer(value) => format!("i:{}", value),
        ValueRef::Real(value) => format!("r:{:016x}", value.to_bits()),
        ValueRef::Text(value) => format!("t:{}", hex::encode(value)),
        ValueRef::Blob(value) => format!("b:{}", hex::encode(value)),
    }
}

fn quote_sql_identifier(identifier: &str) -> String { format!("\"{}\"", identifier.replace('"', "\"\"")) }

fn open_wallet_db(path: &Path, consensus_params: ZcoinConsensusParams) -> Result<ReloadedWalletDb, String> {
    WalletDb::for_path(path, consensus_params, SystemClock, OsRng).map_err(|e| e.to_string())
}

fn initialize_compact_db(path: &Path) -> Result<(), String> {
    let compact_db = BlockDb::for_path(path).map_err(|e| e.to_string())?;
    init_cache_database(&compact_db).map_err(|e| e.to_string())
}

fn initialize_wallet_db(path: &Path, consensus_params: ZcoinConsensusParams) -> Result<(), String> {
    let mut wallet_db = open_wallet_db(path, consensus_params)?;
    init_wallet_db(&mut wallet_db, None).map_err(|e| e.to_string())
}

fn sapling_ufvk(extfvk: &ExtendedFullViewingKey) -> Result<UnifiedFullViewingKey, String> {
    UnifiedFullViewingKey::from_sapling_extended_full_viewing_key(extfvk.clone()).map_err(|e| e.to_string())
}

fn chain_state_from_checkpoint(check_point: &CheckPointBlockInfo) -> Result<ChainState, String> {
    let commitment_tree: sapling::CommitmentTree = read_commitment_tree(check_point.sapling_tree.as_slice())
        .map_err(|e| format!("Invalid Sapling checkpoint tree: {}", e))?;
    Ok(ChainState::new(
        BlockHeight::from_u32(check_point.height),
        BlockHash(check_point.hash.0),
        commitment_tree.to_frontier(),
    ))
}

fn import_wallet_account(
    wallet_db: &mut ReloadedWalletDb,
    ufvk: &UnifiedFullViewingKey,
    prior_chain_state: ChainState,
) -> Result<(), String> {
    let checkpoint_height = prior_chain_state.block_height();
    let birthday = AccountBirthday::from_parts(prior_chain_state, None);
    wallet_db
        .import_account_ufvk(
            "KDF Reloaded shielded account",
            ufvk,
            &birthday,
            AccountPurpose::Spending { derivation: None },
            Some("kdf-reloaded"),
        )
        .map_err(|e| e.to_string())?;
    wallet_db.update_chain_tip(checkpoint_height).map_err(|e| e.to_string())
}

fn wallet_chain_state_before(wallet_db: &mut ReloadedWalletDb, from_height: BlockHeight) -> Result<ChainState, String> {
    let prior_height = from_height.saturating_sub(1);
    let block_hash = wallet_db
        .get_block_hash(prior_height)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Shielded wallet DB has no block hash at scan anchor {}", prior_height))?;
    let sapling_frontier = wallet_db
        .with_sapling_tree_mut(|tree| tree.frontier())
        .map_err(|e| e.to_string())?;
    Ok(ChainState::new(prior_height, block_hash, sapling_frontier))
}

/// Adapts Pirate lightwalletd's legacy compact-block wire to the modern scanner.
///
/// Pirate's dictated `CompactBlock` schema predates the optional
/// `ChainMetadata` field. The modern scanner needs the Sapling tree size on the
/// first block when the preceding checkpoint has not yet been persisted as
/// block metadata. KDF already has that trusted checkpoint frontier, so this
/// adapter supplies only the missing final size for the first block. Subsequent
/// blocks derive their starting positions from the preceding scanned block.
struct PirateChainMetadataBlockSource<'a> {
    inner: &'a BlockDb,
    first_height: BlockHeight,
    sapling_tree_size_before: u32,
}

impl BlockSource for PirateChainMetadataBlockSource<'_> {
    type Error = SqliteClientError;

    fn with_blocks<F, WalletErrT>(
        &self,
        from_height: Option<BlockHeight>,
        limit: Option<usize>,
        mut with_block: F,
    ) -> Result<(), ChainScanError<WalletErrT, Self::Error>>
    where
        F: FnMut(zcash_compact::CompactBlock) -> Result<(), ChainScanError<WalletErrT, Self::Error>>,
    {
        self.inner.with_blocks(from_height, limit, |mut block| {
            if block.height() == self.first_height && block.chain_metadata.is_none() {
                let sapling_output_count = block.vtx.iter().try_fold(0u32, |count, tx| {
                    let tx_output_count = u32::try_from(tx.outputs.len()).map_err(|_| {
                        ChainScanError::<WalletErrT, Self::Error>::BlockSource(SqliteClientError::CorruptedData(
                            format!("Compact block {} has too many Sapling outputs", block.height()),
                        ))
                    })?;
                    count.checked_add(tx_output_count).ok_or_else(|| {
                        ChainScanError::<WalletErrT, Self::Error>::BlockSource(SqliteClientError::CorruptedData(
                            format!("Compact block {} Sapling output count overflow", block.height()),
                        ))
                    })
                })?;
                let sapling_commitment_tree_size = self
                    .sapling_tree_size_before
                    .checked_add(sapling_output_count)
                    .ok_or_else(|| {
                        ChainScanError::<WalletErrT, Self::Error>::BlockSource(SqliteClientError::CorruptedData(
                            format!("Compact block {} Sapling tree size overflow", block.height()),
                        ))
                    })?;
                block.chain_metadata = Some(zcash_compact::ChainMetadata {
                    sapling_commitment_tree_size,
                    orchard_commitment_tree_size: 0,
                    ironwood_commitment_tree_size: 0,
                });
            }
            with_block(block)
        })
    }
}

/// Validates the exact cached chain segment that will be handed to the modern
/// scanner. In particular, the scanner cannot compare its first block with an
/// account-birthday `ChainState` that has not yet been persisted as block
/// metadata, so KDF must enforce that boundary explicitly (R39.8.0n).
fn validate_cached_block_chain(
    block_db: &BlockDb,
    from_height: BlockHeight,
    from_state: &ChainState,
    limit: usize,
) -> Result<bool, String> {
    validated_cached_block_chain_state(block_db, from_height, from_state, limit).map(|state| state.is_some())
}

/// Validates a cached segment and advances the exact Sapling frontier over it.
///
/// The wallet's prunable ShardTree is allowed to compress a complete rightmost
/// subtree, after which it cannot reconstruct the leaf-level frontier. Keeping
/// this compact-source frontier avoids asking the prunable store for information
/// it intentionally discarded between bounded scan calls.
fn validated_cached_block_chain_state(
    block_db: &BlockDb,
    from_height: BlockHeight,
    from_state: &ChainState,
    limit: usize,
) -> Result<Option<ChainState>, String> {
    let mut expected_height = from_height;
    let mut expected_prev_hash = from_state.block_hash();
    let mut sapling_frontier = from_state.final_sapling_tree().clone();
    let initial_sapling_tree_size = sapling_frontier.tree_size();
    let mut validation_error = None;
    let mut saw_block = false;
    let result = block_db.with_blocks::<_, SqliteClientError>(Some(from_height), Some(limit), |block| {
        saw_block = true;
        if validation_error.is_some() {
            return Ok(());
        }

        let Some(block_height) = u32::try_from(block.height).ok().map(BlockHeight::from_u32) else {
            validation_error = Some(format!(
                "Compact block height {} does not fit into the supported range",
                block.height
            ));
            return Ok(());
        };
        let hashes = if block.header.is_empty() {
            BlockHash::try_from_slice(&block.hash)
                .zip(BlockHash::try_from_slice(&block.prev_hash))
                .ok_or_else(|| {
                    format!(
                        "Compact block {} must contain 32-byte hash and prev_hash fields",
                        block_height
                    )
                })
        } else if block.header().is_some() {
            Ok((block.hash(), block.prev_hash()))
        } else {
            Err(format!("Compact block {} contains an invalid header", block_height))
        };
        let (block_hash, prev_hash) = match hashes {
            Ok(hashes) => hashes,
            Err(error) => {
                validation_error = Some(error);
                return Ok(());
            },
        };

        if block_height != expected_height {
            validation_error = Some(format!(
                "Compact block height discontinuity: expected {}, found {}",
                expected_height, block_height
            ));
        } else if prev_hash != expected_prev_hash {
            validation_error = Some(format!(
                "Compact block hash discontinuity at height {}: prev_hash does not match the scan anchor",
                block_height
            ));
        } else {
            for (tx_index, tx) in block.vtx.iter().enumerate() {
                for (output_index, output) in tx.outputs.iter().enumerate() {
                    if output.cmu.len() != 32 {
                        validation_error = Some(format!(
                            "Compact block {} transaction {} Sapling output {} has a {}-byte commitment instead of 32 bytes",
                            block_height,
                            tx_index,
                            output_index,
                            output.cmu.len()
                        ));
                        return Ok(());
                    }
                    let cmu = match output.cmu() {
                        Ok(cmu) => cmu,
                        Err(error) => {
                            validation_error = Some(format!(
                                "Compact block {} transaction {} Sapling output {} has an invalid commitment: {}",
                                block_height, tx_index, output_index, error
                            ));
                            return Ok(());
                        },
                    };
                    if !sapling_frontier.append(sapling::Node::from_cmu(&cmu)) {
                        validation_error = Some(format!(
                            "Sapling commitment tree is full while validating compact block {}",
                            block_height
                        ));
                        return Ok(());
                    }
                }
            }
            expected_height = expected_height + 1;
            expected_prev_hash = block_hash;
        }
        Ok(())
    });
    match result {
        Ok(()) => {},
        Err(ChainScanError::BlockSource(SqliteClientError::CacheMiss(_))) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }

    if let Some(error) = validation_error {
        Err(error)
    } else if saw_block {
        log::trace!(
            "ZCoin compact chain validation advanced range {}..={} with Sapling tree size {} -> {}",
            from_height,
            expected_height - 1,
            initial_sapling_tree_size,
            sapling_frontier.tree_size()
        );
        Ok(Some(ChainState::new(
            expected_height - 1,
            expected_prev_hash,
            sapling_frontier,
        )))
    } else {
        Ok(None)
    }
}

fn preserve_database_for_rebuild(path: &Path, reason: &str) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }

    // Flush any live WAL contents before moving the database. All KDF handles
    // are short-lived at this boundary. Truncating the WAL prevents an old
    // sidecar from being replayed into the fresh database created at `path`.
    if let Ok(conn) = Connection::open(path) {
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| format!("Failed to checkpoint shielded database {}: {}", path.display(), e))?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid shielded database path {}", path.display()))?;
    let timestamp = common::now_ms();
    let mut sequence = 0u32;
    let backup_path = loop {
        let suffix = if sequence == 0 {
            format!("{}.{}.{}.bak", file_name, reason, timestamp)
        } else {
            format!("{}.{}.{}.{}.bak", file_name, reason, timestamp, sequence)
        };
        let candidate = path.with_file_name(suffix);
        if !candidate.exists()
            && DATABASE_SIDECAR_SUFFIXES
                .iter()
                .all(|suffix| !path_with_suffix(&candidate, suffix).exists())
        {
            break candidate;
        }
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| "Too many shielded database backup-name collisions".to_owned())?;
    };
    std::fs::rename(path, &backup_path).map_err(|e| {
        format!(
            "Failed to preserve shielded database {} as {}: {}",
            path.display(),
            backup_path.display(),
            e
        )
    })?;
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let sidecar_path = path_with_suffix(path, suffix);
        if sidecar_path.exists() {
            let backup_sidecar_path = path_with_suffix(&backup_path, suffix);
            std::fs::rename(&sidecar_path, &backup_sidecar_path).map_err(|e| {
                format!(
                    "Preserved shielded database {} but failed to preserve sidecar {} as {}: {}",
                    backup_path.display(),
                    sidecar_path.display(),
                    backup_sidecar_path.display(),
                    e
                )
            })?;
        }
    }
    Ok(Some(backup_path))
}

const DATABASE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Deletes a compact-block cache database together with its SQLite sidecars.
///
/// Sidecars go first: if deleting the main file then fails, the error propagates
/// and no stale `-wal` or `-journal` is left to be replayed into the fresh cache
/// created at the same path.
fn discard_compact_cache(path: &Path) -> Result<(), String> {
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        remove_file_if_exists(&path_with_suffix(path, suffix))?;
    }
    remove_file_if_exists(path)
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "Failed to discard compact cache file {}: {}",
            path.display(),
            e
        )),
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut suffixed = path.as_os_str().to_os_string();
    suffixed.push(suffix);
    suffixed.into()
}

struct ZCoinShieldedHistoryPaths {
    compact_blocks_path: PathBuf,
    wallet_db_path: PathBuf,
    legacy_compact_blocks_path: PathBuf,
    legacy_wallet_db_path: PathBuf,
}

impl ZCoinShieldedHistoryPaths {
    fn new(ticker: &str, mut db_dir_path: PathBuf) -> Self {
        let mut compact_blocks_path = db_dir_path.clone();
        compact_blocks_path.push(format!("{}_RELOADED_COMPACT_BLOCKS.db", ticker));
        let mut legacy_compact_blocks_path = db_dir_path.clone();
        legacy_compact_blocks_path.push(format!("{}_COMPACT_BLOCKS.db", ticker));
        let mut legacy_wallet_db_path = db_dir_path.clone();
        legacy_wallet_db_path.push(format!("{}_WALLET.db", ticker));
        db_dir_path.push(format!("{}_RELOADED_WALLET.db", ticker));
        ZCoinShieldedHistoryPaths {
            compact_blocks_path,
            wallet_db_path: db_dir_path,
            legacy_compact_blocks_path,
            legacy_wallet_db_path,
        }
    }

    fn log_legacy_databases_left_untouched(&self) {
        for (kind, path) in [
            ("compact-block cache", &self.legacy_compact_blocks_path),
            ("wallet", &self.legacy_wallet_db_path),
        ] {
            if path.exists() {
                log::info!(
                    "Leaving legacy/Gleec shielded {} database {} untouched; KDF Reloaded uses a separate database name",
                    kind,
                    path.display()
                );
            }
        }
    }
}

fn confirmations(current_block: u64, block_height: u64) -> u64 {
    if block_height == 0 || block_height > current_block {
        0
    } else {
        current_block + 1 - block_height
    }
}

fn skipped_by_paging(
    rows: &[ZCoinStoredHistoryRow],
    paging: &PagingOptionsEnum<i64>,
    limit: usize,
) -> Result<usize, String> {
    match paging {
        PagingOptionsEnum::FromId(from_id) => rows
            .iter()
            .position(|row| row.internal_id == *from_id)
            .map(|idx| idx + 1)
            .ok_or_else(|| format!("Unknown shielded transaction history internal_id {}", from_id)),
        PagingOptionsEnum::PageNumber(page_number) => Ok((page_number.get() - 1) * limit),
    }
}

fn non_negative_amount(amount: i64, column: usize) -> Result<u64, db_common::sqlite::rusqlite::Error> {
    u64::try_from(amount).map_err(|e| {
        db_common::sqlite::rusqlite::Error::FromSqlConversionFailure(
            column,
            db_common::sqlite::rusqlite::types::Type::Integer,
            Box::new(e),
        )
    })
}

fn split_group_concat(value: String) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split(',').map(str::to_owned).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_common::sqlite::rusqlite::params;
    use rand::{rngs::StdRng, SeedableRng};
    use sapling::{note_encryption::{sapling_note_encryption, SaplingDomain},
                  value::NoteValue,
                  zip32::ExtendedSpendingKey,
                  Note, Rseed};
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zcash_note_encryption::Domain;
    use zcash_protocol::memo::MemoBytes;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_db_dir(label: &str) -> PathBuf {
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let db_dir = std::env::temp_dir().join(format!(
            "kdf-zcoin-{}-{}-{}-{}",
            label,
            std::process::id(),
            common::now_ms(),
            test_id
        ));
        std::fs::create_dir_all(&db_dir).unwrap();
        db_dir
    }

    fn test_extfvk(seed: u8) -> ExtendedFullViewingKey {
        let extsk = ExtendedSpendingKey::master(&[seed; 32]);
        #[allow(deprecated)]
        extsk.to_extended_full_viewing_key()
    }

    fn create_legacy_wallet(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(REFERENCE_LEGACY_WALLET_SCHEMA).unwrap();
    }

    fn test_params() -> ZcoinConsensusParams {
        serde_json::from_value(serde_json::json!({
            "overwinter_activation_height": 1,
            "sapling_activation_height": 2,
            "blossom_activation_height": null,
            "heartwood_activation_height": null,
            "canopy_activation_height": null,
            "coin_type": 133,
            "hrp_sapling_extended_spending_key": "secret-extended-key-main",
            "hrp_sapling_extended_full_viewing_key": "zxviews",
            "hrp_sapling_payment_address": "zs",
            "b58_pubkey_address_prefix": [0x1c, 0xb8],
            "b58_script_address_prefix": [0x1c, 0xbd]
        }))
        .unwrap()
    }

    fn test_params_with_zip212() -> ZcoinConsensusParams {
        serde_json::from_value(serde_json::json!({
            "overwinter_activation_height": 1,
            "sapling_activation_height": 2,
            "blossom_activation_height": null,
            "heartwood_activation_height": null,
            "canopy_activation_height": 2,
            "coin_type": 133,
            "hrp_sapling_extended_spending_key": "secret-extended-key-main",
            "hrp_sapling_extended_full_viewing_key": "zxviews",
            "hrp_sapling_payment_address": "zs",
            "b58_pubkey_address_prefix": [0x1c, 0xb8],
            "b58_script_address_prefix": [0x1c, 0xbd]
        }))
        .unwrap()
    }

    fn open_test_history() -> ZCoinShieldedHistory {
        let db_dir = test_db_dir("wallet-history-test");
        let extfvk = test_extfvk(7);
        ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &extfvk, None).unwrap()
    }

    fn open_checkpointed_test_history(checkpoint_height: u32) -> ZCoinShieldedHistory {
        let db_dir = test_db_dir("wallet-history-checkpoint-test");
        let extfvk = test_extfvk(8);
        let check_point = CheckPointBlockInfo {
            height: checkpoint_height,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 1234,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &extfvk, Some(&check_point)).unwrap()
    }

    fn empty_sapling_tree_bytes() -> Vec<u8> {
        let tree = sapling::CommitmentTree::empty();
        let mut bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&tree, &mut bytes).unwrap();
        bytes
    }

    fn empty_compact_block(height: u64, hash_byte: u8, prev_hash_byte: u8) -> zcash_compact::CompactBlock {
        zcash_compact::CompactBlock {
            height,
            hash: vec![hash_byte; 32],
            prev_hash: vec![prev_hash_byte; 32],
            time: u32::try_from(height).unwrap(),
            header: Vec::new(),
            vtx: Vec::new(),
            chain_metadata: Some(zcash_compact::ChainMetadata {
                sapling_commitment_tree_size: 0,
                orchard_commitment_tree_size: 0,
                ironwood_commitment_tree_size: 0,
            }),
        }
    }

    fn compact_block_with_received_note(
        height: u64,
        hash_byte: u8,
        prev_hash_byte: u8,
        extfvk: &ExtendedFullViewingKey,
        value: u64,
    ) -> zcash_compact::CompactBlock {
        let note = Note::from_parts(
            extfvk.default_address().1,
            NoteValue::from_raw(value),
            Rseed::AfterZip212([42; 32]),
        );
        let mut rng = StdRng::from_seed([7; 32]);
        let encryptor = sapling_note_encryption(
            Some(extfvk.fvk.ovk),
            note.clone(),
            MemoBytes::empty().into_bytes(),
            &mut rng,
        );
        let ciphertext = encryptor.encrypt_note_plaintext();
        let output = zcash_compact::CompactSaplingOutput {
            cmu: note.cmu().to_bytes().to_vec(),
            ephemeral_key: SaplingDomain::epk_bytes(encryptor.epk()).0.to_vec(),
            ciphertext: ciphertext[..52].to_vec(),
        };
        let transaction = zcash_compact::CompactTx {
            index: 1,
            txid: vec![68; 32],
            outputs: vec![output],
            ..Default::default()
        };
        zcash_compact::CompactBlock {
            height,
            hash: vec![hash_byte; 32],
            prev_hash: vec![prev_hash_byte; 32],
            time: u32::try_from(height).unwrap(),
            header: Vec::new(),
            vtx: vec![transaction],
            chain_metadata: Some(zcash_compact::ChainMetadata {
                sapling_commitment_tree_size: 1,
                orchard_commitment_tree_size: 0,
                ironwood_commitment_tree_size: 0,
            }),
        }
    }

    fn deterministic_compact_hash(height: u64) -> Vec<u8> {
        let mut hash = vec![0u8; 32];
        hash[..8].copy_from_slice(&height.to_le_bytes());
        hash
    }

    /// Two real ARRR mainnet transaction IDs, in the internal little-endian byte
    /// order `zcash_client_sqlite` stores in `transactions.txid`, paired with the
    /// big-endian display order block explorers resolve. Captured from the
    /// 2026-09-18 live verification run (see
    /// `docs/plans/arrr-ironwood-compatibility.md`): a 0.1 ARRR receive mined at
    /// height 4138653 and the sweep that spent it, mined at 4138661.
    ///
    /// These are deliberately **not** byte-order-symmetric. The previous fixture
    /// used `[1u8; 32]` / `[2u8; 32]`, which read identically forwards and
    /// backwards and so could never have detected a missing reversal.
    const FIXTURE_RECEIVE_TXID_INTERNAL: [u8; 32] =
        hex_literal_32("a4d43b3b60f54559abed8c6ba71354214c7f20b6a4e5e74ac318ba0a53409733");
    const FIXTURE_RECEIVE_TXID_DISPLAY: &str = "339740530aba18c34ae7e5a4b6207f4c215413a76b8cedab5945f5603b3bd4a4";
    const FIXTURE_SPEND_TXID_INTERNAL: [u8; 32] =
        hex_literal_32("0e3774441248e313b0c0bc1bf49a23e2d523c61c30fc4c897c6fc1e3da6cbb23");
    const FIXTURE_SPEND_TXID_DISPLAY: &str = "23bb6cdae3c16f7c894cfc301cc623d5e2239af41bbcc0b013e348124474370e";

    /// `const`-evaluable hex decoder, so the fixture IDs above can be written as
    /// the hex strings they are quoted as everywhere else.
    const fn hex_literal_32(hex: &str) -> [u8; 32] {
        const fn nibble(b: u8) -> u8 {
            match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => panic!("fixture txid hex must be lowercase [0-9a-f]"),
            }
        }
        let bytes = hex.as_bytes();
        assert!(bytes.len() == 64, "fixture txid hex must be 32 bytes");
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            out[i] = (nibble(bytes[2 * i]) << 4) | nibble(bytes[2 * i + 1]);
            i += 1;
        }
        out
    }

    /// Anchors a freshly rebuilt wallet at `start_height`, the way the fetch path
    /// does when it adopts a caller-supplied sync start.
    fn seed_wallet_anchor(history: &ZCoinShieldedHistory, start_height: u64) {
        let mut wallet_db = open_wallet_db(history.wallet_db_path(), test_params()).unwrap();
        let ufvk = sapling_ufvk(&history.extfvk).unwrap();
        import_wallet_account(
            &mut wallet_db,
            &ufvk,
            ChainState::empty(BlockHeight::from_u32(start_height as u32 - 1), BlockHash([7u8; 32])),
        )
        .unwrap();
    }

    fn insert_history_fixture(history: &ZCoinShieldedHistory) {
        let mut wallet_db = open_wallet_db(history.wallet_db_path(), test_params()).unwrap();
        if wallet_db.get_account_ids().unwrap().is_empty() {
            let ufvk = sapling_ufvk(&history.extfvk).unwrap();
            import_wallet_account(
                &mut wallet_db,
                &ufvk,
                ChainState::empty(BlockHeight::from_u32(10), BlockHash([10u8; 32])),
            )
            .unwrap();
        }
        wallet_db.update_chain_tip(BlockHeight::from_u32(11)).unwrap();
        drop(wallet_db);

        let conn = Connection::open(history.wallet_db_path()).unwrap();
        let account_id: i64 = conn
            .query_row("SELECT id FROM accounts LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let address_id: i64 = conn
            .query_row(
                "SELECT id FROM addresses WHERE account_id = ?1 LIMIT 1",
                params![account_id],
                |row| row.get(0),
            )
            .unwrap();
        let empty_tree = empty_sapling_tree_bytes();
        conn.execute(
            "INSERT INTO blocks (
                height, hash, time, sapling_tree, sapling_commitment_tree_size, sapling_output_count
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![10u32, vec![10u8; 32], 1000u32, &empty_tree, 0u32, 0u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO blocks (
                height, hash, time, sapling_tree, sapling_commitment_tree_size, sapling_output_count
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![11u32, vec![11u8; 32], 1100u32, &empty_tree, 0u32, 0u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transactions (
                id_tx, txid, block, mined_height, tx_index, min_observed_height
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![1i64, FIXTURE_RECEIVE_TXID_INTERNAL.to_vec(), 10u32, 10u32, 0u32, 10u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transactions (
                id_tx, txid, block, mined_height, tx_index, min_observed_height
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![2i64, FIXTURE_SPEND_TXID_INTERNAL.to_vec(), 11u32, 11u32, 0u32, 11u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sapling_received_notes (
                transaction_id, output_index, account_id, diversifier, value, rcm, nf,
                is_change, commitment_tree_position, recipient_key_scope, address_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                1i64,
                0u32,
                account_id,
                vec![3u8; 11],
                125_000_000i64,
                vec![4u8; 32],
                vec![5u8; 32],
                0u32,
                0u32,
                0u32,
                address_id
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sapling_received_notes (
                transaction_id, output_index, account_id, diversifier, value, rcm, nf,
                is_change, commitment_tree_position, recipient_key_scope, address_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                1i64,
                1u32,
                account_id,
                vec![6u8; 11],
                25_000_000i64,
                vec![7u8; 32],
                vec![8u8; 32],
                0u32,
                1u32,
                0u32,
                address_id
            ],
        )
        .unwrap();
        let spent_note_id: i64 = conn
            .query_row(
                "SELECT id FROM sapling_received_notes WHERE transaction_id = 1 AND output_index = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO sapling_received_note_spends (sapling_received_note_id, transaction_id)
             VALUES (?1, ?2)",
            params![spent_note_id, 2i64],
        )
        .unwrap();
    }

    /// Recording a broadcast this wallet made inserts a `transactions` row while
    /// the transaction is still unmined (CRD ch.39 R39.8.0ap). That row carries no
    /// received note, so counting it as scanned would drop the in-flight exclusion
    /// early and hide the transaction's unconfirmed change from the pending-receipt
    /// balance (R39.8.0ah). Only a mined height means the block was scanned.
    #[test]
    fn unmined_transaction_row_is_not_reported_as_scanned() {
        const UNMINED_TXID: [u8; 32] = [0xab; 32];

        let history = open_test_history();
        insert_history_fixture(&history);

        let conn = Connection::open(history.wallet_db_path()).unwrap();
        conn.execute(
            "INSERT INTO transactions (
                id_tx, txid, block, mined_height, tx_index, min_observed_height
             ) VALUES (?1, ?2, NULL, NULL, NULL, ?3)",
            params![99i64, UNMINED_TXID.to_vec(), 11u32],
        )
        .unwrap();
        drop(conn);

        assert!(
            !history.transaction_is_scanned(&UNMINED_TXID).unwrap(),
            "a recorded but unmined transaction must not count as scanned"
        );
        assert!(
            history.transaction_is_scanned(&FIXTURE_RECEIVE_TXID_INTERNAL).unwrap(),
            "a mined transaction must count as scanned"
        );
    }

    #[test]
    fn empty_wallet_uses_recent_lightwalletd_start_when_no_start_requested() {
        let history = open_test_history();
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, None, false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 10_000 - DEFAULT_LIGHT_WALLETD_RECENT_SCAN_BLOCKS,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: false,
            })
        );
    }

    #[test]
    fn empty_wallet_honors_explicit_start_above_sapling_activation() {
        let history = open_test_history();
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, Some(7_000), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 7_000,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: false,
            })
        );
    }

    #[test]
    fn existing_wallet_state_resumes_from_scanned_height() {
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 100, None, false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 43,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: false,
            })
        );
    }

    #[test]
    fn existing_wallet_state_later_explicit_start_rebuilds() {
        // A start later than the wallet's current sync anchor differs from it, so
        // activation rewinds/recreates and rescans from the requested start rather
        // than silently reusing the older, wider scan (R39.8.0h).
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, Some(7_000), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 7_000,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: true,
            })
        );
    }

    #[test]
    fn existing_wallet_state_earlier_explicit_start_triggers_reset() {
        // A start earlier than the wallet anchor requires rewinding and re-seeding
        // to obtain the missing earlier history (R39.8.0h).
        let history = open_checkpointed_test_history(2_000);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, Some(1_000), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 1_000,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: true,
            })
        );
    }

    #[test]
    fn matching_explicit_start_resumes_without_reset() {
        // A requested start equal to the wallet's current sync anchor (anchor + 1)
        // reuses local state and resumes from the scanned tip — no rescan on an
        // unchanged re-activation.
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 100, Some(43), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 43,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: false,
            })
        );
    }

    #[test]
    fn skip_sync_params_resumes_from_local_state_ignoring_requested_start() {
        // With existing local sync state and `skip_sync_params` set, a differing
        // requested start is ignored and the scan resumes from the scanned tip
        // rather than rewinding (R39.6.2).
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 100, Some(7_000), true)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 43,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: false,
            })
        );
    }

    #[test]
    fn fully_scanned_wallet_reuses_state_when_requested_start_matches_anchor() {
        // A wallet already scanned through the tip with an unchanged requested
        // start has nothing to do.
        let history = open_test_history();
        insert_history_fixture(&history);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 11, Some(11), false)
            .unwrap();
        assert_eq!(plan, None);
    }

    #[test]
    fn fully_scanned_wallet_rebuilds_when_requested_start_differs() {
        // Regression: a wallet already scanned through the tip must still rewind
        // when the caller changes the requested sync start, instead of
        // short-circuiting to "nothing to do" and reusing the stale cache.
        let history = open_test_history();
        insert_history_fixture(&history);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 11, Some(5), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 5,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: true,
            })
        );
    }

    #[test]
    fn explicit_start_beyond_tip_is_clamped_to_tip_and_rebuilds() {
        // A start past the current tip is clamped to the tip. Since that still
        // differs from the wallet's anchor, it rebuilds and scans from the tip
        // (an empty, near-instant scan), matching "sync from a future point".
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, Some(7_000_000), false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 10_000,
                reset_stale_empty_checkpoint: false,
                reset_scan_state: true,
            })
        );
    }

    #[test]
    fn stale_empty_checkpoint_uses_recent_start_and_requests_reset() {
        let history = open_checkpointed_test_history(42);
        let plan = history
            .lightwalletd_fetch_plan(&test_params(), 10_000, None, false)
            .unwrap();
        assert_eq!(
            plan,
            Some(LightwalletdFetchPlan {
                start_height: 10_000 - DEFAULT_LIGHT_WALLETD_RECENT_SCAN_BLOCKS,
                reset_stale_empty_checkpoint: true,
                reset_scan_state: false,
            })
        );
    }

    /// A caller-supplied sync start must survive activations that do not repeat it.
    ///
    /// Regression: a wallet part-way through a deep rescan is indistinguishable
    /// from a fresh one -- empty, anchored far behind the recent window -- until
    /// the rescan reaches its first transaction. The reseed therefore fired on the
    /// next activation carrying no `sync_params` and jumped the wallet forward to
    /// the recent window, silently discarding the rescan. Observed in the wild: a
    /// rescan requested at 02:02:11 was wiped 53 seconds later. The damaging case
    /// is restoring a seed whose funds predate the recent window -- the rescan that
    /// would have found them is thrown away and the balance stays at zero
    /// (R39.8.0an).
    #[test]
    fn a_requested_sync_start_is_not_discarded_by_the_reseed() {
        // Born at the coin configuration's checkpoint: the reseed is correct here,
        // because this wallet has never been anchored anywhere deliberately.
        let fresh = open_checkpointed_test_history(42);
        let plan = fresh
            .lightwalletd_fetch_plan(&test_params(), 10_000, None, false)
            .unwrap();
        assert_eq!(
            plan.unwrap().reset_stale_empty_checkpoint,
            true,
            "a wallet still at its birth anchor should skip long-dead history"
        );

        // Now re-anchor it deliberately, as a caller-supplied sync start does.
        let requested_start = 5_000;
        let rescan = fresh
            .lightwalletd_fetch_plan(&test_params(), 10_000, Some(requested_start), false)
            .unwrap()
            .expect("an explicit start must produce a plan");
        assert_eq!(rescan.start_height, requested_start);
        assert!(rescan.reset_scan_state, "a changed anchor rebuilds the wallet");
        fresh.reset_wallet_scan_state().unwrap();
        seed_wallet_anchor(&fresh, requested_start);

        // The next activation carries no sync_params -- exactly what a GUI sends on
        // restart. The rescan must survive it.
        let plan = fresh
            .lightwalletd_fetch_plan(&test_params(), 10_000, None, false)
            .unwrap()
            .expect("a plan is still required");
        assert!(
            !plan.reset_stale_empty_checkpoint,
            "a deliberately re-anchored wallet must not be reseeded forward"
        );
        assert_eq!(
            plan.start_height, requested_start,
            "the rescan must resume from the requested start, not jump to the recent window"
        );
    }

    #[test]
    fn reset_empty_wallet_scan_state_clears_checkpoint_and_compact_cache() {
        let history = open_checkpointed_test_history(42);
        let block = zcash_compact::CompactBlock {
            height: 43,
            ..Default::default()
        };
        history.insert_compact_block(block).unwrap();

        assert_eq!(history.scanned_height().unwrap(), Some(42));
        history.reset_empty_wallet_scan_state().unwrap();
        assert_eq!(history.scanned_height().unwrap(), None);

        let compact_conn = Connection::open(history.compact_blocks_path()).unwrap();
        let compact_count: u32 = compact_conn
            .query_row("SELECT COUNT(*) FROM compactblocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(compact_count, 0);
    }

    #[test]
    fn open_or_create_initializes_public_zcash_wallet_schema() {
        let history = open_test_history();
        assert!(history.compact_blocks_path().exists());
        assert!(history.wallet_db_path().exists());
        assert_eq!(
            history.compact_blocks_path().file_name().unwrap(),
            "ARRR_RELOADED_COMPACT_BLOCKS.db"
        );
        assert_eq!(history.wallet_db_path().file_name().unwrap(), "ARRR_RELOADED_WALLET.db");

        let conn = Connection::open(history.wallet_db_path()).unwrap();
        for table in [
            "accounts",
            "blocks",
            "transactions",
            "sapling_received_notes",
            "sent_notes",
            "sapling_tree_shards",
            "schemer_migrations",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing table {table}");
        }
        let user_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(user_version, 8);
    }

    /// A shielded transaction ID must be reported in the same byte order as every
    /// block explorer, every other KDF coin's history, and this coin's own
    /// `withdraw` response -- big-endian display order.
    ///
    /// Regression: `load_page` hex-encoded the raw `transactions.txid` column,
    /// which `zcash_client_sqlite` stores in internal little-endian order. The
    /// resulting `z_coin_tx_history` `tx_hash` resolved on no explorer and
    /// disagreed with the ID `send_raw_transaction` returned for the very same
    /// transaction. Reproduced live on ARRR mainnet 2026-09-18 with exactly the
    /// two transactions used as fixtures here.
    #[test]
    fn tx_history_reports_txids_in_explorer_display_order() {
        // The stored bytes and the displayed string are byte reversals of each
        // other -- pin that, so neither constant can drift alone.
        let mut reversed = FIXTURE_RECEIVE_TXID_INTERNAL;
        reversed.reverse();
        assert_eq!(hex::encode(reversed), FIXTURE_RECEIVE_TXID_DISPLAY);
        let mut reversed = FIXTURE_SPEND_TXID_INTERNAL;
        reversed.reverse();
        assert_eq!(hex::encode(reversed), FIXTURE_SPEND_TXID_DISPLAY);

        assert_eq!(
            encode_display_txid(FIXTURE_RECEIVE_TXID_INTERNAL.to_vec()),
            FIXTURE_RECEIVE_TXID_DISPLAY
        );

        let history = open_test_history();
        insert_history_fixture(&history);
        let page = history
            .load_page(
                "ARRR",
                "zs-wallet",
                8,
                12,
                &PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
                10,
            )
            .unwrap();

        let rendered: Vec<&str> = page.transactions.iter().map(|t| t.tx_hash.as_str()).collect();
        assert_eq!(rendered, vec![FIXTURE_SPEND_TXID_DISPLAY, FIXTURE_RECEIVE_TXID_DISPLAY]);
        // Guard the specific failure: the raw internal order must never surface.
        for tx in &page.transactions {
            assert_ne!(tx.tx_hash, hex::encode(FIXTURE_RECEIVE_TXID_INTERNAL));
            assert_ne!(tx.tx_hash, hex::encode(FIXTURE_SPEND_TXID_INTERNAL));
        }
    }

    #[test]
    fn load_page_returns_newest_first_wallet_scan_history() {
        let history = open_test_history();
        insert_history_fixture(&history);

        let page = history
            .load_page(
                "ARRR",
                "zs-wallet",
                8,
                12,
                &PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
                10,
            )
            .unwrap();

        assert_eq!(page.total, 2);
        assert_eq!(page.transactions[0].internal_id, 2);
        // Newest first, so index 0 is the spend and index 1 the receive. Both
        // must be rendered in explorer display order, not the internal
        // little-endian order the wallet database stores (R39.8.7).
        assert_eq!(page.transactions[0].tx_hash, FIXTURE_SPEND_TXID_DISPLAY);
        assert_eq!(page.transactions[1].tx_hash, FIXTURE_RECEIVE_TXID_DISPLAY);
        assert_eq!(
            page.transactions[0].spent_by_me,
            BigDecimal::from(25) / BigDecimal::from(100)
        );
        assert_eq!(page.transactions[0].received_by_me, BigDecimal::from(0));
        assert_eq!(
            page.transactions[0].my_balance_change,
            BigDecimal::from(-25) / BigDecimal::from(100)
        );
        assert_eq!(page.transactions[0].from, vec!["zs-wallet"]);
        assert_eq!(page.transactions[0].confirmations, 2);

        assert_eq!(page.transactions[1].internal_id, 1);
        assert_eq!(
            page.transactions[1].received_by_me,
            BigDecimal::from(15) / BigDecimal::from(10)
        );
        assert!(page.transactions[1].to.contains(&"zs-wallet".to_owned()));
        assert_eq!(page.transactions[1].confirmations, 3);
    }

    #[test]
    fn balance_uses_unspent_mined_received_notes() {
        let history = open_test_history();
        insert_history_fixture(&history);

        assert_eq!(history.balance(test_params()).unwrap(), 125_000_000);
    }

    #[test]
    fn from_id_unknown_is_storage_error() {
        let history = open_test_history();
        insert_history_fixture(&history);

        let err = history
            .load_page("ARRR", "zs-wallet", 8, 12, &PagingOptionsEnum::FromId(999), 10)
            .unwrap_err();

        assert!(err.contains("Unknown shielded transaction history internal_id 999"));
    }

    #[test]
    fn initialized_but_unscanned_wallet_db_does_not_reach_activation_tip() {
        let history = open_checkpointed_test_history(10);
        assert_eq!(history.scanned_height().unwrap(), Some(10));

        let err = history
            .scan_cached_blocks_to_height(test_params(), 11, 1_000, 0, |_, _| {})
            .unwrap_err();
        assert!(err.contains("below activation tip 11"));
    }

    #[test]
    fn checkpoint_at_activation_tip_counts_as_scanned_through_tip() {
        let history = open_checkpointed_test_history(10);
        let scanned = history
            .scan_cached_blocks_to_height(test_params(), 10, 1_000, 0, |_, _| {})
            .unwrap();
        assert_eq!(scanned, 10);
    }

    /// Builds a `TreeState` for the checkpoint-validation tests. Height 10 is above
    /// `test_params()`'s Sapling activation of 2, so the empty-tree shortcut does
    /// not apply and every payload is validated on its merits.
    fn tree_state_with_tree(tree_hex: &str) -> z_coin_grpc::TreeState {
        z_coin_grpc::TreeState {
            network: "main".to_owned(),
            height: 10,
            hash: hex::encode((0u8..32).collect::<Vec<_>>()),
            time: 10,
            tree: tree_hex.to_owned(),
            ..Default::default()
        }
    }

    /// A bare 32-byte commitment root is not a commitment tree. Pirate's
    /// lightwalletd substitutes one whenever the node cannot supply a frontier, so
    /// the wallet must refuse it rather than anchor on it.
    /// A wallet that syncs to the chain tip records the tip block. An ordinary
    /// one-block reorg then leaves it holding a block the network has dropped, and
    /// the tree state the servers return for that height no longer matches.
    ///
    /// Regression: that mismatch was a hard error with no rewind, so every later
    /// activation repeated the same comparison and the wallet could never sync
    /// again -- observed in the wild as "all lightwalletd servers failed", with all
    /// reachable servers agreeing precisely because it was the wallet that was
    /// stale. The divergence must instead be reported, rewound past, and
    /// re-anchored (R39.8.0al).
    #[test]
    fn a_reorged_scan_tip_is_rewound_past_instead_of_failing_forever() {
        let params = test_params_with_zip212();
        let db_dir = test_db_dir("reorged-scan-tip");
        let extfvk = test_extfvk(21);
        let check_point = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 1234,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), params.clone(), &extfvk, Some(&check_point))
                .unwrap();

        // Note-bearing blocks, so each one advances the commitment tree and leaves
        // a checkpoint the backend can later rewind to. A rewind target without a
        // checkpoint is refused outright.
        let other = test_extfvk(22);
        let blocks: Vec<_> = (11..=30)
            .map(|height| {
                let mut block = compact_block_with_received_note(height, 0, 0, &other, 1_000);
                // The fixture hardcodes one txid, which collides across blocks on
                // `tx_locator_map`'s uniqueness constraint.
                block.vtx[0].txid = deterministic_compact_hash(height + 10_000);
                block.hash = deterministic_compact_hash(height);
                block.prev_hash = if height == 11 {
                    vec![9u8; 32]
                } else {
                    deterministic_compact_hash(height - 1)
                };
                block.chain_metadata = None;
                block
            })
            .collect();
        history.insert_compact_blocks(&blocks).unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), 30, 1_000, 0, |_, _| {})
                .unwrap(),
            30
        );

        // The tree state the chain reports for the scan tip, built from the
        // wallet's own frontier so that only the block hash can disagree.
        let tip_state = history.initial_chain_state.lock().as_ref().unwrap().clone();
        let tip_tree = sapling::CommitmentTree::from_frontier(tip_state.final_sapling_tree());
        let mut tip_tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&tip_tree, &mut tip_tree_bytes).unwrap();

        // The reorg: the chain's block 30 is not the block the wallet scanned.
        let mut reorged_hash = deterministic_compact_hash(30);
        reorged_hash[0] ^= 0xff;
        reorged_hash.reverse(); // a TreeState hash is in display order
        let reorged_tip = z_coin_grpc::TreeState {
            network: "main".to_owned(),
            height: 30,
            hash: hex::encode(&reorged_hash),
            time: 30,
            tree: hex::encode(&tip_tree_bytes),
            ..Default::default()
        };

        drop(history);
        let reopened = ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &extfvk, None).unwrap();

        // Before the fix this was an unrecoverable error, repeated on every retry.
        assert_eq!(
            reopened
                .init_wallet_checkpoint_from_tree_state(params.clone(), reorged_tip)
                .unwrap(),
            CheckpointOutcome::Diverged
        );
        assert!(
            reopened.initial_chain_state.lock().is_none(),
            "a diverged tip must never become the anchor"
        );

        // Rewind past the divergence, as ensure_lightwalletd_chain_state does.
        let rewound = reopened.truncate_wallet_to_height(20).unwrap();
        assert!(rewound <= 20, "rewind must not stop above the request, got {}", rewound);
        assert!(rewound < 30, "rewind must drop the reorged tip");
        assert_eq!(
            reopened.scanned_block_height().unwrap(),
            Some(rewound),
            "the wallet's scan tip must follow the rewind"
        );

        // Re-anchoring at the rewound height now succeeds, so the next fetch
        // resumes there and rescans the reorged range.
        let mut rewound_db = open_wallet_db(reopened.wallet_db_path(), params.clone()).unwrap();
        let rewound_state =
            wallet_chain_state_before(&mut rewound_db, BlockHeight::from_u32(rewound as u32 + 1)).unwrap();
        drop(rewound_db);
        let rewound_tree = sapling::CommitmentTree::from_frontier(rewound_state.final_sapling_tree());
        let mut rewound_tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&rewound_tree, &mut rewound_tree_bytes).unwrap();
        let mut rewound_display = deterministic_compact_hash(rewound);
        rewound_display.reverse();
        let rewound_tip = z_coin_grpc::TreeState {
            network: "main".to_owned(),
            height: rewound,
            hash: hex::encode(rewound_display),
            time: rewound as u32,
            tree: hex::encode(rewound_tree_bytes),
            ..Default::default()
        };
        assert_eq!(
            reopened
                .init_wallet_checkpoint_from_tree_state(params, rewound_tip)
                .unwrap(),
            CheckpointOutcome::Accepted,
            "the wallet must re-anchor once it has rewound past the divergence"
        );
        assert!(reopened.initial_chain_state.lock().is_some());
    }

    /// Serves fixed tree states by height and records every height asked for.
    struct FixtureTreeStates {
        states: HashMap<u64, z_coin_grpc::TreeState>,
        requested: Vec<u64>,
    }

    impl TreeStateSource for FixtureTreeStates {
        async fn tree_state(&mut self, height: u64) -> Result<z_coin_grpc::TreeState, String> {
            self.requested.push(height);
            self.states
                .get(&height)
                .cloned()
                .ok_or_else(|| format!("fixture has no tree state at height {}", height))
        }
    }

    /// Height of the checkpoint block every reorg fixture anchors at.
    const FIXTURE_CHECKPOINT_HEIGHT: u64 = 10;
    /// The block of the fixture chain that pays the wallet itself, when the
    /// fixture wants a note. A wallet note with a recorded commitment-tree position
    /// constrains where the backend can truncate: it cannot rewind to any height at
    /// or above the note unless that height still has a checkpoint of its own. A
    /// note in the first scanned block therefore leaves no truncation target below
    /// the retained checkpoints, so the backend refuses the request; a note in a
    /// later block lets it fall back to the block just before the note.
    const FIXTURE_FIRST_BLOCK_HEIGHT: u64 = FIXTURE_CHECKPOINT_HEIGHT + 1;
    /// In the sparse fixture only heights ending in 2 carry an output.
    const FIXTURE_OWN_SPARSE_NOTE_HEIGHT: u64 = 12;
    const FIXTURE_OWN_NOTE_VALUE: u64 = 125_000_000;
    /// Fork height of a chain that never forks.
    const FIXTURE_NO_FORK: u64 = u64::MAX;

    /// Hash of the fixture chain's block at `height` on `branch` (0 is the chain
    /// the wallet scanned, 1 the chain that replaced its tail).
    fn fixture_hash(height: u64, branch: u8) -> Vec<u8> {
        let mut hash = deterministic_compact_hash(height);
        hash[31] = branch;
        hash
    }

    /// The block at `height` of a chain that follows branch 0 below `fork_height`
    /// and branch 1 from it on. `with_output` blocks carry a Sapling output, which
    /// is what gives a block a commitment-tree checkpoint.
    fn fixture_block(
        height: u64,
        fork_height: u64,
        own_note_height: u64,
        with_output: bool,
    ) -> zcash_compact::CompactBlock {
        let branch_of = |h: u64| u8::from(h >= fork_height);
        let mut block = if with_output {
            let key = if height == own_note_height {
                test_extfvk(23)
            } else {
                test_extfvk(24)
            };
            let value = if height == own_note_height {
                FIXTURE_OWN_NOTE_VALUE
            } else {
                1
            };
            let mut block = compact_block_with_received_note(height, 0, 0, &key, value);
            // The fixture hardcodes one txid, which collides across blocks on
            // `tx_locator_map`'s uniqueness constraint.
            block.vtx[0].txid = fixture_hash(height + 10_000, branch_of(height));
            block
        } else {
            empty_compact_block(height, 0, 0)
        };
        block.hash = fixture_hash(height, branch_of(height));
        block.prev_hash = if height == FIXTURE_CHECKPOINT_HEIGHT + 1 {
            vec![9u8; 32]
        } else {
            fixture_hash(height - 1, branch_of(height - 1))
        };
        block.chain_metadata = None;
        block
    }

    fn fixture_blocks(
        heights: std::ops::RangeInclusive<u64>,
        fork_height: u64,
        own_note_height: u64,
        with_output: impl Fn(u64) -> bool,
    ) -> Vec<zcash_compact::CompactBlock> {
        heights
            .map(|height| fixture_block(height, fork_height, own_note_height, with_output(height)))
            .collect()
    }

    /// A tree state as lightwalletd reports it: hash in display order, the
    /// commitment tree after that block.
    fn fixture_tree_state(height: u64, hash: &[u8], state: &ChainState) -> z_coin_grpc::TreeState {
        let tree = sapling::CommitmentTree::from_frontier(state.final_sapling_tree());
        let mut tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&tree, &mut tree_bytes).unwrap();
        let mut display_hash = hash.to_vec();
        display_hash.reverse();
        z_coin_grpc::TreeState {
            network: "main".to_owned(),
            height,
            hash: hex::encode(display_hash),
            time: height as u32,
            tree: hex::encode(tree_bytes),
            ..Default::default()
        }
    }

    /// The same block, as the chain reports it after a reorg replaced it.
    fn reorged_tree_state(state: &z_coin_grpc::TreeState) -> z_coin_grpc::TreeState {
        let mut reorged = state.clone();
        let mut display_hash = hex::decode(&state.hash).unwrap();
        display_hash.reverse();
        display_hash[31] = 1;
        display_hash.reverse();
        reorged.hash = hex::encode(display_hash);
        reorged
    }

    struct ScannedFixture {
        history: ZCoinShieldedHistory,
        params: ZcoinConsensusParams,
        db_dir: PathBuf,
        own_note_height: u64,
        /// What the chain reports at the checkpoint and at every scanned height,
        /// all on the original chain.
        chain: HashMap<u64, z_coin_grpc::TreeState>,
    }

    /// Scans fixture blocks `11..=tip` in one session and returns the history still
    /// open -- its in-memory chain state at `tip`, as a running wallet has it.
    fn scanned_fixture(
        label: &str,
        tip: u64,
        own_note_height: u64,
        with_output: impl Fn(u64) -> bool,
    ) -> ScannedFixture {
        let params = test_params_with_zip212();
        let extfvk = test_extfvk(23);
        let empty_tree = empty_sapling_tree_bytes();
        let check_point = CheckPointBlockInfo {
            height: FIXTURE_CHECKPOINT_HEIGHT as u32,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 1234,
            sapling_tree: empty_tree.clone().into(),
        };
        let db_dir = test_db_dir(label);
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), params.clone(), &extfvk, Some(&check_point))
                .unwrap();
        history
            .insert_compact_blocks(&fixture_blocks(
                11..=tip,
                FIXTURE_NO_FORK,
                own_note_height,
                &with_output,
            ))
            .unwrap();

        // The chain's tree state at the checkpoint and at each scanned height,
        // walked forward from the checkpoint over the same blocks the wallet scans.
        let block_db = BlockDb::for_path(history.compact_blocks_path()).unwrap();
        let mut state = chain_state_from_checkpoint(&check_point).unwrap();
        let mut chain = HashMap::new();
        chain.insert(
            FIXTURE_CHECKPOINT_HEIGHT,
            fixture_tree_state(FIXTURE_CHECKPOINT_HEIGHT, &[9u8; 32], &state),
        );
        for height in 11..=tip {
            state = validated_cached_block_chain_state(&block_db, BlockHeight::from_u32(height as u32), &state, 1)
                .unwrap()
                .unwrap();
            chain.insert(height, fixture_tree_state(height, &fixture_hash(height, 0), &state));
        }

        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), tip, 1_000, 0, |_, _| {})
                .unwrap(),
            tip
        );
        assert_eq!(
            in_memory_anchor(&history),
            Some(tip),
            "the scan must leave its tip in memory -- the state the anchor check used to trust"
        );
        assert_eq!(history.balance(params.clone()).unwrap(), FIXTURE_OWN_NOTE_VALUE);
        ScannedFixture {
            history,
            params,
            db_dir,
            own_note_height,
            chain,
        }
    }

    fn in_memory_anchor(history: &ZCoinShieldedHistory) -> Option<u64> {
        history
            .initial_chain_state
            .lock()
            .as_ref()
            .map(|state| u64::from(u32::from(state.block_height())))
    }

    fn checkpoint_ids(history: &ZCoinShieldedHistory) -> Vec<u64> {
        let conn = Connection::open(history.wallet_db_path()).unwrap();
        let mut statement = conn
            .prepare("SELECT checkpoint_id FROM sapling_tree_checkpoints ORDER BY checkpoint_id")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, u64>(0))
            .unwrap()
            .map(|id| id.unwrap())
            .collect()
    }

    fn cached_block_heights(history: &ZCoinShieldedHistory) -> Vec<u64> {
        let conn = Connection::open(history.compact_blocks_path()).unwrap();
        let mut statement = conn
            .prepare("SELECT height FROM compactblocks ORDER BY height")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, u64>(0))
            .unwrap()
            .map(|height| height.unwrap())
            .collect()
    }

    fn backup_files(db_dir: &Path) -> Vec<String> {
        std::fs::read_dir(db_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".bak"))
            .collect()
    }

    fn wallet_account_count(history: &ZCoinShieldedHistory, params: &ZcoinConsensusParams) -> usize {
        open_wallet_db(history.wallet_db_path(), params.clone())
            .unwrap()
            .get_account_ids()
            .unwrap()
            .len()
    }

    fn wallet_block_hash(history: &ZCoinShieldedHistory, params: &ZcoinConsensusParams, height: u64) -> Vec<u8> {
        open_wallet_db(history.wallet_db_path(), params.clone())
            .unwrap()
            .get_block_hash(BlockHeight::from_u32(height as u32))
            .unwrap()
            .unwrap()
            .0
            .to_vec()
    }

    /// What one pass over a wallet whose scan tip was reorged did: where it
    /// re-anchored, and the oldest checkpoint the wallet held before the pass.
    struct ReorgedPass {
        anchor: u64,
        oldest_checkpoint: u64,
    }

    /// The observed wedge: an ARRR wallet kept current by the 30-second sync met a
    /// one-block reorg of its own tip. This drives one pass the way the sync does
    /// -- anchor check, cache resume, fetch, scan -- against the exact stuck state,
    /// and demands recovery rather than detection alone. `own_note_height` places
    /// the wallet's one note, which decides how the backend answers the rewind.
    fn run_pass_over_a_reorged_scan_tip(label: &str, own_note_height: u64) -> ReorgedPass {
        let tip = 160;
        let ScannedFixture {
            history,
            params,
            db_dir,
            mut chain,
            ..
        } = scanned_fixture(label, tip, own_note_height, |_| true);
        let checkpoints = checkpoint_ids(&history);
        let oldest_checkpoint = checkpoints[0];
        assert_eq!(
            *checkpoints.last().unwrap(),
            tip,
            "every block is checkpointed up to the tip"
        );
        // The backend keeps exactly 100 checkpoints, one per block here, so the
        // oldest sits at `tip - 99`. `ZCOIN_REORG_REWIND_DEPTH` relies on this; a
        // dependency bump that changes the window must fail here, not in the field.
        assert_eq!(checkpoints.len(), 100, "retained checkpoint window: {:?}", checkpoints);
        assert_eq!(oldest_checkpoint, tip - 99, "retained checkpoint window moved");

        // The reorg replaced the wallet's tip: the chain's block `tip` differs and
        // the cache holds the first blocks of the new chain on top of the old tip.
        chain.insert(tip, reorged_tree_state(&chain[&tip]));
        history
            .insert_compact_blocks(&fixture_blocks(tip + 1..=tip + 2, tip, own_note_height, |_| true))
            .unwrap();
        let stuck = history
            .scan_cached_blocks_to_height(params.clone(), tip + 2, 1_000, 0, |_, _| {})
            .unwrap_err();
        assert!(
            stuck.contains(&format!("hash discontinuity at height {}", tip + 1)),
            "{stuck}"
        );

        let mut source = FixtureTreeStates {
            states: chain,
            requested: Vec::new(),
        };
        let anchor =
            common::block_on(history.ensure_lightwalletd_chain_state(&mut source, params.clone(), tip)).unwrap();

        assert_eq!(
            source.requested.first(),
            Some(&tip),
            "the in-memory state at the scan tip must not stand in for the chain's view"
        );
        assert!(
            anchor > FIXTURE_CHECKPOINT_HEIGHT && anchor < tip,
            "the wallet must rewind below the reorged block, but anchored at {}",
            anchor
        );
        assert_eq!(
            source.requested,
            vec![tip, anchor],
            "one rewind suffices and nothing is rebuilt (a rebuild would ask for the sync-start anchor)"
        );
        assert_eq!(history.scanned_block_height().unwrap(), Some(anchor));
        assert_eq!(wallet_account_count(&history, &params), 1, "a rewind keeps the account");
        assert_eq!(in_memory_anchor(&history), Some(anchor));
        assert_eq!(
            cached_block_heights(&history),
            (FIXTURE_FIRST_BLOCK_HEIGHT..=anchor).collect::<Vec<_>>(),
            "the orphaned block and everything cached above the rewound height must be gone"
        );

        // The rest of the pass: nothing cached above the anchor, so fetching
        // resumes right after it, and the server's blocks chain onto the wallet.
        assert_eq!(
            history.compact_cache_resume_height(anchor + 1, tip + 2).unwrap(),
            anchor
        );
        history
            .insert_compact_blocks(&fixture_blocks(anchor + 1..=tip + 2, tip, own_note_height, |_| true))
            .unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), tip + 2, 1_000, 0, |_, _| {})
                .unwrap(),
            tip + 2
        );
        assert_eq!(history.scanned_block_height().unwrap(), Some(tip + 2));
        assert_eq!(
            wallet_block_hash(&history, &params, tip),
            fixture_hash(tip, 1),
            "the wallet must now hold the new chain's block at the old tip height"
        );
        assert_eq!(history.balance(params).unwrap(), FIXTURE_OWN_NOTE_VALUE);
        assert!(
            backup_files(&db_dir).is_empty(),
            "recovery must not leave backups: {:?}",
            backup_files(&db_dir)
        );
        ReorgedPass {
            anchor,
            oldest_checkpoint,
        }
    }

    /// Regression for the ARRR wedge (R39.8.0al, R39.8.0n). Three defects kept the
    /// wallet from syncing for days:
    ///
    /// - the scan leaves its last block in memory as the chain state, and the anchor
    ///   check returned early whenever that state sat at the requested height, so
    ///   the chain was never asked and nothing rewound;
    /// - the wallet is dense in checkpoints (one per block), so a 100-block rewind
    ///   from the tip lands just below the retained window; where the backend has no
    ///   height to truncate to there, it refuses, which recovery treated as fatal
    ///   and sent to a rebuild;
    /// - the compact cache still held the orphaned block, which the scanner was fed
    ///   again on every pass.
    ///
    /// Here the wallet's note is in the first scanned block, the case in which a
    /// rewind below the window would be refused. The rewind must land on the oldest
    /// retained checkpoint, inside the window, without a rebuild.
    #[test]
    fn a_reorged_scan_tip_is_rewound_inside_the_window_and_the_new_chain_scanned_in_one_pass() {
        let pass = run_pass_over_a_reorged_scan_tip("same-session-reorg-window", FIXTURE_FIRST_BLOCK_HEIGHT);
        assert_eq!(
            pass.anchor, pass.oldest_checkpoint,
            "a one-block reorg must rewind to the oldest retained checkpoint, not rebuild"
        );
    }

    /// The same wedge for a wallet whose first note is mined in a later block -- the
    /// shape of every long-lived field wallet. Below the retained window the backend
    /// would truncate to the block just before that note, so a rewind that left the
    /// window turned every one-block reorg into a rescan of the wallet's whole note
    /// history. The rewind must stay inside the window instead.
    #[test]
    fn a_reorged_scan_tip_of_a_wallet_with_older_notes_still_rewinds_inside_the_window() {
        let note_height = FIXTURE_FIRST_BLOCK_HEIGHT + 1;
        let pass = run_pass_over_a_reorged_scan_tip("same-session-reorg-older-note", note_height);
        assert_eq!(
            pass.anchor,
            pass.oldest_checkpoint,
            "the rewind must not fall below the window to the block before the wallet's first note ({})",
            note_height - 1
        );
    }

    /// The other half: asking the chain on every pass must still accept an
    /// unchanged chain and keep the anchor where it was.
    #[test]
    fn an_unchanged_chain_keeps_its_anchor_after_the_check() {
        let tip = 30;
        let ScannedFixture {
            history, params, chain, ..
        } = scanned_fixture("same-session-agree", tip, FIXTURE_FIRST_BLOCK_HEIGHT, |_| true);

        let mut source = FixtureTreeStates {
            states: chain,
            requested: Vec::new(),
        };
        let anchor = common::block_on(history.ensure_lightwalletd_chain_state(&mut source, params, tip)).unwrap();

        assert_eq!(anchor, tip);
        assert_eq!(source.requested, vec![tip]);
        assert_eq!(history.scanned_block_height().unwrap(), Some(tip));
        assert_eq!(in_memory_anchor(&history), Some(tip));
    }

    /// A wallet scanned with sparse checkpoints (an activation scan, where most
    /// blocks carry no output) has one at or below the rewind target, so the
    /// rewind lands directly and needs no redirect.
    #[test]
    fn a_reorg_in_a_sparse_checkpoint_wallet_rewinds_directly() {
        let tip = 160;
        // Only every tenth block carries an output, the wallet's own note among them.
        let ScannedFixture {
            history,
            params,
            db_dir,
            own_note_height,
            mut chain,
        } = scanned_fixture("sparse-reorg", tip, FIXTURE_OWN_SPARSE_NOTE_HEIGHT, |height| {
            height % 10 == 2
        });
        let checkpoints = checkpoint_ids(&history);
        let target = tip - ZCOIN_REORG_REWIND_DEPTH;
        let expected = checkpoints.iter().copied().filter(|id| *id <= target).max();
        let expected = expected.expect("fixture must keep a checkpoint at or below the rewind target");
        assert!(
            checkpoints.len() < (tip - FIXTURE_CHECKPOINT_HEIGHT) as usize,
            "fixture must be sparse: {:?}",
            checkpoints
        );

        chain.insert(tip, reorged_tree_state(&chain[&tip]));
        let mut source = FixtureTreeStates {
            states: chain,
            requested: Vec::new(),
        };
        let anchor =
            common::block_on(history.ensure_lightwalletd_chain_state(&mut source, params.clone(), tip)).unwrap();

        assert_eq!(
            anchor, expected,
            "the rewind lands on the newest checkpoint at or below the target"
        );
        assert_eq!(source.requested, vec![tip, anchor]);
        assert_eq!(history.scanned_block_height().unwrap(), Some(anchor));
        assert!(!cached_block_heights(&history).iter().any(|height| *height > anchor));

        history
            .insert_compact_blocks(&fixture_blocks(anchor + 1..=tip + 1, tip, own_note_height, |height| {
                height % 10 == 2
            }))
            .unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params, tip + 1, 1_000, 0, |_, _| {})
                .unwrap(),
            tip + 1
        );
        assert!(backup_files(&db_dir).is_empty());
    }

    /// A rewind below the oldest retained checkpoint lands on that checkpoint
    /// instead of failing, and the compact cache above it is cleared with it.
    #[test]
    fn a_refused_rewind_is_redirected_to_the_oldest_retained_checkpoint() {
        let tip = 160;
        let ScannedFixture { history, .. } =
            scanned_fixture("rewind-window", tip, FIXTURE_FIRST_BLOCK_HEIGHT, |_| true);
        let oldest_checkpoint = checkpoint_ids(&history)[0];
        // One block below the retained window. With the wallet's note in the first
        // scanned block the backend has nothing to truncate to there.
        let below_window = oldest_checkpoint - 1;
        // The backend itself refuses the request, naming that checkpoint as safe.
        {
            let mut probe = open_wallet_db(history.wallet_db_path(), test_params_with_zip212()).unwrap();
            match probe.truncate_to_height(BlockHeight::from_u32(below_window as u32)) {
                Err(SqliteClientError::RequestedRewindInvalid {
                    safe_rewind_height: Some(safe),
                    ..
                }) => assert_eq!(u64::from(u32::from(safe)), oldest_checkpoint),
                other => panic!("expected the backend to refuse the rewind, got {:?}", other),
            }
        }

        let rewound = history.truncate_wallet_to_height(below_window).unwrap();
        assert_eq!(rewound, oldest_checkpoint);
        assert_eq!(history.scanned_block_height().unwrap(), Some(oldest_checkpoint));
        assert_eq!(
            cached_block_heights(&history),
            (11..=oldest_checkpoint).collect::<Vec<_>>()
        );
    }

    /// A divergence beyond everything the wallet retains cannot be repaired by
    /// rewinding, so the wallet is rebuilt -- at its sync start, never at height 0.
    ///
    /// Before this, the rebuild re-anchored at 0 and refetched the chain from the
    /// first block into a wallet with no account.
    #[test]
    fn a_divergence_beyond_the_retained_window_rebuilds_at_the_sync_start() {
        let tip = 160;
        let fork = 20;
        let ScannedFixture {
            history,
            params,
            db_dir,
            own_note_height,
            mut chain,
        } = scanned_fixture("deep-reorg", tip, FIXTURE_FIRST_BLOCK_HEIGHT, |_| true);
        let oldest_checkpoint = checkpoint_ids(&history)[0];
        let sync_start = FIXTURE_CHECKPOINT_HEIGHT + 1;
        assert_eq!(history.wallet_sync_start_height().unwrap(), Some(sync_start));

        // The chain replaced everything from `fork` on, far below the retained window.
        for height in fork..=tip {
            let reorged = reorged_tree_state(&chain[&height]);
            chain.insert(height, reorged);
        }
        let mut source = FixtureTreeStates {
            states: chain,
            requested: Vec::new(),
        };
        let anchor =
            common::block_on(history.ensure_lightwalletd_chain_state(&mut source, params.clone(), tip)).unwrap();

        assert_eq!(
            anchor, FIXTURE_CHECKPOINT_HEIGHT,
            "the rebuild re-anchors at the sync start"
        );
        assert_eq!(
            source.requested,
            vec![tip, oldest_checkpoint, FIXTURE_CHECKPOINT_HEIGHT],
            "one redirected rewind, then the rebuild anchor; height 0 is never asked for"
        );
        assert_eq!(
            wallet_account_count(&history, &params),
            1,
            "the rebuilt wallet has its account"
        );
        assert_eq!(history.wallet_sync_start_height().unwrap(), Some(sync_start));
        assert_eq!(history.scanned_height().unwrap(), Some(FIXTURE_CHECKPOINT_HEIGHT));
        assert_eq!(history.scanned_block_height().unwrap(), None, "nothing is scanned yet");
        assert_eq!(in_memory_anchor(&history), Some(FIXTURE_CHECKPOINT_HEIGHT));
        assert_eq!(history.balance(params.clone()).unwrap(), 0);
        assert!(
            cached_block_heights(&history).is_empty(),
            "the compact cache starts empty"
        );
        let backups = backup_files(&db_dir);
        assert_eq!(backups.len(), 1, "exactly one wallet backup per rebuild: {:?}", backups);
        assert!(
            backups[0].starts_with("ARRR_RELOADED_WALLET.db.chain-divergence."),
            "{:?}",
            backups
        );

        // Fetching resumes at the sync start, and the new chain scans into the
        // rebuilt wallet, finding the note again.
        assert_eq!(
            history.compact_cache_resume_height(anchor + 1, tip + 5).unwrap(),
            FIXTURE_CHECKPOINT_HEIGHT
        );
        history
            .insert_compact_blocks(&fixture_blocks(anchor + 1..=tip + 5, fork, own_note_height, |_| true))
            .unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), tip + 5, 1_000, 0, |_, _| {})
                .unwrap(),
            tip + 5
        );
        assert_eq!(history.balance(params).unwrap(), FIXTURE_OWN_NOTE_VALUE);
        assert_eq!(backup_files(&db_dir), backups, "scanning creates no further backup");
    }

    /// The rebuild reads the sync start before it destroys anything. A wallet that
    /// records none is not rebuilt at a guessed height: the pass fails and the
    /// wallet is left exactly as it was.
    #[test]
    fn a_rebuild_without_a_recorded_sync_start_fails_and_leaves_the_wallet_alone() {
        let history = open_test_history();
        let db_dir = history.wallet_db_path().parent().unwrap().to_path_buf();
        assert_eq!(history.wallet_sync_start_height().unwrap(), None);
        let mut source = FixtureTreeStates {
            states: HashMap::new(),
            requested: Vec::new(),
        };

        let error = common::block_on(history.rebuild_at_sync_start(&mut source, test_params())).unwrap_err();

        assert!(error.contains("no recorded sync start"), "{error}");
        assert!(source.requested.is_empty(), "no height is guessed or requested");
        assert!(backup_files(&db_dir).is_empty());
    }

    /// A backend that cannot answer the rebuild's anchor query leaves the wallet in
    /// place: the old database is replaced only once the new anchor is in hand.
    #[test]
    fn a_rebuild_whose_anchor_cannot_be_fetched_leaves_the_wallet_alone() {
        let tip = 30;
        let ScannedFixture {
            history,
            params,
            db_dir,
            ..
        } = scanned_fixture("rebuild-no-anchor", tip, FIXTURE_FIRST_BLOCK_HEIGHT, |_| true);
        let mut source = FixtureTreeStates {
            states: HashMap::new(),
            requested: Vec::new(),
        };

        let error = common::block_on(history.rebuild_at_sync_start(&mut source, params.clone())).unwrap_err();

        assert!(error.contains("no tree state at height 10"), "{error}");
        assert_eq!(source.requested, vec![FIXTURE_CHECKPOINT_HEIGHT]);
        assert_eq!(history.scanned_block_height().unwrap(), Some(tip));
        assert_eq!(history.balance(params).unwrap(), FIXTURE_OWN_NOTE_VALUE);
        assert!(backup_files(&db_dir).is_empty());
    }

    /// A compact cache that does not link to the wallet's anchor is replaced, and
    /// the replaced one is deleted rather than kept beside it.
    ///
    /// Regression: each replacement used to be renamed to
    /// `*.invalid-chain.<ms>.bak`. When the failure repeats on every background
    /// pass, that left one more copy every 30 seconds -- about 2 GB over three
    /// days on ARRR mainnet (R39.8.0n).
    #[test]
    fn an_invalid_compact_cache_is_discarded_without_leaving_a_backup() {
        let history = open_checkpointed_test_history(10);
        // Block 11 does not link to the checkpoint's hash ([9; 32]).
        history.insert_compact_block(empty_compact_block(11, 11, 5)).unwrap();

        assert_eq!(history.compact_cache_resume_height(11, 20).unwrap(), 10);

        assert!(
            cached_block_heights(&history).is_empty(),
            "the invalid segment must not be reused"
        );
        let db_dir = history.compact_blocks_path().parent().unwrap();
        assert!(
            backup_files(db_dir).is_empty(),
            "compact cache backups left behind: {:?}",
            backup_files(db_dir)
        );
    }

    #[test]
    fn discard_compact_cache_removes_the_database_and_its_sidecars() {
        let path = test_db_dir("discard-compact-cache").join("ARRR_RELOADED_COMPACT_BLOCKS.db");
        std::fs::write(&path, b"db").unwrap();
        std::fs::write(path_with_suffix(&path, "-wal"), b"wal").unwrap();
        std::fs::write(path_with_suffix(&path, "-journal"), b"journal").unwrap();

        discard_compact_cache(&path).unwrap();

        assert!(!path.exists());
        for suffix in DATABASE_SIDECAR_SUFFIXES {
            assert!(!path_with_suffix(&path, suffix).exists(), "{} left behind", suffix);
        }
        // Nothing to delete is not an error.
        discard_compact_cache(&path).unwrap();
    }

    // Shapes measured against Pirate lightwalletd servers (mainnet activation
    // 152855, test chain activation 61), inlined.
    //   activation - 1: tree 64 hex chars, saplingFrontier empty  (substituted root)
    //   activation:     tree "000000",     saplingFrontier "000000" (real empty tree)
    //   deep height:    tree == saplingFrontier, 1028 hex chars     (real tree)

    #[test]
    fn substitution_discriminator_uses_the_frontier_not_the_length() {
        let root = [0x5cu8; 32];
        // 64-char tree, empty frontier: the substitution.
        assert!(is_substituted_commitment_root(&root, true));
        // A 32-byte tree beside a populated frontier is not the substitution shape.
        assert!(!is_substituted_commitment_root(&root, false));
        // The legitimate empty tree is short, whatever its frontier.
        let empty = empty_sapling_tree_bytes();
        assert!(!is_substituted_commitment_root(&empty, false));
        assert!(!is_substituted_commitment_root(&empty, true));
    }

    #[test]
    fn parse_checkpoint_tree_accepts_legitimate_empty_tree_with_matching_frontier() {
        let empty = empty_sapling_tree_bytes();
        let tree = parse_checkpoint_commitment_tree(
            &empty,
            false,
            BlockHeight::from_u32(152_855),
            BlockHeight::from_u32(152_855),
        )
        .unwrap();
        assert_eq!(tree.size(), 0);
    }

    #[test]
    fn parse_checkpoint_tree_accepts_a_real_tree_with_matching_frontier() {
        let mut tree = sapling::CommitmentTree::empty();
        tree.append(sapling::Node::from_scalar(bls12_381::Scalar::from(7u64)))
            .unwrap();
        let mut bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&tree, &mut bytes).unwrap();
        let parsed = parse_checkpoint_commitment_tree(
            &bytes,
            false,
            BlockHeight::from_u32(4_100_000),
            BlockHeight::from_u32(152_855),
        )
        .unwrap();
        assert_eq!(parsed.size(), 1);
    }

    #[test]
    fn parse_checkpoint_tree_rejects_substituted_root_above_activation_with_actionable_message() {
        let error = parse_checkpoint_commitment_tree(
            &[0x5cu8; 32],
            true,
            BlockHeight::from_u32(4_100_000),
            BlockHeight::from_u32(152_855),
        )
        .unwrap_err();
        assert!(error.contains("bare 32-byte commitment root"), "{}", error);
        assert!(error.contains("\"earliest\""), "{}", error);
        assert!(error.contains("152855"), "{}", error);
    }

    /// The `sync_params: "earliest"` defect: the anchor one block under Sapling
    /// activation comes back as a substituted root. Nothing exists in the Sapling
    /// tree before activation, so the empty tree is the correct anchor.
    #[test]
    fn parse_checkpoint_tree_below_activation_treats_substituted_root_as_empty_tree() {
        let tree = parse_checkpoint_commitment_tree(
            &[0x5cu8; 32],
            true,
            BlockHeight::from_u32(152_854),
            BlockHeight::from_u32(152_855),
        )
        .unwrap();
        assert_eq!(tree.size(), 0);
        // A 32-byte tree with a populated frontier below activation is not the
        // known substitution and stays refused.
        assert!(parse_checkpoint_commitment_tree(
            &[0x5cu8; 32],
            false,
            BlockHeight::from_u32(152_854),
            BlockHeight::from_u32(152_855),
        )
        .is_err());
    }

    #[test]
    fn tree_state_substituted_root_below_activation_is_accepted_as_anchor() {
        let history = open_test_history();
        let mut state = tree_state_with_tree(&hex::encode([0x5cu8; 32]));
        state.height = 1; // test_params() activates Sapling at 2
        assert!(state.sapling_frontier.is_empty());
        history
            .init_wallet_checkpoint_from_tree_state(test_params(), state)
            .unwrap();
        assert!(history.initial_chain_state.lock().is_some());
    }

    /// `earliest` resolves to the Sapling activation height, and any lower request
    /// (explicit height, or a date before activation) is floored to the same start,
    /// so all of them reach the same, now-accepted, anchor.
    #[test]
    fn earliest_and_pre_activation_requests_all_start_at_sapling_activation() {
        let history = open_test_history();
        for requested in [0u64, 1, 2] {
            let plan = history
                .lightwalletd_fetch_plan(&test_params(), 100, Some(requested), false)
                .unwrap()
                .unwrap();
            assert_eq!(plan.start_height, 2, "requested {}", requested);
        }
    }

    #[test]
    fn tree_state_bare_commitment_root_is_rejected() {
        let history = open_test_history();
        let root = hex::encode([0x7au8; 32]);
        assert_eq!(root.len(), 64);
        let error = history
            .init_wallet_checkpoint_from_tree_state(test_params(), tree_state_with_tree(&root))
            .unwrap_err();
        assert!(error.contains("bare 32-byte commitment root"), "{}", error);
        assert!(history.initial_chain_state.lock().is_none());
    }

    /// The dangerous variant: a root whose leading bytes happen to be zero parses
    /// *successfully* as an empty tree, because `read_commitment_tree` never checks
    /// that it consumed the whole buffer. Before the length guard this was accepted
    /// and anchored the wallet on a tree of size 0.
    #[test]
    fn tree_state_bare_root_of_leading_zeros_is_rejected_not_silently_empty() {
        let mut root = [0xabu8; 32];
        root[0] = 0;
        root[1] = 0;
        root[2] = 0;
        // Demonstrate the trap directly: the raw parse succeeds and yields an empty
        // tree, discarding 29 bytes.
        let parsed: sapling::CommitmentTree = read_commitment_tree(&root[..]).unwrap();
        assert_eq!(parsed.size(), 0, "fixture no longer reproduces the silent-empty parse");

        let history = open_test_history();
        let error = history
            .init_wallet_checkpoint_from_tree_state(test_params(), tree_state_with_tree(&hex::encode(root)))
            .unwrap_err();
        assert!(error.contains("bare 32-byte commitment root"), "{}", error);
        assert!(history.initial_chain_state.lock().is_none());
    }

    /// Anything appended after a well-formed tree means the payload is not what the
    /// server claimed; a partially consumed buffer must not become an anchor.
    #[test]
    fn tree_state_with_trailing_bytes_is_rejected() {
        let history = open_test_history();
        let mut bytes = empty_sapling_tree_bytes();
        let tree_len = bytes.len();
        bytes.extend_from_slice(&[0xffu8; 8]);
        let error = history
            .init_wallet_checkpoint_from_tree_state(test_params(), tree_state_with_tree(&hex::encode(bytes)))
            .unwrap_err();
        assert!(error.contains("trailing byte"), "{}", error);
        assert!(
            error.contains(&format!("{}-byte commitment tree", tree_len)),
            "{}",
            error
        );
        assert!(history.initial_chain_state.lock().is_none());
    }

    /// An empty tree field is only legitimate at or below Sapling activation. The
    /// fetch plan floors every request at that height, so above it an empty field
    /// means the server had nothing to give us.
    #[test]
    fn tree_state_empty_tree_is_rejected_above_sapling_activation_and_allowed_at_it() {
        let history = open_test_history();
        let error = history
            .init_wallet_checkpoint_from_tree_state(test_params(), tree_state_with_tree(""))
            .unwrap_err();
        assert!(error.contains("is empty"), "{}", error);
        assert!(error.contains("Sapling activated at 2"), "{}", error);
        assert!(history.initial_chain_state.lock().is_none());

        // At activation itself the empty tree is the correct answer.
        let mut at_activation = tree_state_with_tree("");
        at_activation.height = 2;
        history
            .init_wallet_checkpoint_from_tree_state(test_params(), at_activation)
            .unwrap();
        assert!(history.initial_chain_state.lock().is_some());
    }

    /// A server that sends both fields must agree with itself; when it does, the
    /// checkpoint is accepted exactly as before.
    #[test]
    fn tree_state_sapling_frontier_matching_tree_is_accepted() {
        let history = open_test_history();
        let tree_hex = hex::encode(empty_sapling_tree_bytes());
        let mut state = tree_state_with_tree(&tree_hex);
        state.height = 2;
        state.sapling_frontier = tree_hex;
        state.ironwood_tree = hex::encode([0x11u8; 32]);
        history
            .init_wallet_checkpoint_from_tree_state(test_params(), state)
            .unwrap();
        assert!(history.initial_chain_state.lock().is_some());
    }

    #[test]
    fn tree_state_display_hash_is_the_compact_chain_little_endian_anchor() {
        let history = open_test_history();
        let display_hash = (0u8..32).collect::<Vec<_>>();
        let mut compact_hash = display_hash.clone();
        compact_hash.reverse();
        history
            .init_wallet_checkpoint_from_tree_state(test_params(), z_coin_grpc::TreeState {
                network: "main".to_owned(),
                height: 10,
                hash: hex::encode(display_hash),
                time: 10,
                tree: hex::encode(empty_sapling_tree_bytes()),
                ..Default::default()
            })
            .unwrap();

        let mut block = empty_compact_block(11, 11, 0);
        block.prev_hash = compact_hash;
        history.insert_compact_block(block).unwrap();

        assert_eq!(
            history
                .scan_cached_blocks_to_height(test_params(), 11, 1_000, 0, |_, _| {})
                .unwrap(),
            11
        );
    }

    /// Pirate testnet block 389 -- captured from `testlightwalletd1.cryptoforge.cc`
    /// on 2026-09-28, when that chain was at tip 53 202 with
    /// `branchId=37a5165b`, i.e. **Ironwood already active** (it activates at
    /// height 297 there).
    ///
    /// Its single transaction is the case this wallet has to survive after 3 Oct
    /// and could not otherwise be shown: a **version-6 transaction that touches
    /// both shielded pools at once** -- one Sapling spend, two Sapling outputs,
    /// and two Ironwood actions carried in `CompactTx` field 6. Pirate's
    /// `lightwalletd` puts Ironwood actions in field 6 (confirmed by the Pirate
    /// maintainer and, here, from the wire), a field this project's
    /// `compact_formats.proto` does not declare at all, so prost skips it as an
    /// unknown field and `convert_compact_tx` drops the actions deliberately.
    ///
    /// The release notes claim receiving, balance and history keep working across
    /// the upgrade. That claim rested on reading the code; this test rests on the
    /// chain. The assertion is deliberately not "scanning returned Ok": it is
    /// that the Sapling commitment tree we build from the block is byte-identical
    /// to the one `lightwalletd` itself reports for that height, which a scan
    /// that silently lost an output would fail.
    ///
    /// The fixture is inlined rather than fetched because the maintainer would
    /// not guarantee the test chain stays up; the block, and both bracketing tree
    /// states, are recorded here so the regression outlives the network.
    #[test]
    fn a_post_ironwood_v6_block_scans_to_the_same_sapling_tree_lightwalletd_reports() {
        use prost::Message;

        /// Raw `CompactBlock` protobuf for testnet height 389.
        const BLOCK_389_HEX: &str = concat!(
            "1085031a200f74f26e7742237702f44b6548952aee87649d4264e43d8db82b4c314c58030422",
            "2051ac3c0a025f331fcfdb288f278a45fb424b765086b350cf1c57746dce466406288f9da0d3",
            "063afe040801122008ced81faa87809f83cd6a4df74b2a956fa3785292116683552e2fba134d",
            "e9ec22220a20050899d5d9c1fa66ca3e1bce6f0bc9187c08e229a7df1f6b7b0c4fc3d01559c0",
            "2a7a0a209161e18aad6a412a3c57fd040ac15f532d5296603a7c608d4d9020c7eead82641220",
            "aadf17949507e6bcbb0e0c4316b56eb59fec0cb1814592dab833db55e401b6e31a34913f9664",
            "d14935fae8fd1bc6b6b3d7a49fa499d24269aa87099f7adacc4f190879b5d25a8b9e88177b99",
            "a12e9019be2784bbaafe2a7a0a205e32f4d009420beec8469345c1ae3809735bb5e820b42993",
            "b3e076562426e62b1220db59b0eb8e21888d871ca951d93cbfcff3709e99a357b7c3e9ea2427",
            "8df7a72a1a34da12a43877c3a4ce873e52999d011826a46958ed713efbc2bc1b20d2f4410b9b",
            "2c77882378cf22a381bf7c8d2efdbe4d2f21674b329c010a202caefca055a245598c3d888ecb",
            "4a978232c79e9cfa1b89000d3f22963a2fb21512209ee073ca70f29bcbfa59bb04f325567cb8",
            "8a14679cf0f016374c01b72f7d2f141a20d1ef7b44c85f83d17ffc67376eca3c1f7558996e42",
            "f6de7b2e9e4614467f589f22340180351dac92aca0cdeee51fa7480d77309da90931fa42944f",
            "374f37821d712294769a0700b6602cf5981503d02b11629ca56f26329c010a2061acce9276ba",
            "361d4deb56e56fb8aa0e0c3c768c394a902ee4d5a60bcca0080c1220c25fcc41b88ade59fc8b",
            "d80dd17d6a8104064ad444d5fe6a02b15f640c526b321a20ff503325721da4b314a2622fa428",
            "1ee1234a7c9df0ab0656bca6add1b206e7182234c63633711fba0bea09b39aa264ebea4e59da",
            "ab6a410b65f86590a4dc7408b8481f55446b57d0bc5fa4037d120e89b35639990364",
        );
        /// `GetTreeState` at 388 -- the anchor the wallet starts from.
        const TREE_388_HEX: &str = concat!(
            "01fce57d8fee40b8268479aa4839759f951bd7cca5f94237b0193a03e5e9884613001f000000",
            "00000000000000000000000000000000000000000000000000000000",
        );
        /// `GetTreeState` at 389 -- what the server says the tree must become.
        const TREE_389_HEX: &str = concat!(
            "015e32f4d009420beec8469345c1ae3809735bb5e820b42993b3e076562426e62b001f012f2e",
            "c5ee7925881d1e8c5b5a1c695ae156b88c8437641db3a60094e5b83a2b630000000000000000",
            "00000000000000000000000000000000000000000000",
        );
        /// Internal (little-endian) block hashes, as `check_point_block` stores them.
        const HASH_388_INTERNAL: &str = "51ac3c0a025f331fcfdb288f278a45fb424b765086b350cf1c57746dce466406";
        const HASH_389_INTERNAL: &str = "0f74f26e7742237702f44b6548952aee87649d4264e43d8db82b4c314c580304";

        // Pirate's test chain activates Sapling at 61, not at the mainnet height.
        let params: ZcoinConsensusParams = serde_json::from_value(serde_json::json!({
            "overwinter_activation_height": 61,
            "sapling_activation_height": 61,
            "blossom_activation_height": null,
            "heartwood_activation_height": null,
            "canopy_activation_height": null,
            "coin_type": 133,
            "hrp_sapling_extended_spending_key": "secret-extended-key-main",
            "hrp_sapling_extended_full_viewing_key": "zxviews",
            "hrp_sapling_payment_address": "zs",
            "b58_pubkey_address_prefix": [0x1c, 0xb8],
            "b58_script_address_prefix": [0x1c, 0xbd]
        }))
        .unwrap();

        let mut hash_388 = [0u8; 32];
        hash_388.copy_from_slice(&hex::decode(HASH_388_INTERNAL).unwrap());
        let checkpoint = CheckPointBlockInfo {
            height: 388,
            hash: rpc::v1::types::H256(hash_388),
            time: 1785204289,
            sapling_tree: hex::decode(TREE_388_HEX).unwrap().into(),
        };

        // The viewing key is a stranger's: none of these notes are ours, and the
        // commitment tree is built from every output regardless of ownership --
        // which is exactly what makes the tree the right thing to assert on.
        let db_dir = test_db_dir("post-ironwood-v6-block");
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &test_extfvk(31), Some(&checkpoint))
                .unwrap();

        let block = z_coin_grpc::CompactBlock::decode(hex::decode(BLOCK_389_HEX).unwrap().as_slice()).unwrap();

        // Pin the fixture's shape, so a future edit cannot quietly turn this into
        // a test about an ordinary Sapling block.
        assert_eq!(block.height, 389);
        assert_eq!(block.vtx.len(), 1, "fixture should carry exactly one transaction");
        assert_eq!(block.vtx[0].spends.len(), 1, "one Sapling spend");
        assert_eq!(block.vtx[0].outputs.len(), 2, "two Sapling outputs");
        assert_eq!(
            hex::encode(&block.prev_hash),
            HASH_388_INTERNAL,
            "the fixture must chain to the checkpoint it is scanned from"
        );

        history
            .insert_compact_block(convert_compact_block(block).unwrap())
            .unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), 389, 1_000, 0, |_, _| {})
                .unwrap(),
            389,
            "scanning a version-6 block must reach its height"
        );

        // The decisive comparison, made with the same code production uses to
        // detect a diverged wallet.
        assert_eq!(
            history
                .init_wallet_checkpoint_from_tree_state(params, z_coin_grpc::TreeState {
                    network: "test".to_owned(),
                    height: 389,
                    hash: hex::encode(
                        hex::decode(HASH_389_INTERNAL)
                            .unwrap()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                    ),
                    time: 1785204367,
                    tree: TREE_389_HEX.to_owned(),
                    ..Default::default()
                })
                .unwrap(),
            CheckpointOutcome::Accepted,
            "our Sapling tree after the v6 block must equal lightwalletd's own"
        );
    }

    #[test]
    fn pirate_compact_block_without_chain_metadata_scans_received_note() {
        let db_dir = test_db_dir("pirate-compact-metadata-adapter");
        let params = test_params_with_zip212();
        let extsk = ExtendedSpendingKey::master(&[15; 32]);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let checkpoint = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 10,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &extfvk, Some(&checkpoint)).unwrap();
        let mut block = compact_block_with_received_note(11, 11, 9, &extfvk, 125_000_000);
        block.chain_metadata = None;
        history.insert_compact_block(block).unwrap();

        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), 11, 1_000, 0, |_, _| {})
                .unwrap(),
            11
        );
        assert_eq!(history.balance(params).unwrap(), 125_000_000);
    }

    #[test]
    fn pirate_metadata_free_scan_keeps_exact_frontier_across_progress_batches() {
        let db_dir = test_db_dir("pirate-progress-batch-frontier");
        let params = test_params_with_zip212();
        let tracked_extfvk = test_extfvk(16);
        let other_extfvk = test_extfvk(17);
        let output = compact_block_with_received_note(11, 11, 10, &other_extfvk, 1).vtx[0].outputs[0].clone();
        let node = sapling::Node::from_cmu(&output.cmu().unwrap());
        // This mirrors the public Pirate range's commitment counts at each
        // 1,000-block progress boundary. After the eighth batch the rightmost
        // four-leaf subtree is complete and can be pruned, so asking ShardTree
        // to reconstruct the exact frontier for batch nine is invalid.
        let checkpoint_tree_size = 16_670_675u64;
        let checkpoint_position = incrementalmerkletree::Position::from(checkpoint_tree_size - 1);
        let checkpoint_frontier =
            incrementalmerkletree::frontier::Frontier::from_parts(checkpoint_position, node.clone(), vec![
                node;
                usize::from(
                    checkpoint_position.past_ommer_count()
                )
            ])
            .unwrap();
        let checkpoint_tree = sapling::CommitmentTree::from_frontier(&checkpoint_frontier);
        let mut checkpoint_tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&checkpoint_tree, &mut checkpoint_tree_bytes).unwrap();
        let checkpoint_hash: [u8; 32] = deterministic_compact_hash(10).try_into().unwrap();
        let checkpoint = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256(checkpoint_hash),
            time: 10,
            sapling_tree: checkpoint_tree_bytes.into(),
        };
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &tracked_extfvk, Some(&checkpoint))
                .unwrap();

        // Every block advances the Sapling frontier, but the output is encrypted
        // to another wallet so this fixture exercises tree maintenance without
        // creating a thousand wallet transactions.
        let batch_output_counts = [494usize, 379, 305, 214, 328, 488, 519, 294];
        let blocks = (11u64..=8011)
            .map(|height| {
                let offset = usize::try_from(height - 11).unwrap();
                let output_count = if offset == 8_000 {
                    1
                } else {
                    let batch = offset / 1_000;
                    let in_batch = offset % 1_000;
                    if batch == 7 && in_batch == 995 {
                        6
                    } else {
                        let (count, span) = if batch == 7 {
                            (batch_output_counts[batch] - 6, 995)
                        } else {
                            (batch_output_counts[batch], 1_000)
                        };
                        usize::from(in_batch < span && ((in_batch + 1) * count / span > in_batch * count / span))
                    }
                };
                zcash_compact::CompactBlock {
                    height,
                    hash: deterministic_compact_hash(height),
                    prev_hash: deterministic_compact_hash(height - 1),
                    time: u32::try_from(height).unwrap(),
                    header: Vec::new(),
                    vtx: if output_count > 0 {
                        vec![zcash_compact::CompactTx {
                            index: 1,
                            txid: deterministic_compact_hash(height + 10_000),
                            outputs: vec![output.clone(); output_count],
                            ..Default::default()
                        }]
                    } else {
                        Vec::new()
                    },
                    chain_metadata: None,
                }
            })
            .collect::<Vec<_>>();
        history.insert_compact_blocks(&blocks).unwrap();

        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), 8011, 1_000, 0, |_, _| {})
                .unwrap(),
            8011
        );
        assert_eq!(history.balance(params.clone()).unwrap(), 0);

        // Reopening the wallet starts a new process-equivalent history handle,
        // which intentionally has no in-memory frontier. A lightwalletd
        // TreeState at the persisted scan height must restore the exact frontier
        // before the next bounded scan instead of asking the prunable ShardTree
        // to reconstruct data it may have compressed.
        let final_state = history.initial_chain_state.lock().as_ref().unwrap().clone();
        let final_tree = sapling::CommitmentTree::from_frontier(final_state.final_sapling_tree());
        let mut final_tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&final_tree, &mut final_tree_bytes).unwrap();
        let mut display_hash = deterministic_compact_hash(8011);
        display_hash.reverse();
        let tree_state = z_coin_grpc::TreeState {
            network: "main".to_owned(),
            height: 8011,
            hash: hex::encode(display_hash),
            time: 8011,
            tree: hex::encode(final_tree_bytes),
            ..Default::default()
        };
        let db_dir = history.wallet_db_path().parent().unwrap().to_owned();
        drop(history);

        let reopened =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &tracked_extfvk, None).unwrap();
        assert!(reopened.initial_chain_state.lock().is_none());

        // A hash the wallet disagrees with is reported as a divergence, not an
        // error: the caller rewinds past it. Either way it must never become the
        // anchor.
        let mut wrong_hash_state = tree_state.clone();
        wrong_hash_state.hash = hex::encode([0u8; 32]);
        assert_eq!(
            reopened
                .init_wallet_checkpoint_from_tree_state(params.clone(), wrong_hash_state)
                .unwrap(),
            CheckpointOutcome::Diverged
        );
        assert!(reopened.initial_chain_state.lock().is_none());

        let mut wrong_size_frontier = final_state.final_sapling_tree().clone();
        assert!(wrong_size_frontier.append(sapling::Node::from_cmu(&output.cmu().unwrap())));
        let wrong_size_tree = sapling::CommitmentTree::from_frontier(&wrong_size_frontier);
        let mut wrong_size_tree_bytes = Vec::new();
        zcash_primitives::merkle_tree::write_commitment_tree(&wrong_size_tree, &mut wrong_size_tree_bytes).unwrap();
        let mut wrong_size_state = tree_state.clone();
        wrong_size_state.tree = hex::encode(wrong_size_tree_bytes);
        let error = reopened
            .init_wallet_checkpoint_from_tree_state(params.clone(), wrong_size_state)
            .unwrap_err();
        assert!(error.contains("tree size"));
        assert!(reopened.initial_chain_state.lock().is_none());

        assert_eq!(
            reopened
                .init_wallet_checkpoint_from_tree_state(params.clone(), tree_state)
                .unwrap(),
            CheckpointOutcome::Accepted
        );
        reopened
            .insert_compact_block(zcash_compact::CompactBlock {
                height: 8012,
                hash: deterministic_compact_hash(8012),
                prev_hash: deterministic_compact_hash(8011),
                time: 8012,
                header: Vec::new(),
                vtx: Vec::new(),
                chain_metadata: None,
            })
            .unwrap();
        assert_eq!(
            reopened
                .scan_cached_blocks_to_height(params.clone(), 8012, 1_000, 0, |_, _| {})
                .unwrap(),
            8012
        );
        assert_eq!(reopened.balance(params).unwrap(), 0);
    }

    #[test]
    fn compact_blocks_honor_scan_pacing_and_persist_after_reopen() {
        let db_dir = test_db_dir("compact-scan-reopen");
        let extfvk = test_extfvk(12);
        let checkpoint = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256([10u8; 32]),
            time: 10,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), test_params(), &extfvk, Some(&checkpoint))
                .unwrap();
        history.insert_compact_block(empty_compact_block(11, 11, 10)).unwrap();
        history.insert_compact_block(empty_compact_block(12, 12, 11)).unwrap();

        let mut progress = Vec::new();
        let mut pauses = Vec::new();
        assert_eq!(
            history
                .scan_cached_blocks_to_height_with_sleeper(
                    test_params(),
                    12,
                    1,
                    20,
                    |scanned, target| progress.push((scanned, target)),
                    |duration| pauses.push(duration),
                )
                .unwrap(),
            12
        );
        assert_eq!(progress, vec![(10, 12), (11, 12), (12, 12)]);
        assert_eq!(pauses, vec![Duration::from_millis(20)]);
        drop(history);

        let reopened =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &extfvk, Some(&checkpoint)).unwrap();
        assert_eq!(reopened.scanned_height().unwrap(), Some(12));
        assert_eq!(reopened.balance(test_params()).unwrap(), 0);
    }

    #[test]
    fn compact_block_batch_persistence_is_atomic() {
        let history = open_test_history();
        let conn = Connection::open(history.compact_blocks_path()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_second_test_block
             BEFORE INSERT ON compactblocks
             WHEN NEW.height = 12
             BEGIN
                 SELECT RAISE(ABORT, 'test batch failure');
             END;",
        )
        .unwrap();
        drop(conn);

        let blocks = [empty_compact_block(11, 11, 10), empty_compact_block(12, 12, 11)];
        assert!(history.insert_compact_blocks(&blocks).is_err());
        let conn = Connection::open(history.compact_blocks_path()).unwrap();
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM compactblocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "a failed network batch must not be partially cached");

        conn.execute_batch("DROP TRIGGER reject_second_test_block;").unwrap();
        drop(conn);
        history.insert_compact_blocks(&blocks).unwrap();
        let conn = Connection::open(history.compact_blocks_path()).unwrap();
        let range: (u32, u32, u32) = conn
            .query_row(
                "SELECT COUNT(*), MIN(height), MAX(height) FROM compactblocks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(range, (2, 11, 12));
    }

    #[test]
    fn compact_cache_resume_uses_highest_validated_height_after_reopen() {
        let db_dir = test_db_dir("compact-cache-resume-reopen");
        let extfvk = test_extfvk(14);
        let checkpoint = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 10,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), test_params(), &extfvk, Some(&checkpoint))
                .unwrap();
        history
            .insert_compact_blocks(&[empty_compact_block(11, 11, 9), empty_compact_block(12, 12, 11)])
            .unwrap();
        assert_eq!(history.validated_cached_resume_height(11, 20).unwrap(), Some(12));
        assert_eq!(history.validated_cached_resume_height(11, 11).unwrap(), Some(11));
        drop(history);

        let reopened =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &extfvk, Some(&checkpoint)).unwrap();
        assert_eq!(reopened.validated_cached_resume_height(11, 20).unwrap(), Some(12));
    }

    #[test]
    fn compact_cache_resume_rejects_a_discontinuous_segment() {
        let history = open_checkpointed_test_history(10);
        history
            .insert_compact_blocks(&[empty_compact_block(11, 11, 9), empty_compact_block(12, 12, 99)])
            .unwrap();

        let error = history.validated_cached_resume_height(11, 20).unwrap_err();
        assert!(error.contains("hash discontinuity at height 12"), "{error}");
    }

    #[test]
    fn legacy_wallet_rebuild_rescans_and_reconstructs_balance_and_history() {
        let db_dir = test_db_dir("legacy-rescan-balance-history");
        let paths = ZCoinShieldedHistoryPaths::new("ARRR", db_dir.clone());
        create_legacy_wallet(&paths.wallet_db_path);
        let params = test_params_with_zip212();
        let extsk = ExtendedSpendingKey::master(&[13; 32]);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let checkpoint = CheckPointBlockInfo {
            height: 10,
            hash: rpc::v1::types::H256([9; 32]),
            time: 10,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };

        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), params.clone(), &extfvk, Some(&checkpoint))
                .unwrap();
        history
            .insert_compact_block(compact_block_with_received_note(11, 11, 9, &extfvk, 125_000_000))
            .unwrap();
        let mut progress = Vec::new();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), 11, 1_000, 0, |scanned, target| {
                    progress.push((scanned, target));
                })
                .unwrap(),
            11
        );
        assert_eq!(progress, vec![(10, 11), (11, 11)]);
        assert_eq!(history.balance(params.clone()).unwrap(), 125_000_000);
        let page = history
            .load_page(
                "ARRR",
                "zs-wallet",
                8,
                11,
                &PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
                10,
            )
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(
            page.transactions[0].received_by_me,
            BigDecimal::from(125) / BigDecimal::from(100)
        );
        drop(history);

        let reopened =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &extfvk, Some(&checkpoint)).unwrap();
        assert_eq!(reopened.scanned_height().unwrap(), Some(11));
        assert_eq!(reopened.balance(params).unwrap(), 125_000_000);
    }

    #[test]
    fn compact_scan_rejects_wrong_checkpoint_link_without_advancing_wallet() {
        let history = open_checkpointed_test_history(10);
        history.insert_compact_block(empty_compact_block(11, 11, 99)).unwrap();

        let error = history
            .scan_cached_blocks_to_height(test_params(), 11, 1_000, 0, |_, _| {})
            .unwrap_err();
        assert!(error.contains("hash discontinuity at height 11"), "{error}");
        assert_eq!(history.scanned_height().unwrap(), Some(10));

        history.insert_compact_block(empty_compact_block(11, 11, 9)).unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(test_params(), 11, 1_000, 0, |_, _| {})
                .unwrap(),
            11
        );
    }

    #[test]
    fn compact_scan_rejects_height_gaps_and_malformed_hashes() {
        let history = open_checkpointed_test_history(10);
        history.insert_compact_block(empty_compact_block(11, 11, 9)).unwrap();
        history.insert_compact_block(empty_compact_block(13, 13, 12)).unwrap();
        let error = history
            .scan_cached_blocks_to_height(test_params(), 13, 1_000, 0, |_, _| {})
            .unwrap_err();
        assert!(error.contains("expected 12, found 13"), "{error}");
        assert_eq!(history.scanned_height().unwrap(), Some(10));

        let malformed = open_checkpointed_test_history(10);
        let mut malformed_block = empty_compact_block(11, 11, 9);
        malformed_block.hash.pop();
        malformed.insert_compact_block(malformed_block).unwrap();
        let error = malformed
            .scan_cached_blocks_to_height(test_params(), 11, 1_000, 0, |_, _| {})
            .unwrap_err();
        assert!(error.contains("32-byte hash and prev_hash"), "{error}");
        assert_eq!(malformed.scanned_height().unwrap(), Some(10));
    }

    #[test]
    fn wallet_schema_classifier_recognizes_absent_and_empty_files() {
        let db_dir = test_db_dir("schema-empty");
        let absent = db_dir.join("absent.db");
        assert_eq!(
            classify_wallet_db(&absent, test_params()),
            WalletDbGeneration::AbsentOrEmpty
        );

        let empty = db_dir.join("empty.db");
        let conn = Connection::open(&empty).unwrap();
        conn.execute_batch("VACUUM;").unwrap();
        drop(conn);
        assert_eq!(
            classify_wallet_db(&empty, test_params()),
            WalletDbGeneration::AbsentOrEmpty
        );
    }

    #[test]
    fn wallet_schema_classifier_recognizes_both_reference_fixtures_as_legacy() {
        let db_dir = test_db_dir("schema-reference-legacy");
        for reference_name in ["v2.6.0-beta", "dev"] {
            let path = db_dir.join(format!("{}.db", reference_name));
            create_legacy_wallet(&path);
            assert_eq!(
                classify_wallet_db(&path, test_params()),
                WalletDbGeneration::ReferenceLegacy,
                "reference fixture {reference_name}"
            );
        }
    }

    #[test]
    fn wallet_schema_classifier_recognizes_selected_current_schema() {
        let db_dir = test_db_dir("schema-current");
        let path = db_dir.join("current.db");
        initialize_wallet_db(&path, test_params()).unwrap();
        assert_eq!(
            classify_wallet_db(&path, test_params()),
            WalletDbGeneration::SelectedCurrent
        );
    }

    #[test]
    fn wallet_schema_classifier_rejects_extra_object_and_partial_schema() {
        let db_dir = test_db_dir("schema-objects");
        let extra = db_dir.join("extra.db");
        create_legacy_wallet(&extra);
        Connection::open(&extra)
            .unwrap()
            .execute_batch("CREATE TABLE unexpected (id INTEGER PRIMARY KEY);")
            .unwrap();
        assert_eq!(classify_wallet_db(&extra, test_params()), WalletDbGeneration::Unknown);

        let partial = db_dir.join("partial.db");
        Connection::open(&partial)
            .unwrap()
            .execute_batch(
                "CREATE TABLE accounts (
                    account INTEGER PRIMARY KEY,
                    extfvk TEXT NOT NULL,
                    address TEXT NOT NULL
                );",
            )
            .unwrap();
        assert_eq!(classify_wallet_db(&partial, test_params()), WalletDbGeneration::Unknown);
    }

    #[test]
    fn wallet_schema_classifier_rejects_altered_column_and_constraint() {
        let db_dir = test_db_dir("schema-altered");
        let altered_column = db_dir.join("altered-column.db");
        let altered_column_schema =
            REFERENCE_LEGACY_WALLET_SCHEMA.replacen("address TEXT NOT NULL", "address BLOB NOT NULL", 1);
        Connection::open(&altered_column)
            .unwrap()
            .execute_batch(&altered_column_schema)
            .unwrap();
        assert_eq!(
            classify_wallet_db(&altered_column, test_params()),
            WalletDbGeneration::Unknown
        );

        let altered_constraint = db_dir.join("altered-constraint.db");
        let altered_constraint_schema =
            REFERENCE_LEGACY_WALLET_SCHEMA.replace("txid BLOB NOT NULL UNIQUE", "txid BLOB NOT NULL");
        Connection::open(&altered_constraint)
            .unwrap()
            .execute_batch(&altered_constraint_schema)
            .unwrap();
        assert_eq!(
            classify_wallet_db(&altered_constraint, test_params()),
            WalletDbGeneration::Unknown
        );
    }

    #[test]
    fn wallet_schema_classifier_rejects_nonzero_legacy_version_and_corruption() {
        let db_dir = test_db_dir("schema-version-corrupt");
        let nonzero_version = db_dir.join("nonzero-version.db");
        create_legacy_wallet(&nonzero_version);
        Connection::open(&nonzero_version)
            .unwrap()
            .execute_batch("PRAGMA user_version = 1;")
            .unwrap();
        assert_eq!(
            classify_wallet_db(&nonzero_version, test_params()),
            WalletDbGeneration::Unknown
        );

        let corrupt = db_dir.join("corrupt.db");
        std::fs::write(&corrupt, b"not a SQLite database").unwrap();
        assert_eq!(classify_wallet_db(&corrupt, test_params()), WalletDbGeneration::Corrupt);
    }

    #[test]
    fn compact_schema_classifier_is_strict() {
        let db_dir = test_db_dir("schema-compact");
        let recognized = db_dir.join("recognized.db");
        initialize_compact_db(&recognized).unwrap();
        assert_eq!(classify_compact_db(&recognized), CompactDbGeneration::Recognized);

        let unknown = db_dir.join("unknown.db");
        Connection::open(&unknown)
            .unwrap()
            .execute_batch(
                "CREATE TABLE compactblocks (height INTEGER PRIMARY KEY, data BLOB NOT NULL);
                 CREATE TABLE unexpected (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        assert_eq!(classify_compact_db(&unknown), CompactDbGeneration::Unknown);

        let corrupt = db_dir.join("corrupt.db");
        std::fs::write(&corrupt, b"not a SQLite database").unwrap();
        assert_eq!(classify_compact_db(&corrupt), CompactDbGeneration::Corrupt);
    }

    #[test]
    fn compact_schema_classifier_recovers_a_locked_hot_journal_without_mutating_source() {
        let db_dir = test_db_dir("schema-compact-hot-journal");
        let path = db_dir.join("recognized.db");
        initialize_compact_db(&path).unwrap();

        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = DELETE;
                 PRAGMA locking_mode = EXCLUSIVE;
                 BEGIN EXCLUSIVE;
                 INSERT INTO compactblocks (height, data) VALUES (1, X'01');",
            )
            .unwrap();
        let journal_path = path_with_suffix(&path, "-journal");
        assert!(journal_path.exists());
        assert!(read_only_schema_fingerprint_once(&path).is_err());

        let database_before = std::fs::read(&path).unwrap();
        let journal_before = std::fs::read(&journal_path).unwrap();
        assert_eq!(classify_compact_db(&path), CompactDbGeneration::Recognized);
        assert_eq!(std::fs::read(&path).unwrap(), database_before);
        assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);

        writer.execute_batch("ROLLBACK;").unwrap();
    }

    #[test]
    fn unknown_and_corrupt_wallets_fail_without_mutation() {
        for (label, schema) in [
            (
                "unknown",
                Some(
                    "CREATE TABLE accounts (
                        account INTEGER PRIMARY KEY,
                        extfvk TEXT NOT NULL,
                        address TEXT NOT NULL
                    );",
                ),
            ),
            ("corrupt", None),
        ] {
            let db_dir = test_db_dir(&format!("schema-preserve-{label}"));
            let paths = ZCoinShieldedHistoryPaths::new("ARRR", db_dir.clone());
            if let Some(schema) = schema {
                Connection::open(&paths.wallet_db_path)
                    .unwrap()
                    .execute_batch(schema)
                    .unwrap();
            } else {
                std::fs::write(&paths.wallet_db_path, b"not a SQLite database").unwrap();
            }
            let before = std::fs::read(&paths.wallet_db_path).unwrap();

            let result = ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &test_extfvk(9), None);
            let (error, _) = result.unwrap_err().split();
            assert!(matches!(error, ZCoinBuildError::ShieldedDbSchema { .. }));
            assert_eq!(std::fs::read(&paths.wallet_db_path).unwrap(), before);
            assert!(!paths.compact_blocks_path.exists());
        }
    }

    #[test]
    fn unknown_compact_cache_fails_without_mutation() {
        let db_dir = test_db_dir("schema-preserve-compact");
        let paths = ZCoinShieldedHistoryPaths::new("ARRR", db_dir.clone());
        Connection::open(&paths.compact_blocks_path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE compactblocks (height INTEGER PRIMARY KEY, data BLOB NOT NULL);
                 CREATE TABLE unexpected (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        let before = std::fs::read(&paths.compact_blocks_path).unwrap();

        let result = ZCoinShieldedHistory::open_or_create("ARRR", db_dir, test_params(), &test_extfvk(10), None);
        let (error, _) = result.unwrap_err().split();
        assert!(matches!(error, ZCoinBuildError::ShieldedDbSchema { .. }));
        assert_eq!(std::fs::read(&paths.compact_blocks_path).unwrap(), before);
        assert!(!paths.wallet_db_path.exists());
    }

    #[test]
    fn recognized_legacy_wallet_is_preserved_and_rebuilt_without_touching_gleec_db() {
        let db_dir = test_db_dir("schema-legacy-rebuild");
        let paths = ZCoinShieldedHistoryPaths::new("ARRR", db_dir.clone());
        create_legacy_wallet(&paths.wallet_db_path);
        create_legacy_wallet(&paths.legacy_wallet_db_path);
        let gleec_before = std::fs::read(&paths.legacy_wallet_db_path).unwrap();

        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), test_params(), &test_extfvk(11), None)
                .unwrap();

        assert_eq!(
            classify_wallet_db(history.wallet_db_path(), test_params()),
            WalletDbGeneration::SelectedCurrent
        );
        assert_eq!(
            classify_compact_db(history.compact_blocks_path()),
            CompactDbGeneration::Recognized
        );
        assert_eq!(std::fs::read(&paths.legacy_wallet_db_path).unwrap(), gleec_before);
        assert_eq!(
            classify_wallet_db(&paths.legacy_wallet_db_path, test_params()),
            WalletDbGeneration::ReferenceLegacy
        );

        let backups = std::fs::read_dir(&db_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
                    name.starts_with("ARRR_RELOADED_WALLET.db.legacy-v2.") && name.ends_with(".bak")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            classify_wallet_db(&backups[0], test_params()),
            WalletDbGeneration::ReferenceLegacy
        );

        let conn = Connection::open(history.wallet_db_path()).unwrap();
        let account_count: u32 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(account_count, 0, "legacy account rows must not be migrated or reused");
    }

    /// A real wallet created by the previous Reloaded build (`zcash_client_sqlite 0.21.1`,
    /// one account imported from `test_extfvk(8)` at a checkpoint), captured by running that
    /// build's normal initialisation path. It is tracked because it is small and is the only
    /// way to exercise the upstream migrator against a faithful old schema.
    const PREVIOUS_RELOADED_WALLET_FIXTURE: &[u8] =
        include_bytes!("test-fixtures/wallet-zcsqlite-0.21.1-checkpointed.db");

    fn place_previous_reloaded_wallet(db_dir: &Path) -> ZCoinShieldedHistoryPaths {
        let paths = ZCoinShieldedHistoryPaths::new("ARRR", db_dir.to_path_buf());
        std::fs::write(&paths.wallet_db_path, PREVIOUS_RELOADED_WALLET_FIXTURE).unwrap();
        paths
    }

    fn backups_with_prefix(db_dir: &Path, prefix: &str) -> Vec<PathBuf> {
        std::fs::read_dir(db_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
            })
            .collect()
    }

    #[test]
    fn previous_reloaded_wallet_fixture_is_recognized_as_upgradeable() {
        let db_dir = test_db_dir("schema-previous-recognized");
        let paths = place_previous_reloaded_wallet(&db_dir);
        let before = std::fs::read(&paths.wallet_db_path).unwrap();
        assert_eq!(
            classify_wallet_db(&paths.wallet_db_path, test_params()),
            WalletDbGeneration::UpgradeablePreviousReloaded
        );
        // Classification is read-only.
        assert_eq!(std::fs::read(&paths.wallet_db_path).unwrap(), before);
    }

    /// Runs the real upstream migrator (`init_wallet_db`, 23 migrations) over a wallet
    /// captured from the previous build, not a DDL reconstruction.
    #[test]
    fn previous_reloaded_wallet_is_backed_up_and_migrated_in_place() {
        let db_dir = test_db_dir("schema-previous-migrate");
        let paths = place_previous_reloaded_wallet(&db_dir);

        // `test_extfvk(8)` owns the fixture's account; open_or_create rejects a wallet whose
        // account does not match, so success also proves the account survived migration.
        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), test_params(), &test_extfvk(8), None).unwrap();

        assert_eq!(
            classify_wallet_db(history.wallet_db_path(), test_params()),
            WalletDbGeneration::SelectedCurrent
        );
        let conn = Connection::open(history.wallet_db_path()).unwrap();
        let migrations: u32 = conn
            .query_row("SELECT COUNT(*) FROM schemer_migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(migrations, 71, "48 previous + 23 new migrations");
        let accounts: u32 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(accounts, 1, "the migrated wallet keeps its account");

        let backups = backups_with_prefix(&db_dir, "ARRR_RELOADED_WALLET.db.pre-upgrade.");
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), PREVIOUS_RELOADED_WALLET_FIXTURE);
        assert!(backups_with_prefix(&db_dir, "ARRR_RELOADED_WALLET.db.upgrade-failed.").is_empty());
        assert!(!path_with_suffix(&paths.wallet_db_path, ".upgrade-work").exists());
    }

    /// Forces the migration attempt to fail before it starts (the working-copy path is
    /// occupied by a directory) and proves the outcome degrades to preserve-and-rebuild.
    /// The post-migration verification failure branch is not separately exercised.
    #[test]
    fn failed_previous_wallet_upgrade_falls_back_to_preserve_and_rebuild() {
        let db_dir = test_db_dir("schema-previous-fallback");
        let paths = place_previous_reloaded_wallet(&db_dir);
        std::fs::create_dir(path_with_suffix(&paths.wallet_db_path, ".upgrade-work")).unwrap();

        let history =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir.clone(), test_params(), &test_extfvk(8), None).unwrap();

        assert_eq!(
            classify_wallet_db(history.wallet_db_path(), test_params()),
            WalletDbGeneration::SelectedCurrent
        );
        let conn = Connection::open(history.wallet_db_path()).unwrap();
        let accounts: u32 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(accounts, 0, "the rebuilt wallet starts empty and is rescanned");
        for prefix in [
            "ARRR_RELOADED_WALLET.db.pre-upgrade.",
            "ARRR_RELOADED_WALLET.db.upgrade-failed.",
        ] {
            let backups = backups_with_prefix(&db_dir, prefix);
            assert_eq!(backups.len(), 1, "{prefix}");
            assert_eq!(std::fs::read(&backups[0]).unwrap(), PREVIOUS_RELOADED_WALLET_FIXTURE);
        }
    }

    // Re-pinned for zcash_client_sqlite 0.22.0 (was 467 records / 48 migrations on 0.21.1).
    // Wallet DBs created by the 0.21.1 schema no longer match this fingerprint; they are
    // upgraded by R39.8.0ba (see previous_reloaded_wallet_is_backed_up_and_migrated_in_place).
    #[test]
    fn selected_current_schema_identity_is_pinned() {
        use sha2::{Digest, Sha256};

        let fingerprint = selected_current_wallet_fingerprint(test_params()).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(format!("user_version:{}\n", fingerprint.user_version));
        for record in &fingerprint.records {
            hasher.update(record.as_bytes());
            hasher.update(b"\n");
        }

        let mut conn = Connection::open_in_memory().unwrap();
        db_common::sqlite::rusqlite::vtab::array::load_module(&conn).unwrap();
        {
            let mut wallet_db = WalletDb::from_connection(&mut conn, test_params(), SystemClock, OsRng);
            init_wallet_db(&mut wallet_db, None).unwrap();
        }
        let mut stmt = conn
            .prepare("SELECT hex(id) FROM schemer_migrations ORDER BY id")
            .unwrap();
        let migration_ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut migration_hasher = Sha256::new();
        for migration_id in &migration_ids {
            migration_hasher.update(migration_id.as_bytes());
            migration_hasher.update(b"\n");
        }
        assert_eq!(fingerprint.user_version, 8);
        assert_eq!(fingerprint.records.len(), 727);
        assert_eq!(
            hex::encode(hasher.finalize()),
            "4ebc85f7ea3516d019b3d0d4680a6cb75ec980bfe81437abb4913d50b3407350"
        );
        assert_eq!(migration_ids.len(), 71);
        assert_eq!(
            hex::encode(migration_hasher.finalize()),
            "1142e14a2d3103f1cabbbcbc2d686000e021bf88bcbcce2d34f7d0a040e2d08b"
        );
    }

    /// CRD ch.39 T39.8.0g acceptance gate.
    ///
    /// This test exists because the failure mode of trial decryption is
    /// silent: wrong key or type plumbing yields "no notes found", which is
    /// indistinguishable from an empty mempool and identical to the behaviour
    /// before pending receipts existed. A test that only asserted "no crash" or
    /// "empty result" would pass against a completely broken implementation, so
    /// this one builds a real encrypted output for a known viewing key and
    /// asserts both that it is detected and that the recovered value is right.
    #[test]
    fn pending_receipt_detection_recovers_the_note_value() {
        use sapling::keys::PreparedIncomingViewingKey;
        use sapling::note_encryption::{sapling_note_encryption, try_sapling_compact_note_decryption,
                                       CompactOutputDescription, Zip212Enforcement};
        use sapling::value::NoteValue;
        use sapling::{Note, Rseed};
        use zcash_note_encryption::{Domain, COMPACT_NOTE_SIZE};

        const VALUE: u64 = 123_456_789;

        let mut rng = OsRng;
        let extsk = sapling::zip32::ExtendedSpendingKey::master(&[0u8; 32]);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let (_, payment_address) = extfvk.default_address();

        let note = Note::from_parts(
            payment_address,
            NoteValue::from_raw(VALUE),
            Rseed::AfterZip212([0u8; 32]),
        );
        let encryptor = sapling_note_encryption(None, note.clone(), [0u8; 512], &mut rng);
        let enc_ciphertext = encryptor.encrypt_note_plaintext();
        let epk = sapling::note_encryption::SaplingDomain::epk_bytes(encryptor.epk());

        let compact = CompactOutputDescription {
            ephemeral_key: epk,
            cmu: note.cmu(),
            enc_ciphertext: enc_ciphertext[..COMPACT_NOTE_SIZE]
                .try_into()
                .expect("compact prefix is COMPACT_NOTE_SIZE bytes"),
        };

        let ivk = PreparedIncomingViewingKey::new(&extfvk.fvk.vk.ivk());
        let (decrypted, _addr) = try_sapling_compact_note_decryption(&ivk, &compact, Zip212Enforcement::On)
            .expect("a wallet-owned output must trial-decrypt with the wallet's own viewing key");

        assert_eq!(
            Note::value(&decrypted).inner(),
            VALUE,
            "the recovered pending amount must equal the note value, not merely be non-zero"
        );

        // A different wallet's key must not match, or every mempool output on
        // the network would be reported as an incoming payment.
        #[allow(deprecated)]
        let other = sapling::zip32::ExtendedSpendingKey::master(&[7u8; 32]).to_extended_full_viewing_key();
        let other_ivk = PreparedIncomingViewingKey::new(&other.fvk.vk.ivk());
        assert!(
            try_sapling_compact_note_decryption(&other_ivk, &compact, Zip212Enforcement::On).is_none(),
            "an output belonging to another wallet must not decrypt"
        );
    }
    // -----------------------------------------------------------------------------------------
    // A withdrawal is built here and broadcast by the caller (CRD ch.39 R39.8.0ap, D39.8.0e / T39.8.0h)
    // -----------------------------------------------------------------------------------------

    /// One scanned 1.25 ARRR note, one spending key, and the pieces `build_from_wallet_db` needs.
    /// `None` when the Sapling parameter files are absent: a spend proof cannot be made without them
    /// (the same arrangement as the version-6 construction tests).
    struct WithdrawFixture {
        history: ZCoinShieldedHistory,
        params: ZcoinConsensusParams,
        extsk: ExtendedSpendingKey,
        prover: zcash_proofs::prover::LocalTxProver,
        my_addr: sapling::PaymentAddress,
        to_addr: sapling::PaymentAddress,
    }

    impl WithdrawFixture {
        const NOTE_VALUE: u64 = 125_000_000;
        const FEE_SAT: u64 = 10_000;
        const SEND_SAT: u64 = 1_000_000;

        fn new() -> Option<WithdrawFixture> {
            let dir = std::env::var_os("ARRR_SAPLING_PARAMS")
                .map(PathBuf::from)
                .unwrap_or_else(crate::utxo::zcash_params_path);
            let (spend, output) = (
                dir.join(crate::z_coin::SAPLING_SPEND_NAME),
                dir.join(crate::z_coin::SAPLING_OUTPUT_NAME),
            );
            if !spend.exists() || !output.exists() {
                eprintln!(
                    "SKIPPED: Sapling parameters not found in {} (set ARRR_SAPLING_PARAMS); a spend proof can be \
                     neither made nor checked without them",
                    dir.display()
                );
                return None;
            }
            let prover = zcash_proofs::prover::LocalTxProver::new(&spend, &output);

            let params = test_params_with_zip212();
            let extsk = ExtendedSpendingKey::master(&[15; 32]);
            #[allow(deprecated)]
            let extfvk = extsk.to_extended_full_viewing_key();
            let checkpoint = CheckPointBlockInfo {
                height: 10,
                hash: rpc::v1::types::H256([9u8; 32]),
                time: 10,
                sapling_tree: empty_sapling_tree_bytes().into(),
            };
            let history = ZCoinShieldedHistory::open_or_create(
                "ARRR",
                test_db_dir("withdraw-lock"),
                params.clone(),
                &extfvk,
                Some(&checkpoint),
            )
            .unwrap();
            history
                .insert_compact_block(compact_block_with_received_note(11, 11, 9, &extfvk, Self::NOTE_VALUE))
                .unwrap();
            assert_eq!(
                history
                    .scan_cached_blocks_to_height(params.clone(), 11, 1_000, 0, |_, _| {})
                    .unwrap(),
                11
            );
            assert_eq!(history.balance(params.clone()).unwrap(), Self::NOTE_VALUE);

            let to_addr = ExtendedSpendingKey::master(&[99; 32]).default_address().1;
            let my_addr = extsk.default_address().1;
            Some(WithdrawFixture {
                history,
                params,
                extsk,
                prover,
                my_addr,
                to_addr,
            })
        }

        fn inputs(&self) -> crate::z_coin::z_coin_ops::WalletDbBuild<'_> {
            crate::z_coin::z_coin_ops::WalletDbBuild {
                ticker: "ARRR",
                decimals: 8,
                wallet_db_path: self.history.wallet_db_path(),
                params: &self.params,
                spending_key: &self.extsk,
                my_addr: &self.my_addr,
                prover: &self.prover,
            }
        }

        fn output(&self) -> crate::z_coin::ZOutput {
            crate::z_coin::ZOutput {
                to_addr: self.to_addr.clone(),
                amount: zcash_protocol::value::Zatoshis::const_from_u64(Self::SEND_SAT),
                viewing_key: None,
                memo: None,
            }
        }

        fn fee(&self) -> BigDecimal { big_decimal_from_sat_unsigned(Self::FEE_SAT, 8) }

        /// A withdrawal as `withdraw` builds it: selected from the wallet database, signed, returned.
        /// Nothing is recorded.
        fn build(
            &self,
        ) -> Result<zcash_primitives::transaction::Transaction, crate::z_coin::z_coin_errors::GenTxError> {
            crate::z_coin::z_coin_ops::build_from_wallet_db(&self.inputs(), vec![], vec![self.output()], self.fee(), 12)
                .map(|(tx, _)| tx)
                .map_err(|e| e.into_inner())
        }

        /// The second step of the withdraw contract: the caller submits the serialized transaction
        /// and the backend accepts it. Returns what was recorded.
        fn broadcast(
            &self,
            tx: &zcash_primitives::transaction::Transaction,
        ) -> Option<zcash_primitives::transaction::Transaction> {
            let mut bytes = Vec::new();
            tx.write(&mut bytes).unwrap();
            crate::z_coin::z_coin_ops::record_broadcast_in_wallet_db(
                self.history.wallet_db_path(),
                &self.params,
                &bytes,
            )
            .unwrap()
        }

        /// The fee column the wallet database holds for `tx`.
        fn recorded_fee(&self, tx: &zcash_primitives::transaction::Transaction) -> Option<u64> {
            let conn = Connection::open(self.history.wallet_db_path()).unwrap();
            conn.query_row(
                "SELECT fee FROM transactions WHERE txid = ?1",
                params![&tx.txid().as_ref()[..]],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap()
            .map(|fee| fee as u64)
        }
    }

    fn sapling_nullifiers(tx: &zcash_primitives::transaction::Transaction) -> Vec<[u8; 32]> {
        tx.sapling_bundle()
            .map(|bundle| {
                bundle
                    .shielded_spends()
                    .iter()
                    .map(|spend| spend.nullifier().0)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The defect, as it reaches the wallet database: two withdrawals built one after the other,
    /// with nothing broadcast and no scan between them, are handed the same note. This is the
    /// intended behaviour when the first is abandoned (see below), and the failure when it is not.
    #[test]
    fn unrecorded_withdraw_builds_select_the_same_note() {
        let Some(fixture) = WithdrawFixture::new() else { return };
        let (first, second) = (fixture.build().unwrap(), fixture.build().unwrap());
        assert_eq!(sapling_nullifiers(&first).len(), 1);
        assert_eq!(sapling_nullifiers(&first), sapling_nullifiers(&second));
    }

    /// T39.8.0h, withdrawal side (R39.8.0ap, D39.8.0e): withdraw builds and returns, and the caller
    /// submits through `send_raw_transaction`. Once the backend has accepted those bytes, a second
    /// build before any scan is not handed the same note and is told it is short.
    #[test]
    fn a_broadcast_withdrawal_holds_its_note_against_the_next_build() {
        use crate::z_coin::z_coin_errors::GenTxError;
        let Some(fixture) = WithdrawFixture::new() else { return };

        let first = fixture.build().unwrap();
        let recorded = fixture.broadcast(&first);
        assert_eq!(recorded.as_ref().map(|tx| tx.txid()), Some(first.txid()));
        assert_eq!(
            fixture
                .history
                .spent_value_of_transaction(first.txid().as_ref())
                .unwrap(),
            WithdrawFixture::NOTE_VALUE
        );
        assert_eq!(
            fixture.recorded_fee(&first),
            Some(WithdrawFixture::FEE_SAT),
            "the fee recorded from the raw bytes is the fee the build paid"
        );

        match fixture.build() {
            Err(GenTxError::InsufficientBalance { available, .. }) => assert_eq!(available, BigDecimal::from(0)),
            Err(other) => panic!("expected an insufficient-balance failure, got {other}"),
            Ok(_) => panic!("a second build after the first was broadcast must not reuse its note"),
        }
    }

    /// What the redesign buys, and the most likely thing to regress: a build the caller never
    /// submits (the user cancelled the confirmation) records nothing, so the next build reuses the
    /// note freely and nothing is stranded until expiry.
    #[test]
    fn an_abandoned_withdrawal_strands_no_notes() {
        let Some(fixture) = WithdrawFixture::new() else { return };

        let abandoned = fixture.build().unwrap();
        assert_eq!(
            fixture
                .history
                .spent_value_of_transaction(abandoned.txid().as_ref())
                .unwrap(),
            0,
            "building alone records nothing"
        );
        let retry = fixture
            .build()
            .expect("the note is free: the first build was never broadcast");
        assert_eq!(sapling_nullifiers(&abandoned), sapling_nullifiers(&retry));
    }

    /// The release the record depends on when a broadcast transaction is never mined: it stops
    /// holding its notes once the chain passes its expiry height, and not a block earlier. The
    /// wallet database counts the transaction as unexpired while `expiry_height >= target`,
    /// where target is the chain tip plus one.
    #[test]
    fn a_broadcast_withdrawal_releases_its_note_once_the_expiry_height_passes() {
        use crate::z_coin::z_coin_errors::GenTxError;
        let Some(fixture) = WithdrawFixture::new() else { return };

        let first = fixture.build().unwrap();
        fixture.broadcast(&first);
        let expiry: u32 = first.expiry_height().into();
        assert_eq!(
            expiry,
            12 + 40,
            "the builder's default expiry delta is what bounds the hold"
        );

        // Advance the wallet's scanned tip block by block, as the sync loop does; a bare
        // `update_chain_tip` would leave unscanned ranges that make every note unspendable.
        let advance_to = |tip: u32| {
            let from = fixture.history.scanned_height().unwrap().unwrap();
            let blocks = (from + 1..=u64::from(tip))
                .map(|h| {
                    let mut block = empty_compact_block(h, h as u8, (h - 1) as u8);
                    // The tree size is derived while scanning; the fixture's zero would contradict the note.
                    block.chain_metadata = None;
                    block
                })
                .collect::<Vec<_>>();
            fixture.history.insert_compact_blocks(&blocks).unwrap();
            fixture
                .history
                .scan_cached_blocks_to_height(fixture.params.clone(), u64::from(tip), 1_000, 0, |_, _| {})
                .unwrap();
        };

        // Tip = expiry - 1, so target = expiry: the transaction can still be mined in that block.
        advance_to(expiry - 1);
        assert!(
            matches!(fixture.build(), Err(GenTxError::InsufficientBalance { available, .. }) if available == BigDecimal::from(0)),
            "still held while the transaction can yet be mined"
        );
        // Tip = expiry, so target = expiry + 1: it can no longer be mined.
        advance_to(expiry);
        let retry = fixture
            .build()
            .map_err(|e| e.to_string())
            .expect("the note is free once the transaction has expired");
        assert_eq!(sapling_nullifiers(&first), sapling_nullifiers(&retry));
    }
}

/// A wallet-owned Sapling output observed in a mempool transaction that the
/// wallet database has not yet scanned (CRD ch.39 R39.8.0af).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingReceipt {
    /// Transaction identifier, as reported by the backend.
    pub(crate) txid: [u8; 32],
    /// Index of the decrypting output within that transaction. Together with
    /// `txid` this keys the receipt, so repeated observation across polls
    /// contributes its value at most once (R39.8.0ah).
    pub(crate) output_index: u32,
    /// Note value in zatoshi.
    pub(crate) value: u64,
}

impl ZCoinShieldedHistory {
    /// Trial-decrypt the backend's current mempool and return every
    /// wallet-owned output it contains that the wallet database has not
    /// already scanned.
    ///
    /// Returns an empty set rather than an error when the backend cannot be
    /// reached or the response is unusable: a pending view is advisory, and
    /// R39.8.0ai requires it to degrade to zero rather than fail the balance.
    pub(crate) async fn fetch_pending_receipts(&self, servers: &[String], tip: u64) -> Vec<PendingReceipt> {
        use sapling::keys::PreparedIncomingViewingKey;
        use sapling::note::ExtractedNoteCommitment;
        use sapling::note_encryption::{try_sapling_compact_note_decryption, CompactOutputDescription};
        use sapling::Note;
        use zcash_note_encryption::{EphemeralKeyBytes, COMPACT_NOTE_SIZE};
        use zcash_primitives::transaction::components::sapling::zip212_enforcement;

        let ivk = PreparedIncomingViewingKey::new(&self.extfvk.fvk.vk.ivk());
        // Mempool transactions will be mined at the next height at the
        // earliest, so enforcement is evaluated against the tip.
        // Same reason as the scanner: a mempool note from a modern Pirate wallet
        // carries the post-ZIP-212 lead byte, which the coin's own parameters would
        // reject outright (R39.8.0am).
        let enforcement = zip212_enforcement(
            &ZcoinDecryptionParams::new(self.consensus_params.clone()),
            BlockHeight::from_u32(tip as u32),
        );

        for server in servers {
            let mut client = match Self::connect_lightwalletd(server).await {
                Ok(client) => client,
                Err(e) => {
                    log::debug!("ZCoin mempool poll: {server} unreachable: {e}");
                    continue;
                },
            };
            let stream = match client.get_mempool_tx(z_coin_grpc::Exclude { txid: Vec::new() }).await {
                Ok(resp) => resp.into_inner(),
                Err(e) => {
                    log::debug!("ZCoin mempool poll: {server} rejected GetMempoolTx: {e}");
                    continue;
                },
            };
            futures::pin_mut!(stream);

            let mut found = Vec::new();
            while let Some(tx) = stream.next().await {
                let tx = match tx {
                    Ok(tx) => tx,
                    Err(e) => {
                        log::debug!("ZCoin mempool poll: stream from {server} ended early: {e}");
                        break;
                    },
                };
                let txid: [u8; 32] = match tx.hash.as_slice().try_into() {
                    Ok(hash) => hash,
                    // A malformed identifier cannot be reconciled against the
                    // wallet database later, so the entry is unusable.
                    Err(_) => continue,
                };
                for (index, output) in tx.outputs.iter().enumerate() {
                    let Some(cmu) =
                        Option::from(ExtractedNoteCommitment::from_bytes(
                            &match output.cmu.as_slice().try_into() {
                                Ok(bytes) => bytes,
                                Err(_) => continue,
                            },
                        ))
                    else {
                        continue;
                    };
                    let epk: [u8; 32] = match output.epk.as_slice().try_into() {
                        Ok(bytes) => bytes,
                        Err(_) => continue,
                    };
                    let enc_ciphertext: [u8; COMPACT_NOTE_SIZE] = match output.ciphertext.as_slice().try_into() {
                        Ok(bytes) => bytes,
                        Err(_) => continue,
                    };
                    let description = CompactOutputDescription {
                        ephemeral_key: EphemeralKeyBytes(epk),
                        cmu,
                        enc_ciphertext,
                    };
                    if let Some((note, _addr)) = try_sapling_compact_note_decryption(&ivk, &description, enforcement) {
                        found.push(PendingReceipt {
                            txid,
                            output_index: index as u32,
                            value: Note::value(&note).inner(),
                        });
                    }
                }
            }
            return found;
        }

        Vec::new()
    }

    /// True when `txid` already has scanned wallet-database state, meaning its
    /// value is accounted for through normal note accounting and must not also
    /// be counted as pending (R39.8.0ah).
    pub(crate) fn transaction_is_scanned(&self, txid: &[u8; 32]) -> Result<bool, String> {
        let conn = Connection::open_with_flags(&self.wallet_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        // A row alone does not mean scanned: recording a broadcast we made inserts
        // one while the transaction is still unmined (R39.8.0ap), and such a row
        // carries no received note. Only a mined height means the block was scanned
        // and the wallet holds the real notes.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions WHERE txid = ?1 AND mined_height IS NOT NULL;",
                params![&txid[..]],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(count > 0)
    }

    /// Total value of the wallet's notes that the transaction `txid` spends, as recorded in
    /// the wallet database. Zero when the transaction is unknown or spends none of them.
    pub(crate) fn spent_value_of_transaction(&self, txid: &[u8; 32]) -> Result<u64, String> {
        let conn = Connection::open_with_flags(&self.wallet_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(rn.value), 0)
                 FROM sapling_received_note_spends s
                 JOIN sapling_received_notes rn ON rn.id = s.sapling_received_note_id
                 JOIN transactions t ON t.id_tx = s.transaction_id
                 WHERE t.txid = ?1;",
                params![&txid[..]],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        u64::try_from(total).map_err(|e| e.to_string())
    }
}

/// Step B of the ARRR Ironwood plan: `zcash_primitives` 0.30.1 reads version-6
/// transactions unconditionally. In 0.28.0 the v6 arm of `TxVersion::read` sat behind
/// `cfg(zcash_unstable = "nu7")`, which this build does not set, so every v6
/// transaction was rejected as "Unknown transaction format".
#[cfg(test)]
mod v6_transaction_parsing_tests {
    use zcash_primitives::transaction::{Transaction, TxVersion};
    use zcash_protocol::consensus::BranchId;

    /// Raw bytes of the Pirate test-chain transaction at height 68373 (txid
    /// `cdc4274f6c4a...` in internal byte order): a version-6 transaction, recorded from the
    /// offline fixture corpus so the regression outlives that chain.
    const V6_TX_68373_HEX: &str = concat!(
        "0600008098b684d85b16a53700000000dd0b01000000018c9d97662739c35007da2a0e82df63dc1d",
        "98f4d376127d7a46a6f5f51c1644ad08164481c8a82d474cf3ea3344d14f84303f84a62eafcdbc3d",
        "0293f541e2fec5d11a2e89df9e32dbea9f3eaaa683db9b42161f6be5925a22f86dae754b23c35602",
        "e29c9bb9f8a7f785ac6fcc8e20039df45c875a52fcd64fbd73b8338acb49b7c62c153c2857ee02e5",
        "1262391d973a14cfc326ab0d06dd4f665efadf497e7a6741ccfe961355b697c8a7f3c4d846e97f9e",
        "0c77de46a66be7b14421256fa8b5db273450893d0bbf3af5232d763b5c0b1a2e96d70baee0ab6ab8",
        "f8f0ed9925069e3f325430b32d715e997a7c3b437ed50eddb3b6486c32ad4a869f13f175369a4769",
        "ccef5b0b4e5f5f061c44966131b2ef6cc43884c65e7159a81b567e409145e0cf2eaa19a4a0589be7",
        "3e31a3ecc24bfcb45093299134384ebda38d41092038fb320ce63264ad18086209305323aae2eedd",
        "8b0dc2192d6c9fe25ae84487b446f6e79778fa710afc3fc2c6d761152398d57bd2b5f02bff164edb",
        "e4430602f55b119571778f582c0c1128ec49f55fd108108075e3d2d0d4d425fa55ee14cbcf4b3fab",
        "ddac4b15e3fe760a9a26d62f5abadc5ec4fd13d9c9a7d912b08581d5d5bd5fc0ef7d5527e7d5a8bf",
        "4e2a03c6356c0bfabf6a93a13e1af98cbf547116764af592f51074cb14362e74e61c669e774ba40e",
        "dc2c82003c6e11d5a9b96ae6b4efee2dc3c1b5625bd722ac535be1f9c6aed005ab7713fa558617af",
        "af185ce498eceacaa79a30ff323ad1313c7addef522b3edeb6f04e4263cb0889fa4a21882f8b2435",
        "7a025de15737c8a618a5c2b30078bae8e0ad54e723f4f705b7b95e3d759f5e573e15497fe97d8058",
        "3857a3c527e421e796a7e933e11401614dc7a0e225e74cbee471f464f0764c1263cdc9791f709bb2",
        "08d1b1c934e0b4851ccbd3a1cef480272eb3a07912f1096d76a3718edff250f2d819ecc82d1cd22c",
        "85a0d33a71b4307ad3e21562de5a24005286387d18f4ea8ff6ef63b301297ce4d479d03403ef895c",
        "0480aa5ec4aad867bdb3e2c0e51c2918e4ddce5073a7352cca41196e3bd47c18996908424c41182f",
        "99a1c8dc3f690689bead535a4283f71caa86fc55447b63b7a09612009e7f993bd3c63129a86288c3",
        "3c8de3f4c7217e2f676a23b3d4c4f6bc616ba7aeb5f0c58c965de94ee3b32b18f86b3a2f3bea5237",
        "d69684dbf8b5d308d5ee3d6f6fa994421db220126df7c9b3298943ebf67ababe983795f799124c22",
        "b23ee8d41937caa6d990407ff0059ca580aabf15f62a4196b81fa7750cb817f9c8d4126261d5b413",
        "4dba1b8f0a6ec73c519569b68f1e9684182008820bd7c7502aaa185d016cea3f770e119ce76e6cd6",
        "77ca553fb0116470478cecb6d3d20daa42f7b0af8433e93d6b3909511eefc8c62b3cd5c256289be9",
        "ffec1464d6ba8e0713db617d3c074faaa3b0afaee2b8f5caf56bea218d8a91ade3603c945f367a14",
        "0bc22f6e8521a0b31baef1f80a420f962c4f105da7c198a5161cd1093c5b57780e1b84dfb0044733",
        "c30edf368e32f4bd10031d3e47089c6c6c2bed9fa1462285a544015c21603de539dcc9cd17654adf",
        "29184662700e974261e3116fdb520bb957077185ed805800b157698e31bd51824e2f8eba6f57ab25",
        "214a7be200b9dc64a780c185a0301a6f206a970da277e036a4c96f3d6f4ec6d5dd2bf64895610275",
        "10d39250658a3f462ec02556bd8730ceb6a2f3a1f6f93431ba2c176d69297137160979b6e9ae725e",
        "e3a9efdf1de6ba07184ca144f44fe5049af4fc72f3cad146c54149693e4989457f9a53f0ec4a3ae0",
        "175df32fe7b41ed96a9488014ad8faf35864755f0a7f273774d8782c889b5c98994017ed07058e7c",
        "75856089192889136715457a6f727db5058395f49552caeb49085750807ad1eb302aaf3d811ad5a4",
        "75273965c9b6c05351f0f6d05e0087a3611d34593637d0acbeddbbc27b2eb372fbe4a30a4da48ce5",
        "cb4b199beb4aae99152cff406a9e70f3c40bf8bda991c0646beaabc1b461ad72202f3debce368db4",
        "a950837863dffc37a828e8cbc8262ba9c08b5a7dc7a260ce9b3765a4ad0ff96ae03c44b70b4df137",
        "ebb8da51a6b048925b30826a037ea2bdbece598dcdc18fe99c114fa60354948bf1f373e6ecc3ff5d",
        "3b634c4e64e9268543c4befc332248393ac1406e1ebd70a8d2b5ed094f081d4714b1f497ee8214be",
        "84422062a915ccc70985860c19bab7145af3eb06c31a7ec007e97023a6f302fd1027000000000000",
        "09076428c32f511cb5427f2c7a5e97195342a18c0692937420c8d1f048eb9529acb5870af996dbf9",
        "984f82fcba49a233d9dfc66f4a522682347ed1a35bfe6959e24eb502186cd0501ea478ea3d519495",
        "a7419df8319044b6773ba1ffc0c3f96316d8e6da4ef3cbe31843e428bf87d3121460478659eb4a55",
        "dd06a7017a2995290464652a97253ca3674810fc448d6b56cff602928ef3d9f31cfc716a68704ef4",
        "e309a7381b6263d9e113146e9f41ec52a71f14aaf61745fd4da8865caa69c445db684e457ff98fbc",
        "623039fa555b5c927da7b657bb2915afbb7a9cb5b44b55327efe3fd4841f396f12b512d8390702a0",
        "742ee5ed0e162132f61d5e88be490f8a31f554bee15d8e20c070d2e268987c29e815de89f497fe66",
        "b6eaad61079fcf0aae6f52541922b8f707e58d4b32f05525b8f351b3c410d4892c48acbda160e1ac",
        "d647ae18fd86ec73ce35b43832a8ef76b34b41faecb4a6e7b1c6a567f15d005c5557b4bc1cc005b5",
        "c87cd389669001cbf42d428b2f28d334c757801bcd418e0502071f0243941652c7ec0f8b9a714715",
        "1b57d89bb79c808daa6caaab932c4e8f2db7b9cfe5cf440b77f394f79ae51586825cc0e5ddb2861d",
        "73de07ba29fb90d40af05c96a8c9e60ce9b66170c95d58d3ddb82ff768d3f48de63b44efebb30a42",
        "a7b8aa2073aa1df99a1ce6ff26a2d8a29372c9d007a6f2f75bcd663252c26d22a72b5c97004e5a36",
        "4bea7bf8031a3cdcafa19a3fef9a43c16dbb0bb1745615ffb1a271df8ffce05f09f80bb294b03ad2",
        "2f5d7ac0b9d80688d2b638b2501c8d4d16e770c160698270fd8a7bdf59d9ef19fc04317d6c3f0523",
        "929bc544c811d32ff7b33ed26e9ec173fbe4eda27aadda94b1c7b6f7988b603445b7a13e95b3568d",
        "592ddd415157d1ad4992cefb5dc3641a5d97ed20295963f307ddac4767da1cc7e5f045eb18d1efe9",
        "74fe2b9b0cd9efc0474244e0a334e1b7435631b85937350f4d1fba590afdee6f064e4df691dabf19",
        "ce177d4b1407e48e09274cd18504ad060000",
    );
    const V6_TX_68373_TXID_INTERNAL_PREFIX: &str = "cdc4274f6c4a";

    fn parse(raw: &[u8]) -> std::io::Result<Transaction> {
        // The branch id argument only matters for pre-v5 layouts; v5/v6 carry their own.
        Transaction::read(raw, BranchId::Sapling)
    }

    #[test]
    fn a_v6_transaction_parses_with_the_expected_version_and_txid() {
        let raw = hex::decode(V6_TX_68373_HEX).unwrap();
        let tx = parse(&raw).unwrap();
        assert_eq!(tx.version(), TxVersion::V6);
        assert_eq!(tx.consensus_branch_id(), BranchId::Nu6_3);
        assert!(
            hex::encode(tx.txid().as_ref()).starts_with(V6_TX_68373_TXID_INTERNAL_PREFIX),
            "txid must be the v6 digest, not a double-SHA256 of the wire bytes"
        );
        // Round trip: the parser must consume and re-emit exactly the same bytes.
        let mut out = Vec::new();
        tx.write(&mut out).unwrap();
        assert_eq!(out, raw);
    }

    /// Parses every transaction of the offline corpus (`local/arrr-ironwood-fixtures`,
    /// untracked third-party chain data) when `ARRR_IRONWOOD_FIXTURES` points at it, and
    /// checks each parsed txid against the corpus index. Without the variable it is a no-op.
    #[test]
    fn the_offline_ironwood_corpus_parses_and_txids_match() {
        let Some(dir) = std::env::var_os("ARRR_IRONWOOD_FIXTURES") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("tx_index.json")).unwrap()).unwrap();
        let mut v6 = 0usize;
        let mut v4 = 0usize;
        for entry in index.as_array().unwrap() {
            let txid_internal = entry["txid_internal"].as_str().unwrap();
            let height = entry["height"].as_u64().unwrap();
            let file = std::fs::read_dir(dir.join("raw"))
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .find(|n| n.starts_with(&format!("h{height}_")) && n.contains(&txid_internal[..12]))
                .unwrap();
            let raw = hex::decode(std::fs::read_to_string(dir.join("raw").join(file)).unwrap().trim()).unwrap();
            let tx = parse(&raw).unwrap_or_else(|e| panic!("height {height}: {e}"));
            assert_eq!(hex::encode(tx.txid().as_ref()), txid_internal, "height {height}");
            match tx.version() {
                TxVersion::V6 => v6 += 1,
                TxVersion::V4 => v4 += 1,
                other => panic!("height {height}: unexpected version {other:?}"),
            }
            let sapling = tx.sapling_bundle();
            assert_eq!(
                sapling.map_or(0, |b| b.shielded_spends().len() as u64),
                entry["spends"].as_u64().unwrap(),
                "height {height} spends"
            );
            assert_eq!(
                sapling.map_or(0, |b| b.shielded_outputs().len() as u64),
                entry["outputs"].as_u64().unwrap(),
                "height {height} outputs"
            );
            let mut out = Vec::new();
            tx.write(&mut out).unwrap();
            assert_eq!(out, raw, "height {height} round trip");
        }
        assert_eq!((v4, v6), (1, 18));
    }
}
