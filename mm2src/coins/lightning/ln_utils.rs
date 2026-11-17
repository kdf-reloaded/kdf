use super::*;
use crate::lightning::ln_platform::{get_best_header, ln_best_block_update_loop, update_best_block};
use crate::utxo::rpc_clients::BestBlock as RpcBestBlock;
use crate::PrivKeyActivationPolicy;
use bitcoin::hash_types::BlockHash;
use bitcoin_hashes::{sha256d, Hash};
use common::executor::{spawn, Timer};
use common::log;
use common::log::LogState;
use crypto::{CryptoCtx, KeyPairPolicy};
use lightning::chain::keysinterface::{InMemorySigner, KeysManager};
use lightning::chain::{chainmonitor, BestBlock, Watch};
use lightning::ln::channelmanager;
use lightning::ln::channelmanager::{ChainParameters, ChannelManagerReadArgs, SimpleArcChannelManager};
use lightning::routing::network_graph::NetworkGraph;
use lightning::util::config::UserConfig;
use lightning::util::ser::ReadableArgs;
use lightning_persister::storage::{DbStorage, FileSystemStorage, NodesAddressesMap, Scorer};
use lightning_persister::LightningPersister;
use mm2_core::mm_ctx::MmArc;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const NETWORK_GRAPH_PERSIST_INTERVAL: u64 = 600;
const SCORER_PERSIST_INTERVAL: u64 = 600;

pub type ChainMonitor = chainmonitor::ChainMonitor<
    InMemorySigner,
    Arc<Platform>,
    Arc<Platform>,
    Arc<Platform>,
    Arc<LogState>,
    Arc<LightningPersister>,
>;

pub type ChannelManager = SimpleArcChannelManager<ChainMonitor, Platform, Platform, LogState>;

/// The name of the file that records which node a Lightning data directory belongs to.
const NODE_ID_FILE: &str = "node_id";

/// The name of the sub-directory (and, as a suffix, of the SQL table names) of the internal-key identity.
const INTERNAL_KEY_DIR: &str = "internal-key";

/// Which Lightning identity (node seed, and with it node id, channel manager, monitors and history) an activation
/// runs.
///
/// The two identities never share a directory, a backup directory or a table: switching the policy for a ticker
/// shows the other identity's channels and payments no more, and switching back shows them again (CRD ch.05 R46
/// item 2, status note of 2026-11-17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightningIdentity {
    /// The passphrase-hash seed: today's identity, and the only one under an iguana login (where it equals the
    /// internal key). Its data stays exactly where it always was.
    PassphraseSeed,
    /// The internal key (`CryptoCtx::mm2_internal_privkey_secret()`) of a global-HD login, selected by
    /// `priv_key_policy: IguanaPrivKey`.
    InternalKey,
}

