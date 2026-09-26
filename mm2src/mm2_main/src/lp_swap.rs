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
use coins::{lp_coinfind, lp_coinfind_or_err, CoinFindError, MmCoinEnum, TradeFee, TransactionEnum,
            SWAP_HTLC_PUBKEY_LEN};
use common::log::{debug, warn};
use common::{bits256, calc_total_pages,
             executor::{spawn, Timer},
             log::{error, info},
             mm_number::{BigDecimal, MmNumber, MmNumberMultiRepr},
             now_ms, HttpStatusCode, PagingOptions};
use crypto::secret_hash_algo::SecretHashAlgo;
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
use secp256k1::ecdsa::Signature;
use secp256k1::{PublicKey, SecretKey};
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

/// What a V2 ledger entry reserves. One swap can hold two entries for the same
/// coin — an ERC20 trade against its own platform coin puts the volume entry and
/// the spend headroom in the same bucket — so removal has to name the kind as
/// well as the swap.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LockedAmountV2Kind {
    /// The role's own outgoing volume and the fee to send it (CRD ch.52 R58).
    Volume,
    /// Balance kept free to pay for spending the incoming payment (ch.52 R64).
    SpendHeadroom,
}

/// V2 swap locked amount information, keyed by coin ticker in SwapsContext.
#[derive(Debug)]
struct LockedAmountV2Info {
    swap_uuid: Uuid,
    kind: LockedAmountV2Kind,
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

/// How much of a trade fee that is not itself skimmed from a payment the payer
/// must keep free in its own balance.
///
/// Both protocols owe the same answer here (CRD ch.51 R53/R54/R55, ch.52
/// R58/R62/R64), and it is not simply the fee amount. A coin that reports
/// `paid_from_trading_vol` takes the fee out of the payment it concerns — a
/// UTXO spend pays the miner from the HTLC output itself — so nothing has to
/// stay free for it. A coin that does not report it burns the fee from the
/// account balance, and that balance is the one named by `TradeFee::coin`,
/// which for an ERC20 payment is the platform coin rather than the token.
/// This is the same predicate V1's `get_locked_amount` applies to a
/// registry entry's nested `trade_fee` (chapter 51 R53); here it is applied
/// once, at estimation time, rather than re-evaluated on every query.
///
/// Returns the reservable amount; the caller files it under `fee.coin`.
fn fee_reservable_amount(fee: &TradeFee) -> MmNumber {
    if fee.paid_from_trading_vol {
        MmNumber::from(0)
    } else {
        fee.amount.clone()
    }
}

/// Record one component of a running V2 swap's ledger entry — a headroom
/// reservation (ch.52 R64) or a volume/send-fee reservation (R58) — under the
/// named coin's bucket. Idempotent per (swap, kind, coin): re-applying the same
/// event does not double-reserve, which matters because a resumed process
/// re-derives its reservations by reading its own persisted log back (R63).
///
/// The whole reservation goes in `LockedAmount::amount` rather than in a
/// synthesised nested `trade_fee`: the caller has already applied
/// `fee_reservable_amount`'s paid-from-trading-volume rule, so nesting it
/// would apply that rule a second time and, worse, only when the bucket
/// happens to equal the descriptor's own coin — which is exactly the
/// coin-conflation bug this per-kind, per-coin bucketing avoids.
fn reserve_v2_amount(ctx: &MmArc, uuid: Uuid, kind: LockedAmountV2Kind, coin: &str, amount: MmNumber) {
    let swap_ctx = match SwapsContext::from_ctx(ctx) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to reserve amount for swap {}: {}", uuid, e);
            return;
        },
    };
    let mut locked = swap_ctx.locked_amounts_v2.lock().unwrap();
    let entries = locked.entry(coin.to_owned()).or_default();
    if entries.iter().any(|info| info.swap_uuid == uuid && info.kind == kind) {
        return;
    }
    entries.push(LockedAmountV2Info {
        swap_uuid: uuid,
        kind,
        locked_amount: LockedAmount {
            coin: coin.to_owned(),
            amount,
            trade_fee: None,
        },
    });
}

