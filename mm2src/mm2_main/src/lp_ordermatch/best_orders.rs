use super::capability_presentation::{log_left_out, CapabilityFilter};
use super::{addr_format_from_protocol_info, is_my_order, mm2_internal_pubkey_hex, orderbook_is_filled,
            subscribe_to_orderbook_topic, BaseRelProtocolInfo, OrderConfirmationsSettings, Orderbook, OrderbookItem,
            OrderbookP2PItem, OrderbookP2PItemWithProof, OrdermatchContext, OrdermatchRequest};
use crate::mm2::lp_network::{request_any_relay, P2PRequest};
use crate::mm2::lp_ordermatch::{orderbook_address, RpcOrderbookEntryV2};
use coins::{coin_conf, is_wallet_only_conf, is_wallet_only_ticker};
use common::mm_number::{BigRational, MmNumber};
use common::{log, HttpStatusCode};
use derive_more::Display;
use futures::StreamExt;
use http::{Response, StatusCode};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use num_traits::Zero;
use serde_json::{self as json, Value as Json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BestOrdersAction {
    Buy,
    Sell,
}

#[derive(Debug, Deserialize)]
pub struct BestOrdersRequest {
    coin: String,
    action: BestOrdersAction,
    volume: MmNumber,
}

/// How to select best orders in the v2 RPC: by total volume or by count.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum RequestBestOrdersBy {
    #[serde(rename = "volume")]
    Volume(MmNumber),
    #[serde(rename = "number")]
    Number(usize),
}

/// V2 best-orders request supporting both volume-based and number-based queries,
/// plus an optional flag to exclude our own orders from results.
#[derive(Debug, Deserialize)]
pub struct BestOrdersRequestV2 {
    coin: String,
    action: BestOrdersAction,
    request_by: RequestBestOrdersBy,
    #[serde(default)]
    exclude_mine: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct BestOrdersP2PRes {
    orders: HashMap<String, Vec<OrderbookP2PItemWithProof>>,
    #[serde(default)]
    protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    #[serde(default)]
    conf_infos: HashMap<Uuid, OrderConfirmationsSettings>,
}

/// How a best-orders request selects the orders of a pair.
enum BestOrdersBy {
    /// The best-priced orders until their volume covers the requested one.
    Volume(BigRational),
    /// The given number of best-priced orders.
    Number(usize),
}

/// The best orders the own book holds for `pair`, best price first: the selection this node applies when it
/// answers a best-orders request as a relay, over the orders `accept` admits. `None` when the book holds no
/// orders for the pair.
///
/// The two P2P handlers pass a predicate that admits every order, so their answers do not depend on it;
/// the RPC refill of CRD ch.59 R59.4C.5 admits presentable orders only.
fn select_best_orders<'a>(
    orderbook: &'a Orderbook,
    pair: &(String, String),
    action: BestOrdersAction,
    by: &BestOrdersBy,
    accept: impl Fn(&OrderbookItem) -> bool,
) -> Option<Vec<&'a OrderbookItem>> {
    let orders = match orderbook.ordered.get(pair) {
        Some(orders) => orders,
        None => {
            log::debug!("No orders for pair {:?}", pair);
            return None;
        },
    };
    let mut best_orders = vec![];
    let mut collected_volume = BigRational::zero();
    // By number: the positions of the price order used so far.
    let mut positions_used = 0;
    for ordered in orders {
        if matches!(by, BestOrdersBy::Number(number) if positions_used >= *number) {
            break;
        }
        let o = match orderbook.order_set.get(&ordered.uuid) {
            Some(o) => o,
            None => {
                log::debug!("No order with uuid {:?}", ordered.uuid);
                positions_used += 1;
                continue;
            },
        };
        if !accept(o) {
            continue;
        }
        match by {
            BestOrdersBy::Number(_) => {
                positions_used += 1;
                best_orders.push(o);
            },
            BestOrdersBy::Volume(required_volume) => {
                let min_volume = match action {
                    BestOrdersAction::Buy => o.min_volume.clone(),
                    BestOrdersAction::Sell => &o.min_volume * &o.price,
                };
                if &min_volume > required_volume {
                    log::debug!("Order {} min_vol {:?} > {:?}", o.uuid, min_volume, required_volume);
                    continue;
                }

                let max_volume = match action {
                    BestOrdersAction::Buy => o.max_volume.clone(),
                    BestOrdersAction::Sell => &o.max_volume * &o.price,
                };
                best_orders.push(o);

                collected_volume += max_volume;
                if &collected_volume >= required_volume {
                    break;
                }
            },
        }
    }
    Some(best_orders)
}