impl LightningIdentity {
    /// An absent value or `ContextPrivKey` keeps the passphrase-hash seed; `IguanaPrivKey` selects the internal key
    /// under a global-HD login; under an iguana login the two are one key, so nothing changes there.
    pub fn select(ctx: &MmArc, policy: PrivKeyActivationPolicy) -> EnableLightningResult<LightningIdentity> {
        if !matches!(policy, PrivKeyActivationPolicy::IguanaPrivKey) {
            return Ok(LightningIdentity::PassphraseSeed);
        }
        let crypto_ctx =
            CryptoCtx::from_ctx(ctx).mm_err(|e| EnableLightningError::InvalidConfiguration(e.to_string()))?;
        Ok(match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::GlobalHDAccount(_) => LightningIdentity::InternalKey,
            KeyPairPolicy::Iguana => LightningIdentity::PassphraseSeed,
        })
    }

    /// The 32-byte node seed of the identity.
    fn seed(&self, ctx: &MmArc) -> EnableLightningResult<[u8; 32]> {
        match self {
            LightningIdentity::PassphraseSeed => Ok(ctx.legacy_passphrase_key_pair().private().secret.into()),
            LightningIdentity::InternalKey => {
                let crypto_ctx =
                    CryptoCtx::from_ctx(ctx).mm_err(|e| EnableLightningError::InvalidConfiguration(e.to_string()))?;
                Ok(crypto_ctx.mm2_internal_privkey_secret().into())
            },
        }
    }

    /// The other identity of the node.
    fn other(&self) -> LightningIdentity {
        match self {
            LightningIdentity::PassphraseSeed => LightningIdentity::InternalKey,
            LightningIdentity::InternalKey => LightningIdentity::PassphraseSeed,
        }
    }

    /// How the identity is named to the user, with the `priv_key_policy` that selects it.
    fn user_name(&self) -> &'static str {
        match self {
            LightningIdentity::PassphraseSeed => {
                "passphrase-seed identity (the default priv_key_policy, ContextPrivKey)"
            },
            LightningIdentity::InternalKey => "internal-key identity (priv_key_policy IguanaPrivKey)",
        }
    }

    /// The identity's own part of a data path, below the ticker.
    fn below_ticker(&self, ticker_dir: PathBuf) -> PathBuf {
        match self {
            LightningIdentity::PassphraseSeed => ticker_dir,
            LightningIdentity::InternalKey => ticker_dir.join(INTERNAL_KEY_DIR),
        }
    }

    /// The name the persister keys its SQL tables by. The passphrase seed keeps the name it always had.
    fn storage_ticker(&self, ticker: &str) -> String {
        let base = ticker.replace('-', "_");
        match self {
            LightningIdentity::PassphraseSeed => base,
            LightningIdentity::InternalKey => format!("{}_{}", base, INTERNAL_KEY_DIR.replace('-', "_")),
        }
    }
}

#[inline]
fn ln_data_dir(ctx: &MmArc, ticker: &str, identity: LightningIdentity) -> PathBuf {
    identity.below_ticker(ctx.dbdir().join("LIGHTNING").join(ticker))
}

#[inline]
fn ln_data_backup_dir(ctx: &MmArc, path: Option<String>, ticker: &str, identity: LightningIdentity) -> Option<PathBuf> {
    path.map(|p| {
        identity.below_ticker(
            PathBuf::from(&p)
                .join(&hex::encode(&**ctx.rmd160()))
                .join("LIGHTNING")
                .join(ticker),
        )
    })
}

/// Refuses to open a data directory that belongs to another node.
///
/// The directory records the node id it was first opened with. A directory that records none (the data of a node
/// from before the record existed) is adopted by the node that opens it. A directory that records another node id
/// is never opened: its channel manager and monitors would be read with keys that are not theirs.
fn check_node_identity(dir: &Path, node_id: &str) -> EnableLightningResult<()> {
    let path = dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(recorded) if recorded.trim().eq_ignore_ascii_case(node_id) => Ok(()),
        Ok(recorded) => MmError::err(EnableLightningError::NodeIdentityMismatch {
            data_dir: dir.display().to_string(),
            recorded: recorded.trim().to_owned(),
            requested: node_id.to_owned(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, node_id).map_to_mm(|e| EnableLightningError::IOError(e.to_string()))
        },
        Err(e) => MmError::err(EnableLightningError::IOError(e.to_string())),
    }
}

/// Refuses to start an identity while the other identity of the node still has channels that are not settled.
///
/// Only one identity runs at a time, and the channels of the other one are then not watched, so a counterparty could
/// close one with an old state unnoticed. The other identity's SQL channel history is read for this (see
/// `has_unsettled_channels` for what counts as unsettled), which needs none of its keys. A missing or empty history,
/// and a history of closed channels only, allow the activation. A history that cannot be read refuses it: when it is
/// unknown whether a channel is closed, the activation does not go on.
async fn check_other_identity_settled(
    ctx: &MmArc,
    ticker: &str,
    identity: LightningIdentity,
) -> EnableLightningResult<()> {
    let other = identity.other();
    let sqlite_connection = ctx
        .sqlite_connection
        .ok_or(MmError::new(EnableLightningError::DbError(
            "sqlite_connection is not initialized".into(),
        )))?
        .clone();
    let unsettled =
        lightning_persister::has_unsettled_channels(sqlite_connection, other.storage_ticker(ticker)).await?;
    if unsettled {
        return MmError::err(EnableLightningError::OtherIdentityHasOpenChannels {
            ticker: ticker.to_owned(),
            other_identity: other.user_name().to_owned(),
        });
    }
    Ok(())
}

