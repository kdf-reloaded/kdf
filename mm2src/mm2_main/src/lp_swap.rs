//! Atomic swap loops and states
//!
//! # A note on the terminology used
//!
//! Alice = Buyer = Liquidity receiver = Taker
//! ("*The process of an atomic swap begins with the person who makes the initial request — this is the liquidity receiver*" - Komodo Whitepaper).
//!
//! Bob = Seller = Liquidity provider = Market maker
//! ("*On the other side of the atomic swap, we have the liquidity provider — we call this person, Bob*" - Komodo Whitepaper).
//!
//! # Algorithm updates
//!
//! At the end of 2018 most UTXO coins have BIP65 (https://github.com/bitcoin/bips/blob/master/bip-0065.mediawiki).
//! The previous swap protocol discussions took place at 2015-2016 when there were just a few
//! projects that implemented CLTV opcode support:
//! https://bitcointalk.org/index.php?topic=1340621.msg13828271#msg13828271
//! https://bitcointalk.org/index.php?topic=1364951
//! So the Tier Nolan approach is a bit outdated, the main purpose was to allow swapping of a coin
//! that doesn't have CLTV at least as Alice side (as APayment is 2of2 multisig).
//! Nowadays the protocol can be simplified to the following (UTXO coins, BTC and forks):
//!
//! 1. AFee: OP_DUP OP_HASH160 FEE_RMD160 OP_EQUALVERIFY OP_CHECKSIG
//!
//! 2. BPayment:
//!    OP_IF
//!    <now + LOCKTIME*2> OP_CLTV OP_DROP <bob_pub> OP_CHECKSIG
//!    OP_ELSE
//!    OP_SIZE 32 OP_EQUALVERIFY OP_HASH160 <hash(bob_privN)> OP_EQUALVERIFY <alice_pub> OP_CHECKSIG
//!    OP_ENDIF
//!
//! 3. APayment:
//!    OP_IF
//!    <now + LOCKTIME> OP_CLTV OP_DROP <alice_pub> OP_CHECKSIG
//!    OP_ELSE
//!    OP_SIZE 32 OP_EQUALVERIFY OP_HASH160 <hash(bob_privN)> OP_EQUALVERIFY <bob_pub> OP_CHECKSIG
//!    OP_ENDIF
//!