pub fn process_best_orders_p2p_request(
    ctx: MmArc,
    coin: String,
    action: BestOrdersAction,
    required_volume: BigRational,
) -> Result<Option<Vec<u8>>, String> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("ordermatch_ctx must exist at this point");
    let orderbook = ordermatch_ctx.orderbook.lock();
    let search_pairs_in = match action {
        BestOrdersAction::Buy => &orderbook.pairs_existing_for_base,
        BestOrdersAction::Sell => &orderbook.pairs_existing_for_rel,
    };
    let tickers = match search_pairs_in.get(&coin) {
        Some(tickers) => tickers,
        None => return Ok(None),
    };
    let mut result = HashMap::new();
    let pairs = tickers.iter().map(|ticker| match action {
        BestOrdersAction::Buy => (coin.clone(), ticker.clone()),
        BestOrdersAction::Sell => (ticker.clone(), coin.clone()),
    });

    let mut protocol_infos = HashMap::new();
    let mut conf_infos = HashMap::new();

    let by = BestOrdersBy::Volume(required_volume);
    for pair in pairs {
        // CRD ch.59 R59.4C.4: a relay serves every order, presentable to this node or not.
        let Some(selected) = select_best_orders(&orderbook, &pair, action, &by, |_| true) else {
            continue;
        };
        let mut best_orders = vec![];
        for o in selected {
            let order_w_proof = orderbook.orderbook_item_with_proof(o.clone());
            protocol_infos.insert(order_w_proof.order.uuid, order_w_proof.order.base_rel_proto_info());
            if let Some(info) = order_w_proof.order.conf_settings {
                conf_infos.insert(order_w_proof.order.uuid, info);
            }
            best_orders.push(order_w_proof.into());
        }
        match action {
            BestOrdersAction::Buy => result.insert(pair.1, best_orders),
            BestOrdersAction::Sell => result.insert(pair.0, best_orders),
        };
    }
    let response = BestOrdersP2PRes {
        orders: result,
        protocol_infos,
        conf_infos,
    };
    let encoded = rmp_serde::to_vec_named(&response).expect("rmp_serde::to_vec_named should not fail here");
    Ok(Some(encoded))
}

/// P2P handler: return the top `number` best-priced orders per pair for `coin`.
pub fn process_best_orders_p2p_request_by_number(
    ctx: MmArc,
    coin: String,
    action: BestOrdersAction,
    number: usize,
) -> Result<Option<Vec<u8>>, String> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("ordermatch_ctx must exist at this point");
    let orderbook = ordermatch_ctx.orderbook.lock();
    let search_pairs_in = match action {
        BestOrdersAction::Buy => &orderbook.pairs_existing_for_base,
        BestOrdersAction::Sell => &orderbook.pairs_existing_for_rel,
    };
    let tickers = match search_pairs_in.get(&coin) {
        Some(tickers) => tickers,
        None => return Ok(None),
    };
    let mut result = HashMap::new();
    let pairs = tickers.iter().map(|ticker| match action {
        BestOrdersAction::Buy => (coin.clone(), ticker.clone()),
        BestOrdersAction::Sell => (ticker.clone(), coin.clone()),
    });

    let mut protocol_infos = HashMap::new();
    let mut conf_infos = HashMap::new();

    let by = BestOrdersBy::Number(number);
    for pair in pairs {
        // CRD ch.59 R59.4C.4: a relay serves every order, presentable to this node or not.
        let Some(selected) = select_best_orders(&orderbook, &pair, action, &by, |_| true) else {
            continue;
        };
        let mut best_orders = vec![];
        for o in selected {
            let order_w_proof = orderbook.orderbook_item_with_proof(o.clone());
            protocol_infos.insert(order_w_proof.order.uuid, order_w_proof.order.base_rel_proto_info());
            if let Some(info) = order_w_proof.order.conf_settings {
                conf_infos.insert(order_w_proof.order.uuid, info);
            }
            best_orders.push(order_w_proof.into());
        }
        match action {
            BestOrdersAction::Buy => result.insert(pair.1, best_orders),
            BestOrdersAction::Sell => result.insert(pair.0, best_orders),
        };
    }
    let response = BestOrdersP2PRes {
        orders: result,
        protocol_infos,
        conf_infos,
    };
    let encoded = rmp_serde::to_vec_named(&response).expect("rmp_serde::to_vec_named should not fail here");
    Ok(Some(encoded))
}

