//! # Purpose
//!
//! The readiness check before a swap starts, on the order-matching side (maintainer decision of 2026-11-25/26,
//! Reloaded-original, under CRD ch.51 §51.6G R100). A coin whose swap-evidence reads cannot be agreed right now
//! (`MmCoin::swap_evidence_ready`) starts no swap: the maker declines a taker's request without an answer, its
//! orders with the coin are paused (no keep-alive, so peers drop them; they stay saved) and announced again in full
//! once the coin is ready; a taker's `buy`/`sell` is refused, and a taker swap stops before its fee.
//!
//! # Public exports
//!
//! - [`EvidencePause`] — the paused coins and the paused order-book pairs of this node, kept in the
//!   `OrdermatchContext`.
//! - [`pair_not_ready`] — the text of the first coin of a pair that is not ready.
//! - [`refresh_maker_evidence_pause`] — the step of the keep-alive loop that judges the coins of the own maker orders.
//! - [`paused_line`], [`resumed_line`], [`taker_refusal_text`] — the user-visible texts.
//!
//! # Invariants
//!
//! - One INFO line per change of a coin's state, naming the coin and the kind only.
//! - Nothing is sent to a peer for a declined request: there is no wire change.
//! - A paused order is never cancelled.

use super::*;

/// The coins whose swap evidence is not available now and the order-book pairs whose keep-alives are skipped.
#[derive(Debug, Default)]
pub(crate) struct EvidencePause {
    /// The tickers of the paused coins.
    paused: HashSet<String>,
    /// The order-book pairs of the own maker orders with a paused coin; their keep-alives are not sent.
    paused_pairs: HashSet<AlbOrderedOrderbookPair>,
    /// The INFO lines logged since the node started, in order.
    #[cfg(test)]
    lines: Vec<String>,
    /// The WARN lines of the new orders that started paused, in order.
    #[cfg(test)]
    new_order_lines: Vec<String>,
}

/// How a coin's state changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseChange {
    Unchanged,
    Paused,
    Resumed,
}

impl EvidencePause {
    /// Records that `ticker` is not ready, with `not_ready` the coin's text; logs the one INFO line of the change.
    pub(crate) fn note_not_ready(&mut self, ticker: &str, not_ready: &str) -> PauseChange {
        if !self.paused.insert(ticker.to_owned()) {
            return PauseChange::Unchanged;
        }
        self.log(paused_line(not_ready));
        PauseChange::Paused
    }

    /// Records that `ticker` is ready; logs the one INFO line of the change.
    pub(crate) fn note_ready(&mut self, ticker: &str) -> PauseChange {
        if !self.paused.remove(ticker) {
            return PauseChange::Unchanged;
        }
        self.log(resumed_line(ticker));
        PauseChange::Resumed
    }

    /// Whether `ticker` is paused.
    pub(crate) fn is_paused(&self, ticker: &str) -> bool { self.paused.contains(ticker) }

    /// Whether the keep-alive of the order-book pair `alb_pair` is skipped.
    pub(crate) fn is_pair_paused(&self, alb_pair: &str) -> bool { self.paused_pairs.contains(alb_pair) }

    /// Adds the order-book pair of an order that starts paused.
    pub(crate) fn pause_pair(&mut self, alb_pair: AlbOrderedOrderbookPair) { self.paused_pairs.insert(alb_pair); }

    /// Logs the one WARN line of a new maker order that starts paused.
    pub(crate) fn log_new_order_paused(&mut self, line: String) {
        log::warn!("{}", line);
        #[cfg(test)]
        self.new_order_lines.push(line);
    }

    /// The WARN lines of the new orders that started paused.
    #[cfg(test)]
    pub(crate) fn new_order_lines(&self) -> Vec<String> { self.new_order_lines.clone() }

    fn log(&mut self, line: String) {
        log::info!("{}", line);
        #[cfg(test)]
        self.lines.push(line);
    }