/// Release every entry of `kind` that `uuid` holds in `coin`'s bucket. A no-op
/// if none exist, so callers may name a bucket unconditionally — including one
/// a swap never actually reserved in, such as a trading coin whose platform
/// ticker is itself, where the volume and send-fee entries share a bucket and
/// a caller releasing both by coin need not know they coincided.
fn release_v2_amount(ctx: &MmArc, uuid: &Uuid, kind: LockedAmountV2Kind, coin: &str) {
    let swap_ctx = match SwapsContext::from_ctx(ctx) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to release amount for swap {}: {}", uuid, e);
            return;
        },
    };
    let mut locked = swap_ctx.locked_amounts_v2.lock().unwrap();
    if let Some(entries) = locked.get_mut(coin) {
        entries.retain(|info| !(info.swap_uuid == *uuid && info.kind == kind));
    }
}

/// Record the headroom a running V2 swap needs to spend the payment it is owed
/// (ch.52 R64).
///
/// `fee_coin` is the coin that actually pays the fee — `platform_ticker()` of
/// the counterparty coin — which is also the ledger bucket the balance checks
/// read.
fn reserve_v2_spend_headroom(ctx: &MmArc, uuid: Uuid, fee_coin: &str, headroom: MmNumber) {
    reserve_v2_amount(ctx, uuid, LockedAmountV2Kind::SpendHeadroom, fee_coin, headroom);
}

/// Release the headroom entry of `uuid` once the incoming payment has been spent
/// and the fee is no longer owed (ch.52 R64).
fn release_v2_spend_headroom(ctx: &MmArc, uuid: &Uuid, fee_coin: &str) {
    release_v2_amount(ctx, uuid, LockedAmountV2Kind::SpendHeadroom, fee_coin);
}