/// The pairs of one best-orders call whose order books are fetched at the same time (CRD ch.59 R59.4C.5).
const BEST_ORDERS_REFILL_PARALLELISM: usize = 4;

/// The orders of one coin in a best-orders answer, with the coin's configuration entry.
struct BestOrdersOfCoin {
    coin: String,
    coin_conf: Json,
    orders: Vec<OrderbookItem>,
}

/// What a best-orders RPC call lists of a relay's answer (CRD ch.59 R59.4C.3, R59.4C.5).
///
/// A coin that is not in the configuration, or is wallet-only there, is dropped as before. Of every other
/// coin the presentable orders are kept. When the relay's answer for a pair loses at least one order, the
/// pair's full order book is fetched once, with the request `orderbook` makes, and the pair's part is built
/// from the node's own book: the relay's selection over presentable orders only. If the fetch fails, the
/// pair's part is the relay's answer without the orders that are not presentable, and one warning is logged.
async fn listed_best_orders(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    requested_coin: &str,
    action: BestOrdersAction,
    by: &BestOrdersBy,
    p2p_response: BestOrdersP2PRes,
    is_mine: &(impl Fn(&str) -> bool + Sync),
) -> Vec<BestOrdersOfCoin> {
    let BestOrdersP2PRes {
        orders,
        protocol_infos,
        conf_infos,
    } = p2p_response;
    let mut answered = Vec::with_capacity(orders.len());
    for (coin, orders_w_proofs) in orders {
        let coin_conf = coin_conf(ctx, &coin);
        if coin_conf.is_null() {
            log::warn!("Coin {} is not found in config", coin);
            continue;
        }
        if is_wallet_only_conf(&coin_conf) {
            log::warn!(
                "Coin {} was removed from best orders because it's defined as wallet only in config",
                coin
            );
            continue;
        }
        let orders = orders_w_proofs
            .into_iter()
            .map(|order_w_proof| {
                let order = order_w_proof.order;
                let proto_info = protocol_infos.get(&order.uuid).cloned().unwrap_or_default();
                let conf_info = conf_infos.get(&order.uuid).copied();
                OrderbookItem::from_p2p_and_info(order, proto_info, conf_info)
            })
            .collect();
        answered.push(BestOrdersOfCoin {
            coin,
            coin_conf,
            orders,
        });
    }

    let requested_ticker = ordermatch_ctx.orderbook_ticker_bypass(requested_coin);
    let requested_ticker = &requested_ticker;
    futures::stream::iter(answered)
        .map(|of_coin| async move {
            let BestOrdersOfCoin {
                coin,
                coin_conf,
                orders,
            } = of_coin;
            let capability_filter = CapabilityFilter::for_pair(ctx, requested_coin, &coin).await;
            let answered_number = orders.len();
            let is_presentable =
                |order: &OrderbookItem| capability_filter.is_presentable(order, is_mine(&order.pubkey));
            let listed: Vec<_> = orders.into_iter().filter(|order| is_presentable(order)).collect();
            let left_out = answered_number - listed.len();
            if left_out == 0 {
                return BestOrdersOfCoin {
                    coin,
                    coin_conf,
                    orders: listed,
                };
            }

            let pair = match action {
                BestOrdersAction::Buy => (requested_ticker.clone(), coin.clone()),
                BestOrdersAction::Sell => (coin.clone(), requested_ticker.clone()),
            };
            log_left_out("best_orders", &pair.0, &pair.1, left_out);
            let fetched = match subscribe_to_orderbook_topic(ctx, &pair.0, &pair.1, true).await {
                Ok(()) if orderbook_is_filled(ctx, &pair.0, &pair.1) => Ok(()),
                Ok(()) => Err("no relay answered the request".to_owned()),
                Err(e) => Err(e),
            };
            let orders = match fetched {
                Ok(()) => {
                    let orderbook = ordermatch_ctx.orderbook.lock();
                    select_best_orders(&orderbook, &pair, action, by, is_presentable)
                        .map(|selected| selected.into_iter().cloned().collect())
                        .unwrap_or_default()
                },
                Err(e) => {
                    log::warn!(
                        "best_orders: the order book of {}/{} could not be fetched, the relay's answer is listed \
                         without the orders left out: {}",
                        pair.0,
                        pair.1,
                        e
                    );
                    listed
                },
            };
            BestOrdersOfCoin {
                coin,
                coin_conf,
                orders,
            }
        })
        .buffer_unordered(BEST_ORDERS_REFILL_PARALLELISM)
        .collect()
        .await
}