/// The node id (compressed public key, hex) the keys manager gives the node.
fn node_id_of(keys_manager: &KeysManager) -> EnableLightningResult<String> {
    let secret = keys_manager
        .get_node_secret(Recipient::Node)
        .map_to_mm(|_| EnableLightningError::UnsupportedMode("Lightning network".into(), "local node".into()))?;
    let public = PublicKey::from_secret_key(&bitcoin::secp256k1::Secp256k1::signing_only(), &secret);
    Ok(hex::encode(public.serialize()))
}

pub async fn init_persister(
    ctx: &MmArc,
    platform: Arc<Platform>,
    ticker: String,
    backup_path: Option<String>,
    identity: LightningIdentity,
    keys_manager: &KeysManager,
) -> EnableLightningResult<Arc<LightningPersister>> {
    let ln_data_dir = ln_data_dir(ctx, &ticker, identity);
    let ln_data_backup_dir = ln_data_backup_dir(ctx, backup_path, &ticker, identity);
    let persister = Arc::new(LightningPersister::new(
        identity.storage_ticker(&ticker),
        ln_data_dir,
        ln_data_backup_dir,
        ctx.sqlite_connection
            .ok_or(MmError::new(EnableLightningError::DbError(
                "sqlite_connection is not initialized".into(),
            )))?
            .clone(),
    ));
    let is_initialized = persister.is_fs_initialized().await?;
    if !is_initialized {
        persister.init_fs().await?;
    }
    // Nothing is read from the directory, and nothing is spawned for it, before it is known to be this node's.
    check_node_identity(&persister.main_path(), &node_id_of(keys_manager)?)?;
    // And the other identity of the node must have nothing left to watch.
    check_other_identity_settled(ctx, &ticker, identity).await?;
    let is_db_initialized = persister.is_db_initialized().await?;
    if !is_db_initialized {
        persister.init_db().await?;
    }

    let closed_channels_without_closing_tx = persister.get_closed_channels_with_no_closing_tx().await?;
    for channel_details in closed_channels_without_closing_tx {
        let platform = platform.clone();
        let persister = persister.clone();
        let user_channel_id = channel_details.rpc_id;
        spawn(async move {
            if let Ok(closing_tx_hash) = platform
                .get_channel_closing_tx(channel_details)
                .await
                .error_log_passthrough()
            {
                if let Err(e) = persister.add_closing_tx_to_db(user_channel_id, closing_tx_hash).await {
                    log::error!(
                        "Unable to update channel {} closing details in DB: {}",
                        user_channel_id,
                        e
                    );
                }
            }
        });
    }

    Ok(persister)
}

pub fn init_keys_manager(ctx: &MmArc, identity: LightningIdentity) -> EnableLightningResult<Arc<KeysManager>> {
    // The current time is used to derive random numbers from the seed where required, to ensure all random generation is unique across restarts.
    let seed = identity.seed(ctx)?;
    let cur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_to_mm(|e| EnableLightningError::SystemTimeError(e.to_string()))?;

    Ok(Arc::new(KeysManager::new(&seed, cur.as_secs(), cur.subsec_nanos())))
}