    /// The INFO lines logged since the node started.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<String> { self.lines.clone() }
}

/// The INFO line of a coin whose orders are paused: `"{ticker}: swap evidence not available ({kind}); its orders are
/// paused"`.
pub(crate) fn paused_line(not_ready: &str) -> String { format!("{not_ready}; its orders are paused") }

/// The INFO line of a coin whose orders are resumed.
pub(crate) fn resumed_line(ticker: &str) -> String {
    format!("{ticker}: swap evidence available again; its orders are resumed")
}

/// The text a taker gets: `"{ticker}: swap evidence not available ({kind}); try again when the coin's servers
/// answer"`.
pub(crate) fn taker_refusal_text(not_ready: &str) -> String {
    format!("{not_ready}; try again when the coin's servers answer")
}

/// The text of the first coin of the pair that cannot start a swap now (`MmCoin::swap_evidence_ready`), base first;
/// `None` when both can. The two coins are judged at the same time.
pub(crate) async fn pair_not_ready(first: &MmCoinEnum, second: &MmCoinEnum) -> Option<String> {
    let (first, second) = futures::join!(first.swap_evidence_ready(), second.swap_evidence_ready());
    first.err().or_else(|| second.err())
}

/// Judges one coin and records its state: `Ok` or the coin's text, and the change.
async fn judge(ordermatch_ctx: &OrdermatchContext, coin: &MmCoinEnum) -> (Result<(), String>, PauseChange) {
    // The maker's pause: a host whose retries are pending still counts (CRD ch.51 R100, readiness retries).
    let readiness = coin.swap_evidence_ready_for_pause().await;
    let mut pause = ordermatch_ctx.evidence_pause.lock();
    let change = match &readiness {
        Ok(()) => pause.note_ready(coin.ticker()),
        Err(not_ready) => pause.note_not_ready(coin.ticker(), not_ready),
    };
    (readiness, change)
}

/// Judges the coins of a new maker order (`setprice`; a taker order that becomes a maker order is judged by
/// [`converted_maker_order_starts_paused`]): the not-ready texts of the coins that are not ready, base first (empty
/// when the order does not start paused). A non-empty answer records the paused coin (with its INFO line at a change)
/// and the order's pair. A coin that is ready is left to the keep-alive loop, which re-announces the other orders of a
/// coin that recovered.
pub(crate) async fn new_maker_order_starts_paused(
    ordermatch_ctx: &OrdermatchContext,
    order: &MakerOrder,
    base: &MmCoinEnum,
    rel: &MmCoinEnum,
) -> Vec<String> {
    let (base_ready, rel_ready) = futures::join!(
        base.swap_evidence_ready_for_pause(),
        rel.swap_evidence_ready_for_pause()
    );
    let mut pause = ordermatch_ctx.evidence_pause.lock();
    let mut not_ready_texts = Vec::new();
    for (coin, readiness) in [(base, base_ready), (rel, rel_ready)] {
        if let Err(not_ready) = readiness {
            pause.note_not_ready(coin.ticker(), &not_ready);
            not_ready_texts.push(not_ready);
        }
    }
    if !not_ready_texts.is_empty() {
        pause.pause_pair(alb_ordered_pair(
            order.base_orderbook_ticker(),
            order.rel_orderbook_ticker(),
        ));
    }
    not_ready_texts
}

/// Judges, without asking any node, whether the maker order a timed-out taker order becomes starts paused: `true`
/// when one of its coins is paused now, which records the order's pair. It runs while the taker orders are locked, so
/// it never waits on a probe; a coin that is not ready but not paused yet is judged by the next keep-alive refresh,
/// which pauses the order's pair then, and a request for the order meanwhile is still declined by [`pair_not_ready`].
pub(crate) fn converted_maker_order_starts_paused(ordermatch_ctx: &OrdermatchContext, order: &MakerOrder) -> bool {
    let mut pause = ordermatch_ctx.evidence_pause.lock();
    if !pause.is_paused(&order.base) && !pause.is_paused(&order.rel) {
        return false;
    }
    pause.pause_pair(alb_ordered_pair(
        order.base_orderbook_ticker(),
        order.rel_orderbook_ticker(),
    ));
    true
}