pub async fn best_orders_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: BestOrdersRequest = try_s!(json::from_value(req));
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    if is_wallet_only_ticker(&ctx, &req.coin) {
        return ERR!("Coin {} is wallet only", &req.coin);
    }
    let my_pubsecp = try_s!(mm2_internal_pubkey_hex(&ctx));
    let my_p2p_pubkeys = {
        let orderbook = ordermatch_ctx.orderbook.lock();
        orderbook.my_p2p_pubkeys.clone()
    };
    let is_mine = |pubkey: &str| is_my_order(pubkey, &my_pubsecp, &my_p2p_pubkeys);
    let volume: BigRational = req.volume.into();
    let p2p_request = OrdermatchRequest::BestOrders {
        coin: ordermatch_ctx.orderbook_ticker_bypass(&req.coin),
        action: req.action,
        volume: volume.clone(),
    };
    let by = BestOrdersBy::Volume(volume);

    let best_orders_res =
        try_s!(request_any_relay::<BestOrdersP2PRes>(ctx.clone(), P2PRequest::Ordermatch(p2p_request)).await);
    let mut response = HashMap::new();
    if let Some((p2p_response, peer_id)) = best_orders_res {
        log::debug!("Got best orders {:?} from peer {}", p2p_response, peer_id);
        // CRD ch.59 R59.4C.3, R59.4C.5: only presentable orders are listed.
        let listed = listed_best_orders(
            &ctx,
            &ordermatch_ctx,
            &req.coin,
            req.action,
            &by,
            p2p_response,
            &is_mine,
        )
        .await;
        for BestOrdersOfCoin {
            coin,
            coin_conf,
            orders,
        } in listed
        {
            for item in orders {
                let addr_format = match req.action {
                    BestOrdersAction::Buy => addr_format_from_protocol_info(&item.rel_protocol_info),
                    BestOrdersAction::Sell => addr_format_from_protocol_info(&item.base_protocol_info),
                };
                let address = match orderbook_address(&ctx, &coin, &coin_conf, &item.pubkey, addr_format) {
                    Ok(a) => a.into_legacy_string(),
                    Err(e) => {
                        log::error!("Error {} getting coin {} address from pubkey {}", e, coin, item.pubkey);
                        continue;
                    },
                };
                let conf_settings = item.conf_settings;
                let order = OrderbookP2PItem::from(item);
                let is_mine = is_mine(&order.pubkey);
                let entry = match req.action {
                    BestOrdersAction::Buy => order.as_rpc_best_orders_buy(address, conf_settings.as_ref(), is_mine),
                    BestOrdersAction::Sell => order.as_rpc_best_orders_sell(address, conf_settings.as_ref(), is_mine),
                };
                response.entry(coin.clone()).or_insert_with(Vec::new).push(entry);
            }
        }
    }

    let res = json!({ "result": response, "original_tickers": &ordermatch_ctx.original_tickers });
    Response::builder()
        .body(json::to_vec(&res).expect("Serialization failed"))
        .map_err(|e| ERRL!("{}", e))
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum BestOrdersRpcError {
    CoinIsWalletOnly(String),
    P2PError(String),
}

impl HttpStatusCode for BestOrdersRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            BestOrdersRpcError::CoinIsWalletOnly(_) => StatusCode::BAD_REQUEST,
            BestOrdersRpcError::P2PError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
pub struct BestOrdersV2Response {
    orders: HashMap<String, Vec<RpcOrderbookEntryV2>>,
    original_tickers: HashMap<String, HashSet<String>>,
}

