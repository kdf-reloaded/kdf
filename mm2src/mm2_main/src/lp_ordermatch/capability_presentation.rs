//! Orders that lack a required capability are not listed (CRD chapter 59, §59.4C).
//!
//! A coin can require a capability of its counterparty (§59.4A): its acceptance check then declines, at
//! the two match messages, a counterparty whose protocol information does not declare it. This module
//! keeps such a counterparty's orders out of what the node shows, with the protocol information the
//! order book stores with every order. It acts on answers only: the stored book, the trie,
//! synchronization and every answer to a peer are untouched (R59.4C.4).
//!
//! - [`CapabilityFilter`] is built once per call for a pair, from the required sets of its two coins
//!   (R59.4C.1), and says whether an order is presentable (R59.4C.2).
//! - [`log_left_out`] writes the one debug line of a call that left out orders (R59.4C.7).
//! - [`listed_in_event_stream`] is the form the synchronous event emitters use.
//! - [`named_orders_refusal`] is the early refusal of `buy` and `sell` (R59.4C.6).

use super::{is_my_order, mm2_internal_pubkey_hex, OrderbookItem, OrdermatchContext, TakerAction};
use coins::coin_protocol_capability::{missing_capabilities, required_counterparty_capabilities,
                                      required_counterparty_capabilities_now};
use common::log;
use mm2_core::mm_ctx::MmArc;
use std::collections::HashSet;
use uuid::Uuid;

/// What the node requires of a counterparty for one coin of a pair.
struct RequiredOfCoin {
    /// The coin whose required set was read.
    coin: String,
    /// The ticker the order book files the coin's orders under.
    orderbook_ticker: String,
    /// The coin's required set (R59.4C.1).
    required: Vec<&'static str>,
}

impl RequiredOfCoin {
    /// Whether an order that names `ticker` trades this coin, by the ticker relation the matching of a taker
    /// request and a maker order uses: the coin's own ticker or its order-book ticker.
    fn trades_as(&self, ticker: &str) -> bool { self.coin == ticker || self.orderbook_ticker == ticker }

    /// The first key of the coin's required set that `info` does not declare.
    fn missing_in(&self, info: &[u8]) -> Option<MissingCapability> {
        let capability = missing_capabilities(&self.required, Some(info)).first().copied()?;
        Some(MissingCapability {
            coin: self.coin.clone(),
            capability,
        })
    }
}

/// A capability that an order does not declare for one of its coins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MissingCapability {
    /// The coin that requires the capability.
    pub(crate) coin: String,
    /// The capability key (R59.4A.4).
    pub(crate) capability: &'static str,
}

/// The required sets of the two coins of a pair, read once per call (R59.4C.1), and the test they give
/// for an order of that pair in either direction (R59.4C.2).
pub(crate) struct CapabilityFilter {
    coins: [RequiredOfCoin; 2],
}

impl CapabilityFilter {
    /// Reads the required sets of the coins `first` and `second`: from the activated coin when the coin is
    /// activated, and from its configuration entry otherwise.
    pub(crate) async fn for_pair(ctx: &MmArc, first: &str, second: &str) -> CapabilityFilter {
        let required = [
            required_counterparty_capabilities(ctx, first).await,
            required_counterparty_capabilities(ctx, second).await,
        ];
        CapabilityFilter::from_required(ctx, [first, second], required)
    }

    /// As [`CapabilityFilter::for_pair`], for a caller that cannot wait.
    pub(crate) fn for_pair_now(ctx: &MmArc, first: &str, second: &str) -> CapabilityFilter {
        let required = [
            required_counterparty_capabilities_now(ctx, first),
            required_counterparty_capabilities_now(ctx, second),
        ];
        CapabilityFilter::from_required(ctx, [first, second], required)
    }