/******************************************************************************
 * Copyright © 2014-2018 The SuperNET Developers.                             *
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
//  lp_swap.rs
//  marketmaker
//

use crate::mm2::lp_network::{broadcast_p2p_msg, Libp2pPeerId};
use async_std::sync as async_std_sync;
use coins::{lp_coinfind, lp_coinfind_or_err, CoinFindError, TradeFee, TransactionEnum};
use common::log::{debug, warn};
use common::{bits256, calc_total_pages,
             executor::{spawn, Timer},
             log::{error, info},
             mm_number::{BigDecimal, MmNumber, MmNumberMultiRepr},
             now_ms, HttpStatusCode, PagingOptions};
use derive_more::Display;
use futures::future::{abortable, AbortHandle, TryFutureExt};
use http::{Response, StatusCode};
use kdf_crypto::sha256;
use mm2_core::mm_ctx::{from_ctx, MmArc};
use mm2_err_handle::prelude::*;
use mm2_p2p::{decode_signed, encode_and_sign, pub_sub_topic, TopicPrefix};
use parking_lot::Mutex as PaMutex;
use primitives::hash::{H160, H264};
use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
use secp256k1::{PublicKey, SecretKey, Signature};
use serde::Serialize;
use serde_json::{self as json, Value as Json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};
use swap_v2_common::ActiveSwapV2Info;
use swap_v2_pb::*;
use uuid::Uuid;

#[cfg(feature = "custom-swap-locktime")]
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "lp_swap/check_balance.rs"] mod check_balance;
#[path = "lp_swap/dex_fee.rs"] mod dex_fee;
#[allow(unused_imports)]
pub use dex_fee::{compute_dex_fee, compute_dex_fee_with_taker_pubkey, dex_fee_amount, dex_fee_amount_from_taker_coin};
pub(crate) use dex_fee::{compute_dex_fee_with_taker_pubkey_from_coin, dex_fee_rate, dex_fee_threshold};
#[path = "lp_swap/maker_swap.rs"] mod maker_swap;
#[path = "lp_swap/maker_swap_v2.rs"] pub mod maker_swap_v2;
#[path = "lp_swap/max_maker_vol_rpc.rs"] mod max_maker_vol_rpc;
#[path = "lp_swap/my_swaps_storage.rs"] mod my_swaps_storage;
#[path = "lp_swap/pubkey_banning.rs"] mod pubkey_banning;
#[path = "lp_swap/recreate_swap_data.rs"] mod recreate_swap_data;
#[path = "lp_swap/saved_swap.rs"] mod saved_swap;
#[path = "lp_swap/swap_events.rs"] pub(crate) mod swap_events;
#[path = "lp_swap/swap_lock.rs"] mod swap_lock;
#[path = "lp_swap/mm2_swap_v2.pb.rs"]
#[rustfmt::skip]
mod swap_v2_pb;
#[path = "lp_swap/nft_maker_swap_v2.rs"]
pub mod nft_maker_swap_v2;
#[path = "lp_swap/swap_v2_common.rs"] pub mod swap_v2_common;
#[path = "lp_swap/swap_v2_rpcs.rs"] pub(crate) mod swap_v2_rpcs;
#[path = "lp_swap/swap_versioning.rs"] pub mod swap_versioning;
#[path = "lp_swap/swap_watcher.rs"] pub mod swap_watcher;
#[path = "lp_swap/taker_swap.rs"] mod taker_swap;
#[path = "lp_swap/taker_swap_v2.rs"] pub mod taker_swap_v2;
#[path = "lp_swap/trade_preimage.rs"] mod trade_preimage;

#[cfg(target_arch = "wasm32")]
#[path = "lp_swap/swap_wasm_db.rs"]
mod swap_wasm_db;

#[allow(unused_imports)]
pub use check_balance::{check_other_coin_balance_for_swap, CheckBalanceError};

#[path = "lp_swap/swap_msg.rs"] mod swap_msg;
pub use swap_msg::*;

#[path = "lp_swap/swap_rpc.rs"] mod swap_rpc;
use keys::{KeyPair, SECP_SIGN, SECP_VERIFY};
pub use maker_swap::{calc_max_maker_vol, check_balance_for_maker_swap, maker_swap_trade_preimage, run_maker_swap,
                     MakerSavedEvent, MakerSavedSwap, MakerSwap, MakerSwapEvent, MakerSwapStatusChanged,
                     MakerTradePreimage, RunMakerSwapInput};
pub use max_maker_vol_rpc::max_maker_vol;
use my_swaps_storage::{MySwapsOps, MySwapsStorage};
use pubkey_banning::BannedPubkey;
pub use pubkey_banning::{ban_pubkey_rpc, is_pubkey_banned, list_banned_pubkeys_rpc, unban_pubkeys_rpc};
pub use recreate_swap_data::recreate_swap_data;
#[allow(unused_imports)]
pub use saved_swap::{SavedSwap, SavedSwapError, SavedSwapIo, SavedSwapResult};
use std::num::NonZeroUsize;
pub use swap_rpc::*;
#[allow(unused_imports)]
pub use swap_watcher::{process_watcher_msg, watcher_topic, SwapWatcherMsg, TakerSwapWatcherData, WATCHER_PREFIX};
#[allow(unused_imports)]
pub use taker_swap::{calc_max_taker_vol, check_balance_for_taker_swap, max_taker_vol, max_taker_vol_from_available,
                     max_taker_vol_v2, min_trading_vol_v2, run_taker_swap, taker_swap_trade_preimage,
                     RunTakerSwapInput, TakerSavedSwap, TakerSwap, TakerSwapEvent, TakerSwapPreparedParams,
                     TakerTradePreimage};
pub use trade_preimage::trade_preimage_rpc;

pub const SWAP_PREFIX: TopicPrefix = "swap";

pub const TX_HELPER_PREFIX: TopicPrefix = "txhlp";

/// V2 swap P2P topic prefix.
pub const SWAP_V2_PREFIX: TopicPrefix = "swapv2";

/// Swap type discriminant for legacy V1 swaps in the DB.
pub const LEGACY_SWAP_TYPE: u8 = 0;
/// Swap type discriminant for maker V2 swaps in the DB.
pub const MAKER_SWAP_V2_TYPE: u8 = 1;
/// Swap type discriminant for taker V2 swaps in the DB.
pub const TAKER_SWAP_V2_TYPE: u8 = 2;

/// Simple map for watcher deduplication. Key: taker_fee_hash, Value: expiry timestamp (seconds).
pub type WatcherEntryMap = HashMap<Vec<u8>, u64>;

cfg_wasm32! {
    use mm2_db::indexed_db::{ConstructibleDb, DbLocked};
    use swap_wasm_db::{InitDbResult, SwapDb};

    pub type SwapDbLocked<'a> = DbLocked<'a, SwapDb>;
}

/// Interval for broadcasting negotiation messages (seconds).
pub const NEGOTIATE_SEND_INTERVAL: f64 = 30.0;

/// Interval for broadcasting transaction info messages (seconds).
pub const TX_INFO_SEND_INTERVAL: f64 = 600.0;

async fn recv_swap_msg<T>(
    ctx: MmArc,
    mut getter: impl FnMut(&mut SwapMsgStore) -> Option<T>,
    uuid: &Uuid,
    timeout: u64,
) -> Result<T, String> {
    let started = now_ms() / 1000;
    let timeout = BASIC_COMM_TIMEOUT + timeout;
    let wait_until = started + timeout;
    loop {
        Timer::sleep(1.).await;
        let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        let mut msgs = swap_ctx.swap_msgs.lock().unwrap();
        if let Some(msg_store) = msgs.get_mut(uuid) {
            if let Some(msg) = getter(msg_store) {
                return Ok(msg);
            }
        }
        let now = now_ms() / 1000;
        if now > wait_until {
            return ERR!("Timeout ({} > {})", now - started, timeout);
        }
    }
}

/// Includes the grace time we add to the "normal" timeouts
/// in order to give different and/or heavy communication channels a chance.
const BASIC_COMM_TIMEOUT: u64 = 90;

#[cfg(not(feature = "custom-swap-locktime"))]
/// Default atomic swap payment locktime, in seconds.
/// Maker sends payment with LOCKTIME * 2
/// Taker sends payment with LOCKTIME
const PAYMENT_LOCKTIME: u64 = 3600 * 2 + 300 * 2;

#[cfg(feature = "custom-swap-locktime")]
/// Default atomic swap payment locktime, in seconds.
/// Maker sends payment with LOCKTIME * 2
/// Taker sends payment with LOCKTIME
pub(crate) static PAYMENT_LOCKTIME: AtomicU64 = AtomicU64::new(super::CUSTOM_PAYMENT_LOCKTIME_DEFAULT);

#[inline]
/// Returns `PAYMENT_LOCKTIME`
pub fn get_payment_locktime() -> u64 {
    #[cfg(not(feature = "custom-swap-locktime"))]
    return PAYMENT_LOCKTIME;
    #[cfg(feature = "custom-swap-locktime")]
    PAYMENT_LOCKTIME.load(Ordering::Relaxed)
}

const _SWAP_DEFAULT_NUM_CONFIRMS: u32 = 1;
const _SWAP_DEFAULT_MAX_CONFIRMS: u32 = 6;
/// MM2 checks that swap payment is confirmed every WAIT_CONFIRM_INTERVAL seconds
const WAIT_CONFIRM_INTERVAL: u64 = 15;

#[derive(Debug, PartialEq, Serialize)]
pub enum RecoveredSwapAction {
    RefundedMyPayment,
    SpentOtherPayment,
}

#[derive(Debug, PartialEq)]
pub struct RecoveredSwap {
    action: RecoveredSwapAction,
    coin: String,
    transaction: TransactionEnum,
}

/// Represents the amount of a coin locked by ongoing swap
#[derive(Debug)]
pub struct LockedAmount {
    coin: String,
    amount: MmNumber,
    trade_fee: Option<TradeFee>,
}

pub trait AtomicSwap: Send + Sync {
    fn locked_amount(&self) -> Vec<LockedAmount>;

    fn uuid(&self) -> &Uuid;

    fn maker_coin(&self) -> &str;

    fn taker_coin(&self) -> &str;
}

#[derive(Serialize)]
#[serde(tag = "type", content = "event")]
pub enum SwapEvent {
    Maker(MakerSwapEvent),
    Taker(TakerSwapEvent),
}

impl From<MakerSwapEvent> for SwapEvent {
    fn from(maker_event: MakerSwapEvent) -> Self { SwapEvent::Maker(maker_event) }
}

impl From<TakerSwapEvent> for SwapEvent {
    fn from(taker_event: TakerSwapEvent) -> Self { SwapEvent::Taker(taker_event) }
}

/// V2 swap locked amount information, keyed by coin ticker in SwapsContext.
#[derive(Debug)]
struct LockedAmountV2Info {
    swap_uuid: Uuid,
    locked_amount: LockedAmount,
}

/// Storage for P2P messages, which are exchanged during SwapV2 protocol execution.
#[derive(Debug)]
pub struct SwapV2MsgStore {
    maker_negotiation: Option<MakerNegotiation>,
    taker_negotiation: Option<TakerNegotiation>,
    maker_negotiated: Option<MakerNegotiated>,
    taker_funding: Option<TakerFundingInfo>,
    maker_payment: Option<MakerPaymentInfo>,
    taker_payment: Option<TakerPaymentInfo>,
    taker_payment_spend_preimage: Option<TakerPaymentSpendPreimage>,
    accept_only_from: PublicKey,
}

impl SwapV2MsgStore {
    pub fn new(accept_only_from: PublicKey) -> Self {
        SwapV2MsgStore {
            maker_negotiation: None,
            taker_negotiation: None,
            maker_negotiated: None,
            taker_funding: None,
            maker_payment: None,
            taker_payment: None,
            taker_payment_spend_preimage: None,
            accept_only_from,
        }
    }
}

struct SwapsContext {
    running_swaps: Mutex<Vec<Weak<dyn AtomicSwap>>>,
    banned_pubkeys: Mutex<HashMap<H256Json, BannedPubkey>>,
    /// The cloneable receiver of multi-consumer async channel awaiting for shutdown_tx.send() to be
    /// invoked to stop all running swaps.
    /// MM2 is used as static lib on some platforms e.g. iOS so it doesn't run as separate process.
    /// So when stop was invoked the swaps could stay running on shared executors causing
    /// Very unpleasant consequences
    shutdown_rx: async_std_sync::Receiver<()>,
    swap_msgs: Mutex<HashMap<Uuid, SwapMsgStore>>,
    /// Per-swap message stores for V2 protocol messages. Keyed by swap UUID.
    swap_v2_msgs: Mutex<HashMap<Uuid, SwapV2MsgStore>>,
    /// Active V2 swaps currently running (for status queries).
    active_swaps_v2: Mutex<Vec<ActiveSwapV2Info>>,
    /// V2 swap locked amounts, keyed by coin ticker.
    locked_amounts_v2: Mutex<HashMap<String, Vec<LockedAmountV2Info>>>,
    /// Deduplication map for taker swap watchers. Key = taker_fee_hash, value = expiry timestamp.
    /// Prevents multiple watchers from running for the same swap simultaneously.
    pub taker_swap_watchers: PaMutex<WatcherEntryMap>,
    #[cfg(target_arch = "wasm32")]
    swap_db: ConstructibleDb<SwapDb>,
}

impl SwapsContext {
    /// Obtains a reference to this crate context, creating it if necessary.
    fn from_ctx(ctx: &MmArc) -> Result<Arc<SwapsContext>, String> {
        Ok(try_s!(from_ctx(&ctx.swaps_ctx, move || {
            let (shutdown_tx, shutdown_rx) = async_std_sync::channel(1);
            let mut shutdown_tx = Some(shutdown_tx);
            ctx.on_stop(Box::new(move || {
                if let Some(shutdown_tx) = shutdown_tx.take() {
                    info!("on_stop] firing shutdown_tx!");
                    spawn(async move {
                        shutdown_tx.send(()).await;
                    });
                    Ok(())
                } else {
                    ERR!("on_stop callback called twice!")
                }
            }));

            Ok(SwapsContext {
                running_swaps: Mutex::new(vec![]),
                banned_pubkeys: Mutex::new(HashMap::new()),
                shutdown_rx,
                swap_msgs: Mutex::new(HashMap::new()),
                swap_v2_msgs: Mutex::new(HashMap::new()),
                active_swaps_v2: Mutex::new(Vec::new()),
                locked_amounts_v2: Mutex::new(HashMap::new()),
                taker_swap_watchers: PaMutex::new(HashMap::new()),
                #[cfg(target_arch = "wasm32")]
                swap_db: ConstructibleDb::new(ctx),
            })
        })))
    }

    pub fn init_msg_store(&self, uuid: Uuid, accept_only_from: bits256) {
        let store = SwapMsgStore::new(accept_only_from);
        self.swap_msgs.lock().unwrap().insert(uuid, store);
    }

    /// Initialise a V2 message store for the given swap UUID.
    /// Messages from senders other than `accept_only_from` are silently dropped.
    pub fn init_v2_msg_store(&self, uuid: Uuid, accept_only_from: PublicKey) {
        let store = SwapV2MsgStore::new(accept_only_from);
        self.swap_v2_msgs.lock().unwrap().insert(uuid, store);
    }

    /// Remove the V2 message store for a finished swap.
    pub fn remove_v2_msg_store(&self, uuid: &Uuid) { self.swap_v2_msgs.lock().unwrap().remove(uuid); }

    /// Register an active V2 swap for RPC queries.
    pub fn add_active_swap_v2(&self, info: ActiveSwapV2Info) { self.active_swaps_v2.lock().unwrap().push(info); }

    /// Remove an active V2 swap by UUID.
    pub fn remove_active_swap_v2(&self, uuid: &Uuid) {
        self.active_swaps_v2.lock().unwrap().retain(|s| &s.uuid != uuid);
    }

    /// Return a snapshot of currently active V2 swaps.
    pub fn active_swaps_v2_snapshot(&self) -> Vec<ActiveSwapV2Info> { self.active_swaps_v2.lock().unwrap().clone() }

    #[cfg(target_arch = "wasm32")]
    pub async fn swap_db(&self) -> InitDbResult<SwapDbLocked<'_>> { Ok(self.swap_db.get_or_initialize().await?) }
}

/// Get total amount of selected coin locked by all currently ongoing swaps
pub fn get_locked_amount(ctx: &MmArc, coin: &str) -> MmNumber {
    let swap_ctx = SwapsContext::from_ctx(ctx).unwrap();

    // V1 locked amounts (from running_swaps)
    let swap_lock = swap_ctx.running_swaps.lock().unwrap();
    let v1_total = swap_lock
        .iter()
        .filter_map(|swap| swap.upgrade())
        .flat_map(|swap| swap.locked_amount())
        .fold(MmNumber::from(0), |mut total_amount, locked| {
            if locked.coin == coin {
                total_amount += locked.amount;
            }
            if let Some(trade_fee) = locked.trade_fee {
                if trade_fee.coin == coin && !trade_fee.paid_from_trading_vol {
                    total_amount += trade_fee.amount;
                }
            }
            total_amount
        });
    drop(swap_lock);

    // V2 locked amounts
    let locked_v2 = swap_ctx.locked_amounts_v2.lock().unwrap();
    let v2_total = locked_v2
        .get(coin)
        .map(|entries| {
            entries.iter().fold(MmNumber::from(0), |mut total, info| {
                total += info.locked_amount.amount.clone();
                if let Some(ref fee) = info.locked_amount.trade_fee {
                    if fee.coin == coin && !fee.paid_from_trading_vol {
                        total += fee.amount.clone();
                    }
                }
                total
            })
        })
        .unwrap_or_else(|| MmNumber::from(0));

    v1_total + v2_total
}

// ────────────────────────────────────────────────────────────────────────────
// get_locked_amount RPC
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GetLockedAmountReq {
    coin: String,
}

#[derive(Serialize)]
pub struct GetLockedAmountResp {
    coin: String,
    locked_amount: MmNumberMultiRepr,
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetLockedAmountRpcError {
    #[display(fmt = "No such coin: {coin}")]
    NoSuchCoin { coin: String },
}

impl HttpStatusCode for GetLockedAmountRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetLockedAmountRpcError::NoSuchCoin { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for GetLockedAmountRpcError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => GetLockedAmountRpcError::NoSuchCoin { coin },
        }
    }
}

pub async fn get_locked_amount_rpc(
    ctx: MmArc,
    req: GetLockedAmountReq,
) -> Result<GetLockedAmountResp, MmError<GetLockedAmountRpcError>> {
    lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let locked_amount = get_locked_amount(&ctx, &req.coin);

    Ok(GetLockedAmountResp {
        coin: req.coin,
        locked_amount: locked_amount.into(),
    })
}

/// Get number of currently running swaps
pub fn running_swaps_num(ctx: &MmArc) -> u64 {
    let swap_ctx = SwapsContext::from_ctx(ctx).unwrap();
    let swaps = swap_ctx.running_swaps.lock().unwrap();
    swaps.iter().fold(0, |total, swap| match swap.upgrade() {
        Some(_) => total + 1,
        None => total,
    })
}

/// Get total amount of selected coin locked by all currently ongoing swaps except the one with selected uuid
fn get_locked_amount_by_other_swaps(ctx: &MmArc, except_uuid: &Uuid, coin: &str) -> MmNumber {
    let swap_ctx = SwapsContext::from_ctx(ctx).unwrap();
    let swap_lock = swap_ctx.running_swaps.lock().unwrap();

    swap_lock
        .iter()
        .filter_map(|swap| swap.upgrade())
        .filter(|swap| swap.uuid() != except_uuid)
        .flat_map(|swap| swap.locked_amount())
        .fold(MmNumber::from(0), |mut total_amount, locked| {
            if locked.coin == coin {
                total_amount += locked.amount;
            }
            if let Some(trade_fee) = locked.trade_fee {
                if trade_fee.coin == coin && !trade_fee.paid_from_trading_vol {
                    total_amount += trade_fee.amount;
                }
            }
            total_amount
        })
}

pub fn active_swaps_using_coin(ctx: &MmArc, coin: &str) -> Result<Vec<Uuid>, String> {
    let swap_ctx = try_s!(SwapsContext::from_ctx(ctx));
    let swaps = try_s!(swap_ctx.running_swaps.lock());
    let mut uuids = vec![];
    for swap in swaps.iter() {
        if let Some(swap) = swap.upgrade() {
            if swap.maker_coin() == coin || swap.taker_coin() == coin {
                uuids.push(*swap.uuid())
            }
        }
    }
    drop(swaps);

    for swap in swap_ctx.active_swaps_v2_snapshot() {
        if swap.maker_coin == coin || swap.taker_coin == coin {
            uuids.push(swap.uuid);
        }
    }
    Ok(uuids)
}

pub fn active_swaps(ctx: &MmArc) -> Result<Vec<(Uuid, u8)>, String> {
    let swap_ctx = try_s!(SwapsContext::from_ctx(ctx));
    let swaps = try_s!(swap_ctx.running_swaps.lock());
    let mut uuids: Vec<(Uuid, u8)> = swaps
        .iter()
        .filter_map(|swap| swap.upgrade())
        .map(|swap| (*swap.uuid(), LEGACY_SWAP_TYPE))
        .collect();
    drop(swaps);

    let v2_swaps = swap_ctx.active_swaps_v2_snapshot();
    uuids.extend(v2_swaps.iter().map(|info| (info.uuid, info.swap_type as u8)));
    Ok(uuids)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct SwapConfirmationsSettings {
    pub maker_coin_confs: u64,
    pub maker_coin_nota: bool,
    pub taker_coin_confs: u64,
    pub taker_coin_nota: bool,
}

impl SwapConfirmationsSettings {
    pub fn requires_notarization(&self) -> bool { self.maker_coin_nota || self.taker_coin_nota }
}

fn coin_with_4x_locktime(ticker: &str) -> bool { matches!(ticker, "BCH" | "BTG" | "SBTC") }

#[derive(Debug)]
pub enum AtomicLocktimeVersion {
    V1,
    V2 {
        my_conf_settings: SwapConfirmationsSettings,
        other_conf_settings: SwapConfirmationsSettings,
    },
}

pub fn lp_atomic_locktime_v1(maker_coin: &str, taker_coin: &str) -> u64 {
    if maker_coin == "BTC" || taker_coin == "BTC" {
        get_payment_locktime() * 10
    } else if coin_with_4x_locktime(maker_coin) || coin_with_4x_locktime(taker_coin) {
        get_payment_locktime() * 4
    } else {
        get_payment_locktime()
    }
}

pub fn lp_atomic_locktime_v2(
    maker_coin: &str,
    taker_coin: &str,
    my_conf_settings: &SwapConfirmationsSettings,
    other_conf_settings: &SwapConfirmationsSettings,
) -> u64 {
    if maker_coin == "BTC"
        || taker_coin == "BTC"
        || coin_with_4x_locktime(maker_coin)
        || coin_with_4x_locktime(taker_coin)
        || my_conf_settings.requires_notarization()
        || other_conf_settings.requires_notarization()
    {
        get_payment_locktime() * 4
    } else {
        get_payment_locktime()
    }
}

/// Some coins are "slow" (block time is high - e.g. BTC average block time is ~10 minutes).
/// https://bitinfocharts.com/comparison/bitcoin-confirmationtime.html
/// We need to increase payment locktime accordingly when at least 1 side of swap uses "slow" coin.
pub fn lp_atomic_locktime(maker_coin: &str, taker_coin: &str, version: AtomicLocktimeVersion) -> u64 {
    match version {
        AtomicLocktimeVersion::V1 => lp_atomic_locktime_v1(maker_coin, taker_coin),
        AtomicLocktimeVersion::V2 {
            my_conf_settings,
            other_conf_settings,
        } => lp_atomic_locktime_v2(maker_coin, taker_coin, &my_conf_settings, &other_conf_settings),
    }
}

#[derive(Clone, Debug, Eq, Deserialize, PartialEq, Serialize)]
pub struct NegotiationDataV1 {
    started_at: u64,
    payment_locktime: u64,
    secret_hash: [u8; 20],
    persistent_pubkey: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Deserialize, PartialEq, Serialize)]
pub struct NegotiationDataV2 {
    started_at: u64,
    payment_locktime: u64,
    secret_hash: Vec<u8>,
    persistent_pubkey: Vec<u8>,
    maker_coin_swap_contract: Vec<u8>,
    taker_coin_swap_contract: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Deserialize, PartialEq, Serialize)]
pub struct NegotiationDataV3 {
    started_at: u64,
    payment_locktime: u64,
    secret_hash: Vec<u8>,
    maker_coin_swap_contract: Vec<u8>,
    taker_coin_swap_contract: Vec<u8>,
    maker_coin_htlc_pub: Vec<u8>,
    taker_coin_htlc_pub: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum NegotiationDataMsg {
    V1(NegotiationDataV1),
    V2(NegotiationDataV2),
    V3(NegotiationDataV3),
}

impl NegotiationDataMsg {
    pub fn started_at(&self) -> u64 {
        match self {
            NegotiationDataMsg::V1(v1) => v1.started_at,
            NegotiationDataMsg::V2(v2) => v2.started_at,
            NegotiationDataMsg::V3(v3) => v3.started_at,
        }
    }

    pub fn payment_locktime(&self) -> u64 {
        match self {
            NegotiationDataMsg::V1(v1) => v1.payment_locktime,
            NegotiationDataMsg::V2(v2) => v2.payment_locktime,
            NegotiationDataMsg::V3(v3) => v3.payment_locktime,
        }
    }

    pub fn secret_hash(&self) -> &[u8] {
        match self {
            NegotiationDataMsg::V1(v1) => &v1.secret_hash,
            NegotiationDataMsg::V2(v2) => &v2.secret_hash,
            NegotiationDataMsg::V3(v3) => &v3.secret_hash,
        }
    }

    pub fn maker_coin_htlc_pub(&self) -> &[u8] {
        match self {
            NegotiationDataMsg::V1(v1) => &v1.persistent_pubkey,
            NegotiationDataMsg::V2(v2) => &v2.persistent_pubkey,
            NegotiationDataMsg::V3(v3) => &v3.maker_coin_htlc_pub,
        }
    }

    pub fn taker_coin_htlc_pub(&self) -> &[u8] {
        match self {
            NegotiationDataMsg::V1(v1) => &v1.persistent_pubkey,
            NegotiationDataMsg::V2(v2) => &v2.persistent_pubkey,
            NegotiationDataMsg::V3(v3) => &v3.taker_coin_htlc_pub,
        }
    }

    pub fn maker_coin_swap_contract(&self) -> Option<&[u8]> {
        match self {
            NegotiationDataMsg::V1(_) => None,
            NegotiationDataMsg::V2(v2) => Some(&v2.maker_coin_swap_contract),
            NegotiationDataMsg::V3(v3) => Some(&v3.maker_coin_swap_contract),
        }
    }

    pub fn taker_coin_swap_contract(&self) -> Option<&[u8]> {
        match self {
            NegotiationDataMsg::V1(_) => None,
            NegotiationDataMsg::V2(v2) => Some(&v2.taker_coin_swap_contract),
            NegotiationDataMsg::V3(v3) => Some(&v3.taker_coin_swap_contract),
        }
    }
}

/// Data to be exchanged and validated on swap start, the replacement of LP_pubkeys_data, LP_choosei_data, etc.
#[derive(Debug, Default, Deserializable, Eq, PartialEq, Serializable)]
struct SwapNegotiationData {
    started_at: u64,
    payment_locktime: u64,
    secret_hash: H160,
    persistent_pubkey: H264,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionIdentifier {
    /// Raw bytes of signed transaction in hexadecimal string, this should be sent as is to send_raw_transaction RPC to broadcast the transaction
    tx_hex: BytesJson,
    /// Transaction hash in hexadecimal format
    tx_hash: BytesJson,
}