/// The step of the keep-alive loop before the keep-alives (maintainer decision of 2026-11-25/26): judges every coin
/// of the own maker orders (a check that, in the healthy case, sends nothing), logs one INFO line per change, sets the
/// order-book pairs whose keep-alives are skipped, and announces again in full the orders of a coin that is ready
/// again and every order of a pair that is no longer paused, unless the order's pair is still paused. Orders whose
/// coins are not active are left alone.
pub(crate) async fn refresh_maker_evidence_pause(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext) {
    // What each order needs, copied under its lock, which is released at once.
    let orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
    let mut tickers: BTreeSet<String> = BTreeSet::new();
    let mut shapes = Vec::with_capacity(orders.len());
    for (uuid, order_mutex) in orders {
        let order = order_mutex.lock().await;
        tickers.insert(order.base.clone());
        tickers.insert(order.rel.clone());
        let alb_pair = alb_ordered_pair(order.base_orderbook_ticker(), order.rel_orderbook_ticker());
        shapes.push((
            uuid,
            order_mutex.clone(),
            order.base.clone(),
            order.rel.clone(),
            alb_pair,
        ));
    }

    let mut coins: HashMap<String, MmCoinEnum> = HashMap::new();
    let mut resumed: HashSet<String> = HashSet::new();
    for ticker in tickers {
        let Ok(Some(coin)) = lp_coinfind(ctx, &ticker).await else {
            continue;
        };
        if judge(ordermatch_ctx, &coin).await.1 == PauseChange::Resumed {
            resumed.insert(ticker.clone());
        }
        coins.insert(ticker, coin);
    }

    // An order is announced again when one of its coins resumed, and every order of a pair whose keep-alives were
    // skipped and no longer are: the pair is shared by the orders of every ticker with the same order-book ticker (a
    // coin and its alias), so an order whose own coins never paused lost its keep-alives too.
    let mut to_announce = Vec::new();
    {
        let mut pause = ordermatch_ctx.evidence_pause.lock();
        let mut paused_pairs = HashSet::new();
        for (_, _, base, rel, alb_pair) in shapes.iter() {
            if pause.is_paused(base) || pause.is_paused(rel) {
                paused_pairs.insert(alb_pair.clone());
            }
        }
        let left: HashSet<AlbOrderedOrderbookPair> = pause.paused_pairs.difference(&paused_pairs).cloned().collect();
        for (uuid, order_mutex, base, rel, alb_pair) in shapes.iter() {
            if paused_pairs.contains(alb_pair) {
                continue;
            }
            if resumed.contains(base) || resumed.contains(rel) || left.contains(alb_pair) {
                to_announce.push((*uuid, order_mutex.clone(), base.clone(), rel.clone()));
            }
        }
        pause.paused_pairs = paused_pairs;
    }

    for (uuid, order_mutex, base, rel) in to_announce {
        let (Some(base_coin), Some(rel_coin)) = (coins.get(&base), coins.get(&rel)) else {
            continue;
        };
        // CRD ch.60 R60.6.5: an order of a pair that cannot trade is never announced.
        if ordermatch_ctx.order_is_untradable(ctx, "Maker", &uuid, base_coin, rel_coin) {
            continue;
        }
        let order = order_mutex.lock().await;
        // The order may have been cancelled or filled meanwhile: then it is not announced.
        if !ordermatch_ctx.maker_orders_ctx.lock().contains_key(&uuid) {
            continue;
        }
        maker_order_created_p2p_notify(
            ctx.clone(),
            &order,
            base_coin.coin_protocol_info(),
            rel_coin.coin_protocol_info(),
        );
    }
}