pub async fn init_channel_manager(
    platform: Arc<Platform>,
    logger: Arc<LogState>,
    persister: Arc<LightningPersister>,
    keys_manager: Arc<KeysManager>,
    user_config: UserConfig,
) -> EnableLightningResult<(Arc<ChainMonitor>, Arc<ChannelManager>)> {
    // Initialize the FeeEstimator. UtxoStandardCoin implements the FeeEstimator trait, so it'll act as our fee estimator.
    let fee_estimator = platform.clone();

    // Initialize the BroadcasterInterface. UtxoStandardCoin implements the BroadcasterInterface trait, so it'll act as our transaction
    // broadcaster.
    let broadcaster = platform.clone();

    // Initialize the ChainMonitor
    let chain_monitor: Arc<ChainMonitor> = Arc::new(chainmonitor::ChainMonitor::new(
        Some(platform.clone()),
        broadcaster.clone(),
        logger.clone(),
        fee_estimator.clone(),
        persister.clone(),
    ));

    // Read ChannelMonitor state from disk, important for lightning node is restarting and has at least 1 channel
    let mut channelmonitors = persister
        .read_channelmonitors(keys_manager.clone())
        .map_to_mm(|e| EnableLightningError::IOError(e.to_string()))?;

    // This is used for Electrum only to prepare for chain synchronization
    for (_, chan_mon) in channelmonitors.iter() {
        chan_mon.load_outputs_to_watch(&platform);
    }

    let rpc_client = match &platform.coin.as_ref().rpc_client {
        UtxoRpcClientEnum::Electrum(c) => c.clone(),
        UtxoRpcClientEnum::Native(_) => {
            return MmError::err(EnableLightningError::UnsupportedMode(
                "Lightning network".into(),
                "electrum".into(),
            ))
        },
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => {
            return MmError::err(EnableLightningError::UnsupportedMode(
                "Lightning network".into(),
                "electrum".into(),
            ))
        },
    };
    let best_header = get_best_header(&rpc_client).await?;
    platform.update_best_block_height(best_header.block_height());
    let best_block = RpcBestBlock::from(best_header.clone());
    let best_block_hash = BlockHash::from_hash(
        sha256d::Hash::from_slice(&best_block.hash.0).map_to_mm(|e| EnableLightningError::HashError(e.to_string()))?,
    );
    let (channel_manager_blockhash, channel_manager) = {
        if let Ok(mut f) = File::open(persister.manager_path()) {
            let mut channel_monitor_mut_references = Vec::new();
            for (_, channel_monitor) in channelmonitors.iter_mut() {
                channel_monitor_mut_references.push(channel_monitor);
            }
            // Read ChannelManager data from the file
            let read_args = ChannelManagerReadArgs::new(
                keys_manager.clone(),
                fee_estimator.clone(),
                chain_monitor.clone(),
                broadcaster.clone(),
                logger.clone(),
                user_config,
                channel_monitor_mut_references,
            );
            <(BlockHash, ChannelManager)>::read(&mut f, read_args)
                .map_to_mm(|e| EnableLightningError::IOError(e.to_string()))?
        } else {
            // Initialize the ChannelManager to starting a new node without history
            let chain_params = ChainParameters {
                network: platform.network.clone().into(),
                best_block: BestBlock::new(best_block_hash, best_block.height as u32),
            };
            let new_channel_manager = channelmanager::ChannelManager::new(
                fee_estimator.clone(),
                chain_monitor.clone(),
                broadcaster.clone(),
                logger.clone(),
                keys_manager.clone(),
                user_config,
                chain_params,
            );
            (best_block_hash, new_channel_manager)
        }
    };

    let channel_manager: Arc<ChannelManager> = Arc::new(channel_manager);

    // Sync ChannelMonitors and ChannelManager to chain tip if the node is restarting and has open channels
    if channel_manager_blockhash != best_block_hash {
        platform
            .process_txs_unconfirmations(&chain_monitor, &channel_manager)
            .await;
        platform
            .process_txs_confirmations(&rpc_client, &persister, &chain_monitor, &channel_manager)
            .await;
        update_best_block(&chain_monitor, &channel_manager, best_header).await;
    }

    // Give ChannelMonitors to ChainMonitor
    for (_, channel_monitor) in channelmonitors.drain(..) {
        let funding_outpoint = channel_monitor.get_funding_txo().0;
        chain_monitor
            .watch_channel(funding_outpoint, channel_monitor)
            .map_to_mm(|e| EnableLightningError::IOError(format!("{:?}", e)))?;
    }

    // Update best block whenever there's a new chain tip or a block has been newly disconnected
    spawn(ln_best_block_update_loop(
        // It's safe to use unwrap here for now until implementing Native Client for Lightning
        platform,
        persister.clone(),
        chain_monitor.clone(),
        channel_manager.clone(),
        rpc_client.clone(),
        best_block,
    ));

    Ok((chain_monitor, channel_manager))
}

