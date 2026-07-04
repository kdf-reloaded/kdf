/******************************************************************************
 * Copyright © 2014-2019 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/
//
//  lp_native_dex.rs
//  marketmaker
//

use coins::register_balance_update_handler;
use common::executor::{spawn, spawn_boxed, Timer};
use common::log::{error, info, warn};
use crypto::{CryptoCtx, CryptoInitError, EncryptedMnemonicData, HwError, HwProcessingError};
use derive_more::Display;
use kdf_crypto::sha256;
use mm2_core::mm_ctx::{MmArc, MmCtx};
use mm2_err_handle::prelude::*;
use mm2_net_config::{net_config_for, NetConfig, SUPPORTED_NETIDS};
use mm2_p2p::{spawn_gossipsub, AdexBehaviourError, NodeType, RelayAddress, RelayAddressError, WssCerts};
use rpc_task::RpcTaskError;
use serde_json::{self as json};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::str;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use crate::mm2::database::init_and_migrate_db;
use crate::mm2::lp_message_service::{init_message_service, InitMessageServiceError};
use crate::mm2::lp_network::{lp_network_ports, p2p_event_process_loop, subscribe_to_own_peer_healthcheck_topic,
                             NetIdError, P2PContext};
use crate::mm2::lp_ordermatch::{broadcast_maker_orders_keep_alive_loop, clean_memory_loop, init_ordermatch_context,
                                lp_ordermatch_loop, orders_kick_start, BalanceUpdateOrdermatchHandler,
                                OrdermatchInitError};
use crate::mm2::lp_swap::{running_swaps_num, swap_kick_starts};
use crate::mm2::rpc::spawn_rpc;
use crate::mm2::{MM_DATETIME, MM_VERSION};

cfg_native! {
    use mm2_io::fs::{ensure_dir_is_writable, ensure_file_is_writable};
    use mm2_net::ip_addr::myipaddr;
    use db_common::sqlite::rusqlite::Error as SqlError;
}

#[path = "lp_init/init_context.rs"] mod init_context;
#[path = "lp_init/init_hw.rs"] pub mod init_hw;

pub type P2PResult<T> = Result<T, MmError<P2PInitError>>;
pub type MmInitResult<T> = Result<T, MmError<MmInitError>>;

#[derive(Clone, Debug, Display, Serialize)]
pub enum P2PInitError {
    #[display(
        fmt = "Invalid WSS key/cert at {:?}. The file must contain {}'",
        path,
        expected_format
    )]
    InvalidWssCert { path: PathBuf, expected_format: String },
    #[display(fmt = "Error deserializing '{}' config field: {}", field, error)]
    ErrorDeserializingConfig { field: String, error: String },
    #[display(fmt = "The '{}' field not found in the config", field)]
    FieldNotFoundInConfig { field: String },
    #[display(fmt = "Error reading WSS key/cert file {:?}: {}", path, error)]
    ErrorReadingCertFile { path: PathBuf, error: String },
    #[display(fmt = "Error getting my IP address: '{}'", _0)]
    ErrorGettingMyIpAddr(String),
    #[display(fmt = "Invalid netid: '{}'", _0)]
    InvalidNetId(NetIdError),
    #[display(fmt = "Invalid relay address: '{}'", _0)]
    InvalidRelayAddress(RelayAddressError),
    #[display(fmt = "Error listening on P2P address '{}': {}", address, error)]
    ErrorListeningOnAddress { address: String, error: String },
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    #[display(fmt = "WASM node can be a seed if only 'p2p_in_memory' is true")]
    WasmNodeCannotBeSeed,
    #[display(fmt = "Internal error: '{}'", _0)]
    Internal(String),
}

impl From<NetIdError> for P2PInitError {
    fn from(e: NetIdError) -> Self { P2PInitError::InvalidNetId(e) }
}

impl From<AdexBehaviourError> for P2PInitError {
    fn from(e: AdexBehaviourError) -> Self {
        match e {
            AdexBehaviourError::ParsingRelayAddress(e) => P2PInitError::InvalidRelayAddress(e),
            AdexBehaviourError::ListenOn { address, error } => P2PInitError::ErrorListeningOnAddress { address, error },
        }
    }
}

#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MmInitError {
    Canceled,
    #[display(fmt = "Initialization timeout {:?}", _0)]
    Timeout(Duration),
    #[display(fmt = "Error deserializing '{}' config field: {}", field, error)]
    ErrorDeserializingConfig {
        field: String,
        error: String,
    },
    #[display(fmt = "The '{}' field not found in the config", field)]
    FieldNotFoundInConfig {
        field: String,
    },
    #[display(fmt = "P2P initializing error: '{}'", _0)]
    P2PError(P2PInitError),
    #[display(fmt = "Error creating DB director '{:?}': {}", path, error)]
    ErrorCreatingDbDir {
        path: PathBuf,
        error: String,
    },
    #[display(fmt = "{} db dir is not writable", path)]
    DbDirectoryIsNotWritable {
        path: String,
    },
    #[display(fmt = "{} db file is not writable", path)]
    DbFileIsNotWritable {
        path: String,
    },
    #[display(fmt = "sqlite initializing error: {}", _0)]
    ErrorSqliteInitializing(String),
    #[display(fmt = "DB migrating error: {}", _0)]
    ErrorDbMigrating(String),
    #[display(fmt = "Swap kick start error: {}", _0)]
    SwapsKickStartError(String),
    #[display(fmt = "Order kick start error: {}", _0)]
    OrdersKickStartError(String),
    NullStringPassphrase,
    #[display(fmt = "Invalid passphrase: {}", _0)]
    InvalidPassphrase(String),
    #[display(fmt = "No Trezor device available")]
    NoTrezorDeviceAvailable,
    #[display(fmt = "Hardware Wallet error: {}", _0)]
    HardwareWalletError(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(
        fmt = "Unsupported netid {}: no compiled configuration. Supported: {:?}",
        netid,
        supported
    )]
    UnsupportedNetId {
        netid: u16,
        supported: &'static [u16],
    },
    #[display(fmt = "Wallet error: {}", _0)]
    WalletError(String),
}

impl From<P2PInitError> for MmInitError {
    fn from(e: P2PInitError) -> Self {
        match e {
            P2PInitError::ErrorDeserializingConfig { field, error } => {
                MmInitError::ErrorDeserializingConfig { field, error }
            },
            P2PInitError::FieldNotFoundInConfig { field } => MmInitError::FieldNotFoundInConfig { field },
            P2PInitError::Internal(e) => MmInitError::Internal(e),
            other => MmInitError::P2PError(other),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<SqlError> for MmInitError {
    fn from(e: SqlError) -> Self { MmInitError::ErrorSqliteInitializing(e.to_string()) }
}

impl From<OrdermatchInitError> for MmInitError {
    fn from(e: OrdermatchInitError) -> Self {
        match e {
            OrdermatchInitError::ErrorDeserializingConfig { field, error } => {
                MmInitError::ErrorDeserializingConfig { field, error }
            },
            OrdermatchInitError::Internal(internal) => MmInitError::Internal(internal),
        }
    }
}

impl From<InitMessageServiceError> for MmInitError {
    fn from(e: InitMessageServiceError) -> Self {
        match e {
            InitMessageServiceError::ErrorDeserializingConfig { field, error } => {
                MmInitError::ErrorDeserializingConfig { field, error }
            },
        }
    }
}

impl From<CryptoInitError> for MmInitError {
    fn from(e: CryptoInitError) -> Self {
        match e {
            e @ CryptoInitError::InitializedAlready | e @ CryptoInitError::NotInitialized => {
                MmInitError::Internal(e.to_string())
            },
            CryptoInitError::EmptyPassphrase => MmInitError::NullStringPassphrase,
            CryptoInitError::InvalidPassphrase(pass) => MmInitError::InvalidPassphrase(pass.to_string()),
            CryptoInitError::Internal(internal) => MmInitError::Internal(internal),
        }
    }
}

impl From<HwError> for MmInitError {
    fn from(e: HwError) -> Self {
        match e {
            HwError::NoTrezorDeviceAvailable => MmInitError::NoTrezorDeviceAvailable,
            HwError::Internal(internal) => MmInitError::Internal(internal),
            hw => MmInitError::HardwareWalletError(hw.to_string()),
        }
    }
}

impl From<RpcTaskError> for MmInitError {
    fn from(e: RpcTaskError) -> Self {
        let error = e.to_string();
        match e {
            RpcTaskError::Canceled => MmInitError::Canceled,
            RpcTaskError::Timeout(timeout) => MmInitError::Timeout(timeout),
            RpcTaskError::NoSuchTask(_) | RpcTaskError::UnexpectedTaskStatus { .. } => MmInitError::Internal(error),
            RpcTaskError::Internal(internal) => MmInitError::Internal(internal),
        }
    }
}

impl From<HwProcessingError<RpcTaskError>> for MmInitError {
    fn from(e: HwProcessingError<RpcTaskError>) -> Self {
        match e {
            HwProcessingError::HwError(hw) => MmInitError::from(hw),
            HwProcessingError::ProcessorError(rpc_task) => MmInitError::from(rpc_task),
        }
    }
}

impl MmInitError {
    pub fn db_directory_is_not_writable(path: &str) -> MmInitError {
        MmInitError::DbDirectoryIsNotWritable { path: path.to_owned() }
    }
}

fn validate_netid_range(conf: &json::Value) -> MmInitResult<()> {
    if let Some(netid) = conf["netid"].as_u64() {
        if netid > u16::MAX as u64 {
            return MmError::err(MmInitError::ErrorDeserializingConfig {
                field: "netid".to_owned(),
                error: format!("netid {} exceeds u16::MAX ({})", netid, u16::MAX),
            });
        }
    }
    Ok(())
}

fn startup_net_config(netid: u16) -> MmInitResult<&'static dyn NetConfig> {
    match net_config_for(netid) {
        Some(cfg) => Ok(cfg),
        None => MmError::err(MmInitError::UnsupportedNetId {
            netid,
            supported: SUPPORTED_NETIDS,
        }),
    }
}

/// The three mutually-exclusive forms the configuration `passphrase` field may
/// take, recognised purely by structural shape (R26). Defined here (rather than
/// in the native-only `lp_wallet` module) because the startup handshake must
/// recognise the field on both native and WASM targets.
#[derive(Debug)]
pub(crate) enum PassphraseForm {
    /// The field is missing or JSON `null`.
    Absent,
    /// A JSON string carrying the mnemonic in clear.
    Plaintext(String),
    /// A JSON object matching the encrypted-mnemonic envelope.
    Encrypted(EncryptedMnemonicData),
}

/// Parses the configuration `passphrase` field into one of the three forms of
/// R26. The object (encrypted) form is recognised before the string (plaintext)
/// form; an object that is not a well-formed envelope is a configuration error.
pub(crate) fn parse_passphrase_form(conf: &json::Value) -> MmInitResult<PassphraseForm> {
    let value = &conf["passphrase"];
    if value.is_null() {
        return Ok(PassphraseForm::Absent);
    }
    if value.is_object() {
        let data: EncryptedMnemonicData =
            json::from_value(value.clone()).map_to_mm(|e| MmInitError::ErrorDeserializingConfig {
                field: "passphrase".to_owned(),
                error: format!("passphrase object is not a well-formed encrypted envelope: {e}"),
            })?;
        return Ok(PassphraseForm::Encrypted(data));
    }
    let plaintext: String = json::from_value(value.clone()).map_to_mm(|e| MmInitError::ErrorDeserializingConfig {
        field: "passphrase".to_owned(),
        error: e.to_string(),
    })?;
    Ok(PassphraseForm::Plaintext(plaintext))
}

fn default_seednode_from_str(netid: u16, seed: &str) -> Option<RelayAddress> {
    match seed.parse() {
        Ok(RelayAddress::IPv4(ipv4)) => Some(RelayAddress::IPv4(ipv4)),
        Ok(RelayAddress::Dns(dns)) => Some(RelayAddress::Dns(dns)),
        Ok(RelayAddress::Memory(_)) => {
            error!(
                "Invalid default P2P seednode '{}' for netid {}: registry seednodes must be IPv4 or DNS hosts",
                seed, netid
            );
            None
        },
        Err(e) => {
            error!("Invalid default P2P seednode '{}' for netid {}: {}", seed, netid, e);
            None
        },
    }
}

fn default_seednodes_from_strings(netid: u16, seed_nodes: &[&str]) -> Vec<RelayAddress> {
    seed_nodes
        .iter()
        .filter_map(|seed| default_seednode_from_str(netid, seed))
        .collect()
}

/// Returns compile-time seed nodes from NetConfig for the given netid,
/// preserving hostnames so libp2p can resolve them and report seed-level diagnostics.
fn default_seednodes(netid: u16) -> P2PResult<Vec<RelayAddress>> {
    match net_config_for(netid) {
        Some(cfg) => Ok(default_seednodes_from_strings(netid, cfg.seed_nodes())),
        None => MmError::err(P2PInitError::Internal(format!(
            "No compiled network configuration for netid {}",
            netid
        ))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fix_directories(ctx: &MmCtx) -> MmInitResult<()> {
    let dbdir = ctx.dbdir();
    std::fs::create_dir_all(&dbdir).map_to_mm(|e| MmInitError::ErrorCreatingDbDir {
        path: dbdir.clone(),
        error: e.to_string(),
    })?;

    if !ensure_dir_is_writable(&dbdir.join("SWAPS")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("SWAPS"));
    }
    if !ensure_dir_is_writable(&dbdir.join("SWAPS").join("MY")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("SWAPS/MY"));
    }
    if !ensure_dir_is_writable(&dbdir.join("SWAPS").join("STATS")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("SWAPS/STATS"));
    }
    if !ensure_dir_is_writable(&dbdir.join("SWAPS").join("STATS").join("MAKER")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("SWAPS/STATS/MAKER"));
    }
    if !ensure_dir_is_writable(&dbdir.join("SWAPS").join("STATS").join("TAKER")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("SWAPS/STATS/TAKER"));
    }
    if !ensure_dir_is_writable(&dbdir.join("TRANSACTIONS")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("TRANSACTIONS"));
    }
    if !ensure_dir_is_writable(&dbdir.join("GTC")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("GTC"));
    }
    if !ensure_dir_is_writable(&dbdir.join("PRICES")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("PRICES"));
    }
    if !ensure_dir_is_writable(&dbdir.join("UNSPENTS")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("UNSPENTS"));
    }
    if !ensure_dir_is_writable(&dbdir.join("ORDERS")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("ORDERS"));
    }
    if !ensure_dir_is_writable(&dbdir.join("ORDERS").join("MY")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("ORDERS/MY"));
    }
    if !ensure_dir_is_writable(&dbdir.join("ORDERS").join("MY").join("MAKER")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("ORDERS/MY/MAKER"));
    }
    if !ensure_dir_is_writable(&dbdir.join("ORDERS").join("MY").join("TAKER")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("ORDERS/MY/TAKER"));
    }
    if !ensure_dir_is_writable(&dbdir.join("ORDERS").join("MY").join("HISTORY")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("ORDERS/MY/HISTORY"));
    }
    if !ensure_dir_is_writable(&dbdir.join("TX_CACHE")) {
        return MmError::err(MmInitError::db_directory_is_not_writable("TX_CACHE"));
    }
    ensure_file_is_writable(&dbdir.join("GTC").join("orders")).map_to_mm(|_| MmInitError::DbFileIsNotWritable {
        path: "GTC/orders".to_owned(),
    })?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn migrate_db(ctx: &MmArc) -> MmInitResult<()> {
    let migration_num_path = ctx.dbdir().join(".migration");
    let mut current_migration = match std::fs::read(&migration_num_path) {
        Ok(bytes) => {
            let mut num_bytes = [0; 8];
            if bytes.len() == 8 {
                num_bytes.clone_from_slice(&bytes);
                u64::from_le_bytes(num_bytes)
            } else {
                0
            }
        },
        Err(_) => 0,
    };

    if current_migration < 1 {
        migration_1(ctx);
        current_migration = 1;
    }
    std::fs::write(&migration_num_path, current_migration.to_le_bytes())
        .map_to_mm(|e| MmInitError::ErrorDbMigrating(e.to_string()))?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn migration_1(_ctx: &MmArc) {}

pub async fn lp_init_continue(ctx: MmArc) -> MmInitResult<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        fix_directories(&ctx)?;
        ctx.init_sqlite_connection()
            .map_to_mm(MmInitError::ErrorSqliteInitializing)?;
        init_and_migrate_db(&ctx).await?;
        migrate_db(&ctx)?;
    }

    init_ordermatch_context(&ctx).mm_err(Into::into)?;
    init_message_service(&ctx).await.mm_err(Into::into)?;
    init_p2p(ctx.clone()).await.mm_err(Into::into)?;

    let balance_update_ordermatch_handler = BalanceUpdateOrdermatchHandler::new(ctx.clone());
    register_balance_update_handler(ctx.clone(), Box::new(balance_update_ordermatch_handler)).await;

    ctx.initialized.pin(true).map_to_mm(MmInitError::Internal)?;

    // launch kickstart threads before RPC is available, this will prevent the API user to place
    // an order and start new swap that might get started 2 times because of kick-start
    kick_start(ctx.clone()).await?;

    spawn(lp_ordermatch_loop(ctx.clone()));

    spawn(broadcast_maker_orders_keep_alive_loop(ctx.clone()));

    spawn(clean_memory_loop(ctx.weak()));
    Ok(())
}

#[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
/// * `ctx_cb` - callback used to share the `MmCtx` ID with the call site.
pub async fn lp_init(ctx: MmArc) -> MmInitResult<()> {
    info!("Version: {} DT {}", MM_VERSION, MM_DATETIME);

    validate_netid_range(&ctx.conf)?;

    // Validate netid against compiled network configurations ("deny except config exists").
    let netid = ctx.netid();
    let net_cfg = match startup_net_config(netid) {
        Ok(net_cfg) => net_cfg,
        Err(err) => {
            let supported_desc: Vec<String> = SUPPORTED_NETIDS
                .iter()
                .filter_map(|&id| net_config_for(id).map(|cfg| format!("  netid {} — {}", id, cfg.network_name())))
                .collect();
            if netid == 0 {
                error!(
                    "No 'netid' specified in MM2.json. You must set a supported network ID.\nSupported networks:\n{}",
                    supported_desc.join("\n")
                );
            } else {
                error!(
                    "Unsupported netid {}: no compiled configuration.\nSupported networks:\n{}",
                    netid,
                    supported_desc.join("\n")
                );
            }
            return Err(err);
        },
    };
    info!("Network: {} (netid {})", net_cfg.network_name(), netid);

    // Recognise the three forms of the configured `passphrase` field (R26):
    // absent/null, an encrypted envelope object, or a plaintext mnemonic string.
    let passphrase_form = parse_passphrase_form(&ctx.conf)?;

    // Resolve the signing-identity seed from the startup wallet handshake per the
    // R27 decision matrix. On native targets this consults the on-disk wallet
    // store (load-and-use on re-login, generate/first-save/import, or confirm); on
    // WASM there is no store, so only the anonymous and legacy-plaintext rows apply.
    #[cfg(not(target_arch = "wasm32"))]
    let resolved_seed = {
        let wallet_name = ctx.conf["wallet_name"].as_str();
        let wallet_password = ctx.conf["wallet_password"].as_str();
        crate::mm2::lp_wallet::initialize_wallet_passphrase(&ctx, passphrase_form, wallet_name, wallet_password)
            .await
            .map_err(|e| MmError::new(MmInitError::WalletError(e.to_string())))?
    };
    #[cfg(target_arch = "wasm32")]
    let resolved_seed = match passphrase_form {
        PassphraseForm::Absent => None,
        PassphraseForm::Plaintext(seed) => Some(seed),
        PassphraseForm::Encrypted(_) => {
            return MmError::err(MmInitError::WalletError(
                "An encrypted passphrase requires the native wallet store and is unsupported on this target"
                    .to_string(),
            ));
        },
    };

    // Identity invariant (R28): every resolved plaintext seed — including the pure
    // re-login load-and-use path — initialises the node's signing identity. Only
    // the anonymous row leaves the node without an identity. Per R45.4.8 this is
    // the single startup site that consumes the resolved seed, so `enable_hd`
    // selects the key-pair policy here for every resolved-seed path: a truthy
    // `enable_hd` binds the seed to a global-HD account, otherwise to the
    // baseline Iguana single-key context.
    if let Some(passphrase) = resolved_seed {
        if ctx.enable_hd() {
            CryptoCtx::init_with_global_hd_account(ctx.clone(), &passphrase).mm_err(Into::into)?;
        } else {
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), &passphrase).mm_err(Into::into)?;
        }
    }
    lp_init_continue(ctx.clone()).await?;

    let ctx_id = ctx.ffi_handle().map_to_mm(MmInitError::Internal)?;

    spawn_rpc(ctx_id);
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    if let Err(err) =
        crate::mm2::rpc::streaming_activations::shutdown_signal::install_shutdown_signal_listener(ctx.clone())
    {
        warn!("Could not install shutdown-signal listener: {}", err);
    }
    let ctx_c = ctx.clone();
    spawn(async move {
        if let Err(err) = ctx_c.init_metrics() {
            warn!("Couldn't initialize metrics system: {}", err);
        }
    });
    // In the mobile version we might depend on `lp_init` staying around until the context stops.
    loop {
        if ctx.is_stopping() {
            break;
        };
        Timer::sleep(0.2).await
    }

    // wait for swaps to stop
    loop {
        if running_swaps_num(&ctx) == 0 {
            break;
        };
        Timer::sleep(0.2).await
    }
    Ok(())
}

async fn kick_start(ctx: MmArc) -> MmInitResult<()> {
    let mut coins_needed_for_kick_start = swap_kick_starts(ctx.clone())
        .await
        .map_to_mm(MmInitError::SwapsKickStartError)?;
    coins_needed_for_kick_start.extend(
        orders_kick_start(&ctx)
            .await
            .map_to_mm(MmInitError::OrdersKickStartError)?,
    );
    let mut lock = ctx
        .coins_needed_for_kick_start
        .lock()
        .map_to_mm(|poison| MmInitError::Internal(poison.to_string()))?;
    *lock = coins_needed_for_kick_start;
    Ok(())
}

async fn init_p2p(ctx: MmArc) -> P2PResult<()> {
    let i_am_seed = ctx.conf["i_am_seed"].as_bool().unwrap_or(false);

    if i_am_seed && ctx.secp256k1_key_pair.as_option().is_none() {
        return MmError::err(P2PInitError::Internal(
            "i_am_seed requires an initialized signing identity".to_owned(),
        ));
    }

    let seednodes = seednodes(&ctx)?;

    let ctx_on_poll = ctx.clone();
    let force_p2p_key = if i_am_seed {
        let key = sha256(&*ctx.secp256k1_key_pair().private().secret);
        Some(key.take())
    } else {
        None
    };

    let node_type = if i_am_seed {
        relay_node_type(&ctx).await?
    } else {
        light_node_type(&ctx)?
    };

    let spawn_result = spawn_gossipsub(force_p2p_key, spawn_boxed, seednodes, node_type, move |swarm| {
        let behaviour = swarm.behaviour();
        mm_gauge!(
            ctx_on_poll.metrics,
            "p2p.connected_relays.len",
            behaviour.connected_relays_len() as i64
        );
        mm_gauge!(
            ctx_on_poll.metrics,
            "p2p.relay_mesh.len",
            behaviour.relay_mesh_len() as i64
        );
        let (period, received_msgs) = behaviour.received_messages_in_period();
        mm_gauge!(
            ctx_on_poll.metrics,
            "p2p.received_messages.period_in_secs",
            period.as_secs() as i64
        );

        mm_gauge!(ctx_on_poll.metrics, "p2p.received_messages.count", received_msgs as i64);

        let connected_peers_count = behaviour.connected_peers_len();

        mm_gauge!(
            ctx_on_poll.metrics,
            "p2p.connected_peers.count",
            connected_peers_count as i64
        );
    })
    .await;
    let (cmd_tx, event_rx, peer_id, p2p_abort) = spawn_result?;
    let mut p2p_abort = Some(p2p_abort);
    ctx.on_stop(Box::new(move || {
        if let Some(handle) = p2p_abort.take() {
            handle.abort();
        }
        Ok(())
    }));
    ctx.peer_id.pin(peer_id.to_string()).map_to_mm(P2PInitError::Internal)?;
    let p2p_context = P2PContext::new(cmd_tx);
    p2p_context.store_to_mm_arc(&ctx);
    subscribe_to_own_peer_healthcheck_topic(&ctx, &peer_id.to_string());
    spawn(p2p_event_process_loop(ctx.weak(), event_rx, i_am_seed));

    Ok(())
}

fn resolve_bootstrap_relays<F>(
    conf: &json::Value,
    netid: u16,
    p2p_in_memory: bool,
    registry_seednodes: F,
) -> P2PResult<Vec<RelayAddress>>
where
    F: FnOnce(u16) -> P2PResult<Vec<RelayAddress>>,
{
    if conf["seednodes"].is_null() {
        if p2p_in_memory {
            // If the network is in memory, there is no need to use default seednodes.
            info!("P2P in-memory network selected; no seednodes will be used");
            return Ok(Vec::new());
        }
        let seednodes = registry_seednodes(netid)?;
        if seednodes.is_empty() {
            warn!(
                "No default P2P seednodes configured for netid {}; relay discovery will rely on already known peers",
                netid
            );
        } else {
            info!(
                "Using {} default P2P seednodes for netid {}: {:?}",
                seednodes.len(),
                netid,
                seednodes
            );
        }
        return Ok(seednodes);
    }

    let seednodes: Vec<RelayAddress> =
        json::from_value(conf["seednodes"].clone()).map_to_mm(|e| P2PInitError::ErrorDeserializingConfig {
            field: "seednodes".to_owned(),
            error: e.to_string(),
        })?;
    if seednodes.is_empty() {
        warn!("The 'seednodes' config field is present but empty; relay discovery may not find peers");
    } else {
        info!("Using {} P2P seednodes from config: {:?}", seednodes.len(), seednodes);
    }
    Ok(seednodes)
}

fn seednodes(ctx: &MmArc) -> P2PResult<Vec<RelayAddress>> {
    resolve_bootstrap_relays(&ctx.conf, ctx.netid(), ctx.p2p_in_memory(), default_seednodes)
}

#[cfg(target_arch = "wasm32")]
async fn relay_node_type(ctx: &MmArc) -> P2PResult<NodeType> {
    if ctx.p2p_in_memory() {
        return relay_in_memory_node_type(ctx);
    }
    MmError::err(P2PInitError::WasmNodeCannotBeSeed)
}

#[cfg(not(target_arch = "wasm32"))]
async fn relay_node_type(ctx: &MmArc) -> P2PResult<NodeType> {
    if ctx.p2p_in_memory() {
        return relay_in_memory_node_type(ctx);
    }

    let netid = ctx.netid();
    let ip = myipaddr(ctx.clone())
        .await
        .map_to_mm(P2PInitError::ErrorGettingMyIpAddr)?;
    let network_ports = lp_network_ports(netid).mm_err(Into::into)?;
    let wss_certs = wss_certs(ctx)?;
    if wss_certs.is_none() {
        const WARN_MSG: &str = r#"Please note TLS private key and certificate are not specified.
To accept P2P WSS connections, please pass 'wss_certs' to the config.
Example:    "wss_certs": { "server_priv_key": "/path/to/key.pem", "certificate": "/path/to/cert.pem" }"#;
        warn!("{}", WARN_MSG);
    }

    Ok(NodeType::Relay {
        ip,
        network_ports,
        wss_certs,
    })
}

fn relay_in_memory_node_type(ctx: &MmArc) -> P2PResult<NodeType> {
    let port = ctx
        .p2p_in_memory_port()
        .or_mm_err(|| P2PInitError::FieldNotFoundInConfig {
            field: "p2p_in_memory_port".to_owned(),
        })?;
    Ok(NodeType::RelayInMemory { port })
}

fn light_node_type(ctx: &MmArc) -> P2PResult<NodeType> {
    if ctx.p2p_in_memory() {
        return Ok(NodeType::LightInMemory);
    }

    let netid = ctx.netid();
    let network_ports = lp_network_ports(netid).mm_err(Into::into)?;
    Ok(NodeType::Light { network_ports })
}

/// Returns non-empty vector of keys/certs or an error.
#[cfg(not(target_arch = "wasm32"))]
fn extract_cert_from_file<T, P>(path: PathBuf, parser: P, expected_format: String) -> P2PResult<Vec<T>>
where
    P: Fn(&mut dyn io::BufRead) -> Result<Vec<T>, ()>,
{
    let certfile = fs::File::open(path.as_path()).map_to_mm(|e| P2PInitError::ErrorReadingCertFile {
        path: path.clone(),
        error: e.to_string(),
    })?;
    let mut reader = io::BufReader::new(certfile);
    match parser(&mut reader) {
        Ok(certs) if certs.is_empty() => MmError::err(P2PInitError::InvalidWssCert { path, expected_format }),
        Ok(certs) => Ok(certs),
        Err(_) => MmError::err(P2PInitError::InvalidWssCert { path, expected_format }),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wss_certs(ctx: &MmArc) -> P2PResult<Option<WssCerts>> {
    use futures_rustls::rustls;

    #[derive(Deserialize)]
    struct WssCertsInfo {
        server_priv_key: PathBuf,
        certificate: PathBuf,
    }

    if ctx.conf["wss_certs"].is_null() {
        return Ok(None);
    }
    let certs: WssCertsInfo =
        json::from_value(ctx.conf["wss_certs"].clone()).map_to_mm(|e| P2PInitError::ErrorDeserializingConfig {
            field: "wss_certs".to_owned(),
            error: e.to_string(),
        })?;

    // First, try to extract the all PKCS8 private keys
    let mut server_priv_keys = extract_cert_from_file(
        certs.server_priv_key.clone(),
        rustls::internal::pemfile::pkcs8_private_keys,
        "Private key, DER-encoded ASN.1 in either PKCS#8 or PKCS#1 format".to_owned(),
    )
    // or try to extract all PKCS1 private keys
    .or_else(|_| {
        extract_cert_from_file(
            certs.server_priv_key.clone(),
            rustls::internal::pemfile::rsa_private_keys,
            "Private key, DER-encoded ASN.1 in either PKCS#8 or PKCS#1 format".to_owned(),
        )
    })?;
    // `extract_cert_from_file` returns either non-empty vector or an error.
    let server_priv_key = server_priv_keys.remove(0);

    let certs = extract_cert_from_file(
        certs.certificate,
        rustls::internal::pemfile::certs,
        "Certificate, DER-encoded X.509 format".to_owned(),
    )?;
    Ok(Some(WssCerts { server_priv_key, certs }))
}

#[cfg(test)]
mod tests {
    use super::{default_seednodes, default_seednodes_from_strings, parse_passphrase_form, resolve_bootstrap_relays,
                startup_net_config, validate_netid_range, MmInitError, P2PInitError, PassphraseForm};
    #[cfg(not(target_arch = "wasm32"))]
    use crate::mm2::lp_network::lp_network_ports;
    #[cfg(not(target_arch = "wasm32"))] use mm2_p2p::NetworkInfo;
    use mm2_p2p::RelayAddress;
    use serde_json::json;

    #[test]
    fn passphrase_absent_is_absent_form() {
        let conf = json!({});
        let form = parse_passphrase_form(&conf).expect("passphrase resolution should succeed");
        assert!(matches!(form, PassphraseForm::Absent));

        let null_conf = json!({ "passphrase": null });
        let form = parse_passphrase_form(&null_conf).expect("passphrase resolution should succeed");
        assert!(matches!(form, PassphraseForm::Absent));
    }

    #[test]
    fn passphrase_unexpected_types_refuse_cleanly() {
        let num_conf = json!({ "passphrase": 1 });
        let err = parse_passphrase_form(&num_conf).unwrap_err();
        assert!(matches!(
            err.get_inner(),
            MmInitError::ErrorDeserializingConfig { field, .. } if field == "passphrase"
        ));

        let bool_conf = json!({ "passphrase": true });
        let err = parse_passphrase_form(&bool_conf).unwrap_err();
        assert!(matches!(
            err.get_inner(),
            MmInitError::ErrorDeserializingConfig { field, .. } if field == "passphrase"
        ));
    }

    #[test]
    fn malformed_passphrase_envelope_refuses_cleanly() {
        let conf = json!({ "passphrase": { "foo": "bar" } });
        let err = parse_passphrase_form(&conf).unwrap_err();
        assert!(matches!(
            err.get_inner(),
            MmInitError::ErrorDeserializingConfig { field, .. } if field == "passphrase"
        ));
    }

    #[test]
    fn empty_string_passphrase_is_plaintext() {
        let conf = json!({ "passphrase": "" });
        let form = parse_passphrase_form(&conf).expect("passphrase resolution should succeed");
        assert!(matches!(form, PassphraseForm::Plaintext(s) if s.is_empty()));
    }

    #[test]
    fn well_formed_envelope_is_encrypted_form() {
        let envelope = crypto::encrypt_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            "pw",
        )
        .unwrap();
        let conf = json!({ "passphrase": serde_json::to_value(&envelope).unwrap() });
        let form = parse_passphrase_form(&conf).expect("passphrase resolution should succeed");
        assert!(matches!(form, PassphraseForm::Encrypted(_)));
    }

    #[test]
    fn out_of_range_netid_refuses_cleanly() {
        let conf = json!({ "netid": u16::MAX as u64 + 1 });
        let err = validate_netid_range(&conf).unwrap_err();
        assert!(matches!(
            err.get_inner(),
            MmInitError::ErrorDeserializingConfig { field, .. } if field == "netid"
        ));
    }

    #[test]
    fn bootstrap_resolution_operator_seednodes_win_over_registry_fixture() {
        let conf = json!({ "seednodes": ["operator.seed.example", "198.51.100.20"] });
        let registry_seednodes = ["registry.seed.example", "203.0.113.7"];
        let actual = resolve_bootstrap_relays(&conf, 6133, false, |netid| {
            panic!(
                "registry fallback must not be consulted for netid {} when operator seednodes exist: {:?}",
                netid, registry_seednodes
            )
        })
        .expect("operator seednodes should parse");

        assert_eq!(actual, vec![
            RelayAddress::Dns("operator.seed.example".to_owned()),
            RelayAddress::IPv4("198.51.100.20".to_owned()),
        ]);
    }

    #[test]
    fn bootstrap_resolution_explicit_empty_seednodes_suppress_registry_fallback() {
        let conf = json!({ "seednodes": [] });
        let actual = resolve_bootstrap_relays(&conf, 6133, false, |_| {
            panic!("registry fallback must not be consulted when seednodes is explicitly empty")
        })
        .expect("explicit empty seednodes should be accepted");

        assert!(actual.is_empty());
    }

    #[test]
    fn bootstrap_resolution_absent_or_null_seednodes_use_registry_fallback() {
        let registry_seednodes = ["203.0.113.8", "registry.seed.example"];
        let omitted_conf = json!({});
        let null_conf = json!({ "seednodes": null });
        let expected = vec![
            RelayAddress::IPv4("203.0.113.8".to_owned()),
            RelayAddress::Dns("registry.seed.example".to_owned()),
        ];

        let omitted_actual = resolve_bootstrap_relays(&omitted_conf, 6133, false, |netid| {
            Ok(default_seednodes_from_strings(netid, &registry_seednodes))
        })
        .expect("omitted seednodes should use registry fallback");
        let null_actual = resolve_bootstrap_relays(&null_conf, 6133, false, |netid| {
            Ok(default_seednodes_from_strings(netid, &registry_seednodes))
        })
        .expect("null seednodes should use registry fallback");

        assert_eq!(omitted_actual, expected);
        assert_eq!(null_actual, expected);
    }

    #[test]
    fn bootstrap_resolution_empty_registry_fallback_is_allowed() {
        let conf = json!({});
        let actual = resolve_bootstrap_relays(&conf, 6133, false, |_| Ok(Vec::new()))
            .expect("empty registry fallback should not refuse startup");

        assert!(actual.is_empty());
    }

    #[test]
    fn bootstrap_resolution_in_memory_omits_registry_fallback() {
        let conf = json!({});
        let actual = resolve_bootstrap_relays(&conf, 6133, true, |_| {
            panic!("registry fallback must not be consulted for in-memory P2P")
        })
        .expect("in-memory P2P should resolve to no bootstrap relays");

        assert!(actual.is_empty());
    }

    #[test]
    fn registry_seednode_strings_accept_only_ipv4_or_dns_hosts() {
        let actual = default_seednodes_from_strings(6133, &[
            "203.0.113.9",
            "registry.seed.example",
            "/memory/123",
            "/ip4/203.0.113.9/tcp/9999",
        ]);

        assert_eq!(actual, vec![
            RelayAddress::IPv4("203.0.113.9".to_owned()),
            RelayAddress::Dns("registry.seed.example".to_owned()),
        ]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_registry_hosts_use_netid_tcp_port_not_wss() {
        let netid = 6133;
        let registry_seednodes = ["203.0.113.10", "registry.seed.example"];
        let relays = resolve_bootstrap_relays(&json!({}), netid, false, |netid| {
            Ok(default_seednodes_from_strings(netid, &registry_seednodes))
        })
        .expect("registry fallback hosts should parse");
        let network_ports = lp_network_ports(netid).expect("netid should map to P2P ports");
        assert_ne!(network_ports.tcp, network_ports.wss);

        let network_info = NetworkInfo::Distributed { network_ports };
        let multiaddrs: Vec<String> = relays
            .iter()
            .map(|relay| {
                relay
                    .try_to_multiaddr(network_info)
                    .expect("registry host should normalize")
                    .to_string()
            })
            .collect();

        assert_eq!(multiaddrs, vec![
            format!("/ip4/203.0.113.10/tcp/{}", network_ports.tcp),
            format!("/dns/registry.seed.example/tcp/{}", network_ports.tcp),
        ]);
        assert!(!multiaddrs
            .iter()
            .any(|addr| addr.ends_with(&format!("/tcp/{}", network_ports.wss))));
    }

    #[test]
    fn startup_net_config_rejects_unknown_netid_before_seed_resolution() {
        let unsupported_netid = 9999;
        let err = startup_net_config(unsupported_netid)
            .err()
            .expect("unsupported netid should be rejected");
        assert!(matches!(
            err.get_inner(),
            MmInitError::UnsupportedNetId { netid, .. } if *netid == unsupported_netid
        ));

        let err = resolve_bootstrap_relays(&json!({}), unsupported_netid, false, default_seednodes)
            .expect_err("unknown netid must not resolve to an empty bootstrap list");
        assert!(matches!(
            err.get_inner(),
            P2PInitError::Internal(msg) if msg.contains("No compiled network configuration for netid 9999")
        ));
    }
}
