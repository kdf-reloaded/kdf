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
//  lp_ordermatch.rs
//  marketmaker
//

use async_trait::async_trait;
use best_orders::BestOrdersAction;
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use coins::utxo::{compressed_pub_key_from_priv_raw, ChecksumType, UtxoAddressFormat};
use coins::{coin_conf, find_pair, lp_coinfind, BalanceTradeFeeUpdatedHandler, CoinProtocol, FeeApproxStage, MmCoinEnum};
use common::executor::{spawn, Timer};
use common::log::{error, LogOnError};
use common::mm_number::{BigDecimal, BigRational, Fraction, MmNumber, MmNumberMultiRepr};
use common::time_cache::TimeCache;
use common::{bits256, log, new_uuid, now_ms, HttpStatusCode};
use crypto::privkey::SerializableSecp256k1Keypair;
use crypto::CryptoCtx;
use derive_more::Display;
use futures::channel::mpsc::{unbounded, UnboundedSender};
#[cfg(test)] use futures::channel::oneshot;
use futures::{compat::Future01CompatExt, lock::Mutex as AsyncMutex, StreamExt, TryFutureExt};
use hash256_std_hasher::Hash256StdHasher;
use hash_db::Hasher;
use http::{Response, StatusCode};
use keys::{AddressFormat, KeyPair};
use mm2_core::mm_ctx::{from_ctx, MmArc, MmWeak};
use mm2_err_handle::prelude::*;
use mm2_event_stream::StreamerId;
use mm2_p2p::{decode_signed, encode_and_sign, encode_message, pub_sub_topic, TopicPrefix, TOPIC_SEPARATOR};
#[cfg(test)] use mocktopus::macros::*;
use num_traits::identities::Zero;
use parking_lot::{Mutex as PaMutex, RwLock as PaRwLock};
use rpc::v1::types::H256 as H256Json;
use serde_json::{self as json, Value as Json};
use sp_trie::{delta_trie_root, MemoryDB, Trie, TrieConfiguration, TrieDB, TrieDBMut, TrieHash, TrieMut};
use std::collections::hash_map::{Entry, HashMap};
use std::collections::{BTreeSet, HashSet};
use std::convert::TryInto;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use timed_map::TimedMap;
use trie_db::NodeCodec as NodeCodecT;
use uuid::Uuid;

use crate::mm2::lp_network::{broadcast_p2p_msg, request_any_relay, request_one_peer, subscribe_to_topic, Libp2pPeerId,
                             P2PRequest};
use crate::mm2::lp_swap::{calc_max_maker_vol, check_balance_for_maker_swap, check_balance_for_taker_swap,
                          check_other_coin_balance_for_swap, insert_new_swap_to_db, is_pubkey_banned,
                          lp_atomic_locktime, run_maker_swap, run_taker_swap, swap_versioning::SwapVersion,
                          AtomicLocktimeVersion, MakerSwap, RunMakerSwapInput, RunTakerSwapInput,
                          SwapConfirmationsSettings, TakerSwap};

pub use best_orders::{best_orders_rpc, best_orders_rpc_v2};
use my_orders_storage::{delete_my_maker_order, delete_my_taker_order, save_maker_order_on_update,
                        save_my_new_maker_order, save_my_new_taker_order, MyActiveOrders, MyOrdersFilteringHistory,
                        MyOrdersHistory, MyOrdersStorage};
pub use orderbook_depth::orderbook_depth_rpc;
pub use orderbook_rpc::{orderbook_rpc, orderbook_rpc_v2};

cfg_wasm32! {
    use mm2_db::indexed_db::{ConstructibleDb, DbLocked};
    use ordermatch_wasm_db::{InitDbResult, OrdermatchDb};

    pub type OrdermatchDbLocked<'a> = DbLocked<'a, OrdermatchDb>;
}

#[path = "lp_ordermatch/best_orders.rs"] mod best_orders;
#[path = "lp_ordermatch/lp_bot.rs"] mod lp_bot;
#[cfg(test)]
pub use lp_bot::{process_price_request, StartSimpleMakerBotRequest, KMD_PRICE_ENDPOINT};
pub use lp_bot::{start_simple_market_maker_bot, stop_simple_market_maker_bot, TradingBotEvent};