pub async fn persist_network_graph_loop(persister: Arc<LightningPersister>, network_graph: Arc<NetworkGraph>) {
    loop {
        if let Err(e) = persister.save_network_graph(network_graph.clone()).await {
            log::warn!(
                "Failed to persist network graph error: {}, please check disk space and permissions",
                e
            );
        }
        Timer::sleep(NETWORK_GRAPH_PERSIST_INTERVAL as f64).await;
    }
}

pub async fn persist_scorer_loop(persister: Arc<LightningPersister>, scorer: Arc<Mutex<Scorer>>) {
    loop {
        if let Err(e) = persister.save_scorer(scorer.clone()).await {
            log::warn!(
                "Failed to persist scorer error: {}, please check disk space and permissions",
                e
            );
        }
        Timer::sleep(SCORER_PERSIST_INTERVAL as f64).await;
    }
}

pub async fn get_open_channels_nodes_addresses(
    persister: Arc<LightningPersister>,
    channel_manager: Arc<ChannelManager>,
) -> EnableLightningResult<NodesAddressesMap> {
    let channels = channel_manager.list_channels();
    let mut nodes_addresses = persister.get_nodes_addresses().await?;
    nodes_addresses.retain(|pubkey, _node_addr| {
        channels
            .iter()
            .map(|chan| chan.counterparty.node_id)
            .any(|node_id| node_id == *pubkey)
    });
    Ok(nodes_addresses)
}

