use super::z_rpc::z_coin_grpc;
use super::{CheckPointBlockInfo, ZCoinBuildError, ZcoinConsensusParams};
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use common::mm_number::BigDecimal;
use common::{calc_total_pages, log, PagingOptionsEnum};
use db_common::sqlite::rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use futures::StreamExt;
use mm2_err_handle::prelude::*;
use parking_lot::Mutex;
use prost_14::Message as ProstMessage;
use rand::rngs::OsRng;
use sapling::zip32::ExtendedFullViewingKey;
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

type LightwalletdClient = z_coin_grpc::compact_tx_streamer_client::CompactTxStreamerClient<Channel>;
type ReloadedWalletDb = WalletDb<Connection, ZcoinConsensusParams, SystemClock, OsRng>;

#[derive(Clone, Debug)]
pub(crate) struct ZCoinShieldedHistory {
    compact_blocks_path: PathBuf,
    wallet_db_path: PathBuf,
    consensus_params: ZcoinConsensusParams,
    extfvk: ExtendedFullViewingKey,
    initial_chain_state: Arc<Mutex<Option<ChainState>>>,
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
            WalletDbGeneration::ReferenceLegacy => {
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

        Ok(ZCoinShieldedHistory {
            compact_blocks_path: paths.compact_blocks_path,
            wallet_db_path: paths.wallet_db_path,
            consensus_params,
            extfvk: extfvk.clone(),
            initial_chain_state: Arc::new(Mutex::new(initial_chain_state)),
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
    ) -> Result<u64, String> {
        let Some(fetch_plan) = self.lightwalletd_fetch_plan(
            consensus_params,
            target_height,
            requested_start_height,
            skip_sync_params,
        )?
        else {
            log::info!(
                "ZCoin shielded wallet DB already scanned through requested lightwalletd target height {}",
                target_height
            );
            return Ok(self.scanned_height()?.unwrap_or(0));
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
        for server in servers {
            match self
                .fetch_compact_blocks_from_server(consensus_params.clone(), server, fetch_plan, target_height, progress)
                .await
            {
                Ok(fetched_height) => return Ok(fetched_height),
                Err(e) => {
                    log::warn!("ZCoin lightwalletd server {} failed: {}", server, e);
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
        Err(error)
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
            let should_reseed_empty_checkpoint =
                resumed_start < default_recent_start && self.wallet_scan_state_is_empty()?;
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
    ) -> Result<u64, String> {
        let started = Instant::now();
        let start_height = fetch_plan.start_height;
        log::info!(
            "ZCoin lightwalletd server {} scan started: compact block range {}..={}",
            server,
            start_height,
            target_height
        );

        let mut client = Self::connect_lightwalletd(server).await?;

        if fetch_plan.reset_stale_empty_checkpoint {
            log::info!(
                "ZCoin shielded wallet DB has stale empty checkpoint; resetting before fetching from height {}",
                start_height
            );
            self.reset_empty_wallet_scan_state()?;
        } else if fetch_plan.reset_scan_state {
            log::info!(
                "ZCoin shielded wallet DB scan state reset requested before fetching from height {}",
                start_height
            );
            self.reset_wallet_scan_state()?;
        }

        if start_height > 0 {
            self.ensure_lightwalletd_chain_state(&mut client, consensus_params.clone(), start_height - 1)
                .await?;
        }

        let mut fetched_height = start_height.saturating_sub(1);
        match self.validated_cached_resume_height(start_height, target_height) {
            Ok(Some(cached_height)) => {
                fetched_height = cached_height;
                log::info!(
                    "ZCoin lightwalletd reusing validated compact cache through height {}",
                    cached_height
                );
            },
            Ok(None) => {},
            Err(error) => {
                log::warn!(
                    "ZCoin compact cache cannot resume from height {}: {}; preserving it and refetching",
                    start_height,
                    error
                );
                preserve_database_for_rebuild(&self.compact_blocks_path, "invalid-chain")?;
                initialize_compact_db(&self.compact_blocks_path)?;
            },
        }
        progress(fetched_height, target_height);
        let mut batch_start = fetched_height
            .checked_add(1)
            .ok_or_else(|| "Compact block height overflow".to_owned())?;
        while batch_start <= target_height {
            let batch_end = std::cmp::min(target_height, batch_start + LIGHTWALLETD_BLOCK_BATCH_SIZE - 1);
            fetched_height = self
                .fetch_compact_block_batch_from_server(&mut client, batch_start, batch_end)
                .await?;
            if fetched_height < batch_end {
                return Err(format!(
                    "lightwalletd returned compact blocks through {}, below requested batch end {}",
                    fetched_height, batch_end
                ));
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
        *self.initial_chain_state.lock() = None;
        preserve_database_for_rebuild(&self.wallet_db_path, "rescan")?;
        preserve_database_for_rebuild(&self.compact_blocks_path, "rescan")?;
        initialize_compact_db(&self.compact_blocks_path)?;
        initialize_wallet_db(&self.wallet_db_path, self.consensus_params.clone())
    }

    fn reset_unscanned_wallet_db(&self) -> Result<(), String> {
        if !self.wallet_scan_state_is_empty()? {
            return Err("Refusing to rebuild an unscanned shielded wallet DB that contains transactions".to_owned());
        }
        *self.initial_chain_state.lock() = None;
        preserve_database_for_rebuild(&self.wallet_db_path, "checkpoint")?;
        initialize_wallet_db(&self.wallet_db_path, self.consensus_params.clone())
    }

    async fn connect_lightwalletd(server: &str) -> Result<LightwalletdClient, String> {
        let endpoint_url = lightwalletd_endpoint(server);
        let endpoint = Endpoint::from_shared(endpoint_url.clone())
            .map_err(|e| lightwalletd_error_with_sources(&e))?
            .connect_timeout(LIGHTWALLETD_CONNECT_TIMEOUT)
            .timeout(LIGHTWALLETD_REQUEST_TIMEOUT);
        let endpoint = if endpoint_url.starts_with("https://") {
            endpoint
                .tls_config(ClientTlsConfig::new())
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

    async fn ensure_lightwalletd_chain_state(
        &self,
        client: &mut LightwalletdClient,
        consensus_params: ZcoinConsensusParams,
        checkpoint_height: u64,
    ) -> Result<(), String> {
        if self
            .initial_chain_state
            .lock()
            .as_ref()
            .is_some_and(|state| u64::from(u32::from(state.block_height())) == checkpoint_height)
        {
            log::trace!(
                "ZCoin shielded wallet reusing in-memory chain state at height {}",
                checkpoint_height
            );
            return Ok(());
        }

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

        log::info!(
            "ZCoin lightwalletd requesting wallet checkpoint tree state at height {}",
            checkpoint_height
        );
        let request = z_coin_grpc::BlockId {
            height: checkpoint_height,
            hash: Vec::new(),
        };
        let tree_state = tokio::time::timeout(LIGHTWALLETD_REQUEST_TIMEOUT, client.get_tree_state(request))
            .await
            .map_err(|_| format!("GetTreeState timed out at height {}", checkpoint_height))?
            .map_err(|e| lightwalletd_error_with_sources(&e))?
            .into_inner();
        if tree_state.height != checkpoint_height {
            return Err(format!(
                "lightwalletd returned tree state at height {}, expected {}",
                tree_state.height, checkpoint_height
            ));
        }
        self.init_wallet_checkpoint_from_tree_state(consensus_params, tree_state)
    }

    fn init_wallet_checkpoint_from_tree_state(
        &self,
        consensus_params: ZcoinConsensusParams,
        tree_state: z_coin_grpc::TreeState,
    ) -> Result<(), String> {
        let hash = decode_display_block_hash("lightwalletd tree-state block hash", &tree_state.hash)?;
        let sapling_tree = decode_hex_field("lightwalletd tree-state sapling tree", &tree_state.tree)?;
        let checkpoint_height = BlockHeight::from_u32(tree_state.height.try_into().map_err(|_| {
            format!(
                "lightwalletd tree-state height {} does not fit into u32",
                tree_state.height
            )
        })?);
        let commitment_tree: sapling::CommitmentTree =
            read_commitment_tree(sapling_tree.as_slice()).map_err(|e| e.to_string())?;
        let chain_state = ChainState::new(checkpoint_height, BlockHash(hash), commitment_tree.to_frontier());
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
                return Err(format!(
                    "lightwalletd tree-state hash at height {} does not match the shielded wallet DB",
                    checkpoint_height
                ));
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
        Ok(())
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
            let summary = match scan_cached_blocks(
                &consensus_params,
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
                    tx_hash: hex::encode(txid),
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
        proto_version: block.proto_version,
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
        Ok(_) => WalletDbGeneration::Unknown,
        Err(_) => WalletDbGeneration::Corrupt,
    }
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
            proto_version: 0,
            height,
            hash: vec![hash_byte; 32],
            prev_hash: vec![prev_hash_byte; 32],
            time: u32::try_from(height).unwrap(),
            header: Vec::new(),
            vtx: Vec::new(),
            chain_metadata: Some(zcash_compact::ChainMetadata {
                sapling_commitment_tree_size: 0,
                orchard_commitment_tree_size: 0,
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
            proto_version: 0,
            height,
            hash: vec![hash_byte; 32],
            prev_hash: vec![prev_hash_byte; 32],
            time: u32::try_from(height).unwrap(),
            header: Vec::new(),
            vtx: vec![transaction],
            chain_metadata: Some(zcash_compact::ChainMetadata {
                sapling_commitment_tree_size: 1,
                orchard_commitment_tree_size: 0,
            }),
        }
    }

    fn deterministic_compact_hash(height: u64) -> Vec<u8> {
        let mut hash = vec![0u8; 32];
        hash[..8].copy_from_slice(&height.to_le_bytes());
        hash
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
            params![1i64, vec![1u8; 32], 10u32, 10u32, 0u32, 10u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transactions (
                id_tx, txid, block, mined_height, tx_index, min_observed_height
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![2i64, vec![2u8; 32], 11u32, 11u32, 0u32, 11u32],
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
                    proto_version: 0,
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
        };
        let db_dir = history.wallet_db_path().parent().unwrap().to_owned();
        drop(history);

        let reopened =
            ZCoinShieldedHistory::open_or_create("ARRR", db_dir, params.clone(), &tracked_extfvk, None).unwrap();
        assert!(reopened.initial_chain_state.lock().is_none());

        let mut wrong_hash_state = tree_state.clone();
        wrong_hash_state.hash = hex::encode([0u8; 32]);
        let error = reopened
            .init_wallet_checkpoint_from_tree_state(params.clone(), wrong_hash_state)
            .unwrap_err();
        assert!(error.contains("does not match the shielded wallet DB"));
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

        reopened
            .init_wallet_checkpoint_from_tree_state(params.clone(), tree_state)
            .unwrap();
        reopened
            .insert_compact_block(zcash_compact::CompactBlock {
                proto_version: 0,
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
        assert_eq!(fingerprint.records.len(), 467);
        assert_eq!(
            hex::encode(hasher.finalize()),
            "dfba9135b6d5ae446e88d83d162e9458730b5b098051d8565fdd91fe6be5bea8"
        );
        assert_eq!(migration_ids.len(), 48);
        assert_eq!(
            hex::encode(migration_hasher.finalize()),
            "7b506df8a2b119fb143664a1c0b5eddfa7de5fe8458f07829818c59e5b39ddbb"
        );
    }
}
