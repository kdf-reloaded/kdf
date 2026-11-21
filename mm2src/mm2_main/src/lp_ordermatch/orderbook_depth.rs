use super::capability_presentation::{log_left_out, CapabilityFilter};
use super::{is_my_order, mm2_internal_pubkey_hex, orderbook_topic_from_base_rel, Orderbook, OrderbookDepthError,
            OrdermatchContext, OrdermatchRequest};
use crate::mm2::lp_network::{request_any_relay, P2PRequest};
use crate::mm2::rpc::legacy_adapter::legacy_result;
use coins::coin_protocol_capability::required_counterparty_capabilities;
use coins::is_wallet_only_ticker;
use common::log;
use http::Response;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::{self as json, Value as Json};
use std::collections::HashMap;

/// The request of `orderbook_depth`: the pairs to count the orders of, each as `[base, rel]`.
#[derive(Debug, Deserialize)]
pub struct OrderbookDepthReq {
    pub(crate) pairs: Vec<(String, String)>,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub struct PairDepth {
    asks: usize,
    bids: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct OrderbookDepthP2PResponse {
    depth: HashMap<(String, String), PairDepth>,
}

/// A requested pair with the number of asks and bids the node counts for it. The answer of `orderbook_depth` is a list
/// of these, in no guaranteed order.
#[derive(Debug, Deserialize, Serialize)]
pub struct PairWithDepth {
    pair: (String, String),
    depth: PairDepth,
}

/// The typed core of `orderbook_depth`: for every requested pair, the number of asks and bids, from the node's own
/// orderbook where it holds one and from a relay otherwise. The legacy surface renders its answer as `{"result":
/// [<pair with depth>, ...]}` ([`orderbook_depth_rpc`]).
///
/// # Errors
///
/// An [`OrderbookDepthError`]: a pair holds a wallet-only coin, the relay request fails, or the node's order-matching
/// context or its own key cannot be had.
pub async fn orderbook_depth_core(
    ctx: MmArc,
    req: OrderbookDepthReq,
) -> Result<Vec<PairWithDepth>, MmError<OrderbookDepthError>> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).map_to_mm(OrderbookDepthError::Internal)?;

    let wallet_only_pairs: Vec<_> = req
        .pairs
        .iter()
        .filter(|pair| is_wallet_only_ticker(&ctx, &pair.0) || is_wallet_only_ticker(&ctx, &pair.1))
        .cloned()
        .collect();

    if !wallet_only_pairs.is_empty() {
        return MmError::err(OrderbookDepthError::CoinIsWalletOnly(format!(
            "Pairs {:?} have wallet only coins",
            wallet_only_pairs
        )));
    }

    let mut result = Vec::with_capacity(req.pairs.len());

    // CRD ch.59 R59.4C.3: the counts taken from the node's own book cover presentable orders only. The
    // required sets are read here, once per coin, before the locks below are taken.
    let my_pubsecp = mm2_internal_pubkey_hex(&ctx).map_to_mm(OrderbookDepthError::Internal)?;
    let mut required_of_coin: HashMap<String, Vec<&'static str>> = HashMap::new();
    for coin in req.pairs.iter().flat_map(|pair| [&pair.0, &pair.1]) {
        if !required_of_coin.contains_key(coin) {
            let required = required_counterparty_capabilities(&ctx, coin).await;
            required_of_coin.insert(coin.clone(), required);
        }
    }
    let capability_filter = |pair: &(String, String)| {
        // Every coin of the request was read above.
        let required = |coin: &String| required_of_coin.get(coin).cloned().unwrap_or_default();
        CapabilityFilter::from_required(&ctx, [&pair.0, &pair.1], [required(&pair.0), required(&pair.1)])
    };