#[path = "lp_ordermatch/my_orders_storage.rs"]
mod my_orders_storage;
#[path = "lp_ordermatch/new_protocol.rs"] mod new_protocol;
#[path = "lp_ordermatch/order_events.rs"]
pub(crate) mod order_events;
#[path = "lp_ordermatch/order_requests_tracker.rs"]
mod order_requests_tracker;
#[path = "lp_ordermatch/orderbook_depth.rs"] mod orderbook_depth;
#[path = "lp_ordermatch/orderbook_events.rs"]
pub(crate) mod orderbook_events;
#[path = "lp_ordermatch/orderbook_rpc.rs"] mod orderbook_rpc;
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "ordermatch_tests.rs"]
pub mod ordermatch_tests;

#[cfg(target_arch = "wasm32")]
#[path = "lp_ordermatch/ordermatch_wasm_db.rs"]
mod ordermatch_wasm_db;

#[path = "lp_ordermatch/ordermatch_types.rs"]
mod ordermatch_types;
pub use ordermatch_types::*;

#[path = "lp_ordermatch/ordermatch_orderbook.rs"]
mod ordermatch_orderbook;
pub use ordermatch_orderbook::*;

#[path = "lp_ordermatch/ordermatch_trading.rs"]
mod ordermatch_trading;
pub use ordermatch_trading::*;

pub const ORDERBOOK_PREFIX: TopicPrefix = "orbk";
#[cfg(not(test))]
pub const MIN_ORDER_KEEP_ALIVE_INTERVAL: u64 = 30;
#[cfg(test)]
pub const MIN_ORDER_KEEP_ALIVE_INTERVAL: u64 = 5;
const MAKER_ORDER_TIMEOUT: u64 = MIN_ORDER_KEEP_ALIVE_INTERVAL * 3;
const TAKER_ORDER_TIMEOUT: u64 = 30;
const ORDER_MATCH_TIMEOUT: u64 = 30;
const ORDERBOOK_REQUESTING_TIMEOUT: u64 = MIN_ORDER_KEEP_ALIVE_INTERVAL * 2;
const MAX_ORDERS_NUMBER_IN_ORDERBOOK_RESPONSE: usize = 1000;
#[cfg(not(test))]
const TRIE_STATE_HISTORY_TIMEOUT: u64 = 14400;
#[cfg(test)]
const TRIE_STATE_HISTORY_TIMEOUT: u64 = 3;
#[cfg(not(test))]
const TRIE_ORDER_HISTORY_TIMEOUT: u64 = 300;
#[cfg(test)]
const TRIE_ORDER_HISTORY_TIMEOUT: u64 = 3;

/// Alphabetically ordered orderbook pair
type AlbOrderedOrderbookPair = String;
type PubkeyOrders = Vec<(Uuid, OrderbookP2PItem)>;

pub type OrdermatchInitResult<T> = Result<T, MmError<OrdermatchInitError>>;

#[derive(Debug, Deserialize, Display, Serialize)]
pub enum OrdermatchInitError {
    #[display(fmt = "Error deserializing '{}' config field: {}", field, error)]
    ErrorDeserializingConfig {
        field: String,
        error: String,
    },
    Internal(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelAllOrdersResponse {
    cancelled: Vec<Uuid>,
    currently_matching: Vec<Uuid>,
}

#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum CancelAllOrdersError {
    LegacyError(String),
}

/// Wrapper around `TimedMap` that enriches the flat maker-order store with:
///
///  * **Per-order TTL** via `TimedMap::insert_expirable_unchecked` — orders with
///    `timeout_in_minutes` are evicted automatically.
///  * **Ticker tracking** (`order_tickers`, `count_by_tickers`) — enables
///    fast "does this coin have active maker orders?" queries without iterating
///    all orders.
///
/// Call [`MakerOrdersContext::drop_expired`] periodically (e.g. in
/// `lp_ordermatch_loop`) to collect and cancel orders whose TTL has elapsed.
pub struct MakerOrdersContext {
    orders: TimedMap<Uuid, Arc<AsyncMutex<MakerOrder>>>,
    /// uuid → base ticker, for reverse lookup on removal.
    order_tickers: HashMap<Uuid, String>,
    /// base ticker → count of active orders for that ticker.
    count_by_tickers: HashMap<String, usize>,
}

impl MakerOrdersContext {
    fn new() -> Self {
        MakerOrdersContext {
            orders: TimedMap::new(),
            order_tickers: HashMap::new(),
            count_by_tickers: HashMap::new(),
        }
    }