pub async fn best_orders_rpc_v2(
    ctx: MmArc,
    req: BestOrdersRequestV2,
) -> Result<BestOrdersV2Response, MmError<BestOrdersRpcError>> {
    if is_wallet_only_ticker(&ctx, &req.coin) {
        return MmError::err(BestOrdersRpcError::CoinIsWalletOnly(req.coin));
    }
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let my_pubsecp = mm2_internal_pubkey_hex(&ctx).map_to_mm(BestOrdersRpcError::P2PError)?;
    let my_p2p_pubkeys = {
        let orderbook = ordermatch_ctx.orderbook.lock();
        orderbook.my_p2p_pubkeys.clone()
    };

    let is_mine = |pubkey: &str| is_my_order(pubkey, &my_pubsecp, &my_p2p_pubkeys);

    // Build the appropriate P2P request based on query type (volume vs count).
    let by = match req.request_by {
        RequestBestOrdersBy::Volume(ref mm_number) => BestOrdersBy::Volume(mm_number.to_ratio()),
        RequestBestOrdersBy::Number(size) => BestOrdersBy::Number(size),
    };
    let p2p_request = match &by {
        BestOrdersBy::Volume(volume) => OrdermatchRequest::BestOrders {
            coin: ordermatch_ctx.orderbook_ticker_bypass(&req.coin),
            action: req.action,
            volume: volume.clone(),
        },
        BestOrdersBy::Number(size) => OrdermatchRequest::BestOrdersByNumber {
            coin: ordermatch_ctx.orderbook_ticker_bypass(&req.coin),
            action: req.action,
            number: *size,
        },
    };

    let best_orders_res = request_any_relay::<BestOrdersP2PRes>(ctx.clone(), P2PRequest::Ordermatch(p2p_request))
        .await
        .mm_err(|e| BestOrdersRpcError::P2PError(format!("{:?}", e)))?;
    let mut orders = HashMap::new();
    if let Some((p2p_response, peer_id)) = best_orders_res {
        log::debug!("Got best orders {:?} from peer {}", p2p_response, peer_id);
        // CRD ch.59 R59.4C.3, R59.4C.5: only presentable orders are listed.
        let listed = listed_best_orders(
            &ctx,
            &ordermatch_ctx,
            &req.coin,
            req.action,
            &by,
            p2p_response,
            &is_mine,
        )
        .await;
        for BestOrdersOfCoin {
            coin,
            coin_conf,
            orders: items,
        } in listed
        {
            for item in items {
                let is_mine = is_mine(&item.pubkey);
                if req.exclude_mine && is_mine {
                    continue;
                }
                let addr_format = match req.action {
                    BestOrdersAction::Buy => addr_format_from_protocol_info(&item.rel_protocol_info),
                    BestOrdersAction::Sell => addr_format_from_protocol_info(&item.base_protocol_info),
                };
                let address = match orderbook_address(&ctx, &coin, &coin_conf, &item.pubkey, addr_format) {
                    Ok(a) => a,
                    Err(e) => {
                        log::error!("Error {} getting coin {} address from pubkey {}", e, coin, item.pubkey);
                        continue;
                    },
                };
                let conf_settings = item.conf_settings;
                let order = OrderbookP2PItem::from(item);
                let entry = match req.action {
                    BestOrdersAction::Buy => order.as_rpc_best_orders_buy_v2(address, conf_settings.as_ref(), is_mine),
                    BestOrdersAction::Sell => {
                        order.as_rpc_best_orders_sell_v2(address, conf_settings.as_ref(), is_mine)
                    },
                };
                orders.entry(coin.clone()).or_insert_with(Vec::new).push(entry);
            }
        }
    }

    Ok(BestOrdersV2Response {
        orders,
        original_tickers: ordermatch_ctx.original_tickers.clone(),
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod best_orders_test {
    use super::*;
    use crate::mm2::lp_ordermatch::ordermatch_tests::make_random_orders;
    use crate::mm2::lp_ordermatch::{OrderbookItem, TrieProof};
    use std::iter::FromIterator;

    #[test]
    fn check_best_orders_p2p_res_serde() {
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        struct BestOrderV1 {
            pubkey: String,
            base: String,
            rel: String,
            price: BigRational,
            max_volume: BigRational,
            min_volume: BigRational,
            uuid: Uuid,
            created_at: u64,
        }

        impl From<OrderbookItem> for BestOrderV1 {
            fn from(o: OrderbookItem) -> Self {
                BestOrderV1 {
                    pubkey: o.pubkey,
                    base: o.base,
                    rel: o.rel,
                    price: o.price,
                    max_volume: o.max_volume,
                    min_volume: o.min_volume,
                    uuid: o.uuid,
                    created_at: o.created_at,
                }
            }
        }

        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        struct BestOrderWithProofV1 {
            /// Orderbook item
            order: BestOrderV1,
            /// Last pubkey message payload that contains most recent pair trie root
            last_message_payload: Vec<u8>,
            /// Proof confirming that orderbook item is in the pair trie
            proof: TrieProof,
        }

        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        struct BestOrdersResV1 {
            orders: HashMap<String, Vec<BestOrderWithProofV1>>,
        }

        let orders = make_random_orders("".into(), &[1; 32], "DOC".into(), "MARTY".into(), 10);
        let v1_orders: Vec<_> = orders
            .clone()
            .into_iter()
            .map(|order| BestOrderWithProofV1 {
                order: order.into(),
                last_message_payload: vec![],
                proof: vec![],
            })
            .collect();

        let v1 = BestOrdersResV1 {
            orders: HashMap::from_iter(std::iter::once(("DOC".into(), v1_orders))),
        };

        let v1_serialized = rmp_serde::to_vec_named(&v1).unwrap();

        let mut new: BestOrdersP2PRes = rmp_serde::from_slice(&v1_serialized).unwrap();
        new.protocol_infos.insert(Uuid::new_v4(), BaseRelProtocolInfo {
            base: vec![1],
            rel: vec![2],
        });
        new.conf_infos
            .insert(Uuid::new_v4(), OrderConfirmationsSettings::default());

        let new_serialized = rmp_serde::to_vec_named(&new).unwrap();

        let v1_from_new: BestOrdersResV1 = rmp_serde::from_slice(&new_serialized).unwrap();
        assert_eq!(v1, v1_from_new);

        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        struct BestOrdersResV2 {
            orders: HashMap<String, Vec<OrderbookP2PItemWithProof>>,
            #[serde(default)]
            protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
        }
        let v2_orders: Vec<_> = orders
            .into_iter()
            .map(|order| OrderbookP2PItemWithProof {
                order: order.into(),
                last_message_payload: vec![],
                proof: vec![],
            })
            .collect();

        let v2 = BestOrdersResV2 {
            orders: HashMap::from_iter(std::iter::once(("DOC".into(), v2_orders))),
            protocol_infos: HashMap::from_iter(std::iter::once((Uuid::new_v4(), BaseRelProtocolInfo {
                base: vec![1],
                rel: vec![2],
            }))),
        };

        let v2_serialized = rmp_serde::to_vec_named(&v2).unwrap();

        let mut new: BestOrdersP2PRes = rmp_serde::from_slice(&v2_serialized).unwrap();
        new.conf_infos
            .insert(Uuid::new_v4(), OrderConfirmationsSettings::default());

        let new_serialized = rmp_serde::to_vec_named(&new).unwrap();

        let v2_from_new: BestOrdersResV2 = rmp_serde::from_slice(&new_serialized).unwrap();
        assert_eq!(v2, v2_from_new);
    }

    // ---- Legacy `best_orders` address rendering (CRD ch.32 R-F6c..R-F6e, T-F6.3, T-F6.4) ----

    /// Answers the next `best_orders` P2P request of `ctx` with `orders` grouped under their coins.
    fn answer_best_orders_request(orders: Vec<(&str, Vec<OrderbookItem>)>) {
        use crate::mm2::lp_network::P2PContext;
        use futures::StreamExt;
        use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
        use mocktopus::mocking::{MockResult, Mockable};

        let (cmd_tx, mut cmd_rx) = futures::channel::mpsc::channel(10);
        P2PContext::fetch_from_mm_arc
            .mock_safe(move |_| MockResult::Return(std::sync::Arc::new(P2PContext::new(cmd_tx.clone()))));
        let orders = orders
            .into_iter()
            .map(|(coin, items)| {
                let items = items
                    .into_iter()
                    .map(|item| OrderbookP2PItemWithProof {
                        order: item.into(),
                        last_message_payload: vec![],
                        proof: TrieProof::default(),
                    })
                    .collect();
                (coin.to_owned(), items)
            })
            .collect();
        let response = BestOrdersP2PRes {
            orders,
            protocol_infos: HashMap::new(),
            conf_infos: HashMap::new(),
        };
        let encoded = rmp_serde::to_vec_named(&response).unwrap();
        std::thread::spawn(move || {
            if let Some(AdexBehaviourCmd::RequestAnyRelay { response_tx, .. }) =
                futures::executor::block_on(cmd_rx.next())
            {
                let _ = response_tx.send(Some((mm2_p2p::PeerId::random(), encoded)));
            }
        });
    }

    fn legacy_best_orders(ctx: &MmArc, coin: &str, action: &str) -> Json {
        let req = json::json!({ "coin": coin, "action": action, "volume": "1" });
        let response = common::block_on(best_orders_rpc(ctx.clone(), req)).expect("legacy best_orders must succeed");
        json::from_slice::<Json>(response.body()).unwrap()["result"].clone()
    }

    fn v2_best_orders(ctx: &MmArc, coin: &str, action: &str) -> Json {
        let req: BestOrdersRequestV2 = json::from_value(json::json!({
            "coin": coin, "action": action, "request_by": {"type": "number", "value": 10}
        }))
        .unwrap();
        let response =
            common::block_on(best_orders_rpc_v2(ctx.clone(), req)).expect("mmrpc 2.0 best_orders must succeed");
        json::to_value(response).unwrap()["orders"].clone()
    }

    fn orders_of(result: &Json, coin: &str) -> Vec<Json> {
        result.get(coin).and_then(|o| o.as_array()).cloned().unwrap_or_default()
    }

    /// T-F6.1 / T-F6.4: a shielded-coin order is listed by the legacy `best_orders` under its coin with
    /// `address` = `Shielded`, and by mmrpc 2.0 with `{"address_type":"Shielded"}`.
    #[test]
    fn test_legacy_best_orders_lists_shielded_coin_order_t_f6_4() {
        let (ctx, pubkey, secret) = crate::mm2::lp_ordermatch::ordermatch_tests::ctx_for_legacy_rendering_tests(&[]);
        let item = make_random_orders(pubkey, &secret, "ZOMBIE".into(), "DOC".into(), 1).remove(0);
        let uuid = item.uuid.to_string();

        answer_best_orders_request(vec![("ZOMBIE", vec![item.clone()])]);
        let legacy = orders_of(&legacy_best_orders(&ctx, "DOC", "buy"), "ZOMBIE");
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0]["uuid"], uuid);
        assert_eq!(legacy[0]["address"], "Shielded");

        answer_best_orders_request(vec![("ZOMBIE", vec![item])]);
        let v2 = orders_of(&v2_best_orders(&ctx, "DOC", "buy"), "ZOMBIE");
        assert_eq!(v2.len(), 1);
        assert_eq!(v2[0]["address"], json::json!({"address_type": "Shielded"}));
    }

    /// T-F6.2 / T-F6.3 / Tendermint: UTXO and Tendermint orders render as strings, the order of a coin
    /// family with no derivation (Solana) is omitted while the others are served.
    #[test]
    fn test_legacy_best_orders_omits_only_unresolvable_order_t_f6_3() {
        let (ctx, pubkey, secret) = crate::mm2::lp_ordermatch::ordermatch_tests::ctx_for_legacy_rendering_tests(&[]);
        let doc = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "MARTY".into(), 1).remove(0);
        let sol = make_random_orders(pubkey.clone(), &secret, "SOL".into(), "MARTY".into(), 1).remove(0);
        let iris = make_random_orders(pubkey, &secret, "IRIS".into(), "MARTY".into(), 1).remove(0);
        let all = || {
            vec![
                ("DOC", vec![doc.clone()]),
                ("SOL", vec![sol.clone()]),
                ("IRIS", vec![iris.clone()]),
            ]
        };

        answer_best_orders_request(all());
        let legacy = legacy_best_orders(&ctx, "MARTY", "buy");
        answer_best_orders_request(all());
        let v2 = v2_best_orders(&ctx, "MARTY", "buy");

        assert!(orders_of(&legacy, "SOL").is_empty());
        assert!(orders_of(&v2, "SOL").is_empty());
        for coin in ["DOC", "IRIS"] {
            let legacy_orders = orders_of(&legacy, coin);
            let v2_orders = orders_of(&v2, coin);
            assert_eq!((legacy_orders.len(), v2_orders.len()), (1, 1), "coin {coin}");
            let legacy_addr = legacy_orders[0]["address"].as_str().expect("plain string");
            assert_eq!(
                v2_orders[0]["address"],
                json::json!({"address_type": "Transparent", "address_data": legacy_addr})
            );
        }
        // Tendermint: the legacy path used to refuse the family (Reloaded-only regression).
        assert!(orders_of(&legacy, "IRIS")[0]["address"]
            .as_str()
            .unwrap()
            .starts_with("iaa"));
    }
}