    /// The filter of the pair `coins` whose required sets were read before, in the same order.
    pub(crate) fn from_required(ctx: &MmArc, coins: [&str; 2], required: [Vec<&'static str>; 2]) -> CapabilityFilter {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).ok();
        let orderbook_ticker = |coin: &str| match &ordermatch_ctx {
            Some(ordermatch_ctx) => ordermatch_ctx.orderbook_ticker_bypass(coin),
            None => coin.to_owned(),
        };
        let [first, second] = coins;
        let [first_required, second_required] = required;
        CapabilityFilter {
            coins: [
                RequiredOfCoin {
                    orderbook_ticker: orderbook_ticker(first),
                    coin: first.to_owned(),
                    required: first_required,
                },
                RequiredOfCoin {
                    orderbook_ticker: orderbook_ticker(second),
                    coin: second.to_owned(),
                    required: second_required,
                },
            ],
        }
    }

    /// Whether neither coin requires anything, so that every order is presentable.
    pub(crate) fn requires_nothing(&self) -> bool { self.coins.iter().all(|coin| coin.required.is_empty()) }

    /// The first required capability that `order` does not declare: its base coin is judged before its rel
    /// coin, and the keys of a coin in the order of its required set. `None` when nothing is missing. A coin of
    /// the order is one of the filter's when the order names the coin's own ticker or its order-book ticker.
    pub(crate) fn missing(&self, order: &OrderbookItem) -> Option<MissingCapability> {
        let sides = [
            (&order.base, &order.base_protocol_info),
            (&order.rel, &order.rel_protocol_info),
        ];
        sides.into_iter().find_map(|(ticker, info)| {
            let of_coin = self.coins.iter().find(|coin| coin.trades_as(ticker))?;
            of_coin.missing_in(info)
        })
    }

    /// Whether `order` sells the first coin of the filter for the second one, so that a request which buys the
    /// first coin with the second one can match it by its coins. The tickers are compared as the matching of a
    /// taker request and a maker order compares them.
    pub(crate) fn is_on_the_side(&self, order: &OrderbookItem) -> bool {
        let [first, second] = &self.coins;
        first.trades_as(&order.base) && second.trades_as(&order.rel)
    }

    /// [`CapabilityFilter::missing`] for an order that [`CapabilityFilter::is_on_the_side`]: its base coin is the
    /// first coin of the filter and its rel coin the second one, whichever of the coin's tickers the order names.
    pub(crate) fn missing_on_the_side(&self, order: &OrderbookItem) -> Option<MissingCapability> {
        let [first, second] = &self.coins;
        first
            .missing_in(&order.base_protocol_info)
            .or_else(|| second.missing_in(&order.rel_protocol_info))
    }

    /// Whether `order` is presentable (R59.4C.2). An order of the node's own always is.
    pub(crate) fn is_presentable(&self, order: &OrderbookItem, is_mine: bool) -> bool {
        is_mine || self.missing(order).is_none()
    }
}

/// The debug line of a call on `surface` that left out `left_out` orders of the pair (R59.4C.7). Nothing is
/// logged when no order was left out, and no line is logged per order.
pub(crate) fn log_left_out(surface: &str, base: &str, rel: &str, left_out: usize) {
    if left_out > 0 {
        log::debug!(
            "{}: left out {} order(s) of {}/{} that do not declare a capability this node requires",
            surface,
            left_out,
            base,
            rel
        );
    }
}

/// Whether the order-book event stream reports a change of `order` (R59.4C.3). The emitters are
/// synchronous, so the required sets are read without waiting.
pub(crate) fn listed_in_event_stream(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext, order: &OrderbookItem) -> bool {
    let filter = CapabilityFilter::for_pair_now(ctx, &order.base, &order.rel);
    if filter.missing(order).is_none() {
        return true;
    }
    match mm2_internal_pubkey_hex(ctx) {
        Ok(my_pubsecp) => is_my_order(
            &order.pubkey,
            &my_pubsecp,
            &ordermatch_ctx.orderbook.lock().my_p2p_pubkeys,
        ),
        Err(_) => false,
    }
}

/// Why a `buy` or `sell` that names orders in `match_by` is refused at once (R59.4C.6). The typed surface
/// carries the three members as they are; the error text is built from the coin and the capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NamedOrdersRefusal {
    /// The coin that requires the capability.
    pub(crate) coin: String,
    /// The capability key (R59.4A.4) that the named order with the lowest identifier does not declare.
    pub(crate) capability: &'static str,
    /// The identifiers of the named orders, in ascending order.
    pub(crate) orders: Vec<Uuid>,
}

/// The early refusal of `buy` and `sell` (R59.4C.6): `Some(refusal)` when the node's own book holds
/// every order of `named`, each of them is an order the request can match by its coins, none is the node's
/// own and none is presentable. `None` in every other case, the request then proceeds as before.
///
/// An order the request can match by its coins is on the request's pair and on the side the request takes:
/// for `buy` the order's base and rel are the request's, for `sell` they are swapped. A named order that the
/// node holds but that is not such an order is not judged, like a named order the node does not hold. The
/// refusal names the coin and the capability of the named order with the lowest identifier (`match_by`
/// carries the orders as a set), and within that order its base coin is judged before its rel coin.
pub(crate) async fn named_orders_refusal(
    ctx: &MmArc,
    action: &TakerAction,
    base_coin: &str,
    rel_coin: &str,
    named: &HashSet<Uuid>,
) -> Option<NamedOrdersRefusal> {
    // The maker orders a request can match sell what the request buys.
    let (order_base, order_rel) = match action {
        TakerAction::Buy => (base_coin, rel_coin),
        TakerAction::Sell => (rel_coin, base_coin),
    };
    let filter = CapabilityFilter::for_pair(ctx, order_base, order_rel).await;
    if filter.requires_nothing() {
        return None;
    }
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).ok()?;
    let my_pubsecp = mm2_internal_pubkey_hex(ctx).ok()?;
    let orderbook = ordermatch_ctx.orderbook.lock();
    let mut uuids: Vec<Uuid> = named.iter().copied().collect();
    uuids.sort();
    let mut first_missing = None;
    for uuid in &uuids {
        let order = orderbook.order_set.get(uuid)?;
        if !filter.is_on_the_side(order) || is_my_order(&order.pubkey, &my_pubsecp, &orderbook.my_p2p_pubkeys) {
            return None;
        }
        let missing = filter.missing_on_the_side(order)?;
        first_missing.get_or_insert(missing);
    }
    first_missing.map(|missing| NamedOrdersRefusal {
        coin: missing.coin,
        capability: missing.capability,
        orders: uuids,
    })
}
