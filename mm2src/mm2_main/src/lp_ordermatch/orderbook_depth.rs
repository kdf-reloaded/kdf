use super::{orderbook_topic_from_base_rel, OrderbookDepthError, OrdermatchContext, OrdermatchRequest};
use crate::mm2::lp_network::{request_any_relay, P2PRequest};
use crate::mm2::rpc::legacy_adapter::legacy_result;
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
/// context cannot be had.
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
                    let asks = orderbook
                        .unordered
                        .get(&orderbook_pair)
                        .map_or(0, |orders| orders.len());
                    let reversed = (orderbook_pair.1, orderbook_pair.0);
                    let bids = orderbook.unordered.get(&reversed).map_or(0, |orders| orders.len());
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