/// Remove every ledger entry belonging to `uuid`, whatever coin it was filed
/// under (ch.52 R59).
///
/// The two coins of the swap are not enough to find them all: an ERC20 leg files
/// its spend headroom under the platform coin, which need be neither of them.
fn release_all_v2_locked_amounts(ctx: &MmArc, uuid: &Uuid) {
    let swap_ctx = match SwapsContext::from_ctx(ctx) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to release locked amounts for swap {}: {}", uuid, e);
            return;
        },
    };
    let mut locked = swap_ctx.locked_amounts_v2.lock().unwrap();
    for entries in locked.values_mut() {
        entries.retain(|info| info.swap_uuid != *uuid);
    }
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

    let v1_total = swap_lock
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
        });
    drop(swap_lock);

    // V2 swaps reserve through their own ledger rather than the running-swap
    // registry above, so reading only the registry would report a node with
    // live V2 swaps as having balance it has already committed — admitting a
    // new swap against it (CRD ch.52 R61). The aggregate `get_locked_amount`
    // already sums both; this self-excluding variant is what every swap-start
    // balance check calls, so it must agree.
    let locked_v2 = swap_ctx.locked_amounts_v2.lock().unwrap();
    let v2_total = locked_v2
        .get(coin)
        .map(|entries| {
            entries
                .iter()
                .filter(|info| &info.swap_uuid != except_uuid)
                .fold(MmNumber::from(0), |mut total, info| {
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

/// Width of a public-key field on the legacy swap wire (CRD ch.51 R62).
///
/// The same width the coin layer is held to by R63, seen from the message
/// layer, so the two are one constant rather than two that could drift.
pub const SWAP_WIRE_PUBKEY_LEN: usize = SWAP_HTLC_PUBKEY_LEN;

/// Width of the secret hash carried by the shape-1 negotiation payload
/// (CRD ch.51 R65). Shapes 2 and 3 carry a variable-length hash, so this is a
/// lower bound on what the fixed-width field can be narrowed to, not an
/// equality.
pub const SWAP_WIRE_SECRET_HASH_MIN_LEN: usize = 20;

/// Reject a counterparty-supplied field that cannot fill its fixed-width slot.
///
/// These fields arrive as variable-length byte sequences and are narrowed to
/// fixed-width types whose conversion indexes the slice without checking it. A
/// field shorter than the target therefore panics the swap task, and a longer
/// one is silently truncated — both reachable from a peer-supplied negotiation
/// message, so neither may be left to the conversion.
///
/// A chain whose native key is not a 33-byte secp256k1 point reaches the short
/// case honestly rather than maliciously; ch.51 R64 binds how such a key
/// occupies the field, and until a coin produces that form its negotiation must
/// fail as a length error (R62) rather than take the process down.
pub fn validate_wire_field_len(field: &[u8], expected: usize, what: &str) -> Result<(), String> {
    if field.len() < expected {
        return Err(format!("{what} is {} bytes, expected at least {expected}", field.len()));
    }
    Ok(())
}

/// Exact-width check for the public-key fields bound by ch.51 R62.
pub fn validate_wire_pubkey(field: &[u8], what: &str) -> Result<(), String> {
    if field.len() != SWAP_WIRE_PUBKEY_LEN {
        return Err(format!(
            "{what} must be exactly {SWAP_WIRE_PUBKEY_LEN} bytes, got {}",
            field.len()
        ));
    }
    Ok(())
}

/// Select the secret-hash algorithm for a legacy swap's `(maker_coin,
/// taker_coin)` pair (CRD ch.51 R71/R72).
///
/// The 20-byte `RIPEMD160(SHA-256(_))` default is correct for the ordinary
/// case, but a coin family whose own atomic-swap payment construction
/// commits to a native 32-byte hash -- Siacoin's spend-policy HTLC, Bitcoin
/// Lightning, or a Tendermint-family coin's IBC-HTLC -- has no way to accept
/// or be satisfied by a shorter external value. Whenever one of those
/// families sits on either side of the pair, both peers must independently
/// select the 32-byte `SHA-256(_)` alternate instead; the algorithm itself is
/// never carried on the wire, only its result, so this selection has to be a
/// pure function of the two coins' identities that every conforming peer
/// evaluates identically (R71).
pub fn select_secret_hash_algo(maker_coin: &MmCoinEnum, taker_coin: &MmCoinEnum) -> SecretHashAlgo {
    fn commits_to_a_native_32_byte_hash(coin: &MmCoinEnum) -> bool {
        match coin {
            MmCoinEnum::SiaCoin(_) | MmCoinEnum::TendermintCoin(_) | MmCoinEnum::TendermintToken(_) => true,
            #[cfg(not(target_arch = "wasm32"))]
            MmCoinEnum::LightningCoin(_) => true,
            _ => false,
        }
    }

    if commits_to_a_native_32_byte_hash(maker_coin) || commits_to_a_native_32_byte_hash(taker_coin) {
        SecretHashAlgo::SHA256
    } else {
        SecretHashAlgo::DHASH160
    }
}

/// Resolve the two per-coin public keys this node negotiates a legacy swap
/// with, by asking each coin (ch.51 R63).
///
/// A secp256k1 chain answers with the key it is handed — its own HTLC keypair
/// when [`SwapOps::get_htlc_key_pair`] gives one, otherwise this node's
/// persistent key — so nothing changes for it. A chain that signs with another
/// curve answers with its own key, in the 33-byte form R64 dictates. Neither
/// the negotiation message nor the swap machines become key-length-polymorphic
/// in the process: the result is exactly `SWAP_WIRE_PUBKEY_LEN` bytes either
/// way.
///
/// # Errors
///
/// Fails when either coin cannot produce a key of the bound width; the swap
/// then fails to start rather than negotiating a key it cannot sign with.
pub fn derive_htlc_pubkeys(
    ctx: &MmArc,
    maker_coin: &MmCoinEnum,
    maker_coin_htlc_key_pair: &Option<KeyPair>,
    taker_coin: &MmCoinEnum,
    taker_coin_htlc_key_pair: &Option<KeyPair>,
) -> Result<(H264, H264), String> {
    let node_pubkey = *ctx.secp256k1_key_pair().public();
    let maker_coin_htlc_pubkey = maker_coin
        .derive_htlc_pubkey(maker_coin_htlc_key_pair.as_ref().map_or(&node_pubkey, |k| k.public()))
        .map_err(|e| format!("!{}.derive_htlc_pubkey {}", maker_coin.ticker(), e))?;
    let taker_coin_htlc_pubkey = taker_coin
        .derive_htlc_pubkey(taker_coin_htlc_key_pair.as_ref().map_or(&node_pubkey, |k| k.public()))
        .map_err(|e| format!("!{}.derive_htlc_pubkey {}", taker_coin.ticker(), e))?;

    Ok((
        H264::from(&maker_coin_htlc_pubkey[..]),
        H264::from(&taker_coin_htlc_pubkey[..]),
    ))
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

#[cfg(test)]
mod ironwood_freeze_margin_tests {
    use super::*;

    /// A shielded coin that cannot yet build Ironwood-era transactions stops
    /// entering new swaps `coins::z_coin::IRONWOOD_SWAP_FREEZE_MARGIN_SECS` before
    /// its upgrade, so that a payment made in the last tradeable second is still
    /// spendable or refundable before activation.
    ///
    /// That margin has to be a constant in `coins`, because the locktime rules live
    /// here and this crate depends on `coins` rather than the other way round. This
    /// test is what stops the two drifting: if the locktime or its multipliers
    /// change, the margin must be raised to match.
    #[test]
    fn payment_locktime_covers_ironwood_freeze_margin() {
        // The longest lock the framework produces: the maker leg (`* 2`) under the
        // legacy slow-coin rule (`* 10`, `lp_atomic_locktime_v1`), which a peer
        // negotiating without confirmation settings can still reach.
        let longest_maker_payment_lock = get_payment_locktime() * 10 * 2;
        assert!(
            coins::z_coin::IRONWOOD_SWAP_FREEZE_MARGIN_SECS >= longest_maker_payment_lock,
            "Ironwood freeze margin {}s no longer covers the longest maker payment lock {}s \
             (payment locktime {}s); raise IRONWOOD_SWAP_FREEZE_MARGIN_SECS in coins::z_coin",
            coins::z_coin::IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            longest_maker_payment_lock,
            get_payment_locktime()
        );

        // And the refund grace the swap machines add on top of the lock.
        let longest_wait_refund_until = longest_maker_payment_lock + 3700;
        assert!(
            coins::z_coin::IRONWOOD_SWAP_FREEZE_MARGIN_SECS >= longest_wait_refund_until,
            "Ironwood freeze margin {}s does not cover the refund deadline {}s",
            coins::z_coin::IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            longest_wait_refund_until
        );
    }
}

#[cfg(test)]
mod wire_field_tests {
    use super::*;

    /// ch.51 R62: a key field is exactly 33 bytes, and any other width is a
    /// length error rather than something the narrowing conversion has to cope
    /// with. The conversion indexes the slice unchecked, so a short field would
    /// otherwise panic the swap task on a peer-supplied message.
    #[test]
    fn wire_pubkey_accepts_only_the_bound_width() {
        assert!(validate_wire_pubkey(&[0u8; SWAP_WIRE_PUBKEY_LEN], "k").is_ok());

        // A 32-byte ed25519 key is the honest short case: until it is padded to
        // the bound width (R64) it must be refused, not truncated or panicked on.
        assert!(validate_wire_pubkey(&[0u8; 32], "k").is_err());

        assert!(validate_wire_pubkey(&[], "k").is_err());
        assert!(validate_wire_pubkey(&[0u8; 34], "k").is_err());
    }

    /// R64's padding convention makes an ed25519 key acceptable: native bytes
    /// first, trailing zero. Guards the width, not the curve.
    #[test]
    fn ed25519_key_padded_to_the_bound_width_is_accepted() {
        let mut padded = [7u8; SWAP_WIRE_PUBKEY_LEN];
        padded[SWAP_WIRE_PUBKEY_LEN - 1] = 0;
        assert!(validate_wire_pubkey(&padded, "k").is_ok());
        assert_eq!(&padded[..32], &[7u8; 32], "native key occupies the leading bytes");
    }

    /// R65: the secret hash is fixed-width in shape 1 but variable in shapes 2
    /// and 3, so the guard is a lower bound — enough to keep the narrowing
    /// conversion in range without rejecting a legitimately wider hash.
    #[test]
    fn secret_hash_guard_is_a_lower_bound() {
        assert!(validate_wire_field_len(&[0u8; 20], SWAP_WIRE_SECRET_HASH_MIN_LEN, "h").is_ok());
        assert!(validate_wire_field_len(&[0u8; 32], SWAP_WIRE_SECRET_HASH_MIN_LEN, "h").is_ok());
        assert!(validate_wire_field_len(&[0u8; 19], SWAP_WIRE_SECRET_HASH_MIN_LEN, "h").is_err());
    }
}

#[cfg(test)]
mod persisted_event_vocabulary_tests {
    use super::maker_swap::{MAKER_ERROR_EVENTS, MAKER_SUCCESS_EVENTS};
    use super::taker_swap::{TAKER_ERROR_EVENTS, TAKER_SUCCESS_EVENTS};

    /// Pin the legacy persisted event vocabularies.
    ///
    /// The saved-swap JSON is a public compatibility surface: deployed peers
    /// and GUIs read these lists, and CRD ch.44 R44.8A.3/.4 binds the names
    /// each side's parser accepts. Nothing else in this tree checks them
    /// against a fixed expectation — the recreate-swap fixtures derive them
    /// from these same constants, so they agree by construction, and the
    /// integration tests that use them need a live network.
    ///
    /// So this test exists to make a change deliberate rather than silent:
    /// adding, removing, renaming or reordering an event must be accompanied
    /// by updating this list and the chapter that binds it.
    #[test]
    fn legacy_event_vocabulary_is_pinned() {
        assert_eq!(
            TAKER_SUCCESS_EVENTS.as_slice(),
            [
            "Started",
            "Negotiated",
            "TakerFeeSent",
            "TakerPaymentInstructionsReceived",
            "MakerPaymentReceived",
            "MakerPaymentWaitConfirmStarted",
            "MakerPaymentValidatedAndConfirmed",
            "TakerPaymentSent",
            "WatcherMessageSent",
            "TakerPaymentSpent",
            "MakerPaymentSpent",
            "MakerPaymentSpendConfirmed",
            "MakerPaymentSpentByWatcher",
            "TakerPaymentRefundStarted",
            "TakerPaymentRefundFinished",
            "TakerPaymentRefundedByWatcher",
            "Finished",
            ],
            "TAKER_SUCCESS_EVENTS is a persisted compatibility surface (CRD ch.44 R44.8A); changing it must be deliberate"
        );
        assert_eq!(
            TAKER_ERROR_EVENTS.as_slice(),
            [
            "StartFailed",
            "NegotiateFailed",
            "TakerFeeSendFailed",
            "MakerPaymentValidateFailed",
            "MakerPaymentWaitConfirmFailed",
            "TakerPaymentTransactionFailed",
            "TakerPaymentWaitConfirmFailed",
            "TakerPaymentDataSendFailed",
            "TakerPaymentWaitForSpendFailed",
            "MakerPaymentSpendFailed",
            "MakerPaymentSpendConfirmFailed",
            "TakerPaymentWaitRefundStarted",
            "TakerPaymentRefunded",
            "TakerPaymentRefundFailed",
            ],
            "TAKER_ERROR_EVENTS is a persisted compatibility surface (CRD ch.44 R44.8A); changing it must be deliberate"
        );
        assert_eq!(
            MAKER_SUCCESS_EVENTS.as_slice(),
            [
            "Started",
            "Negotiated",
            "MakerPaymentInstructionsReceived",
            "TakerFeeValidated",
            "MakerPaymentSent",
            "TakerPaymentReceived",
            "TakerPaymentWaitConfirmStarted",
            "TakerPaymentValidatedAndConfirmed",
            "TakerPaymentSpent",
            "TakerPaymentSpendConfirmStarted",
            "TakerPaymentSpendConfirmed",
            "MakerPaymentRefundStarted",
            "MakerPaymentRefundFinished",
            "Finished",
            ],
            "MAKER_SUCCESS_EVENTS is a persisted compatibility surface (CRD ch.44 R44.8A); changing it must be deliberate"
        );
        assert_eq!(
            MAKER_ERROR_EVENTS.as_slice(),
            [
            "StartFailed",
            "NegotiateFailed",
            "TakerFeeValidateFailed",
            "MakerPaymentTransactionFailed",
            "MakerPaymentDataSendFailed",
            "MakerPaymentWaitConfirmFailed",
            "TakerPaymentValidateFailed",
            "TakerPaymentWaitConfirmFailed",
            "TakerPaymentSpendFailed",
            "TakerPaymentSpendConfirmFailed",
            "MakerPaymentWaitRefundStarted",
            "MakerPaymentRefunded",
            "MakerPaymentRefundFailed",
            ],
            "MAKER_ERROR_EVENTS is a persisted compatibility surface (CRD ch.44 R44.8A); changing it must be deliberate"
        );
    }
}

#[cfg(test)]
mod v2_spend_headroom_tests {
    use super::*;
    use common::new_uuid;
    use mm2_core::mm_ctx::MmCtxBuilder;

    fn test_ctx() -> MmArc {
        MmCtxBuilder::default()
            .with_conf(json::json!({"netid": 8762}))
            .into_mm_arc()
    }

    /// The shape V1 files its counterparty-spend reservation in: zero volume,
    /// carrying the coin's own fee descriptor (ch.51 R54/R55, and see
    /// `TakerSwap::locked_amount`). Placed in the V2 ledger so that both shapes
    /// are scored by the same predicate — which ch.52 R62 requires the two totals
    /// to share anyway.
    fn v1_shaped_headroom_total(fee: &TradeFee) -> MmNumber {
        let ctx = test_ctx();
        let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        swap_ctx
            .locked_amounts_v2
            .lock()
            .unwrap()
            .entry(fee.coin.clone())
            .or_default()
            .push(LockedAmountV2Info {
                swap_uuid: new_uuid(),
                kind: LockedAmountV2Kind::Volume,
                locked_amount: LockedAmount {
                    coin: fee.coin.clone(),
                    amount: MmNumber::from(0),
                    trade_fee: Some(fee.clone()),
                },
            });
        get_locked_amount(&ctx, &fee.coin)
    }

    fn v2_headroom_total(fee: &TradeFee) -> MmNumber {
        let ctx = test_ctx();
        reserve_v2_spend_headroom(&ctx, new_uuid(), &fee.coin, fee_reservable_amount(fee));
        get_locked_amount(&ctx, &fee.coin)
    }

    /// A UTXO spend pays the miner out of the HTLC output it is claiming, so no
    /// balance has to stay free for it. An EVM spend burns gas from the account,
    /// so it does. These are the two coin families on the V2 surface, and they are
    /// the reason the answer cannot be "reserve the fee" or "reserve nothing".
    fn utxo_style_fee() -> TradeFee {
        TradeFee {
            coin: "MARTY".into(),
            amount: MmNumber::from("0.00001"),
            paid_from_trading_vol: true,
        }
    }

    fn evm_style_fee() -> TradeFee {
        TradeFee {
            coin: "ETH".into(),
            amount: MmNumber::from("0.0021"),
            paid_from_trading_vol: false,
        }
    }

    /// ch.52 R64 with ch.51 R54/R55: closing the V2 gap only helps if the two
    /// protocols then answer the same question the same way, since one node runs
    /// both and `max_taker_vol` sums them.
    #[test]
    fn v2_spend_headroom_agrees_with_the_v1_reservation() {
        for (label, fee) in [
            ("a fee paid out of the payment being claimed", utxo_style_fee()),
            ("a fee burned from our own balance", evm_style_fee()),
        ] {
            assert_eq!(
                v1_shaped_headroom_total(&fee),
                v2_headroom_total(&fee),
                "V1 and V2 must reserve the same amount for {label}"
            );
        }

        // And the answers are actually different from each other, so the
        // agreement above is not two zeroes agreeing by accident.
        assert_eq!(v2_headroom_total(&utxo_style_fee()), MmNumber::from(0));
        assert_eq!(v2_headroom_total(&evm_style_fee()), MmNumber::from("0.0021"));
    }

    /// Both directions, as `finished_swap_releases_its_reservation` does for the
    /// legacy registry: reserving must be visible, and releasing must undo it.
    /// A headroom that were never released would shrink `max_taker_vol` for the
    /// life of the process, the ledger being in-memory only (ch.52 R65).
    #[test]
    fn v2_spend_headroom_is_held_until_the_incoming_payment_is_spent() {
        let ctx = test_ctx();
        let uuid = new_uuid();
        let fee = evm_style_fee();

        assert_eq!(
            get_locked_amount(&ctx, &fee.coin),
            MmNumber::from(0),
            "nothing is reserved before the swap initialises"
        );

        reserve_v2_spend_headroom(&ctx, uuid, &fee.coin, fee_reservable_amount(&fee));
        assert_eq!(
            get_locked_amount(&ctx, &fee.coin),
            MmNumber::from("0.0021"),
            "a live swap must hold the fee it needs to claim what it is owed"
        );

        // Re-applying the same event must not double-reserve; a resume can apply
        // it a second time (ch.52 R63).
        reserve_v2_spend_headroom(&ctx, uuid, &fee.coin, fee_reservable_amount(&fee));
        assert_eq!(
            get_locked_amount(&ctx, &fee.coin),
            MmNumber::from("0.0021"),
            "reserving twice for one swap must not reserve twice"
        );

        release_v2_spend_headroom(&ctx, &uuid, &fee.coin);
        assert_eq!(
            get_locked_amount(&ctx, &fee.coin),
            MmNumber::from(0),
            "once the payment is spent the fee is no longer owed"
        );
    }

    /// ch.52 R61: the self-excluding total is what every swap-start balance check
    /// reads, so a headroom invisible to it would admit a second swap against the
    /// balance the first needs to collect — the failure R61 was written for,
    /// reached by a different route.
    #[test]
    fn v2_spend_headroom_is_visible_to_the_self_excluding_total() {
        let ctx = test_ctx();
        let holder = new_uuid();
        let other = new_uuid();
        let fee = evm_style_fee();

        reserve_v2_spend_headroom(&ctx, holder, &fee.coin, fee_reservable_amount(&fee));

        assert_eq!(
            get_locked_amount_by_other_swaps(&ctx, &other, &fee.coin),
            MmNumber::from("0.0021"),
            "another swap's balance check must see the headroom"
        );
        assert_eq!(
            get_locked_amount_by_other_swaps(&ctx, &holder, &fee.coin),
            MmNumber::from(0),
            "a swap must not block itself"
        );
    }

    /// An ERC20 leg files its headroom under the platform coin, which need be
    /// neither of the swap's two coins — and which may equally well be one of
    /// them, when the other leg is that same platform coin. Both cases have to
    /// work: the volume entry must not carry the headroom away with it, and
    /// termination must still find the headroom to release (ch.52 R59).
    #[test]
    fn erc20_headroom_shares_a_bucket_with_the_volume_entry_without_colliding() {
        let ctx = test_ctx();
        let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        let uuid = new_uuid();

        // Volume reserved in ETH, headroom for spending an ERC20 payment also
        // billed to ETH: one bucket, two entries, one swap.
        swap_ctx
            .locked_amounts_v2
            .lock()
            .unwrap()
            .entry("ETH".to_owned())
            .or_default()
            .push(LockedAmountV2Info {
                swap_uuid: uuid,
                kind: LockedAmountV2Kind::Volume,
                locked_amount: LockedAmount {
                    coin: "ETH".into(),
                    amount: MmNumber::from("1"),
                    trade_fee: None,
                },
            });
        reserve_v2_spend_headroom(&ctx, uuid, "ETH", MmNumber::from("0.0021"));
        assert_eq!(get_locked_amount(&ctx, "ETH"), MmNumber::from("1.0021"));

        // The payment goes out: the volume is committed, the headroom is not.
        if let Some(entries) = swap_ctx.locked_amounts_v2.lock().unwrap().get_mut("ETH") {
            entries.retain(|info| info.swap_uuid != uuid || info.kind != LockedAmountV2Kind::Volume);
        }
        assert_eq!(
            get_locked_amount(&ctx, "ETH"),
            MmNumber::from("0.0021"),
            "committing the volume must leave the headroom standing"
        );

        release_all_v2_locked_amounts(&ctx, &uuid);
        assert_eq!(
            get_locked_amount(&ctx, "ETH"),
            MmNumber::from(0),
            "termination must clear every bucket the swap wrote to"
        );
    }

    /// ch.52 R58/V5: this is the shape the volume entry used before this fix —
    /// one `LockedAmountV2Info`, filed under the trading coin's bucket, with the
    /// send fee nested inside as a `TradeFee` naming whatever coin the fee is
    /// actually billed to. `get_locked_amount` only ever checks a nested
    /// `trade_fee` against the bucket it is iterating, never against the fee's
    /// own coin — so a fee that disagrees with its bucket, which is exactly what
    /// an ERC20 leg's platform-coin gas fee does, was silently uncounted
    /// everywhere: not in the trading coin's total, and not in the fee's own
    /// coin's total either. Confirms the defect this fix closes actually existed
    /// in the shape the old code produced, independent of any code this commit
    /// changed.
    #[test]
    fn nested_trade_fee_naming_a_different_coin_than_its_bucket_was_invisible() {
        let ctx = test_ctx();
        let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        swap_ctx
            .locked_amounts_v2
            .lock()
            .unwrap()
            .entry("MYTOKEN".to_owned())
            .or_default()
            .push(LockedAmountV2Info {
                swap_uuid: new_uuid(),
                kind: LockedAmountV2Kind::Volume,
                locked_amount: LockedAmount {
                    coin: "MYTOKEN".into(),
                    amount: MmNumber::from("1"),
                    trade_fee: Some(TradeFee {
                        coin: "ETH".into(),
                        amount: MmNumber::from("0.01"),
                        paid_from_trading_vol: false,
                    }),
                },
            });

        assert_eq!(
            get_locked_amount(&ctx, "MYTOKEN"),
            MmNumber::from("1"),
            "the volume amount is still counted in its own bucket"
        );
        assert_eq!(
            get_locked_amount(&ctx, "ETH"),
            MmNumber::from(0),
            "but the nested fee, naming a coin other than the bucket it was filed under, was invisible everywhere — the real gas fee this represents was never reserved"
        );
    }

    /// The fix for the test above: the send fee is its own ledger entry, in its
    /// own coin's bucket, rather than nested inside the volume entry (ch.52 R58,
    /// closing V5). Mirrors `finished_swap_releases_its_reservation` and
    /// `erc20_headroom_shares_a_bucket_with_the_volume_entry_without_colliding`:
    /// both directions, plus idempotency and a shared-bucket case.
    #[test]
    fn send_fee_in_a_different_coin_than_the_trading_coin_is_reserved_and_released_in_its_own_bucket() {
        let ctx = test_ctx();
        let uuid = new_uuid();

        // A token traded against its own platform coin's gas: the volume lives
        // under the token, the send fee under the platform coin.
        reserve_v2_amount(&ctx, uuid, LockedAmountV2Kind::Volume, "MYTOKEN", MmNumber::from("1"));
        reserve_v2_amount(&ctx, uuid, LockedAmountV2Kind::Volume, "ETH", MmNumber::from("0.01"));

        assert_eq!(get_locked_amount(&ctx, "MYTOKEN"), MmNumber::from("1"));
        assert_eq!(
            get_locked_amount(&ctx, "ETH"),
            MmNumber::from("0.01"),
            "the send fee must be visible in its own coin's bucket, unlike the nested shape above"
        );

        // A resume re-derives its reservations by reading the persisted log
        // back (R63); re-applying the same event must not double-reserve.
        reserve_v2_amount(&ctx, uuid, LockedAmountV2Kind::Volume, "ETH", MmNumber::from("0.01"));
        assert_eq!(get_locked_amount(&ctx, "ETH"), MmNumber::from("0.01"));

        // The payment is broadcast: both halves were committed together and
        // release together, whether or not they shared a bucket.
        release_v2_amount(&ctx, &uuid, LockedAmountV2Kind::Volume, "MYTOKEN");
        release_v2_amount(&ctx, &uuid, LockedAmountV2Kind::Volume, "ETH");
        assert_eq!(get_locked_amount(&ctx, "MYTOKEN"), MmNumber::from(0));
        assert_eq!(get_locked_amount(&ctx, "ETH"), MmNumber::from(0));
    }
}

#[cfg(test)]
mod secret_hash_algo_selection_tests {
    use super::*;
    use coins::TestCoin;

    // The overwhelmingly common case, and the one this repository's swap
    // test suite already builds coin pairs for throughout
    // (`MmCoinEnum::Test`) -- confirms the default stays the 20-byte
    // algorithm when neither side is one of the R72-named families.
    #[test]
    fn an_ordinary_pair_selects_the_20_byte_default() {
        let a = MmCoinEnum::Test(TestCoin::default());
        let b = MmCoinEnum::Test(TestCoin::default());
        assert!(matches!(select_secret_hash_algo(&a, &b), SecretHashAlgo::DHASH160));
    }

    // NOT independently tested here: that Sia, Tendermint, TendermintToken,
    // and (off-wasm32) Lightning select the 32-byte alternate. This
    // repository's test infrastructure has no lightweight way to construct a
    // functional SiaCoin/TendermintCoin/LightningCoin (each wraps a real API
    // client, unlike the MmCoinEnum::Test double used above), and a test that
    // merely re-typed the same family list a second time to compare against
    // itself would pass regardless of whether `select_secret_hash_algo`'s
    // match arms were ever kept in sync with it — worse than no test, since
    // it would look like coverage. That branch is verified by CRD ch.51 R72
    // cross-reference and code review instead (see the doc comment on
    // `select_secret_hash_algo` above); a real regression test needs test
    // doubles for these coin families that do not exist yet.
}