    /// Insert a maker order.  If `timeout_in_minutes` is set the entry will
    /// expire after that many minutes; otherwise it lives until explicitly
    /// removed.
    pub fn add_order(&mut self, order: &MakerOrder, order_arc: Arc<AsyncMutex<MakerOrder>>) {
        let uuid = order.uuid;
        let ticker = order.base.clone();

        if let Some(t) = order.timeout_in_minutes {
            self.orders
                .insert_expirable_unchecked(uuid, order_arc, Duration::from_secs(u64::from(t) * 60));
        } else {
            self.orders.insert_constant_unchecked(uuid, order_arc);
        }

        self.order_tickers.insert(uuid, ticker.clone());
        *self.count_by_tickers.entry(ticker).or_insert(0) += 1;
    }

    /// Remove an order by UUID.  Returns the `Arc<AsyncMutex<MakerOrder>>` if it
    /// was present.
    pub fn remove_order(&mut self, uuid: &Uuid) -> Option<Arc<AsyncMutex<MakerOrder>>> {
        let removed = self.orders.remove(uuid);
        if removed.is_some() {
            if let Some(ticker) = self.order_tickers.remove(uuid) {
                if let Some(count) = self.count_by_tickers.get_mut(&ticker) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        removed
    }

    /// Get an order by UUID (returns a clone of the `Arc`).
    pub fn get_order(&self, uuid: &Uuid) -> Option<Arc<AsyncMutex<MakerOrder>>> { self.orders.get(uuid).cloned() }

    /// Check whether an order is present.
    pub fn contains_key(&self, uuid: &Uuid) -> bool { self.orders.contains_key(uuid) }

    /// Returns true if there is at least one active maker order for `ticker`.
    pub fn coin_has_active_maker_orders(&self, ticker: &str) -> bool {
        self.count_by_tickers.get(ticker).copied() > Some(0)
    }

    /// Clone the entire order map (snapshot for iteration without holding the
    /// lock).
    pub fn clone_orders(&self) -> HashMap<Uuid, Arc<AsyncMutex<MakerOrder>>> {
        self.orders.clone().into_iter().collect()
    }

    /// Insert a pre-wrapped order directly (used for taker→maker conversion and
    /// kick-start where we already have the Arc).
    pub fn insert_raw(
        &mut self,
        uuid: Uuid,
        ticker: String,
        order_arc: Arc<AsyncMutex<MakerOrder>>,
        timeout_in_minutes: Option<u16>,
    ) {
        if let Some(t) = timeout_in_minutes {
            self.orders
                .insert_expirable_unchecked(uuid, order_arc, Duration::from_secs(u64::from(t) * 60));
        } else {
            self.orders.insert_constant_unchecked(uuid, order_arc);
        }
        self.order_tickers.insert(uuid, ticker.clone());
        *self.count_by_tickers.entry(ticker).or_insert(0) += 1;
    }

    /// Collect all expired orders (those whose TTL has elapsed) and clean up
    /// ticker tracking.  Returns the expired entries for the caller to handle
    /// cancellation / P2P notification.
    pub fn drop_expired(&mut self) -> Vec<(Uuid, Arc<AsyncMutex<MakerOrder>>)> {
        let expired = self.orders.drop_expired_entries();
        for (uuid, _) in &expired {
            if let Some(ticker) = self.order_tickers.remove(uuid) {
                if let Some(count) = self.count_by_tickers.get_mut(&ticker) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        expired
    }

    /// Iterate over all order UUIDs.
    pub fn keys(&self) -> Vec<Uuid> { self.orders.keys() }

    /// Number of active orders.
    pub fn len(&self) -> usize { self.orders.len() }

    /// Iterate over all (uuid, order_arc) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&Uuid, &Arc<AsyncMutex<MakerOrder>>)> { self.orders.iter() }
}

impl Default for MakerOrdersContext {
    fn default() -> Self { Self::new() }
}

pub(crate) struct OrdermatchContext {
    pub maker_orders_ctx: PaMutex<MakerOrdersContext>,
    pub my_taker_orders: AsyncMutex<HashMap<Uuid, TakerOrder>>,
    pub orderbook: PaMutex<Orderbook>,
    /// Trie data store extracted from `Orderbook` to reduce contention.
    pub trie_store: Arc<PaMutex<TrieStore>>,
    /// Sender to enqueue trie mutations for background application.
    pub trie_ops_tx: UnboundedSender<Vec<TrieOp>>,
    /// Tracks which orderbook topics we are subscribed to, separate from the order index.
    pub orderbook_subscriptions: PaRwLock<HashMap<String, OrderbookRequestingState>>,
    /// The map from coin original ticker to the orderbook ticker
    /// It is used to share the same orderbooks for concurrently activated coins with different protocols
    /// E.g. BTC and BTC-Segwit
    pub orderbook_tickers: HashMap<String, String>,
    /// The map from orderbook ticker to original tickers having it in the config
    pub original_tickers: HashMap<String, HashSet<String>>,
    /// Pending MakerReserved messages for a specific TakerOrder UUID
    /// Used to select a trade with the best price upon matching
    pending_maker_reserved: AsyncMutex<HashMap<Uuid, Vec<MakerReserved>>>,
    #[cfg(target_arch = "wasm32")]
    ordermatch_db: ConstructibleDb<OrdermatchDb>,
}

#[cfg(test)]
impl Default for OrdermatchContext {
    /// Creates a test `OrdermatchContext` with a live trie-ops channel+worker.
    /// The receiver side is spawned as a local future (requires an active async
    /// runtime or the `common::executor` threadpool).
    fn default() -> Self {
        let trie_store = Arc::new(PaMutex::new(TrieStore::default()));
        let (trie_ops_tx, mut trie_ops_rx) = unbounded::<Vec<TrieOp>>();
        let ts = trie_store.clone();
        // Drive the worker on the common executor so trie ops are applied even in
        // unit-test contexts that don't have a full MmCtx runtime.
        common::executor::spawn(async move {
            while let Some(ops) = trie_ops_rx.next().await {
                let mut store = ts.lock();
                store.apply_ops(ops);
            }
        });
        OrdermatchContext {
            maker_orders_ctx: Default::default(),
            my_taker_orders: Default::default(),
            orderbook: Default::default(),
            trie_store,
            trie_ops_tx,
            orderbook_subscriptions: PaRwLock::new(HashMap::default()),
            pending_maker_reserved: Default::default(),
            orderbook_tickers: Default::default(),
            original_tickers: Default::default(),
        }
    }
}

pub fn init_ordermatch_context(ctx: &MmArc) -> OrdermatchInitResult<()> {
    // Helper
    #[derive(Deserialize)]
    struct CoinConf {
        coin: String,
        orderbook_ticker: Option<String>,
    }

    let coins: Vec<CoinConf> =
        json::from_value(ctx.conf["coins"].clone()).map_to_mm(|e| OrdermatchInitError::ErrorDeserializingConfig {
            field: "coins".to_owned(),
            error: e.to_string(),
        })?;
    let mut orderbook_tickers = HashMap::new();
    let mut original_tickers = HashMap::new();
    for coin in coins {
        if let Some(orderbook_ticker) = coin.orderbook_ticker {
            orderbook_tickers.insert(coin.coin.clone(), orderbook_ticker.clone());
            original_tickers
                .entry(orderbook_ticker)
                .or_insert_with(HashSet::new)
                .insert(coin.coin);
        }
    }

    let trie_store = Arc::new(PaMutex::new(TrieStore::default()));
    let trie_ops_tx = spawn_trie_store_worker(ctx, trie_store.clone());

    let ordermatch_context = OrdermatchContext {
        maker_orders_ctx: Default::default(),
        my_taker_orders: Default::default(),
        orderbook: Default::default(),
        trie_store,
        trie_ops_tx,
        orderbook_subscriptions: PaRwLock::new(HashMap::default()),
        pending_maker_reserved: Default::default(),
        orderbook_tickers,
        original_tickers,
        #[cfg(target_arch = "wasm32")]
        ordermatch_db: ConstructibleDb::new(ctx),
    };

    from_ctx(&ctx.ordermatch_ctx, move || Ok(ordermatch_context))
        .map(|_| ())
        .map_to_mm(OrdermatchInitError::Internal)
}

#[cfg_attr(all(test, not(target_arch = "wasm32")), mockable)]
impl OrdermatchContext {
    /// Obtains a reference to this crate context, creating it if necessary.
    #[cfg(not(target_arch = "wasm32"))]
    fn from_ctx(ctx: &MmArc) -> Result<Arc<OrdermatchContext>, String> {
        let ctx2 = ctx.clone();
        Ok(try_s!(from_ctx(&ctx.ordermatch_ctx, move || {
            let trie_store = Arc::new(PaMutex::new(TrieStore::default()));
            let trie_ops_tx = spawn_trie_store_worker(&ctx2, trie_store.clone());
            Ok(OrdermatchContext {
                maker_orders_ctx: Default::default(),
                my_taker_orders: Default::default(),
                orderbook: Default::default(),
                trie_store,
                trie_ops_tx,
                orderbook_subscriptions: PaRwLock::new(HashMap::default()),
                pending_maker_reserved: Default::default(),
                orderbook_tickers: Default::default(),
                original_tickers: Default::default(),
            })
        })))
    }

    /// Obtains a reference to this crate context, creating it if necessary.
    #[cfg(target_arch = "wasm32")]
    fn from_ctx(ctx: &MmArc) -> Result<Arc<OrdermatchContext>, String> {
        let ctx2 = ctx.clone();
        Ok(try_s!(from_ctx(&ctx.ordermatch_ctx, move || {
            let trie_store = Arc::new(PaMutex::new(TrieStore::default()));
            let trie_ops_tx = spawn_trie_store_worker(&ctx2, trie_store.clone());
            Ok(OrdermatchContext {
                maker_orders_ctx: Default::default(),
                my_taker_orders: Default::default(),
                orderbook: Default::default(),
                trie_store,
                trie_ops_tx,
                orderbook_subscriptions: PaRwLock::new(HashMap::default()),
                pending_maker_reserved: Default::default(),
                orderbook_tickers: Default::default(),
                original_tickers: Default::default(),
                ordermatch_db: ConstructibleDb::new(&ctx2),
            })
        })))
    }

    /// Obtains a reference to this crate context, creating it if necessary.
    #[allow(dead_code)]
    fn from_ctx_weak(ctx_weak: &MmWeak) -> Result<Arc<OrdermatchContext>, String> {
        let ctx = try_s!(MmArc::from_weak(ctx_weak).ok_or("Context expired"));
        Self::from_ctx(&ctx)
    }

    fn orderbook_ticker(&self, ticker: &str) -> Option<String> { self.orderbook_tickers.get(ticker).cloned() }

    /// Block until the background trie worker has applied all previously enqueued ops.
    #[cfg(test)]
    pub fn wait_trie_ops_flushed(&self) {
        let (tx, rx) = oneshot::channel::<()>();
        let _ = self.trie_ops_tx.unbounded_send(vec![TrieOp::Flush(tx)]);
        let _ = futures::executor::block_on(rx);
    }

    fn orderbook_ticker_bypass(&self, ticker: &str) -> String {
        self.orderbook_ticker(ticker).unwrap_or_else(|| ticker.to_owned())
    }

    fn orderbook_pair_bypass(&self, pair: &(String, String)) -> (String, String) {
        (
            self.orderbook_ticker(&pair.0).unwrap_or_else(|| pair.0.clone()),
            self.orderbook_ticker(&pair.1).unwrap_or_else(|| pair.1.clone()),
        )
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn ordermatch_db(&self) -> InitDbResult<OrdermatchDbLocked<'_>> {
        Ok(self.ordermatch_db.get_or_initialize().await?)
    }
}