    // the Iter::filter uses &Self::Item, which is undesirable, we need owned pair
    #[allow(clippy::unnecessary_filter_map)]
    let mut to_request_from_relay: Vec<_> = {
        let subscriptions = ordermatch_ctx.orderbook_subscriptions.read();
        let orderbook = ordermatch_ctx.orderbook.lock();
        req.pairs
            .into_iter()
            .filter_map(|original_pair| {
                let orderbook_pair = ordermatch_ctx.orderbook_pair_bypass(&original_pair);
                let topic = orderbook_topic_from_base_rel(&orderbook_pair.0, &orderbook_pair.1);
                if subscriptions.contains_key(&topic) {
                    let capability_filter = capability_filter(&original_pair);
                    let (asks, asks_left_out) =
                        presentable_depth(&orderbook, &orderbook_pair, &capability_filter, &my_pubsecp);
                    let reversed = (orderbook_pair.1, orderbook_pair.0);
                    let (bids, bids_left_out) =
                        presentable_depth(&orderbook, &reversed, &capability_filter, &my_pubsecp);
                    log_left_out(
                        "orderbook_depth",
                        &original_pair.0,
                        &original_pair.1,
                        asks_left_out + bids_left_out,
                    );
                    result.push(PairWithDepth {
                        pair: original_pair,
                        depth: PairDepth { asks, bids },
                    });
                    None
                } else {
                    Some((orderbook_pair, original_pair))
                }
            })
            .collect()
    };

    if !to_request_from_relay.is_empty() {
        let p2p_request = OrdermatchRequest::OrderbookDepth {
            pairs: to_request_from_relay
                .iter()
                .map(|(orderbook_pair, _)| orderbook_pair.clone())
                .collect(),
        };
        log::debug!("Sending request_any_relay({:?})", p2p_request);
        let p2p_response =
            request_any_relay::<OrderbookDepthP2PResponse>(ctx.clone(), P2PRequest::Ordermatch(p2p_request))
                .await
                .mm_err(|e| OrderbookDepthError::P2PSubscribeError(e.to_string()))?;
        log::debug!("Received response {:?}", p2p_response);
        if let Some((response, _)) = p2p_response {
            for (pair, depth) in response.depth {
                to_request_from_relay = to_request_from_relay
                    .into_iter()
                    .filter_map(|(orderbook_pair, original_pair)| {
                        if orderbook_pair == pair && original_pair != pair {
                            result.push(PairWithDepth {
                                pair: original_pair,
                                depth,
                            });
                            None
                        } else {
                            Some((orderbook_pair, original_pair))
                        }
                    })
                    .collect();
                result.push(PairWithDepth { pair, depth });
            }
        }
    }

    Ok(result)
}

/// The legacy `orderbook_depth`: decodes the inline body, calls [`orderbook_depth_core`] and answers `{"result":
/// [<pair with depth>, ...]}`.
pub async fn orderbook_depth_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: OrderbookDepthReq = try_s!(json::from_value(req));
    legacy_result(orderbook_depth_core(ctx, req).await)
}

/// The number of presentable orders the own book holds for `pair`, and the number left out
/// (CRD ch.59 R59.4C.2, R59.4C.3).
fn presentable_depth(
    orderbook: &Orderbook,
    pair: &(String, String),
    capability_filter: &CapabilityFilter,
    my_pubsecp: &Option<String>,
) -> (usize, usize) {
    let Some(uuids) = orderbook.unordered.get(pair) else {
        return (0, 0);
    };
    if capability_filter.requires_nothing() {
        return (uuids.len(), 0);
    }
    let left_out = uuids
        .iter()
        .filter_map(|uuid| orderbook.order_set.get(uuid))
        .filter(|order| {
            let is_mine = is_my_order(&order.pubkey, my_pubsecp, &orderbook.my_p2p_pubkeys);
            !capability_filter.is_presentable(order, is_mine)
        })
        .count();
    (uuids.len() - left_out, left_out)
}

pub fn process_orderbook_depth_p2p_request(
    ctx: MmArc,
    pairs: Vec<(String, String)>,
) -> Result<Option<Vec<u8>>, String> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("ordermatch_ctx must exist at this point");
    let orderbook = ordermatch_ctx.orderbook.lock();
    let depth = pairs
        .into_iter()
        .map(|pair| {
            let asks = orderbook.unordered.get(&pair).map_or(0, |orders| orders.len());
            let reversed = (pair.1.clone(), pair.0.clone());
            let bids = orderbook.unordered.get(&reversed).map_or(0, |orders| orders.len());
            (pair, PairDepth { asks, bids })
        })
        .collect();
    let response = OrderbookDepthP2PResponse { depth };
    let encoded = rmp_serde::to_vec_named(&response).expect("rmp_serde::to_vec_named should not fail here");
    Ok(Some(encoded))
}