/// CRD ch.05 R46 item 2, status note of 2026-11-17: the Lightning identity per login and policy, the data
/// directories of the identities, and the refusal of a directory that belongs to another node.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod identity_tests {
    use super::*;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    const MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn hd_ctx() -> MmArc {
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "enable_hd": true }))
            .into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), MNEMONIC).unwrap();
        ctx
    }

    fn iguana_ctx() -> MmArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), MNEMONIC).unwrap();
        ctx
    }

    fn identity(ctx: &MmArc, policy: PrivKeyActivationPolicy) -> LightningIdentity {
        LightningIdentity::select(ctx, policy).unwrap_or_else(|_| panic!("the identity is selected"))
    }

    fn node_id(ctx: &MmArc, identity: LightningIdentity) -> String {
        let keys_manager = init_keys_manager(ctx, identity).unwrap_or_else(|_| panic!("keys manager"));
        node_id_of(&keys_manager).unwrap_or_else(|_| panic!("node id"))
    }

    #[test]
    fn under_an_hd_login_only_iguana_priv_key_selects_the_internal_key_seed() {
        let ctx = hd_ctx();
        let internal = CryptoCtx::from_ctx(&ctx).unwrap().mm2_internal_privkey_secret();
        let passphrase = ctx.legacy_passphrase_key_pair().private().secret;
        assert_ne!(internal, passphrase);

        let iguana = identity(&ctx, PrivKeyActivationPolicy::IguanaPrivKey);
        assert_eq!(iguana, LightningIdentity::InternalKey);
        assert_eq!(
            iguana.seed(&ctx).unwrap_or_else(|_| panic!("seed")),
            internal.as_slice()
        );
        for policy in [PrivKeyActivationPolicy::ContextPrivKey] {
            let kept = identity(&ctx, policy);
            assert_eq!(kept, LightningIdentity::PassphraseSeed);
            assert_eq!(
                kept.seed(&ctx).unwrap_or_else(|_| panic!("seed")),
                passphrase.as_slice()
            );
        }
        // Two identities, two nodes.
        assert_ne!(node_id(&ctx, iguana), node_id(&ctx, LightningIdentity::PassphraseSeed));
    }

    #[test]
    fn under_an_iguana_login_every_policy_is_the_one_identity() {
        let ctx = iguana_ctx();
        let passphrase = ctx.legacy_passphrase_key_pair().private().secret;
        for policy in [
            PrivKeyActivationPolicy::ContextPrivKey,
            PrivKeyActivationPolicy::IguanaPrivKey,
        ] {
            let selected = identity(&ctx, policy);
            assert_eq!(selected, LightningIdentity::PassphraseSeed);
            assert_eq!(
                selected.seed(&ctx).unwrap_or_else(|_| panic!("seed")),
                passphrase.as_slice()
            );
        }
    }

    #[test]
    fn the_passphrase_identity_keeps_its_paths_and_the_internal_key_identity_has_its_own() {
        let ctx = hd_ctx();
        let legacy = ctx.dbdir().join("LIGHTNING").join("LNBTC");
        assert_eq!(ln_data_dir(&ctx, "LNBTC", LightningIdentity::PassphraseSeed), legacy);
        let own = ln_data_dir(&ctx, "LNBTC", LightningIdentity::InternalKey);
        assert_eq!(own, legacy.join("internal-key"));
        assert_ne!(own, legacy);

        let backup = |identity| ln_data_backup_dir(&ctx, Some("/backup".to_owned()), "LNBTC", identity).unwrap();
        let legacy_backup = backup(LightningIdentity::PassphraseSeed);
        assert_eq!(
            legacy_backup,
            PathBuf::from("/backup")
                .join(hex::encode(&**ctx.rmd160()))
                .join("LIGHTNING")
                .join("LNBTC")
        );
        assert_eq!(
            backup(LightningIdentity::InternalKey),
            legacy_backup.join("internal-key")
        );
        assert!(ln_data_backup_dir(&ctx, None, "LNBTC", LightningIdentity::InternalKey).is_none());

        // The SQL tables are not shared either.
        assert_eq!(LightningIdentity::PassphraseSeed.storage_ticker("LN-BTC"), "LN_BTC");
        assert_eq!(
            LightningIdentity::InternalKey.storage_ticker("LN-BTC"),
            "LN_BTC_internal_key"
        );
    }

    #[test]
    fn a_data_directory_of_another_node_is_refused_and_its_own_is_adopted_once() {
        let dir = std::env::temp_dir().join(format!(
            "kdf_ln_identity_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (ours, other) = ("02".to_owned() + &"11".repeat(32), "03".to_owned() + &"22".repeat(32));

        // A directory without a record is adopted, and then recognized.
        assert!(check_node_identity(&dir, &ours).is_ok());
        assert_eq!(std::fs::read_to_string(dir.join(NODE_ID_FILE)).unwrap(), ours);
        assert!(check_node_identity(&dir, &ours).is_ok());
        assert!(check_node_identity(&dir, &ours.to_uppercase()).is_ok());

        // Another node id is refused, and the record is left as it was.
        match check_node_identity(&dir, &other) {
            Err(e) => match e.into_inner() {
                EnableLightningError::NodeIdentityMismatch {
                    recorded, requested, ..
                } => {
                    assert_eq!((recorded, requested), (ours.clone(), other.clone()));
                },
                _ => panic!("a node identity mismatch is expected"),
            },
            Ok(()) => panic!("a directory of another node must not be opened"),
        }
        assert_eq!(std::fs::read_to_string(dir.join(NODE_ID_FILE)).unwrap(), ours);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- the other identity must have nothing left to watch -----

    fn ctx_with_db(ctx: MmArc) -> (MmArc, Arc<Mutex<db_common::sqlite::rusqlite::Connection>>) {
        let conn = Arc::new(Mutex::new(
            db_common::sqlite::rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let _ = ctx.sqlite_connection.pin(conn.clone());
        (ctx, conn)
    }

    /// Creates the history tables of `storage_ticker`, as an identity's first activation does.
    fn create_history(conn: &Arc<Mutex<db_common::sqlite::rusqlite::Connection>>, storage_ticker: &str) {
        let persister = LightningPersister::new(storage_ticker.to_owned(), std::env::temp_dir(), None, conn.clone());
        common::block_on(persister.init_db()).unwrap();
    }

    fn insert_channel(
        conn: &Arc<Mutex<db_common::sqlite::rusqlite::Connection>>,
        storage_ticker: &str,
        rpc_id: u32,
        funding_tx: Option<&str>,
        closing_tx: Option<&str>,
        is_closed: bool,
    ) {
        let sql = format!(
            "INSERT INTO {}_channels_history (rpc_id, channel_id, counterparty_node_id, funding_tx, closing_tx, \
             is_outbound, is_public, is_closed, created_at, last_updated) VALUES (?1, 'c', 'n', ?2, ?3, 1, 0, ?4, 0, 0);",
            storage_ticker
        );
        conn.lock()
            .unwrap()
            .execute(&sql, db_common::sqlite::rusqlite::params![
                rpc_id, funding_tx, closing_tx, is_closed
            ])
            .unwrap();
    }

    fn refusal(ctx: &MmArc, identity: LightningIdentity) -> Option<String> {
        match common::block_on(check_other_identity_settled(ctx, "LNBTC", identity)) {
            Ok(()) => None,
            Err(e) => match e.into_inner() {
                EnableLightningError::OtherIdentityHasOpenChannels { ticker, other_identity } => {
                    assert_eq!(ticker, "LNBTC");
                    Some(other_identity)
                },
                _ => panic!("only the open-channels refusal is expected"),
            },
        }
    }

    #[test]
    fn an_open_channel_of_the_other_identity_refuses_in_both_directions() {
        // A channel that is pending open (no funding transaction yet) or open (funded, not closed).
        for (funding, closing) in [(None, None), (Some("aa"), None)] {
            let (ctx, conn) = ctx_with_db(hd_ctx());
            create_history(&conn, "LNBTC");
            create_history(&conn, "LNBTC_internal_key");
            insert_channel(&conn, "LNBTC", 1, funding, closing, false);
            // The internal-key identity is refused, and the message names the identity to close under.
            let named = refusal(&ctx, LightningIdentity::InternalKey).expect("refused");
            assert!(
                named.contains("passphrase-seed") && named.contains("ContextPrivKey"),
                "{named}"
            );
            // The passphrase-seed identity is not refused by that: its own channels are the ones that run.
            assert!(refusal(&ctx, LightningIdentity::PassphraseSeed).is_none());

            let (ctx, conn) = ctx_with_db(hd_ctx());
            create_history(&conn, "LNBTC_internal_key");
            insert_channel(&conn, "LNBTC_internal_key", 1, funding, closing, false);
            let named = refusal(&ctx, LightningIdentity::PassphraseSeed).expect("refused");
            assert!(
                named.contains("internal-key") && named.contains("IguanaPrivKey"),
                "{named}"
            );
            assert!(refusal(&ctx, LightningIdentity::InternalKey).is_none());
        }
    }

    #[test]
    fn a_funded_closed_channel_without_a_recorded_closing_transaction_refuses() {
        let (ctx, conn) = ctx_with_db(hd_ctx());
        create_history(&conn, "LNBTC");
        // Closed, funded, closing transaction not recorded: unknown whether it is settled, so refuse.
        insert_channel(&conn, "LNBTC", 1, Some("aa"), None, true);
        assert!(refusal(&ctx, LightningIdentity::InternalKey).is_some());
    }

    #[test]
    fn closed_channels_only_an_empty_and_a_missing_history_allow() {
        let (ctx, conn) = ctx_with_db(hd_ctx());
        // No table of the other identity at all.
        assert!(refusal(&ctx, LightningIdentity::InternalKey).is_none());
        // An empty table.
        create_history(&conn, "LNBTC");
        assert!(refusal(&ctx, LightningIdentity::InternalKey).is_none());
        // Closed channels only: funded and closed with the closing transaction recorded, and one closed before it
        // was ever funded.
        insert_channel(&conn, "LNBTC", 1, Some("aa"), Some("bb"), true);
        insert_channel(&conn, "LNBTC", 2, None, None, true);
        assert!(refusal(&ctx, LightningIdentity::InternalKey).is_none());
    }

    #[test]
    fn under_an_iguana_login_the_other_identity_has_no_history_and_nothing_changes() {
        let (ctx, conn) = ctx_with_db(iguana_ctx());
        // Only the passphrase identity exists; its own table is not the one checked.
        create_history(&conn, "LNBTC");
        insert_channel(&conn, "LNBTC", 1, Some("aa"), None, false);
        assert!(refusal(&ctx, LightningIdentity::PassphraseSeed).is_none());
    }
}
